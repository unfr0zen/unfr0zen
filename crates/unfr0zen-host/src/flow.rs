//! The recovery flow every device shares: the options a front end collects,
//! the run's directories and log, and the run itself, which finds the unit
//! and hands it to its device's flow. Reports through [`Progress`]; the
//! terminal and the window share it.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use unfr0zen_core::nestlocal::{self, Matter, Mqtt, Pki};
use unfr0zen_core::patch::{self, Options};
use unfr0zen_core::{Error, Result, Slot};

use crate::device::{self, Device};
use crate::progress::{self, Progress};
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
    pub cloud_url: Option<String>,
    pub backplate_sim: bool,
    /// Resolve Nest's and Google's names to nowhere; implies the default
    /// NTP server unless one is given, since time.nest.com goes too.
    pub block_cloud: bool,
    pub overlay: Option<PathBuf>,
    pub mqtt: Option<String>,
    pub mqtt_user: Option<String>,
    pub mqtt_password: Option<String>,
    pub mqtt_tls: bool,
    pub mqtt_ca: Option<PathBuf>,
    /// Serve the unit over Matter too (or instead of MQTT).
    pub matter: bool,
    /// Enable the onboard Thread transport for Matter.
    pub matter_thread: bool,
    /// The Matter pairing code to enter on the phone, as typed;
    /// DEFAULT_MATTER_CODE when not given.
    pub matter_code: Option<String>,
    /// Join a Zigbee network through the unit's radio, as a thermostat.
    pub zigbee: bool,
    /// Doom, launched from the UNFR0ZEN screen (through nestlocal).
    pub doom: bool,
    /// An SSH server on the unit, root logging in with its password; off by default.
    pub ssh: bool,
    /// The USB root shell (an ACM port); off by default. Without it the run
    /// cannot log in at the end to confirm the result.
    pub usb_shell: bool,
}

impl PatchInputs {
    /// Checks the options against each other and the filesystem, and builds the
    /// patch, all before the unit is touched. The error text is shown as is.
    pub fn build(&self) -> Result<Options> {
        if (self.mqtt_user.is_some() || self.mqtt_password.is_some() || self.mqtt_tls || self.mqtt_ca.is_some())
            && self.mqtt.is_none() {
            return Err(failure("--mqtt-user, --mqtt-password, --mqtt-tls and --mqtt-ca need --mqtt"));
        }
        let code = self.matter_code.as_deref().map(str::trim).filter(|c| !c.is_empty());
        let matter = self.matter || code.is_some();
        if self.matter_thread && (!matter || self.zigbee) {
            return Err(failure("Matter over Thread requires Matter and cannot share the radio with Zigbee"));
        }
        let nestlocal = self.mqtt.is_some() || matter || self.zigbee || self.doom;
        if nestlocal && self.overlay.is_some() {
            return Err(failure("--mqtt, --matter, --zigbee and --doom build their own overlay; drop --overlay"));
        }
        if nestlocal && self.cloud_url.is_some() {
            return Err(failure("--mqtt, --matter, --zigbee and --doom point the unit at nestlocal; drop --cloud-url"));
        }
        let read = |flag: &str, path: &Path| fs::read(path)
            .map_err(|e| failure(format!("{flag}: {}: {e}", path.display())));
        if let Some(url) = &self.cloud_url {
            patch::check_cloud_url(url)?;
        }
        if let Some(server) = self.ntp_server.as_deref().filter(|s| !s.is_empty()) {
            patch::check_ntp_server(server)?;
        }
        let mqtt = match &self.mqtt {
            Some(url) => Some(Mqtt {
                url: url.clone(), user: self.mqtt_user.clone(), password: self.mqtt_password.clone(),
                tls: self.mqtt_tls, ca: self.mqtt_ca.as_deref().map(|p| read("--mqtt-ca", p)).transpose()?,
            }),
            None => None,
        };
        if let Some(mqtt) = &mqtt {
            nestlocal::mqtt_options(mqtt)?;
        }
        let overlay = self.overlay.as_deref().map(|p| patch::overlay_from_tar(&read("--overlay", p)?)).transpose()?;
        Ok(Options {
            root_hash: patch::root_hash(self.root_password.as_deref().unwrap_or(patch::DEFAULT_ROOT_PASSWORD))?,
            ntp_server: self.ntp_server.clone().or_else(|| self.block_cloud.then(String::new)),
            cloud_url: self.cloud_url.clone(),
            backplate_sim: self.backplate_sim,
            block_cloud: self.block_cloud,
            overlay,
            mqtt,
            matter: matter.then(|| Matter::from_pairing_code(code.unwrap_or(nestlocal::DEFAULT_MATTER_CODE))).transpose()?,
            thread: self.matter_thread.then(assets::thread),
            zigbee: self.zigbee,
            doom: self.doom.then(assets::doom),
            ssh: self.ssh,
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
    pub slot: Option<Slot>,
    pub dry_run: bool,
    /// Read and save the whole volume before patching (slow; off by default).
    pub backup: bool,
    /// How long to wait for the unit in its boot ROM's USB download mode.
    pub wait_usb: Duration,
    pub wait_shell: Duration,
    /// Where backups and run records go.
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
    fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// /tmp/unfr0zen is the log root; an older logo file there is kept aside.
fn prepare_log_root(progress: &mut dyn Progress) -> Result<PathBuf> {
    let root = if cfg!(unix) { PathBuf::from("/tmp/unfr0zen") } else { std::env::temp_dir().join("unfr0zen") };
    if root.is_file() || root.is_symlink() {
        let mut saved = root.with_file_name("unfr0zen-logo");
        if saved.exists() {
            saved = root.with_file_name(format!("unfr0zen-logo-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
        }
        fs::rename(&root, &saved)?;
        progress.status(&format!("Preserved previous logo at {}", saved.display()));
    }
    fs::create_dir_all(&root)?;
    Ok(root)
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

/// Starts the run log in its own directory under the log root; returns the directory.
pub(crate) fn start_log(progress: &mut dyn Progress, stamp: &str) -> Result<PathBuf> {
    let log_dir = prepare_log_root(progress)?.join(stamp);
    private_dir(&log_dir)?;
    let log_path = log_dir.join("unfr0zen.log");
    progress::set_log_file(&log_path)?;
    progress.status(&format!("Detailed log: {}", log_path.display()));
    Ok(log_dir)
}

/// Runs the recovery from whatever mode the unit is in to a confirmed root
/// shell, writing run records under `work_dir` and the timestamped run log.
/// Returns the final status line.
pub fn run(config: &Config, progress: &mut dyn Progress) -> Result<String> {
    let unit = device::detect(progress, config.wait_usb, config.omap)?;
    match unit.device {
        Device::Diamond3 => diamond3::run(config, progress, unit),
        Device::Diamond | Device::J49 => j49::run(config, progress, unit),
    }
}
