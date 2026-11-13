//! The gen 1: its own x-loader (platform/diamond/xloader.sh), which the OMAP
//! flow (crate::j49) sends its boot ROM in place of the j49's; the rest of
//! the flow is the j49's.

/// Empty in a build without Docker.
pub const XLOADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/diamond-x-load.bin"));
