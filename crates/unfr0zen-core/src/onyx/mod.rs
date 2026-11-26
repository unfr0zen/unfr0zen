//! The Nest Thermostat E (onyx, i.MX6UL): its boot ROM's Serial Download
//! Protocol session, the signed loader image the ROM accepts, and the
//! recovery FIT its factory USB boot path serves.

pub(crate) mod display;
pub mod fit;
pub mod imx;
pub mod metadata;
pub(crate) mod nand;
pub(crate) mod pads;
pub mod recovery;
pub(crate) mod rootfs;
