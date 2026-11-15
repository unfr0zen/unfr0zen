//! The gen 3's scratch volume, where the setup installs unfr0zen's programs
//! and data (nestlocal, the update engine, themes, Doom, Thread, the stock
//! backups) with a manifest of their SHA-256s (unfr0zen-core's
//! diamond3::scratch). A scratch volume wiped since (an OTA, a factory reset)
//! turns those features off: this says which files are gone or changed, so
//! the log tells to set the unit up again.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::logf;

const DIR: &str = "/media/scratch/.unfr0zen";

/// The first of `paths` that is a file, else the last: a program on the gen
/// 3's scratch volume, else where the gen 1 and gen 2 (or a unit set up
/// before) have it.
pub fn installed(paths: &[&'static str]) -> &'static str {
    paths.iter().copied().find(|p| Path::new(p).is_file()).unwrap_or(paths[paths.len() - 1])
}

/// The manifest's files that are missing or differ under `dir`.
fn damaged(dir: &Path, manifest: &str) -> Vec<String> {
    manifest.lines().filter_map(|line| {
        let (sha, name) = line.split_once("  ")?;
        // Hashed as it is read: the largest files are megabytes.
        let mut hash = Sha256::new();
        let ok = std::fs::File::open(dir.join(name)).and_then(|mut file| std::io::copy(&mut file, &mut hash)).is_ok()
            && hash.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>() == sha;
        (!ok).then(|| name.to_string())
    }).collect()
}

/// Checks the scratch files in the background, logging the outcome.
pub fn check() {
    let spawned = std::thread::Builder::new().name("scratch".into()).spawn(|| {
        let dir = Path::new(DIR);
        let manifest = match std::fs::read_to_string(dir.join("manifest")) {
            Ok(manifest) => manifest,
            Err(e) => return logf!("scratch: {DIR}/manifest: {e}; set the unit up again to restore the files kept there"),
        };
        let damaged = damaged(dir, &manifest);
        if damaged.is_empty() {
            logf!("scratch: {} files verified", manifest.lines().count());
        } else {
            logf!("scratch: missing or changed in {DIR}: {}; set the unit up again to restore them", damaged.join(", "));
        }
    });
    if let Err(e) = spawned {
        logf!("scratch: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_takes_the_first_present_else_the_last() {
        let cargo = env!("CARGO_MANIFEST_DIR");
        let here: &'static str = Box::leak(format!("{cargo}/Cargo.toml").into_boxed_str());
        assert_eq!(installed(&["/nonexistent/a", here, "/nonexistent/b"]), here);
        assert_eq!(installed(&["/nonexistent/a", "/nonexistent/b"]), "/nonexistent/b");
    }

    #[test]
    fn missing_and_changed_files_are_named_and_intact_ones_not() {
        let dir = std::env::temp_dir().join(format!("nestlocal-scratch-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("doom")).unwrap();
        std::fs::write(dir.join("doom/doom"), b"DOOM").unwrap();
        std::fs::write(dir.join("doom/doom1.wad"), b"changed").unwrap();
        let sha = |data: &[u8]| Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect::<String>();
        let manifest = format!("{}  doom/doom\n{}  doom/doom1.wad\n{}  lib/libnestlocal_theme.so\n",
                               sha(b"DOOM"), sha(b"WAD"), sha(b"ELF"));
        assert_eq!(damaged(&dir, &manifest), ["doom/doom1.wad", "lib/libnestlocal_theme.so"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
