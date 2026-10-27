//! unfr0zen: recover a Nest over USB, patch its selected root volume, and
//! verify the USB root shell; or back up, verify and restore its whole NAND.
//! The terminal front end over unfr0zen-host, which the unfr0zen executable
//! runs when it is given arguments on Linux and macOS.

mod ui;

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use clap::{CommandFactory, Parser, Subcommand};
use unfr0zen_host::flow::default_data_dir;
use unfr0zen_host::diamond3::boot;
use unfr0zen_host::nand::{self, Job};
use unfr0zen_host::update;
use unfr0zen_host::{init_logging, log_path, run, Config, Device, PatchInputs, Slot};

use ui::TerminalProgress;

#[derive(Parser, Debug)]
#[command(name = "unfr0zen", version = env!("UNFR0ZEN_BUILD_VERSION"), about = "Recover a Nest over USB (diamond3, j49 or experimental Diamond), patch its selected root volume from \
    the unit, and with --usb-shell verify its USB root shell. The original volume is saved only with --backup; all NAND writes target only \
    the selected root0/root1 volume. The commands only boot the unit, or back up, verify and restore the whole NAND.", args_conflicts_with_subcommands = true)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// seconds to wait after the front button prompt
    #[arg(long, alias = "wait-sdp", default_value_t = 120, global = true)]
    wait_usb: u64,
    /// seconds to wait for the app root shell
    #[arg(long, default_value_t = 180)]
    wait_shell: u64,
    /// backup and run record directory (default: unfr0zen's data directory)
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// patch copies with the volume mounted read-only, list the changes, write nothing, and boot the unit unchanged
    #[arg(long)]
    dry_run: bool,
    /// read the whole root volume and save it as original.ubifs before patching (about 45 s)
    #[arg(long)]
    backup: bool,
    /// override stock boot order when the previous Linux slot is known
    #[arg(long, value_parser = ["root0", "root1"])]
    slot: Option<String>,
    /// j49/diamond: patch FILE, a raw NAND image (2112-byte pages), in place instead of a unit
    #[arg(long, value_name = "FILE")]
    nand_image: Option<PathBuf>,
    /// the unit's generation (default: identified on USB; a boot ROM Diamond and j49 share is taken as j49)
    #[arg(long, global = true, value_parser = parse_device, value_name = "diamond|j49|diamond3")]
    device: Option<Device>,
    /// sync time from HOST instead of time.nest.com (default pool.ntp.org)
    #[arg(long, num_args = 0..=1, default_missing_value = "", value_name = "HOST", help_heading = "Root filesystem options")]
    ntp_server: Option<String>,
    /// root's password for the USB shell and SSH (default: unfr0zen)
    #[arg(long, value_name = "PASSWORD", help_heading = "Root filesystem options")]
    root_password: Option<String>,
    /// run an SSH server on the unit, root logging in with its password (default: no SSH server)
    #[arg(long, help_heading = "Root filesystem options")]
    ssh: bool,
    /// a root shell on the unit's USB serial port, which the run then confirms (default: no USB serial port)
    #[arg(long, help_heading = "Root filesystem options")]
    usb_shell: bool,
    /// relay to this MQTT broker (--mqtt, --matter, --zigbee and --doom each install nestlocal)
    #[arg(long, value_name = "URL", help_heading = "Root filesystem options")]
    mqtt: Option<String>,
    /// MQTT user name for --mqtt
    #[arg(long, help_heading = "Root filesystem options")]
    mqtt_user: Option<String>,
    /// MQTT password for --mqtt (needs --mqtt-user)
    #[arg(long, help_heading = "Root filesystem options")]
    mqtt_password: Option<String>,
    /// nestlocal connects to the broker over TLS
    #[arg(long, help_heading = "Root filesystem options")]
    mqtt_tls: bool,
    /// PEM CA certificate to verify the broker against (implies --mqtt-tls)
    #[arg(long, value_name = "FILE", help_heading = "Root filesystem options")]
    mqtt_ca: Option<PathBuf>,
    /// serve the unit over Matter, with or without --mqtt (prints the pairing code to add it with)
    #[arg(long, help_heading = "Root filesystem options")]
    matter: bool,
    /// Matter pairing code to enter on the phone, 11 digits (implies --matter; default: 0347-100-5534)
    #[arg(long, value_name = "CODE", help_heading = "Root filesystem options")]
    matter_code: Option<String>,
    /// serve Matter over Thread too, through the unit's radio (needs --matter; replaces Heat Link, excludes --zigbee)
    #[arg(long, help_heading = "Root filesystem options")]
    matter_thread: bool,
    /// join a Zigbee network through the unit's radio, as a thermostat (replaces Heat Link); pair by permitting joins on the coordinator
    #[arg(long, help_heading = "Root filesystem options")]
    zigbee: bool,
    /// install Doom (shareware episode), launched from the UNFR0ZEN settings screen; hold the button 3 s to quit
    #[arg(long, help_heading = "Root filesystem options")]
    doom: bool,
    /// run client-ng, the new thermostat UI, in place of nlclient from start-up; without it a unit that has it runs nlclient again
    #[arg(long, alias = "new-ui", help_heading = "Root filesystem options")]
    client_ng: bool,
    /// tar of root-owned files to add
    #[arg(long, value_name = "TAR", help_heading = "Root filesystem options")]
    overlay: Option<PathBuf>,
    /// set nlclient's cloudregisterurl (Nest's frontdoor) to URL; no nestlocal
    #[arg(long, value_name = "URL", help_heading = "Root filesystem options")]
    cloud_url: Option<String>,
    /// run nlclient's backplate simulator: 1H1C, simulated temperature
    #[arg(long, help_heading = "Root filesystem options")]
    backplate_sim: bool,
    /// resolve Nest's and Google's servers to nowhere in /etc/hosts, and check for the internet at Cloudflare (implies --ntp-server)
    #[arg(long, help_heading = "Root filesystem options")]
    block_cloud: bool,
    #[arg(long, hide = true, global = true)]
    skip_logo: bool,
    /// where nestlocal's key set for new units is kept
    #[arg(long, hide = true, global = true)]
    data_dir: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Patch a local stock nlclient or restore it using its undo journal; no USB or host tools
    Nlclient {
        binary: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Restore stock from this journal instead of building the custom screen
        #[arg(long)]
        undo: Option<PathBuf>,
    },
    /// diamond3: load recovery U-Boot over USB and leave it at its prompt; prints its serial port
    Uboot,
    /// diamond3: boot the application firmware, through recovery and the stock U-Boot when needed
    Boot,
    // A diamond3's whole 512 MiB NAND as an ECC-corrected image (2048-byte
    // pages, no OOB); the OMAP boards' as raw 2112-byte pages with OOB.
    /// Back up the whole NAND to OUT and check it with a second read; nothing is
    /// written to NAND. Run the same command again to resume an interrupted backup
    Backup {
        /// default: nand-<date>-<time>.bin in the current directory
        out: Option<PathBuf>,
    },
    /// Compare IMAGE, a NAND backup, with the NAND
    Verify { image: PathBuf },
    /// Write IMAGE, a backup verified against its .sha256, back to NAND (asks first)
    Restore { image: PathBuf },
    /// Replace this executable with the latest release from GitHub, if newer
    Update,
}

fn parse_device(value: &str) -> Result<Device, String> {
    match value {
        "diamond" => Ok(Device::Diamond),
        "j49" => Ok(Device::J49),
        "diamond3" => Ok(Device::Diamond3),
        _ => Err("expected diamond, j49 or diamond3".into()),
    }
}


/// Asks on the terminal before a restore overwrites NAND.
fn confirm_restore(what: &str) -> bool {
    ui::waiting(what);
    print!("⠿ Type RESTORE to overwrite NAND: ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).is_ok() && answer.trim() == "RESTORE"
}

impl Args {
    fn patch_inputs(&self) -> PatchInputs {
        PatchInputs {
            root_password: self.root_password.clone(),
            ntp_server: self.ntp_server.clone(),
            cloud_url: self.cloud_url.clone(),
            backplate_sim: self.backplate_sim,
            block_cloud: self.block_cloud,
            overlay: self.overlay.clone(),
            mqtt: self.mqtt.clone(),
            mqtt_user: self.mqtt_user.clone(),
            mqtt_password: self.mqtt_password.clone(),
            mqtt_tls: self.mqtt_tls,
            mqtt_ca: self.mqtt_ca.clone(),
            matter: self.matter,
            matter_thread: self.matter_thread,
            matter_code: self.matter_code.clone(),
            zigbee: self.zigbee,
            doom: self.doom,
            client_ng: self.client_ng,
            ssh: self.ssh,
            usb_shell: self.usb_shell,
        }
    }
}

/// Linux gives USB to root: re-run under sudo, with the user's directories.
#[cfg(target_os = "linux")]
fn elevate(args: &Args, work_dir: &std::path::Path, data_dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    if unsafe { libc::geteuid() } == 0 {
        return Ok(());
    }
    ui::waiting("Requesting USB access");
    let mut command = std::process::Command::new("sudo");
    command.arg("-p").arg("\x1b[38;2;0;255;255m⠿ Enter sudo password for USB access: \x1b[0m").arg("--")
        .arg(std::env::current_exe()?).args(std::env::args_os().skip(1)).arg("--skip-logo");
    if args.work_dir.is_none() && args.command.is_none() {
        command.arg("--work-dir").arg(work_dir);
    }
    if args.data_dir.is_none() {
        command.arg("--data-dir").arg(data_dir);
    }
    Err(std::io::Error::other(format!("could not run sudo: {}", command.exec())))
}

pub fn main() {
    init_logging();
    ui::enable_colors();
    let args = Args::parse();
    if let Some(Command::Nlclient { binary, out, undo }) = &args.command {
        match local_nlclient(binary, out, undo.as_deref()) {
            Ok(message) => println!("{message}"),
            Err(error) => { eprintln!("unfr0zen: {error}"); std::process::exit(1); }
        }
        return;
    }
    let fail = |message: String| -> ! { Args::command().error(clap::error::ErrorKind::ValueValidation, message).exit() };

    if args.device.is_some_and(|d| d != Device::Diamond3) && matches!(args.command.as_ref(), Some(Command::Boot | Command::Uboot)) {
        fail("boot and uboot are diamond3-only".into());
    }
    if args.device == Some(Device::Diamond3) && args.nand_image.is_some() {
        fail("--nand-image is a j49 or Diamond NAND image".into());
    }
    let options = args.patch_inputs().build().unwrap_or_else(|e| fail(e.to_string()));
    if !args.skip_logo {
        ui::logo();
    }
    // Before elevation: an update needs no USB access.
    if let Some(Command::Update) = args.command {
        finish(update());
    }
    let data_dir = args.data_dir.clone().unwrap_or_else(default_data_dir);
    let work_dir = args.work_dir.clone().unwrap_or_else(|| data_dir.join("runs"));
    #[cfg(target_os = "linux")]
    if let Err(e) = if args.nand_image.is_none() { elevate(&args, &work_dir, &data_dir) } else { Ok(()) } {
        ui::error(&format!("unfr0zen: {e}"));
        std::process::exit(1);
    }

    let config = Config {
        options,
        omap: args.device,
        slot: args.slot.as_deref().and_then(Slot::parse),
        dry_run: args.dry_run,
        backup: args.backup,
        wait_usb: Duration::from_secs(args.wait_usb),
        wait_shell: Duration::from_secs(args.wait_shell),
        work_dir,
        data_dir,
    };
    let mut progress = TerminalProgress::default();
    let result = match &args.command {
        None => match &args.nand_image {
            Some(image) => unfr0zen_host::j49::run_image(&config, image, &mut progress),
            None => run(&config, &mut progress),
        },
        Some(Command::Nlclient { .. }) => unreachable!("handled before USB setup"),
        Some(Command::Uboot) => boot::uboot(config.wait_usb, &mut progress),
        Some(Command::Boot) => boot::boot(config.wait_usb, &mut progress),
        Some(Command::Backup { out }) => {
            let job = Job::Backup(out.clone().unwrap_or_else(nand::default_backup));
            nand::run(&job, config.wait_usb, config.omap, &mut progress, &mut confirm_restore)
        }
        Some(Command::Verify { image }) => {
            nand::run(&Job::Verify(image.clone()), config.wait_usb, config.omap, &mut progress, &mut confirm_restore)
        }
        Some(Command::Restore { image }) => {
            nand::run(&Job::Restore(image.clone()), config.wait_usb, config.omap, &mut progress, &mut confirm_restore)
        }
        Some(Command::Update) => update(),
    };
    finish(result);
}

fn finish(result: unfr0zen_host::Result<String>) -> ! {
    match result {
        Ok(message) => {
            ui::done(&message);
            std::process::exit(0);
        }
        Err(error) => {
            log::error!("{error}");
            let hint = if log_path().is_some() { " (details in the run log)" } else { "" };
            ui::error(&format!("unfr0zen: {error}{hint}"));
            std::process::exit(1);
        }
    }
}

/// The offline entry point uses exactly the same builder and object as recovery.
fn local_nlclient(binary: &std::path::Path, out: &std::path::Path, undo: Option<&std::path::Path>) -> unfr0zen_host::Result<String> {
    use unfr0zen_host::{nlclient, assets};
    let source = std::fs::read(binary)?;
    let (output, sidecars) = if let Some(journal) = undo {
        let patch = nlclient::Patch::parse(&std::fs::read(journal)?)?;
        (patch.apply(&source, true)?, None)
    } else {
        let built = nlclient::builder::build(&source, assets::CUSTOM_SCREEN)?;
        let manifest = serde_json::to_vec_pretty(&built.manifest).map_err(|e| unfr0zen_host::Error::Io(e.to_string()))?;
        (built.output, Some((built.journal, manifest)))
    };
    // Never overwrite the input or one of its output files by accident.
    let absolute = |p: &std::path::Path| -> std::io::Result<PathBuf> {
        if p.exists() { p.canonicalize() } else {
            Ok(p.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| std::path::Path::new(".")).canonicalize()?.join(p.file_name().ok_or_else(|| std::io::Error::other("output needs a file name"))?))
        }
    };
    let input = binary.canonicalize()?;
    let destinations = if sidecars.is_some() { vec![out.to_path_buf(), out.with_extension("json"), out.with_extension("undo.gz")] } else { vec![out.to_path_buf()] };
    let mut seen = std::collections::BTreeSet::new();
    for path in &destinations {
        let path = absolute(path)?;
        if path == input || undo.is_some_and(|u| u.canonicalize().ok().as_ref() == Some(&path)) || !seen.insert(path) {
            return Err(unfr0zen_host::Error::Device("nlclient input and output paths must be distinct".into()));
        }
    }
    std::fs::write(out, &output)?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o755))?;
    }
    if let Some((journal, manifest)) = sidecars {
        std::fs::write(out.with_extension("undo.gz"), journal)?;
        std::fs::write(out.with_extension("json"), manifest)?;
    }
    Ok(format!("Wrote {} ({} bytes)", out.display(), output.len()))
}

fn update() -> unfr0zen_host::Result<String> {
    let version = env!("UNFR0ZEN_BUILD_VERSION");
    if update::release_version(version).is_none() {
        return Ok(format!("unfr0zen {version} is a development build; release builds update themselves"));
    }
    ui::waiting("Checking GitHub for a newer release");
    let Some(release) = update::check(update::App::Cli, version)? else {
        return Ok(format!("unfr0zen {version} is the latest release"));
    };
    ui::waiting(&format!("Installing unfr0zen {}", release.tag));
    update::install(update::App::Cli, &release)?;
    Ok(format!("Updated unfr0zen {version} to {}: {}", release.tag, release.url))
}
