//! Bringing the unit up without changing it: to recovery U-Boot and its
//! console, or on to the application firmware through the stock U-Boot.

use std::time::Duration;

use unfr0zen_core::Result;

use super::usb::{self, Mode};
use super::{EntryPoint, connect_recovery, find_unit, reach_recovery, report_silicon};
use crate::device::{self, CancelFlag, Unit};
use crate::flow::{seconds, start_log};
use crate::progress::{stage, Progress};
use crate::usb::{wait_acm, Id};

fn open_log(progress: &mut dyn Progress) -> Result<()> {
    start_log(progress, &chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()).map(drop)
}

/// Loads recovery U-Boot from whatever mode the unit is in and leaves it at its
/// prompt, which keeps the watchdog serviced. Returns the final status line,
/// naming the console's serial port.
pub fn uboot(wait_usb: Duration, entry_point: EntryPoint, progress: &mut dyn Progress) -> Result<String> {
    let cancel: CancelFlag = Default::default();
    let unit = device::detect(progress, wait_usb, None, &cancel)?;
    let (port, id) = find_unit(progress, wait_usb, false, unit, &cancel)?;
    load_if_needed(progress, port, id, entry_point)?;
    let console = stage(progress, "Wait for the recovery console", || wait_acm(usb::RECOVERY, seconds(45)))?;
    Ok(format!("Recovery U-Boot ready on {console}"))
}

/// Loads recovery U-Boot into a unit in SDP. One already running it is left
/// alone, with no run log.
fn load_if_needed(progress: &mut dyn Progress, port: String, id: Id, entry_point: EntryPoint) -> Result<()> {
    if id == usb::SDP {
        open_log(progress)?;
    }
    reach_recovery(progress, port, id, entry_point).map(drop)
}

/// Boots the application firmware from whatever mode the unit is in: through
/// recovery U-Boot, which chainloads the stock U-Boot from NAND.
pub fn boot(wait_usb: Duration, entry_point: EntryPoint, progress: &mut dyn Progress) -> Result<String> {
    let cancel: CancelFlag = Default::default();
    let unit = device::detect(progress, wait_usb, None, &cancel)?;
    boot_unit(unit, wait_usb, entry_point, progress, &cancel)
}

/// [`boot`] for the unit found. One already running its application firmware
/// is left alone, with no run log.
fn boot_unit(unit: Unit, wait_usb: Duration, entry_point: EntryPoint, progress: &mut dyn Progress,
             cancel: &CancelFlag) -> Result<String> {
    let (port, id) = find_unit(progress, wait_usb, true, unit, cancel)?;
    if usb::mode(id) == Some(Mode::App) {
        return Ok("The application firmware is running".into());
    }
    open_log(progress)?;
    let port = reach_recovery(progress, port, id, entry_point)?;
    let mut board = connect_recovery(progress, &port)?;
    report_silicon(progress, &mut board);
    stage(progress, "Chainload stock U-Boot from NAND", || board.boot_vendor())?;
    stage(progress, "Wait for the application firmware", || usb::wait_device(&[Mode::App], seconds(120), Some(&port), &Default::default()))?;
    Ok("The application firmware is running".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Notes(Vec<String>);

    impl Progress for Notes {
        fn begin(&mut self, _: &str) {}
        fn end(&mut self, _: &str, _: Duration, _: bool) {}
        fn status(&mut self, message: &str) { self.0.push(message.to_string()); }
        fn waiting(&mut self, _: &str) {}
    }

    #[test]
    fn a_unit_already_in_recovery_gets_no_log_and_nothing_loaded() {
        let mut notes = Notes(Vec::new());
        load_if_needed(&mut notes, "1".into(), usb::RECOVERY, EntryPoint::Auto).unwrap();
        assert!(notes.0.is_empty(), "{:?}", notes.0);
    }

    #[test]
    fn booting_a_unit_already_running_its_application_opens_no_log() {
        let unit = Unit { device: device::Device::Diamond3, port: "1".into(), id: usb::APP_ACM };
        let mut notes = Notes(Vec::new());
        let message = boot_unit(unit, Duration::ZERO, EntryPoint::Auto, &mut notes, &Default::default()).unwrap();
        assert_eq!(message, "The application firmware is running");
        assert!(!notes.0.iter().any(|note| note.starts_with("Detailed log")), "{:?}", notes.0);
    }
}
