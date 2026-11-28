//! The stand-in on the gen E: a relay between bluez and the BCM43341, where
//! nlclient scans for the sensor's beacons itself, through bluez on hci0, and
//! Matter commissions over BLE. Stock hciattach brings the chip up at boot (its
//! patch RAM, address, 115200 baud with RTS/CTS) from the UART /dev/ttyble0
//! names. The stand-in points that name at a PTY (the holder port.rs keeps for
//! the gen 3's, which outlives nestlocal), attaches the PTY as hci0 in place
//! of the chip's UART, and passes H4 packets whole between the two: while
//! nlclient scans, the sensor's beacons join the chip's advertising reports.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use super::beacon;

/// H4's packet types.
const COMMAND: u8 = 0x01;
const ACL: u8 = 0x02;
const SCO: u8 = 0x03;
const EVENT: u8 = 0x04;
const LE_META: u8 = 0x3e;
const ADVERTISING_REPORT: u8 = 0x02;
const LE_SET_SCAN_ENABLE: u16 = 0x200c;

/// The length of the H4 packet `pending` starts with, once its header is
/// there. A byte no packet starts with is one of its own, passed on as it is.
fn length(pending: &[u8]) -> Option<usize> {
    let header = |size: usize, length: usize| (pending.len() >= size).then_some(size + length);
    let byte = |at: usize| pending.get(at).copied().map_or(0, usize::from);
    match *pending.first()? {
        COMMAND => header(4, byte(3)),
        ACL => header(5, byte(3) | byte(4) << 8),
        SCO => header(4, byte(3)),
        EVENT => header(3, byte(2)),
        _ => Some(1),
    }
}

/// An H4 stream, in any pieces, as whole packets.
#[derive(Default)]
struct Framer {
    pending: Vec<u8>,
}

impl Framer {
    fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(bytes);
        let mut packets = Vec::new();
        while let Some(length) = length(&self.pending).filter(|&length| self.pending.len() >= length) {
            packets.push(self.pending.drain(..length).collect());
        }
        packets
    }
}

/// Both directions between bluez and the chip. What goes to bluez is whole
/// packets, so a beacon can go between any two writes.
#[derive(Default)]
pub struct Relay {
    host: Framer,
    chip: Framer,
    scanning: bool,
}

impl Relay {
    /// Whether the host scans for advertisements.
    pub fn scanning(&self) -> bool {
        self.scanning
    }

    /// The host's bytes, for the chip.
    pub fn for_chip(&mut self, bytes: &[u8]) -> Vec<u8> {
        let packets = self.host.push(bytes);
        for packet in &packets {
            if packet.len() >= 5
                && packet[0] == COMMAND
                && u16::from_le_bytes([packet[1], packet[2]]) == LE_SET_SCAN_ENABLE
            {
                self.scanning = packet[4] == 1;
            }
        }
        packets.concat()
    }

    /// The chip's bytes, for the host.
    pub fn for_host(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.chip.push(bytes).concat()
    }
}

/// The sensor's beacon as an LE advertising report.
pub fn advertisement(report: &[u8; beacon::REPORT_LEN]) -> Vec<u8> {
    // The report's header (type, address type, address, length), as many
    // data bytes as its length says, then its RSSI.
    let length = usize::from(report[8]);
    let mut body = vec![ADVERTISING_REPORT, 1];
    body.extend_from_slice(&report[..9 + length]);
    body.push(report[beacon::REPORT_LEN - 1]);
    let mut out = vec![EVENT, LE_META, body.len() as u8];
    out.extend_from_slice(&body);
    out
}

/// Whether a device's uevent gives it the device tree alias `alias`.
fn has_alias(uevent: &str, alias: &str) -> bool {
    uevent.lines().filter_map(|line| line.strip_prefix("OF_ALIAS_")?.split_once('=')).any(|(_, name)| name == alias)
}

#[cfg(target_os = "linux")]
mod unit {
    use std::ffi::CString;
    use std::fs;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::Arc;

    use super::{advertisement, has_alias, Relay};
    use crate::device::Hci;
    use crate::logf;
    use crate::sensor::{port, Sensor};

    /// The terminal line discipline, linux/tty.h's N_TTY.
    const N_TTY: libc::c_int = 0;

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

    /// The PTY attached as the host's controller, in place of the chip's UART
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
        logf!("external temperature: hci0 is attached on {pty}, relayed to the chip");
        Ok(())
    }

    /// The chip's UART: the tty whose device has the alias.
    fn chip_uart(alias: &str) -> io::Result<PathBuf> {
        for entry in fs::read_dir("/sys/class/tty")? {
            let entry = entry?;
            if has_alias(&fs::read_to_string(entry.path().join("device/uevent")).unwrap_or_default(), alias) {
                return Ok(Path::new("/dev").join(entry.file_name()));
            }
        }
        Err(io::Error::new(io::ErrorKind::NotFound, format!("no UART has the alias {alias}")))
    }

    /// The chip's UART as stock hciattach leaves it: raw, 115200 baud, RTS/CTS.
    fn open_chip(path: &Path) -> io::Result<OwnedFd> {
        let name = CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
        // SAFETY: name is a valid path; the descriptor is owned at once.
        let fd =
            unsafe { libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd was just opened.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // A hciattach not yet gone would keep the HCI line discipline on it.
        let discipline: libc::c_int = N_TTY;
        // SAFETY: termios is plain data for tcgetattr to fill; fd is a tty.
        let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
        let set = unsafe {
            libc::ioctl(fd.as_raw_fd(), libc::TIOCSETD, &discipline) < 0
                || libc::tcgetattr(fd.as_raw_fd(), &mut termios) < 0
                || {
                    libc::cfmakeraw(&mut termios);
                    termios.c_cflag |= libc::CRTSCTS | libc::CLOCAL | libc::CREAD;
                    libc::cfsetspeed(&mut termios, libc::B115200) < 0
                }
                || libc::tcsetattr(fd.as_raw_fd(), libc::TCSANOW, &termios) < 0
        };
        if set {
            return Err(io::Error::last_os_error());
        }
        Ok(fd)
    }

    /// Reads what `fd` has, into `buffer`.
    fn read<'a>(fd: &OwnedFd, buffer: &'a mut [u8]) -> &'a [u8] {
        // SAFETY: buffer is writable for its length.
        let read = unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        &buffer[..read.max(0) as usize]
    }

    /// `inherited`: an earlier nestlocal relayed hci0, which may be scanning
    /// already, without a scan enable for this one to see. Beacons sent while
    /// it is not are lost, and repeated.
    fn run(sensor: Arc<Sensor>, master: OwnedFd, chip: OwnedFd, wake: OwnedFd, inherited: bool) {
        let mut relay = Relay { scanning: inherited, ..Relay::default() };
        let mut buffer = [0u8; 4096];
        let mut was_scanning = relay.scanning();
        loop {
            let mut fds = [master.as_raw_fd(), chip.as_raw_fd(), wake.as_raw_fd()].map(|fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
            // SAFETY: fds is valid for its length.
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 1000) };
            if fds[2].revents & libc::POLLIN != 0 {
                while !read(&wake, &mut buffer).is_empty() {}
            }
            if fds[0].revents & libc::POLLIN != 0 {
                let out = relay.for_chip(read(&master, &mut buffer));
                port::write_all(chip.as_raw_fd(), &out);
            }
            if fds[1].revents & libc::POLLIN != 0 {
                let out = relay.for_host(read(&chip, &mut buffer));
                port::write_all(master.as_raw_fd(), &out);
            }
            if relay.scanning() != was_scanning {
                was_scanning = relay.scanning();
                logf!(
                    "external temperature: nlclient {} for the sensor",
                    if was_scanning { "is listening" } else { "stopped listening" }
                );
            }
            if relay.scanning() {
                if let Some(report) = sensor.due() {
                    port::write_all(master.as_raw_fd(), &advertisement(&report));
                }
            } else {
                sensor.quiet_tick();
            }
        }
    }

    /// Puts the PTY in place of the chip's UART, then relays between them.
    pub fn start(sensor: Arc<Sensor>, hci: &Hci) -> io::Result<()> {
        let (master, inherited) = port::fetch_master()?;
        let uart = chip_uart(hci.alias)?;
        attach(&fs::read_link(hci.uart)?)?;
        let chip = open_chip(&uart)?;
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
        std::thread::Builder::new()
            .name("hci-relay".into())
            .spawn(move || run(sensor, master, chip, wake, inherited))?;
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
    fn host_packets_reach_the_chip_whole_and_unchanged() {
        let mut relay = Relay::default();
        let mut bytes = vec![0xff];
        bytes.extend([ACL, 0x01, 0x00, 0x02, 0x00, 0xaa, 0xbb]);
        bytes.extend(command(LE_SET_SCAN_ENABLE, &[1, 0]));
        let (first, rest) = bytes.split_at(4);
        let mut out = relay.for_chip(first);
        assert_eq!(out, [0xff], "a stray byte passes as it is");
        assert!(!relay.scanning());
        out.extend(relay.for_chip(rest));
        assert_eq!(out, bytes);
        assert!(relay.scanning());
        relay.for_chip(&command(LE_SET_SCAN_ENABLE, &[0, 0]));
        assert!(!relay.scanning());
    }

    #[test]
    fn chip_packets_reach_the_host_only_whole() {
        let mut relay = Relay::default();
        let event = [EVENT, 0x0e, 4, 1, 0x03, 0x0c, 0];
        let acl = [ACL, 0x40, 0x20, 0x03, 0x00, 1, 2, 3];
        let sco = [SCO, 0x01, 0x00, 0x01, 9];
        let mut bytes = event.to_vec();
        bytes.extend(acl);
        bytes.extend(sco);
        for split in 0..bytes.len() {
            let mut out = relay.for_host(&bytes[..split]);
            let held = out.len();
            assert!(
                [0, event.len(), event.len() + acl.len()].contains(&held),
                "{split} bytes in, {held} out, mid-packet"
            );
            out.extend(relay.for_host(&bytes[split..]));
            assert_eq!(out, bytes);
        }
    }

    #[test]
    fn a_beacon_goes_out_as_one_advertising_report() {
        let report = beacon::report(0x18b4_3000_1234_5678, &[7; 16], 42, 215);
        let event = advertisement(&report);
        let length = usize::from(report[8]);
        assert_eq!(event[..5], [EVENT, LE_META, (2 + 9 + length + 1) as u8, ADVERTISING_REPORT, 1]);
        assert_eq!(event[5..5 + 9 + length], report[..9 + length]);
        assert_eq!(*event.last().unwrap(), report[beacon::REPORT_LEN - 1]);
        assert_eq!(event.len(), 3 + usize::from(event[2]));
    }

    #[test]
    fn the_chip_uart_is_found_by_any_of_its_aliases() {
        let uevent = "DRIVER=imx-uart\nOF_NAME=serial\nOF_ALIAS_0=serial1\nOF_ALIAS_1=serial-ble0\n";
        assert!(has_alias(uevent, "serial-ble0"));
        assert!(has_alias(uevent, "serial1"));
        assert!(!has_alias(uevent, "serial-ble"));
        assert!(!has_alias("OF_NAME=serial-ble0\n", "serial-ble0"));
    }
}
