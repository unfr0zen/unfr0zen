//! An update set: the files one apply changes, which the unit swaps in and
//! its guard rolls back together.

use serde::{Deserialize, Serialize};

/// The `Bundle::version` this protocol speaks.
pub const BUNDLE_VERSION: u32 = 1;

/// unfr0zen's own directory on the scratch volume, which the unit mounts at
/// media/scratch (unfr0zen-core's diamond3::scratch): the one place under
/// media a set may read or change.
pub const SCRATCH_DIR: &str = "media/scratch/.unfr0zen";

/// Where nestlocal is installed: on the scratch volume, where a setup puts
/// it, or on the root volume of a unit set up before that (nestlocal.init
/// runs the first it finds).
pub const SCRATCH_NESTLOCAL: &str = "media/scratch/.unfr0zen/bin/nestlocal";
pub const ROOT_NESTLOCAL: &str = "nestlabs/sbin/nestlocal";

/// The only paths a set may change and still only restart nestlocal: its
/// binary, wherever it is, and what it reads at start, the list of the
/// scratch directory's files, and the record of the last run's settings,
/// which only a later setup reads.
pub const RESTART_PATHS: [&str; 8] =
    [ROOT_NESTLOCAL, SCRATCH_NESTLOCAL, "etc/nestlocal/options", "etc/nestlocal/mqtt.password",
     "etc/nestlocal/mqtt-ca.pem", "etc/nestlocal/device-name", "media/scratch/.unfr0zen/manifest",
     "etc/unfr0zen/options.json"];

/// First path components a set never reads or writes: other partitions, the
/// kernel, and filesystems that are not the root's, but SCRATCH_DIR.
const DENIED: [&str; 9] = ["dev", "proc", "sys", "boot", "media", "tmp", "var", "run", "mnt"];

/// Whether path is SCRATCH_DIR or in it.
pub fn in_scratch(path: &str) -> bool {
    path.strip_prefix(SCRATCH_DIR).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The unit's own record of an apply (its journal and staging list), which
/// only the apply, keep and the guard change.
pub const UPDATE_DIR: &str = "etc/nestlocal/update";
/// The suffixes of a file's staged copy and of its backup while a set is on
/// trial: the apply's own, like UPDATE_DIR.
pub const STAGED_SUFFIX: &str = ".unfr0zen-new";
pub const BACKUP_SUFFIX: &str = ".unfr0zen-previous";

/// What the unit does once a set is swapped in. Ordered lightest first, so
/// the stricter of two is their max.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    Restart,
    Reboot,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Write,
    Remove,
    Directory,
}

/// One path of a set. Paths are relative to `/`, as in core.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: Kind,
    /// "uid:gid".
    pub owner: String,
    pub mode: u32,
    /// The SHA-256 (hex) the unit's file must have before the swap; None when
    /// it must be absent.
    pub old: Option<String>,
    /// The SHA-256 (hex) of what a write puts there.
    pub new: Option<String>,
    /// The bytes a write sends.
    pub size: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub version: u32,
    pub class: Class,
    pub force: bool,
    pub entries: Vec<Entry>,
}

/// The unit's answer to an apply: each file of the set once swapped in, as
/// (path, "uid:gid mode-octal", SHA-256 hex), and each directory, as (path,
/// "uid:gid mode-octal").
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub files: Vec<(String, String, String)>,
    pub directories: Vec<(String, String)>,
}

/// Whether a set may read or write `path`. Both ends check it; nestlocal
/// already runs as root, so it guards against bugs, not attackers. Paths
/// reach the unit's shell (the guard), so only characters it takes literally.
pub fn allowed(path: &str) -> Result<(), String> {
    if !path.bytes().all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b)) {
        return Err(format!("{path:?} may only use A-Z, a-z, 0-9, '.', '_', '-' and '/'"));
    }
    let mut parts = path.split('/');
    if parts.clone().any(|part| part.is_empty() || part == "." || part == "..") {
        return Err(format!("{path:?} is not a plain path relative to /"));
    }
    match parts.next() {
        Some(first) if DENIED.contains(&first) && !in_scratch(path) => {
            return Err(format!("{path} is outside what an update may change"));
        }
        _ => {}
    }
    let own = path == UPDATE_DIR || path.starts_with(&format!("{UPDATE_DIR}/"))
        || path.ends_with(STAGED_SUFFIX) || path.ends_with(BACKUP_SUFFIX);
    match own {
        true => Err(format!("{path} is the unit's own record of an update")),
        false => Ok(()),
    }
}

/// The class a set's paths need: Restart only when every one is in
/// RESTART_PATHS.
pub fn class(entries: &[Entry]) -> Class {
    if entries.iter().all(|entry| RESTART_PATHS.contains(&entry.path.as_str())) {
        Class::Restart
    } else {
        Class::Reboot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> Entry {
        Entry {
            path: path.into(),
            kind: Kind::Write,
            owner: "0:0".into(),
            mode: 0o644,
            old: Some("ab".repeat(32)),
            new: Some("cd".repeat(32)),
            size: 12,
        }
    }

    #[test]
    fn allowed_paths() {
        for path in ["etc/nestlocal/options", "nestlabs/sbin/nlclient", SCRATCH_DIR, SCRATCH_NESTLOCAL,
                     "media/scratch/.unfr0zen/doom/doom1.wad"] {
            assert_eq!(allowed(path), Ok(()), "{path}");
        }
        for path in ["boot/kernel_fdt.itb", "dev/mtd0", "media/data/x", "etc/../boot/x", "/etc/x", "etc/a b", "",
                     "media/scratch", "media/scratch/dropbox.vfat", "media/scratch/.unfr0zenx/a",
                     "media/scratch/.unfr0zen/../x", "media/scratch/.unfr0zen/bin/nestlocal.unfr0zen-new"] {
            assert!(allowed(path).is_err(), "{path:?}");
        }
        for path in ["proc/1/mem", "sys/x", "tmp/x", "var/run/x", "run/x", "mnt/x", "etc//x", "etc/./x", "etc/", "etc/x\n"] {
            assert!(allowed(path).is_err(), "{path:?}");
        }
    }

    #[test]
    fn the_units_own_record_of_an_update_is_not_allowed() {
        for path in ["etc/nestlocal/update", "etc/nestlocal/update/journal", "etc/nestlocal/update/staging",
                     "etc/monitrc.unfr0zen-previous", "nestlabs/sbin/nestlocal.unfr0zen-new"] {
            assert!(allowed(path).is_err(), "{path:?}");
        }
        for path in ["etc/nestlocal/updater", "etc/nestlocal/options", "nestlabs/sbin/nestlocal.previous"] {
            assert_eq!(allowed(path), Ok(()), "{path}");
        }
    }

    #[test]
    fn class_is_restart_only_for_nestlocal_and_its_options() {
        let restart = [entry("nestlabs/sbin/nestlocal"), entry("etc/nestlocal/options")];
        assert_eq!(class(&restart), Class::Restart);
        let scratch = [entry(SCRATCH_NESTLOCAL), entry("media/scratch/.unfr0zen/manifest"), entry("etc/nestlocal/options")];
        assert_eq!(class(&scratch), Class::Restart);
        assert_eq!(class(&[entry("media/scratch/.unfr0zen/doom/doom")]), Class::Reboot);
        let all = RESTART_PATHS.map(entry);
        assert_eq!(class(&all), Class::Restart);
        let reboot = [restart[0].clone(), restart[1].clone(), entry("etc/monitrc")];
        assert_eq!(class(&reboot), Class::Reboot);
        assert_eq!(class(&[]), Class::Restart);
    }

    #[test]
    fn bundles_round_trip_through_json() {
        let bundle = Bundle {
            version: BUNDLE_VERSION,
            class: Class::Reboot,
            force: true,
            entries: vec![
                entry("etc/nestlocal/options"),
                Entry { kind: Kind::Remove, new: None, size: 0, ..entry("etc/nestlocal/mqtt-ca.pem") },
                Entry { kind: Kind::Directory, old: None, new: None, size: 0, mode: 0o700, ..entry("root/.ssh") },
            ],
        };
        let json = serde_json::to_string(&bundle).unwrap();
        assert_eq!(serde_json::from_str::<Bundle>(&json).unwrap(), bundle);

        let applied = Applied { files: vec![("etc/nestlocal/options".into(), "0:0 644".into(), "cd".repeat(32))],
                                directories: vec![("root/.ssh".into(), "0:0 700".into())] };
        let json = serde_json::to_string(&applied).unwrap();
        assert_eq!(serde_json::from_str::<Applied>(&json).unwrap(), applied);
    }
}
