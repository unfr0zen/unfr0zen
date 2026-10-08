//! Updates from the releases of the public release repository on GitHub:
//! its latest stable release (no drafts, no -rc pre-releases), and that
//! release's file for this app and platform, as CI names them
//! (unfr0zen-<platform>-<version>.zip|tar.gz for the window,
//! unfr0zen-cli-<platform>-<version>... for the terminal).
//!
//! Linux and Windows replace the running executable. macOS replaces the whole
//! unfr0zen.app the window runs from, so its signature and Info.plist come
//! with it; the terminal tool there is a plain executable like elsewhere.

use std::fs;
use std::path::{Path, PathBuf};

use self_update::backends::github;
use self_update::{Download, Extract, ReleaseAsset};
use unfr0zen_core::{Error, Result};

pub const REPO_OWNER: &str = "will-tm";
pub const REPO_NAME: &str = "unfr0zen";

/// This build's platform, in the release files' names.
pub const PLATFORM: &str = if cfg!(target_os = "macos") {
    "macos-universal"
} else if cfg!(windows) {
    "windows-x86_64"
} else if cfg!(target_arch = "aarch64") {
    "linux-arm64"
} else {
    "linux-x86_64"
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum App {
    Gui,
    Cli,
}

impl App {
    /// The start of this app's release file name on this platform.
    fn asset_prefix(self) -> String {
        match self {
            App::Gui => format!("unfr0zen-{PLATFORM}-"),
            App::Cli => format!("unfr0zen-cli-{PLATFORM}-"),
        }
    }

    /// The executable inside the release file.
    fn executable(self) -> &'static str {
        match (self, cfg!(windows)) {
            (App::Gui, false) => "unfr0zen-gui",
            (App::Gui, true) => "unfr0zen-gui.exe",
            (App::Cli, false) => "unfr0zen",
            (App::Cli, true) => "unfr0zen.exe",
        }
    }
}

/// A newer release than this build.
#[derive(Clone, Debug, PartialEq)]
pub struct Available {
    /// Its tag, e.g. v0.1.6.
    pub tag: String,
    /// Its page on GitHub.
    pub url: String,
    asset: String,
    download: String,
}

/// The semver of a release build's version ("v0.1.5" → "0.1.5"), or None for
/// a development build, whose git describe has commits past a tag, no tag, or
/// local changes: those are never offered an update.
pub fn release_version(build: &str) -> Option<&str> {
    let version = build.strip_prefix('v')?;
    let (core, rc) = version.split_once("-rc").map_or((version, None), |(core, rc)| (core, Some(rc)));
    let numbers: Vec<&str> = core.split('.').collect();
    let plain = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (numbers.len() == 3 && numbers.iter().all(|n| plain(n)) && rc.is_none_or(plain)).then_some(version)
}

/// The release file for `app` on this platform among `names`.
fn pick_asset(app: App, names: &[&str]) -> Option<usize> {
    let prefix = app.asset_prefix();
    names.iter().position(|name| name.starts_with(&prefix) && (name.ends_with(".zip") || name.ends_with(".tar.gz")))
}

fn failed(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Device(format!("update: {what}: {e}"))
}

/// The latest release, if it is newer than `build` and has a file for `app`
/// on this platform. Development builds get None without asking GitHub.
pub fn check(app: App, build: &str) -> Result<Option<Available>> {
    let Some(current) = release_version(build) else { return Ok(None) };
    let updater = github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(app.executable())
        .current_version(current)
        .show_output(false)
        .no_confirm(true)
        .build()
        .map_err(|e| failed("configure", e))?;
    let latest = updater.get_latest_release().map_err(|e| failed("latest release", e))?
        .into_vec().into_iter().next();
    let Some(latest) = latest else { return Ok(None) };
    if !self_update::version::bump_is_greater(current, latest.version()).unwrap_or(false) {
        return Ok(None);
    }
    let assets: &[ReleaseAsset] = latest.assets();
    let names: Vec<&str> = assets.iter().map(|a| a.name()).collect();
    let Some(index) = pick_asset(app, &names) else { return Ok(None) };
    let tag = format!("v{}", latest.version());
    Ok(Some(Available {
        url: latest.release_notes_url().map_or_else(
            || format!("https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/tag/{tag}"), str::to_string),
        tag,
        asset: assets[index].name().to_string(),
        download: assets[index].download_url().to_string(),
    }))
}

/// Downloads `release` and puts it in place of the running app. The new
/// version runs from the next start.
pub fn install(app: App, release: &Available) -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| failed("current executable", e))?;
    let bundle = if cfg!(target_os = "macos") && app == App::Gui { Some(app_bundle(&exe)?) } else { None };
    // Beside what it replaces, so the final renames stay on one volume.
    let parent = bundle.as_deref().unwrap_or(&exe).parent()
        .ok_or_else(|| failed("install directory", exe.display()))?;
    let work = tempfile::Builder::new().prefix(".unfr0zen-update-").tempdir_in(parent)
        .map_err(|e| failed(&format!("{} is not writable", parent.display()), e))?;

    let archive = work.path().join(&release.asset);
    let file = fs::File::create(&archive).map_err(|e| failed("download", e))?;
    // The asset's API URL answers with the file itself only when asked for one.
    Download::from_url(&release.download).request_header("Accept", "application/octet-stream")
        .download_to(file).map_err(|e| failed("download", e))?;
    let unpacked = work.path().join("unpacked");
    Extract::from_source(&archive).extract_into(&unpacked).map_err(|e| failed("unpack", e))?;

    match bundle {
        Some(bundle) => swap_bundle(&bundle, &unpacked.join("unfr0zen.app"), work.path()),
        None => {
            let new = unpacked.join(app.executable());
            if !new.is_file() {
                return Err(failed("unpack", format!("{} has no {}", release.asset, app.executable())));
            }
            self_replace::self_replace(&new).map_err(|e| failed("replace", e))
        }
    }
}

/// The unfr0zen.app holding `exe` (…/unfr0zen.app/Contents/MacOS/unfr0zen).
fn app_bundle(exe: &Path) -> Result<PathBuf> {
    exe.ancestors().nth(3)
        .filter(|bundle| bundle.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
        .ok_or_else(|| failed("not running from an .app bundle", exe.display()))
}

/// Puts `new` where `bundle` is, moving the old one into `work` first and
/// back if the new one cannot take its place.
fn swap_bundle(bundle: &Path, new: &Path, work: &Path) -> Result<()> {
    if !new.join("Contents/Info.plist").is_file() {
        return Err(failed("unpack", format!("{} is not an app bundle", new.display())));
    }
    let old = work.join("old.app");
    fs::rename(bundle, &old).map_err(|e| failed("move the current app aside", e))?;
    if let Err(e) = fs::rename(new, bundle) {
        let _ = fs::rename(&old, bundle);
        return Err(failed("move the new app in place", e));
    }
    Ok(())
}

/// Starts the app that is now installed, for after this process has let go
/// of its single-instance lock.
pub fn relaunch(app: App) -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| failed("current executable", e))?;
    let mut command = if cfg!(target_os = "macos") && app == App::Gui {
        let mut open = std::process::Command::new("open");
        open.arg("-n").arg(app_bundle(&exe)?);
        open
    } else {
        std::process::Command::new(exe)
    };
    command.spawn().map(drop).map_err(|e| failed("restart", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_release_builds_have_a_release_version() {
        assert_eq!(release_version("v0.1.5"), Some("0.1.5"));
        assert_eq!(release_version("v1.20.300-rc2"), Some("1.20.300-rc2"));
        for build in ["v0.1.5-3-g9d49c21", "v0.1.5-dirty", "v0.1.5-3-g9d49c21-dirty", "9d49c21", "0.1.5",
                      "v0.1", "v0.1.5.1", "v0.1.x", "v0.1.5-rc", "v0.1.5-rc1-2-gabc", "v0.1.5-beta1", ""] {
            assert_eq!(release_version(build), None, "{build}");
        }
    }

    #[test]
    fn each_app_takes_only_its_own_file() {
        let names = [
            format!("unfr0zen-cli-{PLATFORM}-v0.1.6.tar.gz"),
            format!("unfr0zen-{PLATFORM}-v0.1.6.zip"),
            format!("unfr0zen-{PLATFORM}-v0.1.6.tar.gz"),
            "unfr0zen-other-platform-v0.1.6.zip".to_string(),
        ];
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        assert_eq!(pick_asset(App::Cli, &names), Some(0));
        assert_eq!(pick_asset(App::Gui, &names), Some(1));
        assert_eq!(pick_asset(App::Gui, &names[2..]), Some(0));
        assert_eq!(pick_asset(App::Gui, &["unfr0zen-other-platform-v0.1.6.zip", "notes.txt"]), None);
        assert_eq!(pick_asset(App::Cli, &[&format!("unfr0zen-cli-{PLATFORM}-v0.1.6.sha256")]), None);
    }

    #[test]
    fn a_development_build_never_asks() {
        assert_eq!(check(App::Gui, "v0.1.5-3-g9d49c21-dirty").unwrap(), None);
    }

    #[test]
    fn the_bundle_is_three_levels_above_its_executable() {
        assert_eq!(app_bundle(Path::new("/Applications/unfr0zen.app/Contents/MacOS/unfr0zen")).unwrap(),
                   Path::new("/Applications/unfr0zen.app"));
        assert!(app_bundle(Path::new("/usr/local/bin/unfr0zen-gui")).is_err());
    }

    #[test]
    fn a_bundle_swap_replaces_the_whole_bundle_or_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let app = |root: &Path, version: &str| {
            fs::create_dir_all(root.join("Contents/MacOS")).unwrap();
            fs::write(root.join("Contents/Info.plist"), version).unwrap();
            fs::write(root.join("Contents/MacOS/unfr0zen"), version).unwrap();
        };
        let bundle = dir.path().join("unfr0zen.app");
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();
        app(&bundle, "old");
        app(&work.join("new.app"), "new");
        fs::write(bundle.join("Contents/MacOS/leftover"), "").unwrap();

        swap_bundle(&bundle, &work.join("new.app"), &work).unwrap();
        assert_eq!(fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap(), "new");
        assert!(!bundle.join("Contents/MacOS/leftover").exists());

        // Not a bundle: nothing moves.
        fs::create_dir(work.join("junk")).unwrap();
        assert!(swap_bundle(&bundle, &work.join("junk"), &work).is_err());
        assert_eq!(fs::read_to_string(bundle.join("Contents/MacOS/unfr0zen")).unwrap(), "new");
    }
}
