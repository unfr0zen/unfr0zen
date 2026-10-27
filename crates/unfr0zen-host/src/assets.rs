//! What the binary carries (build.rs) for any device: the initramfs's busybox,
//! nestlocal for --mqtt, Doom with its WAD for --doom, and client-ng
//! for --client-ng. Each device's own firmware is in its module.

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

/// client-ng for --client-ng: the binary in /nestlabs/sbin, its skins,
/// menus and plugins (with Doom's logo, from the WAD) in
/// /nestlabs/share/client-ng.
pub fn client_ng() -> Overlay {
    const SHARE: &str = "nestlabs/share/client-ng";
    const LOGO: &str = "config/plugins/doom-logo.png";
    let data = gunzip(include_bytes!(concat!(env!("OUT_DIR"), "/client.gz")));
    let u32_at = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
    let mut staged = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let n = u32_at(at);
        let name = String::from_utf8(data[at + 4..at + 4 + n].to_vec()).expect("embedded at build time");
        let mode = u32_at(at + 4 + n) as u32;
        let len = u32_at(at + 8 + n);
        staged.push((name, mode, data[at + 12 + n..at + 12 + n + len].to_vec()));
        at += 12 + n + len;
    }
    staged.retain(|(name, _, _)| name != LOGO);
    staged.push((LOGO.into(), 0o644, include_bytes!(concat!(env!("OUT_DIR"), "/doom-logo.png")).to_vec()));
    let mut directories = std::collections::BTreeSet::from([SHARE.to_string()]);
    let files = staged.into_iter().map(|(name, mode, bytes)| {
        if name == "client-ng" {
            return (unfr0zen_core::patch::CLIENT_BINARY.to_string(), mode, bytes);
        }
        let path = format!("{SHARE}/{name}");
        let mut dir = std::path::Path::new(&path).parent();
        while let Some(d) = dir.filter(|d| d.as_os_str().len() > SHARE.len()) {
            directories.insert(d.to_string_lossy().into_owned());
            dir = d.parent();
        }
        (path, mode, bytes)
    }).collect();
    Overlay { files, directories: directories.into_iter().map(|d| (d, 0o755)).collect() }
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

#[cfg(test)]
mod tests {
    #[test]
    fn client_ng_carries_the_client_its_menus_and_dooms_logo_in_place() {
        let ui = super::client_ng();
        let file = |name: &str| ui.files.iter().find(|(n, _, _)| n == name).map(|(_, mode, data)| (*mode, data.len()));
        assert!(matches!(file(unfr0zen_core::patch::CLIENT_BINARY), Some((0o755, n)) if n > 0));
        assert!(file("nestlabs/share/client-ng/config/menus.lua").is_some());
        assert!(file("nestlabs/share/client-ng/config/plugins/unfr0zen.lua").is_some());
        let logo = ui.files.iter().find(|(n, _, _)| n.ends_with("plugins/doom-logo.png")).unwrap();
        assert!(logo.2.starts_with(b"\x89PNG"));
        let dirs: Vec<&str> = ui.directories.iter().map(|(d, _)| d.as_str()).collect();
        assert!(dirs.contains(&"nestlabs/share/client-ng/config/plugins"));
        assert_eq!(dirs[0], "nestlabs/share/client-ng", "parents first");
    }
}
