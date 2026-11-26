//! The gen 3's firmware the binary carries (build.rs): the ROM bootstrap,
//! the boot wrapper and recovery U-Boot, and its initramfs's board script.

pub const RECOVERY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/u-boot-recovery.bin"));
pub const OFFICIAL_LOADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/u-boot-sdp-official.hab"));
pub(crate) const INITRAMFS_BOARD: &[u8] =
    include_bytes!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/diamond3/initramfs-board.sh"));
