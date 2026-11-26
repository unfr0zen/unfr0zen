//! The development board's emulated boot ROM over its USB host bridge socket:
//! the same line protocol the emulator's Python helpers speak (SETUP/TOKEN
//! over a Unix socket), so this crate's SDP session can drive the emulated
//! unit instead of a real HID device.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use unfr0zen_core::diamond3::sdp::{Hid, HID_DATA_SIZE};
use unfr0zen_core::{Error, Result};

pub struct QemuRom {
    sock: UnixStream,
    reader: BufReader<UnixStream>,
    timeout: Duration,
}

impl QemuRom {
    /// Enumerates the mask ROM before opening its HID interface.
    pub fn enumerate(&mut self) -> Result<()> {
        self.setup(&[0x80, 6, 0, 1, 0, 0, 18, 0])?;
        let descriptor = self.transfer_in(0, 18, Instant::now() + self.timeout)?;
        self.transfer_out(0, &[])?;
        if descriptor.len() != 18 || descriptor[8..12] != [0xa2, 0x15, 0x7d, 0] {
            return Err(Error::Device("the USB bridge is not an Onyx SDP device".into()));
        }
        self.control_out(&[0, 5, 5, 0, 0, 0, 0, 0], &[])?;
        self.control_out(&[0, 9, 1, 0, 0, 0, 0, 0], &[])
    }

    /// Connects to the bridge socket and waits for the guest to attach.
    pub fn connect(path: &str, timeout: Duration) -> Result<Self> {
        let deadline = Instant::now() + timeout;
        let sock = loop {
            match UnixStream::connect(path) {
                Ok(sock) => break sock,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => return Err(Error::Io(format!("cannot reach {path}: {e}"))),
            }
        };
        let mut rom =
            QemuRom { reader: BufReader::new(sock.try_clone().map_err(|e| Error::Io(e.to_string()))?), sock, timeout };
        rom.expect_event("CONNECT", timeout)?;
        rom.reset()?;
        rom.raw_send("SPEED hs")?;
        Ok(rom)
    }

    fn raw_send(&mut self, line: &str) -> Result<()> {
        #[cfg(test)]
        if std::env::var_os("ONYX_QEMU_TRACE").is_some() {
            eprintln!("USB > {}", &line[..line.len().min(64)]);
        }
        self.sock.write_all(format!("{line}\n").as_bytes()).map_err(|e| Error::Io(e.to_string()))
    }

    fn line(&mut self, deadline: Instant) -> Result<String> {
        let mut line = String::new();
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Timeout("no reply from the USB bridge".into()));
        }
        self.sock.set_read_timeout(Some(remaining)).map_err(|e| Error::Io(e.to_string()))?;
        match self.reader.read_line(&mut line) {
            Ok(0) => Err(Error::Io("USB bridge closed the socket".into())),
            Ok(_) => {
                #[cfg(test)]
                if std::env::var_os("ONYX_QEMU_TRACE").is_some() {
                    eprintln!("USB < {}", &line[..line.len().min(64)]);
                }
                Ok(line.trim_end().to_string())
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                Err(Error::Timeout("no reply from the USB bridge".into()))
            }
            Err(e) => Err(Error::Io(e.to_string())),
        }
    }

    fn expect_event(&mut self, want: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let line = self.line(deadline)?;
            if line.starts_with(want) {
                return Ok(());
            }
        }
    }

    fn reset(&mut self) -> Result<()> {
        self.raw_send("RESET")?;
        self.expect_event("RESETDONE", self.timeout)?;
        Ok(())
    }

    fn setup(&mut self, packet: &[u8]) -> Result<()> {
        self.raw_send(&format!("SETUP {}", hex(packet)))?;
        self.expect_event("SETUPACK", self.timeout)?;
        Ok(())
    }

    fn token(&mut self, ep: u8, direction_in: bool, value: &[u8]) -> Result<(String, Vec<u8>)> {
        let command = if direction_in {
            format!("TOKENIN {ep} {}", value.len())
        } else {
            format!("TOKENOUT {ep} {}", hex(value))
        };
        let deadline = Instant::now() + self.timeout;
        self.raw_send(&command)?;
        loop {
            let line = self.line(deadline)?;
            let parts: Vec<&str> = line.splitn(5, ' ').collect();
            if parts.len() < 4 || parts[0] != "TOKEN" {
                continue;
            }
            let reply_ep = parts[1].parse::<u8>().map_err(|e| Error::Io(e.to_string()))?;
            let reply_in = parts[2] == "IN";
            if reply_ep != ep || reply_in != direction_in {
                return Err(Error::Io(format!("out-of-order TOKEN reply: {line}")));
            }
            let status = parts[3].to_string();
            let payload = parts.get(4).copied().unwrap_or("");
            let bytes = if direction_in { unhex(payload) } else { Vec::new() };
            return Ok((status, bytes));
        }
    }

    fn control_out(&mut self, setup: &[u8], data: &[u8]) -> Result<()> {
        self.setup(setup)?;
        for chunk in data.chunks(64) {
            self.transfer_out(0, chunk)?;
        }
        let deadline = Instant::now() + self.timeout;
        self.transfer_in(0, 0, deadline)?;
        if Instant::now() >= deadline {
            return Err(Error::Timeout("USB control status timed out".into()));
        }
        Ok(())
    }

    fn transfer_out(&mut self, ep: u8, data: &[u8]) -> Result<()> {
        let deadline = Instant::now() + self.timeout;
        loop {
            let (status, _) = self.token(ep, false, data)?;
            if status != "NAK" {
                return expect_ack(&status);
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout("USB OUT transfer timed out".into()));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn transfer_in(&mut self, ep: u8, len: usize, deadline: Instant) -> Result<Vec<u8>> {
        loop {
            if Instant::now() >= deadline {
                return Ok(Vec::new());
            }
            let (status, bytes) = self.token(ep, true, &vec![0u8; len])?;
            match status.as_str() {
                "ACK" => return Ok(bytes),
                "NAK" => std::thread::sleep(Duration::from_millis(2)),
                "STALL" => return Err(Error::Device("endpoint stalled".into())),
                other => return Err(Error::Io(format!("UDC transfer failed: {other}"))),
            }
        }
    }
}

fn expect_ack(status: &str) -> Result<()> {
    match status {
        "ACK" => Ok(()),
        "STALL" => Err(Error::Device("endpoint stalled".into())),
        other => Err(Error::Io(format!("UDC transfer failed: {other}"))),
    }
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2).map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect()
}

impl Hid for QemuRom {
    fn write(&mut self, report: &[u8]) -> Result<usize> {
        // HID SET_REPORT over the default endpoint: class request to the
        // interface, report ID first.
        let value = 0x200 | report[0] as u16;
        let mut setup = Vec::new();
        setup.push(0x21); // class, interface
        setup.push(9); // SET_REPORT
        setup.extend_from_slice(&value.to_le_bytes());
        setup.extend_from_slice(&0u16.to_le_bytes());
        setup.extend_from_slice(&(report.len() as u16).to_le_bytes());
        self.control_out(&setup, report)?;
        Ok(report.len())
    }

    fn read(&mut self, timeout_ms: i32) -> Result<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(0) as u64);
        let mut report = Vec::new();
        loop {
            let packet = self.transfer_in(1, 64, deadline)?;
            let short = packet.len() < 64;
            report.extend(packet);
            if short {
                return Ok(report);
            }
        }
    }
}

/// The emulated unit's HID data report size, for callers sizing buffers.
pub const DATA_SIZE: usize = HID_DATA_SIZE;

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    #[ignore = "requires ONYX_QEMU_USB connected to an emulated Onyx mask ROM"]
    fn signed_loader_enters_recovery_through_the_mask_rom() {
        let path = std::env::var("ONYX_QEMU_USB").expect("ONYX_QEMU_USB");
        let mut rom = QemuRom::connect(&path, Duration::from_secs(15)).unwrap();
        rom.enumerate().unwrap();
        let recovery = crate::onyx::assets::RECOVERY;
        let fdt = recovery.windows(4).rposition(|bytes| bytes == [0xd0, 0x0d, 0xfe, 0xed]).unwrap();
        struct Notes;
        impl crate::Progress for Notes {
            fn begin(&mut self, label: &str) {
                eprintln!("{label}");
            }
            fn end(&mut self, _: &str, _: Duration, _: bool) {}
            fn status(&mut self, message: &str) {
                eprintln!("{message}");
            }
            fn waiting(&mut self, message: &str) {
                eprintln!("{message}");
            }
        }
        crate::onyx::boot::load_recovery(
            &mut rom,
            crate::onyx::assets::OFFICIAL_LOADER,
            &unfr0zen_core::onyx::metadata::template(),
            recovery,
            &recovery[fdt..],
            &mut Notes,
        )
        .unwrap();

        // The recovery FIT booted: the emulated unit now enumerates as the
        // recovery U-Boot's gadget instead of the boot ROM.
        rom.expect_event("CONNECT", Duration::from_secs(30)).unwrap();
        rom.reset().unwrap();
        rom.raw_send("SPEED hs").unwrap();
        rom.setup(&[0x80, 6, 0, 1, 0, 0, 18, 0]).unwrap();
        let descriptor = rom.transfer_in(0, 18, Instant::now() + Duration::from_secs(15)).unwrap();
        assert_eq!(&descriptor[8..10], &crate::onyx::usb::RECOVERY.0.to_le_bytes());
        assert_eq!(&descriptor[10..12], &crate::onyx::usb::RECOVERY.1.to_le_bytes());
    }

    #[test]
    fn connects_and_exchanges_hid_reports_with_async_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usb.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            socket.write_all(b"CONNECT\n").unwrap();
            let packet = format!("CONNECT\nTOKEN 1 IN ACK 0401000000{}\n", "00".repeat(59));
            for (expected, reply) in [
                ("RESET", "DISCONNECT\nRESETDONE\n"),
                ("SPEED hs", ""),
                ("SETUP 2109010200000300", "SETUPACK\n"),
                ("TOKENOUT 0 010203", "TOKEN 0 OUT ACK 3\n"),
                ("TOKENIN 0 0", "TOKEN 0 IN ACK \n"),
                ("TOKENIN 1 64", "TOKEN 1 IN NAK \n"),
                ("TOKENIN 1 64", packet.as_str()),
                ("TOKENIN 1 64", "TOKEN 1 IN ACK 00\n"),
            ] {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line.trim_end(), expected);
                socket.write_all(reply.as_bytes()).unwrap();
            }
        });
        let mut rom = QemuRom::connect(path.to_str().unwrap(), Duration::from_secs(2)).unwrap();
        assert_eq!(rom.write(&[1, 2, 3]).unwrap(), 3);
        let report = rom.read(1000).unwrap();
        assert_eq!(report.len(), 65);
        assert_eq!(&report[..5], &[4, 1, 0, 0, 0]);
        server.join().unwrap();
    }
}

#[cfg(test)]
mod real_unit {
    use std::time::{Duration, Instant};

    /// The full recovery chain against a real unit in download mode using
    /// the three-session delivery: SRC POR → cold boot → warm delivery.
    #[test]
    #[ignore = "requires a real Thermostat E in USB download mode"]
    fn real_unit_recovery_chain() {
        crate::onyx::usb::load_recovery().unwrap();

        // The recovery gadget replaces the ROM's download device.
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut gadget = false;
        while Instant::now() < deadline {
            for (_, (vid, pid)) in crate::usb::list().unwrap() {
                if (vid, pid) == crate::onyx::usb::RECOVERY {
                    gadget = true;
                }
            }
            if gadget {
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        if !gadget && std::env::var_os("ONYX_DIAG").is_some() {
            let mut api = hidapi::HidApi::new().unwrap();
            read_markers(&mut api);
        }
        assert!(gadget, "recovery gadget did not enumerate within 8 s");
    }

    /// The recovery boot's OCRAM stage markers, read from a unit in download
    /// mode. The loader's serial download does not answer reads, so the
    /// session is re-opened until the unit's watchdog has handed the unit
    /// back to the boot ROM, whose reads work; the markers survive the warm
    /// reset in the ocràm.
    pub(crate) fn read_markers(api: &mut hidapi::HidApi) {
        use unfr0zen_core::diamond3::sdp::read_word;
        let markers = [
            ("dram_init", 0x8500_004C),
            ("early_init", 0x8500_0050),
            ("board_init", 0x8500_0054),
            ("late entry", 0x8500_0058),
            ("gadget init", 0x8500_005C),
            ("main_loop state", 0x8500_007C),
            ("bootdelay cmdline", 0x8500_0080),
        ];
        for attempt in 1..=4 {
            let mut rom = crate::common::usb::wait_hid(api, crate::onyx::usb::SDP, Duration::from_secs(90)).unwrap();
            // Only the boot ROM's session answers reads.
            match read_word(&mut rom, 0x8500_004C) {
                Ok(word) => {
                    eprintln!("ROM session on attempt {attempt}: dram_init {word:#010x}");
                    for (name, address) in markers {
                        match read_word(&mut rom, address) {
                            Ok(word) => eprintln!("{name} @ {address:#x}: {word:#010x}"),
                            Err(e) => eprintln!("{name} @ {address:#x}: {e}"),
                        }
                    }
                    // The full exception record, when one was captured:
                    // magic|type, nonce, pc, lr, sp, cpsr, dfsr, dfar, ifsr,
                    // ifar, reloc_off, r0-r3, count.
                    match read_word(&mut rom, 0x8500_0100) {
                        Ok(word) if word & 0xFFFFFF00 == 0xE7C0DE00 => {
                            eprintln!("exception record type {}", word & 0xFF);
                            for (i, address) in (0x8500_0104..0x8500_0144).step_by(4).enumerate() {
                                let word = read_word(&mut rom, address).unwrap_or(0);
                                eprintln!("  exc[{i:2}] @ {address:#x}: {word:#010x}");
                            }
                        }
                        Ok(word) => eprintln!("no exception record ({word:#010x})"),
                        Err(e) => eprintln!("exception record unreadable: {e}"),
                    }
                    return;
                }
                Err(e) => {
                    eprintln!(
                        "attempt {attempt}: session does not answer reads ({e}); \
                               waiting for the watchdog to hand the unit back to the boot rom"
                    );
                    drop(rom);
                    std::thread::sleep(Duration::from_secs(15));
                }
            }
        }
        eprintln!("markers unreadable: no session answered reads");
    }
}

#[cfg(test)]
mod real_diag {
    /// Dumps the previous run's OCRAM markers from a unit that just entered
    /// download mode, without touching anything.
    #[test]
    #[ignore = "requires a real Thermostat E in USB download mode"]
    fn real_unit_diag_only() {
        let mut api = hidapi::HidApi::new().unwrap();
        crate::onyx::qemu::real_unit::read_markers(&mut api);
    }
}
