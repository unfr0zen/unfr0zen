//! What a session reads of the unit's root filesystem: regular files and
//! directories on the allow-list (update_wire::bundle::allowed), on the
//! root's own filesystem, or the scratch volume's for the scratch directory,
//! reached without a symlink. nestlocal already runs as root, so this guards
//! against the app's bugs, not attackers.

use std::fs::{File, Metadata};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use update_wire::bundle::{allowed, in_scratch};
use update_wire::hex;
use update_wire::read::{Found, ReadRequest};

use crate::logf;

/// The most a whole-file read sets aside before it reads: a file's size,
/// up to this.
const PREALLOCATE: u64 = 16 << 20;

/// Where the scratch volume is mounted, under the root.
pub const SCRATCH_MOUNT: &str = "media/scratch";

/// The options of the filesystem last mounted at `at` ("/",
/// "/media/scratch") in mounts, /proc/mounts' text; None when none is.
pub fn mount_options<'a>(mounts: &'a str, at: &str) -> Option<&'a str> {
    mounts.lines().rev().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        (fields.get(1) == Some(&at)).then(|| fields.get(3).copied().unwrap_or(""))
    })
}

/// Whether mounts (/proc/mounts) has a filesystem mounted at /media/scratch.
/// Without one, media/scratch is only the root volume's mount point, which a
/// set must not fill and a read must not take for the scratch volume.
pub fn scratch_mounted(mounts: &Path) -> bool {
    std::fs::read_to_string(mounts).is_ok_and(|mounts| mount_options(&mounts, &format!("/{SCRATCH_MOUNT}")).is_some())
}

/// The filesystems a name's components may be on: the root's, and from
/// media/scratch on, for a name in the scratch directory, the scratch
/// volume's mounted there (none, so nothing there is reached, when nothing
/// is mounted there: scratch_mounted).
#[derive(Clone, Copy, Debug)]
pub struct Devices {
    root: u64,
    scratch: Option<u64>,
}

impl Devices {
    pub fn of(root: &Path, scratch_mounted: bool) -> std::io::Result<Devices> {
        let scratch = scratch_mounted.then(|| std::fs::symlink_metadata(root.join(SCRATCH_MOUNT)).ok())
            .flatten().filter(Metadata::is_dir).map(|m| m.dev());
        Ok(Devices { root: std::fs::metadata(root)?.dev(), scratch })
    }

    /// The device name's component `index` (from 0) must be on.
    fn of_part(&self, name: &str, index: usize) -> Option<u64> {
        if index >= 1 && in_scratch(name) { self.scratch } else { Some(self.root) }
    }

    /// Whether name under root is reached without a symlink, each component
    /// on its filesystem, as far as it exists; the metadata of the whole, and
    /// its device, when it exists.
    pub fn reach(&self, root: &Path, name: &str) -> Option<Option<(Metadata, u64)>> {
        let mut path = PathBuf::from(root);
        let mut meta = None;
        for (index, part) in name.split('/').enumerate() {
            path.push(part);
            let device = self.of_part(name, index);
            match std::fs::symlink_metadata(&path) {
                Ok(m) if !m.file_type().is_symlink() && Some(m.dev()) == device => {
                    let dev = m.dev();
                    meta = Some((m, dev));
                }
                Ok(_) => return None,
                Err(_) => return Some(None),
            }
        }
        Some(meta)
    }
}

/// Each of req's names under root, in the request's order; mounts is the
/// unit's /proc/mounts.
pub fn read(root: &Path, mounts: &Path, req: &ReadRequest) -> Vec<Found> {
    let devices = match Devices::of(root, scratch_mounted(mounts)) {
        Ok(devices) => devices,
        Err(e) => {
            logf!("wifi-update: {}: {e}", root.display());
            return vec![Found::Absent; req.names.len()];
        }
    };
    req.names.iter().map(|name| found(root, &devices, name, req.contents)).collect()
}

/// name under root. Absent unless it is allowed, every component of it is
/// on its filesystem (Devices) and none is a symlink, and it is a regular
/// file or a directory.
fn found(root: &Path, devices: &Devices, name: &str, contents: bool) -> Found {
    if allowed(name).is_err() {
        return Found::Absent;
    }
    let Some(Some((meta, device))) = devices.reach(root, name) else {
        return Found::Absent;
    };
    let path = root.join(name);
    match meta {
        meta if meta.is_dir() => Found::Directory { owner_mode: format!("{} {:o}", owner(&meta), meta.mode() & 0o7777) },
        meta if meta.is_file() => file(&path, device, contents).unwrap_or_else(|e| {
            logf!("wifi-update: read {name}: {e}");
            Found::Absent
        }),
        _ => Found::Absent,
    }
}

fn owner(meta: &Metadata) -> String {
    format!("{}:{}", meta.uid(), meta.gid())
}

/// A regular file's digest, and its contents if asked for. It is opened
/// without following a symlink, or blocking on a FIFO, put in its place
/// since it was looked at.
fn file(path: &Path, device: u64, contents: bool) -> std::io::Result<Found> {
    let mut file = File::options().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.dev() != device {
        return Ok(Found::Absent);
    }
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut data = contents.then(|| Vec::with_capacity(meta.len().min(PREALLOCATE) as usize));
    let mut size = 0;
    let mut buf = vec![0; 64 << 10];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
        size += n as u64;
        if let Some(data) = &mut data {
            data.extend_from_slice(&buf[..n]);
        }
    }
    Ok(Found::File { owner: owner(&meta), mode: meta.mode() & 0o7777, sha256: hex(digest.finish().as_ref()), size, data })
}

/// "uid mode" of root's bin/busybox, else of its bin/toybox; empty if it has
/// neither. What core's patch asks of a root before it writes into it.
pub fn shell_binary(root: &Path) -> String {
    ["bin/busybox", "bin/toybox"].iter()
        .find_map(|name| std::fs::metadata(root.join(name)).ok().filter(Metadata::is_file))
        .map(|meta| format!("{} {:o}", meta.uid(), meta.mode() & 0o7777))
        .unwrap_or_default()
}

/// The U-Boot environment's nlmodel, which names the device; empty when it
/// cannot be read. Read once it can be: it does not change while nestlocal
/// runs.
pub fn nlmodel() -> String {
    static NLMODEL: OnceLock<String> = OnceLock::new();
    if let Some(nlmodel) = NLMODEL.get() {
        return nlmodel.clone();
    }
    Command::new("fw_printenv").args(["-n", "nlmodel"]).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|nlmodel| !nlmodel.is_empty())
        .map(|nlmodel| NLMODEL.get_or_init(|| nlmodel).clone())
        .unwrap_or_default()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nestlocal-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["boot", "etc/nestlocal", "bin"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("boot/x"), b"kernel").unwrap();
        std::fs::write(dir.join("etc/nestlocal/options"), b"-zigbee\n").unwrap();
        std::os::unix::fs::symlink("/", dir.join("etc/link")).unwrap();
        std::os::unix::fs::symlink("nestlocal/options", dir.join("etc/options")).unwrap();
        dir
    }

    const ROOT_MOUNT: &str = "ubi0:root0 / ubifs rw,relatime 0 0\n";

    /// A mounts file beside root with the scratch volume mounted.
    fn scratch_mounts(root: &Path) -> PathBuf {
        let mounts = root.with_extension("scratch-mounts");
        std::fs::write(&mounts, format!("{ROOT_MOUNT}ubi0:scratch /media/scratch ubifs rw,relatime 0 0\n")).unwrap();
        mounts
    }

    fn request(names: &[&str]) -> ReadRequest {
        ReadRequest { names: names.iter().map(|n| n.to_string()).collect(), contents: true }
    }

    #[test]
    fn reads_outside_the_allow_list_or_through_a_symlink_are_absent() {
        let root = root("deny");
        let found = read(&root, &scratch_mounts(&root), &request(&["boot/x", "etc/link", "etc/link/etc/passwd", "etc/options",
                                           "etc/nestlocal/../../boot/x", "/etc/nestlocal/options", "etc/nestlocal/options"]));
        assert_eq!(&found[..6], vec![Found::Absent; 6].as_slice());
        assert!(matches!(&found[6], Found::File { data: Some(data), .. } if data == b"-zigbee\n"), "{:?}", found[6]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_file_on_another_device_than_the_root_is_absent() {
        let root = root("device");
        let device = std::fs::metadata(&root).unwrap().dev();
        let on = |device| Devices { root: device, scratch: None };
        assert!(matches!(found(&root, &on(device), "etc/nestlocal/options", false), Found::File { data: None, .. }));
        assert_eq!(found(&root, &on(device.wrapping_add(1)), "etc/nestlocal/options", false), Found::Absent);
        assert_eq!(found(&root, &on(device.wrapping_add(1)), "etc/nestlocal", false), Found::Absent);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_scratch_directory_is_read_on_the_volume_mounted_at_media_scratch() {
        let root = root("scratch");
        std::fs::create_dir_all(root.join("media/scratch/.unfr0zen/bin")).unwrap();
        std::fs::write(root.join("media/scratch/.unfr0zen/bin/nestlocal"), b"ELF").unwrap();
        std::fs::write(root.join("media/scratch/dropbox.vfat"), b"x").unwrap();
        let names = ["media/scratch/.unfr0zen/bin/nestlocal", "media/scratch/.unfr0zen", "media/scratch/dropbox.vfat",
                     "media/scratch"];
        // Nothing mounted there: the root's mount point, which is not read
        // as the scratch volume.
        let unmounted = root.with_extension("mounts");
        std::fs::write(&unmounted, ROOT_MOUNT).unwrap();
        assert_eq!(read(&root, &unmounted, &request(&names)), vec![Found::Absent; 4]);
        assert_eq!(read(&root, &root.with_extension("none"), &request(&names[..1])), vec![Found::Absent], "no mounts to read");
        // Mounted there (on the test's one filesystem).
        let mounts = scratch_mounts(&root);
        let found_here = read(&root, &mounts, &request(&names));
        assert!(matches!(&found_here[0], Found::File { data: Some(data), .. } if data == b"ELF"), "{:?}", found_here[0]);
        assert!(matches!(found_here[1], Found::Directory { .. }));
        assert_eq!(&found_here[2..], [Found::Absent, Found::Absent].as_slice(), "outside the scratch directory");
        // Another volume mounted there: its files, and none of the root's.
        let device = std::fs::metadata(&root).unwrap().dev();
        let mounted = Devices { root: device, scratch: Some(device.wrapping_add(1)) };
        assert_eq!(found(&root, &mounted, names[0], false), Found::Absent);
        assert!(matches!(found(&root, &mounted, "etc/nestlocal/options", false), Found::File { .. }));
        let through = Devices { root: device.wrapping_add(1), scratch: Some(device) };
        assert_eq!(found(&root, &through, names[0], false), Found::Absent, "media itself is the root's");
        // A symlink in its place is not followed.
        std::fs::remove_dir_all(root.join("media/scratch")).unwrap();
        std::fs::create_dir_all(root.join("elsewhere/.unfr0zen/bin")).unwrap();
        std::fs::write(root.join("elsewhere/.unfr0zen/bin/nestlocal"), b"ELF").unwrap();
        std::os::unix::fs::symlink(root.join("elsewhere"), root.join("media/scratch")).unwrap();
        assert_eq!(read(&root, &mounts, &request(&names[..1])), vec![Found::Absent]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn only_regular_files_and_directories_are_found() {
        let root = root("kinds");
        let fifo = std::ffi::CString::new(root.join("etc/fifo").to_str().unwrap()).unwrap();
        // SAFETY: fifo is NUL-terminated.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
        assert_eq!(read(&root, &scratch_mounts(&root), &request(&["etc/fifo"])), vec![Found::Absent]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_shell_binary_is_busybox_s_or_toybox_s_owner_and_mode() {
        let root = root("shell");
        assert_eq!(shell_binary(&root), "");
        std::fs::write(root.join("bin/toybox"), b"").unwrap();
        std::fs::set_permissions(root.join("bin/toybox"), std::fs::Permissions::from_mode(0o750)).unwrap();
        let uid = std::fs::metadata(root.join("bin/toybox")).unwrap().uid();
        assert_eq!(shell_binary(&root), format!("{uid} 750"));
        std::fs::write(root.join("bin/busybox"), b"").unwrap();
        std::fs::set_permissions(root.join("bin/busybox"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(shell_binary(&root), format!("{uid} 755"), "busybox first");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
