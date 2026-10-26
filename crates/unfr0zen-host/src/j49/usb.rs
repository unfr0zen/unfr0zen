//! The OMAP boards' USB links: their boot ROM's peripheral boot, which x-loader and
//! the flasher keep (one identity throughout), and the application's gadget.

use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

use nusb::MaybeFuture;
use unfr0zen_core::{Error, Result};

use crate::usb::{self, port_name, usb_error, Id, Pipe};

/// The OMAP3630 boot ROM; x-loader's USB loader and the flasher enumerate
/// the same way.
pub const ROM: Id = (0x0451, 0xd00e);
/// The application's dynamic gadget: mass storage, and ACM with a kernel
/// built for it.
pub const APP: Id = (0x2464, 0x0002);
pub const DIAMOND_APP: Id = (0x2464, 0x0001);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Rom,
    App,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Rom => "USB boot (OMAP ROM)",
            Mode::App => "app",
        }
    }
}

pub fn mode(id: Id) -> Option<Mode> {
    mode_for(id, APP)
}

pub fn mode_for(id: Id, app: Id) -> Option<Mode> {
    match id {
        ROM => Some(Mode::Rom),
        id if id == app => Some(Mode::App),
        _ => None,
    }
}

/// Waits for the unit in one of the wanted modes, on the given port if any.
pub fn wait_device(wanted: &[Mode], timeout: Duration, port: Option<&str>) -> Result<(String, Id)> {
    wait_device_for(wanted, timeout, port, APP)
}

pub fn wait_device_for(wanted: &[Mode], timeout: Duration, port: Option<&str>, app: Id) -> Result<(String, Id)> {
    let until = Instant::now() + timeout;
    let mut last = None;
    while Instant::now() < until {
        let found: BTreeMap<String, Id> = usb::list()?.into_iter()
            .filter(|(name, id)| mode_for(*id, app).is_some_and(|m| wanted.contains(&m)) && port.is_none_or(|p| p == name))
            .collect();
        if last.as_ref() != Some(&found) {
            log::info!("USB: {:?}", found.iter().map(|(p, (v, d))| format!("{p} {v:04x}:{d:04x}")).collect::<Vec<_>>());
            last = Some(found.clone());
        }
        if found.len() > 1 {
            return Err(Error::Device("several OMAP units on USB; connect only one".into()));
        }
        if let Some(found) = found.into_iter().next() {
            return Ok(found);
        }
        thread::sleep(Duration::from_millis(100));
    }
    let names: Vec<&str> = wanted.iter().map(|m| m.name()).collect();
    Err(Error::Timeout(format!("the OMAP unit did not enter {} mode", names.join("/"))))
}

/// Opens the boot ROM (or x-loader) on any port, as soon as it appears
/// within `timeout`: peripheral boot lasts only seconds. Its configuration
/// is kept as the ROM set it, as NoLongerEvil's host does; it is set only
/// where the interface cannot be claimed without.
pub fn open_rom(timeout: Duration) -> Result<(String, Pipe)> {
    let until = Instant::now() + timeout;
    loop {
        let info = nusb::list_devices().wait().map_err(usb_error)?
            .find(|info| (info.vendor_id(), info.product_id()) == ROM);
        if let Some(info) = info {
            match claim(&info) {
                Ok(pipe) => return Ok((port_name(&info), pipe)),
                // It vanished mid-open: x-loader re-enumerating, or a ROM that gave up.
                Err(error) if Instant::now() < until => log::info!("OMAP boot ROM not ready: {error}"),
                Err(error) => return Err(error),
            }
        } else if Instant::now() >= until {
            return Err(Error::Timeout("the OMAP boot ROM did not appear on USB".into()));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn claim(info: &nusb::DeviceInfo) -> Result<Pipe> {
    let hint = if cfg!(windows) { " (the OMAP boot ROM needs the WinUSB driver: bind it to 0451:d00e with Zadig)" } else { "" };
    let device = info.open().wait().map_err(|e| usb_error(format!("open the OMAP boot ROM: {e}{hint}")))?;
    let interface = match device.claim_interface(0).wait() {
        Ok(interface) => interface,
        Err(first) => {
            log::info!("claiming the OMAP boot ROM's interface: {first}; setting its configuration");
            device.set_configuration(1).wait().map_err(|e| usb_error(format!("configure the OMAP boot ROM: {e}{hint}")))?;
            device.claim_interface(0).wait().map_err(|e| usb_error(format!("claim the OMAP boot ROM: {e}{hint}")))?
        }
    };
    Pipe::new(&interface, "OMAP USB boot")
}
