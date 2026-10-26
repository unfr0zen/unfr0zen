//! A settings pairing replaces existing fabrics only after the new fabric is
//! committed by CommissioningComplete. AddNOC alone must never revoke a host.
use std::io::Write;
use std::num::NonZeroU8;
use std::path::{Path, PathBuf};

use rs_matter::error::{Error, ErrorCode};
use rs_matter::fabric::FabricPersist;
use rs_matter::persist::{KvBlobStoreAccess, FABRIC_KEYS_START};
use rs_matter::sc::pase::CommWindowState;
use rs_matter::Matter;

pub struct Replacement {
    path: PathBuf,
    previous: Option<Vec<NonZeroU8>>,
}

fn io_error(_: impl std::fmt::Debug) -> Error { ErrorCode::StdIoError.into() }

fn committed(kv: &impl KvBlobStoreAccess, index: NonZeroU8) -> Result<bool, Error> {
    kv.access(|store, buf| store.load(FABRIC_KEYS_START + index.get() as u16, buf).map(|data| data.is_some()))
}

pub fn has_committed_fabrics(matter: &Matter<'_>, kv: &impl KvBlobStoreAccess) -> Result<bool, Error> {
    let indices: Vec<_> = matter.with_state(|s| s.fabrics.iter().map(|f| f.fab_idx()).collect());
    for index in indices {
        if committed(kv, index)? { return Ok(true); }
    }
    Ok(false)
}

impl Replacement {
    pub fn load(storage: &Path) -> Result<Self, Error> {
        let path = storage.join("pairing-replacement.json");
        let previous = match std::fs::read(&path) {
            // Unreadable, the intent is set aside: every fabric stays.
            Ok(data) => serde_json::from_slice(&data).map_err(|e| {
                crate::logf!("matter: pairing replacement intent unreadable, keeping every fabric: {e}");
                if let Err(e) = std::fs::rename(&path, path.with_extension("corrupt")) {
                    crate::logf!("matter: {}: {e}", path.display());
                }
            }).ok(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_error(e)),
        };
        Ok(Self { path, previous })
    }

    pub fn begin(&mut self, matter: &Matter<'_>, kv: &impl KvBlobStoreAccess) -> Result<(), Error> {
        // Keep the baseline when a second click reopens an unfinished attempt.
        if self.previous.is_some() { return Ok(()); }
        let indices: Vec<_> = matter.with_state(|s| s.fabrics.iter().map(|f| f.fab_idx()).collect());
        let mut previous = Vec::new();
        for index in indices {
            if committed(kv, index)? { previous.push(index); }
        }
        let temp = self.path.with_extension("tmp");
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)] {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp).map_err(io_error)?;
        file.write_all(&serde_json::to_vec(&previous).map_err(io_error)?).map_err(io_error)?;
        file.sync_all().map_err(io_error)?;
        std::fs::rename(&temp, &self.path).map_err(io_error)?;
        self.previous = Some(previous);
        Ok(())
    }

    pub fn finish(&mut self, matter: &Matter<'_>, kv: &impl KvBlobStoreAccess) -> Result<Vec<NonZeroU8>, Error> {
        let Some(previous) = &self.previous else { return Ok(Vec::new()); };
        if !matches!(matter.comm_window_state(), CommWindowState::Closed) { return Ok(Vec::new()); }
        let indices: Vec<_> = matter.with_state(|s| s.fabrics.iter().map(|f| f.fab_idx()).collect());
        let mut accepted = false;
        for index in indices {
            // rs-matter writes this blob only on CommissioningComplete, after
            // the new controller proves its operational credentials over CASE.
            if !previous.contains(&index) && committed(kv, index)? { accepted = true; }
        }
        let mut removed = Vec::new();
        if accepted {
            let mut persist = FabricPersist::new(kv);
            for &index in previous {
                // Disk first: a restart must not bring the old credentials back.
                // The intent remains on disk until all removals succeed.
                persist.remove(index)?;
                let present = matter.with_state(|state| {
                    let present = state.fabrics.get(index).is_some();
                    if present { state.fabrics.remove(index)?; }
                    state.sessions.remove_where(|session| session.get_local_fabric_idx() == index.get());
                    Ok::<_, Error>(present)
                })?;
                // A fabric its controller already removed is not reported again.
                if present { removed.push(index); }
            }
            persist.run()?;
            matter.transport().notify_session_removed();
        }
        // Closed without a committed new fabric means timeout/cancellation.
        // Preserve every old fabric in that case.
        std::fs::remove_file(&self.path).map_err(io_error)?;
        self.previous = None;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rs_matter::dm::clusters::basic_info::BasicInfoConfig;
    use rs_matter::dm::devices::test::TEST_DEV_ATT;
    use rs_matter::persist::DirKvBlobStore;
    use rs_matter::BasicCommData;
    use rs_matter::sc::pase::{Spake2pVerifierPassword, Spake2pVerifierPasswordRef};
    use rs_matter::crypto::default_crypto;
    use rs_matter::dm::AttrChangeNotifier;
    use rs_matter::dm::devices::test::DAC_PRIVKEY;

    struct Notify;
    impl AttrChangeNotifier for Notify {
        fn notify_attr_changed(&self, _: u16, _: u32, _: u32) {}
        fn notify_cluster_changed(&self, _: u16, _: u32) {}
        fn notify_endpoint_changed(&self, _: u16) {}
        fn notify_all_changed(&self) {}
    }

    fn credentials() -> BasicCommData {
        BasicCommData {
            password: Spake2pVerifierPassword::new_from_ref(Spake2pVerifierPasswordRef::new(&20202021u32.to_le_bytes())),
            discriminator: 3840,
        }
    }

    struct Store(PathBuf);
    impl Store {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("unfr0zen-matter-pairing-{}-{}", std::process::id(), rand::random::<u64>()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Store { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    fn add(matter: &Matter<'_>, kv: &impl KvBlobStoreAccess, save: bool) -> NonZeroU8 {
        matter.with_state(|state| {
            let fabric = state.fabrics.add_with_post_init(|_| Ok(())).unwrap();
            if save { FabricPersist::new(kv).store(fabric).unwrap(); }
            fabric.fab_idx()
        })
    }

    #[test]
    fn provisional_credentials_are_not_reported_as_paired() {
        let store = Store::new();
        let info = BasicInfoConfig::new();
        let matter = Matter::new(&info, credentials(), &TEST_DEV_ATT, 5540);
        let kv = matter.kv(DirKvBlobStore::new(store.0.clone()));
        assert!(!has_committed_fabrics(&matter, &kv).unwrap());
        add(&matter, &kv, false);
        assert!(matter.has_fabrics());
        assert!(!has_committed_fabrics(&matter, &kv).unwrap());
        add(&matter, &kv, true);
        assert!(has_committed_fabrics(&matter, &kv).unwrap());
    }

    #[test]
    fn provisional_or_failed_commissioning_keeps_previous_host() {
        let store = Store::new();
        let info = BasicInfoConfig::new();
        let matter = Matter::new(&info, credentials(), &TEST_DEV_ATT, 5540);
        let kv = matter.kv(DirKvBlobStore::new(store.0.clone()));
        let old = add(&matter, &kv, true);
        let mut replacement = Replacement::load(&store.0).unwrap();
        replacement.begin(&matter, &kv).unwrap();
        matter.open_basic_comm_window(900, default_crypto(rand::rng(), DAC_PRIVKEY), &Notify).unwrap();
        let pending = add(&matter, &kv, false);
        assert!(replacement.finish(&matter, &kv).unwrap().is_empty());
        assert!(store.0.join("pairing-replacement.json").exists());
        // The window expires or is cancelled before CommissioningComplete.
        matter.close_comm_window(&Notify).unwrap();
        assert!(replacement.finish(&matter, &kv).unwrap().is_empty());
        assert!(committed(&kv, old).unwrap());
        assert!(matter.with_state(|s| s.fabrics.get(old).is_some()));
        assert!(!committed(&kv, pending).unwrap());
    }

    #[test]
    fn successful_commissioning_removes_all_old_credentials_even_after_restart() {
        let store = Store::new();
        let info = BasicInfoConfig::new();
        let matter = Matter::new(&info, credentials(), &TEST_DEV_ATT, 5540);
        let kv = matter.kv(DirKvBlobStore::new(store.0.clone()));
        let first = add(&matter, &kv, true);
        let second = add(&matter, &kv, true);
        let mut replacement = Replacement::load(&store.0).unwrap();
        replacement.begin(&matter, &kv).unwrap();
        matter.open_basic_comm_window(900, default_crypto(rand::rng(), DAC_PRIVKEY), &Notify).unwrap();
        let new = add(&matter, &kv, true);
        assert!(replacement.finish(&matter, &kv).unwrap().is_empty());
        assert!(committed(&kv, first).unwrap());
        matter.close_comm_window(&Notify).unwrap();
        // Reload the durable intent as after a restart just after acceptance.
        let mut replacement = Replacement::load(&store.0).unwrap();
        assert_eq!(replacement.finish(&matter, &kv).unwrap(), vec![first, second]);
        assert!(!committed(&kv, first).unwrap());
        assert!(!committed(&kv, second).unwrap());
        assert!(committed(&kv, new).unwrap());
        assert_eq!(matter.with_state(|s| s.fabrics.iter().map(|f| f.fab_idx()).collect::<Vec<_>>()), vec![new]);
        assert!(!store.0.join("pairing-replacement.json").exists());
        assert!(replacement.finish(&matter, &kv).unwrap().is_empty());
        // A fresh process loading the real on-disk Matter store retains only
        // the new credentials, so the old host cannot return after reboot.
        let restarted = Matter::new(&info, credentials(), &TEST_DEV_ATT, 5540);
        let restarted_kv = restarted.kv(DirKvBlobStore::new(store.0.clone()));
        restarted.startup(&restarted_kv).unwrap();
        assert_eq!(restarted.with_state(|s| s.fabrics.iter().map(|f| f.fab_idx()).collect::<Vec<_>>()), vec![new]);
    }

    fn fabrics(matter: &Matter<'_>) -> Vec<NonZeroU8> {
        matter.with_state(|s| s.fabrics.iter().map(|f| f.fab_idx()).collect())
    }

    #[test]
    fn enabling_pairing_again_keeps_the_first_baseline() {
        let store = Store::new();
        let info = BasicInfoConfig::new();
        let matter = Matter::new(&info, credentials(), &TEST_DEV_ATT, 5540);
        let kv = matter.kv(DirKvBlobStore::new(store.0.clone()));
        let old = add(&matter, &kv, true);
        let mut replacement = Replacement::load(&store.0).unwrap();
        replacement.begin(&matter, &kv).unwrap();
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.0.join("pairing-replacement.json")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(!store.0.join("pairing-replacement.tmp").exists());
        matter.open_basic_comm_window(900, default_crypto(rand::rng(), DAC_PRIVKEY), &Notify).unwrap();
        let new = add(&matter, &kv, true);
        // A second click before the window closed, after the new controller
        // committed: the new controller is not part of what gets replaced.
        replacement.begin(&matter, &kv).unwrap();
        assert_eq!(Replacement::load(&store.0).unwrap().previous, Some(vec![old]));
        matter.close_comm_window(&Notify).unwrap();
        assert_eq!(replacement.finish(&matter, &kv).unwrap(), vec![old]);
        assert_eq!(fabrics(&matter), vec![new]);
        assert!(committed(&kv, new).unwrap());
    }

    #[test]
    fn first_pairing_through_the_settings_screen_removes_nothing() {
        let store = Store::new();
        let info = BasicInfoConfig::new();
        let matter = Matter::new(&info, credentials(), &TEST_DEV_ATT, 5540);
        let kv = matter.kv(DirKvBlobStore::new(store.0.clone()));
        let mut replacement = Replacement::load(&store.0).unwrap();
        replacement.begin(&matter, &kv).unwrap();
        matter.open_basic_comm_window(900, default_crypto(rand::rng(), DAC_PRIVKEY), &Notify).unwrap();
        let new = add(&matter, &kv, true);
        matter.close_comm_window(&Notify).unwrap();
        assert!(replacement.finish(&matter, &kv).unwrap().is_empty());
        assert_eq!(fabrics(&matter), vec![new]);
        assert!(!store.0.join("pairing-replacement.json").exists());
        // The intent is spent: a later close does nothing.
        assert!(Replacement::load(&store.0).unwrap().finish(&matter, &kv).unwrap().is_empty());
    }

    #[test]
    fn a_previous_host_that_left_during_pairing_does_not_stop_the_replacement() {
        let store = Store::new();
        let info = BasicInfoConfig::new();
        let matter = Matter::new(&info, credentials(), &TEST_DEV_ATT, 5540);
        let kv = matter.kv(DirKvBlobStore::new(store.0.clone()));
        let first = add(&matter, &kv, true);
        let second = add(&matter, &kv, true);
        let mut replacement = Replacement::load(&store.0).unwrap();
        replacement.begin(&matter, &kv).unwrap();
        matter.open_basic_comm_window(900, default_crypto(rand::rng(), DAC_PRIVKEY), &Notify).unwrap();
        // The first controller removes its own fabric meanwhile.
        let mut persist = FabricPersist::new(&kv);
        persist.remove(first).unwrap();
        persist.run().unwrap();
        matter.with_state(|s| s.fabrics.remove(first)).unwrap();
        let new = add(&matter, &kv, true);
        assert_ne!(new, first);
        matter.close_comm_window(&Notify).unwrap();
        // Only the fabric still there is reported removed.
        assert_eq!(replacement.finish(&matter, &kv).unwrap(), vec![second]);
        assert_eq!(fabrics(&matter), vec![new]);
        assert!(!committed(&kv, second).unwrap());
        assert!(!store.0.join("pairing-replacement.json").exists());
    }

    #[test]
    fn a_corrupt_replacement_intent_is_set_aside_and_keeps_every_host() {
        let store = Store::new();
        let info = BasicInfoConfig::new();
        let matter = Matter::new(&info, credentials(), &TEST_DEV_ATT, 5540);
        let kv = matter.kv(DirKvBlobStore::new(store.0.clone()));
        let old = add(&matter, &kv, true);
        std::fs::write(store.0.join("pairing-replacement.json"), b"[1, 2").unwrap();
        let mut replacement = Replacement::load(&store.0).unwrap();
        assert_eq!(replacement.previous, None);
        assert!(!store.0.join("pairing-replacement.json").exists());
        assert!(store.0.join("pairing-replacement.corrupt").exists());
        let new = add(&matter, &kv, true);
        assert!(replacement.finish(&matter, &kv).unwrap().is_empty());
        assert_eq!(fabrics(&matter), vec![old, new]);
    }
}
