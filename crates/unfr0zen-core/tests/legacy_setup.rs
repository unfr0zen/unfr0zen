//! Full setup against a private extracted legacy root named by NLCLIENT_LEGACY_ROOT.
//! The fixture is read only; all setup writes stay in memory.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use unfr0zen_core::diamond3::scratch;
use unfr0zen_core::nestlocal::{generate_pki, Matter};
use unfr0zen_core::nlclient;
use unfr0zen_core::setup::{self, Options, Tree};
use unfr0zen_core::shell::{sha256_hex, Digests, File, Volume};
use unfr0zen_core::{Device, Result};

#[derive(Clone)]
struct MemoryVolume {
    files: Tree,
    directories: BTreeMap<String, String>,
}

impl MemoryVolume {
    fn load(root: &Path) -> Self {
        let mut volume = Self { files: Tree::new(), directories: BTreeMap::new() };
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                let metadata = std::fs::symlink_metadata(&path).unwrap();
                let name = path.strip_prefix(root).unwrap().to_str().unwrap().to_string();
                let mode = metadata.permissions().mode() & 0o7777;
                if metadata.is_dir() {
                    volume.directories.insert(name, format!("0:0 {mode:o}"));
                    pending.push(path);
                } else if metadata.is_file() {
                    volume.files.insert(name, (std::fs::read(path).unwrap(), mode));
                }
            }
        }
        volume
    }
}

impl Volume for MemoryVolume {
    fn read(&mut self, name: &str) -> Result<Option<File<Vec<u8>>>> {
        Ok(self.files.get(name).map(|(data, mode)| ("0:0".into(), *mode, data.clone())))
    }
    fn inspect(&mut self, name: &str) -> Result<Option<File<String>>> {
        Ok(self.files.get(name).map(|(data, mode)| ("0:0".into(), *mode, sha256_hex(data))))
    }
    fn directory(&mut self, name: &str) -> Result<Option<String>> {
        Ok(self.directories.get(name).cloned())
    }
    fn free_bytes(&mut self) -> Result<u64> {
        Ok(1 << 30)
    }
    fn shell_binary(&mut self) -> Result<String> {
        Ok("0 755".into())
    }
    fn write(&mut self, name: &str, data: &[u8], owner: &str, mode: u32) -> Result<()> {
        assert_eq!(owner, "0:0");
        self.files.insert(name.into(), (data.to_vec(), mode));
        Ok(())
    }
    fn make_directory(&mut self, name: &str, mode: u32) -> Result<()> {
        self.directories.insert(name.into(), format!("0:0 {mode:o}"));
        Ok(())
    }
    fn remove(&mut self, name: &str) -> Result<()> {
        self.files.remove(name);
        Ok(())
    }
    fn remount(&mut self, _writable: bool) -> Result<()> {
        Ok(())
    }
    fn digests(&mut self, names: &[String]) -> Result<Digests> {
        Ok(names
            .iter()
            .map(|name| {
                let (data, mode) = &self.files[name];
                (name.clone(), (Some(sha256_hex(data)), Some(format!("0:0 {mode:o}"))))
            })
            .collect())
    }
}

#[test]
fn legacy_root_setup_is_dry_run_safe_idempotent_and_reversible() {
    let Some(root) = std::env::var_os("NLCLIENT_LEGACY_ROOT") else { return };
    let mut volume = MemoryVolume::load(Path::new(&root));
    // An extractor may omit permissions; the optional map carries the UBIFS inode modes.
    if let Some(path) = std::env::var_os("NLCLIENT_LEGACY_MODES") {
        let modes: BTreeMap<String, u32> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        for (name, (_, mode)) in &mut volume.files {
            *mode = modes[name];
        }
    }
    let stock = volume.files[nlclient::BINARY].0.clone();
    assert!(nlclient::legacy_abi(&stock), "fixture must hold the reviewed legacy nlclient");
    let build = Path::new(env!("UNFR0ZEN_ROOT")).join("build");
    let modern = std::fs::read(build.join("custom-screen.o")).unwrap();
    let legacy = std::fs::read(build.join("custom-screen-legacy.o")).unwrap();
    let builder = |source: &[u8], sections: &[nlclient::menu::Section]| {
        let object = if nlclient::legacy_abi(source) { &legacy } else { &modern };
        nlclient::build(source, object, sections)
    };
    let pki = generate_pki().unwrap();
    let defaults = Options {
        root_hash: setup::root_hash("unfr0zen").unwrap(),
        local_cloud: b"\x7fELF local cloud".to_vec(),
        wake_filter: b"\x7fELF wake filter".to_vec(),
        ..Options::default()
    };
    let integrations = Options {
        matter: Some(Matter::from_pairing_code(unfr0zen_core::nestlocal::DEFAULT_MATTER_CODE).unwrap()),
        zigbee: true,
        ..defaults.clone()
    };
    for (phase, options) in [("defaults", &defaults), ("matter and zigbee", &integrations), ("disabled", &defaults)] {
        eprintln!("legacy setup: {phase}");
        let before = volume.clone();
        let dry = setup::set_up_on_device(
            &mut volume,
            Device::Diamond3,
            options,
            b"NESTLOCAL",
            &mut || Ok(pki.clone()),
            true,
            &builder,
        )
        .unwrap();
        assert!(volume.files == before.files, "dry run changed file contents or modes");
        assert!(volume.directories == before.directories, "dry run changed directories");
        let changes = setup::set_up_on_device(
            &mut volume,
            Device::Diamond3,
            options,
            b"NESTLOCAL",
            &mut || Ok(pki.clone()),
            false,
            &builder,
        )
        .unwrap();
        assert_eq!(
            changes.iter().map(|change| (&change.name, change.action)).collect::<Vec<_>>(),
            dry.iter().map(|change| (&change.name, change.action)).collect::<Vec<_>>()
        );
        let journal = &volume.files[nlclient::JOURNAL].0;
        let patched = &volume.files[nlclient::BINARY].0;
        assert_ne!(patched, &stock);
        assert_eq!(nlclient::Patch::parse(journal).unwrap().apply(patched, true).unwrap(), stock);
        assert_eq!(volume.files[scratch::name(nlclient::ORIGINAL)].0, stock);
        assert!(setup::set_up_on_device(
            &mut volume,
            Device::Diamond3,
            options,
            b"NESTLOCAL",
            &mut || Ok(pki.clone()),
            false,
            &builder,
        )
        .unwrap()
        .is_empty());
    }
}
