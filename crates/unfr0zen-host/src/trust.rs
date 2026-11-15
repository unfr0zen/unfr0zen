//! The units that trust this computer: for each, the laptop id the unit
//! granted it and the trust key both derived from the session the grant came
//! in (nestlocal's wifi_update/trust.rs), and the address the unit was last seen
//! at. A file a unit, readable by the user alone. Trusted again, this
//! computer proves it holds the old key (replace_proof), so the unit's new
//! grant takes the old laptop's place there, as its file here.

use std::io::Write;
use std::path::PathBuf;

use unfr0zen_core::{Error, Result};

use crate::flow::{default_data_dir, failure, private_dir};
use update_wire::auth::is_id;
use update_wire::{hex, unhex};

/// What this computer keeps for a unit that trusts it.
#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    pub unit: String,
    pub laptop: String,
    pub key: [u8; 32],
    pub address: String,
}

/// Leaves the key out.
impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Entry").field("unit", &self.unit).field("laptop", &self.laptop).field("address", &self.address)
            .finish_non_exhaustive()
    }
}

pub struct Store {
    dir: PathBuf,
}

/// The proof, in hex, that the client of the key exchange whose message is
/// message holds entry's trust key, for the unit to have the exchange's
/// grant replace entry's laptop (update_wire::auth::replace_proof).
pub(crate) fn replace_proof(entry: &Entry, message: &[u8]) -> String {
    update_wire::auth::replace_proof(&entry.key, &entry.unit, &entry.laptop, message)
}

/// The user's, in the data directory.
impl Default for Store {
    fn default() -> Store {
        Store::at(default_data_dir().join("trusted"))
    }
}

impl Store {
    pub fn at(dir: PathBuf) -> Store {
        Store { dir }
    }

    /// The unit's file: its id names it, so nothing that is not an id may.
    fn path(&self, unit: &str) -> Option<PathBuf> {
        is_id(unit).then(|| self.dir.join(format!("{unit}.json")))
    }

    /// What is kept for the unit of id unit, if it trusts this computer.
    pub fn get(&self, unit: &str) -> Option<Entry> {
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(self.path(unit)?).ok()?).ok()?;
        let laptop = value["laptop"].as_str().filter(|laptop| is_id(laptop))?.to_string();
        let key = unhex(value["key"].as_str()?)?.try_into().ok()?;
        let address = value["address"].as_str()?.to_string();
        Some(Entry { unit: unit.to_string(), laptop, key, address })
    }

    /// Keeps entry in place of any for its unit, in a file only the user
    /// reads, written whole or not at all.
    pub fn put(&self, entry: &Entry) -> Result<()> {
        let path = self.path(&entry.unit).filter(|_| is_id(&entry.laptop))
            .ok_or_else(|| failure("the unit granted trust under ids that are not 16 bytes in hex"))?;
        if !self.dir.is_dir() {
            private_dir(&self.dir)?;
        }
        let context = |error: std::io::Error| Error::Io(format!("{}: {error}", path.display()));
        let body = serde_json::json!({"unit": entry.unit, "laptop": entry.laptop, "key": hex(&entry.key), "address": entry.address});
        let mut file = tempfile::NamedTempFile::new_in(&self.dir).map_err(context)?;
        file.write_all(body.to_string().as_bytes()).and_then(|()| file.as_file().sync_all()).map_err(context)?;
        file.persist(&path).map_err(|e| context(e.error))?;
        Ok(())
    }

    /// An empty store in a directory of its own, made on its first put.
    #[cfg(test)]
    pub(crate) fn scratch() -> Store {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static STORES: AtomicUsize = AtomicUsize::new(0);
        let n = STORES.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("unfr0zen-trusted-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::at(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_replacement_proof_matches_the_shared_test_vector() {
        let key: Vec<u8> = (0..32).collect();
        let entry = Entry { unit: "00".repeat(16), laptop: "11".repeat(16), key: key.try_into().unwrap(), address: String::new() };
        let message: Vec<u8> = (0..33).collect();
        assert_eq!(replace_proof(&entry, &message), "d6294793b2415d22cf34f97e8de2e67e7577aca30db7aa948f53cb0269be4b7b");
    }

    #[test]
    fn an_entry_is_kept_in_a_file_only_the_user_reads() {
        let store = Store::scratch();
        let entry = Entry { unit: "ab".repeat(16), laptop: "cd".repeat(16), key: [7; 32], address: "192.168.1.50:8443".into() };
        assert!(store.get(&entry.unit).is_none());
        store.put(&entry).unwrap();
        assert_eq!(store.get(&entry.unit), Some(entry.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(store.dir.join(format!("{}.json", entry.unit))), 0o600);
            assert_eq!(mode(store.dir.clone()), 0o700);
        }
        // A later grant replaces it.
        let newer = Entry { laptop: "ef".repeat(16), ..entry.clone() };
        store.put(&newer).unwrap();
        assert_eq!(store.get(&entry.unit), Some(newer));
        // Ids are the unit's hex: nothing else names a file.
        assert!(store.put(&Entry { unit: "../x".into(), ..entry.clone() }).is_err());
        assert!(store.get("../x").is_none());
        assert!(format!("{entry:?}").contains(&entry.unit) && !format!("{entry:?}").contains("7, 7"), "the key is not shown");
    }
}
