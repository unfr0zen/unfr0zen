//! The diamond3 (Nest gen 3, i.MX6SoloX): its boot ROM's Serial Download
//! Protocol, recovery U-Boot with its fastboot buffer, the NAND as recovery
//! reads and writes it, the /init of the initramfs that patches a UBIFS root
//! slot, and the scratch volume that keeps unfr0zen's programs and data off it.

/// The /init of the gen 3's initramfs (crate::initramfs).
pub const INIT: &[u8] = include_bytes!("../../../../platform/diamond3/initramfs-init");

pub mod fastboot;
pub mod nand;
pub mod recovery;
pub mod rootfs;
pub mod scratch;
pub mod sdp;
