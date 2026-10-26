//! Bringing the unit up without changing it: to recovery U-Boot and its
//! console, or on to the application firmware through the stock U-Boot.

use std::time::Duration;

use unfr0zen_core::Result;

use super::usb::{self, Mode};
use super::{connect_recovery, find_unit, reach_recovery, report_silicon};
use crate::device;
use crate::flow::{seconds, start_log};
use crate::progress::{stage, Progress};
use crate::usb::{wait_acm, Id};

fn log_and_find(progress: &mut dyn Progress, wait_usb: Duration, stop_at_app: bool) -> Result<(String, Id)> {
    let unit = device::detect(progress, wait_usb, None)?;
    let (port, id) = find_unit(progress, wait_usb, stop_at_app, unit)?;
    start_log(progress, &chrono::Local::now().format("%Y%m%d-%H%M%S").to_string())?;
    Ok((port, id))
}

/// Loads recovery U-Boot from whatever mode the unit is in and leaves it at its
/// prompt, which keeps the watchdog serviced. Returns the final status line,
/// naming the console's serial port.
pub fn uboot(wait_usb: Duration, progress: &mut dyn Progress) -> Result<String> {
    let (port, id) = log_and_find(progress, wait_usb, false)?;
    reach_recovery(progress, port, id)?;
    let console = stage(progress, "Wait for the recovery console", || wait_acm(usb::RECOVERY, seconds(45)))?;
    Ok(format!("Recovery U-Boot ready on {console}"))
}

/// Boots the application firmware from whatever mode the unit is in: through
/// recovery U-Boot, which chainloads the stock U-Boot from NAND.
pub fn boot(wait_usb: Duration, progress: &mut dyn Progress) -> Result<String> {
    let (port, id) = log_and_find(progress, wait_usb, true)?;
    if usb::mode(id) == Some(Mode::App) {
        return Ok("The application firmware is running".into());
    }
    let port = reach_recovery(progress, port, id)?;
    let mut board = stage(progress, "Connect to recovery console", || connect_recovery(&port))?;
    report_silicon(progress, &mut board);
    stage(progress, "Chainload stock U-Boot from NAND", || board.boot_vendor())?;
    stage(progress, "Wait for the application firmware", || usb::wait_device(&[Mode::App], seconds(120), Some(&port)))?;
    Ok("The application firmware is running".into())
}
