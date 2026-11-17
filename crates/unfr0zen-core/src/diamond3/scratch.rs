//! The gen 3's scratch volume (ubi0:scratch, 177 MB, mounted at
//! /media/scratch) holds unfr0zen's programs and data, keeping them off the
//! root volume (71 MB): nestlocal, the update engine, Doom, Thread, the
//! themes and the stock backups. What stays on root is what the unit's own
//! programs read at their stock paths (nlclient, its preloads, scripts and
//! settings) and nestlocal's keys and options. The unit boots and heats
//! without anything on scratch: a lost directory turns its features off
//! until the next setup puts it back.
//!
//! Nest's boot script (cleanup_scratch in /etc/init.d/nestlabs) deletes every
//! top-level entry of /media/scratch but dot-names and the dropbox image, so
//! the directory is a dot-name. The setup reaches it through the root volume's
//! own mount point: the initramfs mounts ubi0:scratch at ROOT/media/scratch,
//! and a running unit has it there.

/// The directory, as a name on the root volume.
pub const DIR: &str = "media/scratch/.unfr0zen";
/// The files of DIR, each with its SHA-256, written by every setup:
/// `sha256  name` lines, names relative to DIR.
pub const MANIFEST: &str = "media/scratch/.unfr0zen/manifest";
/// The Lua themes, one directory each (nestlocal's themes/).
pub const THEMES: &str = "media/scratch/.unfr0zen/themes";
/// The theme library's crash-loop guard: the starts in a row of a theme
/// nlclient did not survive (nestlocal-theme's engine::guard).
pub const THEME_GUARD: &str = "media/scratch/.unfr0zen/theme.guard";

/// What the gen 3 keeps on scratch, by the name it has on the root volume
/// of the gen 2 and of units set up before.
pub const MOVED: &[(&str, &str)] = &[
    ("nestlabs/sbin/nestlocal", "media/scratch/.unfr0zen/bin/nestlocal"),
    ("nestlabs/sbin/unfr0zen-apply", "media/scratch/.unfr0zen/bin/unfr0zen-apply"),
    ("nestlabs/sbin/doom", "media/scratch/.unfr0zen/doom/doom"),
    ("nestlabs/sbin/unfr0zen-doom", "media/scratch/.unfr0zen/doom/unfr0zen-doom"),
    ("usr/share/doom/doom1.wad", "media/scratch/.unfr0zen/doom/doom1.wad"),
    ("nestlabs/sbin/ot-daemon", "media/scratch/.unfr0zen/thread/ot-daemon"),
    ("nestlabs/sbin/ot-ctl", "media/scratch/.unfr0zen/thread/ot-ctl"),
    ("nestlabs/sbin/thread-rcp", "media/scratch/.unfr0zen/thread/thread-rcp"),
    ("usr/share/nestlocal/thread-rcp.bin", "media/scratch/.unfr0zen/thread/thread-rcp.bin"),
    ("usr/share/nestlocal/thread-rcp.identity", "media/scratch/.unfr0zen/thread/thread-rcp.identity"),
    ("nestlabs/lib/libnestlocal_theme.so", "media/scratch/.unfr0zen/lib/libnestlocal_theme.so"),
    ("etc/unfr0zen/nlclient.original", "media/scratch/.unfr0zen/backup/nlclient.original"),
    ("etc/unfr0zen/connmand.original", "media/scratch/.unfr0zen/backup/connmand.original"),
];

/// Directories that held only moved files, by the name each has on scratch.
const MOVED_DIRECTORIES: &[(&str, &str)] =
    &[("usr/share/doom", "media/scratch/.unfr0zen/doom"), ("usr/share/nestlocal", "media/scratch/.unfr0zen/thread")];

/// Where the gen 3 keeps the root volume's `name`: on scratch if it moved.
pub fn name(name: &str) -> &str {
    MOVED.iter().find(|(root, _)| *root == name).map_or(name, |(_, scratch)| scratch)
}

/// Where the gen 3 keeps the root volume's directory `name`.
pub fn directory(name: &str) -> &str {
    MOVED_DIRECTORIES.iter().find(|(root, _)| *root == name).map_or(name, |(_, scratch)| scratch)
}

/// Whether `name` is on scratch.
pub fn holds(name: &str) -> bool {
    name.strip_prefix(DIR).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The directories, parents first, that hold `names` on scratch.
pub fn directories<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    for name in names.into_iter().filter(|n| holds(n)) {
        let mut at = name;
        while let Some((parent, _)) = at.rsplit_once('/') {
            if !holds(parent) {
                break;
            }
            out.insert(parent.to_string());
            at = parent;
        }
    }
    // Sorted, a parent comes before its children.
    out.into_iter().collect()
}

/// MANIFEST's contents for `files` (name, SHA-256), those on scratch.
pub fn manifest<'a>(files: impl IntoIterator<Item = (&'a str, String)>) -> Vec<u8> {
    let lines: std::collections::BTreeMap<&str, String> = files
        .into_iter()
        .filter(|(name, _)| holds(name) && *name != MANIFEST)
        .map(|(name, sha)| (&name[DIR.len() + 1..], sha))
        .collect();
    lines.iter().map(|(name, sha)| format!("{sha}  {name}\n")).collect::<String>().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moved_files_and_directories_map_onto_scratch_and_others_stay() {
        assert_eq!(name("usr/share/doom/doom1.wad"), "media/scratch/.unfr0zen/doom/doom1.wad");
        assert_eq!(name("etc/unfr0zen/nlclient.original"), "media/scratch/.unfr0zen/backup/nlclient.original");
        assert_eq!(name("nestlabs/sbin/nestlocal"), "media/scratch/.unfr0zen/bin/nestlocal");
        assert_eq!(name("etc/nestlocal/options"), "etc/nestlocal/options");
        assert_eq!(directory("usr/share/doom"), "media/scratch/.unfr0zen/doom");
        assert_eq!(directory("etc/nestlocal"), "etc/nestlocal");
        assert!(MOVED.iter().all(|(_, scratch)| holds(scratch)));
        assert!(!holds("media/scratch/.unfr0zenx/a") && !holds("media/scratch/dropbox.vfat") && holds(DIR));
    }

    #[test]
    fn directories_come_parents_first_and_only_on_scratch() {
        let names = [
            "media/scratch/.unfr0zen/themes/mine/fonts/Regular.ttf",
            "media/scratch/.unfr0zen/doom/doom",
            "etc/unfr0zen/options.json",
        ];
        assert_eq!(
            directories(names),
            [
                "media/scratch/.unfr0zen",
                "media/scratch/.unfr0zen/doom",
                "media/scratch/.unfr0zen/themes",
                "media/scratch/.unfr0zen/themes/mine",
                "media/scratch/.unfr0zen/themes/mine/fonts"
            ]
        );
    }

    #[test]
    fn the_manifest_lists_scratch_files_by_their_names_there() {
        let sha = crate::shell::sha256_hex;
        let files = [
            ("media/scratch/.unfr0zen/doom/doom", sha(b"DOOM")),
            ("etc/hosts", sha(b"x")),
            ("media/scratch/.unfr0zen/backup/nlclient.original", sha(b"ELF")),
            (MANIFEST, sha(b"old")),
        ];
        let text = String::from_utf8(manifest(files)).unwrap();
        assert_eq!(
            text,
            format!(
                "{}  backup/nlclient.original\n{}  doom/doom\n",
                crate::shell::sha256_hex(b"ELF"),
                crate::shell::sha256_hex(b"DOOM")
            )
        );
    }
}
