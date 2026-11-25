//! The real BLE chip behind the stand-in. nlclient drives the chip as it always
//! does, and everything it and the chip say passes through here, so it hears
//! the paired sensors; the stand-in sensors' beacons go in between the chip's
//! messages.
//!
//! What nlclient holds its chip to, and so does this:
//! - Its loader waits 5 s for the checksum after its last write. A UART paces
//!   its writes, but a PTY takes them at once, so only a little is taken from
//!   it ahead of the chip.
//! - It reopens the UART right after the upload and flushes it, which can take
//!   the ACK that starts the firmware along. The ACK comes from here, once the
//!   chip's checksum is right, and the loader's own is dropped.
//! - A byte it does not expect between messages makes it start its BLE over,
//!   firmware upload and all. It gets whole messages of a length it has room
//!   for, and nothing else.
//! - It restarts a scan that has heard no beacon for half an hour. The stand-in
//!   sensors' beacons would keep it from noticing a deaf chip, so this does.

use std::collections::VecDeque;

use super::chip::{
    op, ACK, HEADER, MAX_PAYLOAD, NAK, ROLE_CENTRAL, ROLE_PERIPHERAL, SOH, STX, SUCCESS, TO_CHIP, TO_HOST,
};
use crate::logf;

/// A byte nlclient's BLE reader does not expect between messages: it restarts
/// its BLE, reset line and firmware upload and all.
pub const RESTART_BLE: u8 = 0x02;
/// The loader breaks the line after each reset (tcsendbreak).
pub const BREAK_MS: u64 = 250;
/// Between the ACK that starts the firmware and the first message to it: what
/// nlclient takes to reopen the UART.
pub const SETTLE_MS: u64 = 20;
/// The header that asks for a Nest bootloader's version, and loads nothing.
const VERSION_REQUEST: u16 = 0xff7f;
/// The longest message nlclient has room for.
const MAX_TO_HOST: usize = 150;
/// A message cut short is given up after this long.
const CUT_SHORT_MS: u64 = 500;
/// So many STX in a row, and no message: the chip is back in its boot ROM.
const ROM_STX: u32 = 3;
/// How long the chip may say nothing while it scans for paired sensors, as
/// nlclient's RcsWatchdog has it.
pub const DEAF_MS: u64 = 1_800_000;
/// And how much of that the unit must have been awake for, to have heard it.
const DEAF_AWAKE_MS: u64 = 60_000;
/// Time that passes faster than this between two ticks passed asleep, and what
/// woke the unit may be the chip: this long after, it would have been heard.
const ASLEEP_MS: u64 = 5_000;
/// How long nlclient gets to start its BLE over, and how often it is asked.
const ASK_AGAIN_MS: u64 = 15_000;
const ASK_TIMES: u32 = 3;
/// What may wait for the chip before no more is taken from nlclient, and the
/// other way round.
const CHIP_BACKLOG: usize = 256;
const HOST_BACKLOG: usize = 4096;

/// The UART's speed: the boot ROM's, or the firmware's (115200, with RTS/CTS).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Line {
    Rom,
    Firmware,
}

/// What goes to the chip's side, in order.
#[derive(Debug, PartialEq)]
pub enum ToChip {
    Bytes(VecDeque<u8>),
    /// Change the line, once all before it is out, and give it SETTLE_MS.
    Line(Line),
}

#[derive(Debug, PartialEq)]
enum Mode {
    /// nlclient has not reset the chip yet: nothing passes. It was asked to
    /// `times` times, last at `asked`.
    Unknown {
        asked: Option<u64>,
        times: u32,
    },
    /// Reset, and the line broken until then. What the chip says is kept.
    Held {
        until: u64,
    },
    /// In the boot ROM, which offers to take firmware.
    Rom,
    /// The header is with the chip.
    Header {
        need: usize,
    },
    Image {
        need: usize,
        got: usize,
        xor: u8,
    },
    Checksum {
        xor: u8,
    },
    Firmware,
}

#[derive(Default, Debug, PartialEq, Clone, Copy)]
pub struct Counts {
    pub to_host: u64,
    pub to_chip: u64,
    pub injected: u64,
    pub junk: u64,
}

pub struct Relay {
    mode: Mode,
    /// The ids of the stand-in sensors, low 16 bits.
    ours: Vec<u16>,
    from_host: Vec<u8>,
    from_chip: Vec<u8>,
    from_chip_at: u64,
    to_host: VecDeque<u8>,
    to_chip: VecDeque<ToChip>,
    role: Option<u8>,
    active: bool,
    filter: Vec<u16>,
    stx: u32,
    heard: u64,
    /// The last tick, and how long the unit was awake since the chip was heard.
    ticked: u64,
    awake: u64,
    /// When it last woke.
    woke: u64,
    pub counts: Counts,
}

fn message(header: &[u8; 7], op: u16, body: &[u8]) -> Vec<u8> {
    let mut out = header.to_vec();
    out.extend_from_slice(&((body.len() + 2) as u16).to_le_bytes());
    out.extend_from_slice(&op.to_le_bytes());
    out.extend_from_slice(body);
    out
}

enum Found {
    Message(Vec<u8>),
    Junk(u8),
    /// More bytes are needed.
    Partial,
}

/// What is first in `input`, taken from it. A message's length alone says where
/// the next one starts: its body may hold anything, and an advertisement's is
/// what anyone nearby put on the air.
fn next(input: &mut Vec<u8>, header: &[u8; 7], max: usize) -> Found {
    if input.is_empty() {
        return Found::Partial;
    }
    let known = input.len().min(header.len());
    if input[..known] != header[..known] {
        return Found::Junk(input.remove(0));
    }
    if input.len() < HEADER {
        return Found::Partial;
    }
    let len = usize::from(u16::from_le_bytes([input[7], input[8]]));
    if !(2..=max).contains(&len) {
        return Found::Junk(input.remove(0));
    }
    if input.len() < HEADER + len {
        return Found::Partial;
    }
    Found::Message(input.drain(..HEADER + len).collect())
}

fn parts(message: &[u8]) -> (u16, &[u8]) {
    (u16::from_le_bytes([message[HEADER], message[HEADER + 1]]), &message[HEADER + 2..])
}

impl Relay {
    /// A relay for stand-in sensors with these ids. With `ask`, nlclient is
    /// running already and is asked to start its BLE over.
    pub fn new(ours: &[u64], ask: bool) -> Relay {
        let mut relay = Relay {
            mode: Mode::Unknown { asked: None, times: 0 },
            ours: ours.iter().map(|id| *id as u16).collect(),
            from_host: Vec::new(),
            from_chip: Vec::new(),
            from_chip_at: 0,
            to_host: VecDeque::new(),
            to_chip: VecDeque::new(),
            role: None,
            active: false,
            filter: Vec::new(),
            stx: 0,
            heard: 0,
            ticked: 0,
            awake: 0,
            woke: 0,
            counts: Counts::default(),
        };
        if ask {
            relay.ask(0);
        }
        relay
    }

    /// Asks nlclient to start its BLE over; nothing passes until it does.
    fn ask(&mut self, now: u64) {
        let times = match self.mode {
            Mode::Unknown { times, .. } => times + 1,
            _ => 1,
        };
        self.forget();
        self.mode = Mode::Unknown { asked: Some(now), times };
        // Alone, and after whatever is on its way: whole messages.
        self.to_host.push_back(RESTART_BLE);
    }

    fn forget(&mut self) {
        self.from_host.clear();
        self.from_chip.clear();
        self.to_chip.clear();
        self.role = None;
        self.active = false;
        self.filter.clear();
        self.stx = 0;
    }

    /// The chip's UART was lost and is there again: whatever it was told is gone.
    pub fn chip_lost(&mut self, now: u64) {
        self.ask(now);
    }

    /// nlclient pulsed the reset line. The caller resets the chip, sets the
    /// line to Line::Rom and breaks it for BREAK_MS.
    pub fn reset(&mut self, now: u64) {
        self.forget();
        self.to_host.clear();
        self.mode = Mode::Held { until: now + BREAK_MS };
    }

    /// Whether the line is to be broken now.
    pub fn breaking(&self) -> bool {
        matches!(self.mode, Mode::Held { .. })
    }

    /// Whether the chip is between its reset and its firmware's start, when
    /// the caller is to come by often.
    pub fn booting(&self) -> bool {
        !matches!(self.mode, Mode::Unknown { .. } | Mode::Firmware) || !self.to_chip.is_empty()
    }

    /// Whether the firmware runs.
    #[cfg(test)]
    pub fn running(&self) -> bool {
        self.mode == Mode::Firmware
    }

    pub fn wants_host(&self) -> bool {
        !self.breaking() && self.chip_backlog() < CHIP_BACKLOG
    }

    /// How much to take from nlclient at once.
    pub fn host_room(&self) -> usize {
        CHIP_BACKLOG.saturating_sub(self.chip_backlog())
    }

    pub fn wants_chip(&self) -> bool {
        self.to_host.len() < HOST_BACKLOG
    }

    fn chip_backlog(&self) -> usize {
        self.to_chip.iter().map(|out| if let ToChip::Bytes(b) = out { b.len() } else { 0 }).sum()
    }

    pub fn host_queue(&mut self) -> &mut VecDeque<u8> {
        &mut self.to_host
    }

    pub fn chip_queue(&mut self) -> &mut VecDeque<ToChip> {
        &mut self.to_chip
    }

    fn send_chip(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(ToChip::Bytes(last)) = self.to_chip.back_mut() {
            last.extend(bytes);
        } else {
            self.to_chip.push_back(ToChip::Bytes(bytes.iter().copied().collect()));
        }
    }

    /// Time passes, the time asleep with it.
    pub fn tick(&mut self, now: u64) {
        let passed = now.saturating_sub(self.ticked);
        self.ticked = now;
        if passed < ASLEEP_MS {
            self.awake += passed;
        } else {
            self.woke = now;
        }
        match self.mode {
            Mode::Held { until } if now >= until => {
                self.mode = Mode::Rom;
                let kept = std::mem::take(&mut self.from_chip);
                self.chip(&kept, now);
            }
            Mode::Unknown { asked: Some(asked), times } if now.saturating_sub(asked) >= ASK_AGAIN_MS => {
                if times < ASK_TIMES {
                    self.ask(now);
                } else {
                    logf!("BLE relay: nlclient does not start its BLE over");
                    self.mode = Mode::Unknown { asked: None, times };
                }
            }
            Mode::Firmware => {
                if !self.from_chip.is_empty() && now.saturating_sub(self.from_chip_at) >= CUT_SHORT_MS {
                    self.counts.junk += self.from_chip.len() as u64;
                    self.from_chip.clear();
                }
                let paired = self.filter.iter().any(|id| !self.ours.contains(id));
                let deaf = now.saturating_sub(self.heard) >= DEAF_MS
                    && self.awake >= DEAF_AWAKE_MS
                    && now.saturating_sub(self.woke) >= ASLEEP_MS;
                if self.scanning() && paired && deaf {
                    logf!("BLE relay: the chip has heard nothing for {} minutes", DEAF_MS / 60_000);
                    self.ask(now);
                }
            }
            _ => {}
        }
    }

    fn scanning(&self) -> bool {
        self.mode == Mode::Firmware && self.active && self.role == Some(ROLE_CENTRAL)
    }

    /// Whether nlclient scans for this sensor: a beacon now reaches it.
    pub fn listening(&self, id: u64) -> bool {
        self.scanning() && self.filter.contains(&(id as u16))
    }

    /// A stand-in sensor's beacon, if it can go out now.
    pub fn inject(&mut self, report: &[u8]) -> bool {
        if !self.scanning() || !self.to_host.is_empty() {
            return false;
        }
        self.to_host.extend(message(&TO_HOST, op::ADVERTISEMENT, report));
        self.counts.injected += 1;
        true
    }

    /// Whether a beacon could go out now.
    pub fn can_inject(&self) -> bool {
        self.scanning() && self.to_host.is_empty()
    }

    /// Bytes from nlclient.
    pub fn host(&mut self, bytes: &[u8]) {
        if matches!(self.mode, Mode::Unknown { .. } | Mode::Held { .. }) {
            return;
        }
        self.from_host.extend_from_slice(bytes);
        loop {
            match self.mode {
                Mode::Rom => {
                    match self.from_host.first() {
                        None => return,
                        Some(&SOH) if self.from_host.len() < 3 => return,
                        Some(&SOH) => {}
                        Some(_) => {
                            self.from_host.remove(0);
                            continue;
                        }
                    }
                    let header: Vec<u8> = self.from_host.drain(..3).collect();
                    self.send_chip(&header);
                    let need = u16::from_le_bytes([header[1], header[2]]);
                    if need != VERSION_REQUEST {
                        self.mode = Mode::Header { need: usize::from(need) };
                    }
                }
                // Nothing is due from nlclient until the chip answers.
                Mode::Header { .. } | Mode::Checksum { .. } => return,
                Mode::Image { need, got, xor } => {
                    let take = self.from_host.len().min(need - got);
                    let piece: Vec<u8> = self.from_host.drain(..take).collect();
                    let xor = piece.iter().fold(xor, |xor, byte| xor ^ byte);
                    self.send_chip(&piece);
                    if got + take < need {
                        self.mode = Mode::Image { need, got: got + take, xor };
                        return;
                    }
                    logf!("BLE relay: nlclient loaded {need} bytes of BLE firmware");
                    self.mode = Mode::Checksum { xor };
                }
                Mode::Firmware => match next(&mut self.from_host, &TO_CHIP, MAX_PAYLOAD) {
                    Found::Message(message) => {
                        let (op, body) = parts(&message);
                        self.commanded(op, body);
                        self.counts.to_chip += 1;
                        self.send_chip(&message);
                    }
                    Found::Junk(_) => {} // the loader's ACK
                    Found::Partial => return,
                },
                Mode::Unknown { .. } | Mode::Held { .. } => return,
            }
        }
    }

    fn commanded(&mut self, op: u16, body: &[u8]) {
        match op {
            op::CONFIGURE_CENTRAL | op::CONFIGURE_PERIPHERAL => self.role = None,
            op::DEACTIVATE => self.active = false,
            // A count and the sensors' ids, answered by nothing.
            op::SENSOR_FILTER => {
                self.filter = body
                    .get(1..)
                    .unwrap_or_default()
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|id| u16::from_le_bytes(*id))
                    .collect();
            }
            _ => {}
        }
    }

    /// Bytes from the chip.
    pub fn chip(&mut self, bytes: &[u8], now: u64) {
        match self.mode {
            Mode::Unknown { .. } => {}
            Mode::Held { .. } => self.from_chip.extend_from_slice(bytes),
            Mode::Firmware => self.firmware(bytes, now),
            _ => {
                for (i, &byte) in bytes.iter().enumerate() {
                    match self.mode {
                        Mode::Header { need } if byte == ACK => self.mode = Mode::Image { need, got: 0, xor: 0 },
                        Mode::Header { .. } if byte == NAK => self.mode = Mode::Rom,
                        Mode::Checksum { xor } if byte == xor => {
                            self.send_chip(&[ACK]);
                            self.to_chip.push_back(ToChip::Line(Line::Firmware));
                            self.mode = Mode::Firmware;
                            self.hears(now);
                            self.to_host.push_back(byte);
                            let rest = self.from_host.split_off(0);
                            self.host(&rest);
                            return self.firmware(&bytes[i + 1..], now);
                        }
                        Mode::Checksum { .. } => self.mode = Mode::Rom,
                        // A chip that missed its reset goes on as before. The loader
                        // is to see nothing, and reset it again: it would wait for as
                        // long as anything comes.
                        Mode::Rom | Mode::Header { .. } if byte != STX => {
                            self.counts.junk += 1;
                            continue;
                        }
                        _ => {}
                    }
                    self.to_host.push_back(byte);
                }
                // What nlclient sent ahead of the chip's answer.
                let rest = self.from_host.split_off(0);
                self.host(&rest);
            }
        }
    }

    fn firmware(&mut self, bytes: &[u8], now: u64) {
        if bytes.is_empty() {
            return;
        }
        self.from_chip.extend_from_slice(bytes);
        self.from_chip_at = now;
        loop {
            match next(&mut self.from_chip, &TO_HOST, MAX_TO_HOST) {
                Found::Message(message) => {
                    let (op, body) = parts(&message);
                    self.answered(op, body);
                    self.stx = 0;
                    self.hears(now);
                    self.counts.to_host += 1;
                    self.to_host.extend(message);
                }
                Found::Junk(byte) => {
                    self.counts.junk += 1;
                    self.stx = if byte == STX { self.stx + 1 } else { 0 };
                    if self.stx >= ROM_STX {
                        logf!("BLE relay: the chip is back in its boot ROM");
                        return self.ask(now);
                    }
                }
                Found::Partial => return,
            }
        }
    }

    fn hears(&mut self, now: u64) {
        self.heard = now;
        self.awake = 0;
    }

    fn answered(&mut self, op: u16, body: &[u8]) {
        match (op, body) {
            (op::CONFIGURED, [role @ (ROLE_CENTRAL | ROLE_PERIPHERAL), SUCCESS, ..]) => self.role = Some(*role),
            (op::ACTIVATED, [status, ..]) => self.active = *status == SUCCESS,
            (op::DEACTIVATED, _) => self.active = false,
            _ => {}
        }
    }
}

#[cfg(target_os = "linux")]
pub mod line {
    //! The relay on its files: nlclient's PTY, the chip's UART and the reset lines.

    use super::{Line, Relay, ToChip, SETTLE_MS};
    use crate::logf;
    use crate::sensor::{boot_time, Sensor};
    use std::fs;
    use std::io;
    use std::os::fd::{AsRawFd, OwnedFd, RawFd};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// What the UART may hold unsent: all of it counts against the loader's 5 s.
    const UART_BACKLOG: libc::c_int = 512;
    /// How often a UART that went away is tried again.
    const REOPEN: Duration = Duration::from_secs(5);

    /// What the relay runs on.
    pub struct Ends {
        /// nlclient's side: the PTY's master, not blocking.
        pub master: OwnedFd,
        /// The chip's UART.
        pub uart: PathBuf,
        /// The directory of the real reset line.
        pub reset: PathBuf,
        /// Watches the decoy's value file, at `value`.
        pub inotify: OwnedFd,
        pub value: PathBuf,
        /// Readable when a sensor has a reading.
        pub wake: OwnedFd,
        pub sensors: Vec<Arc<Sensor>>,
        pub stop: Arc<AtomicBool>,
        /// How much the UART has not sent yet.
        pub unsent: Box<dyn Fn(RawFd) -> libc::c_int + Send>,
    }

    fn check(result: libc::c_int) -> io::Result<libc::c_int> {
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result)
        }
    }

    pub fn open_uart(path: &Path) -> io::Result<OwnedFd> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        set_line(file.as_raw_fd(), Line::Rom, libc::TCSANOW)?;
        Ok(file.into())
    }

    fn set_line(fd: RawFd, line: Line, when: libc::c_int) -> io::Result<()> {
        // SAFETY: termios is plain data for tcgetattr to fill.
        let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
        // SAFETY: fd is open, and termios is valid for each call.
        unsafe {
            check(libc::tcgetattr(fd, &mut termios))?;
            let speed = if line == Line::Rom { libc::B57600 } else { libc::B115200 };
            libc::cfmakeraw(&mut termios);
            libc::cfsetispeed(&mut termios, speed);
            libc::cfsetospeed(&mut termios, speed);
        }
        termios.c_cflag |= libc::CLOCAL | libc::CREAD;
        if line == Line::Rom {
            termios.c_cflag &= !libc::CRTSCTS;
        } else {
            termios.c_cflag |= libc::CRTSCTS;
        }
        termios.c_cc[libc::VMIN] = 1;
        termios.c_cc[libc::VTIME] = 0;
        // SAFETY: as above.
        check(unsafe { libc::tcsetattr(fd, when, &termios) })?;
        Ok(())
    }

    pub fn unsent(fd: RawFd) -> libc::c_int {
        let mut count = 0;
        // SAFETY: count is writable.
        if unsafe { libc::ioctl(fd, libc::TIOCOUTQ, &mut count) } < 0 {
            return 0;
        }
        count
    }

    /// Some(0) is nothing now; None, the file is gone.
    fn read(fd: RawFd, buffer: &mut [u8]) -> Option<usize> {
        // SAFETY: buffer is writable for its length.
        let got = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if got > 0 {
            return Some(got as usize);
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            _ if got == 0 => None,
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => Some(0),
            _ => None,
        }
    }

    /// As read. What it wrote is taken from `bytes`.
    fn write(fd: RawFd, bytes: &mut std::collections::VecDeque<u8>, most: usize) -> Option<usize> {
        let slice = bytes.make_contiguous();
        let len = slice.len().min(most);
        if len == 0 {
            return Some(0);
        }
        // SAFETY: slice is readable for len.
        let wrote = unsafe { libc::write(fd, slice.as_ptr().cast(), len) };
        if wrote >= 0 {
            bytes.drain(..wrote as usize);
            return Some(wrote as usize);
        }
        match io::Error::last_os_error().kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => Some(0),
            _ => None,
        }
    }

    fn drain(fd: RawFd) {
        let mut buffer = [0u8; 4096];
        // SAFETY: buffer is writable for its length.
        while unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) } > 0 {}
    }

    /// Resets the chip as nlclient's loader does, and starts the break.
    fn reset_chip(uart: RawFd, reset: &Path) -> io::Result<()> {
        let value = reset.join("value");
        fs::write(&value, "1\n")?;
        std::thread::sleep(Duration::from_millis(1));
        // SAFETY: uart is open.
        unsafe { libc::tcflush(uart, libc::TCIOFLUSH) };
        set_line(uart, Line::Rom, libc::TCSANOW)?;
        fs::write(&value, "0\n")?;
        // SAFETY: as above; what cannot break the line goes without.
        unsafe { libc::ioctl(uart, libc::TIOCSBRK) };
        Ok(())
    }

    /// Runs the relay until told to stop, or the PTY goes.
    pub fn run(ends: Ends, mut relay: Relay) {
        let start = boot_time();
        let now = || boot_time().saturating_sub(start).as_millis() as u64;
        let mut uart = None;
        let mut tried: Option<Instant> = None;
        let mut first = true;
        let mut broken = false;
        // Until when the line rests after a change.
        let mut resting: Option<Instant> = None;
        let mut buffer = [0u8; 1024];
        let mut was: Vec<bool> = vec![false; ends.sensors.len()];
        while !ends.stop.load(Ordering::Relaxed) {
            if uart.is_none() && tried.is_none_or(|at| at.elapsed() >= REOPEN) {
                tried = Some(Instant::now());
                match open_uart(&ends.uart) {
                    Ok(fd) => {
                        uart = Some(fd);
                        if !first {
                            logf!("BLE relay: {} is back", ends.uart.display());
                            relay.chip_lost(now());
                        }
                    }
                    Err(e) => logf!("BLE relay: {}: {e}", ends.uart.display()),
                }
                first = false;
            }
            let chip = uart.as_ref().map(|fd| fd.as_raw_fd());
            if resting.is_some_and(|until| Instant::now() >= until) {
                resting = None;
            }
            let room = chip.map_or(0, |fd| (UART_BACKLOG - (ends.unsent)(fd)).max(0) as usize);
            let chip_has = matches!(relay.chip_queue().front(), Some(ToChip::Bytes(_)));
            let mut fds = [
                libc::pollfd { fd: ends.master.as_raw_fd(), events: 0, revents: 0 },
                libc::pollfd { fd: ends.inotify.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: ends.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: chip.unwrap_or(-1), events: 0, revents: 0 },
            ];
            if relay.wants_host() {
                fds[0].events |= libc::POLLIN;
            }
            if !relay.host_queue().is_empty() {
                fds[0].events |= libc::POLLOUT;
            }
            if relay.wants_chip() {
                fds[3].events |= libc::POLLIN;
            }
            if chip_has && resting.is_none() && room > 0 {
                fds[3].events |= libc::POLLOUT;
            }
            let timeout = if relay.booting() || resting.is_some() { 5 } else { 1000 };
            // SAFETY: fds is valid for its length.
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };

            if fds[1].revents & libc::POLLIN != 0 {
                drain(ends.inotify.as_raw_fd());
                if fs::read(&ends.value).is_ok_and(|v| v.first() == Some(&b'0')) {
                    // What nlclient said before the reset is not for what comes after.
                    drain(ends.master.as_raw_fd());
                    fds[0].revents &= !libc::POLLIN;
                    fds[3].revents = 0;
                    resting = None;
                    if let Some(fd) = chip {
                        match reset_chip(fd, &ends.reset) {
                            Ok(()) => broken = true,
                            Err(e) => logf!("BLE relay: the chip's reset: {e}"),
                        }
                    }
                    relay.reset(now());
                }
            }
            if fds[2].revents & libc::POLLIN != 0 {
                drain(ends.wake.as_raw_fd());
            }
            if let Some(fd) = chip {
                let mut gone = false;
                if fds[3].revents & libc::POLLIN != 0 {
                    match read(fd, &mut buffer) {
                        Some(got) => relay.chip(&buffer[..got], now()),
                        None => gone = true,
                    }
                } else if fds[3].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                    gone = true;
                }
                if gone {
                    logf!("BLE relay: {} went away", ends.uart.display());
                    uart = None;
                    tried = Some(Instant::now());
                    relay.chip_queue().clear();
                    broken = false;
                }
            }
            if fds[0].revents & libc::POLLIN != 0 {
                let most = relay.host_room().min(buffer.len());
                match read(ends.master.as_raw_fd(), &mut buffer[..most.max(1)]) {
                    Some(got) => relay.host(&buffer[..got]),
                    None => std::thread::sleep(Duration::from_millis(50)),
                }
            } else if fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                std::thread::sleep(Duration::from_millis(50));
            }
            relay.tick(now());

            if let Some(fd) = uart.as_ref().map(|fd| fd.as_raw_fd()) {
                if broken && !relay.breaking() {
                    // SAFETY: fd is open.
                    unsafe { libc::ioctl(fd, libc::TIOCCBRK) };
                    broken = false;
                }
                // Out to the chip, as far as the line and its changes go.
                while resting.is_none() && !broken {
                    let room = (UART_BACKLOG - (ends.unsent)(fd)).max(0) as usize;
                    match relay.chip_queue().front_mut() {
                        Some(ToChip::Bytes(bytes)) if bytes.is_empty() => {
                            relay.chip_queue().pop_front();
                        }
                        Some(ToChip::Bytes(bytes)) => match write(fd, bytes, room) {
                            Some(0) | None => break,
                            Some(_) => {}
                        },
                        Some(ToChip::Line(line)) => {
                            let line = *line;
                            relay.chip_queue().pop_front();
                            // What is before it is out when this returns.
                            if let Err(e) = set_line(fd, line, libc::TCSADRAIN) {
                                logf!("BLE relay: the line: {e}");
                            }
                            resting = Some(Instant::now() + Duration::from_millis(SETTLE_MS));
                        }
                        None => break,
                    }
                }
            }
            let _ = write(ends.master.as_raw_fd(), relay.host_queue(), usize::MAX);

            for (sensor, was) in ends.sensors.iter().zip(&mut was) {
                let listening = relay.listening(sensor.id());
                if listening != *was {
                    logf!(
                        "external temperature: nlclient {} for {}",
                        if listening { "is listening" } else { "stopped listening" },
                        sensor.bucket
                    );
                    *was = listening;
                }
                if !listening {
                    sensor.quiet_tick();
                } else if relay.can_inject() {
                    if let Some(report) = sensor.due() {
                        relay.inject(&report);
                        let _ = write(ends.master.as_raw_fd(), relay.host_queue(), usize::MAX);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: u64 = 0x18B4_3000_00A1_1111;
    const OURS: u64 = 0x18B4_3000_00C3_D82D;

    fn chip_out(relay: &mut Relay) -> Vec<ToChip> {
        relay.chip_queue().drain(..).collect()
    }

    fn host_out(relay: &mut Relay) -> Vec<u8> {
        relay.host_queue().drain(..).collect()
    }

    fn bytes(bytes: &[u8]) -> ToChip {
        ToChip::Bytes(bytes.iter().copied().collect())
    }

    /// As far as a running firmware.
    fn running() -> Relay {
        let mut relay = Relay::new(&[OURS], false);
        relay.reset(1000);
        relay.tick(1000 + BREAK_MS);
        relay.chip(&[STX], 1300);
        relay.host(&[SOH, 2, 0]);
        relay.chip(&[ACK], 1301);
        relay.host(&[0x0f, 0xf0]);
        relay.chip(&[0xff], 1302);
        assert!(relay.running());
        chip_out(&mut relay);
        host_out(&mut relay);
        relay
    }

    /// As far as scanning for both.
    fn scanning() -> Relay {
        let mut relay = running();
        relay.host(&message(&TO_CHIP, op::CONFIGURE_CENTRAL, &[0; 21]));
        relay.chip(&message(&TO_HOST, op::CONFIGURED, &[0, 1]), 1400);
        relay.host(&message(&TO_CHIP, op::ACTIVATE, &[]));
        relay.chip(&message(&TO_HOST, op::ACTIVATED, &[1]), 1401);
        let filter = [&[2][..], &(REAL as u16).to_le_bytes(), &(OURS as u16).to_le_bytes()].concat();
        relay.host(&message(&TO_CHIP, op::SENSOR_FILTER, &filter));
        chip_out(&mut relay);
        host_out(&mut relay);
        relay
    }

    #[test]
    fn nothing_passes_before_a_reset() {
        let mut relay = Relay::new(&[OURS], false);
        relay.host(&message(&TO_CHIP, op::ACTIVATE, &[]));
        relay.chip(&message(&TO_HOST, op::ACTIVATED, &[1]), 1);
        assert!(chip_out(&mut relay).is_empty() && host_out(&mut relay).is_empty());
        assert!(!relay.inject(&[0; 41]));
    }

    #[test]
    fn an_upload_goes_through_and_the_ack_comes_from_here() {
        let mut relay = Relay::new(&[OURS], false);
        relay.reset(0);
        assert!(relay.breaking() && !relay.wants_host());
        relay.chip(&[STX], 10);
        assert!(host_out(&mut relay).is_empty(), "not while the line is broken");
        relay.tick(BREAK_MS);
        assert_eq!(host_out(&mut relay), [STX], "but after");
        relay.host(&[SOH, 2]);
        assert!(chip_out(&mut relay).is_empty());
        relay.host(&[0]);
        assert_eq!(chip_out(&mut relay), [bytes(&[SOH, 2, 0])]);
        // One STX more was on its way.
        relay.chip(&[STX, ACK], 300);
        assert_eq!(host_out(&mut relay), [STX, ACK]);
        relay.host(&[0x0f]);
        relay.host(&[0xf0]);
        assert_eq!(chip_out(&mut relay), [bytes(&[0x0f, 0xf0])]);
        relay.chip(&[0xff], 400);
        assert_eq!(host_out(&mut relay), [0xff]);
        assert_eq!(chip_out(&mut relay), [bytes(&[ACK]), ToChip::Line(Line::Firmware)]);
        // The loader's ACK is dropped; the first message follows the line's change.
        let command = message(&TO_CHIP, op::XTRIM_SET, &[0x86, 4]);
        relay.host(&[&[ACK][..], &command].concat());
        assert_eq!(chip_out(&mut relay), [bytes(&command)]);
    }

    #[test]
    fn an_image_that_looks_like_messages_and_headers_goes_through_whole() {
        let image = [&TO_CHIP[..], &[SOH, 0xff, 0xff, STX, ACK, NAK], &TO_HOST].concat();
        for split in 0..=image.len() {
            let mut relay = Relay::new(&[], false);
            relay.reset(0);
            relay.tick(BREAK_MS);
            relay.chip(&[STX], 300);
            // nlclient does not wait for the ACK before all of it here.
            relay.host(&[SOH, image.len() as u8, 0]);
            relay.host(&image[..split]);
            relay.chip(&[ACK], 301);
            relay.host(&image[split..]);
            let sent: Vec<u8> = chip_out(&mut relay)
                .into_iter()
                .flat_map(|out| if let ToChip::Bytes(b) = out { Vec::from(b) } else { panic!() })
                .collect();
            assert_eq!(sent, [&[SOH, image.len() as u8, 0][..], &image].concat(), "split at {split}");
        }
    }

    #[test]
    fn a_wrong_checksum_starts_no_firmware() {
        let mut relay = Relay::new(&[], false);
        relay.reset(0);
        relay.tick(BREAK_MS);
        relay.host(&[SOH, 1, 0]);
        relay.chip(&[ACK], 300);
        relay.host(&[0x77]);
        chip_out(&mut relay);
        relay.chip(&[0x78], 301);
        assert!(chip_out(&mut relay).is_empty() && !relay.running());
        assert_eq!(host_out(&mut relay), [ACK, 0x78], "nlclient sees it");
    }

    #[test]
    fn a_chip_that_missed_its_reset_is_not_heard_until_it_is_reset_again() {
        let mut relay = scanning();
        relay.reset(2000);
        let advertisement = message(&TO_HOST, op::ADVERTISEMENT, &[7; 41]);
        relay.chip(&advertisement, 2100);
        relay.tick(2000 + BREAK_MS);
        relay.chip(&advertisement, 2300);
        assert!(host_out(&mut relay).is_empty());
        relay.reset(7300);
        relay.tick(7300 + BREAK_MS);
        relay.chip(&[STX], 7600);
        assert_eq!(host_out(&mut relay), [STX]);
    }

    #[test]
    fn a_bootloader_version_request_loads_nothing() {
        let mut relay = Relay::new(&[], false);
        relay.reset(0);
        relay.tick(BREAK_MS);
        relay.host(&[SOH, 0x7f, 0xff]);
        relay.chip(&[NAK, STX], 300);
        relay.host(&[SOH, 1, 0]);
        relay.chip(&[ACK], 301);
        relay.host(&[0x77]);
        relay.chip(&[0x77], 302);
        assert!(relay.running());
    }

    #[test]
    fn messages_go_both_ways_whole_however_they_come() {
        let from_chip =
            [message(&TO_HOST, op::ACTIVATED, &[1]), message(&TO_HOST, op::ADVERTISEMENT, &[5; 41])].concat();
        let from_host =
            [message(&TO_CHIP, op::ACTIVATE, &[]), message(&TO_CHIP, op::SENSOR_FILTER, &[1, 5, 1])].concat();
        for split in 0..=from_chip.len() {
            let mut relay = running();
            relay.chip(&from_chip[..split], 1500);
            let first = host_out(&mut relay);
            relay.chip(&from_chip[split..], 1501);
            assert_eq!([first, host_out(&mut relay)].concat(), from_chip, "split at {split}");
        }
        for split in 0..=from_host.len() {
            let mut relay = running();
            relay.host(&from_host[..split]);
            relay.host(&from_host[split..]);
            assert_eq!(chip_out(&mut relay), [bytes(&from_host)], "split at {split}");
        }
    }

    #[test]
    fn a_beacon_goes_in_only_between_messages_and_only_while_it_is_listened_for() {
        let mut relay = scanning();
        assert!(relay.listening(OURS) && relay.listening(REAL) && !relay.listening(0x1234));
        // Half a message from the chip is not half a message to nlclient.
        let advertisement = message(&TO_HOST, op::ADVERTISEMENT, &[7; 41]);
        relay.chip(&advertisement[..15], 1500);
        assert!(relay.inject(&[9; 41]));
        relay.chip(&advertisement[15..], 1501);
        assert_eq!(host_out(&mut relay), [message(&TO_HOST, op::ADVERTISEMENT, &[9; 41]), advertisement].concat());
        // Not while nlclient is behind.
        relay.chip(&message(&TO_HOST, op::ADVERTISEMENT, &[7; 41]), 1502);
        assert!(!relay.can_inject() && !relay.inject(&[9; 41]));
        host_out(&mut relay);
        relay.host(&message(&TO_CHIP, op::DEACTIVATE, &[]));
        assert!(!relay.listening(OURS) && !relay.inject(&[9; 41]), "from the moment it is told to stop");
    }

    #[test]
    fn a_role_or_activation_the_chip_refuses_is_not_listening() {
        let mut relay = running();
        relay.host(&message(&TO_CHIP, op::CONFIGURE_CENTRAL, &[0; 21]));
        relay.chip(&message(&TO_HOST, op::CONFIGURED, &[0, 0]), 1400);
        relay.chip(&message(&TO_HOST, op::ACTIVATED, &[1]), 1401);
        relay.host(&message(&TO_CHIP, op::SENSOR_FILTER, &[1, OURS as u8, (OURS >> 8) as u8]));
        assert!(!relay.listening(OURS));
        relay.chip(&message(&TO_HOST, op::CONFIGURED, &[0, 1]), 1402);
        assert!(relay.listening(OURS));
        relay.chip(&message(&TO_HOST, op::ACTIVATED, &[0]), 1403);
        assert!(!relay.listening(OURS));
    }

    #[test]
    fn a_message_longer_than_nlclient_has_room_for_stays_here() {
        let mut relay = running();
        let good = message(&TO_HOST, op::ACTIVATED, &[1]);
        relay.chip(&[message(&TO_HOST, op::ADVERTISEMENT, &[0; 149]), good.clone()].concat(), 1500);
        assert_eq!(host_out(&mut relay), good);
        let longest = message(&TO_HOST, 0x22, &[0; 148]);
        relay.chip(&longest, 1501);
        assert_eq!(host_out(&mut relay), longest);
    }

    #[test]
    fn a_message_within_an_advertisement_is_not_a_message() {
        let mut relay = scanning();
        let inner = message(&TO_HOST, op::DEACTIVATED, &[1]);
        let outer = message(&TO_HOST, op::ADVERTISEMENT, &[&[3, 0][..], &inner, &[STX; 25]].concat());
        for split in 0..=outer.len() {
            relay.chip(&outer[..split], 1500);
            relay.chip(&outer[split..], 1501);
            assert_eq!(host_out(&mut relay), outer, "split at {split}");
            assert!(relay.listening(OURS));
        }
    }

    #[test]
    fn a_message_cut_short_does_not_take_the_next_one_along() {
        let mut relay = running();
        relay.chip(&message(&TO_HOST, op::ADVERTISEMENT, &[7; 41])[..30], 1500);
        relay.tick(1500 + CUT_SHORT_MS - 1);
        relay.tick(1500 + CUT_SHORT_MS);
        let good = message(&TO_HOST, op::ACTIVATED, &[1]);
        relay.chip(&good, 2100);
        assert_eq!(host_out(&mut relay), good);
    }

    #[test]
    fn a_chip_in_its_rom_again_has_nlclient_start_over_once() {
        let mut relay = scanning();
        relay.chip(&[STX], 2000);
        relay.chip(&message(&TO_HOST, op::ADVERTISEMENT, &[7; 41]), 2001);
        relay.chip(&[STX, STX], 2002);
        assert!(relay.running(), "a stray one or two is noise");
        host_out(&mut relay);
        relay.chip(&[STX, STX, STX, STX, STX], 2003);
        assert_eq!(host_out(&mut relay), [RESTART_BLE]);
        assert!(!relay.listening(OURS) && !relay.inject(&[9; 41]));
        relay.chip(&[STX; 100], 2004);
        assert!(host_out(&mut relay).is_empty());
        // It does not answer: asked again, and then not any more.
        for (i, at) in [(1, 17_010), (2, 32_020)] {
            relay.tick(at);
            assert_eq!(host_out(&mut relay), [RESTART_BLE], "time {i}");
        }
        relay.tick(47_030);
        relay.tick(90_000);
        assert!(host_out(&mut relay).is_empty());
        // Its reset is taken whenever it comes.
        relay.reset(100_000);
        assert!(relay.breaking());
    }

    #[test]
    fn a_deaf_chip_is_noticed_though_beacons_go_in() {
        let mut relay = scanning();
        let heard = 1401;
        for second in 1..DEAF_MS / 1000 {
            relay.tick(heard + second * 1000);
            if second % 60 == 0 {
                assert!(relay.inject(&[9; 41]), "second {second}");
                host_out(&mut relay);
            }
        }
        relay.tick(heard + DEAF_MS - 1);
        assert!(relay.running());
        relay.tick(heard + DEAF_MS);
        assert_eq!(host_out(&mut relay), [RESTART_BLE]);
    }

    /// A unit that wakes has heard nothing while it slept, and is not deaf.
    #[test]
    fn a_unit_that_slept_is_given_time_to_hear() {
        let mut relay = scanning();
        let woke = 1401 + 4 * DEAF_MS;
        relay.tick(1401 + 1000);
        for second in 0..59 {
            relay.tick(woke + second * 1000);
        }
        assert!(relay.running(), "just awake");
        relay.chip(&message(&TO_HOST, op::ADVERTISEMENT, &[7; 41]), woke + 59_500);
        for second in 60..600 {
            relay.tick(woke + second * 1000);
        }
        assert!(relay.running(), "heard since");
        host_out(&mut relay);
        // Asleep again for long: the chip it wakes for is still to be read.
        let again = woke + 2 * DEAF_MS;
        relay.tick(again);
        relay.tick(again + ASLEEP_MS - 1);
        assert!(relay.running());
        relay.tick(again + ASLEEP_MS);
        assert_eq!(host_out(&mut relay), [RESTART_BLE], "awake for minutes since it was heard, and nothing");
    }

    #[test]
    fn a_chip_with_no_paired_sensor_to_hear_may_be_silent() {
        let mut relay = scanning();
        relay.host(&message(&TO_CHIP, op::SENSOR_FILTER, &[1, OURS as u8, (OURS >> 8) as u8]));
        relay.tick(10 * DEAF_MS);
        assert!(relay.running());
    }

    #[test]
    fn little_is_taken_ahead_of_the_chip() {
        let mut relay = Relay::new(&[], false);
        relay.reset(0);
        relay.tick(BREAK_MS);
        relay.host(&[SOH, 0x20, 0x7b]);
        relay.chip(&[ACK], 300);
        let mut took = 0;
        while relay.wants_host() {
            let room = relay.host_room();
            relay.host(&vec![1; room]);
            took += room;
        }
        assert!(took <= CHIP_BACKLOG, "{took}");
    }

    #[test]
    fn nlclient_behind_holds_the_chip_up() {
        let mut relay = scanning();
        let mut sent = 0;
        while relay.wants_chip() {
            relay.chip(&message(&TO_HOST, op::ADVERTISEMENT, &[7; 41]), 2000);
            sent += 1;
        }
        assert!(sent > 50 && relay.host_queue().len() < HOST_BACKLOG + 100);
    }

    /// Whatever the chip says, nlclient gets whole messages, and at most the
    /// byte that has it start over, last.
    #[test]
    fn nothing_but_whole_messages_whatever_the_chip_says() {
        let mut random = 0x9e37_79b9_7f4a_7c15u64;
        let mut next_random = move |n: u64| {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            random % n
        };
        let mut restarts = 0;
        for _ in 0..20_000 {
            let mut relay = scanning();
            let mut now = 2000;
            let mut out = Vec::new();
            for _ in 0..next_random(40) {
                now += next_random(200);
                match next_random(5) {
                    0 => {
                        let body = vec![next_random(256) as u8; next_random(60) as usize];
                        let whole = message(&TO_HOST, next_random(70) as u16, &body);
                        let from = next_random(whole.len() as u64) as usize;
                        relay.chip(&whole[if next_random(2) == 0 { 0 } else { from }..], now);
                    }
                    1 => {
                        relay.inject(&[next_random(256) as u8; 41]);
                    }
                    2 => {
                        let junk: Vec<u8> = (0..next_random(20))
                            .map(|_| [5, 1, 0xc8, 0x3f, 0, 0x32, 0, 2, 0xff][next_random(9) as usize])
                            .collect();
                        relay.chip(&junk, now);
                    }
                    3 => relay.tick(now),
                    _ => out.extend(host_out(&mut relay)),
                }
            }
            out.extend(host_out(&mut relay));
            while !out.is_empty() {
                match next(&mut out, &TO_HOST, MAX_TO_HOST) {
                    Found::Message(_) => {}
                    Found::Junk(byte) => {
                        assert!(byte == RESTART_BLE && out.is_empty() && !relay.running(), "{byte:#x} then {out:02x?}");
                        restarts += 1;
                    }
                    Found::Partial => panic!("cut short: {out:02x?}"),
                }
            }
        }
        assert!(restarts > 0, "the path was taken");
    }
}
