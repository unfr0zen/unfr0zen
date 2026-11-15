//! A patched unit's settings, read back from the files the patch wrote: the
//! inverse of set_up_on_device, so that a run over Wi-Fi starts from what the
//! unit has and changes only what it is asked to.


use regex::Regex;

use crate::bundle::Settings;
use crate::nestlocal::{self, Matter, Mqtt};
use crate::setup::{ssh_public_key, wwn_block, Tree, AUTHORIZED_KEYS, BACKPLATE_SIM, HOSTS_BEGIN, NTP_LINES, RECORD};
use crate::{device, Device, Result};

/// What a unit's files say a run set. Secrets are in it (the MQTT password,
/// the root hash): it is read over the sealed session and never logged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnitSettings {
    /// The broker, with its password and CA.
    pub mqtt: Option<Mqtt>,
    /// The exact passcode and discriminator: the pairing code would keep
    /// only the discriminator's top 4 bits.
    pub matter: Option<Matter>,
    pub zigbee: bool,
    pub matter_thread: bool,
    pub doom: bool,
    /// The gen 3's customizable home screen themes.
    pub themes: bool,
    pub external_temperature: bool,
    pub update_api: bool,
    /// The name MQTT and Matter give the unit, as nestlocal's file holds it.
    pub device_name: Option<String>,
    /// The time server the unit's ntpd or connman names.
    pub ntp_server: Option<String>,
    pub backplate_sim: bool,
    pub block_cloud: bool,
    pub wwn_boiler: bool,
    pub ssh: bool,
    /// SSH refuses root's password: only a key logs in.
    pub ssh_key_only: bool,
    /// The key a run authorized for root: the last run's record says, else
    /// the first in root's authorized_keys, with SSH on.
    pub ssh_public_key: Option<String>,
    /// Updates install unsigned or signed by another key: only the last
    /// run's record says.
    pub unsigned_updates: bool,
    pub usb_shell: bool,
    /// None here: the gen 2's patched nlclient tells, which the host settles
    /// by rebuilding it from its original.
    pub dark_theme: Option<bool>,
    /// root's crypt hash in etc/shadow; None for a locked or empty password.
    pub root_hash: Option<String>,
}

/// Every file unit_settings reads, on any generation, for one batched read.
pub const SETTINGS_FILES: &[&str] = &[
    nestlocal::OPTIONS, nestlocal::PASSWORD_FILE, nestlocal::CA_FILE, nestlocal::NAME_FILE, NTP_LINES[0].0, NTP_LINES[1].0,
    "etc/nestlabs/client.config", "etc/hosts", "etc/init.d/rcS", "etc/shadow", "etc/inittab",
    "etc/init.d/usb-gadget", "etc/ssh/sshd_config", crate::j49::rootfs::SSH_INIT, AUTHORIZED_KEYS, RECORD,
];

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid pattern")
}

/// A file's text, None when the tree does not have it.
fn file(tree: &Tree, name: &str) -> Result<Option<String>> {
    tree.get(name).map(|(data, _)| String::from_utf8(data.clone()).or_else(|_| device(format!("{name} is not text"))))
        .transpose()
}

/// A device's reader of one of its root filesystem patch's settings.
type Reader = fn(&Tree) -> bool;

/// The unit's settings from its files (`tree`, SETTINGS_FILES at least): a
/// file it lacks reads as its setting off. nestlocal options this cannot
/// read back are refused, so that a run never drops one unasked.
pub fn unit_settings(tree: &Tree, target: Device) -> Result<UnitSettings> {
    let (ssh, ssh_key_only, usb_shell): (Reader, Reader, Reader) = match target {
        Device::Diamond3 => (crate::diamond3::rootfs::ssh, crate::diamond3::rootfs::ssh_key_only, crate::diamond3::rootfs::usb_shell),
        Device::Diamond | Device::J49 => (crate::j49::rootfs::ssh, crate::j49::rootfs::ssh_key_only, crate::j49::rootfs::usb_shell),
        Device::Onyx => return device("Nest Thermostat E (onyx) is not supported"),
    };
    let mut settings = match file(tree, nestlocal::OPTIONS)? {
        Some(line) => options(tree, &line)?,
        None => UnitSettings::default(),
    };
    settings.ntp_server = ntp_server(tree)?;
    if let Some(config) = file(tree, "etc/nestlabs/client.config")? {
        settings.backplate_sim = BACKPLATE_SIM.iter().all(|key| config.contains(&format!(r#"<a key="{key}" value="1"/>"#)));
    }
    settings.block_cloud = file(tree, "etc/hosts")?.is_some_and(|hosts| hosts.contains(HOSTS_BEGIN));
    settings.wwn_boiler = file(tree, "etc/init.d/rcS")?.is_some_and(|rcs| rcs.contains(&wwn_block(true)));
    settings.ssh = ssh(tree);
    settings.ssh_key_only = ssh_key_only(tree);
    settings.usb_shell = usb_shell(tree);
    // What the files cannot tell, the last run's record does; a record that
    // does not parse is passed over, as a unit patched before it was kept.
    let record = file(tree, RECORD)?.and_then(|text| serde_json::from_str::<Settings>(&text).ok());
    // The time server as the run asked for it: its files cannot tell the
    // default, or Nest's own left in place, from one named.
    if let Some(record) = &record {
        settings.ntp_server = record.ntp_server.clone();
    }
    settings.unsigned_updates = record.as_ref().is_some_and(|record| record.unsigned_updates);
    settings.ssh_public_key = match record.and_then(|record| record.ssh_public_key) {
        Some(key) => Some(key),
        None if settings.ssh => file(tree, AUTHORIZED_KEYS)?.and_then(|keys| {
            keys.lines().find_map(|line| ssh_public_key(line.as_bytes()).ok())
        }),
        None => None,
    }.filter(|_| settings.ssh);
    settings.root_hash = file(tree, "etc/shadow")?.and_then(|shadow| {
        shadow.lines().find_map(|line| line.strip_prefix("root:")).and_then(|rest| rest.split(':').next())
            .filter(|hash| hash.starts_with('$')).map(str::to_string)
    });
    Ok(settings)
}

/// The server of the first of ntpd's and connman's files the unit has.
fn ntp_server(tree: &Tree) -> Result<Option<String>> {
    for (name, prefix) in NTP_LINES {
        if let Some(content) = file(tree, name)? {
            let line = regex(&format!(r"(?m)^{}(\S+)$", regex::escape(prefix)));
            return Ok(line.captures(&content).map(|found| found[1].to_string()));
        }
    }
    Ok(None)
}

/// nestlocal's settings from its options line (nestlocal::options and the
/// flags set_up_on_device appends), and the files it names.
fn options(tree: &Tree, line: &str) -> Result<UnitSettings> {
    let mut settings = UnitSettings::default();
    let (mut url, mut user, mut password, mut tls, mut ca) = (String::new(), None, None, false, None);
    let (mut matter, mut passcode, mut discriminator, mut entity_root) = (false, None, None, None);
    let named = |path: &str, file: &str| -> Result<Vec<u8>> {
        if path.strip_prefix('/') != Some(file) {
            return device(format!("{}: {path} is not unfr0zen's {file}", nestlocal::OPTIONS));
        }
        tree.get(file).map(|(data, _)| data.clone())
            .map_or_else(|| device(format!("{}: names {file}, which the unit does not have", nestlocal::OPTIONS)), Ok)
    };
    let mut words = line.split_whitespace();
    while let Some(word) = words.next() {
        let (flag, inline) = word.split_once('=').map_or((word, None), |(flag, value)| (flag, Some(value)));
        let mut value = || inline.or_else(|| words.next())
            .map_or_else(|| device(format!("{}: {flag} without a value", nestlocal::OPTIONS)), Ok);
        let number = |value: &str| -> Result<u64> {
            value.parse().or_else(|_| device(format!("{}: {flag} {value:?} is not a number", nestlocal::OPTIONS)))
        };
        match flag {
            "-mqtt" => url = value()?.to_string(),
            "-mqtt-user" => user = Some(value()?.to_string()),
            "-mqtt-password-file" => {
                let data = named(value()?, nestlocal::PASSWORD_FILE)?;
                let text = String::from_utf8(data).or_else(|_| device(format!("{} is not text", nestlocal::PASSWORD_FILE)))?;
                password = Some(text.strip_suffix('\n').unwrap_or(&text).to_string());
            }
            "-mqtt-tls" => tls = true,
            "-mqtt-ca" => ca = Some(named(value()?, nestlocal::CA_FILE)?),
            "-matter" => matter = true,
            "-matter-passcode" => passcode = Some(number(value()?)?),
            "-matter-discriminator" => discriminator = Some(number(value()?)?),
            "-matter-thread" => settings.matter_thread = true,
            "-zigbee" => settings.zigbee = true,
            "-update-listen" => settings.update_api = !value()?.is_empty(),
            "-ha-entity-root" => entity_root = Some(value()?.to_string()),
            "-device-name-file" => {
                let data = named(value()?, nestlocal::NAME_FILE)?;
                let text = String::from_utf8(data).or_else(|_| device(format!("{} is not text", nestlocal::NAME_FILE)))?;
                settings.device_name = Some(text.trim().to_string()).filter(|name| !name.is_empty());
            }
            "-doom" => settings.doom = true,
            "-themes" => settings.themes = true,
            "-external-temperature" => settings.external_temperature = true,
            _ => return device(format!("{}: {flag} is not an option unfr0zen sets", nestlocal::OPTIONS)),
        }
    }
    if !url.is_empty() {
        settings.mqtt = Some(Mqtt { url, user, password, tls, ca, entity_root });
    }
    if matter {
        let (Some(passcode), Some(discriminator)) = (passcode, discriminator) else {
            return device(format!("{}: -matter without its passcode and discriminator", nestlocal::OPTIONS));
        };
        settings.matter = Some(Matter { passcode: u32::try_from(passcode).or_else(|_| device("Matter passcode out of range"))?,
                                        discriminator: u16::try_from(discriminator).or_else(|_| device("Matter discriminator out of range"))? });
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nestlocal::{self, Matter, Mqtt, Overlay, Pki};
    use crate::setup::tests::{nestlocal_unit, options, stock, FakeVolume};
    use crate::setup::{set_up_on_device, Options};

    /// The volume's files, as a remote read gives them.
    fn tree(volume: &FakeVolume) -> Tree {
        volume.files.iter().map(|(name, (_, mode, data))| (name.clone(), (data.clone(), *mode))).collect()
    }

    fn patched(mut volume: FakeVolume, device: Device, options: &Options, pki: Option<&Pki>) -> Tree {
        let pki = pki.cloned();
        set_up_on_device(&mut volume, device, options, b"ARMV7", &mut || Ok(pki.clone().expect("the unit's own keys")),
                        false, &|source, sections| crate::nlclient::build(source, b"", sections)).unwrap();
        tree(&volume)
    }

    /// What the unit should read back as after a run with these options.
    fn expected(options: &Options) -> UnitSettings {
        UnitSettings {
            mqtt: options.mqtt.clone().map(|mqtt| Mqtt { tls: mqtt.tls || mqtt.ca.is_some(), ..mqtt }),
            matter: options.matter,
            zigbee: options.zigbee,
            matter_thread: options.thread.is_some(),
            doom: options.doom.is_some(),
            themes: options.theme.is_some(),
            external_temperature: options.external_temperature,
            update_api: options.update_api,
            device_name: options.device_name.clone(),
            ntp_server: options.ntp_server.clone(),
            backplate_sim: options.backplate_sim,
            block_cloud: options.block_cloud,
            wwn_boiler: options.wwn_boiler,
            ssh: options.ssh,
            ssh_key_only: options.ssh_key_only,
            ssh_public_key: options.ssh_public_key.clone(),
            unsigned_updates: !options.record.is_empty() && options.unsigned_updates,
            usb_shell: options.usb_shell,
            dark_theme: None,
            root_hash: Some(options.root_hash.clone()),
        }
    }

    const CA: &[u8] = b"-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";

    #[test]
    fn settings_read_back_what_the_patch_wrote() {
        let pki = nestlocal::generate_pki().unwrap();
        let matter = Matter { passcode: 20202021, discriminator: 1234 };
        let thread = Overlay { files: vec![("nestlabs/sbin/ot-daemon".into(), 0o755, b"DAEMON".to_vec())], directories: Vec::new() };
        let doom = Overlay { files: vec![("nestlabs/sbin/doom".into(), 0o755, b"DOOM".to_vec())], directories: Vec::new() };
        let mqtt = Mqtt { url: "tcp://broker.lan:8883".into(), user: Some("thermostat".into()), password: Some("s3cret pass".into()),
                          tls: true, ca: Some(CA.to_vec()), entity_root: Some("nest_hall".into()) };
        let key = crate::setup::tests::public_key("ssh-ed25519", &[&[1; 32]]);
        let recorded = |options: Options| Options { record: crate::bundle::Settings::new(&options, None).record(), ..options };
        let cases = [
            Options { mqtt: Some(mqtt.clone()), update_api: true, device_name: Some("Hall <serial>".into()), ..options() },
            Options { matter: Some(matter), device_name: Some("Hall".into()), ..options() },
            Options { ssh: true, ssh_key_only: true, ssh_public_key: Some(key.clone()), ..options() },
            recorded(Options { mqtt: Some(mqtt), unsigned_updates: true, ssh: true, ssh_public_key: Some(key), ..options() }),
            Options { mqtt: Some(Mqtt { url: "tcp://broker.lan:1883".into(), ..Mqtt::default() }), ..options() },
            Options { matter: Some(matter), ..options() },
            Options { zigbee: true, wwn_boiler: true, doom: Some(doom), external_temperature: true, update_api: true, ..options() },
            Options { block_cloud: true, ntp_server: Some("time.lan".into()), backplate_sim: true, ..options() },
            Options { ssh: true, usb_shell: true, ..options() },
            Options { matter: Some(matter), thread: Some(thread), update_api: true, ..options() },
        ];
        for options in &cases {
            let mut unit = nestlocal_unit(Some(&pki));
            unit.files.insert("etc/connman/main.conf".into(), ("0:0".into(), 0o644, b"[General]\nFallbackTimeservers = time.nest.com\n".to_vec()));
            unit.files.insert("etc/hosts".into(), ("0:0".into(), 0o644, b"127.0.0.1 localhost\n".to_vec()));
            let tree = patched(unit, Device::Diamond3, options, Some(&pki));
            let mut want = expected(options);
            // Without a record, Nest's server left in place reads as named.
            if options.record.is_empty() {
                want.ntp_server.get_or_insert("time.nest.com".into());
            }
            assert_eq!(unit_settings(&tree, Device::Diamond3).unwrap(), want, "{options:?}");
        }
        // The record's time server, the default included, over the files'.
        let options = Options { block_cloud: true, ntp_server: Some(String::new()), ..options() };
        let mut unit = nestlocal_unit(Some(&pki));
        unit.files.insert("etc/connman/main.conf".into(), ("0:0".into(), 0o644, b"[General]\nFallbackTimeservers = time.nest.com\n".to_vec()));
        unit.files.insert("etc/hosts".into(), ("0:0".into(), 0o644, b"127.0.0.1 localhost\n".to_vec()));
        let recorded = Options { record: crate::bundle::Settings::new(&options, None).record(), ..options.clone() };
        let tree = patched(unit, Device::Diamond3, &recorded, Some(&pki));
        assert_eq!(unit_settings(&tree, Device::Diamond3).unwrap().ntp_server.as_deref(), Some(""));
    }

    #[test]
    fn a_j49_reads_back_its_ssh_server_and_usb_shell() {
        let j49 = || FakeVolume::new(vec![
            ("etc/shadow", 0o644, "root:*:16000:0:99999:7:::\n".into()),
            ("etc/inittab", 0o644, "::sysinit:/etc/init.d/rcS\n".into()),
            ("etc/init.d/rcS", 0o750, "a\n${INITDIR}/mountfs start\n${INITDIR}/sshd start\n${INITDIR}/nestlabs start\n".into()),
            ("etc/init.d/rcH", 0o750, "a\n${INITDIR}/umountfs stop\n".into()),
            ("etc/nestlabs/client.config", 0o644, stock()[7].2.clone()),
        ]);
        let server = Overlay { files: vec![("usr/sbin/dropbearmulti".into(), 0o755, b"ELF".to_vec()),
                                           (crate::j49::rootfs::SSH_INIT.into(), 0o755, include_bytes!("../../../platform/j49/ssh.init").to_vec())],
                               directories: Vec::new() };
        for device in [Device::J49, Device::Diamond] {
            for (ssh, usb_shell, ssh_key_only) in [(true, true, false), (false, false, false), (true, false, false), (true, false, true)] {
                let ssh_public_key = ssh_key_only.then(|| crate::setup::tests::public_key("ssh-ed25519", &[&[1; 32]]));
                let options = Options { ssh, usb_shell, ssh_key_only, ssh_public_key, ssh_server: Some(server.clone()), ..options() };
                let tree = patched(j49(), device, &options, None);
                assert_eq!(unit_settings(&tree, device).unwrap(), expected(&options), "{device:?} {options:?}");
            }
        }
    }

    #[test]
    fn the_home_screen_themes_and_either_key_only_login_read_back() {
        // Themes need the unit's own nlclient to set up: the options line
        // says they are on.
        let mut unit = nestlocal_unit(None);
        unit.files.insert(nestlocal::OPTIONS.into(), ("0:0".into(), 0o644, b"-mqtt tcp://b:1883 -themes\n".to_vec()));
        let settings = unit_settings(&tree(&unit), Device::Diamond3).unwrap();
        assert!(settings.themes && settings.mqtt.is_some());
        // A gen 3 an older release set up spells its key-only login as
        // OpenSSH 7.0 does; it reads as the same choice.
        let key = crate::setup::tests::public_key("ssh-ed25519", &[&[1; 32]]);
        let options = Options { ssh: true, ssh_key_only: true, ssh_public_key: Some(key), ..options() };
        let mut tree = patched(nestlocal_unit(None), Device::Diamond3, &options, None);
        let (config, _) = tree.get_mut("etc/ssh/sshd_config").unwrap();
        let text = String::from_utf8(config.clone()).unwrap();
        assert!(text.contains("PermitRootLogin without-password\n"), "{text}");
        *config = text.replace("PermitRootLogin without-password\n", "PermitRootLogin prohibit-password\n").into_bytes();
        let settings = unit_settings(&tree, Device::Diamond3).unwrap();
        assert!(settings.ssh && settings.ssh_key_only);
    }

    #[test]
    fn matter_keeps_its_exact_discriminator() {
        let pki = nestlocal::generate_pki().unwrap();
        let matter = Matter { passcode: 20202021, discriminator: 1234 };
        // The pairing code carries only the discriminator's top 4 bits.
        assert_eq!(Matter::from_pairing_code(&matter.pairing_code()).unwrap().discriminator, 1024);
        let tree = patched(nestlocal_unit(Some(&pki)), Device::Diamond3, &Options { matter: Some(matter), ..options() }, Some(&pki));
        assert_eq!(unit_settings(&tree, Device::Diamond3).unwrap().matter, Some(matter));
    }

    #[test]
    fn a_stock_unit_reads_as_all_off() {
        let unit = nestlocal_unit(None);
        assert_eq!(unit_settings(&tree(&unit), Device::Diamond3).unwrap(), UnitSettings::default());
        let j49 = FakeVolume::new(vec![
            ("etc/shadow", 0o644, "root::16000:0:99999:7:::\n".into()),
            ("etc/inittab", 0o644, "::sysinit:/etc/init.d/rcS\n".into()),
            ("etc/init.d/rcS", 0o750, "${INITDIR}/sshd start\n${INITDIR}/nestlabs start\n".into()),
        ]);
        assert_eq!(unit_settings(&tree(&j49), Device::J49).unwrap(), UnitSettings::default());
    }

    #[test]
    fn every_file_read_is_in_the_batch() {
        let pki = nestlocal::generate_pki().unwrap();
        let mqtt = Mqtt { url: "tcp://b:8883".into(), user: Some("u".into()), password: Some("p".into()), tls: true, ca: Some(CA.to_vec()),
                          entity_root: Some("nest_<serial>".into()) };
        let key = crate::setup::tests::public_key("ssh-ed25519", &[&[1; 32]]);
        let options = Options { mqtt: Some(mqtt), ssh: true, ssh_key_only: true, ssh_public_key: Some(key), device_name: Some("Hall".into()),
                                unsigned_updates: true, ..options() };
        let options = Options { record: crate::bundle::Settings::new(&options, None).record(), ..options };
        let mut unit = nestlocal_unit(Some(&pki));
        unit.files.insert("etc/ntp.conf".into(), ("0:0".into(), 0o644, b"server time.nest.com\n".to_vec()));
        let full = patched(unit, Device::Diamond3, &options, Some(&pki));
        let batch: Tree = full.iter().filter(|(name, _)| SETTINGS_FILES.contains(&name.as_str()))
            .map(|(name, file)| (name.clone(), file.clone())).collect();
        assert_eq!(unit_settings(&batch, Device::Diamond3).unwrap(), unit_settings(&full, Device::Diamond3).unwrap());
    }

    #[test]
    fn options_it_cannot_read_back_are_refused() {
        let mut tree = tree(&FakeVolume::new(stock()));
        for line in ["-mqtt tcp://b:1883 -frobnicate\n", "-matter -matter-passcode 20202021\n", "-mqtt\n",
                     "-mqtt tcp://b:1883 -mqtt-user u -mqtt-password-file /etc/nestlocal/mqtt.password\n"] {
            tree.insert(nestlocal::OPTIONS.into(), (line.as_bytes().to_vec(), 0o644));
            assert!(unit_settings(&tree, Device::Diamond3).is_err(), "{line}");
        }
        assert!(unit_settings(&tree, Device::Onyx).is_err());
    }
}
