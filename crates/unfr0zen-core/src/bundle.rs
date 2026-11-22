//! The setup bundle: one gzipped tar holding everything a setup installs
//! (nestlocal, its overlays, the settings screens' object, the scripts run
//! around the setup) and unfr0zen-apply, which runs the setup on the unit.
//! The same for every unit of a device and release: what a run asks for
//! travels beside it as Settings.
//!
//! manifest.json lists every other member with its SHA-256 and mode, and
//! manifest.sig is an Ed25519 signature of manifest.json's bytes, base64.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::nestlocal::{Matter, Mqtt, Overlay, Pki};
use crate::setup::Options;
use crate::shell::{sha256_hex, sha256_hex_of};
use crate::{device, Device, Error, Result};

pub const FORMAT: u32 = 1;
/// The public key releases sign their bundles with (utils/bundle/release.pub);
/// a unit installs an update only when it is signed by it.
pub const RELEASE_KEY: &str = include_str!(concat!(env!("UNFR0ZEN_ROOT"), "/utils/bundle/release.pub"));
pub const MANIFEST: &str = "manifest.json";
pub const SIGNATURE: &str = "manifest.sig";
/// The setup engine, a static ARMv7 binary.
pub const APPLY: &str = "bin/unfr0zen-apply";
pub const NESTLOCAL: &str = "assets/nestlocal";
pub const WAKE_FILTER: &str = "assets/libwakefilter.so";
/// Preloaded into nlclient on a unit running nestlocal (setup::Options::local_cloud).
pub const LOCAL_CLOUD: &str = "assets/liblocalcloud.so";
pub const SCREEN: &str = "assets/custom-screen.o";
/// The gen 2's native Sensors view helper, for the external temperature.
pub const RCS_SCREEN: &str = "assets/j49-rcs-screen.o";
/// Run before the setup reads the volume; failing, it stops the run with
/// nothing written.
pub const PRE_SETUP: &str = "scripts/pre-setup.sh";
/// Run after the setup's writes are verified on flash; failing, it fails the run.
pub const POST_SETUP: &str = "scripts/post-setup.sh";
/// overlays/NAME/PATH installs PATH on the volume when the run asks for NAME.
const OVERLAYS: &str = "overlays/";
pub const THREAD: &str = "thread";
pub const DOOM: &str = "doom";
/// Customizable home screen themes: the theme preload and library.
pub const THEME: &str = "theme";
/// The SSH server of a unit without its own (the gen 2's dropbear).
pub const SSH: &str = "ssh";

/// Name: (mode, contents).
pub type Files = BTreeMap<String, (u32, Vec<u8>)>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub sha256: String,
    pub mode: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub device: String,
    pub version: String,
    pub files: BTreeMap<String, Member>,
    /// Directories an overlay makes, as overlays/NAME/PATH: mode.
    pub directories: BTreeMap<String, u32>,
}

/// An unpacked bundle, every file checked against the manifest.
#[derive(Clone)]
pub struct Bundle {
    pub manifest: Manifest,
    manifest_bytes: Vec<u8>,
    signature: Option<Vec<u8>>,
    pub files: Files,
}

/// The bundle as gzipped tar: manifest.json, manifest.sig when signed, then
/// the files in name order. The same inputs give the same bytes.
pub fn pack(
    device: Device,
    version: &str,
    files: &Files,
    directories: &BTreeMap<String, u32>,
    key: Option<&SigningKey>,
) -> Vec<u8> {
    let (_, manifest_bytes) = manifest(device, version, files, directories);
    let mut tar = tar::Builder::new(Vec::new());
    let mut add = |name: &str, mode: u32, data: &[u8]| {
        let mut header = tar::Header::new_ustar();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, name, data).expect("writing to memory");
    };
    add(MANIFEST, 0o644, &manifest_bytes);
    if let Some(key) = key {
        let signature = STANDARD.encode(key.sign(&manifest_bytes).to_bytes());
        add(SIGNATURE, 0o644, format!("{signature}\n").as_bytes());
    }
    for (name, (mode, data)) in files {
        add(name, *mode, data);
    }
    let tar = tar.into_inner().expect("writing to memory");
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gzip.write_all(&tar).expect("writing to memory");
    gzip.finish().expect("writing to memory")
}

/// The manifest of these files, and its bytes as manifest.json holds them.
fn manifest(device: Device, version: &str, files: &Files, directories: &BTreeMap<String, u32>) -> (Manifest, Vec<u8>) {
    let manifest = Manifest {
        format: FORMAT,
        device: device.name().into(),
        version: version.into(),
        files: files
            .iter()
            .map(|(name, (mode, data))| (name.clone(), Member { sha256: sha256_hex(data), mode: *mode }))
            .collect(),
        directories: directories.clone(),
    };
    let mut bytes = serde_json::to_vec_pretty(&manifest).expect("a manifest serializes");
    bytes.push(b'\n');
    (manifest, bytes)
}

impl Bundle {
    /// The bundle of these files as unpack would give it, without packing it:
    /// for a run setting up on the host.
    pub fn from_files(device: Device, version: &str, files: Files, directories: &BTreeMap<String, u32>) -> Bundle {
        let (manifest, manifest_bytes) = manifest(device, version, &files, directories);
        Bundle { manifest, manifest_bytes, signature: None, files }
    }

    /// Unpacks a bundle and checks its files against its manifest: every one
    /// listed, present, and unchanged. The signature is checked by verify.
    pub fn unpack(data: &[u8]) -> Result<Bundle> {
        Bundle::read(data)
    }

    /// Unpacks a bundle as it is read, from a file, say: the 64 MB OMAP
    /// units cannot hold the compressed bundle beside its files.
    pub fn read(source: impl Read) -> Result<Bundle> {
        Bundle::read_some(source, |_| true)
    }

    /// Unpacks a bundle as it is read, keeping only the files `keep` names:
    /// every file is still checked against the manifest, the rest dropped as
    /// they are hashed, which a 64 MB unit needs.
    pub fn read_some(source: impl Read, keep: impl Fn(&str) -> bool) -> Result<Bundle> {
        read_limited(source, keep, EXPANDED_LIMIT)
    }

    /// The manifest the bundle starts with, unchecked: what a run needs to
    /// know before it reads the bundle itself (read checks it).
    pub fn read_manifest(source: impl Read) -> Result<Manifest> {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(source));
        for entry in archive.entries().map_err(|e| Error::Device(format!("bundle: {e}")))? {
            let entry = entry.map_err(|e| Error::Device(format!("bundle: {e}")))?;
            if entry.path().map_err(|e| Error::Device(format!("bundle: {e}")))?.to_str() == Some(MANIFEST) {
                return parse_manifest(&read_small(entry, MANIFEST)?);
            }
        }
        device("bundle: no manifest.json")
    }

    /// Checks the manifest's signature against a trusted key.
    pub fn verify(&self, key: &VerifyingKey) -> Result<()> {
        let signature = self.signature.as_deref().map_or_else(|| device("bundle: unsigned"), Ok)?;
        let signature =
            Signature::from_slice(signature).map_err(|_| Error::Device("bundle: malformed signature".into()))?;
        key.verify(&self.manifest_bytes, &signature).map_err(|_| Error::Device("bundle: bad signature".into()))
    }

    pub fn file(&self, name: &str) -> Result<&[u8]> {
        self.files.get(name).map(|(_, data)| data.as_slice()).map_or_else(|| device(format!("bundle: no {name}")), Ok)
    }

    /// Takes file NAME out of the bundle, if it holds one.
    pub fn take(&mut self, name: &str) -> Option<Vec<u8>> {
        self.files.remove(name).map(|(_, data)| data)
    }

    /// Takes the overlay under overlays/NAME/ out of the bundle.
    pub fn take_overlay(&mut self, name: &str) -> Result<Overlay> {
        let overlay = self.overlay_files(name);
        if overlay.files.is_empty() {
            return device(format!("bundle: no {name} overlay"));
        }
        Ok(overlay)
    }

    /// The overlay the bundle holds under overlays/NAME/.
    pub fn overlay(&self, name: &str) -> Result<Overlay> {
        self.clone().take_overlay(name)
    }

    /// The overlay under overlays/NAME/, its files taken out of the bundle.
    fn overlay_files(&mut self, name: &str) -> Overlay {
        let prefix = format!("{OVERLAYS}{name}/");
        let names: Vec<String> = self.files.keys().filter(|path| path.starts_with(&prefix)).cloned().collect();
        let files = names
            .into_iter()
            .map(|path| {
                let (mode, data) = self.files.remove(&path).expect("listed");
                (path[prefix.len()..].to_string(), mode, data)
            })
            .collect();
        let directories = self
            .manifest
            .directories
            .iter()
            .filter_map(|(path, mode)| path.strip_prefix(&prefix).map(|path| (path.to_string(), *mode)))
            .collect();
        Overlay { files, directories }
    }
}

/// The most a bundle's files may hold together, unpacked: its tar headers
/// say what they like, and a unit has 64 MB.
const EXPANDED_LIMIT: u64 = 64 << 20;
/// The most manifest.json or manifest.sig may hold.
const SMALL_LIMIT: u64 = 1 << 20;

fn read_limited(source: impl Read, keep: impl Fn(&str) -> bool, limit: u64) -> Result<Bundle> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(source));
    let (mut manifest_bytes, mut signature) = (None, None);
    // Name: (mode, SHA-256) of every file; the contents of those kept.
    let mut seen = BTreeMap::new();
    let mut members = Files::new();
    let mut left = limit;
    for entry in archive.entries().map_err(|e| Error::Device(format!("bundle: {e}")))? {
        let entry = entry.map_err(|e| Error::Device(format!("bundle: {e}")))?;
        if entry.header().entry_type() != tar::EntryType::Regular {
            return device("bundle: a member is not a regular file");
        }
        let name = entry.path().map_err(|e| Error::Device(format!("bundle: {e}")))?.to_string_lossy().into_owned();
        let mode = entry.header().mode().map_err(|e| Error::Device(format!("bundle: {name}: {e}")))?;
        let twice = || device(format!("bundle: {name} appears twice"));
        if name == MANIFEST {
            if manifest_bytes.replace(read_small(entry, MANIFEST)?).is_some() {
                return twice();
            }
            continue;
        }
        if name == SIGNATURE {
            if signature.replace(read_small(entry, SIGNATURE)?).is_some() {
                return twice();
            }
            continue;
        }
        let size = entry.size();
        if size > left {
            return device(format!("bundle: its files hold more than {} MiB", limit >> 20));
        }
        left -= size;
        // Read no further than the header says, whatever follows.
        let mut entry = entry.take(size);
        let sha = if keep(&name) {
            // Sized from the header: growing as it reads would double the largest.
            let mut contents = Vec::with_capacity(size as usize);
            entry.read_to_end(&mut contents)?;
            let sha = sha256_hex(&contents);
            members.insert(name.clone(), (mode, contents));
            sha
        } else {
            sha256_hex_of(&mut entry)?
        };
        if seen.insert(name.clone(), (mode, sha)).is_some() {
            return twice();
        }
    }
    let manifest_bytes = manifest_bytes.map_or_else(|| device("bundle: no manifest.json"), Ok)?;
    let signature = signature
        .map(|text| {
            STANDARD
                .decode(String::from_utf8_lossy(&text).trim())
                .map_err(|_| Error::Device("bundle: manifest.sig is not base64".into()))
        })
        .transpose()?;
    let manifest = parse_manifest(&manifest_bytes)?;
    if manifest.files.keys().ne(seen.keys()) {
        return device("bundle: its files are not the ones its manifest lists");
    }
    for (name, member) in &manifest.files {
        let (mode, sha) = &seen[name];
        if *sha != member.sha256 || *mode != member.mode {
            return device(format!("bundle: {name} does not match its manifest"));
        }
    }
    Ok(Bundle { manifest, manifest_bytes, signature, files: members })
}

/// A member that holds a few bytes at most: manifest.json or manifest.sig.
fn read_small(entry: impl Read, name: &str) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    entry.take(SMALL_LIMIT + 1).read_to_end(&mut data)?;
    if data.len() as u64 > SMALL_LIMIT {
        return device(format!("bundle: {name} holds more than {} KiB", SMALL_LIMIT >> 10));
    }
    Ok(data)
}

fn parse_manifest(bytes: &[u8]) -> Result<Manifest> {
    let manifest: Manifest =
        serde_json::from_slice(bytes).map_err(|e| Error::Device(format!("bundle: manifest.json: {e}")))?;
    if manifest.format != FORMAT {
        return device(format!("bundle format {} is not {FORMAT}", manifest.format));
    }
    Ok(manifest)
}

/// Where a version stands among others: vMAJOR.MINOR.PATCH, then a release
/// candidate (-rcN) before the release, and a build git describe names after
/// it (-N-gHASH, maybe -dirty) after it. None for anything else.
fn version_order(version: &str) -> Option<(u64, u64, u64, u8, u64)> {
    let version = version.trim().strip_prefix('v').unwrap_or(version.trim());
    let (numbers, rest) = version.split_once('-').map_or((version, None), |(n, r)| (n, Some(r)));
    let mut parts = numbers.split('.').map(|n| n.parse::<u64>().ok());
    let (major, minor, patch) = (parts.next()??, parts.next()??, parts.next()??);
    if parts.next().is_some() {
        return None;
    }
    let (kind, n) = match rest {
        None => (1, 0),
        Some(rest) => match rest.strip_prefix("rc") {
            Some(n) => (0, n.parse().ok()?),
            None => (2, rest.split('-').next()?.parse().ok()?),
        },
    };
    Some((major, minor, patch, kind, n))
}

/// Whether installing `new` over `installed` goes back: None when either
/// version is not one version_order places.
pub fn is_downgrade(installed: &str, new: &str) -> Option<bool> {
    Some(version_order(new)? < version_order(installed)?)
}

/// An overlay's files and directories, as pack takes them under overlays/NAME/.
pub fn add_overlay(files: &mut Files, directories: &mut BTreeMap<String, u32>, name: &str, overlay: &Overlay) {
    for (path, mode, data) in &overlay.files {
        files.insert(format!("{OVERLAYS}{name}/{path}"), (*mode, data.clone()));
    }
    for (path, mode) in &overlay.directories {
        directories.insert(format!("{OVERLAYS}{name}/{path}"), *mode);
    }
}

/// An Ed25519 key, as its 32-byte seed in base64.
pub fn signing_key(text: &str) -> Result<SigningKey> {
    let seed: [u8; 32] = STANDARD
        .decode(text.trim())
        .ok()
        .and_then(|seed| seed.try_into().ok())
        .map_or_else(|| device("not a base64 Ed25519 key seed"), Ok)?;
    Ok(SigningKey::from_bytes(&seed))
}

/// A public Ed25519 key, as its 32 bytes in base64.
pub fn verifying_key(text: &str) -> Result<VerifyingKey> {
    let bytes: [u8; 32] = STANDARD
        .decode(text.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .map_or_else(|| device("not a base64 Ed25519 public key"), Ok)?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| Error::Device("not an Ed25519 public key".into()))
}

pub fn public_key_text(key: &SigningKey) -> String {
    STANDARD.encode(key.verifying_key().to_bytes())
}

/// What one run asks of the setup, beside the bundle as options.json: the
/// part of setup::Options the bundle cannot hold. Bytes are base64.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub root_hash: String,
    pub version: String,
    pub ntp_server: Option<String>,
    pub backplate_sim: bool,
    pub block_cloud: bool,
    pub mqtt: Option<MqttSettings>,
    pub matter: Option<MatterSettings>,
    pub thread: bool,
    pub zigbee: bool,
    pub wwn_boiler: bool,
    pub doom: bool,
    /// Customizable home screen themes.
    #[serde(default)]
    pub theme: bool,
    pub external_temperature: bool,
    pub dark_theme: bool,
    pub ssh: bool,
    pub ssh_public_key: Option<String>,
    pub ssh_key_only: bool,
    pub usb_shell: bool,
    /// Updates install unsigned or signed by another key (a developer option).
    #[serde(default)]
    pub unsigned_updates: bool,
    /// The name the device goes by in Home Assistant, Matter and Zigbee.
    #[serde(default)]
    pub device_name: Option<String>,
    /// nestlocal's key set for a unit without its own.
    pub pki: Option<PkiSettings>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MqttSettings {
    pub url: String,
    pub user: Option<String>,
    pub password: Option<String>,
    pub tls: bool,
    pub ca: Option<String>,
    /// Home Assistant's entity ids start with it.
    #[serde(default)]
    pub entity_root: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatterSettings {
    pub passcode: u32,
    pub discriminator: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PkiSettings {
    pub ca: String,
    pub server_crt: String,
    pub server_key: String,
}

fn decode(field: &str, text: &str) -> Result<Vec<u8>> {
    STANDARD.decode(text).map_err(|_| Error::Device(format!("options.json: {field} is not base64")))
}

impl Settings {
    /// The settings of a run's options; their files come from the bundle.
    pub fn new(options: &Options, pki: Option<&Pki>) -> Settings {
        Settings {
            root_hash: options.root_hash.clone(),
            version: options.version.clone(),
            ntp_server: options.ntp_server.clone(),
            backplate_sim: options.backplate_sim,
            block_cloud: options.block_cloud,
            mqtt: options.mqtt.as_ref().map(|mqtt| MqttSettings {
                url: mqtt.url.clone(),
                user: mqtt.user.clone(),
                password: mqtt.password.clone(),
                tls: mqtt.tls,
                ca: mqtt.ca.as_deref().map(|ca| STANDARD.encode(ca)),
                entity_root: mqtt.entity_root.clone(),
            }),
            matter: options.matter.map(|m| MatterSettings { passcode: m.passcode, discriminator: m.discriminator }),
            thread: options.thread.is_some(),
            zigbee: options.zigbee,
            wwn_boiler: options.wwn_boiler,
            doom: options.doom.is_some(),
            theme: options.theme.is_some(),
            external_temperature: options.external_temperature,
            dark_theme: options.dark_theme,
            ssh: options.ssh,
            ssh_public_key: options.ssh_public_key.clone(),
            ssh_key_only: options.ssh_key_only,
            usb_shell: options.usb_shell,
            unsigned_updates: options.unsigned_updates,
            device_name: options.device_name.clone(),
            pki: pki.map(|pki| PkiSettings {
                ca: STANDARD.encode(&pki.ca),
                server_crt: STANDARD.encode(&pki.server_crt),
                server_key: STANDARD.encode(&pki.server_key),
            }),
        }
    }

    /// The setup's options: these settings with the bundle's files.
    pub fn options(&self, bundle: &Bundle) -> Result<Options> {
        self.take_options(&mut bundle.clone())
    }

    /// The setup's options, the files they need taken out of the bundle: a
    /// unit cannot hold them twice.
    pub fn take_options(&self, bundle: &mut Bundle) -> Result<Options> {
        let matter = self.matter.map(|m| Matter { passcode: m.passcode, discriminator: m.discriminator });
        Ok(Options {
            root_hash: self.root_hash.clone(),
            version: self.version.clone(),
            ntp_server: self.ntp_server.clone(),
            backplate_sim: self.backplate_sim,
            block_cloud: self.block_cloud,
            mqtt: self
                .mqtt
                .as_ref()
                .map(|mqtt| -> Result<Mqtt> {
                    Ok(Mqtt {
                        url: mqtt.url.clone(),
                        user: mqtt.user.clone(),
                        password: mqtt.password.clone(),
                        tls: mqtt.tls,
                        ca: mqtt.ca.as_deref().map(|ca| decode("mqtt.ca", ca)).transpose()?,
                        entity_root: mqtt.entity_root.clone(),
                    })
                })
                .transpose()?,
            matter,
            wake_filter: if matter.is_some() || self.ssh {
                bundle.take(WAKE_FILTER).map_or_else(|| device(format!("bundle: no {WAKE_FILTER}")), Ok)?
            } else {
                Vec::new()
            },
            local_cloud: bundle.take(LOCAL_CLOUD).map_or_else(|| device(format!("bundle: no {LOCAL_CLOUD}")), Ok)?,
            thread: self.thread.then(|| bundle.take_overlay(THREAD)).transpose()?,
            zigbee: self.zigbee,
            wwn_boiler: self.wwn_boiler,
            doom: self.doom.then(|| bundle.take_overlay(DOOM)).transpose()?,
            theme: self.theme.then(|| bundle.take_overlay(THEME)).transpose()?,
            external_temperature: self.external_temperature,
            // Only the gen 2's bundle has these: the gen 3 needs neither.
            external_temperature_screen: if self.external_temperature {
                bundle.take(RCS_SCREEN).unwrap_or_default()
            } else {
                Vec::new()
            },
            dark_theme: self.dark_theme,
            ssh: self.ssh,
            ssh_public_key: self.ssh_public_key.clone(),
            ssh_key_only: self.ssh_key_only,
            ssh_server: if self.ssh { bundle.take_overlay(SSH).ok() } else { None },
            unsigned_updates: self.unsigned_updates,
            // The gen 3's bundle has its engine; the gen 1's and gen 2's none.
            apply: bundle.take(APPLY).unwrap_or_default(),
            record: self.record(),
            usb_shell: self.usb_shell,
            device_name: self.device_name.clone(),
        })
    }

    /// Whether a run with these settings uses the bundle's file NAME: every
    /// file but those of the overlays it leaves out.
    pub fn wants(&self, name: &str) -> bool {
        let Some(overlay) = name.strip_prefix(OVERLAYS).and_then(|path| path.split('/').next()) else {
            return true;
        };
        match overlay {
            THREAD => self.thread,
            DOOM => self.doom,
            THEME => self.theme,
            SSH => self.ssh,
            _ => true,
        }
    }

    /// These settings as the unit keeps them: without the key set, which the
    /// unit has its own copy of once set up.
    fn record(&self) -> Vec<u8> {
        let mut record =
            serde_json::to_vec_pretty(&Settings { pki: None, ..self.clone() }).expect("settings serialize");
        record.push(b'\n');
        record
    }

    pub fn pki(&self) -> Result<Option<Pki>> {
        self.pki
            .as_ref()
            .map(|pki| {
                Ok(Pki {
                    ca: decode("pki.ca", &pki.ca)?,
                    server_crt: decode("pki.server_crt", &pki.server_crt)?,
                    server_key: decode("pki.server_key", &pki.server_key)?,
                })
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contents() -> (Files, BTreeMap<String, u32>) {
        let mut files = Files::from([
            (APPLY.to_string(), (0o755, b"\x7fELF apply".to_vec())),
            (NESTLOCAL.to_string(), (0o755, b"\x7fELF nestlocal".to_vec())),
            (WAKE_FILTER.to_string(), (0o755, b"\x7fELF wake".to_vec())),
            (LOCAL_CLOUD.to_string(), (0o755, b"\x7fELF local cloud".to_vec())),
            (PRE_SETUP.to_string(), (0o755, b"#!/bin/sh\n".to_vec())),
        ]);
        let mut directories = BTreeMap::new();
        add_overlay(
            &mut files,
            &mut directories,
            DOOM,
            &Overlay {
                files: vec![("usr/share/doom/doom1.wad".into(), 0o644, b"IWAD".to_vec())],
                directories: vec![("usr/share/doom".into(), 0o755)],
            },
        );
        add_overlay(
            &mut files,
            &mut directories,
            THEME,
            &Overlay {
                files: vec![(crate::setup::THEME_PRELOAD.into(), 0o755, b"\x7fELF preload".to_vec())],
                directories: Vec::new(),
            },
        );
        (files, directories)
    }

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    #[test]
    fn a_packed_bundle_unpacks_to_its_files_and_verifies_against_its_key_only() {
        let (files, directories) = contents();
        let data = pack(Device::Diamond3, "1.2.3", &files, &directories, Some(&key(1)));
        assert_eq!(data, pack(Device::Diamond3, "1.2.3", &files, &directories, Some(&key(1))));
        let bundle = Bundle::unpack(&data).unwrap();
        assert_eq!((bundle.manifest.device.as_str(), bundle.manifest.version.as_str()), ("diamond3", "1.2.3"));
        assert_eq!(bundle.files, files);
        bundle.verify(&key(1).verifying_key()).unwrap();
        assert!(bundle.verify(&key(2).verifying_key()).unwrap_err().to_string().contains("bad signature"));
        let unsigned = Bundle::unpack(&pack(Device::Diamond3, "1.2.3", &files, &directories, None)).unwrap();
        assert!(unsigned.verify(&key(1).verifying_key()).unwrap_err().to_string().contains("unsigned"));
    }

    #[test]
    fn the_release_key_is_a_public_key() {
        verifying_key(RELEASE_KEY).unwrap();
    }

    #[test]
    fn versions_go_candidates_then_release_then_later_builds() {
        let order = [
            "v0.4.2",
            "v0.4.3-rc1",
            "v0.4.3-rc2",
            "v0.4.3",
            "v0.4.3-1-gabc1234",
            "v0.4.3-12-gdef5678-dirty",
            "v0.5.0",
            "v1.0.0",
        ];
        for pair in order.windows(2) {
            assert_eq!(is_downgrade(pair[1], pair[0]), Some(true), "{} after {}", pair[0], pair[1]);
            assert_eq!(is_downgrade(pair[0], pair[1]), Some(false), "{} after {}", pair[1], pair[0]);
        }
        assert_eq!(is_downgrade("v0.4.3", "v0.4.3"), Some(false));
        assert_eq!(is_downgrade("v0.4.3", "0.4.3"), Some(false));
        assert_eq!(is_downgrade("(version unknown)", "v0.4.3"), None);
        assert_eq!(is_downgrade("v0.4.3", "dev"), None);
    }

    #[test]
    fn keys_round_trip_through_their_text() {
        let text = STANDARD.encode([7u8; 32]);
        let key = signing_key(&text).unwrap();
        assert_eq!(verifying_key(&public_key_text(&key)).unwrap(), key.verifying_key());
        assert!(signing_key("short").is_err());
    }

    /// A bundle whose tar holds these members, without a manifest of its own making.
    fn raw(members: &[(&str, u32, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (name, mode, data) in members {
            let mut header = tar::Header::new_ustar();
            header.set_size(data.len() as u64);
            header.set_mode(*mode);
            tar.append_data(&mut header, name, *data).unwrap();
        }
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(&tar.into_inner().unwrap()).unwrap();
        gzip.finish().unwrap()
    }

    #[test]
    fn a_file_that_differs_from_the_manifest_or_is_not_listed_is_refused() {
        let manifest = |sha: &str| {
            serde_json::to_vec(&Manifest {
                format: FORMAT,
                device: "diamond3".into(),
                version: "1".into(),
                files: BTreeMap::from([("a".to_string(), Member { sha256: sha.into(), mode: 0o644 })]),
                directories: BTreeMap::new(),
            })
            .unwrap()
        };
        let good = manifest(&sha256_hex(b"a"));
        Bundle::unpack(&raw(&[(MANIFEST, 0o644, &good), ("a", 0o644, b"a")])).unwrap();
        let error = |members: &[(&str, u32, &[u8])]| Bundle::unpack(&raw(members)).err().unwrap().to_string();
        assert!(error(&[(MANIFEST, 0o644, &good), ("a", 0o644, b"b")]).contains("does not match"));
        assert!(error(&[(MANIFEST, 0o644, &good), ("a", 0o755, b"a")]).contains("does not match"));
        assert!(error(&[(MANIFEST, 0o644, &good), ("a", 0o644, b"a"), ("b", 0o644, b"")]).contains("not the ones"));
        assert!(error(&[(MANIFEST, 0o644, &good)]).contains("not the ones"));
        assert!(error(&[("a", 0o644, b"a")]).contains("no manifest"));
    }

    #[test]
    fn settings_carry_a_runs_options_and_take_their_files_from_the_bundle() {
        let (files, directories) = contents();
        let bundle = Bundle::unpack(&pack(Device::Diamond3, "1", &files, &directories, None)).unwrap();
        let options = Options {
            root_hash: "$6$x".into(),
            version: "1".into(),
            block_cloud: true,
            mqtt: Some(Mqtt {
                url: "tcp://broker:1883".into(),
                user: Some("u".into()),
                password: Some("p".into()),
                tls: true,
                entity_root: Some("nest_hall".into()),
                ca: Some(b"-----BEGIN CERTIFICATE-----\n".to_vec()),
            }),
            matter: Some(Matter { passcode: 20202021, discriminator: 3840 }),
            wake_filter: b"\x7fELF wake".to_vec(),
            local_cloud: b"\x7fELF local cloud".to_vec(),
            doom: Some(bundle.overlay(DOOM).unwrap()),
            theme: Some(bundle.overlay(THEME).unwrap()),
            ssh: true,
            usb_shell: true,
            unsigned_updates: true,
            device_name: Some("Hall".into()),
            apply: b"\x7fELF apply".to_vec(),
            ..Options::default()
        };
        let pki = Pki { ca: b"ca".to_vec(), server_crt: b"crt".to_vec(), server_key: b"key".to_vec() };
        let settings = Settings::new(&options, Some(&pki));
        let settings: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(settings.pki().unwrap(), Some(pki));
        let rebuilt = settings.options(&bundle).unwrap();
        let kept: Settings = serde_json::from_slice(&rebuilt.record).unwrap();
        assert_eq!(kept, Settings { pki: None, ..settings.clone() });
        assert_eq!(format!("{:?}", Options { record: Vec::new(), ..rebuilt.clone() }), format!("{options:?}"));
        assert_eq!(rebuilt.doom.unwrap().directories, [("usr/share/doom".to_string(), 0o755)]);
        assert_eq!(rebuilt.theme.unwrap().files[0].0, crate::setup::THEME_PRELOAD);
        let thread = Settings { thread: true, ..Settings::default() };
        assert!(thread.options(&bundle).unwrap_err().to_string().contains("no thread overlay"));
    }

    #[test]
    fn a_gen_2_bundle_brings_its_ssh_server_and_sensors_view_only_when_asked() {
        let (mut files, mut directories) = contents();
        files.insert(RCS_SCREEN.into(), (0o644, b"\x7fELF rcs".to_vec()));
        add_overlay(
            &mut files,
            &mut directories,
            SSH,
            &Overlay {
                files: vec![("usr/sbin/dropbearmulti".into(), 0o755, b"DROPBEAR".to_vec())],
                directories: vec![],
            },
        );
        let bundle = Bundle::from_files(Device::J49, "1", files.clone(), &directories);
        assert_eq!(
            bundle.manifest,
            Bundle::unpack(&pack(Device::J49, "1", &files, &directories, None)).unwrap().manifest
        );
        let asked = Settings { ssh: true, external_temperature: true, ..Settings::default() }.options(&bundle).unwrap();
        assert_eq!(asked.external_temperature_screen, b"\x7fELF rcs");
        assert_eq!(asked.ssh_server.unwrap().files[0].0, "usr/sbin/dropbearmulti");
        // SSH wakes the unit through nlsleepd's library.
        assert_eq!(asked.wake_filter, b"\x7fELF wake");
        let not = Settings::default().options(&bundle).unwrap();
        assert!(not.external_temperature_screen.is_empty() && not.ssh_server.is_none() && not.wake_filter.is_empty());
    }

    #[test]
    fn a_bundle_read_in_part_keeps_the_files_asked_for_and_checks_them_all() {
        let (files, directories) = contents();
        let data = pack(Device::J49, "1", &files, &directories, Some(&key(1)));
        let bundle = Bundle::read_some(&data[..], |name| name == APPLY).unwrap();
        assert_eq!(bundle.files.keys().collect::<Vec<_>>(), [APPLY]);
        assert_eq!(bundle.manifest.files.len(), files.len());
        bundle.verify(&key(1).verifying_key()).unwrap();
        assert_eq!(Bundle::read_manifest(&data[..]).unwrap(), bundle.manifest);

        let manifest = serde_json::to_vec(&Manifest {
            format: FORMAT,
            device: "j49".into(),
            version: "1".into(),
            files: BTreeMap::from([
                ("a".to_string(), Member { sha256: sha256_hex(b"a"), mode: 0o644 }),
                ("b".to_string(), Member { sha256: sha256_hex(b"b"), mode: 0o644 }),
            ]),
            directories: BTreeMap::new(),
        })
        .unwrap();
        let tampered = raw(&[(MANIFEST, 0o644, &manifest), ("a", 0o644, b"a"), ("b", 0o644, b"c")]);
        let error = Bundle::read_some(&tampered[..], |name| name == "a").err().unwrap().to_string();
        assert!(error.contains("b does not match"), "{error}");
    }

    #[test]
    fn a_bundle_whose_files_hold_more_than_the_limit_is_refused() {
        let manifest = serde_json::to_vec(&Manifest {
            format: FORMAT,
            device: "j49".into(),
            version: "1".into(),
            files: BTreeMap::from([("a".to_string(), Member { sha256: sha256_hex(b"aa"), mode: 0o644 })]),
            directories: BTreeMap::new(),
        })
        .unwrap();
        let data = raw(&[(MANIFEST, 0o644, &manifest), ("a", 0o644, b"aa")]);
        read_limited(&data[..], |_| true, 2).unwrap();
        let error = read_limited(&data[..], |_| true, 1).err().unwrap().to_string();
        assert!(error.contains("hold more than"), "{error}");
    }

    #[test]
    fn a_run_wants_the_overlays_it_asks_for_and_every_other_file() {
        let settings = Settings { doom: true, ..Settings::default() };
        assert!(settings.wants(APPLY) && settings.wants(NESTLOCAL) && settings.wants(PRE_SETUP));
        assert!(settings.wants("overlays/doom/usr/share/doom/doom1.wad"));
        assert!(!settings.wants("overlays/thread/nestlabs/sbin/ot-daemon"));
        assert!(!settings.wants("overlays/ssh/usr/sbin/dropbearmulti"));
        assert!(!settings.wants("overlays/theme/x"));
    }
}
