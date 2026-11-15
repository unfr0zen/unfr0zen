//! A unit's running root filesystem as set_up_on_device's Volume, over an
//! update session: changes go as one set at the read-only remount, and the
//! readback checks after it read what the unit reports it swapped in.

use std::collections::BTreeMap;

use unfr0zen_core::setup::Tree;
use unfr0zen_core::shell::{sha256_hex, Digests, File, Volume};
use unfr0zen_core::Result;
use update_wire::bundle::{self, Applied, Bundle, Class, Entry, Kind, BUNDLE_VERSION};
use update_wire::read::{Found, Info, ReadRequest};

use crate::flow::failure;
use crate::session::{Sent, Session};

/// A change the patch made to a file.
enum Recorded {
    Write { data: Vec<u8>, owner: String, mode: u32 },
    Remove,
}

pub struct RemoteVolume<'a> {
    session: &'a Session,
    info: Info,
    /// The set may install an older nestlocal.
    force: bool,
    progress: &'a dyn Fn(&str),
    /// What each read found, with contents when they were asked for.
    found: BTreeMap<String, Found>,
    /// Each file's last change, in the order the patch first made one.
    changes: Vec<(String, Recorded)>,
    directories: Vec<(String, u32)>,
    /// What the unit swapped in, once the set is sent.
    applied: Option<Applied>,
    sent: Option<Class>,
}

impl<'a> RemoteVolume<'a> {
    pub fn new(session: &'a Session, info: Info, force: bool, progress: &'a dyn Fn(&str)) -> RemoteVolume<'a> {
        RemoteVolume { session, info, force, progress, found: BTreeMap::new(), changes: Vec::new(), directories: Vec::new(),
                       applied: None, sent: None }
    }

    /// Reads names in one request, with their contents or only their
    /// digests; what was read already is not asked for again. A path an
    /// update may not touch reads as absent without asking.
    pub fn fetch(&mut self, names: &[&str], contents: bool) -> Result<()> {
        let mut wanted: Vec<String> = Vec::new();
        for &name in names {
            if bundle::allowed(name).is_err() {
                self.found.insert(name.into(), Found::Absent);
                continue;
            }
            let have = match self.found.get(name) {
                None => false,
                Some(Found::File { data: None, .. }) => !contents,
                Some(_) => true,
            };
            if !have && !wanted.iter().any(|w| w == name) {
                wanted.push(name.into());
            }
        }
        if wanted.is_empty() {
            return Ok(());
        }
        let found = self.session.read(&ReadRequest { names: wanted.clone(), contents })?;
        if found.len() != wanted.len() {
            return Err(failure(format!("the unit answered a read of {} files with {}", wanted.len(), found.len())));
        }
        for (name, found) in wanted.into_iter().zip(found) {
            if let Found::File { sha256, data: Some(data), .. } = &found {
                if sha256_hex(data) != *sha256 {
                    return Err(failure(format!("the unit's read of {name} does not match its own digest")));
                }
            }
            self.found.insert(name, found);
        }
        Ok(())
    }

    /// The files read with their contents, as unit_settings takes them.
    pub fn tree(&self) -> Tree {
        self.found.iter().filter_map(|(name, found)| match found {
            Found::File { mode, data: Some(data), .. } => Some((name.clone(), (data.clone(), *mode))),
            _ => None,
        }).collect()
    }

    /// The class of the set sent; None until one is.
    pub fn sent(&self) -> Option<Class> {
        self.sent
    }

    fn recorded(&self, name: &str) -> Option<&Recorded> {
        self.changes.iter().find(|(n, _)| n == name).map(|(_, recorded)| recorded)
    }

    fn record(&mut self, name: &str, recorded: Recorded) -> Result<()> {
        bundle::allowed(name).map_err(failure)?;
        match self.changes.iter_mut().find(|(n, _)| n == name) {
            Some((_, slot)) => *slot = recorded,
            None => self.changes.push((name.into(), recorded)),
        }
        Ok(())
    }

    /// Owner, mode and SHA-256 of a file as read so far.
    fn digest_of(&self, name: &str) -> Option<File<String>> {
        match self.found.get(name) {
            Some(Found::File { owner, mode, sha256, .. }) => Some((owner.clone(), *mode, sha256.clone())),
            _ => None,
        }
    }

    /// Owner, mode and SHA-256 of a file as read, without its contents.
    fn read_digest(&mut self, name: &str) -> Result<Option<File<String>>> {
        self.fetch(&[name], false)?;
        Ok(self.digest_of(name))
    }

    /// Sends the recorded changes as one set; the unit's answer stands for
    /// the volume from then on.
    fn send(&mut self) -> Result<()> {
        let names: Vec<String> = self.changes.iter().map(|(name, _)| name.clone()).collect();
        self.fetch(&names.iter().map(String::as_str).collect::<Vec<_>>(), false)?;
        let mut entries: Vec<Entry> = self.directories.iter().map(|(name, mode)| Entry {
            path: name.clone(), kind: Kind::Directory, owner: "0:0".into(), mode: *mode, old: None, new: None, size: 0,
        }).collect();
        let mut contents: Vec<&[u8]> = Vec::new();
        for (name, recorded) in &self.changes {
            let read = self.digest_of(name);
            entries.push(match recorded {
                Recorded::Write { data, owner, mode } => {
                    contents.push(data);
                    Entry { path: name.clone(), kind: Kind::Write, owner: owner.clone(), mode: *mode,
                            old: read.map(|(_, _, sha)| sha), new: Some(sha256_hex(data)), size: data.len() as u64 }
                }
                Recorded::Remove => {
                    let (owner, mode, old) = read.map_or(("0:0".into(), 0, None), |(owner, mode, sha)| (owner, mode, Some(sha)));
                    Entry { path: name.clone(), kind: Kind::Remove, owner, mode, old, new: None, size: 0 }
                }
            });
        }
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        crate::wifi_patch::check_set(self.session, || Ok(self.info.clone()), &paths)?;
        let class = bundle::class(&entries);
        let size: usize = contents.iter().map(|data| data.len()).sum();
        (self.progress)(&format!("Sending {} changes ({size} bytes); the unit then {}", entries.len(), match class {
            Class::Restart => "restarts nestlocal",
            Class::Reboot => "reboots",
        }));
        let set = Bundle { version: BUNDLE_VERSION, class, force: self.force, entries };
        let applied = match self.session.apply(&set, &contents) {
            Ok(applied) => applied,
            Err(Sent::Refused(error)) => return Err(error),
            // The unit finishes a set it has all of, client or not.
            Err(Sent::Lost(error)) => {
                return Err(failure(format!("sending the update failed ({error}): the unit may still have taken it; \
                                            Settings → UPDATE on the thermostat shows whether an update is on trial")));
            }
        };
        (self.applied, self.sent) = (Some(applied), Some(class));
        Ok(())
    }
}

/// "uid:gid mode-octal" as owner and mode.
fn owner_mode(text: &str) -> Option<(String, u32)> {
    let (owner, mode) = text.split_once(' ')?;
    Some((owner.to_string(), u32::from_str_radix(mode, 8).ok()?))
}

impl Volume for RemoteVolume<'_> {
    fn read(&mut self, name: &str) -> Result<Option<File<Vec<u8>>>> {
        match self.recorded(name) {
            Some(Recorded::Write { data, owner, mode }) => return Ok(Some((owner.clone(), *mode, data.clone()))),
            Some(Recorded::Remove) => return Ok(None),
            None => {}
        }
        self.fetch(&[name], true)?;
        Ok(match self.found.get(name) {
            Some(Found::File { owner, mode, data: Some(data), .. }) => Some((owner.clone(), *mode, data.clone())),
            _ => None,
        })
    }

    fn inspect(&mut self, name: &str) -> Result<Option<File<String>>> {
        let recorded = self.recorded(name);
        if let Some(applied) = &self.applied {
            if let Some((_, owner_mode_text, sha)) = applied.files.iter().find(|(path, _, _)| path == name) {
                let (owner, mode) = owner_mode(owner_mode_text)
                    .ok_or_else(|| failure(format!("the unit reported {name} as {owner_mode_text:?}")))?;
                return Ok(Some((owner, mode, sha.clone())));
            }
            if recorded.is_some() {
                // Sent, but not reported swapped in.
                return Ok(None);
            }
        }
        match recorded {
            Some(Recorded::Write { data, owner, mode }) => Ok(Some((owner.clone(), *mode, sha256_hex(data)))),
            Some(Recorded::Remove) => Ok(None),
            None => self.read_digest(name),
        }
    }

    fn file_size(&mut self, name: &str) -> Result<u64> {
        if let Some(Recorded::Write { data, .. }) = self.recorded(name) {
            return Ok(data.len() as u64);
        }
        self.fetch(&[name], false)?;
        match self.found.get(name) {
            Some(Found::File { size, .. }) if self.recorded(name).is_none() => Ok(*size),
            _ => Err(failure(format!("{name} disappeared during inspection"))),
        }
    }

    fn directory(&mut self, name: &str) -> Result<Option<String>> {
        if let Some((_, owner_mode)) = self.applied.as_ref().and_then(|applied| applied.directories.iter().find(|(path, _)| path == name)) {
            return Ok(Some(owner_mode.clone()));
        }
        if let Some((_, mode)) = self.directories.iter().find(|(path, _)| path == name) {
            return Ok(Some(format!("0:0 {mode:o}")));
        }
        self.fetch(&[name], false)?;
        Ok(match self.found.get(name) {
            Some(Found::Directory { owner_mode }) => Some(owner_mode.clone()),
            _ => None,
        })
    }

    fn free_bytes(&mut self) -> Result<u64> {
        Ok(self.info.free_bytes)
    }

    /// The scratch volume's own room, for its directory, which a unit that
    /// does not report it (or has none) reads as the root's.
    fn free_bytes_at(&mut self, name: &str) -> Result<u64> {
        Ok(match self.info.scratch_free_bytes {
            Some(free) if bundle::in_scratch(name) => free,
            _ => self.info.free_bytes,
        })
    }

    /// The unit keeps every file a set replaces until it is kept, so the
    /// space a write needs is checked, and the unit checks again.
    fn checks_space(&self) -> bool {
        true
    }

    fn shell_binary(&mut self) -> Result<String> {
        Ok(self.info.shell_binary.clone())
    }

    fn write(&mut self, name: &str, data: &[u8], owner: &str, mode: u32) -> Result<()> {
        self.record(name, Recorded::Write { data: data.to_vec(), owner: owner.into(), mode })
    }

    fn make_directory(&mut self, name: &str, mode: u32) -> Result<()> {
        bundle::allowed(name).map_err(failure)?;
        self.directories.retain(|(path, _)| path != name);
        self.directories.push((name.into(), mode));
        Ok(())
    }

    fn remove(&mut self, name: &str) -> Result<()> {
        self.record(name, Recorded::Remove)
    }

    /// The patch remounts read-write before its writes and read-only after
    /// them: the set goes then, once.
    fn remount(&mut self, writable: bool) -> Result<()> {
        if writable || self.sent.is_some() {
            return Ok(());
        }
        self.send()
    }

    fn digests(&mut self, names: &[String]) -> Result<Digests> {
        let applied = self.applied.as_ref().ok_or_else(|| failure("nothing was sent to the unit to read back"))?;
        Ok(names.iter().map(|name| {
            let found = applied.files.iter().find(|(path, _, _)| path == name);
            (name.clone(), (found.map(|(_, _, sha)| sha.clone()), found.map(|(_, owner_mode, _)| owner_mode.clone())))
        }).collect())
    }
}
