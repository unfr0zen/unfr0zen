//! Files only root reads, written whole or not at all.

use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The file a write of path goes to first.
pub fn partial(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
}

/// Replaces path with data, mode 0600: a temporary file (partial), synced,
/// then renamed over it, so a reader finds the old file or the new one
/// whole. A temporary file already there is made 0600 too.
pub fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let temporary = partial(path);
    let mut file = std::fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(&temporary)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temporary, path)
}
