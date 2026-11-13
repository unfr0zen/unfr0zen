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
use unfr0zen_host::diamond3::usb::EntryPoint;
use unfr0zen_host::nand::{self, Job};
use unfr0zen_host::update;
use unfr0zen_host::{init_logging, log_path, mask_url, run, set_selection, CancelFlag, Config, Device, PatchInputs};

use ui::TerminalProgress;

fn help_styles() -> clap::builder::Styles {
    use clap::builder::styling::RgbColor;
    let heading = RgbColor(129, 206, 250).on_default().bold();
    let literal = RgbColor(225, 235, 239).on_default().bold();
    clap::builder::Styles::styled().header(heading).usage(heading).literal(literal)
}

#[derive(Parser, Debug)]
#[command(name = "unfr0zen", version = env!("UNFR0ZEN_BUILD_VERSION"), styles = help_styles(), about = "Root and free a Nest Learning Thermostat (3rd, 2nd or experimental 1st generation) over USB.\n\n\
    Without a command: patch the root volume the unit boots (root0 or root1), then boot it. Nothing else on the NAND \
    is written.", args_conflicts_with_subcommands = true)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// seconds to wait after the front button prompt
    #[arg(long, alias = "wait-sdp", default_value_t = 120, global = true)]
    wait_usb: u64,
    /// seconds to wait for the app root shell
    #[arg(long, default_value_t = 180)]
    wait_shell: u64,
    /// run record directory (default: the runs folder in unfr0zen's data directory)
    #[arg(long)]
    work_dir: Option<PathBuf>,
    /// j49/diamond: patch FILE, a raw NAND image (2112-byte pages), in place instead of a unit
    #[arg(long, value_name = "FILE")]
    nand_image: Option<PathBuf>,
    /// the unit's generation (default: identified on USB, or from the NAND's U-Boot environment in USB download mode)
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
    /// authorize one OpenSSH public key for root (gen3's stock SSH may need ECDSA)
    #[arg(long, value_name = "FILE", requires = "ssh", help_heading = "Root filesystem options")]
    ssh_public_key: Option<PathBuf>,
    /// SSH takes keys only: root's password is refused (needs --ssh-public-key)
    #[arg(long, requires = "ssh_public_key", help_heading = "Root filesystem options")]
    ssh_key_only: bool,
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
    /// replaces nestlocal_<serial> in the entity IDs, e.g. nest_famrm gives climate.nest_famrm_thermostat, and <serial> stands for the unit's serial in lowercase; Home Assistant moves the entities to the new IDs with their history and settings, automations and dashboards need updating (default: the unit's)
    #[arg(long, value_name = "ROOT", help_heading = "Root filesystem options")]
    ha_entity_root: Option<String>,
    /// the thermostat's name in Home Assistant (--mqtt) and Matter controllers, e.g. "Nest FamRm", at most 32 characters; <serial> stands for the unit's serial (default: the unit's, at first "Nest Thermostat")
    #[arg(long, value_name = "NAME", help_heading = "Root filesystem options")]
    device_name: Option<String>,
    /// serve the unit over Matter, with or without --mqtt; gen 3 turns off its own Bluetooth for Matter, so real Nest Temperature Sensors stop working
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
    /// run a Heat Link model (Display-3.x, Display-5.x) as heating a Works With Nest boiler through W1; without it firmware 6.4-8 can stop on H71 with no Heat Link (needs --zigbee or --matter-thread)
    #[arg(long, help_heading = "Root filesystem options")]
    wwn_boiler: bool,
    /// how the 3rd generation's boot ROM is entered for recovery: auto, cve-2017-7932, cve-2017-7936 or official-loader
    #[arg(long, global = true)]
    entry_point: Option<EntryPoint>,
    /// install Doom (shareware episode), launched from the UNFR0ZEN settings screen; hold the button 3 s to quit
    #[arg(long, help_heading = "Root filesystem options")]
    doom: bool,
    /// regulate from temperatures sent over MQTT, Zigbee or Matter, using one sensor named after the thermostat's room; gen 2 needs the reviewed 5.9.4-5 nlclient patch; gen 3 turns off its own Bluetooth, so real Nest Temperature Sensors stop working; needs --mqtt, --matter or --zigbee
    #[arg(long, help_heading = "Root filesystem options")]
    external_temperature: bool,
    /// gen 2: show the settings screens in the 3rd generation's dark colours, and the boot logo on black; needs the reviewed 5.9.4-5 nlclient
    #[arg(long, help_heading = "Root filesystem options")]
    dark_theme: bool,
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
        #[arg(long, required_unless_present = "discover")]
        out: Option<PathBuf>,
        /// Restore stock from this journal instead of building the custom screen
        #[arg(long, conflicts_with = "discover")]
        undo: Option<PathBuf>,
        /// Print what discovery finds in BINARY as JSON, and any objection to the build, and write nothing
        #[arg(long)]
        discover: bool,
    },
    /// diamond3: load recovery U-Boot over USB and leave it at its prompt; prints its serial port
    Uboot,
    /// diamond3: boot the application firmware, through recovery and the stock U-Boot when needed
    Boot,
    // A diamond3's whole 512 MiB NAND as an ECC-corrected image (2048-byte
    // pages, no OOB); the OMAP boards' as raw 2112-byte pages with OOB.
    /// Back up the whole NAND to OUT, with its SHA-256 beside it (`verify` reads it
    /// again to compare); nothing is written to NAND. Run it again with the same OUT to resume an interrupted backup
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
            backplate_sim: self.backplate_sim,
            block_cloud: self.block_cloud,
            mqtt: self.mqtt.clone(),
            mqtt_user: self.mqtt_user.clone(),
            mqtt_password: self.mqtt_password.clone(),
            mqtt_tls: self.mqtt_tls,
            mqtt_ca: self.mqtt_ca.clone(),
            device_name: self.device_name.clone(),
            ha_entity_root: self.ha_entity_root.clone(),
            matter: self.matter,
            matter_thread: self.matter_thread,
            matter_code: self.matter_code.clone(),
            zigbee: self.zigbee,
            wwn_boiler: self.wwn_boiler,
            entry_point: self.entry_point.unwrap_or_default(),
            doom: self.doom,
            external_temperature: self.external_temperature,
            dark_theme: self.dark_theme,
            ssh: self.ssh,
            ssh_public_key: self.ssh_public_key.clone(),
            ssh_key_only: self.ssh_key_only,
            usb_shell: self.usb_shell,
        }
    }
}

/// The command line for the run log: secrets and the MQTT user masked, URLs
/// without credentials, paths cut to their file names.
fn logged_arguments(args: impl Iterator<Item = String>) -> String {
    /// How the value of a flag is shown; None for flags shown as given.
    fn shown(flag: &str, value: &str) -> Option<String> {
        match flag {
            "--root-password" | "--mqtt-password" | "--matter-code" | "--mqtt-user" => Some("***".into()),
            "--mqtt" => Some(mask_url(value)),
            "--mqtt-ca" | "--nand-image" | "--work-dir" | "--data-dir" | "--ssh-public-key" =>
                Some(std::path::Path::new(value).file_name().unwrap_or_default().to_string_lossy().into_owned()),
            _ => None,
        }
    }
    let (mut out, mut flag) = (Vec::new(), None::<String>);
    for arg in args {
        if let Some(name) = flag.take() {
            out.push(shown(&name, &arg).unwrap_or(arg));
        } else if let Some((name, value)) = arg.split_once('=').filter(|(name, _)| name.starts_with("--")) {
            out.push(format!("{name}={}", shown(name, value).unwrap_or_else(|| value.into())));
        } else {
            if shown(&arg, "").is_some() { flag = Some(arg.clone()); }
            out.push(arg);
        }
    }
    format!("Arguments: {}", out.join(" "))
}

pub fn main() {
    init_logging(env!("UNFR0ZEN_BUILD_VERSION"));
    update::remove_previous();
    ui::enable_colors();
    set_selection(logged_arguments(std::env::args().skip(1)));
    let args = Args::try_parse().unwrap_or_else(|error| {
        if error.kind() == clap::error::ErrorKind::DisplayHelp {
            if !std::env::args_os().any(|arg| arg == "--skip-logo") {
                ui::logo();
            }
            ui::help(error);
        }
        error.exit();
    });
    if let Some(Command::Nlclient { binary, out, undo, discover }) = &args.command {
        if *discover {
            let found = std::fs::read(binary).map_err(|e| e.to_string())
                .and_then(|source| unfr0zen_host::nlclient::discovery::survey(&source).map_err(|e| e.to_string()));
            match found {
                Ok((found, objection)) => {
                    println!("{}", serde_json::to_string_pretty(&serde_json::json!({"discovery": found, "objection": objection})).unwrap());
                    if objection.is_some() { std::process::exit(1); }
                }
                Err(error) => { eprintln!("unfr0zen: {}: {error}", binary.display()); std::process::exit(1); }
            }
            return;
        }
        let out = out.as_deref().expect("--out is required without --discover");
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
    if let Some(Command::Update) = args.command {
        finish(update());
    }
    let data_dir = args.data_dir.clone().unwrap_or_else(default_data_dir);
    let work_dir = args.work_dir.clone().unwrap_or_else(|| data_dir.join("runs"));

    let config = Config {
        options,
        omap: args.device,
        entry_point: args.entry_point.unwrap_or_default(),
        wait_usb: Duration::from_secs(args.wait_usb),
        wait_shell: Duration::from_secs(args.wait_shell),
        work_dir,
        data_dir,
    };
    let mut progress = TerminalProgress::default();
    let cancel: CancelFlag = Default::default();
    let result = match &args.command {
        None => match &args.nand_image {
            Some(image) => unfr0zen_host::j49::run_image(&config, image, &mut progress),
            None => run(&config, &mut progress, &cancel),
        },
        Some(Command::Nlclient { .. }) => unreachable!("handled before USB setup"),
        Some(Command::Uboot) => boot::uboot(config.wait_usb, config.entry_point, &mut progress),
        Some(Command::Boot) => boot::boot(config.wait_usb, config.entry_point, &mut progress),
        Some(Command::Backup { out }) => {
            let job = Job::Backup(out.clone().unwrap_or_else(nand::default_backup));
            nand::run(&job, config.wait_usb, config.omap, config.entry_point, &mut progress, &mut confirm_restore, &cancel)
        }
        Some(Command::Verify { image }) => {
            nand::run(&Job::Verify(image.clone()), config.wait_usb, config.omap, config.entry_point, &mut progress, &mut confirm_restore, &cancel)
        }
        Some(Command::Restore { image }) => {
            nand::run(&Job::Restore(image.clone()), config.wait_usb, config.omap, config.entry_point, &mut progress, &mut confirm_restore, &cancel)
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

#[cfg(test)]
mod logged_arguments_tests {
    use super::{logged_arguments, Args};
    use clap::Parser;

    #[test]
    fn public_key_flag_requires_ssh_and_logs_only_its_filename() {
        assert!(Args::try_parse_from(["unfr0zen", "--ssh-public-key", "/home/bob/id_ed25519.pub"]).is_err());
        let args = Args::try_parse_from(["unfr0zen", "--ssh", "--ssh-public-key", "/home/bob/id_ed25519.pub"]).unwrap();
        assert_eq!(args.patch_inputs().ssh_public_key.unwrap(), std::path::Path::new("/home/bob/id_ed25519.pub"));
        for args in [vec!["--ssh", "--ssh-public-key", "/home/bob/id_ed25519.pub"],
                     vec!["--ssh", "--ssh-public-key=/home/bob/id_ed25519.pub"]] {
            let logged = logged_arguments(args.into_iter().map(str::to_string));
            assert!(logged.contains("id_ed25519.pub"));
            assert!(!logged.contains("/home/bob"));
        }
    }

    #[test]
    fn secrets_credentials_and_home_paths_stay_out_of_the_log() {
        let args = ["--zigbee", "--root-password", "hunter2", "--mqtt=mqtt://bob:pw@broker.lan:1883",
                    "--mqtt-user", "bob", "--matter-code=20202021", "--mqtt-ca", "/Users/bob/ca.pem"];
        assert_eq!(logged_arguments(args.iter().map(|a| a.to_string())),
                   "Arguments: --zigbee --root-password *** --mqtt=mqtt://***@broker.lan:1883 \
                    --mqtt-user *** --matter-code=*** --mqtt-ca ca.pem");
    }
}

#[cfg(test)]
mod entry_point_tests {
    use super::{Args, EntryPoint};
    use clap::{CommandFactory, Parser};

    #[test]
    fn every_command_takes_the_entry_point() {
        for command in [&["uboot"][..], &["boot"], &["backup"], &["verify", "nand.bin"], &["restore", "nand.bin"], &["update"]] {
            let line = [&["unfr0zen"], command, &["--entry-point", "official-loader"]].concat();
            let args = Args::try_parse_from(&line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
            assert_eq!(args.entry_point, Some(EntryPoint::OfficialLoader), "{line:?}");
        }
        let patch = Args::try_parse_from(["unfr0zen", "--entry-point", "official-loader"]).unwrap();
        assert_eq!(patch.entry_point, Some(EntryPoint::OfficialLoader));
        assert!(Args::try_parse_from(["unfr0zen", "backup", "--entry-point", "nope"]).is_err());
    }

    #[test]
    fn options_go_after_a_command_the_entry_point_like_the_others() {
        for option in [&["--entry-point", "official-loader"][..], &["--wait-usb", "5"], &["--device", "diamond3"]] {
            let before = [&["unfr0zen"], option, &["backup"]].concat();
            assert!(Args::try_parse_from(&before).is_err(), "{before:?}");
            let after = [&["unfr0zen", "backup"][..], option].concat();
            assert!(Args::try_parse_from(&after).is_ok(), "{after:?}");
        }
    }

    #[test]
    fn help_names_the_entry_point_option_by_what_it_takes() {
        let help = Args::command().render_long_help().to_string();
        assert!(help.contains("--entry-point <ENTRY_POINT>"), "{help}");
        assert!(!help.contains("TECHNIQUE"), "{help}");
    }
}
