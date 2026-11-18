//! Replacing a file so that a power cut leaves the old one or the new one,
//! never a short or empty one: the unit loses power whenever it is pulled
//! off its base.

use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Replaces the file at `path` with `bytes`, made with `mode`: written to a
/// sibling and synced, renamed over it, then the directory synced.
pub fn replace(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&partial)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&partial, path)?;
    let dir = path.parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_file_is_replaced_whole_with_its_mode_and_nothing_left_beside_it() {
        let dir = std::env::temp_dir().join(format!("nestlocal-durable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.config");
        std::fs::write(&path, "old, and longer than the new").unwrap();
        replace(&path, b"new", 0o600).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, ["settings.config"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
