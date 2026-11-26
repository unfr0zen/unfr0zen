//! Onyx (Thermostat E) USB delivery — single-session DCD_WRITE flow.
//!
//! DCD_WRITE reinitializes DDR through the ROM regardless of MMDC state,
//! then upload the loader, FIT, and IVT head with zeroed DCD pointer, then
//! JUMP. Works on both cold and warm sessions.

use std::time::{Duration, Instant};

use unfr0zen_core::diamond3::sdp;
use unfr0zen_core::onyx::fit;
use unfr0zen_core::{Error, Result};

use crate::common::usb::{HidDevice, Mode, Profile};
use crate::usb::Id;

/// The i.MX6UL boot ROM's USB download mode (SDP).
pub const SDP: Id = (0x15A2, 0x007D);

/// The ACM console and fastboot function of the Onyx recovery U-Boot.
pub const RECOVERY: Id = (0x18D1, 0x4EE2);

pub const APP_ACM: Id = (0x2464, 0x0054);
pub const INITRAMFS: Id = (0x0525, 0xA4A8);
const APP: [Id; 3] = [(0x2464, 0x0050), (0x2464, 0x0051), APP_ACM];

/// How long to wait for the unit's SDP session at each stage.
const WAIT_SDP: Duration = Duration::from_secs(130);

pub(crate) static PROFILE: Profile = Profile {
    sdp: SDP,
    recovery: RECOVERY,
    app_acm: APP_ACM,
    app: &APP,
    initramfs: INITRAMFS,
    load: load_recovery,
    watch_load: true,
    power_cycle: true,
};

pub fn mode(id: Id) -> Option<Mode> {
    PROFILE.mode(id)
}

pub fn mode_name(id: Id) -> Option<&'static str> {
    mode(id).map(Mode::name)
}

/// Opens the ROM's SDP HID session and verifies it answers a read.
/// Retries on all errors — the ROM may need time after a watchdog or
/// reset before its SDP endpoint responds.
fn open_sdp(timeout: Duration) -> Result<HidDevice> {
    let deadline = Instant::now() + timeout;
    let mut api = hidapi::HidApi::new().map_err(|e| Error::Io(format!("initialize hidapi: {e}")))?;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Timeout("SDP session did not become ready".into()));
        }
        let _ = api.refresh_devices();
        if let Ok(handle) = api.open(SDP.0, SDP.1) {
            let mut dev = HidDevice(handle);
            match sdp::read_word(&mut dev, 0x021B_C460) {
                Ok(_) => {
                    sdp::drain(&mut dev, 200);
                    return Ok(dev);
                }
                Err(e) => log::info!("SDP probe failed ({e}), retrying"),
            }
        }
        std::thread::sleep(Duration::from_millis(500).min(remaining));
    }
}

pub fn load_recovery() -> Result<Option<String>> {
    let loader = super::assets::OFFICIAL_LOADER;
    let recovery = super::assets::RECOVERY;
    let fdt_at = recovery
        .windows(4)
        .rposition(|bytes| bytes == [0xd0, 0x0d, 0xfe, 0xed])
        .ok_or_else(|| Error::Device("Onyx recovery has no device tree".into()))?;

    let donor = unfr0zen_core::onyx::metadata::template();
    let built = fit::build_recovery_fit(
        &donor,
        &fit::RecoveryImage {
            kernel: recovery,
            fdt: &recovery[fdt_at..],
            load: super::boot::RECOVERY_BASE,
            entry: super::boot::RECOVERY_BASE,
        },
        fit::TARGET_CONFIG,
    )?;
    log::info!("recovery FIT: {} bytes", built.len());

    log::info!("opening SDP session");
    let mut rom = open_sdp(WAIT_SDP)?;

    log::info!("DCD_WRITE + upload + jump");
    let result = super::boot::load_recovery_cold(&mut rom, loader, &built, &mut super::boot::LoaderLog)?;
    drop(rom);

    log::info!("{result}");
    Ok(None)
}
