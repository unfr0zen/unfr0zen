//! Serial Download Protocol over HID, spoken by the i.MX6SX boot ROM. Port of
//! scripts/sdp.py and the upload half of scripts/boot_device.py.

pub const UBOOT_BASE: u32 = 0x8780_0000;
pub const HID_DATA_SIZE: usize = 1024;

const REPORT_COMMAND: u8 = 1;
const REPORT_DATA: u8 = 2;
const REPORT_HAB: u8 = 3;
const REPORT_STATUS: u8 = 4;
const READ_REGISTER: u16 = 0x0101;
const WRITE_REGISTER: u16 = 0x0202;
const WRITE_FILE: u16 = 0x0404;
const JUMP_ADDRESS: u16 = 0x0B0B;
const WRITE_COMPLETE: [u8; 4] = [0x88; 4];
/// WRITE_REGISTER answers with its own completion word, 0x128a8a12, not the
/// 0x88888888 the file commands use.
const REGISTER_COMPLETE: [u8; 4] = [0x12, 0x8a, 0x8a, 0x12];

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
    dev.write(&report)?;
    Ok(())
}

/// Reads one 32-bit word; the i.MX6SX answers in report 4.
pub fn read_word(dev: &mut impl Hid, address: u32) -> Result<u32> {
    command(dev, READ_REGISTER, address, 4)?;
    for _ in 0..4 {
        let reply = dev.read(1000)?;
        if reply.len() >= 5 && reply[0] == REPORT_STATUS {
            return Ok(u32::from_le_bytes(reply[1..5].try_into().unwrap()));
        }
    }
    Err(Error::Timeout(format!("No word read reply at {address:#x}")))
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
    dev.write(&report)?;
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

/// Uploads with padded reports, then spot-checks three words. The caller
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
    // Spread-out words catch bad placement or truncation.
    let mut offsets = vec![0, data.len() / 8 * 4, (data.len() - 4) / 4 * 4];
    offsets.dedup();
    for offset in offsets {
        let expected = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
        let got = read_word(dev, address + offset as u32)?;
        if got != expected {
            return Err(Error::Device(format!(
                "Upload readback mismatch at {:#x}: {got:#x} != {expected:#x}",
                address + offset as u32)));
        }
    }
    Ok(())
}

/// Sends JUMP and waits for the HAB report the ROM sends before it
/// jumps. Returns every reply read, for diagnostics.
pub fn jump(dev: &mut impl Hid, address: u32) -> Result<Vec<Vec<u8>>> {
    command(dev, JUMP_ADDRESS, address, 0)?;
    let mut replies = Vec::new();
    for _ in 0..8 {
        let reply = dev.read(1000)?;
        let hab = reply.first() == Some(&REPORT_HAB);
        replies.push(reply);
        if hab {
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

    #[test]
    fn a_word_that_did_not_land_fails_the_spot_check() {
        let ack: &[u8] = &[4, 0x88, 0x88, 0x88, 0x88];
        let mut device = scripted(&[ack, &[4, 1, 2, 3, 4], &[4, 0, 0, 0, 0]]);
        let data: Vec<u8> = (1..=64).collect();
        let error = write_checked(&mut device, UBOOT_BASE, &data).unwrap_err();
        assert!(error.to_string().contains(&format!("mismatch at {:#x}", UBOOT_BASE + 32)), "{error}");
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
        let mut device = scripted(&[&[4, 0, 0, 0, 0], &[3, 0x12, 0x34, 0x34, 0x12]]);
        assert_eq!(jump(&mut device, UBOOT_BASE).unwrap().len(), 2);
        assert_eq!(&device.writes[0][1..7], &[0x0b, 0x0b, 0x87, 0x80, 0x00, 0x00]);
        assert!(matches!(jump(&mut scripted(&[]), UBOOT_BASE), Err(Error::Timeout(_))));
    }
}
