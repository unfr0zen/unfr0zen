//! Nest SPI to OpenThread HDLC bridge. Requires exclusive radio access.
#[path = "../device/mod.rs"]
#[allow(dead_code)]
mod device;
#[path = "../zigbee/spi.rs"]
#[allow(dead_code)]
mod spi;
#[path = "../radio_wakeup.rs"]
#[allow(dead_code)]
mod wakeup;

fn payload(response: &[u8]) -> Result<&[u8], String> {
    match response.split_first() {
        Some((0x52 | 0x54, data)) => Ok(data),
        Some((0x53, fault)) => Err(format!("RCP fault: {fault:02x?}")),
        _ => Err("invalid RCP response marker".into()),
    }
}

fn verify_identity(response: &[u8], expected: &str) -> Result<(), String> {
    let expected = expected.trim();
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
        return Err("invalid RCP firmware identity file".into());
    }
    if response.first() != Some(&0x52) || response.get(1..) != Some(expected.as_bytes()) {
        return Err("RCP firmware identity mismatch; radio needs flashing".into());
    }
    Ok(())
}

fn main() -> Result<(), String> {
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::{fs::PermissionsExt, net::UnixDatagram};
    let trace = std::env::args().any(|arg| arg == "--trace");
    let mut radio = spi::Spi::open()?;
    wakeup::arm()?;
    radio.reset().map_err(|e| format!("resetting the radio: {e}"))?;
    let status = radio.ezsp(&[0]).map_err(|e| format!("RCP status: {e}"))?;
    if status != [0x52, 2, 0] {
        return Err(format!("unsupported RCP: {status:02x?}"));
    }
    let args: Vec<_> = std::env::args().collect();
    let identity_path = match args.iter().position(|arg| arg == "--identity") {
        Some(index) => args.get(index + 1).ok_or("--identity requires a file")?.as_str(),
        None => "/usr/share/nestlocal/thread-rcp.identity",
    };
    let expected = std::fs::read_to_string(identity_path).map_err(|e| format!("{identity_path}: {e}"))?;
    verify_identity(&radio.ezsp(&[4]).map_err(|e| format!("RCP identity: {e}"))?, &expected)?;
    if std::env::args().any(|arg| arg == "--probe") { return Ok(()); }
    let mut master = -1;
    let mut slave = -1;
    let mut name = [0 as libc::c_char; 128];
    unsafe {
        if libc::openpty(
            &mut master,
            &mut slave,
            name.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) != 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let mut termios = std::mem::zeroed();
        if libc::tcgetattr(slave, &mut termios) != 0 {
            return Err("tcgetattr failed".into());
        }
        libc::cfmakeraw(&mut termios);
        if libc::tcsetattr(slave, libc::TCSANOW, &termios) != 0 {
            return Err("tcsetattr failed".into());
        }
        println!(
            "{}",
            std::ffi::CStr::from_ptr(name.as_ptr()).to_string_lossy()
        );
    }
    let mut master = unsafe { std::fs::File::from_raw_fd(master) };
    let _slave = unsafe { std::fs::File::from_raw_fd(slave) };
    if unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut buffer = [0u8; 116];
    let alarm_path = "/run/nestlocal-thread-wake.sock";
    let _ = std::fs::remove_file(alarm_path);
    let alarm = UnixDatagram::bind(alarm_path).map_err(|e| e.to_string())?;
    std::fs::set_permissions(alarm_path, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    alarm.set_nonblocking(true).map_err(|e| e.to_string())?;
    let mut alarm_peer = None;
    let mut started = false;
    loop {
        let mut request = [0; 8];
        match alarm.recv_from(&mut request) {
            Ok((8, peer)) => {
                let peer = peer.as_pathname().ok_or("unnamed alarm peer")?.to_path_buf();
                let mut command = vec![3];
                command.extend_from_slice(&request[4..]);
                if radio.ezsp(&command)? != [0x52, 2, 0] {
                    return Err("RCP rejected host alarm".into());
                }
                alarm.send_to(&request, &peer).map_err(|e| e.to_string())?;
                alarm_peer = Some(peer);
            }
            Ok(_) => return Err("invalid host alarm".into()),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
            Err(error) => return Err(error.to_string()),
        }
        match master.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(length) => {
                started = true;
                if trace {
                    eprintln!("TX {:02x?}", &buffer[..length]);
                }
                let mut command = vec![1];
                command.extend_from_slice(&buffer[..length]);
                radio.ezsp(&command)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
            Err(error) => return Err(error.to_string()),
        }
        if started && radio.host_int() {
            let response = radio.ezsp(&[2])?;
            let data = payload(&response)?;
            if trace {
                eprintln!("RX {data:02x?}");
            }
            master
                .write_all(data)
                .map_err(|e| e.to_string())?;
            if response.first() == Some(&0x54) {
                if let Some(peer) = &alarm_peer {
                    alarm.send_to(&[], peer).map_err(|e| e.to_string())?;
                }
            }
            continue;
        }
        // The PTY carries host commands; POLLPRI signals a radio interrupt.
        let mut fds = [
            libc::pollfd { fd: master.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: radio.host_int_fd(), events: libc::POLLPRI | libc::POLLERR, revents: 0 },
            libc::pollfd { fd: alarm.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
        if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // Reading the level also acknowledges the sysfs edge notification.
        radio.host_int();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_identity_requires_exact_build_and_rejects_protocol_only_probes() {
        let expected = "0123456789abcdef".repeat(4);
        let mut response = vec![0x52];
        response.extend_from_slice(expected.as_bytes());
        assert!(verify_identity(&response, &format!("{expected}\n")).is_ok());
        assert!(verify_identity(&[0x52, 2, 0], &expected).is_err());
        response[64] = b'0';
        assert!(verify_identity(&response, &expected).is_err());
        assert!(verify_identity(&response, "invalid").is_err());
    }

    #[test]
    fn every_hdlc_payload_byte_is_distinct_from_fault_reporting() {
        for byte in 0..=255 {
            assert_eq!(payload(&[0x52, byte]).unwrap(), &[byte]);
        }
        assert!(payload(&[0x52]).unwrap().is_empty());
        assert!(payload(&[0x54]).unwrap().is_empty());
        assert_eq!(payload(&[0x54, 0x7e]).unwrap(), &[0x7e]);
        assert!(payload(&[0x53, 0xee, 1, 0]).unwrap_err().starts_with("RCP fault:"));
        assert!(payload(&[]).is_err());
    }
}
