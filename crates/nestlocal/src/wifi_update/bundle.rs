//! An update set (update_wire::bundle) applied as one: staged, checked,
//! journaled, then swapped in with what it replaced kept as backups, which
//! rootfs/nestlocal-update-guard puts back should the set not come up.

use std::fs::File;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use update_wire::bundle::{allowed, class, in_scratch, Applied, Bundle, Class, Entry, Kind, BUNDLE_VERSION};
use update_wire::read::{CheckRequest, Checked, Found, ReadRequest, Refused, Rule};

use super::files::{self, SCRATCH_MOUNT};
use update_wire::hex;
use crate::logf;

/// The most a set's files may hold together.
pub const MAX_SIZE: u64 = 32 << 20;
/// Free space left over beside the set's files and their backups.
const MARGIN: u64 = 1 << 20;
/// How long a staged binary's -h or -check-options, or a script's sh -n,
/// may take.
const RUN_FOR: Duration = Duration::from_secs(10);
/// nestlocal, on the root volume of a unit set up before the scratch volume
/// held it, and on the scratch volume, which nestlocal.init runs first.
pub const NESTLOCAL: &str = update_wire::bundle::ROOT_NESTLOCAL;
pub const SCRATCH_NESTLOCAL: &str = update_wire::bundle::SCRATCH_NESTLOCAL;
const OPTIONS: &str = "etc/nestlocal/options";
/// Swapped in last: the screen is the one thing a unit cannot do without.
const NLCLIENT: &str = "nestlabs/sbin/nlclient";
/// Swapped in first: the originals other files are patched from.
const ORIGINALS: &str = "etc/unfr0zen/";
/// Scripts checked with sh -n before they are swapped in.
const SCRIPTS: &str = "etc/init.d/";
const STAGED: &str = update_wire::bundle::STAGED_SUFFIX;
const BACKUP: &str = update_wire::bundle::BACKUP_SUFFIX;
/// The starts a set on trial gets before the guard rolls it back
/// (nestlocal-update-guard STARTS).
const STARTS: u32 = 3;
/// The journal format the guard reads. Changing it incompatibly needs
/// overlay-version bumped (mod.rs IDENTITY).
const JOURNAL_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub struct Paths {
    /// The root filesystem a set changes and a session reads (files.rs).
    pub root: PathBuf,
    pub journal: PathBuf,
    /// What an apply under way staged, for the guard to clear should it
    /// stop before the journal.
    pub staging: PathBuf,
    pub pending: PathBuf,
    pub mounts: PathBuf,
    /// The overlay version a USB patch installed (crates/nestlocal/overlay-version).
    pub overlay: PathBuf,
    pub free_space: fn(&Path) -> std::io::Result<u64>,
}

impl Paths {
    pub fn unit() -> Paths {
        Paths {
            root: "/".into(),
            journal: "/etc/nestlocal/update/journal".into(),
            staging: "/etc/nestlocal/update/staging".into(),
            pending: "/media/data/nestlocal-update.pending".into(),
            mounts: "/proc/mounts".into(),
            overlay: "/etc/nestlocal/overlay-version".into(),
            free_space,
        }
    }

    /// The installed nestlocal: the one nestlocal.init runs, on the scratch
    /// volume if it is there, else on the root volume.
    pub fn binary(&self) -> PathBuf {
        self.root.join(self.installed())
    }

    /// The installed nestlocal's path, relative to the root.
    pub fn installed(&self) -> &'static str {
        if self.root.join(SCRATCH_NESTLOCAL).is_file() { SCRATCH_NESTLOCAL } else { NESTLOCAL }
    }

    /// Where the guard records that it put the last set back, beside the
    /// marker: GET /update reports it until another set is swapped in.
    pub fn rolled_back(&self) -> PathBuf {
        self.pending.with_extension("rolled-back")
    }

    /// Who asked for the last set to be rolled back (roll_back_now), beside
    /// the marker: GET /update reports it with rolled_back until another set
    /// is swapped in. The guard's own rollback of a set that did not come up
    /// leaves none.
    pub fn cancelled(&self) -> PathBuf {
        self.pending.with_extension("cancelled")
    }

    /// Who sent the set swapped in last, beside the marker, for the trial
    /// notice: a laptop's name, or "unfr0zen" for a code's session.
    pub fn sender(&self) -> PathBuf {
        self.pending.with_extension("from")
    }

    /// The guard's lock (nestlocal-update-guard LOCK).
    fn guard_lock(&self) -> PathBuf {
        self.root.join("var/run/nestlocal-update-guard.lock")
    }
}

/// The guard's lock, held until dropped, so that keeping a set and the
/// watcher's rollback of it take turns. Like the guard, it takes over a
/// lock still held after 5 s, left by a guard killed while holding it.
struct GuardLock(Option<PathBuf>);

impl GuardLock {
    fn take(path: PathBuf) -> GuardLock {
        let mut tries = 0;
        loop {
            match std::fs::create_dir(&path) {
                Ok(()) => return GuardLock(Some(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 5 => {
                    std::thread::sleep(Duration::from_secs(1));
                    tries += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    logf!("wifi-update: {}: still held after 5 s: taken over", path.display());
                    return GuardLock(None);
                }
                Err(e) => {
                    logf!("wifi-update: {}: {e}", path.display());
                    return GuardLock(None);
                }
            }
        }
    }
}

impl Drop for GuardLock {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_dir(path);
        }
    }
}

#[derive(Debug)]
pub enum ApplyError {
    OnTrial,
    ReadOnly,
    /// The set changes the scratch directory, and no volume is mounted at
    /// media/scratch.
    NoScratch,
    NotAllowed(String),
    Invalid(String),
    /// A file is not what the set expects to replace.
    Changed(String),
    TooLarge,
    NoSpace,
    /// The contents are not what the set says.
    Mismatch(String),
    NotRunnable(String),
    Options(String),
    /// A script, and why sh -n refused it.
    Script(String, String),
    Io(std::io::Error),
}

impl ApplyError {
    /// The HTTP status the apply is answered with.
    pub fn status(&self) -> u16 {
        match self {
            ApplyError::OnTrial | ApplyError::ReadOnly | ApplyError::NoScratch | ApplyError::Changed(_) => 409,
            ApplyError::NotAllowed(_) => 403,
            ApplyError::Invalid(_) | ApplyError::Mismatch(_) => 400,
            ApplyError::TooLarge => 413,
            ApplyError::NoSpace => 507,
            ApplyError::NotRunnable(_) | ApplyError::Options(_) | ApplyError::Script(..) => 422,
            ApplyError::Io(_) => 500,
        }
    }
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::OnTrial => write!(f, "the previous update is still on trial"),
            ApplyError::ReadOnly => write!(f, "the root filesystem, or the scratch volume the set changes, is read-only"),
            ApplyError::NoScratch => write!(f, "the set changes the scratch volume, which is not mounted at /{SCRATCH_MOUNT}: \
                                               restart the thermostat, or set it up again over USB"),
            ApplyError::NotAllowed(why) | ApplyError::Invalid(why) | ApplyError::Mismatch(why) => write!(f, "{why}"),
            ApplyError::Changed(path) => write!(f, "{path} changed since it was read: start again"),
            ApplyError::TooLarge => write!(f, "larger than {} MiB", MAX_SIZE >> 20),
            ApplyError::NoSpace => write!(f, "not enough room to keep a rollback copy: use USB"),
            ApplyError::NotRunnable(why) => write!(f, "the binary does not run: {why}"),
            ApplyError::Options(why) => write!(f, "the options do not parse: {why}"),
            ApplyError::Script(path, why) => write!(f, "{path} does not parse: {why}"),
            ApplyError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for ApplyError {
    fn from(e: std::io::Error) -> ApplyError {
        ApplyError::Io(e)
    }
}

/// Whether the filesystem mounted at `at` ("/", "/media/scratch") is
/// read-only, from its last line in mounts (/proc/mounts' text).
fn read_only(mounts: &str, at: &str) -> bool {
    files::mount_options(mounts, at).is_some_and(|options| options.split(',').any(|o| o == "ro"))
}

/// Bytes free on dir's filesystem, less what it reserves.
pub fn free_space(dir: &Path) -> std::io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).map_err(std::io::Error::other)?;
    // SAFETY: statvfs is plain data, for which zeroes are a valid value.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: path is NUL-terminated and stat outlives the call.
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Not u64 on every target: f_frsize is 32 bits on the unit's ARMv7.
    #[allow(clippy::useless_conversion)]
    let (blocks, size) = (u64::from(stat.f_bavail), u64::from(stat.f_frsize));
    Ok(blocks * size)
}

fn parent(path: &Path) -> &Path {
    path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."))
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Everything written so far reaches the flash.
fn sync() {
    // SAFETY: sync takes nothing and cannot fail.
    unsafe { libc::sync() };
}

/// Removes path, if it is there.
fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Writes path through a new file renamed over it, durably: a power cut
/// leaves the old content or the new, never part of it.
fn replace(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let new = path.with_extension("new");
    let mut file = File::create(&new)?;
    file.write_all(content)?;
    file.sync_all()?;
    std::fs::rename(&new, path)?;
    sync_dir(parent(path))
}

/// Writes the pending marker, durably.
fn mark(pending: &Path, content: &[u8]) -> std::io::Result<()> {
    let mut marker = File::create(pending)?;
    marker.write_all(content)?;
    marker.sync_all()?;
    sync_dir(parent(pending))
}

fn staged(path: &str) -> String {
    format!("{path}{STAGED}")
}

fn backup(path: &str) -> String {
    format!("{path}{BACKUP}")
}

/// "uid:gid" as numbers.
fn owner(owner: &str) -> Option<(u32, u32)> {
    let (uid, gid) = owner.split_once(':')?;
    Some((uid.parse().ok()?, gid.parse().ok()?))
}

/// Gives path the owner who ("uid:gid") and mode.
fn set_owner(path: &Path, who: &str, mode: u32) -> std::io::Result<()> {
    let (uid, gid) = owner(who).ok_or_else(|| std::io::Error::other(format!("{who} is not uid:gid")))?;
    std::os::unix::fs::chown(path, Some(uid), Some(gid))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777))
}

/// "uid:gid mode-octal", as a read reports a directory, from numbers.
fn owner_mode(who: &str, mode: u32) -> Option<String> {
    let (uid, gid) = owner(who)?;
    Some(format!("{uid}:{gid} {:o}", mode & 0o7777))
}


/// The order a set's writes are renamed in, as indices into entries:
/// etc/unfr0zen's originals first, since other files are patched from
/// them, and nlclient last.
pub fn swap_order(entries: &[Entry]) -> Vec<usize> {
    let rank = |entry: &Entry| match entry.path.as_str() {
        path if path.starts_with(ORIGINALS) => 0,
        NLCLIENT => 2,
        _ => 1,
    };
    let mut order: Vec<usize> = (0..entries.len()).filter(|&i| entries[i].kind == Kind::Write).collect();
    order.sort_by_key(|&i| rank(&entries[i]));
    order
}

/// One line of the journal.
#[derive(Debug, Clone, PartialEq)]
enum Line {
    Mkdir(String),
    /// A directory there before the set, whose owner and mode the set
    /// changes, and what they were.
    Owner(String, String, u32),
    /// A file the set writes, and its backup; None when it was absent.
    Write(String, Option<String>),
    Remove(String, String),
}

/// The journal the guard parses: "unfr0zen-update 1", "class restart|reboot",
/// "state swapping|swapped", then "mkdir <path>", "owner <path> <uid:gid>
/// <mode-octal>", "write <path> <backup>|-" and "remove <path> <backup>"
/// lines; paths hold no spaces (allowed()).
fn journal_text(class: Class, state: &str, lines: &[Line]) -> String {
    let class = match class {
        Class::Restart => "restart",
        Class::Reboot => "reboot",
    };
    let mut text = format!("unfr0zen-update {JOURNAL_VERSION}\nclass {class}\nstate {state}\n");
    for line in lines {
        text += &match line {
            Line::Mkdir(path) => format!("mkdir {path}\n"),
            Line::Owner(path, owner, mode) => format!("owner {path} {owner} {mode:o}\n"),
            Line::Write(path, backup) => format!("write {path} {}\n", backup.as_deref().unwrap_or("-")),
            Line::Remove(path, backup) => format!("remove {path} {backup}\n"),
        };
    }
    text
}

/// The journal's entry lines; an error for a journal of another version.
fn read_journal(path: &Path) -> std::io::Result<Vec<Line>> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines();
    if lines.next() != Some(format!("unfr0zen-update {JOURNAL_VERSION}").as_str()) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "not a journal this nestlocal knows"));
    }
    Ok(lines.filter_map(|line| {
        let words: Vec<&str> = line.split(' ').collect();
        match words[..] {
            ["mkdir", path] => Some(Line::Mkdir(path.into())),
            ["owner", path, owner, mode] => Some(Line::Owner(path.into(), owner.into(), u32::from_str_radix(mode, 8).ok()?)),
            ["write", path, "-"] => Some(Line::Write(path.into(), None)),
            ["write", path, backup] => Some(Line::Write(path.into(), Some(backup.into()))),
            ["remove", path, backup] => Some(Line::Remove(path.into(), backup.into())),
            _ => None,
        }
    }).collect())
}

/// The class of the set the journal holds: restart without one, or with
/// one this nestlocal can't read, which keep() refuses anyway.
pub fn journal_class(paths: &Paths) -> Class {
    match std::fs::read_to_string(&paths.journal) {
        Ok(text) if text.lines().next() == Some(format!("unfr0zen-update {JOURNAL_VERSION}").as_str())
            && text.lines().any(|line| line == "class reboot") => Class::Reboot,
        _ => Class::Restart,
    }
}

/// Puts back what lines swapped in, last first, then forgets the set. A
/// write's backup is still a second link to the file it replaced when the
/// swap stopped before renaming it in; that backup is dropped instead.
fn roll_back(paths: &Paths, lines: &[Line]) -> std::io::Result<()> {
    let root = &paths.root;
    for line in lines.iter().rev() {
        match line {
            Line::Mkdir(path) => {
                let _ = std::fs::remove_dir(root.join(path));
            }
            Line::Owner(path, owner, mode) => {
                let path = root.join(path);
                if path.is_dir() {
                    set_owner(&path, owner, *mode)?;
                }
            }
            Line::Write(path, None) => remove(&root.join(path))?,
            Line::Write(path, Some(backup)) | Line::Remove(path, backup) => {
                let (path, backup) = (root.join(path), root.join(backup));
                let Ok(kept) = std::fs::symlink_metadata(&backup) else { continue };
                match std::fs::symlink_metadata(&path) {
                    Ok(meta) if (meta.dev(), meta.ino()) == (kept.dev(), kept.ino()) => remove(&backup)?,
                    _ => std::fs::rename(&backup, &path)?,
                }
            }
        }
    }
    let _ = remove(&paths.pending);
    remove(&paths.journal)?;
    sync();
    Ok(())
}

/// Removes what the staging list names, as the guard does; directories
/// only when no journal claims them.
fn clear_staging(paths: &Paths) -> std::io::Result<()> {
    let list = match std::fs::read_to_string(&paths.staging) {
        Ok(list) => list,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for line in list.lines() {
        if let Some(file) = line.strip_prefix("new ").filter(|file| file.ends_with(STAGED)) {
            remove(&paths.root.join(file))?;
        }
    }
    if !paths.journal.exists() {
        for dir in list.lines().rev().filter_map(|line| line.strip_prefix("mkdir ")) {
            let _ = std::fs::remove_dir(paths.root.join(dir));
        }
    }
    remove(&paths.staging)
}

/// Why the unit refuses a set that changes a path, before anything of the
/// set is read: what an apply checks of each path (begin), and what a
/// check (/update/check) answers.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The filesystem the path is on, "/" or "/media/scratch", is mounted
    /// read-only.
    ReadOnly(String),
    /// The path is in the scratch directory, and no volume is mounted at
    /// media/scratch.
    NoScratch,
    /// allowed() refuses it, and why.
    NotAllowed(String),
    /// It is reached through a symlink or another filesystem.
    Reached,
}

impl Refusal {
    /// The error an apply of a set changing name is refused with.
    fn error(&self, name: &str) -> ApplyError {
        match self {
            Refusal::ReadOnly(_) => ApplyError::ReadOnly,
            Refusal::NoScratch => ApplyError::NoScratch,
            Refusal::NotAllowed(why) => ApplyError::NotAllowed(why.clone()),
            Refusal::Reached => ApplyError::NotAllowed(format!("{name} {}", self.why(name))),
        }
    }

    /// Why a check refuses name, without the name.
    pub fn why(&self, name: &str) -> String {
        match self {
            Refusal::ReadOnly(at) => format!("{at} is mounted read-only"),
            Refusal::NoScratch => format!("no scratch volume is mounted at /{SCRATCH_MOUNT}"),
            Refusal::NotAllowed(why) => {
                let quoted = format!("{name:?}");
                why.strip_prefix(&quoted).or_else(|| why.strip_prefix(name)).map_or(why.as_str(), str::trim_start).to_string()
            }
            Refusal::Reached => "is reached through a symlink or another filesystem".to_string(),
        }
    }

    pub fn rule(&self) -> Rule {
        match self {
            Refusal::ReadOnly(_) => Rule::ReadOnly,
            Refusal::NoScratch => Rule::NoScratch,
            Refusal::NotAllowed(_) => Rule::NotAllowed,
            Refusal::Reached => Rule::Reached,
        }
    }
}

/// Whether the unit refuses a set changing each of names, and why
/// (Refusal), in names' order: its filesystem mounted read-only, the
/// scratch volume not mounted for a path in it, allowed(), and a symlink or
/// another filesystem on the way.
pub fn refusals(paths: &Paths, names: &[&str]) -> std::io::Result<Vec<Option<Refusal>>> {
    let mounts = std::fs::read_to_string(&paths.mounts)?;
    let scratch_at = format!("/{SCRATCH_MOUNT}");
    let (root_read_only, scratch_read_only) = (read_only(&mounts, "/"), read_only(&mounts, &scratch_at));
    // Unmounted, media/scratch is the root volume's mount point.
    let scratch_mounted = files::mount_options(&mounts, &scratch_at).is_some();
    let devices = files::Devices::of(&paths.root, scratch_mounted)?;
    Ok(names.iter().map(|&name| {
        let in_scratch = in_scratch(name);
        if root_read_only {
            // The staging list and the journal are on the root volume.
            Some(Refusal::ReadOnly("/".into()))
        } else if in_scratch && scratch_read_only {
            Some(Refusal::ReadOnly(scratch_at.clone()))
        } else if in_scratch && !scratch_mounted {
            Some(Refusal::NoScratch)
        } else if let Err(why) = allowed(name) {
            Some(Refusal::NotAllowed(why))
        } else if devices.reach(&paths.root, name).is_none() {
            Some(Refusal::Reached)
        } else {
            None
        }
    }).collect())
}

/// The unit's answer to a check: what an apply of a set changing request's
/// paths would refuse of them before reading the set's contents. It writes
/// nothing.
pub fn check(paths: &Paths, request: &CheckRequest) -> std::io::Result<Checked> {
    let names: Vec<&str> = request.paths.iter().map(String::as_str).collect();
    let refused = refusals(paths, &names)?.into_iter().zip(&names)
        .filter_map(|(refused, name)| {
            let refused = refused?;
            Some(Refused { path: name.to_string(), why: refused.why(name), rule: refused.rule() })
        })
        .collect();
    Ok(Checked { refused })
}

/// Starts applying bundle, unless it is refused (ApplyError, in the order
/// checked). The staging list is written before anything is staged.
pub fn begin(paths: &Paths, bundle: Bundle) -> Result<Staging, ApplyError> {
    if pending(paths) {
        return Err(ApplyError::OnTrial);
    }
    let names: Vec<&str> = bundle.entries.iter().map(|entry| entry.path.as_str()).collect();
    let refusals = refusals(paths, &names)?;
    // A read-only filesystem first, then an unmounted scratch volume, then
    // the first path refused for itself.
    let first = refusals.iter().zip(&names).filter_map(|(refused, name)| Some((refused.as_ref()?, *name)))
        .min_by_key(|(refused, _)| match refused {
            Refusal::ReadOnly(_) => 0,
            Refusal::NoScratch => 1,
            Refusal::NotAllowed(_) | Refusal::Reached => 2,
        });
    if let Some((refused, name)) = first {
        return Err(refused.error(name));
    }
    check_valid(&bundle)?;
    let names = bundle.entries.iter().map(|entry| entry.path.clone()).collect();
    let found = files::read(&paths.root, &paths.mounts, &ReadRequest { names, contents: false });
    let mut directories = Vec::new();
    let mut reowned = Vec::new();
    for (entry, found) in bundle.entries.iter().zip(&found) {
        let unchanged = match (entry.kind, &entry.old, found) {
            (Kind::Directory, _, Found::Absent) => {
                directories.push(entry.path.clone());
                true
            }
            (Kind::Directory, _, Found::Directory { owner_mode: was }) => {
                if owner_mode(&entry.owner, entry.mode).as_ref() != Some(was) {
                    let old = was.split_once(' ').and_then(|(owner, mode)| Some((owner, u32::from_str_radix(mode, 8).ok()?)));
                    let (owner, mode) = old.ok_or_else(|| std::io::Error::other(format!("{}: {was}", entry.path)))?;
                    reowned.push(Line::Owner(entry.path.clone(), owner.into(), mode));
                }
                true
            }
            (_, None, Found::Absent) => true,
            (Kind::Write | Kind::Remove, Some(old), Found::File { sha256, .. }) => old.eq_ignore_ascii_case(sha256),
            _ => false,
        };
        if !unchanged {
            return Err(ApplyError::Changed(entry.path.clone()));
        }
    }
    // Made parent first: a path sorts after its parent's.
    directories.sort();
    for entry in bundle.entries.iter().filter(|entry| entry.kind != Kind::Remove) {
        let Some((dir, _)) = entry.path.rsplit_once('/') else { continue };
        if !directories.iter().any(|made| made == dir) && !paths.root.join(dir).is_dir() {
            return Err(ApplyError::Invalid(format!("{}: {dir} is not on the unit, nor in the set", entry.path)));
        }
    }
    let size_in = |scratch: bool| -> u64 {
        bundle.entries.iter().filter(|entry| in_scratch(&entry.path) == scratch).map(|entry| entry.size).sum()
    };
    let (root_size, scratch_size) = (size_in(false), size_in(true));
    let size = root_size + scratch_size;
    if size > MAX_SIZE {
        return Err(ApplyError::TooLarge);
    }
    // No set is on trial: whatever an earlier apply left is no one's, and
    // would count against this one's room.
    clear_staging(paths)?;
    for entry in &bundle.entries {
        remove(&paths.root.join(staged(&entry.path)))?;
        remove(&paths.root.join(backup(&entry.path)))?;
    }
    // Each filesystem keeps the backups of its own files.
    if (paths.free_space)(&paths.root)? < root_size + MARGIN
        || (scratch_size > 0 && (paths.free_space)(&paths.root.join(SCRATCH_MOUNT))? < scratch_size + MARGIN) {
        return Err(ApplyError::NoSpace);
    }
    std::fs::create_dir_all(parent(&paths.staging))?;
    let mut list = String::new();
    for dir in &directories {
        list += &format!("mkdir {dir}\n");
    }
    let writes: Vec<usize> = (0..bundle.entries.len()).filter(|&i| bundle.entries[i].kind == Kind::Write).collect();
    for &i in &writes {
        list += &format!("new {}\n", staged(&bundle.entries[i].path));
    }
    replace(&paths.staging, list.as_bytes())?;
    let class = bundle.class.max(class(&bundle.entries));
    let mut staging = Staging { paths: paths.clone(), entries: bundle.entries, class, writes, next: 0, file: None,
                                created: Vec::new(), reowned, finished: false };
    for dir in directories {
        let path = paths.root.join(&dir);
        std::fs::create_dir(&path)?;
        let entry = staging.entries.iter().find(|entry| entry.path == dir).expect("a directory of the set");
        staging.created.push(dir);
        set_owner(&path, &entry.owner, entry.mode)?;
    }
    Ok(staging)
}

/// What a set must say of itself for it to be applied at all.
fn check_valid(bundle: &Bundle) -> Result<(), ApplyError> {
    if bundle.version != BUNDLE_VERSION {
        return Err(ApplyError::Invalid(format!("update set version {} is not {BUNDLE_VERSION}", bundle.version)));
    }
    let mut seen = std::collections::HashSet::new();
    for entry in &bundle.entries {
        if !seen.insert(entry.path.as_str()) {
            return Err(ApplyError::Invalid(format!("{} is in the set twice", entry.path)));
        }
        let fits = match entry.kind {
            Kind::Write => entry.new.is_some() && owner(&entry.owner).is_some(),
            Kind::Directory => entry.size == 0 && owner(&entry.owner).is_some(),
            Kind::Remove => entry.size == 0,
        };
        if !fits {
            return Err(ApplyError::Invalid(format!("{} is not a well-formed {:?}", entry.path, entry.kind)));
        }
    }
    Ok(())
}

/// A set's file being written to its staged name.
struct Open {
    file: File,
    digest: ring::digest::Context,
    written: u64,
}

/// A set being staged: its writes' contents arrive in entry order.
pub struct Staging {
    paths: Paths,
    entries: Vec<Entry>,
    class: Class,
    /// The writes, as indices into entries.
    writes: Vec<usize>,
    /// The write being staged, as an index into writes.
    next: usize,
    file: Option<Open>,
    /// Directories begin made, in the order it made them.
    created: Vec<String>,
    /// Directories there before the set whose owner or mode it changes,
    /// with what they were; changed once the journal holds them.
    reowned: Vec<Line>,
    finished: bool,
}

impl Staging {
    pub fn write(&mut self, mut data: &[u8]) -> Result<(), ApplyError> {
        while !data.is_empty() {
            if self.file.is_none() {
                if self.next == self.writes.len() {
                    return Err(ApplyError::Mismatch("more contents than the set's files hold".into()));
                }
                self.open()?;
                continue;
            }
            let size = self.entries[self.writes[self.next]].size;
            let open = self.file.as_mut().expect("a staged file");
            let n = (size - open.written).min(data.len() as u64) as usize;
            open.file.write_all(&data[..n])?;
            open.digest.update(&data[..n]);
            open.written += n as u64;
            data = &data[n..];
            self.close_if_full()?;
        }
        Ok(())
    }

    /// Starts the next write's staged file.
    fn open(&mut self) -> Result<(), ApplyError> {
        let entry = &self.entries[self.writes[self.next]];
        let file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
            .open(self.paths.root.join(staged(&entry.path)))?;
        self.file = Some(Open { file, digest: ring::digest::Context::new(&ring::digest::SHA256), written: 0 });
        self.close_if_full()
    }

    /// Finishes the staged file once it holds its size: its SHA-256
    /// checked, its owner and mode set, and fsynced.
    fn close_if_full(&mut self) -> Result<(), ApplyError> {
        let entry = &self.entries[self.writes[self.next]];
        if self.file.as_ref().is_none_or(|open| open.written < entry.size) {
            return Ok(());
        }
        let open = self.file.take().expect("a staged file");
        if Some(hex(open.digest.finish().as_ref())) != entry.new.as_ref().map(|new| new.to_ascii_lowercase()) {
            return Err(ApplyError::Mismatch(format!("{} does not match its SHA-256", entry.path)));
        }
        let (uid, gid) = owner(&entry.owner).expect("checked by begin");
        std::os::unix::fs::fchown(&open.file, Some(uid), Some(gid))?;
        open.file.set_permissions(std::fs::Permissions::from_mode(entry.mode & 0o7777))?;
        open.file.sync_all()?;
        self.next += 1;
        Ok(())
    }

    /// The staged path of a write of path in this set, if it has one.
    fn staged(&self, path: &str) -> Option<PathBuf> {
        self.writes.iter().any(|&i| self.entries[i].path == path).then(|| self.paths.root.join(staged(path)))
    }

    /// What can be known to work before anything is swapped in: a staged
    /// nestlocal runs; the options nestlocal.init will pass, word split as
    /// it does, parse with the nestlocal that will run; staged scripts parse.
    fn check(&self) -> Result<(), ApplyError> {
        let root = &self.paths.root;
        let binary = self.staged(SCRATCH_NESTLOCAL).or_else(|| self.staged(NESTLOCAL));
        if let Some(binary) = &binary {
            run(binary, &["-h"]).map_err(ApplyError::NotRunnable)?;
        }
        let options = self.staged(OPTIONS);
        if binary.is_some() || options.is_some() {
            let removed = self.entries.iter().any(|entry| entry.path == OPTIONS && entry.kind == Kind::Remove);
            let text = match removed {
                true => String::new(),
                false => std::fs::read_to_string(options.unwrap_or(root.join(OPTIONS))).unwrap_or_default(),
            };
            let args: Vec<&str> = std::iter::once("-check-options").chain(text.split_whitespace()).collect();
            run(&binary.unwrap_or(self.paths.binary()), &args).map_err(ApplyError::Options)?;
        }
        let scripts: Vec<&str> = self.writes.iter().map(|&i| self.entries[i].path.as_str())
            .filter(|path| path.starts_with(SCRIPTS)).collect();
        if scripts.is_empty() {
            return Ok(());
        }
        let known = root.join("etc/init.d/rcS");
        if let Err(why) = run(Path::new("/bin/sh"), &["-n", &known.to_string_lossy()]) {
            logf!("wifi-update: sh -n fails on {}: scripts go unchecked: {why}", known.display());
            return Ok(());
        }
        for script in scripts {
            run(Path::new("/bin/sh"), &["-n", &root.join(staged(script)).to_string_lossy()])
                .map_err(|why| ApplyError::Script(script.to_string(), why))?;
        }
        Ok(())
    }

    /// Checks the staged set and swaps it in. A failed swap is put back
    /// here, or else left journaled for the guard.
    pub fn finish(mut self) -> Result<(Applied, Class), ApplyError> {
        while self.next < self.writes.len() {
            if self.file.is_some() {
                return Err(ApplyError::Mismatch("the contents end before the set's files".into()));
            }
            self.open()?;
        }
        self.check()?;
        let mut lines: Vec<Line> = self.created.iter().cloned().map(Line::Mkdir).collect();
        lines.extend(self.reowned.iter().cloned());
        for i in swap_order(&self.entries) {
            let entry = &self.entries[i];
            lines.push(Line::Write(entry.path.clone(), entry.old.is_some().then(|| backup(&entry.path))));
        }
        for entry in self.entries.iter().filter(|entry| entry.kind == Kind::Remove && entry.old.is_some()) {
            lines.push(Line::Remove(entry.path.clone(), backup(&entry.path)));
        }
        replace(&self.paths.journal, journal_text(self.class, "swapping", &lines).as_bytes())?;
        if let Err(e) = self.swap(&lines) {
            match roll_back(&self.paths, &lines) {
                Ok(()) => logf!("wifi-update: the swap failed: {e}: put back"),
                Err(back) => logf!("wifi-update: the swap failed: {e}: putting it back failed too: {back}: the guard will"),
            }
            return Err(e.into());
        }
        self.finished = true;
        sync();
        if let Err(e) = remove(&self.paths.staging) {
            logf!("wifi-update: {}: {e}", self.paths.staging.display());
        }
        // Swapped in, the set restarts or reboots on trial whatever can be
        // read back of it: a file left out of the answer fails the app's
        // check, and the guard puts the set back should it not come up.
        Ok((self.applied(), self.class))
    }

    /// Gives each directory there before the set its owner and mode, links
    /// each backup, renames each staged file in and each removed one to its
    /// backup, marks the set on trial, and journals it swapped. The
    /// marker goes down before the journal says swapped, so that a set
    /// swapped in is never found without its trial.
    fn swap(&self, lines: &[Line]) -> std::io::Result<()> {
        let root = &self.paths.root;
        for line in lines {
            if let Line::Owner(path, ..) = line {
                let entry = self.entries.iter().find(|entry| entry.path == *path).expect("a directory of the set");
                set_owner(&root.join(path), &entry.owner, entry.mode)?;
            }
        }
        for line in lines {
            if let Line::Write(path, Some(backup)) = line {
                std::fs::hard_link(root.join(path), root.join(backup))?;
            }
        }
        for line in lines {
            match line {
                Line::Write(path, _) => std::fs::rename(root.join(staged(path)), root.join(path))?,
                Line::Remove(path, backup) => std::fs::rename(root.join(path), root.join(backup))?,
                Line::Mkdir(_) | Line::Owner(..) => {}
            }
        }
        sync();
        // The last set's rollback is not this one's.
        remove(&self.paths.rolled_back())?;
        remove(&self.paths.cancelled())?;
        mark(&self.paths.pending, b"0\n")?;
        replace(&self.paths.journal, journal_text(self.class, "swapped", lines).as_bytes())
    }

    /// Each write as swapped in: its owner, mode and SHA-256; and each
    /// directory's owner and mode. One that can't be read is left out.
    fn applied(&self) -> Applied {
        let owner_mode = |meta: &std::fs::Metadata| format!("{}:{} {:o}", meta.uid(), meta.gid(), meta.mode() & 0o7777);
        fn readable<T>(path: &str, read: std::io::Result<T>) -> Option<T> {
            read.map_err(|e| logf!("wifi-update: {path}: {e}: left out of the answer")).ok()
        }
        let files = self.writes.iter().filter_map(|&i| {
            let path = &self.entries[i].path;
            let file = self.paths.root.join(path);
            readable(path, (|| Ok((path.clone(), owner_mode(&std::fs::metadata(&file)?), sha256_file(&file)?)))())
        }).collect();
        let directories = self.entries.iter().filter(|entry| entry.kind == Kind::Directory).filter_map(|entry| {
            let read = std::fs::metadata(self.paths.root.join(&entry.path)).map(|meta| (entry.path.clone(), owner_mode(&meta)));
            readable(&entry.path, read)
        }).collect();
        Applied { files, directories }
    }
}

impl Drop for Staging {
    /// A set not swapped in leaves nothing it staged or made.
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        drop(self.file.take());
        for &i in &self.writes {
            let _ = remove(&self.paths.root.join(staged(&self.entries[i].path)));
        }
        for dir in self.created.iter().rev() {
            let _ = std::fs::remove_dir(self.paths.root.join(dir));
        }
        let _ = remove(&self.paths.staging);
    }
}

/// Runs binary with args, which must exit 0 within RUN_FOR; why not, from
/// the first line of what it printed to stderr, or how it exited.
fn run(binary: &Path, args: &[&str]) -> Result<(), String> {
    // Another thread's fork can briefly hold the just-written file open.
    let mut tries = 0;
    let mut child = loop {
        match Command::new(binary).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn() {
            Ok(child) => break child,
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < 20 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let started = Instant::now();
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None if started.elapsed() >= RUN_FOR => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{} still running after {}s", args[0], RUN_FOR.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        use std::io::Read;
        let _ = pipe.read_to_string(&mut stderr);
    }
    match stderr.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(line) => Err(line.to_string()),
        None => Err(format!("{} {status}", args[0])),
    }
}

/// The hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = File::open(path)?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(hex(digest.finish().as_ref()));
        }
        digest.update(&buf[..n]);
    }
}

/// Whether a set is on trial, or swapped in and not yet forgotten.
pub fn pending(paths: &Paths) -> bool {
    paths.pending.exists() || paths.journal.exists()
}

/// Keeps the set this process runs on trial: the marker goes, so the guard
/// no longer rolls it back, then the backups, then the journal. Not when the
/// marker reads 0: that set is swapped in and not yet started, and is its
/// own nestlocal's to keep. Nor when the journal no longer reads swapped:
/// the guard has begun putting the set back, and finishes it. Under the
/// guard's lock, so that the watcher either rolls the set back first or
/// finds it kept. Whether it kept a set.
pub fn keep(paths: &Paths) -> std::io::Result<bool> {
    let _lock = GuardLock::take(paths.guard_lock());
    match std::fs::read(&paths.pending) {
        Ok(marker) if marker.trim_ascii() == b"0" => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        _ => {}
    }
    match std::fs::read_to_string(&paths.journal) {
        Ok(text) if !text.lines().any(|line| line == "state swapped") => {
            return Err(std::io::Error::other("the update is being put back: left to the guard"));
        }
        _ => {}
    }
    let lines = match read_journal(&paths.journal) {
        Ok(lines) => lines,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e),
    };
    remove(&paths.pending)?;
    sync_dir(parent(&paths.pending))?;
    for line in &lines {
        if let Line::Write(_, Some(backup)) | Line::Remove(_, backup) = line {
            remove(&paths.root.join(backup))?;
        }
    }
    remove(&paths.journal)?;
    sync();
    Ok(true)
}

/// Has the guard roll back the set on trial at nestlocal's next start, as
/// by asked: the marker reads STARTS, which that start's count passes
/// (count_start), and Paths::cancelled records by. Under the guard's lock,
/// as keep is.
pub fn roll_back_now(paths: &Paths, by: &str) -> std::io::Result<()> {
    let _lock = GuardLock::take(paths.guard_lock());
    mark(&paths.cancelled(), format!("{by}\n").as_bytes())?;
    mark(&paths.pending, format!("{STARTS}\n").as_bytes()).inspect_err(|_| {
        let _ = remove(&paths.cancelled());
    })
}

/// Who asked for the last set to be rolled back, while it is recorded.
pub fn cancelled_by(paths: &Paths) -> Option<String> {
    std::fs::read_to_string(paths.cancelled()).ok().map(|by| by.trim().to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use update_wire::bundle::BUNDLE_VERSION;

    #[test]
    fn roll_back_now_marks_the_guard_s_last_start() {
        let guard = include_str!("../../rootfs/nestlocal-update-guard");
        assert!(guard.contains(&format!("\nSTARTS={STARTS}\n")), "the guard's STARTS");
    }

    const NEW: &[u8] = b"#!/bin/sh\nexit 0\n";
    const OLD: &[u8] = b"#!/bin/sh\necho old\n";
    const RW: &str = "rootfs / rootfs rw 0 0\nubi0:root0 / ubifs rw,relatime 0 0\n";

    fn plenty(_: &Path) -> std::io::Result<u64> {
        Ok(1 << 40)
    }

    /// Room for the set's files but not for the margin beside them.
    fn little(_: &Path) -> std::io::Result<u64> {
        Ok(MARGIN)
    }

    /// A unit's root with nestlocal OLD, its options and etc/monitrc.
    fn unit(name: &str, mounts: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("nestlocal-bundle-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("root");
        for sub in ["nestlabs/sbin", "etc/nestlocal", "data"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        let paths = Paths {
            journal: root.join("etc/nestlocal/update/journal"),
            staging: root.join("etc/nestlocal/update/staging"),
            pending: root.join("data/nestlocal-update.pending"),
            mounts: dir.join("mounts"),
            overlay: dir.join("overlay-version"),
            free_space: plenty,
            root,
        };
        std::fs::write(&paths.mounts, mounts).unwrap();
        put(&paths, NESTLOCAL, OLD, 0o755);
        put(&paths, OPTIONS, b"-mqtt tcp://b:1883\n", 0o644);
        put(&paths, "etc/monitrc", b"old monitrc\n", 0o600);
        paths
    }

    fn put(paths: &Paths, path: &str, body: &[u8], mode: u32) {
        let path = paths.root.join(path);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn get(paths: &Paths, path: &str) -> Option<Vec<u8>> {
        std::fs::read(paths.root.join(path)).ok()
    }

    fn sha(body: &[u8]) -> String {
        hex(ring::digest::digest(&ring::digest::SHA256, body).as_ref())
    }

    fn owner(paths: &Paths) -> String {
        let meta = std::fs::metadata(&paths.root).unwrap();
        format!("{}:{}", meta.uid(), meta.gid())
    }

    /// A write of body to path over what is there now.
    fn write(paths: &Paths, path: &str, body: &[u8], mode: u32) -> (Entry, Vec<u8>) {
        let entry = Entry {
            path: path.into(),
            kind: Kind::Write,
            owner: owner(paths),
            mode,
            old: get(paths, path).map(|old| sha(&old)),
            new: Some(sha(body)),
            size: body.len() as u64,
        };
        (entry, body.to_vec())
    }

    fn remove(paths: &Paths, path: &str) -> (Entry, Vec<u8>) {
        let old = get(paths, path).map(|old| sha(&old));
        (Entry { path: path.into(), kind: Kind::Remove, owner: owner(paths), mode: 0, old, new: None, size: 0 }, Vec::new())
    }

    fn directory(paths: &Paths, path: &str, mode: u32) -> (Entry, Vec<u8>) {
        (Entry { path: path.into(), kind: Kind::Directory, owner: owner(paths), mode, old: None, new: None, size: 0 },
         Vec::new())
    }

    fn mode(paths: &Paths, path: &str) -> u32 {
        std::fs::metadata(paths.root.join(path)).unwrap().mode() & 0o7777
    }

    fn set(entries: &[(Entry, Vec<u8>)]) -> Bundle {
        Bundle { version: BUNDLE_VERSION, class: Class::Restart, force: false,
                 entries: entries.iter().map(|(entry, _)| entry.clone()).collect() }
    }

    /// Applies entries, their contents sent in 5-byte chunks.
    fn apply(paths: &Paths, entries: &[(Entry, Vec<u8>)]) -> Result<(Applied, Class), ApplyError> {
        let mut staging = begin(paths, set(entries))?;
        let contents: Vec<u8> = entries.iter().flat_map(|(_, body)| body.clone()).collect();
        for chunk in contents.chunks(5) {
            staging.write(chunk)?;
        }
        staging.finish()
    }

    /// Every name under root that ends in suffix.
    fn named(paths: &Paths, suffix: &str) -> Vec<String> {
        fn walk(dir: &Path, suffix: &str, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.to_string_lossy().ends_with(suffix) {
                    out.push(path.display().to_string());
                }
                if entry.file_type().unwrap().is_dir() {
                    walk(&path, suffix, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&paths.root, suffix, &mut out);
        out
    }

    /// Nothing changed, staged or journaled.
    fn assert_untouched(paths: &Paths) {
        assert_eq!(get(paths, NESTLOCAL).unwrap(), OLD);
        assert_eq!(get(paths, OPTIONS).unwrap(), b"-mqtt tcp://b:1883\n");
        assert_eq!(get(paths, "etc/monitrc").unwrap(), b"old monitrc\n");
        assert_eq!(named(paths, STAGED), Vec::<String>::new());
        assert_eq!(named(paths, BACKUP), Vec::<String>::new());
        assert!(!paths.staging.exists() && !paths.journal.exists() && !paths.pending.exists());
        assert!(!pending(paths));
    }

    #[test]
    fn a_bundle_swaps_in_every_file_and_journals_it() {
        let paths = unit("swap", RW);
        let options = b"-mqtt tcp://c:1883\n";
        let entries = [
            write(&paths, NESTLOCAL, NEW, 0o755),
            write(&paths, OPTIONS, options, 0o600),
            remove(&paths, "etc/monitrc"),
            directory(&paths, "etc/extra", 0o755),
            write(&paths, "etc/extra/empty", b"", 0o640),
            write(&paths, "etc/extra/x", b"x\n", 0o644),
        ];
        // The guard put the last set back.
        std::fs::write(paths.rolled_back(), "").unwrap();
        let (applied, class) = apply(&paths, &entries).unwrap();
        assert!(!paths.rolled_back().exists(), "a new set is on trial: the last one's rollback is no longer reported");
        assert_eq!(class, Class::Reboot, "etc/monitrc is not nestlocal's own");
        assert_eq!(get(&paths, NESTLOCAL).unwrap(), NEW);
        assert_eq!(get(&paths, OPTIONS).unwrap(), options);
        assert_eq!(get(&paths, "etc/extra/x").unwrap(), b"x\n");
        assert_eq!(get(&paths, "etc/extra/empty").unwrap(), b"");
        assert!(get(&paths, "etc/monitrc").is_none());
        let mode = |path: &str| mode(&paths, path);
        assert_eq!((mode(NESTLOCAL), mode(OPTIONS), mode("etc/extra/empty")), (0o755, 0o600, 0o640));
        // What was replaced or removed is kept until the set is kept.
        assert_eq!(get(&paths, "nestlabs/sbin/nestlocal.unfr0zen-previous").unwrap(), OLD);
        assert_eq!(get(&paths, "etc/nestlocal/options.unfr0zen-previous").unwrap(), b"-mqtt tcp://b:1883\n");
        assert_eq!(get(&paths, "etc/monitrc.unfr0zen-previous").unwrap(), b"old monitrc\n");
        assert_eq!(std::fs::read_to_string(&paths.journal).unwrap(), "unfr0zen-update 1\nclass reboot\nstate swapped\n\
            mkdir etc/extra\n\
            write nestlabs/sbin/nestlocal nestlabs/sbin/nestlocal.unfr0zen-previous\n\
            write etc/nestlocal/options etc/nestlocal/options.unfr0zen-previous\n\
            write etc/extra/empty -\nwrite etc/extra/x -\n\
            remove etc/monitrc etc/monitrc.unfr0zen-previous\n");
        assert_eq!(std::fs::read_to_string(&paths.pending).unwrap(), "0\n");
        assert!(pending(&paths));
        assert!(!paths.staging.exists());
        assert_eq!(named(&paths, STAGED), Vec::<String>::new());
        let owner = owner(&paths);
        assert_eq!(applied.files, vec![
            (NESTLOCAL.to_string(), format!("{owner} 755"), sha(NEW)),
            (OPTIONS.to_string(), format!("{owner} 600"), sha(options)),
            ("etc/extra/empty".to_string(), format!("{owner} 640"), sha(b"")),
            ("etc/extra/x".to_string(), format!("{owner} 644"), sha(b"x\n")),
        ]);
        assert_eq!(applied.directories, vec![("etc/extra".to_string(), format!("{owner} 755"))]);
    }

    #[test]
    fn directories_get_the_sets_owner_and_mode_and_roll_back_to_theirs() {
        let paths = unit("directories", RW);
        // Made under the process umask, and one already there at another mode.
        std::fs::create_dir(paths.root.join("etc/held")).unwrap();
        for dir in ["etc/held", "etc/nestlocal"] {
            std::fs::set_permissions(paths.root.join(dir), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let entries = [
            directory(&paths, "root", 0o755),
            directory(&paths, "root/.ssh", 0o700),
            directory(&paths, "etc/held", 0o700),
            directory(&paths, "etc/nestlocal", 0o755),
            write(&paths, "root/.ssh/authorized_keys", b"ssh-ed25519 AAAA\n", 0o600),
        ];
        let mut bad = directory(&paths, "etc/bad", 0o700);
        bad.0.owner = "root".into();
        assert!(matches!(apply(&paths, &[bad]), Err(ApplyError::Invalid(_))));
        let (applied, _) = apply(&paths, &entries).unwrap();
        assert_eq!((mode(&paths, "root"), mode(&paths, "root/.ssh"), mode(&paths, "etc/held")), (0o755, 0o700, 0o700));
        let owner = owner(&paths);
        assert_eq!(applied.directories, vec![
            ("root".to_string(), format!("{owner} 755")),
            ("root/.ssh".to_string(), format!("{owner} 700")),
            ("etc/held".to_string(), format!("{owner} 700")),
            ("etc/nestlocal".to_string(), format!("{owner} 755")),
        ]);
        // Only a directory whose owner or mode changes is journaled.
        assert_eq!(std::fs::read_to_string(&paths.journal).unwrap(), format!("unfr0zen-update 1\nclass reboot\n\
            state swapped\nmkdir root\nmkdir root/.ssh\nowner etc/held {owner} 755\n\
            write root/.ssh/authorized_keys -\n"));
        roll_back(&paths, &read_journal(&paths.journal).unwrap()).unwrap();
        assert_eq!(mode(&paths, "etc/held"), 0o755);
        assert!(!paths.root.join("root").exists());
        assert_untouched(&paths);
    }

    #[test]
    fn a_set_names_a_file_whose_directory_is_missing() {
        let paths = unit("nodir", RW);
        let entries = [write(&paths, "etc/monit.d/nestlocal.monitrc", b"check process nestlocal\n", 0o644)];
        let refused = apply(&paths, &entries).unwrap_err();
        assert!(matches!(refused, ApplyError::Invalid(_)));
        assert_eq!(refused.to_string(), "etc/monit.d/nestlocal.monitrc: etc/monit.d is not on the unit, nor in the set");
        assert_untouched(&paths);
    }

    #[test]
    fn a_sets_directories_are_made_parent_first_in_any_order() {
        let paths = unit("dirorder", RW);
        let entries = [
            directory(&paths, "root/.ssh", 0o700),
            directory(&paths, "root", 0o755),
            write(&paths, "root/.ssh/authorized_keys", b"ssh-ed25519 AAAA\n", 0o600),
        ];
        apply(&paths, &entries).unwrap();
        assert_eq!((mode(&paths, "root"), mode(&paths, "root/.ssh")), (0o755, 0o700));
        roll_back(&paths, &read_journal(&paths.journal).unwrap()).unwrap();
        assert!(!paths.root.join("root").exists());
    }

    #[test]
    fn nestlocal_and_its_options_alone_only_restart() {
        let paths = unit("restart", RW);
        let entries = [write(&paths, NESTLOCAL, NEW, 0o755), write(&paths, OPTIONS, b"-zigbee\n", 0o644)];
        assert_eq!(apply(&paths, &entries).unwrap().1, Class::Restart);
        // The stricter of the two classes.
        let paths = unit("restartasked", RW);
        let entries = [write(&paths, NESTLOCAL, NEW, 0o755)];
        let mut staging = begin(&paths, Bundle { class: Class::Reboot, ..set(&entries) }).unwrap();
        staging.write(NEW).unwrap();
        assert_eq!(staging.finish().unwrap().1, Class::Reboot);
    }

    #[test]
    fn a_changed_file_refuses_the_whole_set() {
        let paths = unit("changed", RW);
        let mut options = write(&paths, OPTIONS, b"-zigbee\n", 0o644);
        options.0.old = Some(sha(b"what was read before\n"));
        let error = apply(&paths, &[write(&paths, NESTLOCAL, NEW, 0o755), options]).err().unwrap();
        assert!(matches!(&error, ApplyError::Changed(path) if path == OPTIONS), "{error}");
        assert_eq!((error.status(), error.to_string()),
                   (409, format!("{OPTIONS} changed since it was read: start again")));
        assert_untouched(&paths);
        // A file read as absent that is there now.
        let mut created = write(&paths, "etc/monitrc", b"new\n", 0o644);
        created.0.old = None;
        assert!(matches!(apply(&paths, &[created]), Err(ApplyError::Changed(_))));
        // And one read as there that has gone.
        let mut gone = remove(&paths, "etc/monitrc");
        gone.0.path = "etc/gone".into();
        assert!(matches!(apply(&paths, &[gone]), Err(ApplyError::Changed(_))));
        assert_untouched(&paths);
    }

    /// Room on the root volume, and none for backups on the scratch volume.
    fn scratch_full(dir: &Path) -> std::io::Result<u64> {
        Ok(if dir.ends_with(SCRATCH_MOUNT) { MARGIN } else { 1 << 40 })
    }

    #[test]
    fn a_set_in_the_scratch_directory_applies_on_the_scratch_volume() {
        let paths = unit("scratch", &format!("{RW}ubi0:scratch /media/scratch ubifs rw,relatime 0 0\n"));
        assert_eq!((paths.installed(), paths.binary()), (NESTLOCAL, paths.root.join(NESTLOCAL)));
        std::fs::create_dir_all(paths.root.join("media/scratch/.unfr0zen/bin")).unwrap();
        put(&paths, SCRATCH_NESTLOCAL, OLD, 0o755);
        // nestlocal.init runs the scratch volume's first.
        assert_eq!((paths.installed(), paths.binary()), (SCRATCH_NESTLOCAL, paths.root.join(SCRATCH_NESTLOCAL)));
        let entries = [write(&paths, SCRATCH_NESTLOCAL, NEW, 0o755), write(&paths, OPTIONS, b"-zigbee\n", 0o644)];
        // A read-only scratch volume refuses it; the root's own files do not mind.
        let read_only = Paths { mounts: paths.mounts.with_extension("ro"), ..paths.clone() };
        std::fs::write(&read_only.mounts, format!("{RW}ubi0:scratch /media/scratch ubifs ro,relatime 0 0\n")).unwrap();
        assert!(matches!(apply(&read_only, &entries), Err(ApplyError::ReadOnly)));
        // So does one not mounted, whose mount point is the root volume's.
        let unmounted = Paths { mounts: paths.mounts.with_extension("unmounted"), ..paths.clone() };
        std::fs::write(&unmounted.mounts, RW).unwrap();
        let error = apply(&unmounted, &entries).err().unwrap();
        assert!(matches!(error, ApplyError::NoScratch) && error.status() == 409, "{error}");
        assert!(error.to_string().contains("not mounted at /media/scratch"), "{error}");
        // And a scratch volume without room for its backups.
        let full = Paths { free_space: scratch_full, ..paths.clone() };
        assert!(matches!(apply(&full, &entries), Err(ApplyError::NoSpace)));
        assert_untouched(&paths);
        let (applied, class) = apply(&paths, &entries).unwrap();
        assert_eq!(class, Class::Restart);
        assert!(applied.files.iter().any(|(path, _, applied)| path == SCRATCH_NESTLOCAL && *applied == sha(NEW)));
        assert_eq!(get(&paths, SCRATCH_NESTLOCAL).unwrap(), NEW);
        assert_eq!(get(&paths, NESTLOCAL).unwrap(), OLD, "the root volume's copy is not the one installed");
        assert_eq!(get(&paths, "media/scratch/.unfr0zen/bin/nestlocal.unfr0zen-previous").unwrap(), OLD);
    }

    #[test]
    fn too_little_room_for_backups_refuses_and_leaves_nothing_staged() {
        let paths = Paths { free_space: little, ..unit("room", RW) };
        let error = apply(&paths, &[write(&paths, NESTLOCAL, NEW, 0o755)]).err().unwrap();
        assert!(matches!(error, ApplyError::NoSpace), "{error}");
        assert_eq!(error.status(), 507);
        assert!(error.to_string().contains("use USB"), "{error}");
        assert_untouched(&paths);
    }

    #[test]
    fn a_bundle_during_a_trial_is_refused() {
        let paths = unit("trial", RW);
        apply(&paths, &[write(&paths, NESTLOCAL, NEW, 0o755)]).unwrap();
        let newer = b"#!/bin/sh\nexit 0 # newer\n";
        let error = apply(&paths, &[write(&paths, NESTLOCAL, newer, 0o755)]).err().unwrap();
        assert!(matches!(error, ApplyError::OnTrial), "{error}");
        assert_eq!((error.status(), error.to_string().as_str()), (409, "the previous update is still on trial"));
        // A journal alone is a set not yet forgotten.
        std::fs::remove_file(&paths.pending).unwrap();
        assert!(pending(&paths));
        assert!(matches!(apply(&paths, &[write(&paths, NESTLOCAL, newer, 0o755)]), Err(ApplyError::OnTrial)));
        assert_eq!(get(&paths, NESTLOCAL).unwrap(), NEW);
    }

    #[test]
    fn a_disallowed_path_is_refused() {
        let paths = unit("denied", RW);
        std::fs::create_dir_all(paths.root.join("boot")).unwrap();
        let error = apply(&paths, &[write(&paths, "boot/kernel_fdt.itb", NEW, 0o644)]).err().unwrap();
        assert!(matches!(error, ApplyError::NotAllowed(_)), "{error}");
        assert_eq!(error.status(), 403);
        // Nor through a symlink, even to somewhere allowed.
        std::os::unix::fs::symlink(paths.root.join("nestlabs"), paths.root.join("etc/link")).unwrap();
        let error = apply(&paths, &[write(&paths, "etc/link/sbin/x", NEW, 0o644)]).err().unwrap();
        assert!(matches!(error, ApplyError::NotAllowed(_)), "{error}");
        assert!(get(&paths, "nestlabs/sbin/x").is_none());
        assert_untouched(&paths);
    }

    #[test]
    fn a_check_refuses_what_an_apply_would_and_writes_nothing() {
        fn tree(dir: &Path, out: &mut Vec<(PathBuf, Option<Vec<u8>>)>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
            entries.sort();
            for path in entries {
                let meta = std::fs::symlink_metadata(&path).unwrap();
                out.push((path.clone(), meta.is_file().then(|| std::fs::read(&path).unwrap())));
                if meta.is_dir() {
                    tree(&path, out);
                }
            }
        }
        let snapshot = |paths: &Paths| {
            let mut out = Vec::new();
            tree(&paths.root, &mut out);
            out
        };
        let checked = |paths: &Paths, names: &[&str]| {
            check(paths, &CheckRequest { paths: names.iter().map(|name| name.to_string()).collect() }).unwrap().refused
        };
        let refused = |path: &str, why: &str, rule| Refused { path: path.into(), why: why.into(), rule };
        let paths = unit("check", &format!("{RW}ubi0:scratch /media/scratch ubifs rw,relatime 0 0\n"));
        std::fs::create_dir_all(paths.root.join("boot")).unwrap();
        std::fs::create_dir_all(paths.root.join("media/scratch/.unfr0zen/bin")).unwrap();
        std::os::unix::fs::symlink(paths.root.join("nestlabs"), paths.root.join("etc/link")).unwrap();
        let before = snapshot(&paths);

        // A set the unit takes: files, a new directory, the scratch volume.
        assert_eq!(checked(&paths, &[NESTLOCAL, OPTIONS, "etc/nestlocal/new", SCRATCH_NESTLOCAL]), []);
        // A denied path and one through a symlink, as the apply refuses them.
        let (denied, linked) = ("boot/kernel_fdt.itb", "etc/link/sbin/x");
        assert_eq!(checked(&paths, &[OPTIONS, denied, linked]),
                   [refused(denied, "is outside what an update may change", Rule::NotAllowed),
                    refused(linked, "is reached through a symlink or another filesystem", Rule::Reached)]);
        // A scratch volume not mounted, whose mount point is the root volume's.
        let unmounted = Paths { mounts: paths.mounts.with_extension("unmounted"), ..paths.clone() };
        std::fs::write(&unmounted.mounts, RW).unwrap();
        assert_eq!(checked(&unmounted, &[OPTIONS, SCRATCH_NESTLOCAL]),
                   [refused(SCRATCH_NESTLOCAL, "no scratch volume is mounted at /media/scratch", Rule::NoScratch)]);
        // A read-only root refuses every path.
        let read_only = Paths { mounts: paths.mounts.with_extension("ro"), ..paths.clone() };
        std::fs::write(&read_only.mounts, "ubi0:root0 / ubifs ro,relatime 0 0\n").unwrap();
        assert_eq!(checked(&read_only, &[OPTIONS]), [refused(OPTIONS, "/ is mounted read-only", Rule::ReadOnly)]);
        assert_eq!(snapshot(&paths), before, "a check writes nothing");
        assert_untouched(&paths);

        for name in [denied, linked] {
            let error = apply(&paths, &[write(&paths, name, NEW, 0o644)]).err().unwrap();
            assert!(matches!(error, ApplyError::NotAllowed(_)), "{name}: {error}");
        }
        let error = apply(&unmounted, &[write(&paths, SCRATCH_NESTLOCAL, NEW, 0o755)]).err().unwrap();
        assert!(matches!(error, ApplyError::NoScratch), "{error}");
        let error = apply(&read_only, &[write(&paths, OPTIONS, NEW, 0o644)]).err().unwrap();
        assert!(matches!(error, ApplyError::ReadOnly), "{error}");
        assert_untouched(&paths);
    }

    #[test]
    fn a_wrong_content_sha_is_refused_before_the_journal() {
        let paths = unit("sha", RW);
        let mut binary = write(&paths, NESTLOCAL, NEW, 0o755);
        binary.0.new = Some(sha(OLD));
        let error = apply(&paths, &[write(&paths, OPTIONS, b"-zigbee\n", 0o644), binary]).err().unwrap();
        assert!(matches!(error, ApplyError::Mismatch(_)), "{error}");
        assert_eq!(error.status(), 400);
        assert_untouched(&paths);
    }

    #[test]
    fn contents_that_are_short_or_long_leave_nothing_behind() {
        let paths = unit("short", RW);
        let entries = [write(&paths, NESTLOCAL, NEW, 0o755)];
        let mut staging = begin(&paths, set(&entries)).unwrap();
        staging.write(&NEW[..4]).unwrap();
        assert!(paths.staging.exists());
        assert!(paths.root.join("nestlabs/sbin/nestlocal.unfr0zen-new").exists());
        assert!(matches!(staging.finish(), Err(ApplyError::Mismatch(_))));
        assert_untouched(&paths);
        // Cut off: dropped part way.
        let mut staging = begin(&paths, set(&entries)).unwrap();
        staging.write(&NEW[..4]).unwrap();
        drop(staging);
        assert_untouched(&paths);
        let mut staging = begin(&paths, set(&entries)).unwrap();
        assert!(matches!(staging.write(&[NEW, b"more"].concat()), Err(ApplyError::Mismatch(_))));
        drop(staging);
        assert_untouched(&paths);
    }

    #[test]
    fn a_binary_that_does_not_run_is_refused() {
        let paths = unit("fails", RW);
        let error = apply(&paths, &[write(&paths, NESTLOCAL, b"#!/bin/sh\nexit 3\n", 0o755)]).err().unwrap();
        assert!(matches!(error, ApplyError::NotRunnable(_)), "{error}");
        assert_eq!(error.status(), 422);
        assert_untouched(&paths);
    }

    #[test]
    fn options_a_binary_rejects_refuse_the_set() {
        let paths = unit("options", RW);
        let strict = b"#!/bin/sh\nif [ \"$1\" = -check-options ] && [ \"$2\" != -zigbee ]; then\n\
                       echo \"flag provided but not defined: $2\" >&2\necho usage >&2\nexit 2\nfi\nexit 0\n";
        let entries = [write(&paths, NESTLOCAL, strict, 0o755), write(&paths, OPTIONS, b"-nope\n", 0o644)];
        let error = apply(&paths, &entries).err().unwrap();
        assert!(matches!(error, ApplyError::Options(_)), "{error}");
        assert_eq!(error.status(), 422);
        assert!(error.to_string().ends_with("flag provided but not defined: -nope"), "{error}");
        assert_untouched(&paths);
        // The installed binary checks options sent alone; the installed
        // options are checked by a binary sent alone.
        put(&paths, NESTLOCAL, strict, 0o755);
        assert!(matches!(apply(&paths, &[write(&paths, OPTIONS, b"-nope\n", 0o644)]), Err(ApplyError::Options(_))));
        apply(&paths, &[write(&paths, OPTIONS, b"-zigbee\n", 0o644)]).unwrap();
        let paths = unit("options2", RW);
        assert!(matches!(apply(&paths, &[write(&paths, NESTLOCAL, strict, 0o755)]), Err(ApplyError::Options(_))));
    }

    #[test]
    fn a_script_that_does_not_parse_refuses_the_set() {
        let paths = unit("script", RW);
        std::fs::create_dir_all(paths.root.join("etc/init.d")).unwrap();
        put(&paths, "etc/init.d/rcS", b"#!/bin/sh\nmount -a\n", 0o755);
        let error = apply(&paths, &[write(&paths, "etc/init.d/nestlocal", b"#!/bin/sh\nif then\n", 0o755)]).err().unwrap();
        assert!(matches!(&error, ApplyError::Script(path, _) if path == "etc/init.d/nestlocal"), "{error}");
        assert_eq!(error.status(), 422);
        assert!(get(&paths, "etc/init.d/nestlocal").is_none());
        apply(&paths, &[write(&paths, "etc/init.d/nestlocal", b"#!/bin/sh\nexit 0\n", 0o755)]).unwrap();
    }

    #[test]
    fn read_only_root_and_size_limits_are_refused() {
        let paths = unit("limits", "rootfs / rootfs rw 0 0\nubi0:root0 / ubifs ro,relatime 0 0\n");
        let error = apply(&paths, &[write(&paths, NESTLOCAL, NEW, 0o755)]).err().unwrap();
        assert!(matches!(error, ApplyError::ReadOnly), "{error}");
        assert_eq!(error.status(), 409);
        let paths = unit("limits", RW);
        let mut large = write(&paths, NESTLOCAL, NEW, 0o755);
        large.0.size = MAX_SIZE + 1;
        let error = begin(&paths, set(&[large])).err().unwrap();
        assert!(matches!(error, ApplyError::TooLarge), "{error}");
        assert_eq!(error.status(), 413);
        assert_untouched(&paths);
    }

    #[test]
    fn leftovers_are_cleared_before_a_set_is_staged() {
        let paths = unit("leftovers", RW);
        std::fs::create_dir_all(paths.staging.parent().unwrap()).unwrap();
        std::fs::create_dir_all(paths.root.join("etc/made")).unwrap();
        put(&paths, "etc/nestlocal/options.unfr0zen-new", b"killed upload", 0o644);
        put(&paths, "nestlabs/sbin/nestlocal.unfr0zen-previous", b"stale", 0o644);
        std::fs::write(&paths.staging, "mkdir etc/made\nnew etc/nestlocal/options.unfr0zen-new\n").unwrap();
        let entries = [write(&paths, NESTLOCAL, NEW, 0o755)];
        let mut staging = begin(&paths, set(&entries)).unwrap();
        assert!(!paths.root.join("etc/made").exists());
        assert!(get(&paths, "etc/nestlocal/options.unfr0zen-new").is_none());
        staging.write(NEW).unwrap();
        staging.finish().unwrap();
        assert_eq!(get(&paths, "nestlabs/sbin/nestlocal.unfr0zen-previous").unwrap(), OLD);
    }

    #[test]
    fn a_failed_swap_puts_every_file_back() {
        let paths = unit("swapfails", RW);
        let entries = [
            write(&paths, NESTLOCAL, NEW, 0o755),
            remove(&paths, "etc/monitrc"),
            directory(&paths, "etc/extra", 0o755),
            directory(&paths, "etc/nestlocal", 0o700),
            write(&paths, "etc/extra/x", b"x\n", 0o644),
        ];
        let was = mode(&paths, "etc/nestlocal");
        assert_ne!(was, 0o700);
        let mut staging = begin(&paths, set(&entries)).unwrap();
        staging.write(&[NEW, b"x\n"].concat()).unwrap();
        // A directory where the marker goes: the swap fails once every file is in.
        std::fs::create_dir(&paths.pending).unwrap();
        assert!(matches!(staging.finish(), Err(ApplyError::Io(_))));
        std::fs::remove_dir(&paths.pending).unwrap();
        assert!(!paths.root.join("etc/extra").exists());
        assert_eq!(mode(&paths, "etc/nestlocal"), was);
        assert_untouched(&paths);
    }

    #[test]
    fn keep_removes_marker_then_backups_then_journal() {
        let paths = unit("keep", RW);
        let entries = [write(&paths, NESTLOCAL, NEW, 0o755), remove(&paths, "etc/monitrc")];
        apply(&paths, &entries).unwrap();
        assert!(!keep(&paths).unwrap(), "a marker reading 0 is the next nestlocal's to keep");
        assert!(pending(&paths));
        std::fs::write(&paths.pending, "1\n").unwrap();
        // A backup that cannot go stops the keep after the marker, before
        // the journal: the guard finishes it.
        let backup = paths.root.join("etc/monitrc.unfr0zen-previous");
        std::fs::remove_file(&backup).unwrap();
        std::fs::create_dir_all(backup.join("busy")).unwrap();
        assert!(keep(&paths).is_err());
        assert!(!paths.pending.exists() && paths.journal.exists());
        assert!(!paths.root.join("nestlabs/sbin/nestlocal.unfr0zen-previous").exists());
        std::fs::remove_dir_all(&backup).unwrap();
        std::fs::write(&paths.pending, "1\n").unwrap();
        assert!(keep(&paths).unwrap());
        assert!(!pending(&paths) && !paths.journal.exists());
        assert_eq!(named(&paths, BACKUP), Vec::<String>::new());
        assert_eq!(get(&paths, NESTLOCAL).unwrap(), NEW);
        assert!(!keep(&paths).unwrap(), "nothing pending");
    }

    #[test]
    fn a_set_the_guard_is_putting_back_is_not_kept() {
        let paths = unit("keepswapping", RW);
        let entries = [write(&paths, NESTLOCAL, NEW, 0o755), write(&paths, OPTIONS, b"-mqtt tcp://c:1883\n", 0o644)];
        apply(&paths, &entries).unwrap();
        std::fs::write(&paths.pending, "1\n").unwrap();
        // The guard journals its rollback swapping, then put back the
        // options before it stopped.
        let journal = std::fs::read_to_string(&paths.journal).unwrap();
        std::fs::write(&paths.journal, journal.replace("state swapped\n", "state swapping\n")).unwrap();
        std::fs::rename(paths.root.join(backup(OPTIONS)), paths.root.join(OPTIONS)).unwrap();
        assert!(keep(&paths).is_err());
        assert!(paths.pending.exists() && paths.journal.exists());
        assert!(paths.root.join(backup(NESTLOCAL)).exists(), "the rest of the rollback is lost");
    }

    #[test]
    fn keeping_waits_for_the_guard_rolling_the_set_back() {
        let paths = unit("keeplock", RW);
        let entries = [write(&paths, NESTLOCAL, NEW, 0o755)];
        apply(&paths, &entries).unwrap();
        std::fs::write(&paths.pending, "1\n").unwrap();
        let lock = paths.guard_lock();
        std::fs::create_dir_all(&lock).unwrap();
        // The watcher holds the lock and journals its rollback swapping
        // while keep waits for it.
        let journal = paths.journal.clone();
        let watcher = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            let text = std::fs::read_to_string(&journal).unwrap();
            std::fs::write(&journal, text.replace("state swapped\n", "state swapping\n")).unwrap();
            std::fs::remove_dir(&lock).unwrap();
        });
        let started = Instant::now();
        assert!(keep(&paths).is_err(), "the set the watcher is putting back is kept");
        assert!(started.elapsed() >= Duration::from_secs(1), "keep did not wait for the lock");
        watcher.join().unwrap();
        assert!(paths.pending.exists() && paths.root.join(backup(NESTLOCAL)).exists());
        assert!(!paths.guard_lock().exists(), "keep left the lock behind");
    }

    #[test]
    fn nlclient_is_renamed_last() {
        let paths = unit("order", RW);
        let names = ["nestlabs/sbin/nlclient", "etc/monitrc", "etc/unfr0zen/client.config.original", NESTLOCAL,
                     "etc/unfr0zen/x"];
        let entries: Vec<Entry> = names.iter().map(|name| write(&paths, name, b"", 0o644).0).collect();
        let order: Vec<&str> = swap_order(&entries).into_iter().map(|i| entries[i].path.as_str()).collect();
        assert_eq!(order, ["etc/unfr0zen/client.config.original", "etc/unfr0zen/x", "etc/monitrc", NESTLOCAL,
                           "nestlabs/sbin/nlclient"]);
    }
}
