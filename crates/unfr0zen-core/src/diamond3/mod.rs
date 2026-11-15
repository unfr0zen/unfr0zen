//! The diamond3 (Nest gen 3, i.MX6SoloX): its boot ROM's Serial Download
//! Protocol, recovery U-Boot with its fastboot buffer, the NAND as recovery
//! reads and writes it, and the scratch volume that keeps unfr0zen's programs
//! and data off the root volume.

pub mod fastboot;
pub mod nand;
pub mod recovery;
pub mod rootfs;
pub mod scratch;
pub mod sdp;
