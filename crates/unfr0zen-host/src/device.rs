//! The devices unfr0zen supports. Diamond and j49 share their ROM USB ID;
//! their application gadgets have distinct identities.

use std::thread;
use std::time::{Duration, Instant};

use unfr0zen_core::{Error, Result};

use crate::{diamond3, j49};
use crate::progress::{stage, Progress};
use crate::usb::{self, Id};

/// The step that waits for the person to put the unit in its boot ROM's USB
/// download mode, named so a front end can present it its own way.
pub const WAIT_USB_BOOT_STAGE: &str = "Wait for USB download mode";

/// The prompt shown before that step.
pub(crate) const HOLD_BUTTON: &str = "Hold the front panel button for 10 seconds to enter USB download mode";

pub use unfr0zen_core::Device;

const ALL: [Device; 3] = [Device::Diamond3, Device::Diamond, Device::J49];

/// The device a USB identity belongs to, in any of its modes.
pub fn identify(id: Id) -> Option<Device> {
    if id == j49::usb::ROM { return Some(Device::J49); }
    ALL.into_iter().find(|&device| mode_name(device, id).is_some())
}

/// The name of the device's mode with this identity.
pub fn mode_name(device: Device, id: Id) -> Option<&'static str> {
    match device {
        Device::Diamond3 => diamond3::usb::mode(id).map(diamond3::usb::Mode::name),
        Device::Diamond => j49::usb::mode_for(id, j49::usb::DIAMOND_APP).map(j49::usb::Mode::name),
        Device::J49 => j49::usb::mode(id).map(j49::usb::Mode::name),
    }
}

/// The identity of its boot ROM's USB download mode.
fn usb_boot(device: Device) -> Id {
    match device {
        Device::Diamond3 => diamond3::usb::SDP,
        Device::Diamond => j49::usb::ROM,
        Device::J49 => j49::usb::ROM,
    }
}

/// A unit on the bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub device: Device,
    /// The physical port, as `usb::list` names it.
    pub port: String,
    pub id: Id,
}

/// Every supported unit on the bus.
fn units() -> Result<Vec<Unit>> {
    Ok(usb::list()?.into_iter()
        .filter_map(|(port, id)| identify(id).map(|device| Unit { device, port, id }))
        .collect())
}

/// The one unit, from units on the bus.
fn single(mut found: Vec<Unit>) -> Result<Option<Unit>> {
    if found.len() > 1 {
        let names: Vec<String> = found.iter().map(|u| format!("{} at {}", u.device.name(), u.port)).collect();
        return Err(Error::Device(format!("several units on USB ({}); connect only one", names.join(", "))));
    }
    Ok(found.pop())
}

fn selected(mut unit: Unit, omap: Option<Device>) -> Unit {
    if unit.id == j49::usb::ROM { unit.device = if omap == Some(Device::Diamond) { Device::Diamond } else { Device::J49 }; }
    unit
}

/// The unit on the bus, in whatever mode it is in. With none, asks for the
/// button and waits up to `wait` for one in its boot ROM's USB download mode.
pub fn detect(progress: &mut dyn Progress, wait: Duration, omap: Option<Device>) -> Result<Unit> {
    if let Some(unit) = single(units()?)?.map(|unit| selected(unit, omap)) {
        if omap.is_some_and(|wanted| unit.device != wanted) {
            return Err(Error::Device(format!("selected {}, but USB has {}", omap.unwrap().name(), unit.device.name())));
        }
        let mode = mode_name(unit.device, unit.id).unwrap_or("unknown");
        progress.status(&format!("Detected {} in {mode} mode at {} ({:04x}:{:04x})",
                                 unit.device.name(), unit.port, unit.id.0, unit.id.1));
        progress.device(unit.device.description());
        return Ok(unit);
    }
    progress.waiting("Waiting for device connection");
    progress.waiting(HOLD_BUTTON);
    let unit = stage(progress, WAIT_USB_BOOT_STAGE, || {
        let until = Instant::now() + wait;
        let mut last = None;
        while Instant::now() < until {
            let found: Vec<Unit> = units()?.into_iter().filter(|u| u.id == usb_boot(u.device)).collect();
            if last.as_ref() != Some(&found) {
                log::info!("USB: {:?}", found.iter().map(|u| format!("{} {} {:04x}:{:04x}", u.device.name(), u.port, u.id.0, u.id.1))
                    .collect::<Vec<_>>());
                last = Some(found.clone());
            }
            if let Some(unit) = single(found)? {
                let unit = selected(unit, omap);
                if omap.is_some_and(|wanted| unit.device != wanted) { continue; }
                return Ok(unit);
            }
            thread::sleep(Duration::from_millis(500));
        }
        Err(Error::Timeout("no unit entered USB download mode".into()))
    })?;
    progress.device(unit.device.description());
    Ok(unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_mode_identifies_its_device() {
        for id in [diamond3::usb::SDP, diamond3::usb::RECOVERY, diamond3::usb::APP_ACM, diamond3::usb::INITRAMFS] {
            assert_eq!(identify(id), Some(Device::Diamond3), "{id:04x?}");
        }
        for id in [j49::usb::ROM, j49::usb::APP] {
            assert_eq!(identify(id), Some(Device::J49), "{id:04x?}");
        }
        assert_eq!(identify(j49::usb::DIAMOND_APP), Some(Device::Diamond));
        assert_eq!(identify((0x1d6b, 0x0002)), None);
    }

    #[test]
    fn rom_selection_keeps_the_omap_boards_apart() {
        let rom = Unit { device: Device::J49, port: "1-1".into(), id: j49::usb::ROM };
        assert_eq!(selected(rom.clone(), None).device, Device::J49);
        assert_eq!(selected(rom, Some(Device::Diamond)).device, Device::Diamond);
        let app = Unit { device: Device::Diamond, port: "1-1".into(), id: j49::usb::DIAMOND_APP };
        assert_eq!(selected(app, Some(Device::J49)).device, Device::Diamond);
    }

    #[test]
    fn two_units_are_refused() {
        let unit = |port: &str| Unit { device: Device::Diamond3, port: port.into(), id: diamond3::usb::SDP };
        assert_eq!(single(vec![unit("1-1")]).unwrap(), Some(unit("1-1")));
        assert!(single(vec![unit("1-1"), unit("1-2")]).unwrap_err().to_string().contains("connect only one"));
    }
}
