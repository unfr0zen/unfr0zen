//! The gen 3: its assets, USB identities and ROM loader. Its recovery, setup,
//! boot and NAND flows are the i.MX6 boards' (crate::common).

pub mod assets;
pub mod usb;
pub use unfr0zen_core::diamond3::sensors;
