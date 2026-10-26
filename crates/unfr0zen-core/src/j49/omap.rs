//! The OMAP3 boot ROM's USB peripheral boot and x-loader's USB loader, as
//! NoLongerEvil's host speaks them. Port of scripts/omapload.py (itself after
//! grant-h/omap_loader and Martin Mueller's omap3_usbload).
//!
//! The ROM pushes a 69-byte ASIC ID on bulk IN; the host starts peripheral
//! boot (0xF0030002) and sends the first stage (x-loader) into SRAM. x-loader
//! then asks for files ('USBf'): the host answers with 'USBs', size and
//! address; x-loader echoes the size ('USBn'), takes the data and confirms
//! it ('USBo'), and 'USBj' enters the loaded code. 'USBm' carries debug text.
//! Writes go out in 512-byte transfers 1 ms apart, as NoLongerEvil's do.

use std::thread;
use std::time::{Duration, Instant};

use crate::bulk::Bulk;
use crate::{device, Error, Result};

pub const ASIC_ID_LEN: usize = 69;
/// The first stage must fit below this much of SRAM.
const SRAM_LIMIT: usize = 0x8000;
const SDRAM_BASE: u32 = 0x8000_0000;
/// The smallest j49 SDRAM population.
const SDRAM_MIN: u32 = 0x400_0000;
const PERIPH_BOOT: u32 = 0xF003_0002;
const CMD_FILE: &[u8; 4] = b"USBs";
const CMD_FILE_REQ: &[u8; 4] = b"USBf";
const CMD_JUMP: &[u8; 4] = b"USBj";
const CMD_ECHO_SIZE: &[u8; 4] = b"USBn";
const CMD_REPORT_SIZE: &[u8; 4] = b"USBo";
pub const CMD_MESSAGE: &[u8; 4] = b"USBm";
const PACKET: usize = 512;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(1);

/// The SoC as the boot ROM and the flasher name it: the ASIC ID's device and
/// ROM version, and the silicon version from CONTROL_IDCODE when known.
pub fn silicon(asic: &[u8], idcode: Option<u32>) -> String {
    let (part, rom) = match asic {
        [_, 0x01, 0x05, 0x01, high, low, _, rom, ..] => (u16::from_be_bytes([*high, *low]), *rom),
        _ => return "unknown OMAP (no device ID in its ASIC ID)".into(),
    };
    let mut text = match part {
        0x3630 => "AM3703 (OMAP3630)".to_string(),
        other => format!("OMAP {other:04x}"),
    };
    match idcode {
        // The AM/DM37x's Hawkeye; versions 0-2 are ES1.0-ES1.2.
        Some(id) if (id >> 12) & 0xffff == 0xb891 => text += &format!(" ES1.{}", id >> 28),
        Some(id) => text += &format!(" IDCODE {id:#010x}"),
        None => {}
    }
    text + &format!(", boot ROM {rom:#04x}")
}

/// One read; empty when nothing arrived in time.
pub fn try_receive(bulk: &mut impl Bulk, max: usize, timeout: Duration) -> Result<Vec<u8>> {
    match bulk.receive(max, timeout) {
        Err(Error::Timeout(_)) => Ok(Vec::new()),
        other => other,
    }
}

/// Sends data in 512-byte transfers, 1 ms apart, within `budget`.
pub fn write(bulk: &mut impl Bulk, data: &[u8], budget: Duration) -> Result<()> {
    let deadline = Instant::now() + budget;
    for (index, chunk) in data.chunks(PACKET).enumerate() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::Timeout("USB write deadline expired".into()));
        }
        if index > 0 {
            thread::sleep(Duration::from_millis(1));
        }
        bulk.send(chunk, left.min(TRANSFER_TIMEOUT).max(Duration::from_millis(1)))?;
    }
    Ok(())
}

/// The ASIC ID the boot ROM pushes when it enters peripheral boot.
pub fn probe(bulk: &mut impl Bulk) -> Result<Vec<u8>> {
    for _ in 0..3 {
        let raw = try_receive(bulk, PACKET, Duration::from_secs(1))?;
        if raw.len() == ASIC_ID_LEN {
            return Ok(raw);
        }
        if !raw.is_empty() {
            return device(format!("ASIC ID is {} bytes, expected {ASIC_ID_LEN}", raw.len()));
        }
        thread::sleep(Duration::from_millis(500));
    }
    device("no ASIC ID: the 0451:d00e device is not a live boot ROM (a hung x-loader enumerates the same way); \
            power-cycle the unit and retry")
}

/// The peripheral boot command, then the first stage into SRAM.
pub fn load_first_stage(bulk: &mut impl Bulk, data: &[u8]) -> Result<()> {
    if data.is_empty() || data.len() > SRAM_LIMIT {
        return device(format!("the first stage is {} bytes; SRAM takes 1 to {SRAM_LIMIT}", data.len()));
    }
    write(bulk, &PERIPH_BOOT.to_le_bytes(), Duration::from_secs(5))?;
    write(bulk, &(data.len() as u32).to_le_bytes(), Duration::from_secs(5))?;
    write(bulk, data, Duration::from_secs(10))
}

/// A file x-loader places in SDRAM.
pub struct File<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
    pub address: u32,
}

/// Refuses files that cannot all sit in the smallest SDRAM, or overlap.
pub fn check_files(files: &[File]) -> Result<()> {
    let mut spans = Vec::new();
    for file in files {
        let end = file.address as u64 + file.data.len() as u64;
        if file.data.is_empty() || file.address < SDRAM_BASE || end > SDRAM_BASE as u64 + SDRAM_MIN as u64 {
            return device(format!("{} at {:#x}+{:#x} leaves SDRAM {SDRAM_BASE:#x}-{:#x}", file.name, file.address,
                                  file.data.len(), SDRAM_BASE as u64 + SDRAM_MIN as u64));
        }
        spans.push((file.address as u64, end, file.name));
    }
    spans.sort();
    for pair in spans.windows(2) {
        if pair[1].0 < pair[0].1 {
            return device(format!("{} and {} overlap in SDRAM", pair[0].2, pair[1].2));
        }
    }
    Ok(())
}

/// Answers x-loader's file requests until every file is in SDRAM. Returns
/// its debug messages. Three failed reads, or 30 s in one phase, end it.
pub fn upload_files(bulk: &mut impl Bulk, files: &[File]) -> Result<Vec<String>> {
    check_files(files)?;
    #[derive(PartialEq)]
    enum Phase { Request, Echo, Report }
    let (mut messages, mut index, mut phase, mut failures) = (Vec::new(), 0, Phase::Request, 0);
    let mut started = Instant::now();
    while index < files.len() {
        if started.elapsed() > Duration::from_secs(30) {
            return Err(Error::Timeout(format!("x-loader went quiet loading {} (file {} of {}); power-cycle the unit",
                                              files[index].name, index + 1, files.len())));
        }
        let raw = match bulk.receive(PACKET, TRANSFER_TIMEOUT) {
            Ok(raw) if !raw.is_empty() => raw,
            Ok(_) | Err(_) => {
                failures += 1;
                if failures >= 3 {
                    return device("x-loader did not answer three times; power-cycle the unit");
                }
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        if raw.len() < 4 {
            continue;
        }
        let file = &files[index];
        let word = |at: usize| raw.get(at..at + 4).map(|w| u32::from_le_bytes(w.try_into().unwrap()));
        match (&raw[..4], &phase) {
            (op, _) if op == CMD_MESSAGE => {
                let text = raw[4..].split(|&b| b == 0).next().unwrap_or(&[]);
                messages.push(String::from_utf8_lossy(text).into_owned());
                continue;
            }
            (op, Phase::Request) if op == CMD_FILE_REQ => {
                let request = [&CMD_FILE[..], &(file.data.len() as u32).to_le_bytes(), &file.address.to_le_bytes()].concat();
                write(bulk, &request, Duration::from_secs(2))?;
                phase = Phase::Echo;
            }
            (op, Phase::Echo) if op == CMD_ECHO_SIZE => {
                match word(4) {
                    Some(size) if size as usize == file.data.len() => {}
                    other => return device(format!("x-loader echoed {other:?} bytes for {}, not {}", file.name, file.data.len())),
                }
                write(bulk, file.data, Duration::from_secs(60))?;
                phase = Phase::Report;
            }
            (op, Phase::Report) if op == CMD_REPORT_SIZE => {
                match word(4) {
                    Some(size) if size as usize == file.data.len() => {}
                    other => return device(format!("x-loader received {other:?} of {} bytes of {}", file.data.len(), file.name)),
                }
                index += 1;
                phase = Phase::Request;
            }
            (op, _) => return device(format!("x-loader sent {:?} out of turn", String::from_utf8_lossy(op))),
        }
        started = Instant::now();
    }
    Ok(messages)
}

/// Enters the loaded code: 'USBj', entry, then a zero word, 12 bytes as
/// x-loader's USB loader reads them.
pub fn jump(bulk: &mut impl Bulk, entry: u32) -> Result<()> {
    write(bulk, &[&CMD_JUMP[..], &entry.to_le_bytes(), &[0; 4]].concat(), Duration::from_secs(2))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A device that answers from a script and records what it was sent.
    #[derive(Default)]
    pub(crate) struct Scripted {
        pub(crate) replies: VecDeque<Vec<u8>>,
        pub(crate) sent: Vec<Vec<u8>>,
    }

    impl Bulk for Scripted {
        fn send(&mut self, data: &[u8], _: Duration) -> Result<()> {
            self.sent.push(data.to_vec());
            Ok(())
        }
        fn receive(&mut self, max: usize, _: Duration) -> Result<Vec<u8>> {
            match self.replies.pop_front() {
                Some(mut reply) => {
                    reply.truncate(max);
                    Ok(reply)
                }
                None => Err(Error::Timeout("nothing".into())),
            }
        }
    }

    fn word(op: &[u8; 4], value: u32) -> Vec<u8> {
        [&op[..], &value.to_le_bytes()].concat()
    }

    #[test]
    fn the_asic_id_and_idcode_name_the_silicon() {
        let asic = [0x05, 0x01, 0x05, 0x01, 0x36, 0x30, 0x07, 0x07, 0x13];
        assert_eq!(silicon(&asic, None), "AM3703 (OMAP3630), boot ROM 0x07");
        assert_eq!(silicon(&asic, Some(0x2b89_102f)), "AM3703 (OMAP3630) ES1.2, boot ROM 0x07");
        assert_eq!(silicon(&[0x05], None), "unknown OMAP (no device ID in its ASIC ID)");
    }

    #[test]
    fn the_first_stage_goes_in_512_byte_transfers_after_its_size() {
        let mut bulk = Scripted::default();
        load_first_stage(&mut bulk, &[7; 1000]).unwrap();
        let sizes: Vec<usize> = bulk.sent.iter().map(Vec::len).collect();
        assert_eq!(sizes, [4, 4, 512, 488]);
        assert_eq!(bulk.sent[0], PERIPH_BOOT.to_le_bytes());
        assert_eq!(bulk.sent[1], 1000u32.to_le_bytes());
        assert!(load_first_stage(&mut bulk, &[0; SRAM_LIMIT + 1]).is_err());
    }

    #[test]
    fn a_file_is_requested_echoed_and_confirmed() {
        let data = vec![1u8; 600];
        let mut bulk = Scripted { replies: VecDeque::from([
            b"USBmhello\0".to_vec(), word(CMD_FILE_REQ, 0), word(CMD_ECHO_SIZE, 600), word(CMD_REPORT_SIZE, 600),
        ]), ..Default::default() };
        let messages = upload_files(&mut bulk, &[File { name: "flasher", data: &data, address: 0x8010_0000 }]).unwrap();
        assert_eq!(messages, ["hello"]);
        assert_eq!(bulk.sent[0], [&b"USBs"[..], &600u32.to_le_bytes(), &0x8010_0000u32.to_le_bytes()].concat());
        assert_eq!(bulk.sent[1..].concat(), data);
        jump(&mut bulk, 0x8010_0000).unwrap();
        assert_eq!(bulk.sent.last().unwrap().len(), 12);
    }

    #[test]
    fn a_wrong_echo_stops_the_upload() {
        let mut bulk = Scripted { replies: VecDeque::from([word(CMD_FILE_REQ, 0), word(CMD_ECHO_SIZE, 5)]), ..Default::default() };
        let error = upload_files(&mut bulk, &[File { name: "f", data: &[0; 6], address: 0x8010_0000 }]).unwrap_err();
        assert!(error.to_string().contains("echoed"), "{error}");
    }

    #[test]
    fn files_must_fit_in_sdram_apart() {
        let file = |address| File { name: "f", data: &[0; 16], address };
        assert!(check_files(&[file(0x8000_0000), file(0x8000_0010)]).is_ok());
        assert!(check_files(&[file(0x8000_0000), file(0x8000_0008)]).is_err());
        assert!(check_files(&[file(0x4020_0000)]).is_err());
        assert!(check_files(&[file(0x83ff_fff8)]).is_err());
    }
}
