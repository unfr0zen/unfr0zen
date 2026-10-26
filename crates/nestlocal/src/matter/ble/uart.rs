use super::gtl;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};


pub struct Uart {
    io: async_io::Async<File>,
    _lock: File,
    saved: libc::termios,
    decoder: gtl::Decoder,
}

impl Uart {
    pub fn open() -> io::Result<Self> {
        let ble = crate::device::current().ble.as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "this unit has no BLE peripheral"))?;
        let config = std::fs::read_to_string("/nestlabs/etc/product.config")?;
        if !config.contains("key=\"ble_disabled\" value=\"1\"") {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "stock BLE owns the radio"));
        }
        let firmware = gtl::firmware(&std::fs::read(ble.firmware)?)?;
        let lock = OpenOptions::new().create(true).truncate(false).write(true).mode(0o600)
            .open("/var/run/unfr0zen/ble.lock")?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // Root can bypass TIOCEXCL, so also check nlclient's existing descriptors.
        let device = std::fs::canonicalize(ble.uart)?;
        for process in std::fs::read_dir("/proc")?.flatten() {
            if let Ok(fds) = std::fs::read_dir(process.path().join("fd")) {
                for fd in fds.flatten() {
                    if std::fs::read_link(fd.path()).ok().as_ref() == Some(&device) {
                        return Err(io::Error::new(io::ErrorKind::WouldBlock, "BLE UART is in use"));
                    }
                }
            }
        }
        let temporary = format!("/var/run/unfr0zen/ble-{}.bin", std::process::id());
        let loaded = (|| {
            let mut file = OpenOptions::new().create_new(true).write(true).mode(0o600).open(&temporary)?;
            file.write_all(&firmware)?;
            let mut child = std::process::Command::new("/nestlabs/sbin/da-loader")
                .args([ble.uart, &temporary, "57600", "no-rtscts", ble.reset])
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()?;
            let started = Instant::now();
            loop {
                if let Some(status) = child.try_wait()? {
                    return if status.success() { Ok(()) } else { Err(io::Error::other("DA14580 firmware upload failed")) };
                }
                if started.elapsed() > Duration::from_secs(20) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "DA14580 firmware upload timed out"));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })();
        let _ = std::fs::remove_file(&temporary);
        loaded?;
        let file = OpenOptions::new().read(true).write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK).open(&device)?;
        let fd = file.as_raw_fd();
        let mut saved = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 { return Err(io::Error::last_os_error()); }
        let mut termios = saved;
        unsafe {
            libc::cfmakeraw(&mut termios);
            libc::cfsetispeed(&mut termios, libc::B115200);
            libc::cfsetospeed(&mut termios, libc::B115200);
        }
        termios.c_cflag |= libc::CLOCAL | libc::CREAD | libc::CRTSCTS;
        termios.c_cc[libc::VMIN] = 1;
        termios.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) } != 0
            || unsafe { libc::ioctl(fd, libc::TIOCEXCL) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { io: async_io::Async::new(file)?, _lock: lock, saved, decoder: gtl::Decoder::default() })
    }

    pub async fn send(&self, op: u16, body: &[u8]) -> io::Result<()> {
        let bytes = gtl::frame(op, body);
        let mut offset = 0;
        while offset < bytes.len() {
            let len = self.io.write_with(|mut file| file.write(&bytes[offset..])).await?;
            if len == 0 { return Err(io::ErrorKind::WriteZero.into()); }
            offset += len;
        }
        Ok(())
    }

    pub async fn receive(&mut self) -> io::Result<(u16, Vec<u8>)> {
        loop {
            if let Some(message) = self.decoder.next()? { return Ok(message); }
            let mut bytes = [0; 512];
            let len = self.io.read_with(|mut file| file.read(&mut bytes)).await?;
            if len == 0 { return Err(io::ErrorKind::UnexpectedEof.into()); }
            self.decoder.push(&bytes[..len])?;
        }
    }

    pub async fn command(&mut self, op: u16, body: &[u8], response: u16, success: &[u8]) -> io::Result<()> {
        use embassy_futures::select::{select, Either};
        self.send(op, body).await?;
        let reply = async {
            loop {
                let (op, body) = self.receive().await?;
                if op == response {
                    return if body == success { Ok(()) } else { Err(io::Error::other(format!("GTL response {op:#x}: {body:?}"))) };
                }
            }
        };
        match select(reply, async_io::Timer::after(Duration::from_secs(5))).await {
            Either::First(result) => result,
            Either::Second(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "GTL command timed out")),
        }
    }
}

impl Drop for Uart {
    fn drop(&mut self) {
        let fd = self.io.get_ref().as_raw_fd();
        unsafe {
            libc::tcsetattr(fd, libc::TCSANOW, &self.saved);
            libc::ioctl(fd, libc::TIOCNXCL);
        }
        if let Some(ble) = &crate::device::current().ble {
            let _ = std::fs::write(format!("{}/value", ble.reset), b"1");
        }
    }
}
