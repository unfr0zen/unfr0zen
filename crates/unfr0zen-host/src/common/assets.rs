//! What the i.MX6 boards share of the firmware the binary carries
//! (build.rs): the initramfs's /init, the setup bundle's hook scripts, and
//! the EM3581's Thread image.

pub const INIT: &[u8] = include_bytes!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/common/imx6/initramfs-init"));
pub const PRE_SETUP: &[u8] = include_bytes!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/diamond3/bundle/pre-setup.sh"));
pub const POST_SETUP: &[u8] = include_bytes!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/diamond3/bundle/post-setup.sh"));
/// Gzipped, as with_thread_radio takes them.
pub const THREAD_RADIO: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/thread-radio.bin.gz"));
pub const THREAD_RADIO_IDENTITY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/thread-radio.identity.gz"));
