//! The gen 3's USB links: its identities, and its boot ROM's SDP loader
//! (hidapi) through the officially signed loader.

use std::collections::BTreeMap;
use std::time::Duration;

use unfr0zen_core::diamond3::fastboot::Protocol;
use unfr0zen_core::diamond3::recovery::RAM_BASE;
use unfr0zen_core::diamond3::sdp::{self, UBOOT_BASE};
use unfr0zen_core::{Device, Error, Result};

use crate::common::usb::{rom_silicon, wait_device_ids_for, wait_hid, Mode, Profile, Target};
use crate::usb::{self, Id};

pub const SDP: Id = (0x15A2, 0x0071);
pub const RECOVERY: Id = (0x18D1, 0x4EE0);
pub const APP_ACM: Id = (0x2464, 0x0030);
const APP: [Id; 3] = [(0x2464, 0x0030), (0x2464, 0x0031), (0x0525, 0xB4A4)];
/// The initramfs's own ACM gadget (scripts/initramfs-init).
pub const INITRAMFS: Id = (0x0525, 0xA4A7);

pub(crate) static PROFILE: Profile = Profile {
    sdp: SDP,
    recovery: RECOVERY,
    app_acm: APP_ACM,
    app: &APP,
    initramfs: INITRAMFS,
    load: load_recovery,
    watch_load: false,
    power_cycle: false,
};

pub fn mode(id: Id) -> Option<Mode> {
    PROFILE.mode(id)
}

/// The diamond3 modes on the bus: physical port, identity.
pub fn devices() -> Result<BTreeMap<String, Id>> {
    Ok(usb::list()?.into_iter().filter(|(_, id)| mode(*id).is_some()).collect())
}

/// Waits for the unit in one of the wanted modes, on the given port if any.
pub fn wait_device(
    wanted: &[Mode],
    timeout: Duration,
    port: Option<&str>,
    cancel: &crate::device::CancelFlag,
) -> Result<(String, Id)> {
    wait_device_ids(wanted, &[], timeout, port, cancel)
}

/// A restored system may use Buildroot's ACM/ECM gadget on the same USB port.
pub fn wait_restored_application(timeout: Duration, port: &str) -> Result<(String, Id)> {
    Target(Device::Diamond3).wait_restored(timeout, port)
}

#[cfg(test)]
fn wanted_identity(id: Id, wanted: &[Mode], extra: &[Id]) -> bool {
    mode(id).is_some_and(|m| wanted.contains(&m)) || extra.contains(&id)
}

fn wait_device_ids(
    wanted: &[Mode],
    extra: &[Id],
    timeout: Duration,
    port: Option<&str>,
    cancel: &crate::device::CancelFlag,
) -> Result<(String, Id)> {
    wait_device_ids_for(Target(Device::Diamond3), wanted, extra, timeout, port, cancel)
}

pub fn open_fastboot(port: &str) -> Result<Protocol<usb::Pipe>> {
    crate::common::usb::open_fastboot_for(Target(Device::Diamond3), port)
}

/// One word for the run log: a unit that does not answer an address is
/// worth a warning, never a failed load.
fn diag_word(rom: &mut impl sdp::Hid, address: u32, name: &str) -> Option<u32> {
    sdp::read_word(rom, address).inspect_err(|error| log::warn!("{name} at {address:#010x} not read: {error}")).ok()
}

fn diag_words(rom: &mut impl sdp::Hid, address: u32, len: usize, name: &str) {
    match sdp::read_memory(rom, address, len) {
        Ok(data) => {
            let words: Vec<String> =
                data.chunks(4).map(|w| format!("{:#010x}", u32::from_le_bytes(w.try_into().unwrap()))).collect();
            log::info!("{name}: {}", words.join(" "));
        }
        Err(error) => log::warn!("{name} at {address:#010x} not read: {error}"),
    }
}

/// Fuse state, boot straps and a boot-ROM fingerprint for the run log, so
/// a field report can answer what differs about a unit without hardware
/// access: the OTP shadow banks hold the security configuration and, in
/// bank 3, the SRK hash; the SRC straps hold how the unit entered download
/// mode; and the boot ROM's vector-table data words move with a mask
/// change even when the reported version does not. Each OTP bank spans
/// 0x80 bytes with every fuse value aliased four times at a four-byte
/// stride, so a bank's eight values sit at sixteens and the SRK hash
/// fills bank 3, at 0x021bc580.
fn log_unit_diagnostics(rom: &mut impl sdp::Hid) {
    // OCOTP shadow strip: banks 0 through 3, deduplicated.
    if let Ok(strip) = sdp::read_memory(rom, 0x021B_C400, 0x200) {
        let words: Vec<String> = (0..0x20)
            .map(|i| format!("{:#010x}", u32::from_le_bytes(strip[i * 0x10..i * 0x10 + 4].try_into().unwrap())))
            .collect();
        log::info!("OTP: {}", words.join(" "));
    }
    for (address, name) in [(0x020D_8004, "SBMR1"), (0x020D_801C, "SBMR2")] {
        if let Some(word) = diag_word(rom, address, name) {
            log::info!("{name}: {word:#010x}");
        }
    }
    diag_words(rom, 0x24, 0x24, "ROM vectors");
}

/// Boots recovery U-Boot through Google's official SDP loader: its signed
/// image initializes DDR with its own DCD, then finds and executes a legacy
/// image carrying recovery at the staging address. Returns the silicon
/// revision the ROM reported, if it did.
fn load_recovery() -> Result<Option<String>> {
    let mut api = hidapi::HidApi::new().map_err(|e| Error::Io(e.to_string()))?;
    let mut rom = wait_hid(&mut api, SDP, Duration::from_secs(130))?;
    let rom_id = rom_silicon(&mut rom)
        .inspect_err(|error| {
            log::warn!("Silicon revision not read over SDP: {error}");
            sdp::drain(&mut rom, 50);
        })
        .ok();
    log_unit_diagnostics(&mut rom);
    official_loader(&mut rom)?;
    log::info!("Recovery U-Boot started through the official loader; fastboot buffer at {RAM_BASE:#x}");
    Ok(rom_id.map(|id| id.silicon))
}

/// The official loader's layout inside the HAB image.
const OFFICIAL_IVT_ADDR: u32 = 0x877F_F400;
const OFFICIAL_DCD_OFFSET: usize = 44;
const OFFICIAL_DCD_END: usize = 652;
const OFFICIAL_DCD_STAGING: u32 = 0x0091_0000;
/// The official loader uploads only its active extent, not its trailing zeros.
const OFFICIAL_UPLOAD_LEN: usize = 0x6_0C00;
/// Where the official loader looks for a legacy image to boot.
const LEGACY_IMAGE_STAGING: u32 = 0x8300_0000;

/// Wraps `payload` in a U-Boot legacy image header with the given load and
/// entry addresses. The header's CRC covers itself (with header_crc zeroed)
/// and the payload's CRC covers the payload alone.
fn legacy_image(payload: &[u8], load: u32, entry: u32) -> Vec<u8> {
    let data_crc = crc32fast::hash(payload);
    let mut header = [0u8; 64];
    header[0..4].copy_from_slice(&0x2705_1956u32.to_be_bytes());
    // [4..8] header CRC, filled below
    // [8..12] timestamp: 0
    header[12..16].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    header[16..20].copy_from_slice(&load.to_be_bytes());
    header[20..24].copy_from_slice(&entry.to_be_bytes());
    header[24..28].copy_from_slice(&data_crc.to_be_bytes());
    header[28] = 5; // OS: Linux
    header[29] = 2; // arch: ARM
    header[30] = 2; // type: kernel
                    // [31] compression: none (0)
    header[32..44].copy_from_slice(b"unfr0zen\0\0\0\0");
    let header_crc = crc32fast::hash(&header);
    header[4..8].copy_from_slice(&header_crc.to_be_bytes());
    [header.to_vec(), payload.to_vec()].concat()
}

/// Boots recovery U-Boot through Google's official SDP loader: the signed
/// loader is uploaded and jumped to; its manufacturing boot path finds and
/// executes a legacy image carrying recovery at the staging address.
fn official_loader(rom: &mut impl sdp::Hid) -> Result<()> {
    let image = crate::diamond3::assets::OFFICIAL_LOADER;
    let dcd = &image[OFFICIAL_DCD_OFFSET..OFFICIAL_DCD_END];
    sdp::dcd_write(rom, OFFICIAL_DCD_STAGING, dcd)?;
    log::info!("DCD applied: {} bytes at {OFFICIAL_DCD_STAGING:#x}", dcd.len());
    let loader = &image[..OFFICIAL_UPLOAD_LEN];
    sdp::write_checked(rom, OFFICIAL_IVT_ADDR, loader)?;
    log::info!("Official loader: {} bytes at {OFFICIAL_IVT_ADDR:#x}; read back intact", loader.len());
    let payload = legacy_image(crate::diamond3::assets::RECOVERY, UBOOT_BASE, UBOOT_BASE);
    let padded_len = payload.len().div_ceil(sdp::HID_DATA_SIZE) * sdp::HID_DATA_SIZE;
    let mut padded = payload.clone();
    padded.resize(padded_len, 0);
    sdp::write_checked(rom, LEGACY_IMAGE_STAGING, &padded)?;
    log::info!("Recovery legacy image: {} bytes at {LEGACY_IMAGE_STAGING:#x}; read back intact", padded.len());
    // Zero the IVT DCD pointer so the official loader does not re-apply DCD.
    let mut first_block = loader[..sdp::HID_DATA_SIZE].to_vec();
    first_block[12..16].copy_from_slice(&[0; 4]);
    sdp::write_checked(rom, OFFICIAL_IVT_ADDR, &first_block)?;
    log::info!("IVT DCD pointer zeroed; jumping to official loader");
    let replies = sdp::jump(rom, OFFICIAL_IVT_ADDR)?;
    log::info!("JUMP replies: {:02x?}", replies);
    if let Some(state) = sdp::hab_state(&replies) {
        log::info!("HAB {state}");
    }
    Ok(())
}

#[cfg(test)]
mod restore_usb_tests {
    use super::*;

    #[test]
    fn restore_accepts_stock_and_homebrew_applications() {
        for id in APP.into_iter().chain([(0x1d6b, 0x0104)]) {
            assert!(wanted_identity(id, &[Mode::App], &[(0x1d6b, 0x0104)]));
        }
        for id in [SDP, RECOVERY, INITRAMFS, (0x1d6b, 0x0002)] {
            assert!(!wanted_identity(id, &[Mode::App], &[(0x1d6b, 0x0104)]));
        }
    }

    #[test]
    fn stock_operations_do_not_accept_generic_linux_gadgets() {
        assert!(!wanted_identity((0x1d6b, 0x0104), &[Mode::App], &[]));
        assert_eq!(mode((0x1d6b, 0x0104)), None);
    }

    #[test]
    #[ignore = "requires a booted physical device and UNFR0ZEN_TEST_USB_PORT"]
    fn restored_application_on_requested_port() {
        let port = std::env::var("UNFR0ZEN_TEST_USB_PORT").unwrap();
        let (actual, _) = wait_restored_application(Duration::from_secs(2), &port).unwrap();
        assert_eq!(actual, port);
    }
}
