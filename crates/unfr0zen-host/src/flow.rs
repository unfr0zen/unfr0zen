//! The recovery flow every device shares: the options a front end collects,
//! the run's directories and log, and the run itself, which finds the unit
//! and hands it to its device's flow. Reports through [`Progress`]; the
//! terminal and the window share it.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use unfr0zen_core::nestlocal::{self, Matter, Mqtt, Pki};
use unfr0zen_core::nlclient;
use unfr0zen_core::patch::{self, Options};
use unfr0zen_core::{Error, Result};

use crate::device::{self, Device};
use crate::progress::{self, Phase, Progress};
use crate::diamond3::usb::EntryPoint;
use crate::{assets, diamond3, j49};

pub(crate) fn seconds(n: u64) -> Duration {
    Duration::from_secs(n)
}

pub(crate) fn failure(error: impl std::fmt::Display) -> Error {
    Error::Device(error.to_string())
}

/// The raw patch inputs a front end collects, validated into [`Options`] by
/// [`PatchInputs::build`] before the unit is touched.
#[derive(Clone, Debug, Default)]
pub struct PatchInputs {
    pub root_password: Option<String>,
    pub ntp_server: Option<String>,
    pub backplate_sim: bool,
    /// Resolve Nest's and Google's names to nowhere; implies the default
    /// NTP server unless one is given, since time.nest.com goes too.
    pub block_cloud: bool,
    pub mqtt: Option<String>,
    pub mqtt_user: Option<String>,
    pub mqtt_password: Option<String>,
    pub mqtt_tls: bool,
    pub mqtt_ca: Option<PathBuf>,
    /// The name MQTT and Matter give the unit; None keeps the unit's.
    pub device_name: Option<String>,
    /// What replaces nestlocal_<serial> in the entity IDs; None keeps the unit's.
    pub ha_entity_root: Option<String>,
    /// Serve the unit over Matter too (or instead of MQTT).
    pub matter: bool,
    /// Enable the onboard Thread transport for Matter.
    pub matter_thread: bool,
    /// The Matter pairing code to enter on the phone, as typed;
    /// DEFAULT_MATTER_CODE when not given.
    pub matter_code: Option<String>,
    /// Join a Zigbee network through the unit's radio, as a thermostat.
    pub zigbee: bool,
    /// Run a Heat Link model as heating a Works With Nest boiler through W1.
    pub wwn_boiler: bool,
    /// How the 3rd generation's boot ROM is entered for recovery.
    pub entry_point: EntryPoint,
    /// Doom, launched from the UNFR0ZEN screen (through nestlocal).
    pub doom: bool,
    /// Regulate from an external thermometer's readings: nestlocal plays a Nest
    /// Temperature Sensor.
    pub external_temperature: bool,
    /// Gen 2's settings screens in the gen 3's dark palette.
    pub dark_theme: bool,
    /// An SSH server on the unit, root logging in with its password; off by default.
    pub ssh: bool,
    /// One OpenSSH public key to authorize for root; requires SSH.
    pub ssh_public_key: Option<PathBuf>,
    /// SSH takes keys only, refusing root's password; requires the key.
    pub ssh_key_only: bool,
    /// The USB root shell (an ACM port); off by default. Without it the run
    /// cannot log in at the end to confirm the result.
    pub usb_shell: bool,
}

/// A URL with any user name and password in it replaced by ***.
pub fn mask_url(url: &str) -> String {
    let start = url.find("://").map_or(0, |i| i + 3);
    let end = url[start..].find('/').map_or(url.len(), |i| start + i);
    match url[start..end].rfind('@') {
        Some(at) => format!("{}***@{}", &url[..start], &url[start + at + 1..]),
        None => url.to_string(),
    }
}

impl PatchInputs {
    /// The options as the CLI spells them, for the run log: secrets and the
    /// MQTT user masked, paths cut to their file names.
    pub fn describe(&self) -> String {
        let secret = |value: &Option<String>| value.as_ref().map(|_| "***".to_string());
        let on = |set: bool| set.then(String::new);
        let file = |path: &Option<PathBuf>| path.as_ref()
            .map(|path| path.file_name().unwrap_or_default().to_string_lossy().into_owned());
        let flags = [
            ("ntp-server", self.ntp_server.clone()),
            ("root-password", secret(&self.root_password)),
            ("ssh", on(self.ssh)),
            ("ssh-public-key", file(&self.ssh_public_key)),
            ("ssh-key-only", on(self.ssh_key_only)),
            ("usb-shell", on(self.usb_shell)),
            ("mqtt", self.mqtt.as_deref().map(mask_url)),
            ("mqtt-user", secret(&self.mqtt_user)),
            ("mqtt-password", secret(&self.mqtt_password)),
            ("mqtt-tls", on(self.mqtt_tls)),
            ("mqtt-ca", file(&self.mqtt_ca)),
            ("ha-entity-root", self.ha_entity_root.clone()),
            ("matter", on(self.matter)),
            ("device-name", self.device_name.as_ref().map(|name| format!("{name:?}"))),
            ("matter-code", secret(&self.matter_code)),
            ("matter-thread", on(self.matter_thread)),
            ("zigbee", on(self.zigbee)),
            ("wwn-boiler", on(self.wwn_boiler)),
            ("entry-point", (self.entry_point != EntryPoint::Auto).then(|| self.entry_point.name().to_string())),
            ("doom", on(self.doom)),
            ("external-temperature", on(self.external_temperature)),
            ("dark-theme", on(self.dark_theme)),
            ("backplate-sim", on(self.backplate_sim)),
            ("block-cloud", on(self.block_cloud)),
        ];
        let set: Vec<String> = flags.into_iter().filter_map(|(name, value)| value.map(|value|
            if value.is_empty() { format!("--{name}") } else { format!("--{name} {value}") })).collect();
        if set.is_empty() { "no options".into() } else { set.join(" ") }
    }

    /// Checks the options against each other and the filesystem, and builds the
    /// patch, all before the unit is touched. The error text is shown as is.
    pub fn build(&self) -> Result<Options> {
        if self.ssh_public_key.is_some() && !self.ssh {
            return Err(failure("--ssh-public-key needs --ssh"));
        }
        if self.ssh_key_only && self.ssh_public_key.is_none() {
            return Err(failure("--ssh-key-only needs --ssh-public-key: without a key nothing could log in"));
        }
        if (self.mqtt_user.is_some() || self.mqtt_password.is_some() || self.mqtt_tls || self.mqtt_ca.is_some()
            || self.ha_entity_root.is_some()) && self.mqtt.is_none() {
            return Err(failure("--mqtt-user, --mqtt-password, --mqtt-tls, --mqtt-ca and --ha-entity-root need --mqtt"));
        }
        let code = self.matter_code.as_deref().map(str::trim).filter(|c| !c.is_empty());
        let matter = self.matter || code.is_some();
        if self.device_name.is_some() && self.mqtt.is_none() && !matter {
            return Err(failure("--device-name names the unit for --mqtt or --matter: it needs one of them"));
        }
        if self.matter_thread && (!matter || self.zigbee) {
            return Err(failure("Matter over Thread requires Matter and cannot share the radio with Zigbee"));
        }
        if self.wwn_boiler && !self.zigbee && !self.matter_thread {
            return Err(failure("--wwn-boiler is for a radio nestlocal holds: it needs --zigbee or --matter-thread"));
        }
        if self.external_temperature && self.mqtt.is_none() && !matter && !self.zigbee {
            return Err(failure("--external-temperature takes its readings over --mqtt, --matter or --zigbee"));
        }
        let read = |flag: &str, path: &Path| fs::read(path)
            .map_err(|e| failure(format!("{flag}: {}: {e}", path.display())));
        if let Some(server) = self.ntp_server.as_deref().filter(|s| !s.is_empty()) {
            patch::check_ntp_server(server)?;
        }
        let mqtt = match &self.mqtt {
            Some(url) => Some(Mqtt {
                url: url.clone(), user: self.mqtt_user.clone(), password: self.mqtt_password.clone(),
                tls: self.mqtt_tls, ca: self.mqtt_ca.as_deref().map(|p| read("--mqtt-ca", p)).transpose()?,
                entity_root: self.ha_entity_root.clone(),
            }),
            None => None,
        };
        if let Some(mqtt) = &mqtt {
            nestlocal::mqtt_options(mqtt)?;
        }
        if let Some(name) = &self.device_name {
            nestlocal::check_device_name(name)?;
        }
        let ssh_public_key = self.ssh_public_key.as_deref().map(|path| {
            let data = fs::read(path).map_err(|e| failure(format!("--ssh-public-key: {}: {e}",
                path.file_name().unwrap_or_default().to_string_lossy())))?;
            patch::ssh_public_key(&data)
        }).transpose()?;
        Ok(Options {
            root_hash: patch::root_hash(self.root_password.as_deref().unwrap_or(patch::DEFAULT_ROOT_PASSWORD))?,
            version: progress::app_version().to_string(),
            ntp_server: self.ntp_server.clone().or_else(|| self.block_cloud.then(String::new)),
            backplate_sim: self.backplate_sim,
            block_cloud: self.block_cloud,
            mqtt,
            matter: matter.then(|| Matter::from_pairing_code(code.unwrap_or(nestlocal::DEFAULT_MATTER_CODE))).transpose()?,
            device_name: self.device_name.clone(),
            wake_filter: if matter { assets::WAKE_FILTER.to_vec() } else { Vec::new() },
            thread: self.matter_thread.then(assets::thread),
            zigbee: self.zigbee,
            wwn_boiler: self.wwn_boiler,
            doom: self.doom.then(assets::doom),
            external_temperature: self.external_temperature,
            external_temperature_screen: if self.external_temperature { assets::J49_RCS_SCREEN.to_vec() } else { Vec::new() },
            dark_theme: self.dark_theme,
            ssh: self.ssh,
            ssh_public_key,
            ssh_key_only: self.ssh_key_only,
            ssh_server: None,
            usb_shell: self.usb_shell,
        })
    }
}

/// A fully resolved run.
pub struct Config {
    pub options: Options,
    /// The device to expect, instead of whichever the USB identity names;
    /// Diamond and j49 share their boot ROM's USB ID, taken as j49 without it.
    pub omap: Option<Device>,
    /// How the 3rd generation's boot ROM is entered for recovery.
    pub entry_point: EntryPoint,
    /// How long to wait for the unit in its boot ROM's USB download mode.
    pub wait_usb: Duration,
    pub wait_shell: Duration,
    /// Where run records go.
    pub work_dir: PathBuf,
    /// Where nestlocal's key set for units without their own is kept.
    pub data_dir: PathBuf,
}

/// The default per-user data directory for backups and nestlocal's key set.
pub fn default_data_dir() -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("unfr0zen")
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support/unfr0zen")
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")).join("unfr0zen")
    }
}

pub(crate) fn private_dir(path: &Path) -> Result<()> {
    let context = |error| Error::Io(format!("create private directory {}: {error}", path.display()));
    fs::create_dir_all(path.parent().unwrap_or(Path::new("."))).map_err(context)?;
    fs::create_dir(path).map_err(context)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(context)?;
    }
    Ok(())
}

/// Under sudo, what a run leaves is given back to the user who started it.
pub(crate) struct HandBack(pub(crate) Vec<PathBuf>);

impl Drop for HandBack {
    fn drop(&mut self) {
        #[cfg(not(unix))]
        let _ = &self.0; // nothing to give back without sudo
        #[cfg(unix)]
        if let (Some(uid), Some(gid)) = (std::env::var("SUDO_UID").ok().and_then(|v| v.parse().ok()),
                                         std::env::var("SUDO_GID").ok().and_then(|v| v.parse().ok())) {
            fn walk(path: &Path, uid: u32, gid: u32) {
                let _ = std::os::unix::fs::lchown(path, Some(uid), Some(gid));
                if let Ok(entries) = fs::read_dir(path) {
                    for entry in entries.flatten() {
                        walk(&entry.path(), uid, gid);
                    }
                }
            }
            for path in &self.0 {
                walk(path, uid, gid);
            }
        }
    }
}

/// nestlocal's key set for units without their own, made once and kept.
pub(crate) fn stored_pki(directory: &Path) -> Result<Pki> {
    let read = |name: &str| fs::read(directory.join(name));
    if let (Ok(ca), Ok(server_crt), Ok(server_key)) = (read("ca.crt"), read("server.crt"), read("server.key")) {
        return Ok(Pki { ca, server_crt, server_key });
    }
    let pki = nestlocal::generate_pki()?;
    fs::create_dir_all(directory)?;
    fs::write(directory.join("ca.crt"), &pki.ca)?;
    fs::write(directory.join("server.crt"), &pki.server_crt)?;
    fs::write(directory.join("server.key"), &pki.server_key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory.join("server.key"), fs::Permissions::from_mode(0o600))?;
    }
    log::info!("Made nestlocal's key set in {}", directory.display());
    Ok(pki)
}

/// Links the settings screen against the unit's stock nlclient, keeping
/// what was discovered in the run directory as nlclient.json. A build no
/// reviewed variant names, or one the link fails on, is a sample a review
/// needs, and the run has read it: its stock executable goes there too.
pub(crate) fn screen_builder(run_dir: &Path) -> impl Fn(&[u8]) -> Result<Vec<u8>> + '_ {
    move |source| {
        log::info!("Discovering nlclient ABI and linking UNFR0ZEN settings in memory");
        let path = run_dir.join("nlclient.stock");
        match nlclient::builder::build(source, assets::CUSTOM_SCREEN) {
            Ok(built) => {
                fs::write(run_dir.join("nlclient.json"), format!("{:#}\n", built.manifest))?;
                let status = built.manifest["status"].as_str().unwrap_or_default();
                if built.manifest["reviewed"] == false {
                    fs::write(&path, source)?;
                    log::info!("nlclient: {status}; its stock executable is saved as {}", path.display());
                } else {
                    log::info!("nlclient: {status}");
                }
                Ok(built.journal)
            }
            Err(error) => Err(match fs::write(&path, source) {
                Ok(()) => failure(format!("{error}. The unit's stock nlclient is saved as {}: attach it to a bug report",
                                          path.display())),
                Err(write) => failure(format!("{error} (saving the unit's stock nlclient as {} failed: {write})",
                                              path.display())),
            }),
        }
    }
}

/// Starts the run log under the user's data directory; returns the directory.
pub(crate) fn start_log(progress: &mut dyn Progress, stamp: &str) -> Result<PathBuf> {
    let log_dir = default_data_dir().join("logs").join(stamp);
    private_dir(&log_dir)?;
    let log_path = log_dir.join("unfr0zen.log");
    progress::set_log_file(&log_path)?;
    progress.status(&format!("Detailed log: {}", log_path.display()));
    Ok(log_dir)
}

/// Runs the recovery from whatever mode the unit is in to a confirmed root
/// shell, writing run records under `work_dir` and the timestamped run log.
/// Returns the final status line.
pub fn run(config: &Config, progress: &mut dyn Progress,
           cancel: &device::CancelFlag) -> Result<String> {
    run_after_backup(config, None, progress, cancel)
}

/// [`run`], with the whole NAND backed up to `backup` first, while the unit
/// is in recovery for the patch: one entry into USB download mode for both.
pub fn run_after_backup(config: &Config, backup: Option<&Path>, progress: &mut dyn Progress,
                        cancel: &device::CancelFlag) -> Result<String> {
    if let Some(out) = backup.filter(|out| out.exists()) {
        return Err(failure(format!("{} exists", out.display())));
    }
    progress.phase(Phase::DownloadMode);
    let unit = device::detect(progress, config.wait_usb, config.omap, cancel)?;
    match unit.device {
        Device::Diamond | Device::Diamond3 if config.options.dark_theme =>
            Err(failure("--dark-theme is only for the 2nd generation")),
        Device::Diamond3 => diamond3::run(config, progress, unit, cancel, backup),
        Device::Diamond | Device::J49 => j49::run(config, progress, unit, cancel, backup),
        Device::Onyx => Err(crate::onyx::unsupported(progress)),
    }
}

#[cfg(test)]
mod describe_tests {
    use super::*;

    #[test]
    fn the_entry_point_names_its_choice() {
        assert_eq!(PatchInputs::default().entry_point, EntryPoint::Auto);
        let forced = PatchInputs {
            entry_point: "cve-2017-7936".parse().unwrap(),
            ..PatchInputs::default()
        };
        assert_eq!(forced.describe(), "--entry-point cve-2017-7936");
        assert_eq!(PatchInputs { entry_point: "cve-2017-7932".parse().unwrap(), ..PatchInputs::default() }
            .describe(), "--entry-point cve-2017-7932");
        let error = "nope".parse::<EntryPoint>().err().unwrap();
        assert!(error.contains("auto, cve-2017-7932, cve-2017-7936 or official-loader"), "{error}");
    }

    #[test]
    fn the_device_name_is_for_mqtt_or_matter() {
        let alone = PatchInputs { device_name: Some("Nest FamRm".into()), ..PatchInputs::default() };
        assert!(alone.build().err().unwrap().to_string().contains("--device-name"));
        let matter = PatchInputs { matter: true, ..alone.clone() };
        assert_eq!(matter.build().unwrap().device_name.as_deref(), Some("Nest FamRm"));
        assert_eq!(matter.describe(), "--matter --device-name \"Nest FamRm\"");
        assert!(PatchInputs { mqtt: Some("broker".into()), ..alone.clone() }.build().is_ok());
        assert!(PatchInputs { device_name: Some("x".repeat(33)), ..matter }.build().is_err(), "past Matter's 32");
    }

    #[test]
    fn the_boiler_option_needs_a_radio_nestlocal_holds() {
        let alone = PatchInputs { wwn_boiler: true, ..PatchInputs::default() };
        assert!(alone.build().err().unwrap().to_string().contains("--wwn-boiler"));
        let mqtt = PatchInputs { mqtt: Some("broker".into()), ..alone.clone() };
        assert!(mqtt.build().is_err(), "MQTT leaves the radio to Nest");
        let zigbee = PatchInputs { zigbee: true, ..alone.clone() };
        assert!(zigbee.build().unwrap().wwn_boiler);
        assert_eq!(zigbee.describe(), "--zigbee --wwn-boiler");
        let thread = PatchInputs { matter: true, matter_thread: true, ..alone };
        assert!(thread.build().unwrap().wwn_boiler);
        assert!(!PatchInputs { zigbee: true, ..PatchInputs::default() }.build().unwrap().wwn_boiler, "off unless asked");
    }

    #[test]
    fn directory_errors_identify_the_requested_path() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("not-a-directory");
        fs::write(&parent, b"").unwrap();
        let path = parent.join("run");
        let error = private_dir(&path).unwrap_err().to_string();
        assert!(error.contains(path.to_str().unwrap()), "{error}");
    }

    #[test]
    fn public_key_input_requires_ssh_and_is_validated_before_the_run() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("id_ed25519.pub");
        let mut inputs = PatchInputs { ssh_public_key: Some(path.clone()), ..PatchInputs::default() };
        assert!(inputs.build().unwrap_err().to_string().contains("needs --ssh"));
        inputs.ssh = true;
        let error = inputs.build().unwrap_err().to_string();
        assert!(error.contains("id_ed25519.pub"));
        assert!(!error.contains(directory.path().to_str().unwrap()));
        fs::write(&path, b"-----BEGIN OPENSSH PRIVATE KEY-----\nprivate-material").unwrap();
        let error = inputs.build().unwrap_err().to_string();
        assert!(!error.contains("private-material"));
        fs::write(&path, b"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB user@host\n").unwrap();
        let options = inputs.build().unwrap();
        assert!(options.ssh_public_key.unwrap().starts_with("ssh-ed25519 "));
        assert_eq!(inputs.describe(), "--ssh --ssh-public-key id_ed25519.pub");
    }

    #[test]
    fn unknown_firmware_keeps_the_stock_nlclient_for_a_report() {
        let directory = tempfile::tempdir().unwrap();
        let stock = b"not an ARM executable";
        let error = screen_builder(directory.path())(stock).unwrap_err().to_string();
        let saved = directory.path().join("nlclient.stock");
        assert!(error.contains(saved.to_str().unwrap()), "{error}");
        assert!(error.contains("bug report"), "{error}");
        assert_eq!(fs::read(&saved).unwrap(), stock);
    }

    #[test]
    fn options_read_as_cli_flags_with_secrets_masked() {
        let inputs = PatchInputs { zigbee: true, block_cloud: true, ntp_server: Some("time.lan".into()),
            root_password: Some("hunter2".into()), mqtt: Some("tcp://bob:pw@broker.lan".into()),
            mqtt_ca: Some(PathBuf::from("/home/bob/ca.pem")), ..PatchInputs::default() };
        assert_eq!(inputs.describe(), "--ntp-server time.lan --root-password *** --mqtt tcp://***@broker.lan \
                                       --mqtt-ca ca.pem --zigbee --block-cloud");
        assert_eq!(PatchInputs::default().describe(), "no options");
        assert_eq!(mask_url("broker.lan:1883"), "broker.lan:1883");
        assert_eq!(mask_url("mqtt://u@host/path@x"), "mqtt://***@host/path@x");
    }
}
