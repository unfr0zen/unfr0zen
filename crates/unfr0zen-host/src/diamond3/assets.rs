//! The diamond3 firmware the binary carries (build.rs): the ROM bootstrap,
//! the boot wrapper and recovery U-Boot, the EM3581's Thread image, its
//! initramfs's /init and its bundle's hook scripts.

pub const ROM_STAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/rom-stage.imx"));
pub const BOOT_WRAPPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/boot-wrapper.bin"));
pub const RECOVERY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/u-boot-recovery.bin"));
pub const OFFICIAL_LOADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/u-boot-sdp-official.hab"));
pub const INIT: &[u8] = include_bytes!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/diamond3/initramfs-init"));
pub const PRE_SETUP: &[u8] = include_bytes!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/diamond3/bundle/pre-setup.sh"));
pub const POST_SETUP: &[u8] = include_bytes!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/diamond3/bundle/post-setup.sh"));
/// Gzipped, as with_thread_radio takes them.
pub const THREAD_RADIO: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/thread-radio.bin.gz"));
pub const THREAD_RADIO_IDENTITY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/thread-radio.identity.gz"));
