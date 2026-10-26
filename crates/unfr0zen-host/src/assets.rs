//! What the binary carries (build.rs) for any device: the initramfs's busybox,
//! nestlocal for --mqtt, and Doom with its WAD for --doom. Each device's own
//! firmware is in its module.

use std::io::Read;

use unfr0zen_core::nestlocal::Overlay;

const BUSYBOX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/busybox.gz"));
const NESTLOCAL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/nestlocal.gz"));
/// The UNFR0ZEN screen, compiled once, for linking into the unit's nlclient.
pub const CUSTOM_SCREEN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/custom-screen.o"));

fn gunzip(data: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    flate2::read::GzDecoder::new(data).read_to_end(&mut output).expect("embedded at build time");
    output
}

/// Debian busybox-static 1.37.0-6+b9, armhf.
pub fn busybox() -> Vec<u8> {
    gunzip(BUSYBOX)
}

/// The static ARMv7 nestlocal.
pub fn nestlocal() -> Vec<u8> {
    gunzip(NESTLOCAL)
}

/// Doom for --doom: the game, its launcher and the shareware WAD.
pub fn doom() -> Overlay {
    Overlay {
        directories: vec![("usr/share/doom".into(), 0o755)],
        files: vec![
            ("nestlabs/sbin/doom".into(), 0o755, gunzip(include_bytes!(concat!(env!("OUT_DIR"), "/doom.gz")))),
            ("nestlabs/sbin/unfr0zen-doom".into(), 0o755, include_bytes!("../../nestlocal/rootfs/doom.sh").to_vec()),
            ("usr/share/doom/doom1.wad".into(), 0o644, gunzip(include_bytes!(concat!(env!("OUT_DIR"), "/doom1.wad.gz")))),
        ],
    }
}

/// The onboard Thread transport's host side, installed only when selected;
/// each device's run adds its radio's Thread image (with_thread_radio).
pub fn thread() -> Overlay {
    Overlay {
        directories: vec![("usr/share/nestlocal".into(), 0o755)],
        files: vec![
            ("etc/init.d/nestlocal-thread".into(), 0o755, include_bytes!("../../nestlocal/rootfs/thread.init").to_vec()),
            ("nestlabs/sbin/thread-rcp".into(), 0o755, gunzip(include_bytes!(concat!(env!("OUT_DIR"), "/thread-thread-rcp.gz")))),
            ("nestlabs/sbin/ot-daemon".into(), 0o755, gunzip(include_bytes!(concat!(env!("OUT_DIR"), "/thread-ot-daemon.gz")))),
            ("nestlabs/sbin/ot-ctl".into(), 0o755, gunzip(include_bytes!(concat!(env!("OUT_DIR"), "/thread-ot-ctl.gz")))),
        ],
    }
}

/// The Thread transport with a radio's Thread image and its identity, both gzipped.
pub(crate) fn with_thread_radio(thread: &Overlay, image: &[u8], identity: &[u8]) -> Overlay {
    let mut overlay = thread.clone();
    overlay.files.push(("usr/share/nestlocal/thread-rcp.bin".into(), 0o644, gunzip(image)));
    overlay.files.push(("usr/share/nestlocal/thread-rcp.identity".into(), 0o644, gunzip(identity)));
    overlay
}
