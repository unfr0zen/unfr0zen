//! The gen 1: its own x-loader (platform/diamond/xloader.sh), which the OMAP
//! flow (crate::j49) sends its boot ROM in place of the j49's, and the
//! kernel its initramfs runs on (platform/j49/kernel.sh with diamond); the
//! rest of the flow is the j49's.

/// Empty in a build without Docker.
pub const XLOADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/diamond-x-load.bin"));
/// Empty in a build without Docker.
pub const KERNEL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/diamond-uImage"));
