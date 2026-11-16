//! The j49 firmware the binary carries (build.rs): Nest's x-loader, which the
//! boot ROM runs from SRAM, the NAND flasher it loads, the kernel with a USB
//! serial port (those two built in Docker, and empty in a build without it),
//! the SSH server it has none of, its initramfs's busybox and /init, its
//! bundle's pre-setup hook, and the EM357's Thread image.

use std::io::Read;

use unfr0zen_core::nestlocal::Overlay;

pub const XLOADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/x-load.bin"));
pub const FLASHER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/nand-flasher.bin"));
pub const KERNEL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/j49-uImage"));
/// The gen 1's and gen 2's initramfs /init.
pub const INIT: &[u8] = include_bytes!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/j49/initramfs-init"));
/// The gen 1's and gen 2's pre-setup hook; their post-setup is the gen 3's.
pub const PRE_SETUP: &[u8] = include_bytes!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/j49/bundle/pre-setup.sh"));
/// Gzipped, as with_thread_radio takes them.
pub const THREAD_RADIO: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/thread-radio-em357.bin.gz"));
pub const THREAD_RADIO_IDENTITY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/thread-radio-em357.identity.gz"));

/// The static musl busybox of the gen 1's and gen 2's initramfs.
pub fn busybox() -> Vec<u8> {
    let mut busybox = Vec::new();
    flate2::read::GzDecoder::new(&include_bytes!(concat!(env!("OUT_DIR"), "/j49-busybox.gz"))[..])
        .read_to_end(&mut busybox)
        .expect("embedded at build time");
    busybox
}

/// dropbear and its init script, for --ssh.
pub fn ssh_server() -> Overlay {
    let mut dropbear = Vec::new();
    flate2::read::GzDecoder::new(&include_bytes!(concat!(env!("OUT_DIR"), "/dropbearmulti.gz"))[..])
        .read_to_end(&mut dropbear)
        .expect("embedded at build time");
    Overlay {
        directories: Vec::new(),
        files: vec![
            ("usr/sbin/dropbearmulti".into(), 0o755, dropbear),
            (
                "etc/init.d/unfr0zen-ssh".into(),
                0o755,
                include_bytes!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/j49/ssh.init")).to_vec(),
            ),
        ],
    }
}
