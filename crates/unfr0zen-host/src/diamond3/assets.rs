//! The diamond3 firmware the binary carries (build.rs): the ROM bootstrap,
//! the boot wrapper and recovery U-Boot, and the EM3581's Thread image.

pub const ROM_STAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rom-stage.imx"));
pub const BOOT_WRAPPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/boot-wrapper.bin"));
pub const RECOVERY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/u-boot-recovery.bin"));
/// Gzipped, as with_thread_radio takes them.
pub const THREAD_RADIO: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/thread-radio.bin.gz"));
pub const THREAD_RADIO_IDENTITY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/thread-radio.identity.gz"));
