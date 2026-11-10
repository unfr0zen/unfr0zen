//! The stand-in on the unit: a PTY where nlclient looks for the BLE chip's
//! UART, a decoy of the chip's reset line, and the thread behind them.
//!
//! With Matter alone there is no sensor, but nlclient still needs the
//! stand-in: run with ble_disabled, it resets the chip and loads Nest's sleep
//! firmware into it at each start, which on the real chip breaks an upload
//! nestlocal has under way and replaces one it has finished.
//!
//! nlclient's firmware loader opens /dev/ttyble0 by name and pulses
//! <reset>/value with no delay, so both are replaced before nlclient starts:
//! the symlink points at the PTY, and a directory bind-mounted over the GPIO's
//! tells the thread of each reset through inotify. The real chip stays
//! nestlocal's, on its own UART and the GPIO as it was before the mount.
//!
//! nlclient keeps the PTY open as long as it runs and never reopens a dead
//! one, so the PTY outlives nestlocal: a small process forked at the first
//! start holds it and hands its master to each nestlocal over a socket. A
//! later nestlocal knows nothing of what nlclient told the chip, and sends the
//! byte that makes nlclient start its BLE over.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use super::chip::Chip;
use super::Sensor;
use crate::device::Ble;
use crate::logf;
use crate::settings_ui::DIR;

const SLEEP_FIRMWARE: &str = "/usr/share/dialog/data/firmware/nl_ble_sleep_firmware.bin";
const DECOY: &str = "ble-reset";
/// The PTY's holder, as an abstract socket's name.
const HOLDER: &[u8] = b"nestlocal-ble-pty";
/// A byte nlclient's BLE reader does not expect between messages: it restarts
/// its BLE, reset line and firmware upload and all.
const RESTART_BLE: u8 = 0x02;

/// The real reset GPIO's directory, once the decoy covers its name.
static REAL_RESET: OnceLock<String> = OnceLock::new();

/// The directory that resets the real chip.
pub fn reset_dir(ble: &Ble) -> String {
    REAL_RESET.get().cloned().unwrap_or_else(|| ble.reset.to_string())
}

/// Whether nlclient talks to the stand-in, leaving the real chip to nestlocal.
pub fn owns_chip() -> bool {
    REAL_RESET.get().is_some()
}

/// Whether nlclient's own BLE is off (ble_disabled), as unfr0zen sets it when
/// Matter takes the chip. With it on, the chip stays nlclient's.
pub fn stock_ble_off() -> io::Result<bool> {
    Ok(fs::read_to_string("/nestlabs/etc/product.config")?.contains("key=\"ble_disabled\" value=\"1\""))
}

fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 { Err(io::Error::last_os_error()) } else { Ok(result) }
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)
}

/// Covers the reset GPIO with the decoy, keeping a way to the real one, and
/// watches the decoy's value file.
fn cover_reset(ble: &Ble) -> io::Result<(OwnedFd, PathBuf)> {
    let real = fs::canonicalize(ble.reset)?;
    let real_c = c_path(&real)?;
    // A decoy from an earlier run: nothing else mounts over this GPIO.
    // SAFETY: real_c is a valid path; the result only says whether one was there.
    while unsafe { libc::umount2(real_c.as_ptr(), libc::MNT_DETACH) } == 0 {}
    // SAFETY: as above; the descriptor is owned from here on.
    let kept = check(unsafe { libc::open(real_c.as_ptr(), libc::O_PATH | libc::O_DIRECTORY) })?;
    let _ = REAL_RESET.set(format!("/proc/{}/fd/{kept}", std::process::id()));

    let decoy = Path::new(DIR).join(DECOY);
    fs::create_dir_all(&decoy)?;
    let value = decoy.join("value");
    fs::write(&value, "0\n")?;
    fs::write(decoy.join("direction"), "out\n")?;
    let decoy_c = c_path(&decoy)?;
    let none = std::ptr::null();
    // SAFETY: both paths are valid C strings; a bind mount takes no data.
    check(unsafe { libc::mount(decoy_c.as_ptr(), real_c.as_ptr(), none, libc::MS_BIND, std::ptr::null()) })?;

    // SAFETY: inotify_init1 takes only flags.
    let inotify = check(unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) })?;
    // SAFETY: inotify is ours, and owned from here on.
    let inotify = unsafe { OwnedFd::from_raw_fd(inotify) };
    let value_c = c_path(&value)?;
    // SAFETY: inotify and the path are valid.
    check(unsafe { libc::inotify_add_watch(inotify.as_raw_fd(), value_c.as_ptr(), libc::IN_MODIFY) })?;
    Ok((inotify, value))
}

/// A new PTY's master and its other end, raw, with `link` pointing at it.
fn open_pty(link: &Path) -> io::Result<(OwnedFd, OwnedFd)> {
    // SAFETY: posix_openpt takes only flags; the descriptor is owned at once.
    let master = unsafe { OwnedFd::from_raw_fd(check(libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC))?) };
    // SAFETY: master is a PTY master.
    check(unsafe { libc::grantpt(master.as_raw_fd()) })?;
    check(unsafe { libc::unlockpt(master.as_raw_fd()) })?;
    let mut name = [0 as libc::c_char; 64];
    // SAFETY: name is writable for its length.
    let error = unsafe { libc::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()) };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    // SAFETY: ptsname_r wrote a terminated string.
    let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_owned();
    // SAFETY: name is the slave's path; the descriptor is owned at once.
    let slave = unsafe { OwnedFd::from_raw_fd(check(libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC))?) };
    // SAFETY: termios is plain data for tcgetattr to fill.
    let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
    check(unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut termios) })?;
    unsafe { libc::cfmakeraw(&mut termios) };
    check(unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &termios) })?;

    let partial = link.with_extension("partial");
    let _ = fs::remove_file(&partial);
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(name.to_bytes()), &partial)?;
    fs::rename(&partial, link)?;
    Ok((master, slave))
}

fn holder_address() -> io::Result<SocketAddr> {
    SocketAddr::from_abstract_name(HOLDER)
}

/// Sends the descriptor, and one byte saying whether it has been handed out
/// before, on a connected stream.
fn send_master(stream: &UnixStream, master: RawFd, handed_out: bool) -> io::Result<()> {
    let mut byte = [u8::from(handed_out)];
    let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
    // SAFETY: CMSG_SPACE only computes a size.
    let mut space = vec![0u8; unsafe { libc::CMSG_SPACE(4) } as usize];
    // SAFETY: msghdr is plain data; the pointers set below outlive the call.
    let mut message = unsafe { std::mem::zeroed::<libc::msghdr>() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = space.as_mut_ptr().cast();
    message.msg_controllen = space.len() as _;
    // SAFETY: message's control buffer has room for one header and descriptor.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(4) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), master);
    }
    // SAFETY: message and what it points at are valid for the call.
    if unsafe { libc::sendmsg(stream.as_raw_fd(), &message, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The descriptor send_master sent, and its byte.
fn receive_master(stream: &UnixStream) -> io::Result<(OwnedFd, bool)> {
    let mut byte = [0u8];
    let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
    // SAFETY: CMSG_SPACE only computes a size.
    let mut space = vec![0u8; unsafe { libc::CMSG_SPACE(4) } as usize];
    // SAFETY: msghdr is plain data; the pointers set below outlive the call.
    let mut message = unsafe { std::mem::zeroed::<libc::msghdr>() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = space.as_mut_ptr().cast();
    message.msg_controllen = space.len() as _;
    // SAFETY: message and what it points at are valid for the call.
    if unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) } <= 0 {
        return Err(io::Error::other("no answer from the PTY's holder"));
    }
    // SAFETY: recvmsg filled message, and CMSG_FIRSTHDR checks its length.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        if header.is_null() || (*header).cmsg_type != libc::SCM_RIGHTS {
            return Err(io::Error::other("the PTY's holder sent no descriptor"));
        }
        let fd = std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<RawFd>());
        Ok((OwnedFd::from_raw_fd(fd), byte[0] != 0))
    }
}

/// The holder's life: the PTY, handed to whoever asks.
fn hold(link: &Path) -> io::Result<()> {
    // SAFETY: both only change this process's own attributes.
    unsafe {
        libc::setsid();
        libc::prctl(libc::PR_SET_NAME, c"ble-pty".as_ptr());
    }
    let (master, _slave) = open_pty(link)?;
    let listener = UnixListener::bind_addr(&holder_address()?)?;
    let mut handed_out = false;
    loop {
        let (stream, _) = listener.accept()?;
        if send_master(&stream, master.as_raw_fd(), handed_out).is_ok() {
            handed_out = true;
        }
    }
}

/// Makes sure the holder runs. Called before nestlocal has threads to lose in
/// a fork.
pub fn start_holder(ble: &Ble) -> io::Result<()> {
    if UnixStream::connect_addr(&holder_address()?).is_ok() {
        return Ok(());
    }
    // SAFETY: the process has one thread here, and the child stays in hold.
    match check(unsafe { libc::fork() })? {
        0 => {
            if let Err(e) = hold(Path::new(ble.uart)) {
                logf!("BLE stand-in: the PTY's holder: {e}");
            }
            // SAFETY: the holder has nothing to tidy.
            unsafe { libc::_exit(1) }
        }
        _ => {
            // An nlclient already running holds a PTY that died with the old
            // holder, and only its start finds the new one.
            for pid in crate::pids("nlclient") {
                logf!("BLE stand-in: restarting nlclient for its new PTY");
                // SAFETY: kill takes any pid and signal.
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
            Ok(())
        }
    }
}

/// The PTY's master from its holder, and whether another nestlocal had it.
fn fetch_master() -> io::Result<(OwnedFd, bool)> {
    let address = holder_address()?;
    let mut last = io::Error::other("the PTY's holder is not there");
    for _ in 0..100 {
        match UnixStream::connect_addr(&address).and_then(|stream| receive_master(&stream)) {
            Ok(answer) => return Ok(answer),
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(last)
}

/// Gives the real chip Nest's sleep firmware, as nlclient does when it has no
/// use for BLE: left in its boot ROM the chip would draw more.
fn sleep_real_chip(ble: &'static Ble) {
    let reset = reset_dir(ble);
    let status = std::process::Command::new("/nestlabs/sbin/da-loader")
        .args([ble.node, SLEEP_FIRMWARE, "57600", "no-rtscts", &reset])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    match status {
        Ok(status) if status.success() => logf!("external temperature: the BLE chip sleeps"),
        Ok(status) => logf!("external temperature: BLE chip's sleep firmware: {status}"),
        Err(e) => logf!("external temperature: BLE chip's sleep firmware: {e}"),
    }
}

fn write_all(fd: RawFd, mut bytes: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(1);
    while !bytes.is_empty() && Instant::now() < deadline {
        // SAFETY: bytes is readable for its length.
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written > 0 {
            bytes = &bytes[written as usize..];
        } else {
            let mut poll = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
            // SAFETY: poll is one valid entry.
            unsafe { libc::poll(&mut poll, 1, 50) };
        }
    }
}

fn run(sensor: Option<Arc<Sensor>>, master: OwnedFd, inotify: OwnedFd, value: PathBuf, wake: OwnedFd) {
    let mut chip = Chip::new();
    let mut buffer = [0u8; 4096];
    let start = Instant::now();
    let mut was_listening = false;
    loop {
        let mut fds = [
            libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: inotify.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        let timeout = if chip.in_rom() { 20 } else { 1000 };
        // SAFETY: fds is valid for its length.
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };

        if fds[1].revents & libc::POLLIN != 0 {
            // SAFETY: buffer is writable for its length.
            while unsafe { libc::read(inotify.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) } > 0 {}
            if fs::read(&value).is_ok_and(|v| v.first() == Some(&b'0')) {
                chip.reset();
            }
        }
        if fds[2].revents & libc::POLLIN != 0 {
            // SAFETY: buffer is writable for its length.
            while unsafe { libc::read(wake.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) } > 0 {}
        }
        if fds[0].revents & libc::POLLIN != 0 {
            // SAFETY: buffer is writable for its length.
            let read = unsafe { libc::read(master.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            if read > 0 {
                let answer = chip.received(&buffer[..read as usize]);
                write_all(master.as_raw_fd(), &answer);
            }
        }
        let offered = chip.tick(start.elapsed().as_millis() as u64);
        write_all(master.as_raw_fd(), &offered);

        let Some(sensor) = &sensor else { continue };
        let listening = chip.listening(sensor.id());
        if listening != was_listening {
            logf!("external temperature: nlclient {} for the sensor", if listening { "is listening" } else { "stopped listening" });
            was_listening = listening;
        }
        if listening {
            if let Some(report) = sensor.due() {
                write_all(master.as_raw_fd(), &Chip::advertisement(&report));
            }
        } else {
            sensor.quiet_tick();
        }
    }
}

/// Puts the stand-in in place, then runs it. Returns once nlclient may start.
pub fn start(sensor: Option<Arc<Sensor>>, ble: &'static Ble, matter: bool) -> io::Result<()> {
    fs::create_dir_all(DIR)?;
    let (inotify, value) = cover_reset(ble)?;
    let (master, had_one) = fetch_master()?;
    if had_one {
        write_all(master.as_raw_fd(), &[RESTART_BLE]);
        logf!("BLE stand-in: asked nlclient to start its BLE over");
    }
    let mut ends = [0 as RawFd; 2];
    // SAFETY: ends is writable for two descriptors.
    check(unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) })?;
    // SAFETY: pipe2 made both, and they are owned from here on.
    let (wake, signal) = unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
    let signal = signal.into_raw_fd();
    if let Some(sensor) = &sensor {
        sensor.set_notify(move || {
            // SAFETY: the pipe lives as long as the process; one byte is readable.
            unsafe { libc::write(signal, b"x".as_ptr().cast(), 1) };
        });
    }
    std::thread::Builder::new().name("ble-stand-in".into())
        .spawn(move || run(sensor, master, inotify, value, wake))?;
    if !matter {
        std::thread::spawn(move || sleep_real_chip(ble));
    }
    logf!("BLE stand-in: nlclient's BLE goes to {}", ble.uart);
    Ok(())
}
