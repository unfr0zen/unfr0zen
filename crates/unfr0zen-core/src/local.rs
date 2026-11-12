//! A root volume mounted on the machine running the patch: unfr0zen-apply's,
//! on the unit itself. Every name is relative to the mount point.

use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::process::Command;

use crate::shell::{sha256_hex, Digests, File, Volume};
use crate::{device, Error, Result};

const DROP_CACHES: &str = "/proc/sys/vm/drop_caches";

pub struct LocalVolume {
    root: PathBuf,
}

impl LocalVolume {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        LocalVolume { root: root.into() }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// Owner and mode of a regular file; None if absent or not a file, as
    /// the initramfs shell's `[ -f ]` sees it.
    fn header(&self, name: &str) -> Result<Option<(String, u32, PathBuf)>> {
        let path = self.path(name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return device(format!("{name} on the device is a symbolic link"));
        }
        if !metadata.is_file() {
            return Ok(None);
        }
        Ok(Some((format!("{}:{}", metadata.uid(), metadata.gid()), metadata.mode() & 0o7777, path)))
    }

    fn remount_command(&self, writable: bool) -> Result<()> {
        let option = if writable { "remount,rw" } else { "remount,ro" };
        let status = Command::new("mount").args(["-o", option]).arg(&self.root).status()?;
        if !status.success() {
            return device(format!("mount -o {option} {} failed", self.root.display()));
        }
        Ok(())
    }
}

fn owner_ids(owner: &str) -> Result<(u32, u32)> {
    owner.split_once(':').and_then(|(uid, gid)| Some((uid.parse().ok()?, gid.parse().ok()?)))
        .map_or_else(|| device(format!("not an owner: {owner:?}")), Ok)
}

fn sync() {
    // SAFETY: sync(2) takes no arguments and cannot fail.
    unsafe { libc::sync() }
}

impl Volume for LocalVolume {
    fn read(&mut self, name: &str) -> Result<Option<File<Vec<u8>>>> {
        let Some((owner, mode, path)) = self.header(name)? else { return Ok(None) };
        Ok(Some((owner, mode, fs::read(path)?)))
    }

    fn inspect(&mut self, name: &str) -> Result<Option<File<String>>> {
        let Some((owner, mode, path)) = self.header(name)? else { return Ok(None) };
        Ok(Some((owner, mode, sha256_hex(&fs::read(path)?))))
    }

    fn file_size(&mut self, name: &str) -> Result<u64> {
        Ok(fs::metadata(self.path(name))?.len())
    }

    fn directory(&mut self, name: &str) -> Result<Option<String>> {
        match fs::symlink_metadata(self.path(name)) {
            Ok(metadata) if metadata.is_dir() => {
                Ok(Some(format!("{}:{} {:o}", metadata.uid(), metadata.gid(), metadata.mode() & 0o7777)))
            }
            Ok(_) => device(format!("{name} on the device is not a directory")),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn free_bytes(&mut self) -> Result<u64> {
        let path = std::ffi::CString::new(self.root.as_os_str().as_encoded_bytes())
            .map_err(|_| Error::Device("the root path holds a NUL".into()))?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: path is NUL-terminated and stat is written by statvfs before it is read.
        if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: statvfs returned 0, so it filled stat in.
        let stat = unsafe { stat.assume_init() };
        // Their widths differ between platforms: u32 on some, u64 on others.
        #[allow(clippy::useless_conversion)]
        Ok(u64::from(stat.f_bavail) * u64::from(stat.f_frsize))
    }

    fn shell_binary(&mut self) -> Result<String> {
        for name in ["bin/busybox", "bin/toybox"] {
            if let Ok(metadata) = fs::metadata(self.path(name)) {
                return Ok(format!("{} {:o}", metadata.uid(), metadata.mode() & 0o7777));
            }
        }
        Ok(String::new())
    }

    fn write(&mut self, name: &str, data: &[u8], owner: &str, mode: u32, _payload: &BTreeSet<String>) -> Result<()> {
        let (uid, gid) = owner_ids(owner)?;
        let path = self.path(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut staged = path.clone().into_os_string();
        staged.push(".unfr0zen");
        let staged = PathBuf::from(staged);
        fs::write(&staged, data)?;
        std::os::unix::fs::chown(&staged, Some(uid), Some(gid))?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(mode))?;
        fs::rename(&staged, &path)?;
        Ok(())
    }

    fn make_directory(&mut self, name: &str, mode: u32) -> Result<()> {
        let path = self.path(name);
        fs::create_dir_all(&path)?;
        std::os::unix::fs::chown(&path, Some(0), Some(0))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        Ok(())
    }

    fn remove(&mut self, name: &str) -> Result<()> {
        match fs::remove_file(self.path(name)) {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
    }

    fn remount(&mut self, writable: bool) -> Result<()> {
        // Remounting read-only commits the UBIFS journal.
        if !writable {
            sync();
        }
        self.remount_command(writable)
    }

    fn digests(&mut self, names: &[String]) -> Result<Digests> {
        sync();
        // Read back from flash, not the page cache, where the kernel lets
        // root drop it; a container's /proc refuses, and reads cached.
        match fs::write(DROP_CACHES, "3") {
            Err(e) if !matches!(e.kind(), ErrorKind::NotFound | ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem) => {
                return Err(e.into());
            }
            _ => {}
        }
        names.iter().map(|name| {
            let path = self.path(name);
            let sha = fs::read(&path).ok().map(|data| sha256_hex(&data));
            let owner_mode = fs::metadata(&path).ok()
                .map(|m| format!("{}:{} {:o}", m.uid(), m.gid(), m.mode() & 0o7777));
            Ok((name.clone(), (sha, owner_mode)))
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test's own user and group, which own the files it makes.
    fn owner() -> String {
        // SAFETY: getuid and getgid cannot fail.
        unsafe { format!("{}:{}", libc::getuid(), libc::getgid()) }
    }

    #[test]
    fn writes_by_rename_and_reads_back_owner_mode_and_contents() {
        let dir = tempfile::tempdir().unwrap();
        let mut volume = LocalVolume::new(dir.path());
        let owner = owner();
        volume.write("etc/nestlocal/options", b"-zigbee\n", &owner, 0o640, &BTreeSet::new()).unwrap();
        assert!(!dir.path().join("etc/nestlocal/options.unfr0zen").exists());
        assert_eq!(volume.read("etc/nestlocal/options").unwrap(), Some((owner.clone(), 0o640, b"-zigbee\n".to_vec())));
        assert_eq!(volume.inspect("etc/nestlocal/options").unwrap(), Some((owner, 0o640, sha256_hex(b"-zigbee\n"))));
        assert_eq!(volume.file_size("etc/nestlocal/options").unwrap(), 8);
        let digests = volume.digests(&["etc/nestlocal/options".into(), "absent".into()]).unwrap();
        assert_eq!(digests["etc/nestlocal/options"].0.as_deref(), Some(sha256_hex(b"-zigbee\n").as_str()));
        assert_eq!(digests["absent"], (None, None));
        volume.remove("etc/nestlocal/options").unwrap();
        volume.remove("etc/nestlocal/options").unwrap();
        assert_eq!(volume.read("etc/nestlocal/options").unwrap(), None);
    }

    #[test]
    fn links_are_refused_and_directories_report_owner_and_mode() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("etc")).unwrap();
        fs::set_permissions(dir.path().join("etc"), fs::Permissions::from_mode(0o750)).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("etc/link")).unwrap();
        fs::write(dir.path().join("file"), b"x").unwrap();
        let mut volume = LocalVolume::new(dir.path());
        assert!(volume.read("etc/link").unwrap_err().to_string().contains("symbolic link"));
        assert_eq!(volume.directory("etc").unwrap(), Some(format!("{} 750", owner())));
        assert_eq!(volume.directory("missing").unwrap(), None);
        assert!(volume.directory("file").unwrap_err().to_string().contains("not a directory"));
        assert_eq!(volume.read("etc").unwrap(), None);
        assert!(volume.free_bytes().unwrap() > 0);
    }

    #[test]
    fn the_shell_binary_is_found_through_links() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("bin")).unwrap();
        assert_eq!(LocalVolume::new(dir.path()).shell_binary().unwrap(), "");
        fs::write(dir.path().join("bin/toybox"), b"").unwrap();
        fs::set_permissions(dir.path().join("bin/toybox"), fs::Permissions::from_mode(0o755)).unwrap();
        let uid = owner().split(':').next().unwrap().to_string();
        assert_eq!(LocalVolume::new(dir.path()).shell_binary().unwrap(), format!("{uid} 755"));
    }
}
