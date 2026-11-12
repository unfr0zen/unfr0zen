use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::json;
use unfr0zen_core::bundle::{self, Bundle, Settings};
use unfr0zen_core::local::LocalVolume;
use unfr0zen_core::{nestlocal, nlclient, patch, Device, Error, Result};

struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!("{}: {}", record.level(), record.args());
        }
    }
    fn flush(&self) {}
}

static LOGGER: Logger = Logger;

pub fn main() {
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let paths: Vec<&String> = args.iter().filter(|a| *a != "--dry-run").collect();
    let [bundle, options, root, out] = paths[..] else {
        eprintln!("usage: unfr0zen-apply BUNDLE OPTIONS ROOT OUT [--dry-run]");
        std::process::exit(2);
    };
    if let Err(error) = run(Path::new(bundle), Path::new(options), Path::new(root), Path::new(out), dry_run) {
        eprintln!("unfr0zen-apply: {error}");
        std::process::exit(1);
    }
}

fn failure(message: impl Into<String>) -> Error {
    Error::Device(message.into())
}

fn device(name: &str) -> Result<Device> {
    match name {
        "diamond3" => Ok(Device::Diamond3),
        other => Err(failure(format!("the bundle is for a {other}, which unfr0zen-apply does not patch"))),
    }
}

/// Runs one of the bundle's scripts from OUT, with the run's details in its environment.
fn script(bundle: &Bundle, name: &str, out: &Path, env: &[(&str, String)]) -> Result<()> {
    let path = out.join(Path::new(name).file_name().unwrap());
    fs::write(&path, bundle.file(name)?)?;
    log::info!("Running {name}");
    let status = Command::new("sh").arg(&path).envs(env.iter().map(|(k, v)| (*k, v))).status()?;
    if !status.success() {
        return Err(failure(format!("{name} failed ({status})")));
    }
    Ok(())
}

fn run(bundle: &Path, options: &Path, root: &Path, out: &Path, dry_run: bool) -> Result<()> {
    fs::create_dir_all(out)?;
    let bundle = Bundle::unpack(&fs::read(bundle)?)?;
    let target = device(&bundle.manifest.device)?;
    log::info!("Bundle {} for the {}", bundle.manifest.version, bundle.manifest.device);
    let settings: Settings = serde_json::from_slice(&fs::read(options)?)
        .map_err(|e| failure(format!("{}: {e}", options.display())))?;
    let options = settings.options(&bundle)?;
    let pki = settings.pki()?;
    let env = [
        ("UNFR0ZEN_ROOT", root.display().to_string()),
        ("UNFR0ZEN_VERSION", bundle.manifest.version.clone()),
        ("UNFR0ZEN_DRY_RUN", if dry_run { "1" } else { "0" }.to_string()),
    ];
    script(&bundle, bundle::PRE_PATCH, out, &env)?;

    let nestlocal_binary = if options.mqtt.is_some() || options.matter.is_some() || options.zigbee || options.doom.is_some() {
        bundle.file(bundle::NESTLOCAL)?.to_vec()
    } else {
        Vec::new()
    };
    let screen = bundle.file(bundle::SCREEN)?;
    let manifest = RefCell::new(serde_json::Value::Null);
    let build_screen = |source: &[u8]| -> Result<Vec<u8>> {
        log::info!("Discovering nlclient ABI and linking UNFR0ZEN settings");
        let stock = out.join("nlclient.stock");
        match nlclient::builder::build(source, screen) {
            Ok(built) => {
                if built.manifest["reviewed"] == false {
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
    let mut volume = LocalVolume::new(root);
    let changes = patch::patch_on_device(&mut volume, target, &options, &nestlocal_binary, &mut new_pki,
                                         &BTreeSet::new(), dry_run, &build_screen)?;
    let result = json!({
        "changes": changes.iter().map(|c| json!({"name": c.name, "action": c.action, "old": c.old, "new": c.new}))
            .collect::<Vec<_>>(),
        "nlclient": manifest.into_inner(),
    });
    fs::write(out.join("result.json"), format!("{result:#}\n"))?;
    log::info!("{} changes{}", changes.len(), if dry_run { " (dry run: nothing written)" } else { ", verified on flash" });
    if !dry_run {
        let mut env = env.to_vec();
        env.push(("UNFR0ZEN_CHANGES", changes.len().to_string()));
        script(&bundle, bundle::POST_PATCH, out, &env)?;
    }
    Ok(())
}
