use std::cell::RefCell;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;

use serde_json::json;
use unfr0zen_core::bundle::{self, Bundle, Settings};
use unfr0zen_core::diamond3::scratch;
use unfr0zen_core::local::LocalVolume;
use unfr0zen_core::{nestlocal, nlclient, setup, Device, Error, Result};

struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }
    /// Each line with the seconds since the run started: a host that loses
    /// the unit midway still has a timeline of what arrived.
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let at = STARTED.get_or_init(std::time::Instant::now).elapsed().as_secs_f32();
            eprintln!("{} [{at:.1}s]: {}", record.level(), record.args());
        }
    }
    fn flush(&self) {}
}

static LOGGER: Logger = Logger;
static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

pub fn main() {
    STARTED.get_or_init(std::time::Instant::now);
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|a| a == name);
    let (dry_run, live, remove_bundle) = (flag("--dry-run"), flag("--live"), flag("--remove-bundle"));
    let paths: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.starts_with("--")).collect();
    if let Some(path) = args.iter().find_map(|a| a.strip_prefix("--entropy=")) {
        if let Err(error) = credit_entropy(Path::new(path)) {
            eprintln!("unfr0zen-apply: {path}: {error}");
            std::process::exit(1);
        }
    }
    let result = match paths[..] {
        ["update", bundle] => update(Path::new(bundle)),
        [bundle, options, root, out] => {
            let generation = args.iter().find_map(|a| a.strip_prefix("--device="));
            let paths = (Path::new(bundle), Path::new(options), Path::new(root), Path::new(out));
            run(paths, dry_run, live, remove_bundle, generation)
        }
        _ => {
            eprintln!(
                "usage: unfr0zen-apply BUNDLE OPTIONS ROOT OUT [--dry-run] [--live] [--remove-bundle] [--device=NAME] \
                       [--entropy=FILE] \
                       | update BUNDLE"
            );
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("unfr0zen-apply: {error}");
        std::process::exit(1);
    }
}

/// Credits the kernel's pool with the host's random bytes in `path`. An
/// initramfs on the gen 1 and gen 2 (Linux 2.6.37, without getrandom(2))
/// gathers too little entropy for /dev/random to be ready, which the setup's
/// random seed and salts wait for.
#[cfg(target_os = "linux")]
fn credit_entropy(path: &Path) -> Result<()> {
    /// _IOW('R', 0x03, int[2]), from linux/random.h.
    const RNDADDENTROPY: libc::c_int = 0x4008_5203;
    let bytes = fs::read(path)?;
    let mut info = Vec::with_capacity(8 + bytes.len());
    info.extend_from_slice(&((bytes.len() * 8) as i32).to_ne_bytes());
    info.extend_from_slice(&(bytes.len() as i32).to_ne_bytes());
    info.extend_from_slice(&bytes);
    let random = fs::OpenOptions::new().write(true).open("/dev/random")?;
    // SAFETY: info is a struct rand_pool_info: entropy bits, buffer size, then the buffer.
    if unsafe { libc::ioctl(std::os::fd::AsRawFd::as_raw_fd(&random), RNDADDENTROPY as _, info.as_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    fs::remove_file(path)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn credit_entropy(_: &Path) -> Result<()> {
    Err(failure("crediting entropy needs Linux"))
}

fn failure(message: impl Into<String>) -> Error {
    Error::Device(message.into())
}

fn device(name: &str) -> Result<Device> {
    match name {
        "diamond3" => Ok(Device::Diamond3),
        "j49" => Ok(Device::J49),
        "diamond" => Ok(Device::Diamond),
        other => Err(failure(format!("the bundle is for a {other}, which unfr0zen-apply does not set up"))),
    }
}

/// Runs one of the bundle's scripts from OUT, with the run's details in its environment.
fn script(bundle: &Bundle, name: &str, out: &Path, env: &[(&str, String)]) -> Result<()> {
    script_data(bundle.file(name)?, name, out, env)
}

fn script_data(data: &[u8], name: &str, out: &Path, env: &[(&str, String)]) -> Result<()> {
    let path = out.join(Path::new(name).file_name().unwrap());
    fs::write(&path, data)?;
    log::info!("Running {name}");
    let status = Command::new("sh").arg(&path).envs(env.iter().map(|(k, v)| (*k, v))).status()?;
    if !status.success() {
        return Err(failure(format!("{name} failed ({status})")));
    }
    Ok(())
}

/// The unit's settings, as the last run kept them.
fn read_settings(path: &Path) -> Result<Settings> {
    serde_json::from_slice(&fs::read(path).map_err(|e| failure(format!("{}: {e}", path.display())))?)
        .map_err(|e| failure(format!("{}: {e}", path.display())))
}

/// Checks a new bundle and runs its own engine on the running unit.
fn update(path: &Path) -> Result<()> {
    // A gen 1 or gen 2 gives the update nlclient's memory before either
    // engine reads the bundle: the setup reads, patches and writes nlclient
    // too, and with nlclient's own memory in use the kernel panics on running
    // out (Nest's kernel sets panic_on_oom).
    let target = device(&Bundle::read_manifest(fs::File::open(path)?)?.device)?;
    let nlclient = matches!(target, Device::J49 | Device::Diamond).then(NlclientStopped::stop).transpose()?;
    // This engine keeps only the update's engine, which reads the bundle again.
    let mut bundle = Bundle::read_some(fs::File::open(path)?, |name| name == bundle::APPLY)?;
    device(&bundle.manifest.device)?;
    log::info!("Update to {} for the {}", bundle.manifest.version, bundle.manifest.device);
    let options = Path::new("/").join(setup::RECORD);
    let settings = read_settings(&options)?;
    match bundle.verify(&bundle::verifying_key(bundle::RELEASE_KEY)?) {
        Ok(()) => log::info!("Signed with the release key"),
        Err(error) if settings.unsigned_updates => {
            log::warn!("{error}: installed anyway, as unsigned updates are allowed")
        }
        Err(error) => return Err(failure(format!("{error}: only updates the release key signs install on this unit"))),
    }
    // Going back is refused: an older release, signed as it is, could
    // bring back what a later one fixed. A developer's unit allows it.
    let (installed, new) = (settings.version.as_str(), bundle.manifest.version.as_str());
    match (bundle::is_downgrade(installed, new), bundle::is_downgrade(installed, installed)) {
        (Some(false), _) => {}
        (_, _) if settings.unsigned_updates => {
            log::warn!("{new} over {installed}: installed anyway, as this unit allows it")
        }
        (Some(true), _) => return Err(failure(format!("{new} is older than {installed}, which this unit runs"))),
        // Nothing to compare with: the installed version is not one we order.
        (None, None) => {}
        (None, Some(_)) => return Err(failure(format!("{new} is not a version to compare with {installed}"))),
    }
    let engine =
        bundle.take(bundle::APPLY).map_or_else(|| Err(failure(format!("bundle: no {}", bundle::APPLY))), Ok)?;
    drop(bundle);
    // The run's output, on scratch as tmpfs is the unit's memory: a failed
    // run's stays for its diagnosis until the next update.
    let out = Path::new("/").join(scratch::DIR).join("update-run");
    match fs::remove_dir_all(&out) {
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    let status = run_from_memory(engine, |engine| {
        Command::new(engine).arg(path).arg(&options).arg("/").arg(&out).arg("--live").status()
    })?;
    if !status.success() {
        return Err(failure(format!("the update's engine failed ({status})")));
    }
    let _ = fs::remove_dir_all(&out);
    // The caller restarts the unit into the new setup.
    if let Some(nlclient) = nlclient {
        nlclient.leave_stopped();
    }
    Ok(())
}

/// nlclient stopped while a 64 MB unit (the gen 1 and gen 2) updates itself
/// as it runs. Started again when this is dropped, as the update failed,
/// unless left stopped for the restart into the new setup.
struct NlclientStopped;

impl NlclientStopped {
    /// Fails, nlclient started again, when it does not stop: the update would
    /// run out of memory beside it.
    fn stop() -> Result<NlclientStopped> {
        let stopped = NlclientStopped;
        let status = Command::new("monit").args(["stop", "nlclient"]).status()?;
        if !status.success() {
            return Err(failure(format!("monit could not stop nlclient ({status})")));
        }
        // monit stops it in the background: wait for it to be gone.
        for _ in 0..60 {
            if !Command::new("pidof").arg("nlclient").output()?.status.success() {
                log::info!("nlclient stopped for the update");
                return Ok(stopped);
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        Err(failure("nlclient still runs 30 s after monit stopped it"))
    }

    fn leave_stopped(self) {
        std::mem::forget(self);
    }
}

impl Drop for NlclientStopped {
    fn drop(&mut self) {
        // monit refuses an action while the stop is still its own business
        // ("Other action already in progress"): asked again until it takes it.
        for _ in 0..60 {
            if Command::new("monit").args(["start", "nlclient"]).status().is_ok_and(|s| s.success()) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        log::error!("monit would not start nlclient again: restart the unit");
    }
}

/// Runs an executable held in memory: /tmp and the unit's other writable
/// places are noexec. run gets the path to execute it by. A kernel without
/// memfd_create (the gen 1's and gen 2's 2.6.37) runs it from beside this
/// engine instead, on their scratch volume, which allows executables. The
/// copy in this process's memory is freed before it runs.
#[cfg(target_os = "linux")]
fn run_from_memory<T>(executable: Vec<u8>, run: impl FnOnce(&str) -> std::io::Result<T>) -> Result<T> {
    use std::io::Write;
    use std::os::fd::FromRawFd;
    // SAFETY: the name is NUL-terminated; a failed call returns -1, checked.
    let fd = unsafe { libc::memfd_create(c"unfr0zen-apply".as_ptr(), 0) };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOSYS) {
            return Err(error.into());
        }
        let path = std::env::current_exe()?.with_file_name("unfr0zen-apply.update");
        fs::write(&path, &executable)?;
        drop(executable);
        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        let ran = run(&path.display().to_string());
        let _ = fs::remove_file(&path);
        return Ok(ran?);
    }
    // SAFETY: fd was just created and nothing else owns it.
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    file.write_all(&executable)?;
    drop(executable);
    Ok(run(&format!("/proc/self/fd/{fd}"))?)
}

#[cfg(not(target_os = "linux"))]
fn run_from_memory<T>(_: Vec<u8>, _: impl FnOnce(&str) -> std::io::Result<T>) -> Result<T> {
    Err(failure("updates install on the unit"))
}

/// `generation`: the gen 1 or gen 2 the host read the unit's environment
/// as, which share one bundle (named for the j49). `remove_bundle`: the
/// bundle file goes once read, as an initramfs holds it in RAM.
fn run(
    (bundle_path, options, root, out): (&Path, &Path, &Path, &Path),
    dry_run: bool,
    live: bool,
    remove_bundle: bool,
    generation: Option<&str>,
) -> Result<()> {
    fs::create_dir_all(out)?;
    let mut settings = read_settings(options)?;
    // The overlays this run leaves out are only checked: a 64 MB unit cannot
    // hold them beside the rest.
    let mut bundle = Bundle::read_some(fs::File::open(bundle_path)?, |name| settings.wants(name))?;
    if remove_bundle {
        fs::remove_file(bundle_path)?;
    }
    let target = match (device(&bundle.manifest.device)?, generation.map(device).transpose()?) {
        (Device::J49 | Device::Diamond, Some(unit @ (Device::J49 | Device::Diamond))) => unit,
        (bundled, None) => bundled,
        (bundled, Some(unit)) => {
            return Err(failure(format!("the bundle is for a {}, not a {}", bundled.name(), unit.name())))
        }
    };
    log::info!("Bundle {} for the {}", bundle.manifest.version, bundle.manifest.device);
    if live {
        // The unit's settings are the last run's: this run installs this bundle.
        settings.version = bundle.manifest.version.clone();
    }
    let options = settings.take_options(&mut bundle)?;
    let themes = options.theme.is_some();
    let pki = settings.pki()?;
    let env = [
        ("UNFR0ZEN_ROOT", root.display().to_string()),
        ("UNFR0ZEN_VERSION", bundle.manifest.version.clone()),
        ("UNFR0ZEN_DRY_RUN", if dry_run { "1" } else { "0" }.to_string()),
        ("UNFR0ZEN_LIVE", if live { "1" } else { "0" }.to_string()),
    ];
    script(&bundle, bundle::PRE_SETUP, out, &env)?;

    // Setup decides whether nestlocal is written: a run with no integration
    // still rewrites an installed one. Moved out of the bundle, it costs no
    // more memory than a run with an integration.
    let nestlocal_binary = bundle
        .files
        .remove(bundle::NESTLOCAL)
        .map(|(_, data)| data)
        .ok_or_else(|| failure(format!("bundle: no {}", bundle::NESTLOCAL)))?;
    let screen = &bundle.file(bundle::SCREEN)?.to_vec();
    // The rest is in options now: the 64 MB OMAP units cannot hold a second copy.
    let post_setup = bundle.file(bundle::POST_SETUP)?.to_vec();
    drop(bundle);
    let manifest = RefCell::new(serde_json::Value::Null);
    let build_screen = |source: &[u8], sections: &[nlclient::menu::Section]| -> Result<Vec<u8>> {
        log::info!("Discovering nlclient ABI and linking unfr0zen's settings screens");
        let stock = out.join("nlclient.stock");
        match nlclient::builder::build(source, screen, sections) {
            Ok(built) => {
                if built.manifest["reviewed"] == false || (themes && !nlclient::theme::supported(source)) {
                    fs::write(&stock, source)?;
                }
                log::info!("nlclient: {}", built.manifest["status"].as_str().unwrap_or_default());
                *manifest.borrow_mut() = built.manifest;
                Ok(built.journal)
            }
            Err(error) => {
                fs::write(&stock, source)?;
                Err(error)
            }
        }
    };
    let mut new_pki = || match &pki {
        Some(pki) => Ok(pki.clone()),
        None => nestlocal::generate_pki(),
    };
    let mut volume = if live { LocalVolume::live(root) } else { LocalVolume::new(root) };
    let changes =
        setup::set_up_owned(&mut volume, target, options, nestlocal_binary, &mut new_pki, dry_run, &build_screen)?;
    let result = json!({
        "changes": changes.iter().map(|c| json!({"name": c.name, "action": c.action, "old": c.old, "new": c.new}))
            .collect::<Vec<_>>(),
        "nlclient": manifest.into_inner(),
    });
    fs::write(out.join("result.json"), format!("{result:#}\n"))?;
    log::info!(
        "{} changes{}",
        changes.len(),
        if dry_run { " (dry run: nothing written)" } else { ", verified on flash" }
    );
    if !dry_run {
        // The themes are installed anew and the restart that follows is no
        // crash: the guard counts their starts from none.
        match fs::remove_file(root.join(scratch::THEME_GUARD)) {
            Err(error) if error.kind() != ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
        let mut env = env.to_vec();
        env.push(("UNFR0ZEN_CHANGES", changes.len().to_string()));
        script_data(&post_setup, bundle::POST_SETUP, out, &env)?;
    }
    Ok(())
}
