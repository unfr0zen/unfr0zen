//! The j49 firmware the binary carries (build.rs): Nest's x-loader, which the
//! boot ROM runs from SRAM, the NAND flasher it loads, the kernel with a USB
//! serial port (those two built in Docker, and empty in a build without it),
//! the SSH server it has none of, and the EM357's Thread image.

use std::io::Read;

use unfr0zen_core::nestlocal::Overlay;

pub const XLOADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/x-load.bin"));
pub const DIAMOND_XLOADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/diamond-x-load.bin"));
pub const FLASHER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/nand-flasher.bin"));
pub const KERNEL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/j49-uImage"));
/// Gzipped, as with_thread_radio takes them.
pub const THREAD_RADIO: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/thread-radio-em357.bin.gz"));
pub const THREAD_RADIO_IDENTITY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/thread-radio-em357.identity.gz"));

/// dropbear and its init script, for --ssh.
pub fn ssh_server() -> Overlay {
    let mut dropbear = Vec::new();
    flate2::read::GzDecoder::new(&include_bytes!(concat!(env!("OUT_DIR"), "/dropbearmulti.gz"))[..])
        .read_to_end(&mut dropbear).expect("embedded at build time");
    Overlay {
        directories: Vec::new(),
        files: vec![
            ("usr/sbin/dropbearmulti".into(), 0o755, dropbear),
            ("etc/init.d/unfr0zen-ssh".into(), 0o755, include_bytes!("../../../../platform/j49/ssh.init").to_vec()),
        ],
    }
}
