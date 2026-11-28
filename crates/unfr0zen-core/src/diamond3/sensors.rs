//! The paired Nest Temperature Sensors in a backup of a gen 3's NAND: their ids,
//! keys and rooms, from nlclient's settings on the user-config volume, as the
//! file nestlocal takes them from (crates/nestlocal/src/sensor/roster.rs).
//!
//! The volume is read as UBI and UBIFS leave it, without an index: every
//! logical block's newest copy, and every node whose CRC is right. The newest
//! directory entry says which inode the settings are, and the newest node of
//! each of its blocks what they hold.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

use crate::{Error, Result};

const BLOCK: u64 = 0x2_0000;
const VOLUME_ID_HEADER: usize = 2048;
const DATA: usize = 4096;
const LAYOUT_VOLUME: u32 = 0x7fff_efff;
const VOLUME: &str = "user-config";
const FILE: &[u8] = b"settings.config";
const NODE_MAGIC: [u8; 4] = [0x31, 0x18, 0x10, 0x06];
const DATA_NODE: u8 = 1;
const ENTRY_NODE: u8 = 2;
/// A UBIFS data node's most, uncompressed.
const NODE_DATA: usize = 4096;

/// A paired sensor. Its key is as nlclient has it, in base64.
#[derive(Clone, PartialEq)]
pub struct PairedSensor {
    pub id: String,
    pub key: String,
    pub room: String,
    pub label: String,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn le64(b: &[u8]) -> u64 {
    u64::from(le32(b)) | u64::from(le32(&b[4..])) << 32
}

fn unreadable(what: &str) -> Error {
    Error::Device(format!("the backup has {what}"))
}

/// The newest copy of each of the volume's logical blocks.
fn volume(image: &mut (impl Read + Seek)) -> Result<Vec<Vec<u8>>> {
    let size = image.seek(SeekFrom::End(0))?;
    let mut block = vec![0u8; BLOCK as usize];
    let mut headers = Vec::new();
    let mut id = None;
    for at in 0..size / BLOCK {
        image.seek(SeekFrom::Start(at * BLOCK))?;
        image.read_exact(&mut block[..DATA])?;
        if !block.starts_with(b"UBI#") {
            continue;
        }
        let header = &block[VOLUME_ID_HEADER..VOLUME_ID_HEADER + 64];
        // UBI takes a block whose header's CRC is wrong for no block at all.
        if !header.starts_with(b"UBI!") || be32(&header[60..]) != !crc32fast::hash(&header[..60]) {
            continue;
        }
        let (volume, logical) = (be32(&header[8..]), be32(&header[12..]));
        let sequence = u64::from(be32(&header[40..])) << 32 | u64::from(be32(&header[44..]));
        headers.push((volume, logical, sequence, at));
        if volume == LAYOUT_VOLUME && id.is_none() {
            image.read_exact(&mut block[DATA..])?;
            id = block[DATA..].as_chunks::<172>().0.iter().take(128).position(|record| {
                let len = usize::from(u16::from_be_bytes([record[14], record[15]]));
                be32(record) != 0 && record.get(16..16 + len) == Some(VOLUME.as_bytes())
            });
        }
    }
    let id = id.ok_or_else(|| unreadable("no user-config volume: is it a whole 3rd generation NAND?"))? as u32;
    let mut newest = BTreeMap::new();
    for (_, logical, sequence, at) in headers.into_iter().filter(|(volume, ..)| *volume == id) {
        let kept = newest.entry(logical).or_insert((sequence, at));
        if sequence > kept.0 {
            *kept = (sequence, at);
        }
    }
    let mut blocks = Vec::new();
    for (_, at) in newest.into_values() {
        image.seek(SeekFrom::Start(at * BLOCK + DATA as u64))?;
        let mut data = vec![0u8; BLOCK as usize - DATA];
        image.read_exact(&mut data)?;
        blocks.push(data);
    }
    Ok(blocks)
}

/// Every node in a block whose CRC is right: its type, sequence number and bytes.
fn nodes(block: &[u8]) -> Vec<(u8, u64, &[u8])> {
    let mut found = Vec::new();
    let mut at = 0;
    while at + 24 <= block.len() {
        if block[at..at + 4] == NODE_MAGIC {
            let len = le32(&block[at + 16..]) as usize;
            if (24..=block.len() - at).contains(&len)
                && !crc32fast::hash(&block[at + 8..at + len]) == le32(&block[at + 4..])
            {
                found.push((block[at + 20], le64(&block[at + 8..]), &block[at..at + len]));
                at += len.next_multiple_of(8);
                continue;
            }
        }
        at += 8;
    }
    found
}

/// A data node's bytes, decompressed.
fn data(node: &[u8]) -> Option<Vec<u8>> {
    let size = le32(node.get(40..48)?) as usize;
    let raw = &node[48..];
    if size > NODE_DATA {
        return None;
    }
    let mut out = vec![0u8; size];
    match u16::from_le_bytes([node[44], node[45]]) {
        0 => out.copy_from_slice(raw.get(..size)?),
        1 => {
            if lzokay::decompress::decompress(raw, &mut out).ok()? != size {
                return None;
            }
        }
        2 => flate2::read::DeflateDecoder::new(raw).read_exact(&mut out).ok()?,
        _ => return None,
    }
    Some(out)
}

/// nlclient's settings file, as the volume last had it.
fn settings(blocks: &[Vec<u8>]) -> Result<Vec<u8>> {
    let mut entry: Option<(u64, u64)> = None;
    let all: Vec<_> = blocks.iter().flat_map(|block| nodes(block)).collect();
    for (kind, sequence, node) in &all {
        if *kind != ENTRY_NODE || node.len() < 56 {
            continue;
        }
        let len = usize::from(u16::from_le_bytes([node[50], node[51]]));
        if node.get(56..56 + len) == Some(FILE) && entry.is_none_or(|(newest, _)| *sequence > newest) {
            entry = Some((*sequence, le64(&node[40..])));
        }
    }
    // An inode of 0 is the name removed.
    let inode = entry.map(|(_, inode)| inode).filter(|inode| *inode != 0);
    let inode = inode.ok_or_else(|| unreadable("no settings of nlclient's"))?;
    let mut parts: BTreeMap<u32, (u64, &[u8])> = BTreeMap::new();
    for (kind, sequence, node) in &all {
        if *kind != DATA_NODE || node.len() < 48 || u64::from(le32(&node[24..])) != inode {
            continue;
        }
        let part = parts.entry(le32(&node[28..]) & 0x1fff_ffff).or_insert((*sequence, node));
        if *sequence > part.0 {
            *part = (*sequence, node);
        }
    }
    let mut out = Vec::new();
    for (expected, (number, (_, node))) in parts.into_iter().enumerate() {
        if number as usize != expected {
            return Err(unreadable("only a part of nlclient's settings"));
        }
        out.extend(data(node).ok_or_else(|| unreadable("nlclient's settings in a form that cannot be read"))?);
    }
    let text = String::from_utf8_lossy(&out);
    if !text.trim_end_matches(['\0', '\n', '\r', ' ']).ends_with("</settings>") {
        return Err(unreadable("only a part of nlclient's settings"));
    }
    Ok(out)
}

/// The sensors in nlclient's settings.
fn paired(settings: &str) -> Vec<PairedSensor> {
    let mut found: BTreeMap<String, [String; 3]> = BTreeMap::new();
    for entry in settings.split("<s key=\"kryptonite.").skip(1) {
        let Some((name, rest)) = entry.split_once("\" value=\"") else { continue };
        let Some((value, _)) = rest.split_once('"') else { continue };
        let Some((id, field)) = name.split_once('.') else { continue };
        let Some(field) = ["key", "whereid", "desc"].iter().position(|f| *f == field) else { continue };
        found.entry(id.to_string()).or_default()[field] = value.to_string();
    }
    found
        .into_iter()
        .filter(|(id, [key, ..])| {
            use base64::Engine;
            let key = base64::engine::general_purpose::STANDARD.decode(key);
            id.len() == 16 && id.bytes().all(|c| c.is_ascii_hexdigit()) && key.is_ok_and(|key| key.len() == 16)
        })
        .map(|(id, [key, room, label])| PairedSensor { id, key, room, label })
        .collect()
}

/// The sensors paired with the thermostat whose NAND this is a backup of.
pub fn from_backup(image: &mut (impl Read + Seek)) -> Result<Vec<PairedSensor>> {
    Ok(paired(&String::from_utf8_lossy(&settings(&volume(image)?)?)))
}

/// The file nestlocal takes them from. It has the keys: it is not for showing.
pub fn file(sensors: &[PairedSensor]) -> String {
    let sensors: Vec<_> = sensors
        .iter()
        .map(|s| serde_json::json!({"id": s.id, "key": s.key, "room": s.room, "label": s.label, "enabled": true}))
        .collect();
    let file = serde_json::json!({ "sensors": sensors });
    format!("{}\n", serde_json::to_string_pretty(&file).expect("strings and a flag"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const SETTINGS: &str = r#"<?xml version="1.0" encoding="ISO-8859-1"?>
<settings>
  <s key="a" value="1"/>
  <s key="kryptonite.18B43000AAAA1111.whereid" value="ANNOTATION_0000000000000005"/>
  <s key="kryptonite.18B43000AAAA1111.key" value="ABEiM0RVZneImaq7zN3u/w=="/>
  <s key="kryptonite.18B43000AAAA1111.seqno" value="5"/>
  <s key="kryptonite.18B43000AAAA1111.desc" value="Desk"/>
  <s key="kryptonite.18B43000BBBB2222.key" value="c2hvcnQ="/>
</settings>
"#;

    fn node(kind: u8, sequence: u64, body: &[u8]) -> Vec<u8> {
        let mut node = NODE_MAGIC.to_vec();
        node.extend([0; 4]);
        node.extend(sequence.to_le_bytes());
        node.extend((24 + body.len() as u32).to_le_bytes());
        node.extend([kind, 0, 0, 0]);
        node.extend(body);
        let crc = !crc32fast::hash(&node[8..]);
        node[4..8].copy_from_slice(&crc.to_le_bytes());
        node.resize(node.len().next_multiple_of(8), 0);
        node
    }

    fn entry(sequence: u64, inode: u64, name: &[u8]) -> Vec<u8> {
        let mut body = vec![0; 16];
        body.extend(inode.to_le_bytes());
        body.extend([0, 0]);
        body.extend((name.len() as u16).to_le_bytes());
        body.extend([0; 4]);
        body.extend(name);
        node(ENTRY_NODE, sequence, &body)
    }

    fn part(sequence: u64, inode: u32, number: u32, data: &[u8], lzo: bool) -> Vec<u8> {
        let mut body = inode.to_le_bytes().to_vec();
        body.extend((1 << 29 | number).to_le_bytes());
        body.extend([0; 8]);
        body.extend((data.len() as u32).to_le_bytes());
        body.extend([u8::from(lzo), 0, 0, 0]);
        body.extend(if lzo { lzokay::compress::compress(data).unwrap() } else { data.to_vec() });
        node(DATA_NODE, sequence, &body)
    }

    fn block(volume: u32, logical: u32, sequence: u64, data: &[u8]) -> Vec<u8> {
        let mut block = vec![0xff; BLOCK as usize];
        block[..4].copy_from_slice(b"UBI#");
        let header = &mut block[VOLUME_ID_HEADER..];
        header[..4].copy_from_slice(b"UBI!");
        header[8..12].copy_from_slice(&volume.to_be_bytes());
        header[12..16].copy_from_slice(&logical.to_be_bytes());
        header[40..48].copy_from_slice(&sequence.to_be_bytes());
        let crc = !crc32fast::hash(&header[..60]);
        header[60..64].copy_from_slice(&crc.to_be_bytes());
        block[DATA..DATA + data.len()].copy_from_slice(data);
        block
    }

    fn image(blocks: &[Vec<u8>]) -> Cursor<Vec<u8>> {
        let mut layout = vec![0u8; 172 * 128];
        layout[172 * 3..][..4].copy_from_slice(&9u32.to_be_bytes());
        layout[172 * 3 + 14..][..2].copy_from_slice(&11u16.to_be_bytes());
        layout[172 * 3 + 16..][..11].copy_from_slice(b"user-config");
        let mut image = vec![0xff; BLOCK as usize];
        image.extend(block(LAYOUT_VOLUME, 0, 1, &layout));
        for block in blocks {
            image.extend(block);
        }
        Cursor::new(image)
    }

    fn whole(sequence: u64, inode: u64, text: &str) -> Vec<u8> {
        let mut out = entry(sequence, inode, FILE);
        for (i, chunk) in text.as_bytes().chunks(256).enumerate() {
            out.extend(part(sequence + 1 + i as u64, inode as u32, i as u32, chunk, i % 2 == 0));
        }
        out
    }

    #[test]
    fn the_sensors_come_out_of_the_newest_settings() {
        let old = whole(10, 70, "<settings>\n</settings>\n");
        let new = whole(50, 71, SETTINGS);
        // An older copy of the same logical block, another volume's, and another file.
        let stale = whole(90, 72, &SETTINGS.replace("Desk", "Stale"));
        let other = whole(95, 73, &SETTINGS.replace("Desk", "Other"));
        let mut blocks =
            vec![block(3, 0, 2, &stale), block(3, 0, 7, &old), block(3, 1, 8, &new), block(4, 1, 9, &other)];
        let sensors = from_backup(&mut image(&blocks)).unwrap();
        assert_eq!(sensors.len(), 1, "one whose key is not 16 bytes is none");
        let one = &sensors[0];
        assert_eq!((one.id.as_str(), one.label.as_str()), ("18B43000AAAA1111", "Desk"));
        assert_eq!((one.key.as_str(), one.room.as_str()), ("ABEiM0RVZneImaq7zN3u/w==", "ANNOTATION_0000000000000005"));
        let file: serde_json::Value = serde_json::from_str(&file(&sensors)).unwrap();
        assert_eq!(file["sensors"][0]["key"], "ABEiM0RVZneImaq7zN3u/w==");
        // Without the newest it is the one before; the name removed, none.
        blocks[2] = block(3, 1, 8, &[]);
        assert!(from_backup(&mut image(&blocks)).unwrap().is_empty());
        blocks[2] = block(3, 1, 8, &entry(60, 0, FILE));
        assert!(from_backup(&mut image(&blocks)).is_err());
    }

    #[test]
    fn a_block_whose_header_crc_is_wrong_is_no_copy_of_it() {
        let old = whole(50, 71, SETTINGS);
        let newer = whole(90, 72, &SETTINGS.replace("Desk", "Torn"));
        let mut torn = block(3, 0, 99, &newer);
        torn[VOLUME_ID_HEADER + 60] ^= 0xff;
        let sensors = from_backup(&mut image(&[block(3, 0, 7, &old), torn])).unwrap();
        assert_eq!(sensors[0].label, "Desk");
    }

    #[test]
    fn a_part_of_the_settings_is_not_taken_for_the_whole() {
        for missing in 0..3 {
            let mut data = entry(50, 71, FILE);
            for (i, chunk) in SETTINGS.as_bytes().chunks(200).enumerate() {
                if i != missing {
                    data.extend(part(51 + i as u64, 71, i as u32, chunk, true));
                }
            }
            let e = from_backup(&mut image(&[block(3, 0, 1, &data)]));
            assert!(e.is_err(), "without part {missing}");
        }
    }

    #[test]
    fn a_bad_crc_or_length_is_no_node() {
        let good = whole(50, 71, SETTINGS);
        for at in [4, 16, 17, 30] {
            let mut bad = good.clone();
            bad[at] ^= 0xff;
            assert!(from_backup(&mut image(&[block(3, 0, 1, &bad)])).is_err(), "byte {at}");
        }
    }

    #[test]
    fn something_else_is_no_backup() {
        assert!(from_backup(&mut Cursor::new(vec![0u8; 3 * BLOCK as usize])).is_err());
        assert!(from_backup(&mut Cursor::new(Vec::new())).is_err());
        assert!(from_backup(&mut image(&[])).is_err());
    }

    /// NESTLOCAL_NAND is a real backup's path.
    #[test]
    #[ignore = "needs a real NAND backup: NESTLOCAL_NAND, and NESTLOCAL_SENSORS for how many are paired"]
    fn a_real_backup() {
        let mut image = std::fs::File::open(std::env::var("NESTLOCAL_NAND").unwrap()).unwrap();
        let sensors = from_backup(&mut image).unwrap();
        let wanted: usize = std::env::var("NESTLOCAL_SENSORS").unwrap().parse().unwrap();
        assert_eq!(sensors.len(), wanted);
    }
}
