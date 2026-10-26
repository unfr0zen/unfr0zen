//! The host side of USB common to every device: the bus as physical port and
//! identity (nusb: IOKit, usbfs or WinUSB, no libusb), and ACM serial ports.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::thread;
use std::time::{Duration, Instant};

use nusb::transfer::{Buffer, Bulk, In, Out};
use nusb::MaybeFuture;
use unfr0zen_core::console::Serial;
use unfr0zen_core::{bulk, Error, Result};

pub type Id = (u16, u16);

pub(crate) fn usb_error(error: impl std::fmt::Display) -> Error {
    Error::Io(format!("USB: {error}"))
}

/// A physical port: the bus and the hub ports down to the device.
pub(crate) fn port_name(info: &nusb::DeviceInfo) -> String {
    let chain: Vec<String> = info.port_chain().iter().map(u8::to_string).collect();
    format!("{}-{}", info.bus_id(), chain.join("."))
}

/// Every device on the bus: physical port, identity.
pub fn list() -> Result<BTreeMap<String, Id>> {
    Ok(nusb::list_devices().wait().map_err(usb_error)?
        .map(|info| (port_name(&info), (info.vendor_id(), info.product_id())))
        .collect())
}

/// A claimed interface's bulk OUT and IN endpoints.
pub struct Pipe {
    out: nusb::Endpoint<Bulk, Out>,
    input: nusb::Endpoint<Bulk, In>,
    packet: usize,
    name: &'static str,
}

impl Pipe {
    /// The interface's first bulk endpoint each way; `name` labels its errors.
    pub fn new(interface: &nusb::Interface, name: &'static str) -> Result<Pipe> {
        let descriptor = interface.descriptor().ok_or_else(|| Error::Device(format!("{name} interface has no descriptor")))?;
        let (mut out, mut input, mut packet) = (None, None, 512);
        for endpoint in descriptor.endpoints() {
            if endpoint.transfer_type() == nusb::descriptors::TransferType::Bulk {
                if endpoint.address() & 0x80 != 0 {
                    input = input.or(Some(endpoint.address()));
                    packet = endpoint.max_packet_size();
                } else {
                    out = out.or(Some(endpoint.address()));
                }
            }
        }
        let (Some(out), Some(input)) = (out, input) else {
            return Err(Error::Device(format!("{name} interface lacks bulk endpoints")));
        };
        Ok(Pipe {
            out: interface.endpoint::<Bulk, Out>(out).map_err(usb_error)?,
            input: interface.endpoint::<Bulk, In>(input).map_err(usb_error)?,
            packet,
            name,
        })
    }
}

impl bulk::Bulk for Pipe {
    fn send(&mut self, data: &[u8], timeout: Duration) -> Result<()> {
        self.out.transfer_blocking(Buffer::from(data.to_vec()), timeout).into_result()
            .map(drop).map_err(|e| usb_error(format!("{} send: {e}", self.name)))
    }

    fn receive(&mut self, max: usize, timeout: Duration) -> Result<Vec<u8>> {
        let requested = max.max(1).next_multiple_of(self.packet);
        match self.input.transfer_blocking(Buffer::new(requested), timeout).into_result() {
            Ok(buffer) => Ok(buffer.into_vec()),
            Err(nusb::transfer::TransferError::Cancelled) => Err(Error::Timeout(format!("{}: no answer from the unit", self.name))),
            Err(e) => Err(usb_error(format!("{} receive: {e}", self.name))),
        }
    }
}

/// The ACM serial port of the unit in the given identity.
pub fn acm_port(id: Id) -> Option<String> {
    serialport::available_ports().ok()?.into_iter().find_map(|port| match port.port_type {
        // macOS lists every ACM twice; the callout device does not wait for carrier.
        serialport::SerialPortType::UsbPort(usb) if (usb.vid, usb.pid) == id && !port.port_name.starts_with("/dev/tty.") => {
            Some(port.port_name)
        }
        _ => None,
    })
}

pub fn wait_acm(id: Id, timeout: Duration) -> Result<String> {
    let until = Instant::now() + timeout;
    while Instant::now() < until {
        if let Some(port) = acm_port(id) {
            return Ok(port);
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(Error::Timeout(format!("ACM interface for {:04x}:{:04x} did not appear", id.0, id.1)))
}

/// Waits for the ACM port of `id` and opens it. A port can be listed before
/// it can be opened (Windows answers "a device which does not exist"), so a
/// failed open is retried until the timeout.
pub fn open_acm(id: Id, timeout: Duration) -> Result<Port> {
    let until = Instant::now() + timeout;
    let mut failed = None;
    while Instant::now() < until {
        if let Some(name) = acm_port(id) {
            match Port::open(&name) {
                Ok(port) => return Ok(port),
                Err(error) => {
                    if failed.is_none() {
                        log::info!("{name} not ready yet: {error}");
                    }
                    failed = Some(error);
                }
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(failed.unwrap_or_else(|| Error::Timeout(format!("ACM interface for {:04x}:{:04x} did not appear", id.0, id.1))))
}

/// An ACM port with DTR held: recovery services its watchdog while it is.
pub struct Port(Box<dyn serialport::SerialPort>);

impl Port {
    pub fn open(name: &str) -> Result<Self> {
        let mut port = serialport::new(name, 115_200).timeout(Duration::from_millis(250)).open()
            .map_err(|e| Error::Io(format!("{name}: {e}")))?;
        port.write_data_terminal_ready(true).map_err(|e| Error::Io(format!("{name}: {e}")))?;
        Ok(Port(port))
    }
}

impl Read for Port {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
}

impl Write for Port {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.write(data)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Serial for Port {
    fn clear_input(&mut self) -> io::Result<()> {
        self.0.clear(serialport::ClearBuffer::Input).map_err(io::Error::other)
    }
}

/// Waits for the application's ACM `id` on the port and its uid-0 answer to a
/// one-time challenge: a USB device appearing is not proof of a shell.
///
/// The port stays open: bytes written right after an open can be lost, and
/// a challenge cut short never runs. So after a settle, each challenge goes
/// after Ctrl-C and Enter, which clear a half-received line, every three
/// seconds; an answer to any of them counts.
pub fn verify_shell(port: &str, id: Id, timeout: Duration) -> Result<String> {
    let until = Instant::now() + timeout;
    let mut open: Option<(String, Port)> = None;
    let (mut answers, mut reply, mut next) = (Vec::new(), Vec::new(), Instant::now());
    while Instant::now() < until {
        if open.is_none() {
            let name = if list()?.get(port) == Some(&id) { acm_port(id) } else { None };
            match name.map(|name| Port::open(&name).map(|shell| (name, shell))) {
                Some(Ok(found)) => {
                    log::info!("Opened {} for the root shell check", found.0);
                    open = Some(found);
                    reply.clear();
                    thread::sleep(Duration::from_secs(1));
                    next = Instant::now();
                }
                Some(Err(error)) => {
                    log::info!("Application ACM not ready: {error}");
                    thread::sleep(Duration::from_secs(1));
                }
                None => thread::sleep(Duration::from_millis(500)),
            }
            continue;
        }
        let (name, shell) = open.as_mut().expect("opened above");
        if Instant::now() >= next {
            let mut bytes = [0u8; 8];
            getrandom::fill(&mut bytes).map_err(|e| Error::Io(e.to_string()))?;
            let nonce: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            answers.push(regex::Regex::new(&format!(r"{nonce}:\s*0\b")).expect("valid pattern"));
            if let Err(error) = shell.write_all(format!("\x03\rprintf '{nonce}:'; id -u\r").as_bytes()) {
                log::info!("Root shell check: {name} dropped: {error}");
                open = None;
                continue;
            }
            next = Instant::now() + Duration::from_secs(3);
        }
        let mut buffer = [0u8; 4096];
        match shell.read(&mut buffer) {
            Ok(n) => reply.extend_from_slice(&buffer[..n]),
            Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => {}
            Err(error) => {
                log::info!("Root shell check: {name} dropped: {error}");
                open = None;
                continue;
            }
        }
        let text = String::from_utf8_lossy(&reply);
        if answers.iter().any(|answer| answer.is_match(&text)) {
            log::info!("Confirmed root shell on {name} (uid 0)");
            return Ok(name.clone());
        }
    }
    log::info!("No uid-0 answer; the shell said: {:?}", String::from_utf8_lossy(&reply[reply.len().saturating_sub(300)..]));
    Err(Error::Timeout("application USB ACM appeared without a confirmed uid-0 shell".into()))
}
