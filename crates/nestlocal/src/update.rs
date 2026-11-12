//! unfr0zen's updates: the version the unit runs, the latest release when
//! the internet is there to ask, and installing it when Home Assistant says
//! so (bridge.rs: an update entity on <base>/update, install on
//! <base>/set/update), or a bundle at a URL given instead, for a unit that
//! cannot reach GitHub: a copy on the LAN.
//!
//! A release's patch bundle (unfr0zen-<device>-<tag>.tgz) goes to the
//! engine the patch left on the unit (unfr0zen-apply update), which checks
//! it against its manifest and the release key before running it; the unit
//! then restarts into it. A unit with no engine (gen 1 and gen 2, patched
//! from the host) reports versions only.
//!
//! The last answer about the latest release is kept (-update-store), so a
//! restart shows it at once instead of unknown until the next check.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::logf;

/// The engine the patch keeps on a unit that installs updates.
const ENGINE: &str = "/nestlabs/sbin/unfr0zen-apply";
/// "unfr0zen VERSION", written by the patch.
const VERSION_FILE: &str = "/etc/unfr0zen/version";
/// The latest release, as GitHub's API describes it.
pub const RELEASES: &str = "https://api.github.com/repos/unfr0zen/unfr0zen/releases/latest";
/// Where a downloaded bundle waits for the engine: tmpfs, gone at restart.
const DOWNLOAD: &str = "/tmp/unfr0zen-update.tgz";
const FIRST_CHECK: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
/// After a check that got no answer: a network can drop one.
const RETRY: Duration = Duration::from_secs(300);
/// The release description; a bundle is a few MB, well under the second.
const API_LIMIT: usize = 1 << 20;
const BUNDLE_LIMIT: usize = 64 << 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    /// The release's page.
    pub url: Option<String>,
    /// This device's bundle, if the release has one.
    pub bundle: Option<String>,
}

pub struct Updates {
    device: &'static str,
    installed: Option<String>,
    can_install: bool,
    releases: String,
    store: PathBuf,
    latest: Mutex<Option<Release>>,
    installing: AtomicBool,
    offline_said: AtomicBool,
    on_change: OnceLock<Box<dyn Fn() + Send + Sync>>,
}

/// The version in VERSION_FILE's "unfr0zen VERSION".
fn installed(text: &str) -> Option<String> {
    let version = text.trim().strip_prefix("unfr0zen ").unwrap_or(text.trim());
    (!version.is_empty()).then(|| version.to_string())
}

/// A version's place in release order: vMAJOR.MINOR.PATCH, its -rcN
/// candidates before it and git describe's -N-gHASH builds after it, as
/// unfr0zen-core's bundle::is_downgrade orders them. None for another form.
fn order(version: &str) -> Option<(u64, u64, u64, u8, u64)> {
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

/// The latest version to report: the installed one when it is later than
/// the release (a developer's build), since Home Assistant ranks v0.4.2
/// above v0.4.2-16-gHASH and would offer the older release as an update.
fn latest_version(installed: Option<&str>, release: &str) -> String {
    match installed {
        Some(installed) if order(installed).zip(order(release)).is_some_and(|(i, r)| i > r) => installed.to_string(),
        _ => release.to_string(),
    }
}

/// What a message on <base>/set/update asks for.
#[derive(Debug, PartialEq)]
pub enum Request {
    /// "install": the latest release.
    Latest,
    /// An http:// or https:// URL: the bundle there.
    Url(String),
}

pub fn request(payload: &str) -> Option<Request> {
    let payload = payload.trim();
    if payload == "install" {
        Some(Request::Latest)
    } else if payload.starts_with("http://") || payload.starts_with("https://") {
        Some(Request::Url(payload.to_string()))
    } else {
        None
    }
}

/// The latest release the last check found, as the store keeps it.
fn load(store: &Path) -> Option<Release> {
    serde_json::from_slice(&std::fs::read(store).ok()?).ok()
}

fn save(store: &Path, release: Option<&Release>) {
    let result = match release {
        Some(release) => std::fs::write(store, serde_json::to_vec(release).expect("a release serializes")),
        None => std::fs::remove_file(store).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }),
    };
    if let Err(e) = result {
        logf!("update: {}: {e}", store.display());
    }
}

/// The release in GitHub's description of it, with this device's bundle.
pub fn release(description: &Value, device: &str) -> Option<Release> {
    let version = description["tag_name"].as_str()?.to_string();
    let wanted = format!("unfr0zen-{device}-{version}.tgz");
    let bundle = description["assets"].as_array().into_iter().flatten()
        .find(|asset| asset["name"].as_str() == Some(wanted.as_str()))
        .and_then(|asset| asset["browser_download_url"].as_str()).map(str::to_string);
    Some(Release { version, url: description["html_url"].as_str().map(str::to_string), bundle })
}

impl Updates {
    pub fn new(device: &'static str, releases: &str, store: &Path) -> Arc<Updates> {
        Arc::new(Updates {
            device,
            installed: std::fs::read_to_string(VERSION_FILE).ok().as_deref().and_then(installed),
            can_install: Path::new(ENGINE).is_file(),
            releases: releases.to_string(),
            store: store.to_path_buf(),
            latest: Mutex::new(load(store)),
            installing: AtomicBool::new(false),
            offline_said: AtomicBool::new(false),
            on_change: OnceLock::new(),
        })
    }

    /// Whether this unit installs updates itself.
    pub fn can_install(&self) -> bool {
        self.can_install
    }

    /// What to call when the state changes.
    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.on_change.set(Box::new(f));
    }

    fn changed(&self) {
        if let Some(f) = self.on_change.get() {
            f();
        }
    }

    /// The update entity's state.
    pub fn state(&self) -> Value {
        let latest = self.latest.lock().unwrap().clone();
        json!({
            "installed_version": self.installed,
            // Unknown until a check answers.
            "latest_version": latest.as_ref().map(|r| latest_version(self.installed.as_deref(), &r.version)),
            "release_url": latest.and_then(|r| r.url),
            "in_progress": self.installing.load(Ordering::Relaxed),
        })
    }

    /// Asks for the latest release a minute after start, then every 6 hours,
    /// or 5 minutes after one that got no answer.
    pub async fn keep_checking(self: Arc<Self>) {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            let answered = self.check().await;
            tokio::time::sleep(if answered { CHECK_EVERY } else { RETRY }).await;
        }
    }

    /// Whether the release check got an answer.
    async fn check(&self) -> bool {
        let answer = crate::fetch::get(&self.releases, API_LIMIT, Duration::from_secs(60)).await
            .and_then(|body| serde_json::from_slice::<Value>(&body).map_err(|e| e.to_string()));
        let found = match answer {
            Ok(description) => release(&description, self.device),
            Err(e) => {
                // No internet is the usual reason: said once, not every 6 hours.
                if !self.offline_said.swap(true, Ordering::Relaxed) {
                    logf!("update: the latest release is unknown: {e}");
                }
                return false;
            }
        };
        self.offline_said.store(false, Ordering::Relaxed);
        if *self.latest.lock().unwrap() != found {
            if let Some(release) = &found {
                logf!("update: the latest release is {}{}", release.version,
                      if release.bundle.is_none() { ", with no bundle for this unit" } else { "" });
            }
            save(&self.store, found.as_ref());
            *self.latest.lock().unwrap() = found;
            self.changed();
        }
        true
    }

    /// Installs the latest release, or the bundle at the request's URL: the
    /// unit's engine checks it either way.
    pub fn install(self: &Arc<Self>, request: Request) {
        if !self.can_install {
            return logf!("update: this unit has no update engine ({ENGINE}): patch it with unfr0zen");
        }
        let (bundle, what) = match request {
            Request::Url(url) => (url.clone(), format!("the bundle at {url}")),
            Request::Latest => {
                let Some(release) = self.latest.lock().unwrap().clone() else {
                    return logf!("update: no release known to install");
                };
                let Some(bundle) = release.bundle else {
                    return logf!("update: release {} has no bundle for this unit", release.version);
                };
                (bundle, release.version)
            }
        };
        if self.installing.swap(true, Ordering::Relaxed) {
            return logf!("update: already installing");
        }
        self.changed();
        let updates = self.clone();
        tokio::spawn(async move {
            match updates.install_from(&bundle).await {
                Ok(()) => {
                    logf!("update: {what} installed; restarting");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let _ = std::process::Command::new("reboot").status();
                }
                Err(e) => {
                    logf!("update: {e}");
                    updates.installing.store(false, Ordering::Relaxed);
                    updates.changed();
                }
            }
        });
    }

    async fn install_from(&self, url: &str) -> Result<(), String> {
        logf!("update: downloading {url}");
        let data = crate::fetch::get(url, BUNDLE_LIMIT, Duration::from_secs(600)).await?;
        std::fs::write(DOWNLOAD, &data).map_err(|e| format!("{DOWNLOAD}: {e}"))?;
        let output = tokio::task::spawn_blocking(|| std::process::Command::new(ENGINE).args(["update", DOWNLOAD]).output())
            .await.map_err(|e| e.to_string())?.map_err(|e| format!("{ENGINE}: {e}"))?;
        let _ = std::fs::remove_file(DOWNLOAD);
        for line in String::from_utf8_lossy(&output.stderr).lines().chain(String::from_utf8_lossy(&output.stdout).lines()) {
            logf!("update: {line}");
        }
        if !output.status.success() {
            return Err(format!("the update was not installed ({})", output.status));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_update_takes_install_or_a_bundles_url() {
        assert_eq!(request("install\n"), Some(Request::Latest));
        assert_eq!(request(" http://nas.lan/unfr0zen-diamond3-v0.5.0.tgz "),
                   Some(Request::Url("http://nas.lan/unfr0zen-diamond3-v0.5.0.tgz".into())));
        assert_eq!(request("https://x/b.tgz"), Some(Request::Url("https://x/b.tgz".into())));
        assert_eq!(request("file:///tmp/b.tgz"), None);
        assert_eq!(request("yes"), None);
    }

    #[test]
    fn a_build_later_than_the_release_is_reported_as_the_latest() {
        assert_eq!(latest_version(Some("v0.4.2-16-g980d4b5-dirty"), "v0.4.2"), "v0.4.2-16-g980d4b5-dirty");
        assert_eq!(latest_version(Some("v0.4.3-rc1"), "v0.4.2"), "v0.4.3-rc1");
        assert_eq!(latest_version(Some("v0.4.3-rc2"), "v0.4.3"), "v0.4.3");
        assert_eq!(latest_version(Some("v0.4.2-16-g980d4b5"), "v0.4.3-rc1"), "v0.4.3-rc1");
        assert_eq!(latest_version(Some("v0.4.2"), "v0.4.2"), "v0.4.2");
        assert_eq!(latest_version(Some("custom"), "v0.4.2"), "v0.4.2");
        assert_eq!(latest_version(None, "v0.4.2"), "v0.4.2");
    }

    #[test]
    fn the_installed_version_comes_from_the_patchs_version_file() {
        assert_eq!(installed("unfr0zen v0.4.3\n").as_deref(), Some("v0.4.3"));
        assert_eq!(installed("\n"), None);
    }

    #[test]
    fn the_last_release_found_is_there_again_after_a_restart() {
        let dir = std::env::temp_dir().join(format!("nestlocal-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = dir.join("update.json");
        assert_eq!(load(&store), None);
        let release = Release { version: "v0.5.0".into(), url: Some("https://x/r".into()), bundle: Some("https://x/b.tgz".into()) };
        save(&store, Some(&release));
        let updates = Updates::new("diamond3", RELEASES, &store);
        assert_eq!(updates.state()["latest_version"], "v0.5.0");
        assert_eq!(updates.state()["release_url"], "https://x/r");
        save(&store, None);
        assert_eq!(Updates::new("diamond3", RELEASES, &store).state()["latest_version"], Value::Null);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_release_names_this_devices_bundle_when_it_has_one() {
        let description = json!({
            "tag_name": "v0.5.0", "html_url": "https://github.com/unfr0zen/unfr0zen/releases/tag/v0.5.0",
            "assets": [
                {"name": "unfr0zen-macos-universal-v0.5.0.zip", "browser_download_url": "https://x/app.zip"},
                {"name": "unfr0zen-diamond3-v0.5.0.tgz", "browser_download_url": "https://x/diamond3.tgz"},
            ],
        });
        let gen3 = release(&description, "diamond3").unwrap();
        assert_eq!((gen3.version.as_str(), gen3.bundle.as_deref()), ("v0.5.0", Some("https://x/diamond3.tgz")));
        assert_eq!(release(&description, "j49").unwrap().bundle, None);
        assert_eq!(release(&json!({"message": "Not Found"}), "diamond3"), None);
    }
}
