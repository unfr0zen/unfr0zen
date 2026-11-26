//! What the i.MX6 boards, the gen 3 and the Thermostat E, share: the USB
//! links, the recovery, boot and NAND flows and their assets, each board's part in them given by its
//! [`usb::Profile`].

pub mod assets;
pub mod boot;
pub mod nand;
pub mod recovery;
pub mod usb;
