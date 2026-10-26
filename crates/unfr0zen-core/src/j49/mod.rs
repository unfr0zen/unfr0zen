//! The j49 (Nest gen 2, TI AM3703): its NAND as raw pages with the GPMC's
//! BCH4 code, the U-Boot environment, and the JFFS2 root slots.

pub mod flasher;
pub mod jffs2;
pub mod kernel;
pub mod nand;
pub mod omap;
pub mod rootfs;
