//! The stand-in on the gen E: a Bluetooth controller of its own where nlclient
//! looks for the BCM43341's. nlclient there scans for the sensor's beacons
//! itself, through bluez on hci0, which hciattach brings up from the UART
//! /dev/ttyble0 names. The stand-in points that name at a PTY (the holder
//! port.rs keeps for the gen 3's, which outlives nestlocal), attaches it in
//! place of the real chip, and answers on it as an LE-only controller: each
//! command completes, and while nlclient scans, the sensor's beacons arrive as
//! advertising reports.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use super::beacon;

/// H4's packet types.
const COMMAND: u8 = 0x01;
const ACL: u8 = 0x02;
const EVENT: u8 = 0x04;
/// The events the controller sends.
const COMMAND_COMPLETE: u8 = 0x0e;
const LE_META: u8 = 0x3e;
const ADVERTISING_REPORT: u8 = 0x02;
const LE_SET_SCAN_ENABLE: u16 = 0x200c;
/// The controller's address, 18:B4:30:00:00:01 (Nest's OUI), as the wire
/// carries it.
const ADDRESS: [u8; 6] = [0x01, 0x00, 0x00, 0x30, 0xb4, 0x18];

/// What each command the kernel and bluetoothd send returns after its status:
/// an LE-only Bluetooth 4.0 controller, Broadcom's, with nothing stored.
fn returns(opcode: u16) -> Vec<u8> {
    match opcode {
        // Read Local Version: HCI 4.0, revision, LMP 4.0, Broadcom, subversion.
        0x1001 => vec![0x06, 0x00, 0x00, 0x06, 0x0f, 0x00, 0x00, 0x00],
        // Read Local Supported Commands: none the kernel looks for.
        0x1002 => vec![0; 64],
        // Read Local Supported Features: LE, and no BR/EDR.
        0x1003 => vec![0, 0, 0, 0, 0x60, 0, 0, 0],
        // Read Local Extended Features: page 0 of 0.
        0x1004 => vec![0, 0, 0, 0, 0, 0, 0x60, 0, 0, 0],
        // Read Buffer Size: ACL 251 bytes, 8 packets; no SCO.
        0x1005 => vec![0xfb, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00],
        0x1009 => ADDRESS.to_vec(),
        // Read Local Name.
        0x0c14 => vec![0; 248],
        // LE Read Buffer Size: 251 bytes, 8 packets.
        0x2002 => vec![0xfb, 0x00, 0x08],
        // LE Read Local Supported Features, LE Read Supported States.
        0x2003 | 0x201c => vec![0; 8],
        // LE Read Advertising Channel TX Power, LE Read White List Size.
        0x2007 => vec![0],
        0x200f => vec![8],
        _ => Vec::new(),
    }
}

/// The controller: H4 from the host in, H4 back.
#[derive(Default)]
pub struct Controller {
    pending: Vec<u8>,
    scanning: bool,
}

impl Controller {
    /// Whether the host scans for advertisements.
    pub fn scanning(&self) -> bool {
        self.scanning
    }

    /// Takes the host's bytes, in any pieces, and returns the answers to the
    /// commands among them. ACL data has no one to go to.
    pub fn received(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let (length, command) = match self.pending.first() {
                None => break,
                Some(&COMMAND) if self.pending.len() >= 4 => (4 + usize::from(self.pending[3]), true),
                Some(&ACL) if self.pending.len() >= 5 => {
                    (5 + usize::from(u16::from_le_bytes([self.pending[3], self.pending[4]])), false)
                }
                Some(&COMMAND | &ACL) => break,
                // A byte H4 has no use for: dropped, to find the next packet.
                Some(_) => {
                    self.pending.remove(0);
                    continue;
                }
            };
            if self.pending.len() < length {
                break;
            }
            let packet: Vec<u8> = self.pending.drain(..length).collect();
            if command {
                out.extend(self.command(u16::from_le_bytes([packet[1], packet[2]]), &packet[4..]));
            }
        }
        out
    }

    fn command(&mut self, opcode: u16, parameters: &[u8]) -> Vec<u8> {
        if opcode == LE_SET_SCAN_ENABLE {
            self.scanning = parameters.first() == Some(&1);
        }
        let mut body = vec![1];
        body.extend_from_slice(&opcode.to_le_bytes());
        body.push(0);
        body.extend(returns(opcode));
        event(COMMAND_COMPLETE, &body)
    }

    /// The sensor's beacon as an LE advertising report.
    pub fn advertisement(report: &[u8; beacon::REPORT_LEN]) -> Vec<u8> {
        // The report's header (type, address type, address, length), as
        // many data bytes as its length says, then its RSSI.
        let length = usize::from(report[8]);
        let mut body = vec![ADVERTISING_REPORT, 1];
        body.extend_from_slice(&report[..9 + length]);
        body.push(report[beacon::REPORT_LEN - 1]);
        event(LE_META, &body)
    }
}

fn event(code: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![EVENT, code, body.len() as u8];
    out.extend_from_slice(body);
    out
}

#[cfg(target_os = "linux")]
mod unit {
    use std::fs;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::Arc;

    use super::Controller;
    use crate::logf;
    use crate::sensor::{port, Sensor};

    /// The hciattach processes running, each with its command line.
    fn hciattach() -> Vec<(i32, String)> {
        crate::pids("hciattach")
            .into_iter()
            .map(|pid| {
                let line = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                (pid, String::from_utf8_lossy(&line).replace('\0', " "))
            })
            .collect()
    }

    /// The PTY attached as the host's controller, in place of the real chip
    /// hciattach brought up at boot. One already attached, by an earlier
    /// nestlocal, stays.
    fn attach(pty: &Path) -> io::Result<()> {
        let pty = pty.display().to_string();
        let running = hciattach();
        if running.iter().any(|(_, line)| line.split(' ').any(|word| word == pty)) {
            return Ok(());
        }
        for (pid, _) in running {
            // SAFETY: kill takes any pid and signal.
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        Command::new("setsid")
            .args(["hciattach", "-n", &pty, "any", "115200", "noflow"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        logf!("external temperature: the stand-in controller is attached on {pty}");
        Ok(())
    }

    fn run(sensor: Arc<Sensor>, master: OwnedFd, wake: OwnedFd) {
        let mut controller = Controller::default();
        let mut buffer = [0u8; 4096];
        let mut was_scanning = false;
        loop {
            let mut fds = [
                libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            ];
            // SAFETY: fds is valid for its length.
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 1000) };
            if fds[1].revents & libc::POLLIN != 0 {
                // SAFETY: buffer is writable for its length.
                while unsafe { libc::read(wake.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) } > 0 {}
            }
            if fds[0].revents & libc::POLLIN != 0 {
                // SAFETY: buffer is writable for its length.
                let read = unsafe { libc::read(master.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
                if read > 0 {
                    let answer = controller.received(&buffer[..read as usize]);
                    port::write_all(master.as_raw_fd(), &answer);
                }
            }
            if controller.scanning() != was_scanning {
                was_scanning = controller.scanning();
                logf!(
                    "external temperature: nlclient {} for the sensor",
                    if was_scanning { "is listening" } else { "stopped listening" }
                );
            }
            if controller.scanning() {
                if let Some(report) = sensor.due() {
                    port::write_all(master.as_raw_fd(), &Controller::advertisement(&report));
                }
            } else {
                sensor.quiet_tick();
            }
        }
    }

    /// Puts the controller in place of the chip at `link`, then runs it.
    pub fn start(sensor: Arc<Sensor>, link: &str) -> io::Result<()> {
        let (master, _) = port::fetch_master()?;
        attach(&fs::read_link(link)?)?;
        let mut ends = [0; 2];
        // SAFETY: ends is writable for two descriptors.
        if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: pipe2 made both, and they are owned from here on.
        let (wake, signal) = unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
        let signal = signal.into_raw_fd();
        sensor.set_notify(move || {
            // SAFETY: the pipe lives as long as the process; one byte is readable.
            unsafe { libc::write(signal, b"x".as_ptr().cast(), 1) };
        });
        std::thread::Builder::new().name("hci-stand-in".into()).spawn(move || run(sensor, master, wake))?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub use unit::start;

#[cfg(test)]
mod tests {
    use super::*;

    fn command(opcode: u16, parameters: &[u8]) -> Vec<u8> {
        let mut out = vec![COMMAND];
        out.extend_from_slice(&opcode.to_le_bytes());
        out.push(parameters.len() as u8);
        out.extend_from_slice(parameters);
        out
    }

    #[test]
    fn every_command_completes_with_what_the_kernel_reads() {
        let mut controller = Controller::default();
        assert_eq!(controller.received(&command(0x0c03, &[])), [EVENT, COMMAND_COMPLETE, 4, 1, 0x03, 0x0c, 0]);
        let version = controller.received(&command(0x1001, &[]));
        assert_eq!(version[2], 4 + 8);
        let features = controller.received(&command(0x1003, &[]));
        assert_eq!(features[7 + 4], 0x60, "LE without BR/EDR");
        assert_eq!(controller.received(&command(0x1009, &[]))[7..], ADDRESS);
        assert_eq!(controller.received(&command(0x1002, &[])).len(), 7 + 64);
    }

    #[test]
    fn packets_arrive_in_any_pieces_and_acl_and_stray_bytes_are_dropped() {
        let mut controller = Controller::default();
        let mut bytes = vec![0x00, 0xff];
        bytes.extend([ACL, 0x01, 0x00, 0x02, 0x00, 0xaa, 0xbb]);
        bytes.extend(command(LE_SET_SCAN_ENABLE, &[1, 0]));
        let (first, rest) = bytes.split_at(6);
        assert!(controller.received(first).is_empty());
        let answer = controller.received(rest);
        assert_eq!(answer[..2], [EVENT, COMMAND_COMPLETE]);
        assert!(controller.scanning());
        controller.received(&command(LE_SET_SCAN_ENABLE, &[0, 0]));
        assert!(!controller.scanning());
    }

    #[test]
    fn a_beacon_goes_out_as_one_advertising_report() {
        let report = beacon::report(0x18b4_3000_1234_5678, &[7; 16], 42, 215);
        let event = Controller::advertisement(&report);
        let length = usize::from(report[8]);
        assert_eq!(event[..5], [EVENT, LE_META, (2 + 9 + length + 1) as u8, ADVERTISING_REPORT, 1]);
        assert_eq!(event[5..5 + 9 + length], report[..9 + length]);
        assert_eq!(*event.last().unwrap(), report[beacon::REPORT_LEN - 1]);
        assert_eq!(event.len(), 3 + usize::from(event[2]));
    }
}
