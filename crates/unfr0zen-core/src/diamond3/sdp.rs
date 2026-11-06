//! Serial Download Protocol over HID, spoken by the i.MX6SX boot ROM. Port of
//! scripts/sdp.py and the upload half of scripts/boot_device.py.

use std::fmt;

pub const UBOOT_BASE: u32 = 0x8780_0000;
pub const HID_DATA_SIZE: usize = 1024;

const REPORT_COMMAND: u8 = 1;
const REPORT_DATA: u8 = 2;
const REPORT_HAB: u8 = 3;
const REPORT_STATUS: u8 = 4;
const READ_REGISTER: u16 = 0x0101;
const WRITE_REGISTER: u16 = 0x0202;
const WRITE_FILE: u16 = 0x0404;
const DCD_WRITE: u16 = 0x0A0A;
const JUMP_ADDRESS: u16 = 0x0B0B;
const WRITE_COMPLETE: [u8; 4] = [0x88; 4];
/// WRITE_REGISTER answers with its own completion word, 0x128a8a12, not the
/// 0x88888888 the file commands use.
const REGISTER_COMPLETE: [u8; 4] = [0x12, 0x8a, 0x8a, 0x12];
/// The HAB security state word, as NXP's uuu names it (sdp.h): whether the
/// ROM's HAB is engaged in this session. It does not say whether the unit's
/// fuses enforce image authentication — units that run any image report
/// `enabled` too, and enforcement only shows as a refused jump.
const HAB_ENABLED: [u8; 4] = [0x12, 0x34, 0x34, 0x12];
const HAB_DISABLED: [u8; 4] = [0x56, 0x78, 0x78, 0x56];

/// A HID device that exchanges whole reports, report ID first.
pub trait Hid {
    fn write(&mut self, report: &[u8]) -> Result<usize>;
    /// Returns an empty report when nothing arrived before the timeout.
    fn read(&mut self, timeout_ms: i32) -> Result<Vec<u8>>;
}

pub use crate::{Error, Result};

/// Discards pending responses.
pub fn drain(dev: &mut impl Hid, timeout_ms: i32) {
    while matches!(dev.read(timeout_ms), Ok(r) if !r.is_empty()) {}
}

pub fn command(dev: &mut impl Hid, kind: u16, address: u32, count: u32) -> Result<()> {
    let mut report = [0u8; 17];
    report[0] = REPORT_COMMAND;
    report[1..3].copy_from_slice(&kind.to_be_bytes());
    report[3..7].copy_from_slice(&address.to_be_bytes());
    report[7] = 0x20; // 32-bit access
    report[8..12].copy_from_slice(&count.to_be_bytes());
    let sent = dev.write(&report)?;
    if sent != report.len() {
        return Err(Error::Io(format!("Short HID command {kind:#06x} write: {sent}/{}", report.len())));
    }
    Ok(())
}

/// Reads one 32-bit word; the i.MX6SX answers in report 4.
pub fn read_word(dev: &mut impl Hid, address: u32) -> Result<u32> {
    Ok(read_word_and_hab(dev, address)?.0)
}

/// Reads one 32-bit word and keeps any HAB report the ROM sends ahead of
/// its answer: engaged HAB reports its state before command replies.
pub fn read_word_and_hab(dev: &mut impl Hid, address: u32) -> Result<(u32, Option<HabState>)> {
    command(dev, READ_REGISTER, address, 4)?;
    let mut hab = None;
    for _ in 0..4 {
        let reply = dev.read(1000)?;
        if reply.len() >= 5 {
            let word: [u8; 4] = reply[1..5].try_into().unwrap();
            match reply[0] {
                REPORT_HAB => hab = Some(HabState::from_word(word)),
                REPORT_STATUS => return Ok((u32::from_le_bytes(word), hab)),
                _ => {}
            }
        }
    }
    Err(Error::Timeout(format!("No word read reply at {address:#x}")))
}

/// Bytes one READ_REGISTER asks for; the ROM answers each in 64-byte report 4s.
const READ_CHUNK: usize = 1024;

/// Reads `len` bytes of memory, a chunk per command.
pub fn read_memory(dev: &mut impl Hid, address: u32, len: usize) -> Result<Vec<u8>> {
    let mut data = Vec::with_capacity(len);
    for start in (0..len).step_by(READ_CHUNK) {
        let count = READ_CHUNK.min(len - start);
        let at = address + start as u32;
        command(dev, READ_REGISTER, at, count as u32)?;
        let (mut got, mut quiet) = (0, 0);
        while got < count {
            let reply = dev.read(1000)?;
            match reply.first() {
                Some(&REPORT_STATUS) => {
                    let take = (count - got).min(reply.len() - 1);
                    data.extend_from_slice(&reply[1..1 + take]);
                    got += take;
                    quiet = 0;
                }
                Some(&REPORT_HAB) => {}
                _ => {
                    quiet += 1;
                    if quiet >= 4 {
                        return Err(Error::Timeout(format!("Memory read at {at:#x} stopped after {got} of {count} bytes")));
                    }
                }
            }
        }
    }
    Ok(data)
}

/// Writes one 32-bit peripheral register and waits for the completion status.
/// The value travels where the file commands carry their byte count; the
/// write lands only inside the ROM's hardcoded peripheral windows.
pub fn write_register(dev: &mut impl Hid, address: u32, value: u32) -> Result<()> {
    let mut report = [0u8; 17];
    report[0] = REPORT_COMMAND;
    report[1..3].copy_from_slice(&WRITE_REGISTER.to_be_bytes());
    report[3..7].copy_from_slice(&address.to_be_bytes());
    report[7] = 0x20; // 32-bit access
    report[8..12].copy_from_slice(&4u32.to_be_bytes());
    report[12..16].copy_from_slice(&value.to_be_bytes());
    let sent = dev.write(&report)?;
    if sent != report.len() {
        return Err(Error::Io(format!("Short HID command {WRITE_REGISTER:#06x} write: {sent}/{}", report.len())));
    }
    for _ in 0..5 {
        let reply = dev.read(1000)?;
        if reply.first() == Some(&REPORT_HAB) {
            continue; // a leftover HAB report is not this write's answer
        }
        if reply.len() >= 5 && reply[0] == REPORT_STATUS {
            if reply[1..5] != REGISTER_COMPLETE {
                return Err(Error::Device(format!(
                    "Register write at {address:#x} failed: {:02x?}", &reply[1..5])));
            }
            return Ok(());
        }
    }
    Err(Error::Timeout(format!("No register write acknowledgement for {address:#x}")))
}

/// Uploads with padded reports, then reads the whole upload back. The caller
/// must reserve the padded extent past the end of `data`.
pub fn write_checked(dev: &mut impl Hid, address: u32, data: &[u8]) -> Result<()> {
    if data.len() < 4 {
        return Err(Error::Device(format!("Upload to {address:#x} shorter than one word")));
    }
    command(dev, WRITE_FILE, address, data.len() as u32)?;
    for (index, block) in data.chunks(HID_DATA_SIZE).enumerate() {
        let mut packet = vec![0u8; 1 + HID_DATA_SIZE];
        packet[0] = REPORT_DATA;
        packet[1..1 + block.len()].copy_from_slice(block);
        let sent = dev.write(&packet)?;
        if sent != packet.len() {
            let at = address as usize + index * HID_DATA_SIZE;
            return Err(Error::Io(format!("Short HID write at {at:#x}: {sent}/{}", packet.len())));
        }
    }
    let mut acknowledged = false;
    for _ in 0..5 {
        let reply = dev.read(1000)?;
        if reply.len() >= 5 && reply[0] == REPORT_STATUS {
            if reply[1..5] != WRITE_COMPLETE {
                return Err(Error::Device(format!(
                    "Upload to {address:#x} failed: {:02x?}", &reply[1..5])));
            }
            acknowledged = true;
            break;
        }
    }
    if !acknowledged {
        return Err(Error::Timeout(format!("No upload acknowledgement for {address:#x}")));
    }
    let readback = read_memory(dev, address, data.len())?;
    if let Some(at) = readback.iter().zip(data).position(|(got, sent)| got != sent) {
        return Err(Error::Device(format!(
            "Upload readback mismatch at {:#x}: {:#04x} != {:#04x}",
            address + at as u32, readback[at], data[at])));
    }
    Ok(())
}

/// How a WRITE FILE ended, for callers that need more than the transport's
/// word: completed by the ROM, answered with a status that is not the
/// completion word — the write was rejected — or left without one, either
/// from silence or from a data report the transport could not finish, both
/// of which for a write whose side effects take the session with it are
/// expected.
#[derive(Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    Completed,
    Rejected,
    Silent,
}

/// Uploads with padded reports and waits for the completion word, without
/// reading the upload back. The ROM range-checks only the first eighth of
/// the transfer, so the tail may land past the checked window; the caller
/// owns the geometry. Only a failure before any data reaches the ROM is an
/// error; once the transfer is under way its side effects own the session.
pub fn write_file(dev: &mut impl Hid, address: u32, data: &[u8]) -> Result<WriteOutcome> {
    command(dev, WRITE_FILE, address, data.len() as u32)?;
    for block in data.chunks(HID_DATA_SIZE) {
        let mut packet = vec![0u8; 1 + HID_DATA_SIZE];
        packet[0] = REPORT_DATA;
        packet[1..1 + block.len()].copy_from_slice(block);
        match dev.write(&packet) {
            Ok(sent) if sent == packet.len() => {}
            _ => return Ok(WriteOutcome::Silent),
        }
    }
    for _ in 0..5 {
        // A read that fails here is the session going down as the write's
        // side effects land: indistinguishable from silence, and expected.
        let reply = match dev.read(1000) {
            Ok(reply) => reply,
            Err(_) => return Ok(WriteOutcome::Silent),
        };
        if reply.len() >= 5 && reply[0] == REPORT_STATUS {
            return Ok(if reply[1..5] == WRITE_COMPLETE { WriteOutcome::Completed } else { WriteOutcome::Rejected });
        }
    }
    Ok(WriteOutcome::Silent)
}

/// Applies a DCD block through the ROM's DCD_WRITE command: the ROM parses
/// the DCD's register writes and executes them itself. The acknowledgement
/// word is 0x128a8a12, the same as a peripheral register write.
pub fn dcd_write(dev: &mut impl Hid, address: u32, dcd: &[u8]) -> Result<()> {
    command(dev, DCD_WRITE, address, dcd.len() as u32)?;
    for block in dcd.chunks(HID_DATA_SIZE) {
        let mut packet = vec![0u8; 1 + HID_DATA_SIZE];
        packet[0] = REPORT_DATA;
        packet[1..1 + block.len()].copy_from_slice(block);
        let sent = dev.write(&packet)?;
        if sent != packet.len() {
            return Err(Error::Io(format!("Short HID DCD_WRITE: {sent}/{}", packet.len())));
        }
    }
    for _ in 0..5 {
        let reply = dev.read(2000)?;
        if reply.first() == Some(&REPORT_HAB) {
            continue;
        }
        if reply.len() >= 5 && reply[0] == REPORT_STATUS {
            if reply[1..5] != REGISTER_COMPLETE {
                return Err(Error::Device(format!(
                    "DCD_WRITE at {address:#x} rejected: {:02x?}", &reply[1..5])));
            }
            return Ok(());
        }
    }
    Err(Error::Timeout(format!("No DCD_WRITE acknowledgement for {address:#x}")))
}

/// The HAB security state of a unit, from the magic word in the ROM's HAB
/// report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HabState {
    /// 0x12343412: HAB engaged. Units whose fuses enforce image
    /// authentication and units that run any image report this alike.
    Enabled,
    /// 0x56787856: HAB not engaged.
    Disabled,
    /// A state word outside this table.
    Unknown([u8; 4]),
}

impl HabState {
    fn from_word(word: [u8; 4]) -> HabState {
        match word {
            HAB_ENABLED => HabState::Enabled,
            HAB_DISABLED => HabState::Disabled,
            other => HabState::Unknown(other),
        }
    }
}

impl fmt::Display for HabState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HabState::Enabled => write!(f, "enabled"),
            HabState::Disabled => write!(f, "disabled"),
            HabState::Unknown(word) => write!(f, "unknown state {:02x?}", word),
        }
    }
}

/// The HAB state in the replies to JUMP: the report the ROM sends before it
/// jumps.
pub fn hab_state(replies: &[Vec<u8>]) -> Option<HabState> {
    let report = replies.iter().find(|r| r.first() == Some(&REPORT_HAB))?;
    Some(HabState::from_word(report.get(1..5)?.try_into().ok()?))
}

/// The completion words the ROM uses in status replies; after a JUMP, a
/// status report carrying anything else is the ROM's error report and means
/// it stayed in serial download. The ROM copies these words out as native
/// little-endian bytes, which only the palindrome words hide.
const KNOWN_ACKS: [&[u8]; 3] = [
    &[0x88, 0x88, 0x88, 0x88],
    &[0x12, 0x8a, 0x8a, 0x12],
    &[0x09, 0xd0, 0x0d, 0x90],
];

/// The error report the ROM sends after refusing a JUMP, if it sent one.
pub fn jump_refusal(replies: &[Vec<u8>]) -> Option<&[u8]> {
    replies
        .iter()
        .filter(|r| r.len() >= 5 && r[0] == REPORT_STATUS)
        .find(|r| !KNOWN_ACKS.contains(&&r[1..5]))
        .map(Vec::as_slice)
}

/// HAB status values, reasons and contexts from NXP's HAB tables, the
/// subset a refused jump can report.
const HAB_STATUS: [(u8, &str); 3] = [(0x33, "failure"), (0x69, "warning"), (0xf0, "success")];
const HAB_REASON: [(u8, &str); 12] = [
    (0x03, "unsupported command"),
    (0x05, "invalid ivt"),
    (0x11, "invalid csf"),
    (0x17, "invalid size"),
    (0x18, "invalid signature"),
    (0x0c, "invalid assertion"),
    (0x1d, "invalid key"),
    (0x21, "invalid certificate"),
    (0x22, "invalid address"),
    (0x27, "invalid dcd"),
    (0x28, "call out of sequence"),
    (0x30, "engine failure"),
];
const HAB_CONTEXT: [(u8, &str); 6] = [
    (0x0a, "authenticate image"),
    (0xc0, "csf or dcd command"),
    (0xcf, "run csf"),
    (0xdb, "authenticated data block"),
    (0xdd, "run dcd"),
    (0xe1, "hab entry"),
];

/// Names the four bytes of an HAB status word: status, reason, context and
/// engine, low byte first.
pub fn hab_word_description(word: [u8; 4]) -> String {
    fn lookup<'a>(table: &'a [(u8, &'a str)], byte: u8) -> &'a str {
        table.iter().find(|(value, _)| *value == byte).map(|(_, name)| *name).unwrap_or("unknown")
    }
    let [status, reason, context, engine] = word;
    format!("status {status:#04x} ({}), reason {reason:#04x} ({}), context {context:#04x} ({}), \
             engine {engine:#04x}",
            lookup(&HAB_STATUS, status), lookup(&HAB_REASON, reason),
            lookup(&HAB_CONTEXT, context))
}

/// Sends JUMP and collects the ROM's answers: the HAB report it sends
/// before it jumps, and — when it refuses the jump — the error status that
/// follows. A read that fails after the HAB report is the ROM dropping USB
/// as it jumps; one that fails before it is a session that never saw the
/// command. Returns every non-empty reply, for diagnostics.
pub fn jump(dev: &mut impl Hid, address: u32) -> Result<Vec<Vec<u8>>> {
    command(dev, JUMP_ADDRESS, address, 0)?;
    let hab_seen = |replies: &[Vec<u8>]| replies.iter().any(|r| r.first() == Some(&REPORT_HAB));
    let mut replies: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        let reply = match dev.read(1000) {
            Ok(reply) => reply,
            // Quiet after the HAB report: the ROM dropped USB and jumped.
            Err(_) if hab_seen(&replies) => return Ok(replies),
            Err(error) => return Err(error),
        };
        if reply.is_empty() {
            if hab_seen(&replies) {
                return Ok(replies);
            }
            continue;
        }
        let after_hab = hab_seen(&replies);
        replies.push(reply);
        if after_hab {
            // An answer after the HAB report: the ROM stayed in SDP.
            return Ok(replies);
        }
    }
    Err(Error::Timeout(format!("JUMP to {address:#x} did not return a HAB report")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Answers with a fixed list of reports; accepts `accept` bytes of each write.
    struct Scripted {
        replies: VecDeque<Vec<u8>>,
        writes: Vec<Vec<u8>>,
        accept: usize,
    }

    fn scripted(replies: &[&[u8]]) -> Scripted {
        Scripted { replies: replies.iter().map(|r| r.to_vec()).collect(), writes: Vec::new(), accept: usize::MAX }
    }

    impl Hid for Scripted {
        fn write(&mut self, report: &[u8]) -> Result<usize> {
            self.writes.push(report.to_vec());
            Ok(report.len().min(self.accept))
        }

        fn read(&mut self, _timeout_ms: i32) -> Result<Vec<u8>> {
            Ok(self.replies.pop_front().unwrap_or_default())
        }
    }

    #[test]
    fn a_raw_write_file_sends_the_command_then_whole_data_reports() {
        let ack = vec![4, 0x88, 0x88, 0x88, 0x88];
        let data: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
        let mut device = scripted_owned(&[ack]);
        assert_eq!(write_file(&mut device, 0x0090_7000, &data).unwrap(), WriteOutcome::Completed);
        assert_eq!(device.writes.len(), 3); // command, then two padded data reports
        assert_eq!(&device.writes[0][1..3], &WRITE_FILE.to_be_bytes());
        assert_eq!(&device.writes[0][3..7], &0x0090_7000u32.to_be_bytes());
        assert_eq!(&device.writes[0][8..12], &(data.len() as u32).to_be_bytes());
        for (index, report) in device.writes[1..].iter().enumerate() {
            assert_eq!(report[0], REPORT_DATA);
            assert_eq!(&report[1..1 + 1024], &data[index * 1024..(index + 1) * 1024]);
        }
        // A status that is not the completion word: the write was rejected.
        let rejected = scripted(&[&[4, 0x12, 0x34, 0x56, 0x78]]);
        assert_eq!(write_file(&mut { rejected }, 0x0090_7000, &[0x41; 16]).unwrap(), WriteOutcome::Rejected);
        // No completion word, or a data report the transport could not
        // finish: the write's side effects took the session.
        let quiet = scripted(&[&[3, 0x12, 0x34, 0x34, 0x12]]);
        assert_eq!(write_file(&mut { quiet }, 0x0090_7000, &[0x41; 16]).unwrap(), WriteOutcome::Silent);
        let mut short = Scripted { accept: 100, ..scripted(&[]) };
        assert_eq!(write_file(&mut short, 0x0090_7000, &[0x41; 16]).unwrap(), WriteOutcome::Silent);
    }

    #[test]
    fn uploads_fail_on_a_short_write_a_bad_status_or_no_acknowledgement() {
        let mut short = Scripted { accept: 64, ..scripted(&[]) };
        assert!(write_checked(&mut short, UBOOT_BASE, &[1; 2048]).unwrap_err().to_string().contains("Short HID write"));
        assert_eq!(short.writes.len(), 2); // the command, then the one data report
        let mut refused = scripted(&[&[3, 0x56, 0x78, 0x78, 0x56], &[4, 0x12, 0x34, 0x56, 0x78]]);
        assert!(write_checked(&mut refused, UBOOT_BASE, &[1; 16]).unwrap_err().to_string().contains("failed"));
        let mut silent = scripted(&[]);
        assert!(matches!(write_checked(&mut silent, UBOOT_BASE, &[1; 16]), Err(Error::Timeout(_))));
        assert!(write_checked(&mut scripted(&[]), UBOOT_BASE, &[1; 3]).is_err());
    }

    /// Memory as the ROM reports it: 64 bytes per report 4.
    fn reports(data: &[u8]) -> Vec<Vec<u8>> {
        data.chunks(64).map(|chunk| [&[REPORT_STATUS][..], chunk].concat()).collect()
    }

    fn scripted_owned(replies: &[Vec<u8>]) -> Scripted {
        scripted(&replies.iter().map(Vec::as_slice).collect::<Vec<_>>())
    }

    #[test]
    fn a_byte_that_did_not_land_fails_the_readback() {
        let ack = vec![4, 0x88, 0x88, 0x88, 0x88];
        let data: Vec<u8> = (1..=100).collect();
        let mut landed = data.clone();
        landed[32] = 0;
        let mut device = scripted_owned(&[vec![ack.clone()], reports(&landed)].concat());
        let error = write_checked(&mut device, UBOOT_BASE, &data).unwrap_err();
        assert!(error.to_string().contains(&format!("mismatch at {:#x}", UBOOT_BASE + 32)), "{error}");
        write_checked(&mut scripted_owned(&[vec![ack], reports(&data)].concat()), UBOOT_BASE, &data).unwrap();
    }

    #[test]
    fn memory_reads_gather_reports_past_a_hab_preamble_a_chunk_per_command() {
        let data: Vec<u8> = (0..=255).cycle().take(READ_CHUNK + 100).collect();
        let replies = [vec![vec![3, 0x12, 0x34, 0x34, 0x12]], reports(&data)].concat();
        let mut device = scripted_owned(&replies);
        assert_eq!(read_memory(&mut device, 0x80, data.len()).unwrap(), data);
        assert_eq!(device.writes.len(), 2);
        assert_eq!(&device.writes[1][3..7], &(0x80 + READ_CHUNK as u32).to_be_bytes());
        assert_eq!(&device.writes[1][8..12], &100u32.to_be_bytes());
        let cut = scripted_owned(&reports(&data[..64]));
        assert!(matches!(read_memory(&mut { cut }, 0x80, READ_CHUNK), Err(Error::Timeout(_))));
    }

    #[test]
    fn register_writes_carry_the_value_and_need_their_acknowledgement() {
        let acknowledged: Vec<u8> = [vec![REPORT_STATUS], REGISTER_COMPLETE.to_vec()].concat();
        let mut done = scripted(&[&acknowledged]);
        write_register(&mut done, 0x021B_0000, 0x8311_0000).unwrap();
        let report = &done.writes[0];
        assert_eq!(report[0], REPORT_COMMAND);
        assert_eq!(&report[1..3], &WRITE_REGISTER.to_be_bytes());
        assert_eq!(&report[3..7], &0x021B_0000u32.to_be_bytes());
        assert_eq!(report[7], 0x20);
        assert_eq!(&report[8..12], &4u32.to_be_bytes());
        assert_eq!(&report[12..16], &0x8311_0000u32.to_be_bytes());

        // A leftover HAB report before the status is not this write's answer.
        let mut refused = scripted(&[&[3, 0x56, 0x78, 0x78, 0x56], &[4, 0x12, 0x34, 0x56, 0x78]]);
        assert!(write_register(&mut refused, 0x020C_4068, 0xFFFF_FFFF).unwrap_err().to_string().contains("failed"));
        assert!(matches!(write_register(&mut scripted(&[]), 0x020C_4068, 1), Err(Error::Timeout(_))));
    }

    #[test]
    fn jump_waits_for_the_hab_report() {
        for (word, state) in [(HAB_ENABLED, HabState::Enabled), (HAB_DISABLED, HabState::Disabled)] {
            let report = [&[REPORT_HAB][..], &word].concat();
            let mut device = scripted(&[&[4, 0, 0, 0, 0], &report]);
            let replies = jump(&mut device, UBOOT_BASE).unwrap();
            assert_eq!(replies.len(), 2);
            assert_eq!(hab_state(&replies), Some(state));
            assert_eq!(&device.writes[0][1..7], &[0x0b, 0x0b, 0x87, 0x80, 0x00, 0x00]);
        }
        assert!(matches!(jump(&mut scripted(&[]), UBOOT_BASE), Err(Error::Timeout(_))));
    }

    #[test]
    fn hab_state_decodes_the_security_word() {
        // A refused jump: the HAB report, then an error status.
        let refused = vec![vec![3, 0x12, 0x34, 0x34, 0x12], vec![4, 0x33, 0x21, 0xc0, 0x00]];
        assert_eq!(hab_state(&refused), Some(HabState::Enabled));
        assert_eq!(hab_state(&[vec![3, 0x56, 0x78, 0x78, 0x56]]), Some(HabState::Disabled));
        assert_eq!(
            hab_state(&[vec![3, 0x11, 0x22, 0x33, 0x44]]),
            Some(HabState::Unknown([0x11, 0x22, 0x33, 0x44]))
        );
        assert_eq!(hab_state(&[vec![4, 0x88, 0x88, 0x88, 0x88]]), None);
        assert_eq!(hab_state(&[vec![3, 0x56, 0x78]]), None);
    }

    #[test]
    fn hab_refusals_name_their_status_reason_and_context() {
        let word = [0x33, 0x21, 0xc0, 0x00];
        let described = hab_word_description(word);
        assert!(described.contains("failure"), "{described}");
        assert!(described.contains("invalid certificate"), "{described}");
        assert!(described.contains("csf or dcd command"), "{described}");
        // An untouched word: fuse words read as OTP, nothing HAB-shaped.
        let opaque = hab_word_description([0x00, 0x0f, 0xf0, 0x1a]);
        assert_eq!(opaque.matches("unknown").count(), 3, "{opaque}");
    }

    #[test]
    fn a_refused_jump_collects_the_error_status() {
        // The ROM stays in SDP and reports an error after the HAB report.
        let mut device = scripted(&[&[3, 0x12, 0x34, 0x34, 0x12], &[4, 0x33, 0x21, 0xc0, 0x00]]);
        let replies = jump(&mut device, UBOOT_BASE).unwrap();
        assert_eq!(replies.len(), 2);
        assert_eq!(jump_refusal(&replies), Some(&[4, 0x33, 0x21, 0xc0, 0x00][..]));

        // A quiet ROM after the HAB report jumped, and a completion word
        // after the HAB report is not a refusal.
        let quiet = jump(&mut scripted(&[&[3, 0x12, 0x34, 0x34, 0x12]]), UBOOT_BASE).unwrap();
        assert_eq!(quiet.len(), 1);
        assert_eq!(jump_refusal(&quiet), None);
        let acked = jump(
            &mut scripted(&[&[3, 0x12, 0x34, 0x34, 0x12], &[4, 0x09, 0xd0, 0x0d, 0x90]]),
            UBOOT_BASE,
        )
        .unwrap();
        assert_eq!(jump_refusal(&acked), None);
    }

    #[test]
    fn word_reads_capture_a_hab_preamble() {
        let mut device = scripted(&[&[3, 0x12, 0x34, 0x34, 0x12], &[4, 0x22, 0, 0, 0]]);
        assert_eq!(read_word_and_hab(&mut device, 0x80).unwrap(), (0x22, Some(HabState::Enabled)));
        assert_eq!(read_word(&mut scripted(&[&[4, 0x22, 0, 0, 0]]), 0x80).unwrap(), 0x22);
    }
}
