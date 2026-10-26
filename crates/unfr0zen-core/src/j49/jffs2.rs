//! A j49 root slot's JFFS2, read from its raw pages and changed by appending
//! nodes. Nothing already in the slot is rewritten: JFFS2 is a log, so files
//! added or changed are written as new nodes, of versions above everything
//! the slot holds, into blocks that are still erased. The slot's own nodes
//! stay byte for byte as they are, no block is erased, and a write cut short
//! leaves nodes whose CRCs fail, which JFFS2 skips. A changed file is
//! written whole as a new inode and named last, the way a copy is moved over
//! the original: until its name lands, the name keeps the old file, which
//! JFFS2 then drops as unlinked. Port of scripts/patch_root_j49.py.
//!
//! nestlocal's binary is the one exception, when its new copy does not fit
//! beside the old, or when the patch as a whole did not
//! ([`Slot::make_room_for_nestlocal`]): its name is removed first, the blocks holding nothing but
//! the old copy are erased (and given back JFFS2's clean marker), and the new
//! copy goes there. A write cut short in between leaves the slot without
//! nestlocal until the patch runs again; the rest of it is as it was.
//!
//! As a [`Volume`], the patch sees the slot as the initramfs shell shows a
//! mounted one; [`Slot::programmed`] then gives the pages to program.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};

use super::nand::{self, BLOCK, BLOCK_RAW, PAGE, PAGES_PER_BLOCK, PAGE_RAW};
use crate::shell::{sha256_hex, Digests, File, Volume};
use crate::{device, Result};

const MAGIC: u16 = 0x1985;
const DIRENT: u16 = 0xe001;
const INODE: u16 = 0xe002;
/// Node types that say nothing about any file.
const CLEANMARKER: u16 = 0x2003;
const PADDING: u16 = 0x2004;
const SUMMARY: u16 = 0x2006;
/// An erased block's first OOB as the unit's JFFS2 leaves it: the clean
/// marker node (magic, CLEANMARKER, length 8) after the bad-block marker.
const CLEAN_OOB: (usize, [u8; 8]) = (2, [0x85, 0x19, 0x03, 0x20, 0x08, 0x00, 0x00, 0x00]);
/// Files whose old copy may be erased to make room for the new: nestlocal's,
/// which a unit can always do without until the patch runs again.
const REPLACED_IN_PLACE: [&str; 1] = ["nestlabs/sbin/nestlocal"];
const COMPR_NONE: u8 = 0x00;
const COMPR_ZERO: u8 = 0x01;
const COMPR_RTIME: u8 = 0x02;
const COMPR_ZLIB: u8 = 0x06;
const COMPR_LZO: u8 = 0x07;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const INODE_HEADER: usize = 68;
const DIRENT_HEADER: usize = 40;
/// JFFS2's data nodes cover at most one Linux page.
const DATA_CHUNK: usize = 4096;
/// Erased blocks left over for JFFS2's own use.
const SPARE_BLOCKS: usize = 16;
const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
const ROOT: u32 = 1;

/// JFFS2's CRC32: Linux crc32(0, ...), without zlib's inversions.
fn crc(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new_with_initial(0xffff_ffff);
    hasher.update(data);
    !hasher.finalize()
}

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(data[at..at + 2].try_into().unwrap())
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn rtime_decompress(data: &[u8], size: usize) -> Result<Vec<u8>> {
    let (mut out, mut positions, mut at) = (Vec::with_capacity(size), [0usize; 256], 0);
    while out.len() < size {
        let (Some(&value), Some(&repeat)) = (data.get(at), data.get(at + 1)) else {
            return device("truncated rtime data in JFFS2");
        };
        at += 2;
        out.push(value);
        let start = positions[value as usize];
        positions[value as usize] = out.len();
        for back in (start..).take(repeat as usize) {
            let Some(&byte) = out.get(back) else { return device("bad rtime back-reference in JFFS2") };
            out.push(byte);
        }
    }
    out.truncate(size);
    Ok(out)
}

#[derive(Clone, Debug)]
struct Inode {
    version: u32,
    mode: u32,
    uid: u16,
    gid: u16,
    isize: u32,
    offset: u32,
    dsize: u32,
    compr: u8,
    payload: Vec<u8>,
}

impl Inode {
    fn data(&self) -> Result<Vec<u8>> {
        let size = self.dsize as usize;
        Ok(match self.compr {
            COMPR_NONE => self.payload.clone(),
            COMPR_ZERO => vec![0; size],
            COMPR_ZLIB => {
                let mut out = Vec::with_capacity(size);
                flate2::read::ZlibDecoder::new(&self.payload[..]).read_to_end(&mut out)
                    .map_err(|e| crate::Error::Device(format!("JFFS2 zlib data: {e}")))?;
                out
            }
            COMPR_RTIME => rtime_decompress(&self.payload, size)?,
            COMPR_LZO => {
                let mut out = vec![0; size];
                if lzokay::decompress::decompress(&self.payload, &mut out) != Ok(size) {
                    return device("JFFS2 LZO data");
                }
                out
            }
            other => return device(format!("JFFS2 compression {other:#x} is not supported")),
        })
    }
}

/// A directory entry: its version and the inode it names (0: removed).
#[derive(Clone, Copy, Debug)]
struct Entry {
    version: u32,
    ino: u32,
}

/// A node placed in an erased block: (block, offset in it, bytes).
type Placed = (usize, usize, Vec<u8>);

/// What a block of the slot holds: the inodes of its valid inode nodes, and
/// whether anything else (a directory entry, an unknown node, stray bytes).
#[derive(Default)]
struct Held {
    inodes: BTreeSet<u32>,
    other: bool,
}

/// How to program the patched slot: `before` the erase, the blocks to
/// erase, then `after` (each erased block's clean marker first, where no
/// node starts it). Pages and blocks count from the slot's start.
pub struct Programming {
    /// The slot as it will read.
    pub raw: Vec<u8>,
    pub before: Vec<usize>,
    pub erase: Vec<usize>,
    pub after: Vec<usize>,
}

/// One root slot: its raw pages, the JFFS2 they hold, and nodes to append.
pub struct Slot {
    raw: Vec<u8>,
    bad: BTreeSet<usize>,
    free: Vec<usize>,
    inodes: BTreeMap<u32, Vec<Inode>>,
    dirents: BTreeMap<(u32, Vec<u8>), Entry>,
    max_version: BTreeMap<u32, u32>,
    next_ino: u32,
    /// Erased blocks not yet written to, and the one being filled.
    blocks: Vec<usize>,
    current: Option<(usize, usize)>,
    nodes: Vec<Placed>,
    now: u32,
    held: BTreeMap<usize, Held>,
    /// Blocks erased to make room, and how many nodes were placed before.
    erase: Vec<usize>,
    erase_after: usize,
    /// Whether nestlocal's old blocks are erased whatever its new copy needs.
    always_make_room: bool,
    out_of_room: bool,
}

impl Slot {
    /// Scans a slot as the flasher reads it: raw pages, bit errors and all.
    pub fn load(raw: Vec<u8>) -> Result<Slot> {
        if !raw.len().is_multiple_of(BLOCK_RAW) {
            return device("the slot is not a whole number of erase blocks");
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32);
        let mut slot = Slot { raw, bad: BTreeSet::new(), free: Vec::new(), inodes: BTreeMap::new(), dirents: BTreeMap::new(),
                              max_version: BTreeMap::new(), next_ino: 2, blocks: Vec::new(), current: None, nodes: Vec::new(), now,
                              held: BTreeMap::new(), erase: Vec::new(), erase_after: 0, always_make_room: false,
                              out_of_room: false };
        for block in 0..slot.raw.len() / BLOCK_RAW {
            let chunk = &slot.raw[block * BLOCK_RAW..(block + 1) * BLOCK_RAW];
            if nand::factory_bad(chunk) {
                slot.bad.insert(block);
                continue;
            }
            if chunk.chunks(PAGE_RAW).all(nand::erased) {
                slot.free.push(block);
                continue;
            }
            let mut data = Vec::with_capacity(BLOCK);
            for page in chunk.chunks(PAGE_RAW) {
                let (corrected, _) = nand::correct_page(page)
                    .map_err(|e| crate::Error::Device(format!("slot block {block}: {e}")))?;
                data.extend(corrected);
            }
            let held = slot.scan(&data);
            slot.held.insert(block, held);
        }
        slot.blocks = slot.free.clone();
        Ok(slot)
    }

    fn scan(&mut self, data: &[u8]) -> Held {
        let (mut held, mut at) = (Held::default(), 0);
        while at + 12 <= data.len() {
            let (magic, kind, total) = (u16_at(data, at), u16_at(data, at + 2), u32_at(data, at + 4) as usize);
            if magic != MAGIC || crc(&data[at..at + 8]) != u32_at(data, at + 8) || total < 12 || at + total > data.len() {
                held.other |= data[at..at + 4] != [0xff; 4];
                at += 4;
                continue;
            }
            let node = &data[at..at + total];
            match kind {
                INODE if total >= INODE_HEADER => match self.add_inode(node) {
                    Some(ino) => drop(held.inodes.insert(ino)),
                    None => held.other = true,
                },
                CLEANMARKER | PADDING | SUMMARY => {}
                DIRENT if total >= DIRENT_HEADER => {
                    self.add_dirent(node);
                    held.other = true;
                }
                _ => held.other = true,
            }
            at += (total + 3) & !3;
        }
        held
    }

    fn note(&mut self, ino: u32, version: u32) {
        let highest = self.max_version.entry(ino).or_default();
        *highest = (*highest).max(version);
        self.next_ino = self.next_ino.max(ino + 1);
    }

    /// The inode a valid node is of, once added; None for a broken one.
    fn add_inode(&mut self, node: &[u8]) -> Option<u32> {
        if crc(&node[..INODE_HEADER - 8]) != u32_at(node, 64) {
            return None;
        }
        let csize = u32_at(node, 48) as usize;
        let payload = node.get(INODE_HEADER..INODE_HEADER + csize)?;
        if crc(payload) != u32_at(node, 60) {
            return None;
        }
        let (ino, version) = (u32_at(node, 12), u32_at(node, 16));
        self.inodes.entry(ino).or_default().push(Inode {
            version, mode: u32_at(node, 20), uid: u16_at(node, 24), gid: u16_at(node, 26), isize: u32_at(node, 28),
            offset: u32_at(node, 44), dsize: u32_at(node, 52), compr: node[56], payload: payload.to_vec(),
        });
        self.note(ino, version);
        Some(ino)
    }

    fn add_dirent(&mut self, node: &[u8]) {
        let nsize = node[28] as usize;
        let Some(name) = node.get(DIRENT_HEADER..DIRENT_HEADER + nsize) else { return };
        if crc(&node[..DIRENT_HEADER - 8]) != u32_at(node, 32) || crc(name) != u32_at(node, 36) {
            return;
        }
        let (pino, version, ino) = (u32_at(node, 12), u32_at(node, 16), u32_at(node, 20));
        let entry = self.dirents.entry((pino, name.to_vec())).or_insert(Entry { version, ino });
        if entry.version <= version {
            *entry = Entry { version, ino };
        }
        self.note(pino, version);
        self.note(ino, 0);
    }

    fn latest(&self, ino: u32) -> Option<&Inode> {
        self.inodes.get(&ino)?.iter().max_by_key(|node| node.version)
    }

    fn mode(&self, ino: u32) -> Option<u32> {
        self.latest(ino).map(|node| node.mode)
    }

    /// The contents of an inode, from its nodes in version order.
    fn contents(&self, ino: u32) -> Result<Vec<u8>> {
        let mut nodes: Vec<&Inode> = self.inodes.get(&ino).map(|n| n.iter().collect()).unwrap_or_default();
        nodes.sort_by_key(|node| node.version);
        let mut content = Vec::new();
        for node in &nodes {
            let chunk = node.data()?;
            let end = node.offset as usize + chunk.len();
            if content.len() < end {
                content.resize(end, 0);
            }
            content[node.offset as usize..end].copy_from_slice(&chunk);
            content.truncate(node.isize as usize);
        }
        content.truncate(nodes.last().map_or(0, |node| node.isize as usize));
        Ok(content)
    }

    fn child(&self, parent: u32, name: &str) -> Option<u32> {
        self.dirents.get(&(parent, name.as_bytes().to_vec())).map(|entry| entry.ino).filter(|&ino| ino != 0)
    }

    /// The inode a path names, following symbolic links on the way, and the
    /// last one too with `follow`.
    fn lookup(&self, path: &str, follow: bool) -> Result<Option<u32>> {
        let mut pending: Vec<String> = path.split('/').filter(|p| !p.is_empty()).rev().map(String::from).collect();
        let (mut stack, mut links) = (vec![ROOT], 0);
        while let Some(part) = pending.pop() {
            match part.as_str() {
                "." => continue,
                ".." => {
                    if stack.len() > 1 {
                        stack.pop();
                    }
                    continue;
                }
                _ => {}
            }
            let Some(ino) = self.child(*stack.last().unwrap(), &part) else { return Ok(None) };
            if self.mode(ino).is_some_and(|mode| mode & S_IFMT == S_IFLNK) && (follow || !pending.is_empty()) {
                links += 1;
                if links > 16 {
                    return device(format!("{path}: too many symbolic links"));
                }
                let target = String::from_utf8_lossy(&self.contents(ino)?).into_owned();
                if target.starts_with('/') {
                    stack.truncate(1);
                }
                pending.extend(target.split('/').filter(|p| !p.is_empty()).rev().map(String::from));
                continue;
            }
            stack.push(ino);
        }
        Ok(Some(*stack.last().unwrap()))
    }

    fn owner_mode(&self, ino: u32) -> (String, u32) {
        let node = self.latest(ino).expect("looked up");
        (format!("{}:{}", node.uid, node.gid), node.mode & 0o7777)
    }

    fn version(&mut self, ino: u32) -> u32 {
        let next = self.max_version.get(&ino).copied().unwrap_or(0) + 1;
        self.max_version.insert(ino, next);
        next
    }

    fn place(&mut self, mut node: Vec<u8>) -> Result<()> {
        let size = (node.len() + 3) & !3;
        node.resize(size, 0xff);
        if self.current.is_none_or(|(_, used)| used + size > BLOCK) {
            if self.blocks.len() <= SPARE_BLOCKS {
                self.out_of_room = true;
                return device(format!("the slot has too few erased blocks left: {}, of which {SPARE_BLOCKS} stay free",
                                      self.free.len()));
            }
            self.current = Some((self.blocks.remove(0), 0));
        }
        let (block, used) = self.current.as_mut().expect("chosen above");
        self.nodes.push((*block, *used, node.clone()));
        *used += size;
        self.scan(&node);
        Ok(())
    }

    /// Whether nodes of these sizes, and a directory entry naming `name`
    /// after them, fit in what place() may still use.
    fn fits(&self, nodes: &[Vec<u8>], name: &str) -> bool {
        let mut left = self.current.map_or(0, |(_, used)| BLOCK - used);
        let mut blocks = self.blocks.len().saturating_sub(SPARE_BLOCKS);
        for size in nodes.iter().map(Vec::len).chain([DIRENT_HEADER + name.len()]).map(|len| (len + 3) & !3) {
            if size > left {
                if blocks == 0 {
                    return false;
                }
                blocks -= 1;
                left = BLOCK;
            }
            left -= size;
        }
        true
    }

    /// Removes `base` from `parent`, then frees the blocks holding nothing
    /// but `old`'s nodes, to be erased once what was placed before is
    /// programmed.
    fn make_room(&mut self, parent: u32, base: &str, old: u32) -> Result<()> {
        let dead: Vec<usize> = self.held.iter().filter(|(_, held)| !held.other && held.inodes == BTreeSet::from([old]))
            .map(|(&block, _)| block).filter(|block| !self.erase.contains(block)).collect();
        if dead.is_empty() {
            return Ok(());
        }
        self.dirent(parent, base, 0, 0)?;
        self.erase_after = self.nodes.len();
        self.blocks.extend(&dead);
        self.blocks.sort_unstable();
        self.erase.extend(dead);
        Ok(())
    }

    fn inode(&mut self, ino: u32, mode: u32, ids: (u16, u16), isize: usize, offset: usize, data: &[u8]) -> Result<()> {
        let node = self.inode_node(ino, mode, ids, isize, offset, data)?;
        self.place(node)
    }

    fn inode_node(&mut self, ino: u32, mode: u32, (uid, gid): (u16, u16), isize: usize, offset: usize, data: &[u8]) -> Result<Vec<u8>> {
        let (mut payload, mut compr) = (data.to_vec(), COMPR_NONE);
        if !data.is_empty() {
            let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
            encoder.write_all(data)?;
            let packed = encoder.finish()?;
            if packed.len() < data.len() {
                (payload, compr) = (packed, COMPR_ZLIB);
            }
        }
        let version = self.version(ino);
        let mut node = Vec::with_capacity(INODE_HEADER + payload.len());
        node.extend(MAGIC.to_le_bytes());
        node.extend(INODE.to_le_bytes());
        node.extend(((INODE_HEADER + payload.len()) as u32).to_le_bytes());
        node.extend(crc(&node).to_le_bytes());
        for word in [ino, version, mode] {
            node.extend(word.to_le_bytes());
        }
        node.extend(uid.to_le_bytes());
        node.extend(gid.to_le_bytes());
        for word in [isize as u32, self.now, self.now, self.now, offset as u32, payload.len() as u32, data.len() as u32] {
            node.extend(word.to_le_bytes());
        }
        node.extend([compr, 0, 0, 0]);
        let node_crc = crc(&node);
        node.extend(crc(&payload).to_le_bytes());
        node.extend(node_crc.to_le_bytes());
        node.extend(payload);
        Ok(node)
    }

    fn dirent(&mut self, pino: u32, name: &str, ino: u32, kind: u8) -> Result<()> {
        let version = self.version(pino);
        let mut node = Vec::with_capacity(DIRENT_HEADER + name.len());
        node.extend(MAGIC.to_le_bytes());
        node.extend(DIRENT.to_le_bytes());
        node.extend(((DIRENT_HEADER + name.len()) as u32).to_le_bytes());
        node.extend(crc(&node).to_le_bytes());
        for word in [pino, version, ino, self.now] {
            node.extend(word.to_le_bytes());
        }
        node.extend([name.len() as u8, kind, 0, 0]);
        node.extend(crc(&node).to_le_bytes());
        node.extend(crc(name.as_bytes()).to_le_bytes());
        node.extend(name.as_bytes());
        self.place(node)
    }

    fn split(name: &str) -> (&str, &str) {
        name.trim_matches('/').rsplit_once('/').unwrap_or(("", name.trim_matches('/')))
    }

    /// The directory at path, made with its parents as root-owned `mode`
    /// where missing; an existing one gets `mode` when `chmod`.
    fn ensure_directory(&mut self, path: &str, mode: u32, chmod: bool) -> Result<u32> {
        if path.is_empty() {
            return Ok(ROOT);
        }
        if let Some(ino) = self.lookup(path, true)? {
            let node = self.latest(ino).cloned();
            match node {
                Some(node) if node.mode & S_IFMT == S_IFDIR => {
                    if chmod && (node.mode & 0o7777, node.uid, node.gid) != (mode, 0, 0) {
                        self.inode(ino, S_IFDIR | mode, (0, 0), 0, 0, &[])?;
                    }
                    return Ok(ino);
                }
                _ => return device(format!("{path} exists and is not a directory")),
            }
        }
        let (parent, base) = Self::split(path);
        let parent = self.ensure_directory(parent, 0o755, false)?;
        let ino = self.next_ino;
        self.next_ino += 1;
        self.inode(ino, S_IFDIR | mode, (0, 0), 0, 0, &[])?;
        self.dirent(parent, base, ino, DT_DIR)
            .map(|_| ino)
    }

    /// How to program the new nodes (data and ECC): their pages in the order
    /// the nodes were placed, which programming keeps (a file's name after its
    /// data), split at the erase that makes room. Each page is erased first; a
    /// block's OOB free bytes are kept.
    pub fn programmed(&self) -> Result<Programming> {
        let mut out = self.raw.clone();
        for &block in &self.erase {
            let at = block * BLOCK_RAW;
            out[at..at + BLOCK_RAW].fill(0xff);
            let (offset, marker) = CLEAN_OOB;
            out[at + PAGE + offset..at + PAGE + offset + marker.len()].copy_from_slice(&marker);
        }
        let (mut pages, mut before, mut after): (BTreeMap<usize, Vec<u8>>, Vec<usize>, Vec<usize>) = Default::default();
        for (index, (block, offset, node)) in self.nodes.iter().enumerate() {
            let start = block * BLOCK + offset;
            for page in start / PAGE..=(start + node.len() - 1) / PAGE {
                if !pages.contains_key(&page) {
                    if index < self.erase_after { &mut before } else { &mut after }.push(page);
                }
                let data = pages.entry(page).or_insert_with(|| vec![0xff; PAGE]);
                let (lo, hi) = (start.max(page * PAGE), (start + node.len()).min((page + 1) * PAGE));
                data[lo - page * PAGE..hi - page * PAGE].copy_from_slice(&node[lo - start..hi - start]);
            }
        }
        for (&page, data) in &pages {
            let at = page * PAGE_RAW;
            if !nand::erased(&out[at..at + PAGE_RAW]) {
                return device(format!("slot page {page} is not erased"));
            }
            let raw = nand::encode_page(data, &out[at + PAGE..at + PAGE_RAW]);
            out[at..at + PAGE_RAW].copy_from_slice(&raw);
        }
        let markers = self.erase.iter().map(|block| block * PAGES_PER_BLOCK).filter(|page| !pages.contains_key(page));
        Ok(Programming { raw: out, before, erase: self.erase.clone(), after: markers.chain(after).collect() })
    }

    /// Has a nestlocal written to the slot take its old copy's blocks even
    /// when it fits beside them: for a patch that, as a whole, did not.
    pub fn make_room_for_nestlocal(&mut self) {
        self.always_make_room = true;
    }

    /// Whether a node found no erased block left for it.
    pub fn out_of_room(&self) -> bool {
        self.out_of_room
    }

    /// Erased blocks, and the blocks the new nodes use.
    pub fn usage(&self) -> (usize, usize) {
        (self.free.len(), self.nodes.iter().map(|(block, _, _)| block).collect::<BTreeSet<_>>().len())
    }
}

impl Volume for Slot {
    fn read(&mut self, name: &str) -> Result<Option<File<Vec<u8>>>> {
        let Some(ino) = self.lookup(name, false)? else { return Ok(None) };
        match self.mode(ino) {
            Some(mode) if mode & S_IFMT == S_IFLNK => device(format!("{name} on the device is a symbolic link")),
            Some(mode) if mode & S_IFMT == S_IFREG => {
                let (owner, mode) = self.owner_mode(ino);
                Ok(Some((owner, mode, self.contents(ino)?)))
            }
            _ => Ok(None),
        }
    }

    fn inspect(&mut self, name: &str) -> Result<Option<File<String>>> {
        Ok(self.read(name)?.map(|(owner, mode, data)| (owner, mode, sha256_hex(&data))))
    }

    fn directory(&mut self, name: &str) -> Result<Option<String>> {
        let Some(ino) = self.lookup(name, false)? else { return Ok(None) };
        match self.mode(ino) {
            Some(mode) if mode & S_IFMT == S_IFDIR => {
                let (owner, mode) = self.owner_mode(ino);
                Ok(Some(format!("{owner} {mode:o}")))
            }
            _ => device(format!("{name} on the device is not a directory")),
        }
    }

    fn free_bytes(&mut self) -> Result<u64> {
        let left = self.current.map_or(0, |(_, used)| BLOCK - used);
        Ok((self.blocks.len().saturating_sub(SPARE_BLOCKS) * BLOCK + left) as u64)
    }

    /// Nodes are placed in memory; one that does not fit fails the patch
    /// before any page is programmed.
    fn checks_space(&self) -> bool {
        false
    }

    fn shell_binary(&mut self) -> Result<String> {
        for name in ["bin/busybox", "bin/toybox"] {
            if let Some(ino) = self.lookup(name, true)? {
                if self.mode(ino).is_some_and(|mode| mode & S_IFMT == S_IFREG) {
                    let node = self.latest(ino).expect("has a mode");
                    return Ok(format!("{} {:o}", node.uid, node.mode & 0o7777));
                }
            }
        }
        Ok(String::new())
    }

    fn write(&mut self, name: &str, data: &[u8], owner: &str, mode: u32, _payload: &BTreeSet<String>) -> Result<()> {
        let parse = |id: &str| id.parse::<u16>().map_err(|_| crate::Error::Device(format!("{name}: owner {owner:?}")));
        let (uid, gid) = owner.split_once(':').ok_or_else(|| crate::Error::Device(format!("{name}: owner {owner:?}")))?;
        let ids = (parse(uid)?, parse(gid)?);
        let (parent, base) = Self::split(name);
        let parent = self.ensure_directory(parent, 0o755, false)?;
        if self.child(parent, base).is_some_and(|ino| self.mode(ino).is_some_and(|mode| mode & S_IFMT == S_IFDIR)) {
            return device(format!("{name} exists and is a directory"));
        }
        let ino = self.next_ino;
        self.next_ino += 1;
        let full = S_IFREG | (mode & 0o7777);
        let mut nodes = Vec::new();
        if data.is_empty() {
            nodes.push(self.inode_node(ino, full, ids, 0, 0, &[])?);
        }
        for (index, chunk) in data.chunks(DATA_CHUNK).enumerate() {
            nodes.push(self.inode_node(ino, full, ids, data.len(), index * DATA_CHUNK, chunk)?);
        }
        let old = self.child(parent, base).filter(|&old| self.mode(old).is_some_and(|mode| mode & S_IFMT == S_IFREG));
        if let Some(old) = old.filter(|_| REPLACED_IN_PLACE.contains(&name.trim_matches('/'))
                                          && (self.always_make_room || !self.fits(&nodes, base))) {
            self.make_room(parent, base, old)?;
        }
        for node in nodes {
            self.place(node)?;
        }
        self.dirent(parent, base, ino, DT_REG)
    }

    fn make_directory(&mut self, name: &str, mode: u32) -> Result<()> {
        self.ensure_directory(name.trim_matches('/'), mode, true).map(drop)
    }

    fn remove(&mut self, name: &str) -> Result<()> {
        let (parent, base) = Self::split(name);
        let Some(parent) = self.lookup(parent, true)? else { return Ok(()) };
        if self.child(parent, base).is_some() {
            self.dirent(parent, base, 0, 0)?;
        }
        Ok(())
    }

    fn remount(&mut self, _writable: bool) -> Result<()> {
        Ok(())
    }

    /// Read back from the slot as programmed, through a fresh scan.
    fn digests(&mut self, names: &[String]) -> Result<Digests> {
        let mut check = Slot::load(self.programmed()?.raw)?;
        names.iter().map(|name| {
            let found = check.read(name)?;
            Ok((name.clone(), (found.as_ref().map(|(_, _, data)| sha256_hex(data)),
                               found.map(|(owner, mode, _)| format!("{owner} {mode:o}")))))
        }).collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn lzo_inodes_require_complete_data_of_the_declared_size() {
        let raw = b"JFFS2 LZO compatibility block\n".repeat(64);
        let mut inode = Inode {
            version: 1, mode: S_IFREG | 0o644, uid: 0, gid: 0,
            isize: raw.len() as u32, offset: 0, dsize: raw.len() as u32,
            compr: COMPR_LZO, payload: lzokay::compress::compress(&raw).unwrap(),
        };
        assert_eq!(inode.data().unwrap(), raw);
        for size in [raw.len() - 1, raw.len() + 1] {
            inode.dsize = size as u32;
            assert!(inode.data().is_err());
        }
        inode.dsize = raw.len() as u32;
        inode.payload.pop();
        assert!(inode.data().is_err());
    }

    /// An empty slot of `blocks` erased blocks, with a root-owned busybox.
    pub(crate) fn empty(blocks: usize) -> Slot {
        let mut slot = Slot::load(vec![0xff; blocks * BLOCK_RAW]).unwrap();
        slot.write("bin/busybox", b"\x7fELF", "0:0", 0o755, &BTreeSet::new()).unwrap();
        slot
    }

    fn reload(slot: &Slot) -> Slot {
        Slot::load(slot.programmed().unwrap().raw).unwrap()
    }

    #[test]
    fn the_crc_is_linux_crc32_without_inversion() {
        assert_eq!(crc(b""), 0);
        assert_eq!(crc(b"123456789"), 0x2dfd_2d88);
    }

    #[test]
    fn written_files_read_back_from_programmed_pages() {
        let mut slot = empty(20);
        let big: Vec<u8> = (0..10_000u32).map(|i| (i * 7 % 251) as u8).collect();
        slot.write("etc/nestlocal/options", b"--mqtt x\n", "0:0", 0o600, &BTreeSet::new()).unwrap();
        slot.write("nestlabs/sbin/nestlocal", &big, "0:0", 0o755, &BTreeSet::new()).unwrap();
        let mut back = reload(&slot);
        assert_eq!(back.read("etc/nestlocal/options").unwrap(), Some(("0:0".into(), 0o600, b"--mqtt x\n".to_vec())));
        assert_eq!(back.read("nestlabs/sbin/nestlocal").unwrap().unwrap().2, big);
        assert_eq!(back.directory("etc/nestlocal").unwrap(), Some("0:0 755".into()));
        assert_eq!(back.shell_binary().unwrap(), "0 755");
    }

    #[test]
    fn a_rewrite_is_a_new_inode_named_after_its_data() {
        let mut slot = empty(20);
        slot.write("etc/x", &[b'a'; 9000], "0:0", 0o644, &BTreeSet::new()).unwrap();
        let mut slot = reload(&slot);
        let old = slot.lookup("etc/x", false).unwrap().unwrap();
        let mut x = 7u32;
        let noise: Vec<u8> = (0..7000).map(|_| { x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345); (x >> 16) as u8 }).collect();
        slot.write("etc/x", &noise, "0:0", 0o600, &BTreeSet::new()).unwrap();
        assert_ne!(slot.lookup("etc/x", false).unwrap(), Some(old));
        let mut back = reload(&slot);
        assert_eq!(back.read("etc/x").unwrap(), Some(("0:0".into(), 0o600, noise)));
        // Cut short before its last page, the write leaves the old file.
        let Programming { raw, after: pages, .. } = slot.programmed().unwrap();
        assert!(pages.len() > 2, "the new data spans pages before its name's");
        let mut cut = slot.raw.clone();
        for &page in &pages[..pages.len() - 1] {
            cut[page * PAGE_RAW..(page + 1) * PAGE_RAW].copy_from_slice(&raw[page * PAGE_RAW..(page + 1) * PAGE_RAW]);
        }
        let mut cut = Slot::load(cut).unwrap();
        assert_eq!(cut.read("etc/x").unwrap(), Some(("0:0".into(), 0o644, vec![b'a'; 9000])));
    }

    #[test]
    fn removal_and_directories() {
        let mut slot = empty(20);
        slot.write("etc/a", b"1", "0:0", 0o644, &BTreeSet::new()).unwrap();
        slot.make_directory("usr/share/doom", 0o755).unwrap();
        let mut slot = reload(&slot);
        slot.remove("etc/a").unwrap();
        slot.remove("etc/never").unwrap();
        slot.make_directory("usr/share/doom", 0o700).unwrap();
        let mut back = reload(&slot);
        assert_eq!(back.read("etc/a").unwrap(), None);
        assert_eq!(back.directory("usr/share/doom").unwrap(), Some("0:0 700".into()));
        assert_eq!(back.directory("usr/share").unwrap(), Some("0:0 755".into()));
    }

    #[test]
    fn symbolic_links_are_followed_on_the_way() {
        let mut slot = empty(20);
        slot.write("var/real/f", b"x", "0:0", 0o644, &BTreeSet::new()).unwrap();
        let (parent, ino) = (slot.lookup("var", true).unwrap().unwrap(), slot.next_ino);
        slot.next_ino += 1;
        slot.dirent(parent, "link", ino, 10).unwrap();
        slot.inode(ino, S_IFLNK | 0o777, (0, 0), 4, 0, b"real").unwrap();
        let mut back = reload(&slot);
        assert_eq!(back.read("var/link/f").unwrap().unwrap().2, b"x");
        assert!(back.read("var/link").is_err());
    }

    #[test]
    fn spare_blocks_stay_erased() {
        let mut slot = empty(SPARE_BLOCKS + 2);
        let mut x = 1u32;
        let noise: Vec<u8> = (0..3 * BLOCK).map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        }).collect();
        let error = slot.write("big", &noise, "0:0", 0o644, &BTreeSet::new()).unwrap_err();
        assert!(error.to_string().contains("too few erased blocks"), "{error}");
    }

    fn noise(len: usize, mut x: u32) -> Vec<u8> {
        (0..len).map(|_| { x ^= x << 13; x ^= x >> 17; x ^= x << 5; x as u8 }).collect()
    }

    #[test]
    fn nestlocal_takes_only_the_blocks_nothing_else_shares_when_it_does_not_fit() {
        // Blocks 0-2: busybox, nestlocal across all three, and etc/b in the
        // last; one block free beyond the spare ones.
        let mut slot = empty(SPARE_BLOCKS + 4);
        slot.write("nestlabs/sbin/nestlocal", &noise(BLOCK * 5 / 2, 1), "0:0", 0o755, &BTreeSet::new()).unwrap();
        slot.write("etc/b", b"kept", "0:0", 0o644, &BTreeSet::new()).unwrap();
        let mut slot = reload(&slot);
        let new = noise(BLOCK * 9 / 5, 2);
        slot.write("nestlabs/sbin/nestlocal", &new, "0:0", 0o755, &BTreeSet::new()).unwrap();
        let plan = slot.programmed().unwrap();
        assert_eq!(plan.erase, [1], "block 1 alone held nothing but the old nestlocal");
        assert_eq!(plan.before.len(), 1, "the name's removal, programmed before the erase");
        assert_eq!(plan.raw[BLOCK_RAW + PAGE + 2..BLOCK_RAW + PAGE + 10], CLEAN_OOB.1);
        let mut back = Slot::load(plan.raw).unwrap();
        assert_eq!(back.read("nestlabs/sbin/nestlocal").unwrap().unwrap().2, new);
        assert_eq!(back.read("etc/b").unwrap().unwrap().2, b"kept");
        // Anything else that does not fit still fails.
        let mut slot = reload(&slot);
        assert!(slot.write("etc/big", &noise(3 * BLOCK, 3), "0:0", 0o644, &BTreeSet::new()).is_err());
    }

    #[test]
    fn only_erased_pages_are_programmed() {
        let mut slot = empty(20);
        let Programming { raw, after: pages, .. } = slot.programmed().unwrap();
        assert!(!pages.is_empty());
        slot.raw = raw;
        assert!(slot.programmed().err().unwrap().to_string().contains("is not erased"));
    }
}
