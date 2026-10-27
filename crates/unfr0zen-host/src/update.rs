//! Self-update from the GitHub releases of unfr0zen/unfr0zen.
//!
//! Checks the latest stable release (no drafts, no -rc pre-releases) for a
//! file matching this platform.  Both the window and the terminal use the
//! same release asset (`unfr0zen-<platform>-<version>`); the distinction
//! only matters for install: macOS GUI swaps the whole .app bundle, everything
//! else replaces the executable via `self-replace`.

use std::fs;
use std::path::{Path, PathBuf};

use self_update::backends::github;
use self_update::{Download, Extract};
use unfr0zen_core::{Error, Result};

pub const REPO_OWNER: &str = "unfr0zen";
pub const REPO_NAME: &str = "unfr0zen";

/// This build's platform tag, matching the CI asset names.
pub const PLATFORM: &str = if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
    "windows-arm64"
} else if cfg!(windows) {
    "windows-x86_64"
} else if cfg!(target_arch = "aarch64") {
    "linux-arm64"
} else if cfg!(target_arch = "riscv64") {
    "linux-riscv64"
} else {
    "linux-x86_64"
};

/// Which front end is updating itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum App {
    Gui,
    Cli,
}

impl App {
    /// The executable name inside the release archive.
    fn executable(self) -> &'static str {
        if cfg!(windows) { "unfr0zen.exe" } else { "unfr0zen" }
    }
}

/// A newer release than this build.
#[derive(Clone, Debug, PartialEq)]
pub struct Available {
    /// The tag, e.g. "v0.1.6".
    pub tag: String,
    /// The release page on GitHub.
    pub url: String,
    /// The release notes (GitHub body markdown), if any.
    pub body: Option<String>,
    pub(crate) asset: String,
    pub(crate) download: String,
}

/// The semver of a release build's version ("v0.1.5" → "0.1.5"), or None for
/// a development build whose git-describe has commits past a tag, no tag, or
/// local changes: those never update.
pub fn release_version(build: &str) -> Option<&str> {
    let version = build.strip_prefix('v')?;
    let (core, rc) = version.split_once("-rc")
        .map_or((version, None), |(core, rc)| (core, Some(rc)));
    let numbers: Vec<&str> = core.split('.').collect();
    let plain = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (numbers.len() == 3 && numbers.iter().all(|n| plain(n)) && rc.is_none_or(plain))
        .then_some(version)
}

/// Picks the release asset for this platform from a list of file names.
/// On macOS, prefers the universal build; falls back to the arch-specific one.
fn pick_asset(names: &[&str]) -> Option<usize> {
    let is_archive = |n: &str| n.ends_with(".zip") || n.ends_with(".tar.gz");
    if cfg!(target_os = "macos") {
        let arch = if cfg!(target_arch = "aarch64") { "macos-arm64-" } else { "macos-x86_64-" };
        names.iter().position(|n| n.starts_with("unfr0zen-macos-universal-") && is_archive(n))
            .or_else(|| {
                let prefix = format!("unfr0zen-{arch}");
                names.iter().position(|n| n.starts_with(&prefix) && is_archive(n))
            })
    } else {
        let prefix = format!("unfr0zen-{PLATFORM}-");
        names.iter().position(|n| n.starts_with(&prefix) && is_archive(n))
    }
}

/// A release fetched from GitHub, stripped to what `evaluate` needs.
struct FetchedRelease {
    version: String,
    /// (file name, download URL).
    assets: Vec<(String, String)>,
    notes_url: Option<String>,
    body: Option<String>,
}

/// Pure logic: given the current version and a fetched release, returns
/// `Some(Available)` if it is newer and has an asset for this platform.
fn evaluate(current: &str, release: &FetchedRelease) -> Option<Available> {
    if !self_update::version::bump_is_greater(current, &release.version).unwrap_or(false) {
        return None;
    }
    let names: Vec<&str> = release.assets.iter().map(|(n, _)| n.as_str()).collect();
    let index = pick_asset(&names)?;
    let tag = format!("v{}", release.version);
    Some(Available {
        url: release.notes_url.clone().unwrap_or_else(
            || format!("https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/tag/{tag}")),
        tag,
        body: release.body.clone(),
        asset: release.assets[index].0.clone(),
        download: release.assets[index].1.clone(),
    })
}

fn failed(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Device(format!("update: {what}: {e}"))
}

/// The latest release, if it is newer than `build` and has a file for this
/// platform.  Development builds get None without asking GitHub.
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
    let latest = updater.get_latest_release()
        .map_err(|e| failed("latest release", e))?
        .into_vec().into_iter().next();
    let Some(latest) = latest else { return Ok(None) };
    let fetched = FetchedRelease {
        version: latest.version().to_string(),
        assets: latest.assets().iter()
            .map(|a| (a.name().to_string(), a.download_url().to_string()))
            .collect(),
        notes_url: latest.release_notes_url().map(str::to_string),
        body: latest.body().map(str::to_string),
    };
    Ok(evaluate(current, &fetched))
}

/// Downloads `release` and puts it in place of the running app.
pub fn install(app: App, release: &Available) -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| failed("current executable", e))?;
    let bundle = if cfg!(target_os = "macos") && app == App::Gui {
        app_bundle(&exe).ok()
    } else {
        None
    };
    let parent = bundle.as_deref().unwrap_or(&exe).parent()
        .ok_or_else(|| failed("install directory", exe.display()))?;
    let work = tempfile::Builder::new().prefix(".unfr0zen-update-").tempdir_in(parent)
        .map_err(|e| failed(&format!("{} is not writable", parent.display()), e))?;

    let archive = work.path().join(&release.asset);
    let file = fs::File::create(&archive).map_err(|e| failed("download", e))?;
    Download::from_url(&release.download)
        .request_header("Accept", "application/octet-stream")
        .download_to(file)
        .map_err(|e| failed("download", e))?;
    install_archive(app, &archive, work.path(), bundle.as_deref(), &exe)
}

/// Extracts the archive and puts the result in place.  Factored out of
/// `install` so tests can skip the download.
fn install_archive(
    app: App, archive: &Path, work: &Path, bundle: Option<&Path>, exe: &Path,
) -> Result<()> {
    let unpacked = work.join("unpacked");
    Extract::from_source(archive).extract_into(&unpacked)
        .map_err(|e| failed("unpack", e))?;
    match bundle {
        Some(bundle) => swap_bundle(bundle, &unpacked.join("unfr0zen.app"), work),
        None => {
            let exe_name = app.executable();
            // The macOS archive contains unfr0zen.app, not a bare executable.
            let new = [
                unpacked.join(exe_name),
                unpacked.join(format!("unfr0zen.app/Contents/MacOS/{exe_name}")),
            ].into_iter().find(|p| p.is_file());
            let Some(new) = new else {
                return Err(failed("unpack",
                    format!("{} has no {exe_name}", archive.display())));
            };
            // In production this replaces the running binary; tests pass a
            // fake exe path and verify the archive was extracted correctly.
            if *exe == std::env::current_exe().unwrap_or_default() {
                self_replace::self_replace(&new).map_err(|e| failed("replace", e))
            } else {
                fs::copy(&new, exe).map(drop).map_err(|e| failed("replace", e))
            }
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

/// Replaces `bundle` with `new`, moving the old one into `work` first and
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

/// Starts the installed app, for after this process releases its instance lock.
pub fn relaunch(app: App) -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| failed("current executable", e))?;
    let mut command = if cfg!(target_os = "macos") && app == App::Gui {
        if let Ok(bundle) = app_bundle(&exe) {
            let mut open = std::process::Command::new("open");
            open.arg("-n").arg(bundle);
            open
        } else {
            std::process::Command::new(exe)
        }
    } else {
        std::process::Command::new(exe)
    };
    command.spawn().map(drop).map_err(|e| failed("restart", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── version parsing ─────────────────────────────────────────────

    #[test]
    fn release_builds_have_a_version() {
        assert_eq!(release_version("v0.1.5"), Some("0.1.5"));
        assert_eq!(release_version("v1.20.300"), Some("1.20.300"));
        assert_eq!(release_version("v1.20.300-rc2"), Some("1.20.300-rc2"));
    }

    #[test]
    fn development_builds_have_no_version() {
        for build in [
            "v0.1.5-3-g9d49c21", "v0.1.5-dirty", "v0.1.5-3-g9d49c21-dirty",
            "9d49c21", "0.1.5", "v0.1", "v0.1.5.1", "v0.1.x", "v0.1.5-rc",
            "v0.1.5-rc1-2-gabc", "v0.1.5-beta1", "",
        ] {
            assert_eq!(release_version(build), None, "{build}");
        }
    }

    #[test]
    fn dev_build_skips_check_without_network() {
        assert_eq!(check(App::Gui, "v0.1.5-3-g9d49c21-dirty").unwrap(), None);
        assert_eq!(check(App::Cli, "abcdef0").unwrap(), None);
    }

    // ── asset picking ───────────────────────────────────────────────

    #[test]
    fn picks_the_right_asset_for_this_platform() {
        let names = &[
            "unfr0zen-linux-x86_64-v0.1.8.tar.gz",
            "unfr0zen-linux-arm64-v0.1.8.tar.gz",
            "unfr0zen-linux-riscv64-v0.1.8.tar.gz",
            "unfr0zen-windows-x86_64-v0.1.8.zip",
            "unfr0zen-windows-arm64-v0.1.8.zip",
            "unfr0zen-macos-universal-v0.1.8.zip",
            "unfr0zen-macos-arm64-v0.1.8.zip",
            "unfr0zen-macos-x86_64-v0.1.8.zip",
        ];
        let i = pick_asset(names).expect("should find an asset for this platform");
        let picked = names[i];
        let expected_prefix = if cfg!(target_os = "macos") {
            "unfr0zen-macos-universal"
        } else {
            &format!("unfr0zen-{PLATFORM}")
        };
        assert!(picked.starts_with(expected_prefix),
                "picked {picked} but expected prefix {expected_prefix}");
    }

    #[test]
    fn ignores_non_archive_files() {
        assert_eq!(pick_asset(&["unfr0zen-linux-x86_64-v0.1.8.sha256", "notes.txt"]), None);
    }

    #[test]
    fn ignores_unrelated_platforms() {
        let other = if cfg!(target_os = "macos") {
            "unfr0zen-linux-x86_64-v0.1.8.tar.gz"
        } else if cfg!(target_os = "linux") {
            "unfr0zen-windows-x86_64-v0.1.8.zip"
        } else {
            "unfr0zen-linux-arm64-v0.1.8.tar.gz"
        };
        assert_eq!(pick_asset(&[other]), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_prefers_universal_falls_back_to_arch() {
        let this_arch = if cfg!(target_arch = "aarch64") {
            "unfr0zen-macos-arm64-v0.1.8.zip"
        } else {
            "unfr0zen-macos-x86_64-v0.1.8.zip"
        };
        let uni = "unfr0zen-macos-universal-v0.1.8.zip";
        assert_eq!(pick_asset(&[this_arch, uni]).map(|i| [this_arch, uni][i]), Some(uni));
        assert!(pick_asset(&[this_arch]).is_some());
        let other_arch = if cfg!(target_arch = "aarch64") {
            "unfr0zen-macos-x86_64-v0.1.8.zip"
        } else {
            "unfr0zen-macos-arm64-v0.1.8.zip"
        };
        assert_eq!(pick_asset(&[other_arch]), None);
    }

    #[test]
    fn platform_is_a_known_ci_value() {
        assert!(
            ["linux-x86_64", "linux-arm64", "linux-riscv64",
             "windows-x86_64", "windows-arm64", "macos"].contains(&PLATFORM),
            "PLATFORM={PLATFORM} is not a CI platform"
        );
    }

    // ── evaluate (version comparison + asset matching) ──────────────

    fn fake_release(version: &str) -> FetchedRelease {
        let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
        let asset = if cfg!(target_os = "macos") {
            format!("unfr0zen-macos-universal-v{version}.{ext}")
        } else {
            format!("unfr0zen-{PLATFORM}-v{version}.{ext}")
        };
        FetchedRelease {
            version: version.to_string(),
            assets: vec![(asset, format!("https://example.com/{version}"))],
            notes_url: None,
            body: None,
        }
    }

    #[test]
    fn evaluate_finds_newer_release() {
        let release = fake_release("0.2.0");
        let available = evaluate("0.1.5", &release).expect("should find a newer release");
        assert_eq!(available.tag, "v0.2.0");
    }

    #[test]
    fn evaluate_skips_same_version() {
        assert_eq!(evaluate("0.1.5", &fake_release("0.1.5")), None);
    }

    #[test]
    fn evaluate_skips_older_version() {
        assert_eq!(evaluate("0.2.0", &fake_release("0.1.5")), None);
    }

    #[test]
    fn evaluate_skips_when_no_matching_asset() {
        let release = FetchedRelease {
            version: "0.2.0".into(),
            assets: vec![("unfr0zen-other-platform-v0.2.0.zip".into(), "url".into())],
            notes_url: None,
            body: None,
        };
        assert_eq!(evaluate("0.1.5", &release), None);
    }

    #[test]
    fn evaluate_builds_correct_url() {
        let available = evaluate("0.1.5", &fake_release("0.2.0")).unwrap();
        assert!(available.url.contains("unfr0zen/unfr0zen"), "url={}", available.url);
        assert!(available.url.contains("v0.2.0"), "url={}", available.url);
    }

    // ── bundle path ─────────────────────────────────────────────────

    #[test]
    fn bundle_is_three_levels_above_executable() {
        assert_eq!(
            app_bundle(Path::new("/Applications/unfr0zen.app/Contents/MacOS/unfr0zen")).unwrap(),
            Path::new("/Applications/unfr0zen.app"),
        );
        assert!(app_bundle(Path::new("/usr/local/bin/unfr0zen")).is_err());
    }

    // ── bundle swap ─────────────────────────────────────────────────

    fn make_bundle(root: &Path, version: &str) {
        fs::create_dir_all(root.join("Contents/MacOS")).unwrap();
        fs::write(root.join("Contents/Info.plist"), version).unwrap();
        fs::write(root.join("Contents/MacOS/unfr0zen"), version).unwrap();
    }

    #[test]
    fn bundle_swap_replaces_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("unfr0zen.app");
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();
        make_bundle(&bundle, "old");
        make_bundle(&work.join("new.app"), "new");
        fs::write(bundle.join("Contents/MacOS/leftover"), "").unwrap();

        swap_bundle(&bundle, &work.join("new.app"), &work).unwrap();
        assert_eq!(fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap(), "new");
        assert!(!bundle.join("Contents/MacOS/leftover").exists());
    }

    #[test]
    fn bundle_swap_rolls_back_on_bad_source() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("unfr0zen.app");
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();
        make_bundle(&bundle, "current");
        fs::create_dir(work.join("junk")).unwrap();

        assert!(swap_bundle(&bundle, &work.join("junk"), &work).is_err());
        assert_eq!(fs::read_to_string(bundle.join("Contents/MacOS/unfr0zen")).unwrap(), "current");
    }

    // ── install (archive extraction + replacement) ──────────────────

    fn make_tar_gz(dir: &Path, archive_name: &str, inner_name: &str, content: &[u8]) -> PathBuf {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let path = dir.join(archive_name);
        let file = fs::File::create(&path).unwrap();
        let enc = GzEncoder::new(file, Compression::fast());
        let mut ar = tar::Builder::new(enc);
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        ar.append_data(&mut header, inner_name, content).unwrap();
        ar.finish().unwrap();
        path
    }

    fn make_zip(dir: &Path, archive_name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
        use std::io::Write;
        let path = dir.join(archive_name);
        let file = fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (name, content) in entries {
            if name.ends_with('/') {
                zip.add_directory::<_, ()>(*name, Default::default()).unwrap();
            } else {
                zip.start_file::<_, ()>(*name, Default::default()).unwrap();
                zip.write_all(content).unwrap();
            }
        }
        zip.finish().unwrap();
        path
    }

    /// Linux/riscv64 path: tar.gz with a bare executable.
    #[test]
    fn install_from_tar_gz() {
        let dir = tempfile::tempdir().unwrap();
        let exe_name = App::Cli.executable();
        let fake_exe = dir.path().join(exe_name);
        fs::write(&fake_exe, b"old").unwrap();

        let archive = make_tar_gz(dir.path(), "update.tar.gz", exe_name, b"new-version");
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();

        install_archive(App::Cli, &archive, &work, None, &fake_exe).unwrap();
        assert_eq!(fs::read_to_string(&fake_exe).unwrap(), "new-version");
    }

    /// Windows path: zip with a bare executable.
    #[test]
    fn install_from_zip() {
        let dir = tempfile::tempdir().unwrap();
        let exe_name = App::Cli.executable();
        let fake_exe = dir.path().join(exe_name);
        fs::write(&fake_exe, b"old").unwrap();

        let archive = make_zip(dir.path(), "update.zip", &[(exe_name, b"new-version")]);
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();

        install_archive(App::Cli, &archive, &work, None, &fake_exe).unwrap();
        assert_eq!(fs::read_to_string(&fake_exe).unwrap(), "new-version");
    }

    /// macOS CLI path: the archive has unfr0zen.app, not a bare executable;
    /// install_archive finds the binary inside the bundle.
    #[test]
    fn install_cli_from_macos_bundle_archive() {
        let dir = tempfile::tempdir().unwrap();
        let exe_name = App::Cli.executable();
        let fake_exe = dir.path().join(exe_name);
        fs::write(&fake_exe, b"old").unwrap();

        let archive = make_zip(dir.path(), "update.zip", &[
            ("unfr0zen.app/Contents/MacOS/", b""),
            ("unfr0zen.app/Contents/Info.plist", b"v0.2.0"),
            (&format!("unfr0zen.app/Contents/MacOS/{exe_name}"), b"new-cli"),
        ]);
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();

        install_archive(App::Cli, &archive, &work, None, &fake_exe).unwrap();
        assert_eq!(fs::read_to_string(&fake_exe).unwrap(), "new-cli");
    }

    #[test]
    fn install_archive_fails_on_missing_executable() {
        let dir = tempfile::tempdir().unwrap();
        let fake_exe = dir.path().join(App::Cli.executable());
        fs::write(&fake_exe, b"old").unwrap();

        let archive = make_tar_gz(dir.path(), "bad.tar.gz", "wrong-name", b"data");
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();

        let err = install_archive(App::Cli, &archive, &work, None, &fake_exe).unwrap_err();
        assert!(err.to_string().contains("has no"), "expected 'has no' error, got: {err}");
    }

    /// macOS GUI path: swaps the whole .app bundle.
    #[test]
    fn install_archive_swaps_bundle_from_zip() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("unfr0zen.app");
        make_bundle(&bundle, "old");
        let fake_exe = bundle.join("Contents/MacOS/unfr0zen");

        let archive = make_zip(dir.path(), "update.zip", &[
            ("unfr0zen.app/Contents/MacOS/", b""),
            ("unfr0zen.app/Contents/Info.plist", b"v0.2.0"),
            ("unfr0zen.app/Contents/MacOS/unfr0zen", b"new-binary"),
        ]);
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();

        install_archive(App::Gui, &archive, &work, Some(&bundle), &fake_exe).unwrap();
        assert_eq!(fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap(), "v0.2.0");
        assert_eq!(fs::read_to_string(bundle.join("Contents/MacOS/unfr0zen")).unwrap(), "new-binary");
    }

    #[test]
    fn install_archive_rejects_invalid_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("unfr0zen.app");
        make_bundle(&bundle, "old");
        let fake_exe = bundle.join("Contents/MacOS/unfr0zen");

        // Zip without Contents/Info.plist.
        let archive = make_zip(dir.path(), "bad.zip", &[
            ("unfr0zen.app/Contents/MacOS/unfr0zen", b"bin"),
        ]);
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();

        let err = install_archive(App::Gui, &archive, &work, Some(&bundle), &fake_exe).unwrap_err();
        assert!(err.to_string().contains("not an app bundle"), "got: {err}");
        // Original bundle is still there.
        assert_eq!(fs::read_to_string(bundle.join("Contents/MacOS/unfr0zen")).unwrap(), "old");
    }
}
