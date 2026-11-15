//! The laptops the unit trusts to update it without a code: an id of the
//! unit's own, and at most LIMIT laptops, each with the trust key a session
//! a window with trust keyed derives (window.rs grants it). Kept in FILE,
//! readable by root alone and replaced whole, so a write cut short leaves the
//! last one; each change publishes the laptops' ids and names to the screen
//! as LIST. A laptop trusted again takes its old entry's place, once its
//! exchange proves it holds the old trust key (proves, replace).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use update_wire::auth::{is_id, replace_message, verify};
use update_wire::{hex, random, unhex};

use crate::private_file::{partial, write_private};
use crate::{logf, settings_ui};

/// Where the unit keeps them: /media/data outlives updates and reboots.
pub const FILE: &str = "/media/data/nestlocal-trusted.json";
/// What the screen reads them from (settings_ui::dir()): a line per laptop,
/// "<laptop id> <name>".
pub const LIST: &str = "update.trusted";
/// The most laptops trusted at once.
pub const LIMIT: usize = 4;

/// A laptop the unit trusts.
#[derive(Clone)]
pub struct Laptop {
    pub id: String,
    pub name: String,
    pub key: [u8; 32],
    /// When it was trusted, in RFC 3339.
    pub added: String,
}

/// FILE as it is written.
#[derive(Serialize, Deserialize)]
struct Saved {
    unit: String,
    laptops: Vec<SavedLaptop>,
}

#[derive(Serialize, Deserialize)]
struct SavedLaptop {
    id: String,
    name: String,
    /// In hex.
    key: String,
    added: String,
}

pub struct Store {
    path: PathBuf,
    /// Where LIST is published.
    screen: PathBuf,
    unit: String,
    laptops: Vec<Laptop>,
}

/// An id of 16 random bytes, in lowercase hex.
fn new_id() -> String {
    hex(&random::<16>())
}

impl Store {
    /// The store in path, publishing its list to the screen. An unreadable
    /// or missing file is an empty store with a new unit id, written on its
    /// first change.
    pub fn open(path: &Path) -> Store {
        Store::publishing_to(path, settings_ui::dir())
    }

    fn publishing_to(path: &Path, screen: &Path) -> Store {
        let saved = match std::fs::read(path) {
            Ok(data) => serde_json::from_slice::<Saved>(&data).map_err(|e| logf!("wifi-update: {}: {e}: no laptop is trusted", path.display())).ok(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => {
                logf!("wifi-update: {}: {e}: no laptop is trusted", path.display());
                None
            }
        };
        let saved = saved.filter(|saved| is_id(&saved.unit));
        let (unit, laptops) = match saved {
            Some(saved) => {
                let laptops = saved.laptops.into_iter().filter_map(|laptop| {
                    let key = unhex(&laptop.key)?.try_into().ok()?;
                    is_id(&laptop.id).then_some(Laptop { id: laptop.id, name: laptop.name, key, added: laptop.added })
                }).take(LIMIT).collect();
                (saved.unit, laptops)
            }
            None => (new_id(), Vec::new()),
        };
        let store = Store { path: path.to_path_buf(), screen: screen.to_path_buf(), unit, laptops };
        store.publish();
        store
    }

    /// The unit's id, which a laptop knows its key for the unit by.
    pub fn unit_id(&self) -> &str {
        &self.unit
    }

    pub fn list(&self) -> &[Laptop] {
        &self.laptops
    }

    /// Whether it holds LIMIT laptops: no other can be trusted.
    pub fn full(&self) -> bool {
        self.laptops.len() >= LIMIT
    }

    /// Trusts laptop, unless the list is full.
    pub fn add(&mut self, laptop: Laptop) -> io::Result<()> {
        if self.full() {
            return Err(io::Error::other("the trusted list is full"));
        }
        self.laptops.push(laptop);
        if let Err(e) = self.save() {
            self.laptops.pop();
            return Err(e);
        }
        Ok(())
    }

    /// Trusts laptop in place of the laptop of id old, in one write, so a
    /// full list takes it; adds it as add does when old is not trusted.
    /// Whether it replaced old.
    pub fn replace(&mut self, old: &str, laptop: Laptop) -> io::Result<bool> {
        let Some(at) = self.laptops.iter().position(|trusted| trusted.id == old) else {
            return self.add(laptop).map(|()| false);
        };
        let replaced = std::mem::replace(&mut self.laptops[at], laptop);
        if let Err(e) = self.save() {
            self.laptops[at] = replaced;
            return Err(e);
        }
        Ok(true)
    }

    /// Whether proof, in hex, is the HMAC-SHA256 of replace_message for the
    /// client message message under the trust key of the laptop of id
    /// laptop, which the unit trusts: the exchange's client holds that key.
    pub fn proves(&self, laptop: &str, message: &[u8], proof: &str) -> bool {
        self.find(laptop).is_some_and(|trusted| verify(&trusted.key, &replace_message(&self.unit, laptop, message), proof))
    }

    /// Stops trusting the laptop of id; whether it was trusted.
    pub fn forget(&mut self, id: &str) -> io::Result<bool> {
        let Some(at) = self.laptops.iter().position(|laptop| laptop.id == id) else { return Ok(false) };
        let laptop = self.laptops.remove(at);
        if let Err(e) = self.save() {
            self.laptops.insert(at, laptop);
            return Err(e);
        }
        Ok(true)
    }

    /// The laptop of id, if the unit trusts it.
    pub fn find(&self, id: &str) -> Option<&Laptop> {
        self.laptops.iter().find(|laptop| laptop.id == id)
    }

    /// Writes the store, and publishes its list.
    fn save(&self) -> io::Result<()> {
        let saved = Saved {
            unit: self.unit.clone(),
            laptops: self.laptops.iter().map(|laptop| SavedLaptop {
                id: laptop.id.clone(), name: laptop.name.clone(), key: hex(&laptop.key), added: laptop.added.clone(),
            }).collect(),
        };
        write_private(&self.path, &serde_json::to_vec(&saved).map_err(io::Error::other)?)?;
        self.publish();
        Ok(())
    }

    /// Publishes the laptops' ids and names for the screen.
    fn publish(&self) {
        let list: String = self.laptops.iter().map(|laptop| format!("{} {}\n", laptop.id, laptop.name)).collect();
        if let Err(e) = write_private(&self.screen.join(LIST), list.as_bytes()) {
            logf!("wifi-update: {LIST}: {e}");
        }
    }

    /// An empty store of its own, in a directory of the test run's own that
    /// it also publishes to.
    #[cfg(test)]
    pub fn scratch() -> Store {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static STORES: AtomicUsize = AtomicUsize::new(0);
        let n = STORES.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("nestlocal-trusted-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Store::publishing_to(&dir.join("trusted.json"), &dir)
    }

    /// Makes its writes fail from now on: a directory stands where each
    /// write goes first.
    #[cfg(test)]
    pub fn fail_writes(&self) {
        std::fs::create_dir_all(partial(&self.path)).unwrap();
    }

    /// The list as the screen has it.
    #[cfg(test)]
    pub fn listed(&self) -> String {
        std::fs::read_to_string(self.screen.join(LIST)).unwrap_or_default()
    }
}

/// Forgets every laptop, as the unit does when it runs without the update
/// API: FILE and LIST, under root ("/" on the unit).
pub fn revoke(root: &Path) {
    let file = root.join(FILE.trim_start_matches('/'));
    let list = root.join(settings_ui::DIR.trim_start_matches('/')).join(LIST);
    if file.exists() {
        logf!("wifi-update: the update API is off: no laptop is trusted");
    }
    for path in [partial(&file), file, list] {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use update_wire::auth::replace_proof;

    fn laptop(n: u8) -> Laptop {
        Laptop { id: hex(&[n; 16]), name: format!("laptop {n}"), key: [n; 32], added: "2026-11-13T10:00:00Z".into() }
    }

    fn reopened(store: &Store) -> Store {
        Store::publishing_to(&store.path, &store.screen)
    }

    #[test]
    fn adds_up_to_four_and_refuses_a_fifth() {
        let mut store = Store::scratch();
        assert!(store.list().is_empty() && !store.full());
        assert!(!store.path.exists(), "written on its first change");
        for n in 1..=4 {
            store.add(laptop(n)).unwrap();
        }
        assert!(store.full());
        assert!(store.add(laptop(5)).is_err());
        let ids: Vec<&str> = store.list().iter().map(|laptop| laptop.id.as_str()).collect();
        assert_eq!(ids, (1..=4).map(|n| hex(&[n; 16])).collect::<Vec<_>>());
        let again = reopened(&store);
        assert_eq!(again.list().len(), 4);
        let first = again.find(&hex(&[1; 16])).unwrap();
        assert_eq!((first.name.as_str(), first.key, first.added.as_str()), ("laptop 1", [1; 32], "2026-11-13T10:00:00Z"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&store.path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!store.listed().contains("laptop 5"));
    }

    #[test]
    fn forget_removes_and_republishes_the_list() {
        let mut store = Store::scratch();
        store.add(laptop(1)).unwrap();
        store.add(laptop(2)).unwrap();
        assert_eq!(store.listed(), format!("{} laptop 1\n{} laptop 2\n", hex(&[1; 16]), hex(&[2; 16])));
        assert!(store.forget(&hex(&[1; 16])).unwrap());
        assert!(!store.forget(&hex(&[1; 16])).unwrap(), "forgotten once");
        assert!(store.find(&hex(&[1; 16])).is_none() && store.find(&hex(&[2; 16])).is_some());
        assert_eq!(store.listed(), format!("{} laptop 2\n", hex(&[2; 16])));
        assert_eq!(reopened(&store).list().len(), 1);
        assert!(store.forget(&hex(&[2; 16])).unwrap());
        assert_eq!(store.listed(), "");
    }

    #[test]
    fn a_torn_write_leaves_the_old_file() {
        let mut store = Store::scratch();
        store.add(laptop(1)).unwrap();
        // A write cut off before its rename leaves its temporary file.
        std::fs::write(partial(&store.path), b"{\"unit\":").unwrap();
        let mut again = reopened(&store);
        assert_eq!((again.unit_id(), again.list().len()), (store.unit_id(), 1));
        again.add(laptop(2)).unwrap();
        assert_eq!(reopened(&store).list().len(), 2, "the next write replaces it");
        assert!(!partial(&store.path).exists());
        // An unreadable file is an empty store with a new id.
        std::fs::write(&store.path, b"{\"unit\":").unwrap();
        let unreadable = reopened(&store);
        assert!(unreadable.list().is_empty() && unreadable.unit_id() != store.unit_id());
    }

    #[test]
    fn the_unit_id_is_stable_across_opens() {
        let mut store = Store::scratch();
        let id = store.unit_id().to_string();
        assert!(is_id(&id), "{id}");
        store.add(laptop(1)).unwrap();
        assert_eq!(reopened(&store).unit_id(), id);
        store.forget(&hex(&[1; 16])).unwrap();
        assert_eq!(reopened(&store).unit_id(), id, "an empty list keeps it");
        assert_ne!(Store::scratch().unit_id(), id, "each unit has its own");
    }

    #[test]
    fn replace_takes_the_old_laptops_place_in_a_full_list() {
        let mut store = Store::scratch();
        for n in 1..=4 {
            store.add(laptop(n)).unwrap();
        }
        assert!(store.replace(&hex(&[2; 16]), laptop(5)).unwrap());
        let ids: Vec<String> = store.list().iter().map(|laptop| laptop.id.clone()).collect();
        assert_eq!(ids, [1, 5, 3, 4].map(|n: u8| hex(&[n; 16])));
        assert!(store.listed().contains("laptop 5") && !store.listed().contains("laptop 2"));
        assert_eq!(reopened(&store).list().len(), 4);
        assert!(store.replace(&hex(&[9; 16]), laptop(6)).is_err(), "an old laptop not trusted is an add, refused while full");
        store.forget(&hex(&[1; 16])).unwrap();
        assert!(!store.replace(&hex(&[9; 16]), laptop(6)).unwrap(), "added");
        assert!(store.find(&hex(&[6; 16])).is_some());
    }

    #[test]
    fn a_proof_holds_for_its_laptop_and_message_alone() {
        let mut store = Store::scratch();
        store.add(laptop(1)).unwrap();
        store.add(laptop(2)).unwrap();
        let (id, message) = (hex(&[1; 16]), [7u8; 33]);
        let proof = replace_proof(&[1; 32], store.unit_id(), &id, &message);
        assert!(store.proves(&id, &message, &proof));
        assert!(!store.proves(&id, &[8; 33], &proof), "another exchange's message");
        assert!(!store.proves(&hex(&[2; 16]), &message, &proof), "another laptop");
        assert!(!store.proves(&hex(&[3; 16]), &message, &replace_proof(&[3; 32], store.unit_id(), &hex(&[3; 16]), &message)),
                "a laptop not trusted");
        assert!(!store.proves(&id, &message, &replace_proof(&[2; 32], store.unit_id(), &id, &message)), "another key");
        assert!(!store.proves(&id, &message, "zz"));
    }
}
