//! The radio's SPI protocol (Silicon Labs' EZSP-SPI "SPIP"): the EM35x is an
//! SPI slave with three more lines, nRESET and nWAKE to it (nWAKE where the
//! device wires it), nHOST_INT from it; the device says where they are.
//!
//! A transaction is one chip select: the command and its terminator, then
//! 0xFF clocked until the radio answers (it pulls nHOST_INT low when it
//! has), then the answer and its terminator. The kernel drives chip select,
//! so it is held across the transaction's spidev messages with cs_change.
//! nHOST_INT low with no transaction under way means the radio has
//! something to tell (an EZSP callback, or that it has just reset).

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::thread::sleep;
use std::time::{Duration, Instant};

const SPEED_HZ: u32 = 1_000_000;

const TERMINATOR: u8 = 0xA7;
const EZSP_FRAME: u8 = 0xFE;
const VERSION: u8 = 0x0A;
const ALIVE: u8 = 0x0B;
const VERSION_2: u8 = 0x82;
const ALIVE_OK: u8 = 0xC1;
/// The longest EZSP frame the radio takes or gives.
pub const MAX_FRAME: usize = 133;

/// How long the radio may take to answer, and to come out of reset.
const ANSWER: Duration = Duration::from_millis(300);
const STARTUP: Duration = Duration::from_millis(7500);

#[repr(C)]
#[derive(Default)]
struct Transfer {
    tx_buf: u64,
    rx_buf: u64,
    len: u32,
    speed_hz: u32,
    delay_usecs: u16,
    bits_per_word: u8,
    cs_change: u8,
    tx_nbits: u8,
    rx_nbits: u8,
    word_delay_usecs: u8,
    pad: u8,
}

// spidev's ioctls: _IOW('k', n, size).
const SPI_IOC_MESSAGE_1: u32 = 0x4020_6b00;
const SPI_IOC_WR_MODE: u32 = 0x4001_6b01;
const SPI_IOC_WR_BITS_PER_WORD: u32 = 0x4001_6b03;
const SPI_IOC_WR_MAX_SPEED_HZ: u32 = 0x4004_6b04;

pub struct Spi {
    dev: File,
    host_int: File,
    reset: File,
    wake: Option<File>,
    /// Tests answer transactions from this script instead of the radio.
    #[cfg(test)]
    script: std::collections::VecDeque<Result<Vec<u8>, String>>,
}

fn gpio(line: &str, write: bool) -> std::io::Result<File> {
    OpenOptions::new().read(true).write(write).open(format!("{line}/value"))
}

/// nHOST_INT is low, read without opening the radio.
pub fn host_int_low() -> bool {
    let mut level = *b"1";
    gpio(crate::device::current().radio.host_int, false).and_then(|mut f| f.read(&mut level)).is_ok() && level[0] == b'0'
}

fn io(what: &str) -> impl FnOnce(std::io::Error) -> String + '_ {
    move |e| format!("{what}: {e}")
}

impl Spi {
    pub fn host_int_fd(&self) -> i32 {
        self.host_int.as_raw_fd()
    }

    pub fn open() -> Result<Spi, String> {
        let radio = &crate::device::current().radio;
        let dev = OpenOptions::new().read(true).write(true).open(radio.spi).map_err(io(radio.spi))?;
        let fd = dev.as_raw_fd();
        let (mode, bits, speed) = (0u8, 8u8, SPEED_HZ);
        // SAFETY: each ioctl reads one value of the size its number encodes.
        let ok = unsafe {
            libc::ioctl(fd, SPI_IOC_WR_MODE as _, &mode) >= 0
                && libc::ioctl(fd, SPI_IOC_WR_BITS_PER_WORD as _, &bits) >= 0
                && libc::ioctl(fd, SPI_IOC_WR_MAX_SPEED_HZ as _, &speed) >= 0
        };
        if !ok {
            return Err(format!("{}: {}", radio.spi, std::io::Error::last_os_error()));
        }
        // Requests nHOST_INT's interrupt, which poll and waking the unit
        // need; Nest's /etc/init.d/ncp does it for wpantund.
        std::fs::write(format!("{}/edge", radio.host_int), "falling").map_err(io("nHOST_INT edge"))?;
        Ok(Spi {
            #[cfg(test)]
            script: Default::default(),
            dev,
            host_int: gpio(radio.host_int, false).map_err(io("nHOST_INT"))?,
            reset: gpio(radio.reset, true).map_err(io("nRESET"))?,
            wake: radio.wake.map(|line| gpio(line, true)).transpose().map_err(io("nWAKE"))?,
        })
    }

    /// nHOST_INT is low: the radio has something to say.
    pub fn host_int(&mut self) -> bool {
        let mut level = *b"1";
        let _ = self.host_int.seek(SeekFrom::Start(0));
        let _ = self.host_int.read(&mut level);
        level[0] == b'0'
    }

    /// Waits for nHOST_INT, up to `timeout`, on its falling-edge interrupt.
    pub fn wait_host_int(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.host_int() {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            let mut fd = libc::pollfd { fd: self.host_int_fd(), events: libc::POLLPRI | libc::POLLERR, revents: 0 };
            // SAFETY: one pollfd, for the duration of the call.
            unsafe { libc::poll(&mut fd, 1, left.as_millis().clamp(1, 1000) as i32) };
        }
    }

    /// Resets the radio and runs the protocol's startup checks: the first
    /// answer is the reset report, then it must be alive and speak version 2.
    pub fn reset(&mut self) -> Result<(), String> {
        let line = |file: &mut File, high: bool| file.write_all(if high { b"1" } else { b"0" });
        if let Some(wake) = &mut self.wake {
            line(wake, true).map_err(io("nWAKE"))?;
        }
        line(&mut self.reset, false).map_err(io("nRESET"))?;
        // A host killed mid-transaction leaves chip select held, and the radio
        // would boot into a half transaction: release it while the radio is in reset.
        self.xfer(&[0xFF], false)?;
        sleep(Duration::from_millis(1));
        line(&mut self.reset, true).map_err(io("nRESET"))?;
        self.startup()
    }

    /// The radio boots as a Thread RCP; its command 6 is answered with a
    /// reboot into the Zigbee NCP, which then reports a reset as after nRESET.
    pub fn select_zigbee(&mut self) -> Result<(), String> {
        match self.ezsp(&[6])?.as_slice() {
            [0x52, 2, _] => {}
            other => return Err(format!("the radio did not take the Zigbee selection: {other:02x?}")),
        }
        // The RCP reboots within a millisecond of the answer leaving; until
        // then its own pending data can hold nHOST_INT low like the bootloader.
        sleep(Duration::from_millis(20));
        self.startup()
    }

    /// The protocol's startup checks: the first answer is the reset report,
    /// then the radio must be alive and speak version 2.
    fn startup(&mut self) -> Result<(), String> {
        // nHOST_INT reads high for about 430 ms after a reset, then low once
        // the firmware is ready; a line still low from before the reset is waited out.
        let deadline = Instant::now() + STARTUP;
        while self.host_int() {
            if Instant::now() > deadline {
                return Err("the radio did not start its firmware after reset".into());
            }
            sleep(Duration::from_millis(5));
        }
        // The radio's first answers can still be lost while it boots; the
        // handshake is retried, and a reset report lost with it is not required again.
        let mut retried = false;
        loop {
            if !self.wait_host_int(deadline.saturating_duration_since(Instant::now())) {
                return Err("the radio did not come up after reset".into());
            }
            let handshake = match self.transaction(&[ALIVE]) {
                Ok(answer) if matches!(answer.as_slice(), [0x00, _]) => self.alive(),
                Ok(answer) if retried && answer == [ALIVE_OK] => self.alive(),
                // The protocol's report of the attempt that failed: try again.
                Ok(answer) if retried && matches!(answer.first(), Some(0x01..=0x04)) => {
                    Err(format!("SPI protocol error {answer:02x?}"))
                }
                Ok(other) => return Err(format!("expected the radio's reset report, got {other:02x?}")),
                Err(e) => Err(e),
            };
            match handshake {
                Ok(()) => return Ok(()),
                Err(_) if Instant::now() < deadline => {
                    retried = true;
                    sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// After the reset report: the radio must be alive and speak version 2.
    fn alive(&mut self) -> Result<(), String> {
        if self.transaction(&[ALIVE])? != [ALIVE_OK] {
            return Err("the radio is not alive".into());
        }
        match self.transaction(&[VERSION])?.as_slice() {
            [VERSION_2] => Ok(()),
            other => Err(format!("the radio speaks SPI protocol {other:02x?}, not EZSP-SPI version 2")),
        }
    }

    /// One EZSP frame out, its answer back.
    pub fn ezsp(&mut self, frame: &[u8]) -> Result<Vec<u8>, String> {
        if frame.len() > MAX_FRAME {
            return Err(format!("EZSP frame of {} bytes", frame.len()));
        }
        let mut command = vec![EZSP_FRAME, frame.len() as u8];
        command.extend_from_slice(frame);
        let answer = self.transaction(&command)?;
        match answer.as_slice() {
            [EZSP_FRAME, _, rest @ ..] => Ok(rest.to_vec()),
            [0x00, cause] => Err(format!("the radio reset (cause {cause:#04x})")),
            [error, detail] if *error < 0x05 => Err(format!("SPI protocol error {error:#04x} ({detail:#04x})")),
            other => Err(format!("unexpected answer {other:02x?}")),
        }
    }

    /// A command, and the answer as the radio sends it (its terminator off).
    fn transaction(&mut self, command: &[u8]) -> Result<Vec<u8>, String> {
        #[cfg(test)]
        if let Some(answer) = self.script.pop_front() {
            return answer;
        }
        let mut out = command.to_vec();
        out.push(TERMINATOR);
        self.xfer(&out, true)?;
        let answer = self.answer();
        // Chip select back up, and the protocol's breathing space between
        // transactions.
        let end = self.xfer(&[0xFF], false);
        sleep(Duration::from_millis(1));
        let answer = answer?;
        if end? != [TERMINATOR] {
            return Err(format!("answer {answer:02x?} without its terminator"));
        }
        Ok(answer)
    }

    /// The answer, after the command: 0xFF until the radio has it ready.
    fn answer(&mut self) -> Result<Vec<u8>, String> {
        if !self.wait_host_int(ANSWER) {
            return Err("no answer from the radio".into());
        }
        let start = Instant::now();
        let first = loop {
            let byte = self.xfer(&[0xFF], true)?[0];
            if byte != 0xFF {
                break byte;
            }
            if start.elapsed() > ANSWER {
                return Err("no answer from the radio".into());
            }
            sleep(Duration::from_millis(1));
        };
        let mut answer = vec![first];
        match first {
            EZSP_FRAME => {
                let len = self.xfer(&[0xFF], true)?[0];
                if len as usize > MAX_FRAME {
                    return Err(format!("EZSP frame of {len} bytes"));
                }
                answer.push(len);
                answer.extend(self.xfer(&vec![0xFF; len as usize], true)?);
            }
            0x00..=0x04 => answer.push(self.xfer(&[0xFF], true)?[0]),
            _ => {}
        }
        Ok(answer)
    }

    /// One spidev message; `hold` keeps chip select down after it.
    fn xfer(&mut self, tx: &[u8], hold: bool) -> Result<Vec<u8>, String> {
        let mut rx = vec![0u8; tx.len()];
        let t = Transfer {
            tx_buf: tx.as_ptr() as u64,
            rx_buf: rx.as_mut_ptr() as u64,
            len: tx.len() as u32,
            speed_hz: SPEED_HZ,
            bits_per_word: 8,
            cs_change: hold as u8,
            ..Default::default()
        };
        // SAFETY: the transfer's buffers outlive the call and are len long.
        if unsafe { libc::ioctl(self.dev.as_raw_fd(), SPI_IOC_MESSAGE_1 as _, &t) } < 0 {
            return Err(format!("{}: {}", crate::device::current().radio.spi, std::io::Error::last_os_error()));
        }
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_ioctl_encodes_the_size_of_what_it_passes() {
        let size = |ioctl: u32| ((ioctl >> 16) & 0x3FFF) as usize;
        assert_eq!(size(SPI_IOC_MESSAGE_1), std::mem::size_of::<Transfer>());
        assert_eq!(size(SPI_IOC_WR_MODE), std::mem::size_of::<u8>());
        assert_eq!(size(SPI_IOC_WR_BITS_PER_WORD), std::mem::size_of::<u8>());
        assert_eq!(size(SPI_IOC_WR_MAX_SPEED_HZ), std::mem::size_of_val(&SPEED_HZ));
    }

    /// An Spi over plain files, nHOST_INT's value in `level`.
    fn fake(level: &std::path::Path) -> Spi {
        let file = || OpenOptions::new().read(true).write(true).open(level).unwrap();
        Spi { dev: file(), host_int: file(), reset: file(), wake: Some(file()), script: Default::default() }
    }

    /// A radio coming out of a boot: nHOST_INT high, then low once it is ready.
    fn booting(name: &str, answers: Vec<Result<Vec<u8>, String>>) -> (Spi, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!("nestlocal-spi-{name}-{}", std::process::id()));
        std::fs::write(&path, "1\n").unwrap();
        let mut spi = fake(&path);
        spi.script = answers.into();
        let ready = path.clone();
        std::thread::spawn(move || {
            sleep(Duration::from_millis(50));
            std::fs::write(ready, "0\n").unwrap();
        });
        (spi, path)
    }

    const REPORT: [u8; 2] = [0x00, 0x09];

    #[test]
    fn startup_takes_the_reset_report_then_checks_alive_and_version() {
        let (mut spi, path) = booting("report", vec![Ok(REPORT.to_vec()), Ok(vec![ALIVE_OK]), Ok(vec![VERSION_2])]);
        assert_eq!(spi.startup(), Ok(()));
        assert!(spi.script.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn startup_retries_answers_lost_while_the_radio_boots() {
        // No answer, then the protocol's report of that aborted attempt, then
        // a radio whose reset report went with the first attempt.
        let (mut spi, path) = booting("lost", vec![Err("no answer from the radio".into()), Ok(vec![0x02, 0x00]),
                                                   Ok(vec![ALIVE_OK]), Ok(vec![ALIVE_OK]), Ok(vec![VERSION_2])]);
        assert_eq!(spi.startup(), Ok(()));
        assert!(spi.script.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn startup_requires_a_reset_report_when_nothing_was_lost() {
        let (mut spi, path) = booting("noreport", vec![Ok(vec![ALIVE_OK])]);
        assert!(spi.startup().unwrap_err().contains("reset report"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn startup_gives_up_on_a_radio_on_another_spi_protocol_at_the_reset_deadline() {
        let (mut spi, path) = booting("version", vec![Ok(REPORT.to_vec()), Ok(vec![ALIVE_OK]), Ok(vec![0x81])]);
        // Every retry finds the radio alive but on SPI protocol version 1.
        for _ in 0..2000 {
            spi.script.extend([Ok(vec![ALIVE_OK]), Ok(vec![ALIVE_OK]), Ok(vec![0x81])]);
        }
        let start = Instant::now();
        assert!(spi.startup().unwrap_err().contains("SPI protocol [81]"));
        assert!(start.elapsed() >= STARTUP - Duration::from_millis(100) && start.elapsed() < STARTUP + Duration::from_secs(1));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn zigbee_is_selected_from_the_rcp_and_its_reboot_awaited() {
        // The RCP answers command 6 as an EZSP frame, then the NCP boots.
        let (mut spi, path) = booting("select", vec![Ok(vec![EZSP_FRAME, 3, 0x52, 2, 0]), Ok(REPORT.to_vec()),
                                                     Ok(vec![ALIVE_OK]), Ok(vec![VERSION_2])]);
        assert_eq!(spi.select_zigbee(), Ok(()));
        assert!(spi.script.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_radio_that_refuses_the_selection_is_reported() {
        let (mut spi, path) = booting("refuse", vec![Ok(vec![EZSP_FRAME, 2, 0x00, 0x58])]);
        assert!(spi.select_zigbee().unwrap_err().contains("did not take the Zigbee selection"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn host_int_is_active_low_and_read_afresh_each_time() {
        let path = std::env::temp_dir().join(format!("nestlocal-spi-test-{}", std::process::id()));
        std::fs::write(&path, "1\n").unwrap();
        let mut spi = fake(&path);
        assert!(!spi.host_int());
        std::fs::write(&path, "0\n").unwrap();
        assert!(spi.host_int());
        assert!(spi.wait_host_int(Duration::from_secs(5)));
        std::fs::write(&path, "1\n").unwrap();
        let start = Instant::now();
        assert!(!spi.wait_host_int(Duration::from_millis(50)));
        assert!(start.elapsed() >= Duration::from_millis(50));
        let _ = std::fs::remove_file(&path);
    }
}
