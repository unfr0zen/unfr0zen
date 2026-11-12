//! The patch bundle: one gzipped tar holding everything a patch installs
//! (nestlocal, its overlays, the UNFR0ZEN screen's object, the scripts run
//! around the patch) and unfr0zen-apply, which runs the patch on the unit.
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
use crate::patch::Options;
use crate::shell::sha256_hex;
use crate::{device, Device, Error, Result};

pub const FORMAT: u32 = 1;
pub const MANIFEST: &str = "manifest.json";
pub const SIGNATURE: &str = "manifest.sig";
/// The patch engine, a static ARMv7 binary.
pub const APPLY: &str = "bin/unfr0zen-apply";
pub const NESTLOCAL: &str = "assets/nestlocal";
pub const WAKE_FILTER: &str = "assets/libwakefilter.so";
/// Preloaded into nlclient on a unit running nestlocal (patch::Options::local_cloud).
pub const LOCAL_CLOUD: &str = "assets/liblocalcloud.so";
pub const SCREEN: &str = "assets/custom-screen.o";
/// The gen 2's native Sensors view helper, for the external temperature.
pub const RCS_SCREEN: &str = "assets/j49-rcs-screen.o";
/// Run before the patch reads the volume; failing, it stops the run with
/// nothing written.
pub const PRE_PATCH: &str = "scripts/pre-patch.sh";
/// Run after the patch's writes are verified on flash; failing, it fails the run.
pub const POST_PATCH: &str = "scripts/post-patch.sh";
/// overlays/NAME/PATH installs PATH on the volume when the run asks for NAME.
const OVERLAYS: &str = "overlays/";
pub const THREAD: &str = "thread";
pub const DOOM: &str = "doom";
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
pub struct Bundle {
    pub manifest: Manifest,
    manifest_bytes: Vec<u8>,
    signature: Option<Vec<u8>>,
    pub files: Files,
}

/// The bundle as gzipped tar: manifest.json, manifest.sig when signed, then
/// the files in name order. The same inputs give the same bytes.
pub fn pack(device: Device, version: &str, files: &Files, directories: &BTreeMap<String, u32>,
            key: Option<&SigningKey>) -> Vec<u8> {
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
        files: files.iter().map(|(name, (mode, data))| (name.clone(), Member { sha256: sha256_hex(data), mode: *mode })).collect(),
        directories: directories.clone(),
    };
    let mut bytes = serde_json::to_vec_pretty(&manifest).expect("a manifest serializes");
    bytes.push(b'\n');
    (manifest, bytes)
}

impl Bundle {
    /// The bundle of these files as unpack would give it, without packing it:
    /// for a run patching on the host.
    pub fn from_files(device: Device, version: &str, files: Files, directories: &BTreeMap<String, u32>) -> Bundle {
        let (manifest, manifest_bytes) = manifest(device, version, &files, directories);
        Bundle { manifest, manifest_bytes, signature: None, files }
    }

    /// Unpacks a bundle and checks its files against its manifest: every one
    /// listed, present, and unchanged. The signature is checked by verify.
    pub fn unpack(data: &[u8]) -> Result<Bundle> {
        let mut tar = Vec::new();
        flate2::read::GzDecoder::new(data).read_to_end(&mut tar).map_err(|e| Error::Device(format!("bundle: {e}")))?;
        let mut archive = tar::Archive::new(&tar[..]);
        let mut members = Files::new();
        for entry in archive.entries().map_err(|e| Error::Device(format!("bundle: {e}")))? {
            let mut entry = entry.map_err(|e| Error::Device(format!("bundle: {e}")))?;
            if entry.header().entry_type() != tar::EntryType::Regular {
                return device("bundle: a member is not a regular file");
            }
            let name = entry.path().map_err(|e| Error::Device(format!("bundle: {e}")))?.to_string_lossy().into_owned();
            let mode = entry.header().mode().map_err(|e| Error::Device(format!("bundle: {name}: {e}")))?;
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents)?;
            if members.insert(name.clone(), (mode, contents)).is_some() {
                return device(format!("bundle: {name} appears twice"));
            }
        }
        let (_, manifest_bytes) = members.remove(MANIFEST).map_or_else(|| device("bundle: no manifest.json"), Ok)?;
        let signature = members.remove(SIGNATURE).map(|(_, text)| {
            STANDARD.decode(String::from_utf8_lossy(&text).trim()).map_err(|_| Error::Device("bundle: manifest.sig is not base64".into()))
        }).transpose()?;
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| Error::Device(format!("bundle: manifest.json: {e}")))?;
        if manifest.format != FORMAT {
            return device(format!("bundle format {} is not {FORMAT}", manifest.format));
        }
        if manifest.files.keys().ne(members.keys()) {
            return device("bundle: its files are not the ones its manifest lists");
        }
        for (name, member) in &manifest.files {
            let (mode, data) = &members[name];
            if sha256_hex(data) != member.sha256 || *mode != member.mode {
                return device(format!("bundle: {name} does not match its manifest"));
            }
        }
        Ok(Bundle { manifest, manifest_bytes, signature, files: members })
    }

    /// Checks the manifest's signature against a trusted key.
    pub fn verify(&self, key: &VerifyingKey) -> Result<()> {
        let signature = self.signature.as_deref().map_or_else(|| device("bundle: unsigned"), Ok)?;
        let signature = Signature::from_slice(signature).map_err(|_| Error::Device("bundle: malformed signature".into()))?;
        key.verify(&self.manifest_bytes, &signature).map_err(|_| Error::Device("bundle: bad signature".into()))
    }

    pub fn file(&self, name: &str) -> Result<&[u8]> {
        self.files.get(name).map(|(_, data)| data.as_slice()).map_or_else(|| device(format!("bundle: no {name}")), Ok)
    }

    /// The overlay under overlays/NAME/, if the bundle holds one.
    fn optional_overlay(&self, name: &str) -> Option<Overlay> {
        self.overlay(name).ok()
    }

    /// The overlay the bundle holds under overlays/NAME/.
    pub fn overlay(&self, name: &str) -> Result<Overlay> {
        let prefix = format!("{OVERLAYS}{name}/");
        let files: Vec<_> = self.files.iter().filter_map(|(path, (mode, data))| {
            path.strip_prefix(&prefix).map(|path| (path.to_string(), *mode, data.clone()))
        }).collect();
        if files.is_empty() {
            return device(format!("bundle: no {name} overlay"));
        }
        let directories = self.manifest.directories.iter()
            .filter_map(|(path, mode)| path.strip_prefix(&prefix).map(|path| (path.to_string(), *mode))).collect();
        Ok(Overlay { files, directories })
    }
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
    let seed: [u8; 32] = STANDARD.decode(text.trim()).ok().and_then(|seed| seed.try_into().ok())
        .map_or_else(|| device("not a base64 Ed25519 key seed"), Ok)?;
    Ok(SigningKey::from_bytes(&seed))
}

/// A public Ed25519 key, as its 32 bytes in base64.
pub fn verifying_key(text: &str) -> Result<VerifyingKey> {
    let bytes: [u8; 32] = STANDARD.decode(text.trim()).ok().and_then(|bytes| bytes.try_into().ok())
        .map_or_else(|| device("not a base64 Ed25519 public key"), Ok)?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| Error::Device("not an Ed25519 public key".into()))
}

pub fn public_key_text(key: &SigningKey) -> String {
    STANDARD.encode(key.verifying_key().to_bytes())
}

/// What one run asks of the patch, beside the bundle as options.json: the
/// part of patch::Options the bundle cannot hold. Bytes are base64.
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
    pub external_temperature: bool,
    pub dark_theme: bool,
    pub ssh: bool,
    pub ssh_public_key: Option<String>,
    pub ssh_key_only: bool,
    pub usb_shell: bool,
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
                url: mqtt.url.clone(), user: mqtt.user.clone(), password: mqtt.password.clone(), tls: mqtt.tls,
                ca: mqtt.ca.as_deref().map(|ca| STANDARD.encode(ca)),
                entity_root: mqtt.entity_root.clone(),
            }),
            matter: options.matter.map(|m| MatterSettings { passcode: m.passcode, discriminator: m.discriminator }),
            thread: options.thread.is_some(),
            zigbee: options.zigbee,
            wwn_boiler: options.wwn_boiler,
            doom: options.doom.is_some(),
            external_temperature: options.external_temperature,
            dark_theme: options.dark_theme,
            ssh: options.ssh,
            ssh_public_key: options.ssh_public_key.clone(),
            ssh_key_only: options.ssh_key_only,
            usb_shell: options.usb_shell,
            device_name: options.device_name.clone(),
            pki: pki.map(|pki| PkiSettings {
                ca: STANDARD.encode(&pki.ca), server_crt: STANDARD.encode(&pki.server_crt),
                server_key: STANDARD.encode(&pki.server_key),
            }),
        }
    }

    /// The patch's options: these settings with the bundle's files.
    pub fn options(&self, bundle: &Bundle) -> Result<Options> {
        let matter = self.matter.map(|m| Matter { passcode: m.passcode, discriminator: m.discriminator });
        Ok(Options {
            root_hash: self.root_hash.clone(),
            version: self.version.clone(),
            ntp_server: self.ntp_server.clone(),
            backplate_sim: self.backplate_sim,
            block_cloud: self.block_cloud,
            mqtt: self.mqtt.as_ref().map(|mqtt| -> Result<Mqtt> {
                Ok(Mqtt {
                    url: mqtt.url.clone(), user: mqtt.user.clone(), password: mqtt.password.clone(), tls: mqtt.tls,
                    ca: mqtt.ca.as_deref().map(|ca| decode("mqtt.ca", ca)).transpose()?,
                    entity_root: mqtt.entity_root.clone(),
                })
            }).transpose()?,
            matter,
            wake_filter: if matter.is_some() { bundle.file(WAKE_FILTER)?.to_vec() } else { Vec::new() },
            local_cloud: bundle.file(LOCAL_CLOUD)?.to_vec(),
            thread: self.thread.then(|| bundle.overlay(THREAD)).transpose()?,
            zigbee: self.zigbee,
            wwn_boiler: self.wwn_boiler,
            doom: self.doom.then(|| bundle.overlay(DOOM)).transpose()?,
            external_temperature: self.external_temperature,
            // Only the gen 2's bundle has these: the gen 3 needs neither.
            external_temperature_screen: if self.external_temperature {
                bundle.files.get(RCS_SCREEN).map(|(_, data)| data.clone()).unwrap_or_default()
            } else {
                Vec::new()
            },
            dark_theme: self.dark_theme,
            ssh: self.ssh,
            ssh_public_key: self.ssh_public_key.clone(),
            ssh_key_only: self.ssh_key_only,
            ssh_server: if self.ssh { bundle.optional_overlay(SSH) } else { None },
            record: self.record(),
            usb_shell: self.usb_shell,
            device_name: self.device_name.clone(),
        })
    }

    /// These settings as the unit keeps them: without the key set, which the
    /// unit has its own copy of once patched.
    fn record(&self) -> Vec<u8> {
        let mut record = serde_json::to_vec_pretty(&Settings { pki: None, ..self.clone() }).expect("settings serialize");
        record.push(b'\n');
        record
    }

    pub fn pki(&self) -> Result<Option<Pki>> {
        self.pki.as_ref().map(|pki| Ok(Pki {
            ca: decode("pki.ca", &pki.ca)?,
            server_crt: decode("pki.server_crt", &pki.server_crt)?,
            server_key: decode("pki.server_key", &pki.server_key)?,
        })).transpose()
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
            (PRE_PATCH.to_string(), (0o755, b"#!/bin/sh\n".to_vec())),
        ]);
        let mut directories = BTreeMap::new();
        add_overlay(&mut files, &mut directories, DOOM, &Overlay {
            files: vec![("usr/share/doom/doom1.wad".into(), 0o644, b"IWAD".to_vec())],
            directories: vec![("usr/share/doom".into(), 0o755)],
        });
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
        let manifest = |sha: &str| serde_json::to_vec(&Manifest {
            format: FORMAT, device: "diamond3".into(), version: "1".into(),
            files: BTreeMap::from([("a".to_string(), Member { sha256: sha.into(), mode: 0o644 })]),
            directories: BTreeMap::new(),
        }).unwrap();
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
            root_hash: "$6$x".into(), version: "1".into(), block_cloud: true,
            mqtt: Some(Mqtt { url: "tcp://broker:1883".into(), user: Some("u".into()), password: Some("p".into()), tls: true,
                              entity_root: Some("nest_hall".into()),
                              ca: Some(b"-----BEGIN CERTIFICATE-----\n".to_vec()) }),
            matter: Some(Matter { passcode: 20202021, discriminator: 3840 }),
            wake_filter: b"\x7fELF wake".to_vec(),
            local_cloud: b"\x7fELF local cloud".to_vec(),
            doom: Some(bundle.overlay(DOOM).unwrap()),
            ssh: true, usb_shell: true, device_name: Some("Hall".into()),
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
        let thread = Settings { thread: true, ..Settings::default() };
        assert!(thread.options(&bundle).unwrap_err().to_string().contains("no thread overlay"));
    }

    #[test]
    fn a_gen_2_bundle_brings_its_ssh_server_and_sensors_view_only_when_asked() {
        let (mut files, mut directories) = contents();
        files.insert(RCS_SCREEN.into(), (0o644, b"\x7fELF rcs".to_vec()));
        add_overlay(&mut files, &mut directories, SSH, &Overlay {
            files: vec![("usr/sbin/dropbearmulti".into(), 0o755, b"DROPBEAR".to_vec())], directories: vec![],
        });
        let bundle = Bundle::from_files(Device::J49, "1", files.clone(), &directories);
        assert_eq!(bundle.manifest, Bundle::unpack(&pack(Device::J49, "1", &files, &directories, None)).unwrap().manifest);
        let asked = Settings { ssh: true, external_temperature: true, ..Settings::default() }.options(&bundle).unwrap();
        assert_eq!(asked.external_temperature_screen, b"\x7fELF rcs");
        assert_eq!(asked.ssh_server.unwrap().files[0].0, "usr/sbin/dropbearmulti");
        let not = Settings::default().options(&bundle).unwrap();
        assert!(not.external_temperature_screen.is_empty() && not.ssh_server.is_none());
    }
}
