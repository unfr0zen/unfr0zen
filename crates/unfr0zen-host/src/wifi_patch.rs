//! The Patch page's settings over Wi-Fi: the USB patch run against the unit
//! through an update session (remote.rs), so both write the same files, which
//! the unit applies as one set its guard rolls back in full.

use std::fs;
use std::time::Instant;

use unfr0zen_core::nestlocal::{self, Matter, Pki};
use unfr0zen_core::setup::{self, Change, Options};
pub use unfr0zen_core::setup::changes_text;
use unfr0zen_core::settings::{unit_settings, UnitSettings, SETTINGS_FILES};
use unfr0zen_core::{nlclient, Device, Error, Result};
use update_wire::bundle::{in_scratch, RESTART_PATHS, SCRATCH_NESTLOCAL};
pub use update_wire::bundle::Class;
use update_wire::read::{Info, Refused, Rule};

use crate::flow::{default_data_dir, failure, private_dir, screen_builder, stored_pki, SetupInputs};
use crate::push::{identity, newer_than, ON_TRIAL};
use crate::remote::RemoteVolume;
use crate::session::{self, address, is_code, Code, Session, Timing, UNIT_TIMING};
use crate::trust;
use crate::assets;

/// Why a run that would end the Wi-Fi path is refused: the guard keeps a set
/// only once nestlocal reaches nlclient, and the update API is the way back.
pub const KEEPS_NESTLOCAL: &str = "a run over Wi-Fi keeps nestlocal (MQTT, Matter or Zigbee) and --update-api on: \
                                   use USB to remove nestlocal or the update API";
/// A gen 2's USB shell and dark theme also choose the kernel and its boot order.
const LOCKED: &str = "on a 1st or 2nd generation, --usb-shell and --dark-theme also choose the kernel, so they change over USB only";

/// A unit as a Wi-Fi run first reads it.
pub struct Remote {
    pub device: Device,
    pub settings: UnitSettings,
    pub info: Info,
    /// Why no Wi-Fi run reaches the unit, known from the read: its check of
    /// nestlocal's path (Session::check), or needs_usb for a unit without a
    /// check. A run checks its whole set.
    pub usb_reason: Option<String>,
}

/// What a Wi-Fi run changed, or would change on a dry run.
pub struct Outcome {
    pub changes: Vec<Change>,
    /// What the unit does, or on a dry run would do, with the set: restart
    /// nestlocal, or reboot; None when there is nothing to send.
    pub class: Option<Class>,
}

/// What a Wi-Fi run builds a set with, beside the inputs.
struct Parts<'a> {
    nestlocal: &'a [u8],
    build_screen: &'a dyn setup::ScreenBuilder,
    new_pki: &'a dyn Fn() -> Result<Pki>,
    timing: &'a Timing,
    /// Where a trust the unit grants is kept.
    trusted: &'a trust::Store,
}

/// The unit at host and its settings, read over a session keyed with code.
pub fn read_settings(host: &str, code: Code, progress: &dyn Fn(&str)) -> Result<Remote> {
    read_settings_at(&address(host)?, code, progress, &UNIT_TIMING, &trust::Store::default())
}

fn read_settings_at(addr: &str, code: Code, progress: &dyn Fn(&str), timing: &Timing, trusted: &trust::Store) -> Result<Remote> {
    let (session, _) = session::connect_at(addr, code, progress, timing, trusted)?;
    let read = survey(&session, false, progress).and_then(|(_, remote)| {
        // Every setup keeps nestlocal on the scratch volume (needs_usb).
        let usb_reason = match session.check(&[SCRATCH_NESTLOCAL])? {
            Some(refused) => match verdict(&refused) {
                Err(Error::NeedsUsb(reason)) => Some(reason),
                _ => None,
            },
            None => needs_usb(&remote.info),
        };
        Ok(Remote { usb_reason, ..remote })
    });
    // The form is filled in its own time, and sent in a session of its own.
    session.close();
    read
}

/// What the first read asks for at once: the settings and the files the
/// patch always reads.
fn first_reads(device: Device) -> Vec<&'static str> {
    let rootfs: &[&str] = match device {
        Device::Diamond3 => &unfr0zen_core::diamond3::rootfs::FILES,
        _ => &unfr0zen_core::j49::rootfs::FILES,
    };
    let mut names: Vec<&str> = SETTINGS_FILES.iter().chain(rootfs).chain(&nestlocal::STOCK_FILES).copied()
        .chain([nestlocal::UNLOCK_BUNDLE, nestlocal::RANDOM_SEED, "etc/nestlocal/server.crt", "etc/nestlocal/server.key",
                nlclient::JOURNAL])
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// The unit's device and settings, and the volume that read them. An
/// unknown model is refused before anything is read.
fn survey<'a>(session: &'a Session, force: bool, progress: &'a dyn Fn(&str)) -> Result<(RemoteVolume<'a>, Remote)> {
    let info = session.info()?;
    let device = Device::from_nlmodel(&info.nlmodel).ok_or_else(|| {
        failure(format!("the unit reports model {:?}, which unfr0zen does not set up over Wi-Fi", info.nlmodel))
    })?;
    progress(&format!("{} (nlmodel {}) running nestlocal {}", device.description(), info.nlmodel, info.version));
    let mut volume = RemoteVolume::new(session, info.clone(), force, progress);
    volume.fetch(&first_reads(device), true)?;
    let mut settings = unit_settings(&volume.tree(), device)?;
    settings.dark_theme = Some(matches!(device, Device::Diamond | Device::J49) && dark_theme(&mut volume)?);
    Ok((volume, Remote { device, settings, info, usb_reason: None }))
}

/// Whether a gen 1's or gen 2's nlclient has the dark palette, against the stock one the
/// patch kept.
fn dark_theme(volume: &mut RemoteVolume) -> Result<bool> {
    use unfr0zen_core::shell::Volume;
    let (Some((_, _, stock)), Some((_, _, output))) = (volume.read(nlclient::ORIGINAL)?, volume.read(nlclient::BINARY)?) else {
        return Ok(false);
    };
    Ok(nlclient::dark_theme::applied(&stock, &output))
}

/// The inputs that patch the unit to the settings it has: the Wi-Fi form's
/// pre-fill. The root password stays blank, which keeps the unit's.
pub fn inputs_from(settings: &UnitSettings) -> SetupInputs {
    let mqtt = settings.mqtt.as_ref();
    SetupInputs {
        ntp_server: settings.ntp_server.clone(),
        backplate_sim: settings.backplate_sim,
        block_cloud: settings.block_cloud,
        mqtt: mqtt.map(|mqtt| mqtt.url.clone()),
        mqtt_user: mqtt.and_then(|mqtt| mqtt.user.clone()),
        mqtt_password: mqtt.and_then(|mqtt| mqtt.password.clone()),
        mqtt_tls: mqtt.is_some_and(|mqtt| mqtt.tls),
        mqtt_ca_pem: mqtt.and_then(|mqtt| mqtt.ca.clone()),
        ha_entity_root: mqtt.and_then(|mqtt| mqtt.entity_root.clone()),
        device_name: settings.device_name.clone(),
        matter: settings.matter.is_some(),
        matter_thread: settings.matter_thread,
        matter_code: settings.matter.map(|matter| matter.pairing_code()),
        zigbee: settings.zigbee,
        wwn_boiler: settings.wwn_boiler,
        doom: settings.doom,
        themes: settings.themes,
        external_temperature: settings.external_temperature,
        dark_theme: settings.dark_theme.unwrap_or(false),
        ssh: settings.ssh,
        ssh_key_only: settings.ssh_key_only,
        ssh_public_key_text: settings.ssh_public_key.clone(),
        usb_shell: settings.usb_shell,
        allow_unsigned_updates: settings.unsigned_updates,
        update_api: settings.update_api,
        ..SetupInputs::default()
    }
}

/// Refuses inputs a run over Wi-Fi cannot send, before the unit is asked.
fn check_inputs(inputs: &SetupInputs) -> Result<()> {
    if !inputs.update_api || !inputs.runs_nestlocal() {
        return Err(failure(KEEPS_NESTLOCAL));
    }
    Ok(())
}

/// The options for the unit, as its device's USB run builds them (through
/// its bundle: bundle::options), keeping its root hash when no password is
/// given and its exact Matter discriminator when the pairing code is its own.
fn options_for(device: Device, inputs: &SetupInputs, unit: &UnitSettings) -> Result<Options> {
    if matches!(device, Device::Diamond | Device::J49) && (inputs.usb_shell != unit.usb_shell || inputs.dark_theme != unit.dark_theme.unwrap_or(false)) {
        return Err(failure(LOCKED));
    }
    let mut options = inputs.build()?;
    if let (None, Some(hash)) = (&inputs.root_password, &unit.root_hash) {
        options.root_hash = hash.clone();
    }
    if let (Some(ours), Some(theirs)) = (options.matter, unit.matter) {
        if Matter::from_pairing_code(&theirs.pairing_code()).ok() == Some(ours) {
            options.matter = Some(theirs);
        }
    }
    setup::check_device(device, &options)?;
    crate::bundle::options(device, &options)
}

/// Refuses, unless forced, a nestlocal older than the unit's.
fn check_version(info: &Info, binary: &[u8], force: bool) -> Result<()> {
    let Some(ours) = identity(binary).0 else { return Ok(()) };
    if !force && newer_than(&info.version, &ours) {
        return Err(failure(format!("the unit runs nestlocal {}, newer than this app's {ours}: force the update to install \
                                    the older one", info.version)));
    }
    Ok(())
}

/// What every reason a run needs a USB setup ends with.
const USB_ONCE: &str = "set it up over USB once, and Wi-Fi updates work again after that";
/// The most refused paths a reason names.
const NAMED: usize = 3;

/// Why the unit takes a set changing paths only from a setup over USB;
/// None when it takes it over Wi-Fi. A unit reports the scratch volume's
/// room (Info::scratch_free_bytes) only when its nestlocal changes the
/// scratch directory and has it mounted: an older nestlocal refuses every
/// path under media. A current one answers for itself (check_set).
pub fn usb_only<'a>(info: &Info, mut paths: impl Iterator<Item = &'a str>) -> Option<String> {
    (info.scratch_free_bytes.is_none() && paths.any(in_scratch)).then(|| {
        let version = Some(info.version.as_str()).filter(|v| !v.is_empty()).unwrap_or("unknown");
        format!("the thermostat's nestlocal ({version}) can't move unfr0zen's files to the scratch volume over Wi-Fi; {USB_ONCE}")
    })
}

/// Why no Wi-Fi run reaches the unit info describes, known from info
/// alone: every setup keeps nestlocal on the scratch volume, so every set
/// for a unit without it there puts it there. A unit that answers a check
/// says so only once a run asks it with a set.
pub fn needs_usb(info: &Info) -> Option<String> {
    usb_only(info, [SCRATCH_NESTLOCAL].into_iter())
}

/// Whether a setup over USB gets the unit to take a path it refuses by
/// rule: the allow-list and the way to the path, which a setup moves
/// unfr0zen's files past, and the scratch volume, which it sets up. Not a
/// read-only filesystem, nor a rule this app does not know.
fn usb_fixes(rule: Rule) -> bool {
    matches!(rule, Rule::NoScratch | Rule::NotAllowed | Rule::Reached)
}

/// Refusals as the unit gave them: each why once, after NAMED of its paths
/// and how many more.
fn grouped(refused: &[&Refused]) -> String {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for refused in refused {
        match groups.iter_mut().find(|(why, _)| *why == refused.why) {
            Some((_, paths)) => paths.push(&refused.path),
            None => groups.push((&refused.why, vec![&refused.path])),
        }
    }
    groups.iter().map(|(why, paths)| {
        let mut named = paths.iter().take(NAMED).copied().collect::<Vec<_>>().join(", ");
        if paths.len() > NAMED {
            named += &format!(" and {} more", paths.len() - NAMED);
        }
        format!("{named} ({why})")
    }).collect::<Vec<_>>().join("; ")
}

/// What the unit's check of a set means for a run: with nothing refused it
/// goes on; refused only by rules a USB setup gets past, it needs one
/// (NeedsUsb); refused by any other, it fails with the unit's reasons.
fn verdict(refused: &[Refused]) -> Result<()> {
    if refused.is_empty() {
        return Ok(());
    }
    let (usb, other): (Vec<&Refused>, Vec<&Refused>) = refused.iter().partition(|refused| usb_fixes(refused.rule));
    if !other.is_empty() {
        return Err(failure(format!("the unit refused the update: {}", grouped(&other))));
    }
    Err(Error::NeedsUsb(format!("the thermostat can't take these over Wi-Fi: {}; {USB_ONCE}", grouped(&usb))))
}

/// Asks the unit, before anything is sent, whether its apply would take a
/// set changing paths (verdict). A unit whose nestlocal has no check is
/// held to usb_only, from info; a check that fails otherwise fails the run.
pub(crate) fn check_set(session: &Session, info: impl FnOnce() -> Result<Info>, paths: &[&str]) -> Result<()> {
    match session.check(paths)? {
        Some(refused) => verdict(&refused),
        None => usb_only(&info()?, paths.iter().copied()).map_or(Ok(()), |reason| Err(Error::NeedsUsb(reason))),
    }
}

/// The class a dry run's changes would send; None for none.
fn would_send(changes: &[Change]) -> Option<Class> {
    (!changes.is_empty()).then(|| match changes.iter().all(|change| RESTART_PATHS.contains(&change.name.as_str())) {
        true => Class::Restart,
        false => Class::Reboot,
    })
}

/// Patches the unit at host to inputs, its full settings, as one set, and
/// waits out its trial. A window that lapses first, from the key exchange to
/// the apply, is reopened once when the code is asked for, and the unit read
/// again.
pub fn patch(host: &str, code: Code, inputs: &SetupInputs, dry_run: bool, force: bool, progress: &dyn Fn(&str)) -> Result<Outcome> {
    let addr = address(host)?;
    check_inputs(inputs)?;
    let data_dir = default_data_dir();
    let run_dir = data_dir.join("runs").join(format!("{}-wifi", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    private_dir(&run_dir)?;
    let pki_dir = data_dir.join("nestlocal-pki");
    let build_screen = screen_builder(&run_dir);
    let new_pki = || stored_pki(&pki_dir);
    let trusted = trust::Store::default();
    let parts = Parts { nestlocal: assets::cached_nestlocal(), build_screen: &build_screen, new_pki: &new_pki, timing: &UNIT_TIMING,
                        trusted: &trusted };
    let outcome = patch_at(&addr, code, inputs, dry_run, force, progress, &parts)?;
    fs::write(run_dir.join("changes.txt"), changes_text(&outcome.changes))?;
    Ok(outcome)
}

fn patch_at(addr: &str, code: Code, inputs: &SetupInputs, dry_run: bool, force: bool, progress: &dyn Fn(&str), parts: &Parts)
            -> Result<Outcome> {
    check_inputs(inputs)?;
    if let Code::Given(code) = code {
        if !is_code(code) {
            return Err(failure("the code is the 8 digits the thermostat shows after Allow, or on Settings → UPDATE → Allow update"));
        }
    }
    let (agent, url) = (session::agent(), session::url(addr));
    let mut reopened = false;
    loop {
        progress(&format!("Connecting to nestlocal at {addr}"));
        let status = session::reach(&agent, &url, addr, parts.timing)?;
        // The unit takes no set while another is on trial.
        if status.pending && !dry_run {
            return Err(failure(ON_TRIAL));
        }
        let attempted = session::authenticate(&agent, addr, code, &status, progress, parts.timing, parts.trusted)
            .and_then(|session| {
                let attempted = attempt(&session, inputs, dry_run, force, progress, parts);
                // A set sent spends its session; any other end closes it.
                if dry_run || !matches!(attempted, Ok(Outcome { class: Some(_), .. })) {
                    session.close();
                }
                attempted
            });
        match attempted {
            Err(error) if session::lapsed(&error) && matches!(code, Code::Ask(_)) && !reopened => {
                reopened = true;
                progress("The update window closed before the update went: opening another, then reading the unit again");
            }
            Err(error) => return Err(error),
            Ok(outcome) => {
                if let (Some(class), false) = (outcome.class, dry_run) {
                    wait(&agent, &url, class, progress, parts.timing)?;
                }
                return Ok(outcome);
            }
        }
    }
}

/// One read of the unit, and the set built on it, sent unless dry_run.
fn attempt(session: &Session, inputs: &SetupInputs, dry_run: bool, force: bool, progress: &dyn Fn(&str), parts: &Parts)
           -> Result<Outcome> {
    let (mut volume, remote) = survey(session, force, progress)?;
    let options = options_for(remote.device, inputs, &remote.settings)?;
    check_version(&remote.info, parts.nestlocal, force)?;
    let changes = setup::set_up_on_device(&mut volume, remote.device, &options, parts.nestlocal, &mut || (parts.new_pki)(),
                                         dry_run, parts.build_screen)?;
    if changes.is_empty() {
        progress("Nothing to change: the unit has these settings");
    }
    // What a dry run would send, as RemoteVolume::send checks a set. A dry
    // run records nothing on the volume: its changes name the set's entries,
    // its directories, writes, removals and nlclient (set_up_on_device).
    if dry_run && !changes.is_empty() {
        // A directory's change is named with a trailing '/', its entry without.
        let paths: Vec<&str> = changes.iter().map(|change| change.name.trim_end_matches('/')).collect();
        check_set(session, || Ok(remote.info.clone()), &paths)?;
    }
    let class = if dry_run { would_send(&changes) } else { volume.sent() };
    Ok(Outcome { changes, class })
}

/// Waits, through the restart or reboot, until the unit's trial of the set
/// ends, and fails when the unit put the set back.
fn wait(agent: &ureq::Agent, url: &str, class: Class, progress: &dyn Fn(&str), timing: &Timing) -> Result<()> {
    progress(match class {
        Class::Restart => "Applied: nestlocal restarts, and runs the update on trial until nlclient reaches it",
        Class::Reboot => "Applied: the unit reboots, and runs the update on trial until nlclient reaches nestlocal on the network",
    });
    let start = Instant::now();
    let mut answered = true;
    while start.elapsed() < timing.wait {
        std::thread::sleep(timing.poll);
        // The unit drops off while it restarts or reboots.
        let Ok(now) = session::get(agent, url, timing)? else {
            answered = false;
            continue;
        };
        answered = true;
        if now.pending {
            continue;
        }
        // The set swapped in cleared the record of any rolled back before it.
        if now.cancelled_on_thermostat() {
            return Err(failure(session::PUT_BACK));
        }
        if now.rolled_back {
            return Err(failure("the unit rolled the update back: it did not come up, so the unit put its files back"));
        }
        progress("The unit kept the update");
        return Ok(());
    }
    let minutes = timing.wait.as_secs() / 60;
    Err(failure(if answered {
        format!("the update is still on trial after {minutes} minutes: the unit rolls it back if nlclient does not reach \
                 nestlocal")
    } else {
        "the unit stopped answering (asleep, or off the network) before its trial of the update ended: \
         Settings → UPDATE on the thermostat shows how it went".to_string()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, LazyLock, Mutex};
    use std::time::Duration;

    use spake2::{Ed25519Group, Identity, Password, Spake2};
    use unfr0zen_core::nestlocal::{self, Matter};
    use unfr0zen_core::setup::{set_up_on_device, Options};
    use unfr0zen_core::shell::{sha256_hex, Digests, File, Volume};
    use update_wire::bundle::{allowed, Applied, Bundle, Kind};
    use update_wire::read::{CheckRequest, Checked, Found, ReadRequest, Rule};
    use update_wire::seal;

    use crate::push::tests::{opened, reply, reply_bytes, sealed_answer, CODE};
    use update_wire::auth::{sign, CLIENT, CONFIRMATION, UNIT};
    use update_wire::{hex, unhex};

    use crate::session::CLOSED;

    /// name: (owner, mode, contents), as a root filesystem has them.
    type Files = BTreeMap<String, (String, u32, Vec<u8>)>;
    /// name: "uid:gid mode".
    type Dirs = BTreeMap<String, String>;

    const NESTLOCAL: &str = "nestlabs/sbin/nestlocal";
    const SCRATCH_NESTLOCAL: &str = update_wire::bundle::SCRATCH_NESTLOCAL;
    const BINARY: &[u8] = b"NESTLOCAL ARMV7";
    const HASH: &str = "$6$unit$hash";
    const CA: &[u8] = b"-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
    static PKI: LazyLock<Pki> = LazyLock::new(|| nestlocal::generate_pki().unwrap());
    static TRUSTED: LazyLock<trust::Store> = LazyLock::new(trust::Store::scratch);
    const FAST: Timing = Timing { connect: Duration::from_secs(2), poll: Duration::from_millis(10), wait: Duration::from_secs(5),
                                  request: Duration::from_secs(2), knock: 9 };

    fn no_screen(_: &[u8], _: &[nlclient::menu::Section]) -> Result<Vec<u8>> {
        Err(failure("this unit has no nlclient to build a screen for"))
    }

    /// A stock nlclient: an ELF, as the patch takes one.
    const NLCLIENT: &[u8] = b"\x7fELF\nstock nlclient\n";

    /// A screen builder whose screen is the stock nlclient with screen after it.
    fn screen(screen: &'static [u8]) -> impl Fn(&[u8], &[nlclient::menu::Section]) -> Result<Vec<u8>> {
        move |source, _| nlclient::Patch::journal(source, &[source, screen].concat())
    }

    fn own_pki() -> Result<Pki> {
        Ok(PKI.clone())
    }

    fn parts() -> Parts<'static> {
        Parts { nestlocal: BINARY, build_screen: &no_screen, new_pki: &own_pki, timing: &FAST, trusted: &TRUSTED }
    }

    fn files(list: Vec<(&str, u32, String)>) -> Files {
        list.into_iter().map(|(name, mode, text)| (name.to_string(), ("0:0".to_string(), mode, text.into_bytes()))).collect()
    }

    /// What every generation shares: nestlocal's stock files and the unit's
    /// own keys.
    fn common() -> Vec<(&'static str, u32, String)> {
        let sim: String = ["backplatesimulationrun", "backplatesimulationhvactype", "backplatesimulatetempandhum"].iter()
            .map(|key| format!("<a key=\"{key}\" value=\"0\"/>\n")).collect();
        let text = |data: &[u8]| String::from_utf8(data.to_vec()).unwrap();
        vec![
            ("etc/nestlabs/client.config", 0o644, format!("{sim}<a key=\"cloudregisterurl\" value=\"{}\"/>\n", nestlocal::STOCK_URL)),
            ("etc/monitrc", 0o600, "set daemon 30\ninclude /etc/monit.d/wpantund.monitrc\n".into()),
            ("etc/init.d/networking-defines", 0o644, "NETWORKING_TECHNOLOGIES=\"common wifi wpan\"\n".into()),
            ("etc/nestlabs/product.config", 0o644, "<a key=\"usephoenix\" value=\"0\"/>\n".into()),
            ("etc/ssl/certs/ca-bundle.pem", 0o644, text(&PKI.ca)),
            ("etc/nestlocal/server.crt", 0o644, text(&PKI.server_crt)),
            ("etc/nestlocal/server.key", 0o600, text(&PKI.server_key)),
            ("etc/hosts", 0o644, "127.0.0.1 localhost\n".into()),
            (NLSLEEP, 0o755, "start_nlsleepd () {\n ${STARTDAEMON} -q -b -m -p \"${NLSLEEPD_PID_FILE}\" -a ${NLSLEEPD_APP} -- x\n}\n".into()),
            // nlclient's start, which the local cloud library joins.
            ("etc/init.d/nestlabs", 0o755, "'monit_start')\n ${STARTDAEMON} -q -b -p \"${NLCLIENTPIDFILE}\" -a \"${NLCLIENTAPP}\" -- x\n ;;\n".into()),
        ]
    }

    /// A stock gen 3, with a root password and Nest's time server.
    fn gen3() -> Files {
        let mut list = common();
        list.extend([
            ("etc/shadow", 0o640, format!("root:{HASH}:16000:0:99999:7:::\ndaemon:*:16000:0:99999:7:::\n")),
            ("etc/inittab", 0o644, "::sysinit:/etc/init.d/rcS\nnull::respawn:/usr/sbin/unlocked-getty -L ttyGS0 115200\n".into()),
            ("usr/sbin/unlocked", 0o755, "#!/bin/sh\necho 0\n".into()),
            ("etc/init.d/rcS", 0o750, "a\n${INITDIR}/mountfs start\n${INITDIR}/sshd start\n${INITDIR}/nestlabs start\nb\n".into()),
            ("etc/init.d/rcH", 0o750, "a\n${INITDIR}/umountfs stop\nb\n".into()),
            ("etc/ssh/sshd_config", 0o644, "PermitRootLogin no\nHostKey /media/system-config/ssh/ssh_host_rsa_key\n".into()),
            ("etc/init.d/sshd", 0o750, "SSHRSAKEY=${SSHCONFDIR}/ssh_host_rsa_key\n    CreateKeyIfAbsent rsa \"${SSHRSAKEY}\"\n".into()),
            ("etc/init.d/usb-gadget", 0o750,
             "if [ `unlocked` -eq \"1\" ]; then\n  USB_GADGET_MOD=g_acm_ms\nelse\n  USB_GADGET_MOD=g_mass_storage\nfi\n".into()),
            ("etc/ssl/certs/ca-unlock-bundle.pem", 0o644, String::from_utf8(PKI.ca.clone()).unwrap()),
            ("etc/connman/main.conf", 0o644, "[General]\nFallbackTimeservers = time.nest.com\n".into()),
        ]);
        files(list)
    }

    /// A stock gen 2.
    fn gen2() -> Files {
        let mut list = common();
        list.extend([
            ("etc/shadow", 0o644, format!("root:{HASH}:16000:0:99999:7:::\n")),
            ("etc/inittab", 0o644, "::sysinit:/etc/init.d/rcS\n".into()),
            ("etc/init.d/rcS", 0o750, "a\n${INITDIR}/mountfs start\n${INITDIR}/sshd start\n${INITDIR}/nestlabs start\n".into()),
            ("etc/init.d/rcH", 0o750, "a\n${INITDIR}/umountfs stop\n".into()),
            ("etc/ntp.conf", 0o644, "server time.nest.com\n".into()),
        ]);
        files(list)
    }

    /// A root filesystem patched over USB, in memory.
    #[derive(Clone)]
    struct Disk {
        files: Files,
        dirs: Dirs,
    }

    impl Volume for Disk {
        fn read(&mut self, name: &str) -> Result<Option<File<Vec<u8>>>> {
            Ok(self.files.get(name).cloned())
        }
        fn inspect(&mut self, name: &str) -> Result<Option<File<String>>> {
            Ok(self.files.get(name).map(|(owner, mode, data)| (owner.clone(), *mode, sha256_hex(data))))
        }
        fn directory(&mut self, name: &str) -> Result<Option<String>> {
            Ok(self.dirs.get(name).cloned())
        }
        fn free_bytes(&mut self) -> Result<u64> {
            Ok(1 << 30)
        }
        fn shell_binary(&mut self) -> Result<String> {
            Ok("0 755".into())
        }
        fn write(&mut self, name: &str, data: &[u8], owner: &str, mode: u32) -> Result<()> {
            self.files.insert(name.into(), (owner.into(), mode, data.to_vec()));
            Ok(())
        }
        fn make_directory(&mut self, name: &str, mode: u32) -> Result<()> {
            self.dirs.insert(name.into(), format!("0:0 {mode:o}"));
            Ok(())
        }
        fn remove(&mut self, name: &str) -> Result<()> {
            self.files.remove(name);
            Ok(())
        }
        fn remount(&mut self, _: bool) -> Result<()> {
            Ok(())
        }
        fn digests(&mut self, names: &[String]) -> Result<Digests> {
            Ok(names.iter().map(|name| {
                let (owner, mode, data) = &self.files[name];
                (name.clone(), (Some(sha256_hex(data)), Some(format!("{owner} {mode:o}"))))
            }).collect())
        }
    }

    /// nlsleepd's init script, and the library it preloads to wake the unit.
    const NLSLEEP: &str = "etc/init.d/nlsleep";
    const WAKE_FILTER: &str = "nestlabs/lib/libwakefilter.so";

    /// The options a USB run takes for inputs, keeping the unit's root hash.
    fn options(inputs: &SetupInputs) -> Options {
        Options { root_hash: HASH.into(), ..inputs.build().unwrap() }
    }

    /// files after a USB run with options.
    fn usb(files: &Files, device: Device, options: &Options) -> Disk {
        usb_on(Disk { files: files.clone(), dirs: Dirs::new() }, device, options, &no_screen)
    }

    /// disk after a USB run with options, whose screen build_screen builds:
    /// the options through the device's bundle, as its USB run takes them.
    fn usb_on(disk: Disk, device: Device, options: &Options, build_screen: &dyn setup::ScreenBuilder) -> Disk {
        usb_as_given(disk, device, &crate::bundle::options(device, options).unwrap(), build_screen)
    }

    /// files after each generation's USB run, its options taken as its flow
    /// takes them, not through crate::bundle::options as a Wi-Fi run's are:
    /// the gen 3's packed into its bundle beside the run's Settings as JSON,
    /// which unfr0zen-apply unpacks and reads back on the unit; the gen 2's
    /// with the bundle its flow assembles on the host.
    fn usb_flow(files: &Files, device: Device, options: &Options) -> Disk {
        use unfr0zen_core::bundle::{Bundle as PatchBundle, Settings};
        let options = match device {
            Device::Diamond3 => {
                let bundle = PatchBundle::unpack(&crate::bundle::pack(device, &options.version, None).unwrap()).unwrap();
                let sent = serde_json::to_vec_pretty(&Settings::new(options, Some(&*PKI))).unwrap();
                let settings: Settings = serde_json::from_slice(&sent).unwrap();
                settings.options(&bundle).unwrap()
            }
            _ => Settings::new(options, None).options(&crate::bundle::assemble(device, &options.version).unwrap()).unwrap(),
        };
        usb_as_given(Disk { files: files.clone(), dirs: Dirs::new() }, device, &options, &no_screen)
    }

    /// disk after a patch with exactly these options.
    fn usb_as_given(mut disk: Disk, device: Device, options: &Options, build_screen: &dyn setup::ScreenBuilder) -> Disk {
        set_up_on_device(&mut disk, device, options, BINARY, &mut own_pki, false, build_screen).unwrap();
        disk
    }

    fn mqtt(url: &str) -> SetupInputs {
        SetupInputs { mqtt: Some(url.into()), mqtt_user: Some("thermostat".into()), mqtt_password: Some("s3cret".into()),
                      update_api: true, ..SetupInputs::default() }
    }

    /// What a scripted unit does next.
    #[derive(Default)]
    struct Script {
        /// The route ("/update/read") whose next request finds the window
        /// lapsed.
        lapse: Option<&'static str>,
        /// A file something else changes while the window is closed.
        on_lapse: Option<(&'static str, Vec<u8>)>,
        /// A file something else changes just before the apply is checked.
        on_apply: Option<(&'static str, Vec<u8>)>,
        /// The set's trial ends with the guard putting it back, and whether
        /// the unit says the thermostat asked for that.
        roll_back: bool,
        by_thermostat: bool,
        /// What a check is answered with in place of its answer.
        check_fails: Option<(u16, &'static str)>,
    }

    /// A scripted unit: nestlocal's update API over a root filesystem, one
    /// request per connection. As nestlocal's wifi_update/mod.rs, it refuses a
    /// request in a closed window 403, and in a replaced session 412.
    struct Unit {
        nlmodel: &'static str,
        files: Files,
        dirs: Dirs,
        open: bool,
        session: Option<(String, Vec<u8>)>,
        exchanges: u32,
        /// Prompts raised, and what the last shows.
        opened: u32,
        prompt: Option<&'static str>,
        pending: bool,
        polls: u32,
        /// Every name a read asked for, and every set applied.
        reads: Vec<String>,
        sets: Vec<Bundle>,
        /// The files the set on trial replaced, and whether the last set
        /// was put back.
        before: Option<(Files, Dirs)>,
        rolled_back: bool,
        script: Script,
        /// Sessions closed.
        closed: u32,
        /// Its nestlocal takes the scratch directory, and reports its room;
        /// an older one refuses every path under media.
        scratch: bool,
        /// Apply requests, taken or refused.
        applies: u32,
        /// Its nestlocal answers a check (an older one 404s it), and the
        /// paths of each it answered.
        checks: bool,
        checked: Vec<Vec<String>>,
        /// Its root filesystem is mounted read-only.
        read_only: bool,
    }

    impl Unit {
        fn new(nlmodel: &'static str, disk: Disk) -> Unit {
            Unit { nlmodel, files: disk.files, dirs: disk.dirs, open: true, session: None, exchanges: 0, opened: 0, prompt: None, pending: false,
                   polls: 0, reads: Vec::new(), sets: Vec::new(), before: None, rolled_back: false, script: Script::default(),
                   closed: 0, scratch: true, applies: 0, checks: true, checked: Vec::new(),
                   read_only: false }
        }

        fn status(&mut self) -> String {
            if self.pending {
                self.polls += 1;
                self.pending = self.polls < 2;
                if !self.pending && self.script.roll_back {
                    (self.files, self.dirs) = self.before.take().expect("a set on trial");
                    self.rolled_back = true;
                }
            }
            // The installed nestlocal, as nestlocal.init picks it.
            let binary = [SCRATCH_NESTLOCAL, NESTLOCAL].into_iter().find(|name| self.files.contains_key(*name)).unwrap_or(NESTLOCAL);
            let nestlocal = self.files.get(binary).map(|(_, _, data)| sha256_hex(data));
            serde_json::json!({"open": self.open, "installed": nestlocal, "binary": binary, "running": nestlocal,
                               "pending": self.pending,
                               "version": "v0.4.1", "overlay": 1, "rolled_back": self.rolled_back,
                               "rolled_back_by": self.script.by_thermostat.then_some("thermostat").filter(|_| self.rolled_back),
                               "prompt": self.prompt.map(|state| serde_json::json!({"id": "p1", "state": state}))}).to_string()
        }

        fn exchange(&mut self, message: &[u8]) -> String {
            self.exchanges += 1;
            let (spake, ours) = Spake2::<Ed25519Group>::start_b(&Password::new(CODE.as_bytes()), &Identity::new(CLIENT), &Identity::new(UNIT));
            let key = spake.finish(message).unwrap();
            let session = format!("{:032x}", self.exchanges);
            let confirm = sign(&key, CONFIRMATION);
            self.session = Some((session.clone(), key));
            serde_json::json!({"session": session, "message": hex(&ours), "confirm": confirm}).to_string()
        }

        fn info(&self) -> Info {
            Info { nlmodel: self.nlmodel.into(), version: "v0.4.1".into(), overlay: Some(1), free_bytes: 1 << 30,
                   scratch_free_bytes: self.scratch.then_some(1 << 30), shell_binary: "0 755".into(), pending: self.pending }
        }

        fn read(&mut self, request: &ReadRequest) -> Vec<Found> {
            self.reads.extend(request.names.iter().cloned());
            request.names.iter().map(|name| match (allowed(name), self.files.get(name), self.dirs.get(name)) {
                (Err(_), _, _) => Found::Absent,
                (_, Some((owner, mode, data)), _) => Found::File {
                    owner: owner.clone(), mode: *mode, sha256: sha256_hex(data), size: data.len() as u64,
                    data: request.contents.then(|| data.clone()),
                },
                (_, None, Some(owner_mode)) => Found::Directory { owner_mode: owner_mode.clone() },
                _ => Found::Absent,
            }).collect()
        }

        fn change(&mut self, (name, data): (&str, Vec<u8>)) {
            self.files.get_mut(name).expect("a file the unit has").2 = data;
        }

        /// Why its nestlocal refuses a set changing path before its
        /// contents, without the path, as bundle::refusals; None when it
        /// takes it.
        fn refusal(&self, path: &str) -> Option<(Rule, String)> {
            if self.read_only {
                return Some((Rule::ReadOnly, "/ is mounted read-only".into()));
            }
            if !self.scratch && in_scratch(path) {
                return Some(match self.checks {
                    true => (Rule::NoScratch, "no scratch volume is mounted at /media/scratch".into()),
                    false => (Rule::NotAllowed, "is outside what an update may change".into()),
                });
            }
            allowed(path).err().map(|why| (Rule::NotAllowed, why.strip_prefix(path).map_or(why.as_str(), str::trim_start).to_string()))
        }

        /// The paths of a check its apply would refuse.
        fn check(&mut self, request: &CheckRequest) -> Checked {
            self.checked.push(request.paths.clone());
            let refused = request.paths.iter().filter_map(|path| {
                let (rule, why) = self.refusal(path)?;
                Some(Refused { path: path.clone(), why, rule })
            }).collect();
            Checked { refused }
        }

        /// Applies set as nestlocal's wifi_update/bundle.rs does, or its refusal.
        fn apply(&mut self, set: &Bundle, contents: &[u8]) -> std::result::Result<Applied, (u16, String)> {
            if self.pending {
                return Err((409, "the previous update is still on trial".into()));
            }
            for entry in &set.entries {
                match self.refusal(&entry.path) {
                    Some((Rule::ReadOnly, _)) => return Err((409, "the root filesystem, or the scratch volume the set changes, \
                                                                  is read-only".into())),
                    Some((_, why)) => return Err((403, format!("{} {why}", entry.path))),
                    None => {}
                }
                let now = self.files.get(&entry.path).map(|(_, _, data)| sha256_hex(data));
                if entry.kind != Kind::Directory && now != entry.old {
                    return Err((409, format!("{} changed since it was read: start again", entry.path)));
                }
            }
            self.before = Some((self.files.clone(), self.dirs.clone()));
            let (mut at, mut applied) = (0, Applied { files: Vec::new(), directories: Vec::new() });
            for entry in &set.entries {
                let owner_mode = format!("{} {:o}", entry.owner, entry.mode);
                match entry.kind {
                    Kind::Directory => {
                        self.dirs.insert(entry.path.clone(), owner_mode.clone());
                        applied.directories.push((entry.path.clone(), owner_mode));
                    }
                    Kind::Remove => {
                        self.files.remove(&entry.path);
                    }
                    Kind::Write => {
                        let data = contents[at..at + entry.size as usize].to_vec();
                        at += data.len();
                        assert_eq!(entry.new.as_deref(), Some(sha256_hex(&data).as_str()), "{}", entry.path);
                        applied.files.push((entry.path.clone(), owner_mode, sha256_hex(&data)));
                        self.files.insert(entry.path.clone(), (entry.owner.clone(), entry.mode, data));
                    }
                }
            }
            assert_eq!(at, contents.len(), "every byte sent belongs to a write");
            self.sets.push(set.clone());
            (self.pending, self.polls, self.rolled_back) = (true, 0, false);
            Ok(applied)
        }
    }

    /// Serves unit on 127.0.0.1; its address, and the unit to inspect.
    fn serve(unit: Unit) -> (String, Arc<Mutex<Unit>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let unit = Arc::new(Mutex::new(unit));
        let shared = unit.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = Vec::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    head.push(line.trim_end().to_string());
                }
                let Some(request) = head.first() else { continue };
                let header = |name: &str| head.iter().find_map(|line| line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.trim().to_string()));
                let length = header("Content-Length").map_or(0, |n| n.parse().unwrap());
                let read_body = |reader: &mut BufReader<std::net::TcpStream>| {
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    body
                };
                let mut unit = shared.lock().unwrap();
                if unit.script.lapse.is_some_and(|route| request.starts_with(&format!("POST {route} "))) {
                    (unit.script.lapse, unit.open, unit.session) = (None, false, None);
                    if let Some(change) = unit.script.on_lapse.take() {
                        unit.change(change);
                    }
                }
                if request.starts_with("GET /update ") {
                    let body = unit.status();
                    reply(&mut stream, 200, &body);
                    continue;
                }
                if request.starts_with("POST /update/open ") {
                    read_body(&mut reader);
                    // Someone at the thermostat allows it at once.
                    (unit.opened, unit.open, unit.prompt) = (unit.opened + 1, true, Some("allowed"));
                    reply(&mut stream, 202, "{\"prompt\":\"asking\",\"id\":\"p1\"}");
                    continue;
                }
                if request.starts_with("POST /update/session ") {
                    let body = read_body(&mut reader);
                    if unit.open {
                        let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let answer = unit.exchange(&unhex(request["message"].as_str().unwrap()).unwrap());
                        reply(&mut stream, 200, &answer);
                    } else {
                        reply(&mut stream, 403, "window closed");
                    }
                    continue;
                }
                let session = match unit.session.clone() {
                    _ if !unit.open => Err((403, "window closed")),
                    Some((id, key)) if Some(&id) == header("X-Update-Session").as_ref() => Ok((id, key)),
                    _ => Err((412, "the session was replaced or expired")),
                };
                if let Some(path) = ["/update/info", "/update/read", "/update/check", "/update/close"].into_iter()
                    .find(|path| request.starts_with(&format!("POST {path} "))) {
                    let body = read_body(&mut reader);
                    // An older nestlocal's router knows no check.
                    if path == "/update/check" && !unit.checks {
                        reply(&mut stream, 404, "not found");
                        continue;
                    }
                    if let (Some((status, text)), "/update/check") = (unit.script.check_fails, path) {
                        reply(&mut stream, status, text);
                        continue;
                    }
                    let (id, key) = match session {
                        Ok(session) => session,
                        Err((status, text)) => {
                            reply(&mut stream, status, text);
                            continue;
                        }
                    };
                    if path == "/update/close" {
                        (unit.closed, unit.session) = (unit.closed + 1, None);
                    }
                    let answer = sealed_answer(&body, path, &id, &key, |request| match path {
                        "/update/close" => b"{}".to_vec(),
                        "/update/info" => serde_json::to_vec(&unit.info()).unwrap(),
                        "/update/check" => serde_json::to_vec(&unit.check(&serde_json::from_slice(request).unwrap())).unwrap(),
                        _ => serde_json::to_vec(&unit.read(&serde_json::from_slice(request).unwrap())).unwrap(),
                    }).unwrap();
                    reply_bytes(&mut stream, 200, &answer);
                    continue;
                }
                assert!(request.starts_with("POST /update/apply "), "{request}");
                unit.applies += 1;
                let (id, key) = match session {
                    Ok(session) => session,
                    Err((status, text)) => {
                        reply(&mut stream, status, text);
                        continue;
                    }
                };
                if header("Expect").is_some() {
                    stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap();
                }
                let body = read_body(&mut reader);
                let (set, contents, answer_aad) = opened(&body, &id, &key).expect("a set sealed in its session");
                // The set opened: the window is spent.
                (unit.open, unit.session) = (false, None);
                if let Some(change) = unit.script.on_apply.take() {
                    unit.change(change);
                }
                match unit.apply(&set, &contents) {
                    Ok(applied) => {
                        let sealed = seal::seal(&seal::keys(&key).to_client, &answer_aad, &serde_json::to_vec(&applied).unwrap());
                        reply_bytes(&mut stream, 202, &sealed);
                    }
                    Err((status, text)) => reply(&mut stream, status, &text),
                }
            }
        });
        (addr, unit)
    }

    fn read(addr: &str) -> Remote {
        read_settings_at(addr, Code::Given(CODE), &|_| {}, &FAST, &TRUSTED).unwrap()
    }

    fn send(addr: &str, code: Code, inputs: &SetupInputs) -> Result<Outcome> {
        patch_at(addr, code, inputs, false, false, &|_| {}, &parts())
    }

    /// A gen 3 a USB run patched with inputs, served.
    fn patched(inputs: &SetupInputs) -> (String, Arc<Mutex<Unit>>) {
        serve(Unit::new("Display-3.4", usb(&gen3(), Device::Diamond3, &options(inputs))))
    }

    fn paths(set: &Bundle) -> Vec<&str> {
        set.entries.iter().map(|entry| entry.path.as_str()).collect()
    }

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEB";

    /// Every option a Wi-Fi run can send, as a gen 3 and a gen 2 take them.
    fn every_option() -> SetupInputs {
        SetupInputs { zigbee: true, wwn_boiler: true, block_cloud: true, ntp_server: Some("time.lan".into()), ssh: true,
                      ssh_key_only: true, ssh_public_key_text: Some(KEY.into()), allow_unsigned_updates: true,
                      doom: true, device_name: Some("Hall <serial>".into()),
                      ha_entity_root: Some("nest_<serial>".into()), mqtt_tls: true, mqtt_ca_pem: Some(CA.to_vec()),
                      ..mqtt("tcp://broker.lan:8883") }
    }

    #[test]
    fn a_wifi_patch_sends_exactly_what_usb_would_write() {
        let inputs = every_option();
        for (nlmodel, device, files) in [("Display-3.4", Device::Diamond3, gen3()), ("Display-2.8", Device::J49, gen2())] {
            let stock = Disk { files, dirs: Dirs::new() };
            // The USB run's options as its flow (or the gen 3's engine on the
            // unit) takes them.
            let by_usb = usb_flow(&stock.files, device, &options(&inputs));
            let (addr, unit) = serve(Unit::new(nlmodel, stock));
            let outcome = send(&addr, Code::Given(CODE), &inputs).unwrap();
            assert_eq!(outcome.class, Some(Class::Reboot), "{nlmodel}");
            let mut unit = unit.lock().unwrap();
            assert_eq!(unit.sets.len(), 1, "{nlmodel}");
            // Each run seeds nestlocal's random source anew.
            let mut by_usb = by_usb;
            for files in [&mut unit.files, &mut by_usb.files] {
                let (_, _, seed) = files.get_mut(nestlocal::RANDOM_SEED).expect("seeded");
                assert_eq!(seed.len(), 64);
                seed.clear();
            }
            assert_eq!((&unit.files, &unit.dirs), (&by_usb.files, &by_usb.dirs), "{nlmodel}");
            // What the next release install (unfr0zen-apply update) re-runs
            // the patch with: the settings sent, not the last USB run's.
            let record: unfr0zen_core::bundle::Settings = serde_json::from_slice(&unit.files[setup::RECORD].2).unwrap();
            assert!(record.update_api && record.unsigned_updates && record.ssh_key_only, "{nlmodel}");
            assert_eq!(record.device_name.as_deref(), Some("Hall <serial>"), "{nlmodel}");
            assert_eq!(record.mqtt.unwrap().entity_root.as_deref(), Some("nest_<serial>"), "{nlmodel}");
            // nestlocal and the update engine go on the scratch volume, as a
            // USB run on the unit puts them, every generation.
            use unfr0zen_core::diamond3::scratch;
            assert!(unit.files.contains_key(scratch::name(setup::APPLY)), "{nlmodel} keeps its update engine");
            assert!(unit.files.contains_key(SCRATCH_NESTLOCAL) && !unit.files.contains_key(NESTLOCAL), "{nlmodel}");
            assert!(unit.files.contains_key(scratch::MANIFEST), "{nlmodel}");
        }
    }

    #[test]
    fn every_setting_reads_back_and_sends_nothing_again() {
        for (nlmodel, device, files) in [("Display-3.4", Device::Diamond3, gen3()), ("Display-2.8", Device::J49, gen2())] {
            let (addr, unit) = serve(Unit::new(nlmodel, usb(&files, device, &options(&every_option()))));
            let remote = read(&addr);
            let again = inputs_from(&remote.settings);
            assert_eq!((again.device_name.as_deref(), again.ha_entity_root.as_deref()), (Some("Hall <serial>"), Some("nest_<serial>")));
            assert!(again.ssh_key_only && again.allow_unsigned_updates && again.update_api, "{nlmodel}");
            assert_eq!(again.ssh_public_key_text.as_deref(), Some(KEY), "{nlmodel}");
            let outcome = send(&addr, Code::Given(CODE), &again).unwrap();
            assert_eq!((outcome.changes, outcome.class), (Vec::new(), None), "{nlmodel}");
            assert!(unit.lock().unwrap().sets.is_empty(), "{nlmodel}");
        }
    }

    #[test]
    fn the_restart_paths_are_the_ones_the_patch_writes() {
        for name in [nestlocal::OPTIONS, nestlocal::PASSWORD_FILE, nestlocal::CA_FILE, nestlocal::NAME_FILE, setup::RECORD,
                     NESTLOCAL, SCRATCH_NESTLOCAL, unfr0zen_core::diamond3::scratch::MANIFEST] {
            assert!(RESTART_PATHS.contains(&name), "{name}");
        }
    }

    /// A gen 3 an older setup left nestlocal on the root volume of, whose
    /// nestlocal takes nothing on the scratch volume and has no check.
    fn before_scratch() -> Unit {
        let mut files = gen3();
        files.insert(NESTLOCAL.into(), ("0:0".into(), 0o755, b"OLDER NESTLOCAL".to_vec()));
        Unit { scratch: false, checks: false, ..Unit::new("Display-3.4", Disk { files, dirs: Dirs::new() }) }
    }

    /// An older unit 404s the check: the run holds it to the scratch rule.
    #[test]
    fn a_set_for_the_scratch_volume_is_refused_before_any_upload_to_a_unit_without_it() {
        let inputs = mqtt("tcp://broker.lan:1883");
        let (addr, unit) = serve(before_scratch());
        let reason = needs_usb(&unit.lock().unwrap().info()).expect("known from the unit's information");
        assert!(reason.contains("(v0.4.1)") && reason.contains("over USB once"), "{reason}");
        let remote = read(&addr);
        assert_eq!(remote.usb_reason.as_ref(), Some(&reason), "known at the read");
        for dry_run in [true, false] {
            let error = patch_at(&addr, Code::Given(CODE), &inputs, dry_run, false, &|_| {}, &parts()).err().expect("refused");
            assert!(matches!(&error, Error::NeedsUsb(why) if *why == reason), "{dry_run}: {error}");
            assert_eq!(error.to_string(), format!("this change needs a USB setup: {reason}"));
        }
        let unit = unit.lock().unwrap();
        assert_eq!((unit.applies, unit.sets.len()), (0, 0), "nothing was sent");
        assert!(unit.files.contains_key(NESTLOCAL) && !unit.files.contains_key(SCRATCH_NESTLOCAL));
    }

    #[test]
    fn a_unit_that_reports_the_scratch_volume_takes_the_same_set() {
        // Without a check, by the scratch rule; with one, by its empty answer.
        for checks in [false, true] {
            let unit = Unit { scratch: true, checks, ..before_scratch() };
            assert_eq!(needs_usb(&unit.info()), None);
            let (addr, unit) = serve(unit);
            let inputs = mqtt("tcp://broker.lan:1883");
            patch_at(&addr, Code::Given(CODE), &inputs, true, false, &|_| {}, &parts()).unwrap();
            let outcome = send(&addr, Code::Given(CODE), &inputs).unwrap();
            assert_eq!(outcome.class, Some(Class::Reboot));
            let unit = unit.lock().unwrap();
            assert_eq!((unit.applies, unit.sets.len()), (1, 1));
            assert!(unit.files.contains_key(SCRATCH_NESTLOCAL) && !unit.files.contains_key(NESTLOCAL));
            match checks {
                true => {
                    // The dry run checks the set the run sends.
                    let sorted = |paths: &[String]| {
                        let mut paths = paths.to_vec();
                        paths.sort();
                        paths
                    };
                    let sent = sorted(&paths(&unit.sets[0]).iter().map(|p| p.to_string()).collect::<Vec<_>>());
                    assert_eq!(unit.checked.len(), 2);
                    assert_eq!(sorted(&unit.checked[0]), sent, "the dry run's");
                    assert_eq!(sorted(&unit.checked[1]), sent, "the run's");
                    assert!(sent.iter().any(|path| unit.dirs.contains_key(path)), "with its directories");
                }
                false => assert!(unit.checked.is_empty()),
            }
        }
    }

    /// A current unit says itself what it refuses, whatever its info shows.
    #[test]
    fn a_set_the_unit_s_check_refuses_needs_usb_and_sends_nothing() {
        let inputs = mqtt("tcp://broker.lan:1883");
        let unit = Unit { checks: true, ..before_scratch() };
        let (addr, unit) = serve(unit);
        // Known at the read, from its check of nestlocal's own path.
        let remote = read(&addr);
        assert_eq!(remote.usb_reason.unwrap(), format!("the thermostat can't take these over Wi-Fi: {SCRATCH_NESTLOCAL} \
                                                      (no scratch volume is mounted at /media/scratch); {USB_ONCE}"));
        for dry_run in [true, false] {
            let error = patch_at(&addr, Code::Given(CODE), &inputs, dry_run, false, &|_| {}, &parts()).err().expect("refused");
            let Error::NeedsUsb(why) = &error else { panic!("{dry_run}: {error}") };
            assert!(why.starts_with("the thermostat can't take these over Wi-Fi: media/scratch/.unfr0zen")
                    && why.contains(" more (no scratch volume is mounted at /media/scratch)")
                    && why.matches("no scratch volume").count() == 1
                    && why.ends_with(USB_ONCE), "{dry_run}: {why}");
        }
        let unit = unit.lock().unwrap();
        assert_eq!((unit.applies, unit.sets.len(), unit.checked.len()), (0, 0, 3), "the read's, then each run's; nothing sent");
        assert_eq!(unit.checked[0], [SCRATCH_NESTLOCAL]);
        assert!(unit.checked[2].contains(&SCRATCH_NESTLOCAL.to_string()));
        assert!(unit.files.contains_key(NESTLOCAL) && !unit.files.contains_key(SCRATCH_NESTLOCAL));
    }

    /// A read-only root is no matter for a USB setup: the unit's reason.
    #[test]
    fn a_set_the_unit_refuses_as_read_only_fails_with_its_reason() {
        let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
        unit.lock().unwrap().read_only = true;
        let remote = read(&addr);
        assert!(remote.usb_reason.is_none(), "no USB setup to warn of");
        for dry_run in [true, false] {
            let error = patch_at(&addr, Code::Given(CODE), &mqtt("tcp://b.lan:1883"), dry_run, false, &|_| {}, &parts())
                .err().expect("refused");
            assert!(!matches!(error, Error::NeedsUsb(_)), "{error}");
            let text = error.to_string();
            assert!(text.starts_with("the unit refused the update: ") && text.ends_with(" (/ is mounted read-only)")
                    && text.matches("read-only").count() == 1, "{text}");
        }
        assert_eq!(unit.lock().unwrap().applies, 0, "nothing was sent");
    }

    /// A check that fails, other than an older unit's 404, fails the run:
    /// no fall back to the scratch rule, and nothing sent.
    #[test]
    fn a_failed_check_stops_the_run_without_falling_back() {
        for (status, text, said) in [(500, "statvfs failed", "(500): statvfs failed"), (403, "window closed", CLOSED)] {
            let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
            unit.lock().unwrap().script.check_fails = Some((status, text));
            for dry_run in [true, false] {
                let error = patch_at(&addr, Code::Given(CODE), &mqtt("tcp://b.lan:1883"), dry_run, false, &|_| {}, &parts())
                    .err().expect("failed");
                assert!(!matches!(error, Error::NeedsUsb(_)) && error.to_string().contains(said), "{status} {dry_run}: {error}");
            }
            let unit = unit.lock().unwrap();
            assert_eq!((unit.applies, unit.sets.len()), (0, 0), "{status}: nothing was sent");
        }
    }

    #[test]
    fn a_refusal_gives_each_why_once_with_a_few_of_its_paths() {
        let refused = |path: &str, why: &str, rule| Refused { path: path.into(), why: why.into(), rule };
        let outside = |i: usize| refused(&format!("boot/{i}"), "is outside what an update may change", Rule::NotAllowed);
        let reason = |refusals: &[Refused]| match verdict(refusals) {
            Err(Error::NeedsUsb(reason)) => reason,
            other => panic!("{other:?}"),
        };
        verdict(&[]).unwrap();
        assert_eq!(reason(&[outside(0)]), format!("the thermostat can't take these over Wi-Fi: boot/0 \
                                                   (is outside what an update may change); {USB_ONCE}"));
        let mut many: Vec<Refused> = (0..5).map(outside).collect();
        many.insert(1, refused(SCRATCH_NESTLOCAL, "no scratch volume is mounted at /media/scratch", Rule::NoScratch));
        assert_eq!(reason(&many), format!("the thermostat can't take these over Wi-Fi: boot/0, boot/1, boot/2 and 2 more \
                                           (is outside what an update may change); {SCRATCH_NESTLOCAL} (no scratch volume is \
                                           mounted at /media/scratch); {USB_ONCE}"));
        // Any refusal a USB setup does not get past fails as the unit's.
        for rule in [Rule::ReadOnly, Rule::Other] {
            let mut mixed = many.clone();
            mixed.push(refused("etc/x", "/ is mounted read-only", rule));
            mixed.push(refused("etc/y", "/ is mounted read-only", rule));
            let error = verdict(&mixed).unwrap_err();
            assert!(!matches!(error, Error::NeedsUsb(_)), "{error}");
            assert_eq!(error.to_string(), "the unit refused the update: etc/x, etc/y (/ is mounted read-only)");
        }
    }

    #[test]
    fn only_a_set_touching_the_scratch_directory_needs_usb() {
        let unit = before_scratch();
        let info = unit.info();
        assert_eq!(usb_only(&info, [NESTLOCAL, nestlocal::OPTIONS].into_iter()), None);
        assert!(usb_only(&info, [nestlocal::OPTIONS, unfr0zen_core::diamond3::scratch::MANIFEST].into_iter()).is_some());
        assert!(usb_only(&Info { scratch_free_bytes: Some(1), ..info }, [SCRATCH_NESTLOCAL].into_iter()).is_none());
    }

    #[test]
    fn a_read_or_a_dry_run_closes_its_session() {
        let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
        read(&addr);
        assert_eq!(unit.lock().unwrap().closed, 1);
        let outcome = patch_at(&addr, Code::Given(CODE), &mqtt("tcp://b.lan:1883"), true, false, &|_| {}, &parts()).unwrap();
        assert!(outcome.class.is_some() && unit.lock().unwrap().sets.is_empty());
        assert_eq!(unit.lock().unwrap().closed, 2);
        // A set sent ends its session itself.
        send(&addr, Code::Given(CODE), &mqtt("tcp://b.lan:1883")).unwrap();
        let unit = unit.lock().unwrap();
        assert_eq!((unit.closed, unit.sets.len()), (2, 1));
    }

    #[test]
    fn unchanged_settings_send_nothing() {
        let inputs = SetupInputs { matter: true, zigbee: true, mqtt_tls: true, mqtt_ca_pem: Some(CA.to_vec()), ..mqtt("tcp://broker.lan:8883") };
        // The exact discriminator, which a pairing code does not carry.
        let options = Options { matter: Some(Matter { passcode: 20202021, discriminator: 1234 }), ..options(&inputs) };
        let (addr, unit) = serve(Unit::new("Display-3.4", usb(&gen3(), Device::Diamond3, &options)));
        let remote = read(&addr);
        assert_eq!(remote.settings.matter, options.matter);
        let outcome = send(&addr, Code::Given(CODE), &inputs_from(&remote.settings)).unwrap();
        assert_eq!((outcome.changes, outcome.class), (Vec::new(), None));
        let unit = unit.lock().unwrap();
        assert!(unit.sets.is_empty() && unit.open, "nothing sent, and the window is not spent");
    }

    #[test]
    fn options_only_restart_and_system_files_reboot() {
        let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
        let inputs = SetupInputs { mqtt: Some("tcp://b.lan:1883".into()), ..inputs_from(&read(&addr).settings) };
        assert_eq!(send(&addr, Code::Given(CODE), &inputs).unwrap().class, Some(Class::Restart));
        let set = unit.lock().unwrap().sets[0].clone();
        assert_eq!((paths(&set), set.class), (vec![nestlocal::OPTIONS, setup::RECORD], Class::Restart));

        let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
        let inputs = SetupInputs { block_cloud: true, ..inputs_from(&read(&addr).settings) };
        assert_eq!(send(&addr, Code::Given(CODE), &inputs).unwrap().class, Some(Class::Reboot));
        let set = unit.lock().unwrap().sets[0].clone();
        assert!(paths(&set).contains(&"etc/hosts") && set.class == Class::Reboot, "{set:?}");
    }

    #[test]
    fn a_set_that_adds_the_wake_filter_reboots() {
        // As a USB run that installed no filter left the unit.
        let options = crate::bundle::options(Device::Diamond3, &options(&mqtt("tcp://a.lan:1883"))).unwrap();
        let options = Options { wake_filter: Vec::new(), ..options };
        let disk = usb_as_given(Disk { files: gen3(), dirs: Dirs::new() }, Device::Diamond3, &options, &no_screen);
        let (addr, unit) = serve(Unit::new("Display-3.4", disk));
        let inputs = inputs_from(&read(&addr).settings);
        assert_eq!(send(&addr, Code::Given(CODE), &inputs).unwrap().class, Some(Class::Reboot));
        let set = unit.lock().unwrap().sets[0].clone();
        assert!(paths(&set).contains(&WAKE_FILTER) && paths(&set).contains(&NLSLEEP), "{set:?}");
        assert_eq!(set.class, Class::Reboot);
    }

    #[test]
    fn turning_off_nestlocal_or_the_update_api_is_refused() {
        let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
        let on = inputs_from(&read(&addr).settings);
        for inputs in [SetupInputs { update_api: false, ..on.clone() },
                       SetupInputs { mqtt: None, mqtt_user: None, mqtt_password: None, ..on.clone() },
                       SetupInputs { doom: true, ..SetupInputs::default() }] {
            let error = send(&addr, Code::Given(CODE), &inputs).err().unwrap().to_string();
            assert!(error.contains("use USB to remove nestlocal or the update API"), "{error}");
        }
        let unit = unit.lock().unwrap();
        assert_eq!((unit.exchanges, unit.sets.len()), (1, 0), "refused before the code is tried");
    }

    #[test]
    fn a_j49_refuses_a_usb_shell_or_dark_theme_change() {
        let (addr, unit) = serve(Unit::new("Display-2.8", usb(&gen2(), Device::J49, &options(&mqtt("tcp://a.lan:1883")))));
        let remote = read(&addr);
        assert_eq!((remote.device, remote.settings.usb_shell, remote.settings.dark_theme), (Device::J49, false, Some(false)));
        let same = inputs_from(&remote.settings);
        for inputs in [SetupInputs { usb_shell: true, ..same.clone() }, SetupInputs { dark_theme: true, ..same.clone() }] {
            let error = send(&addr, Code::Given(CODE), &inputs).err().unwrap().to_string();
            assert!(error.contains("change over USB"), "{error}");
        }
        assert!(unit.lock().unwrap().sets.is_empty());
        assert_eq!(send(&addr, Code::Given(CODE), &same).unwrap().class, None);
        // The read, both refusals and the send with nothing to change each
        // close their session.
        assert_eq!(unit.lock().unwrap().closed, 4);
    }

    #[test]
    fn a_blank_root_password_keeps_the_units_hash() {
        let root_hash = |unit: &Unit| String::from_utf8_lossy(&unit.files["etc/shadow"].2).lines()
            .find_map(|line| line.strip_prefix("root:").map(|rest| rest.split(':').next().unwrap().to_string())).unwrap();
        let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
        let inputs = SetupInputs { mqtt: Some("tcp://b.lan:1883".into()), ..inputs_from(&read(&addr).settings) };
        assert_eq!(inputs.root_password, None);
        send(&addr, Code::Given(CODE), &inputs).unwrap();
        {
            let unit = unit.lock().unwrap();
            assert!(!paths(&unit.sets[0]).contains(&"etc/shadow"));
            assert_eq!(root_hash(&unit), HASH);
        }

        let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
        let inputs = SetupInputs { root_password: Some("hunter2".into()), ..inputs_from(&read(&addr).settings) };
        send(&addr, Code::Given(CODE), &inputs).unwrap();
        let unit = unit.lock().unwrap();
        assert!(paths(&unit.sets[0]).contains(&"etc/shadow"));
        assert!(root_hash(&unit).starts_with("$6$") && root_hash(&unit) != HASH);
    }

    #[test]
    fn the_kernel_fit_is_never_read_or_written() {
        let mut stock = gen3();
        stock.insert("boot/kernel_fdt.itb".into(), ("0:0".into(), 0o644, b"FIT".to_vec()));
        let (addr, unit) = serve(Unit::new("Display-3.4", Disk { files: stock, dirs: Dirs::new() }));
        send(&addr, Code::Given(CODE), &mqtt("tcp://a.lan:1883")).unwrap();
        let unit = unit.lock().unwrap();
        assert!(!unit.reads.iter().any(|name| name.starts_with("boot/")), "{:?}", unit.reads);
        assert!(!paths(&unit.sets[0]).iter().any(|name| name.starts_with("boot/")));
        assert_eq!(unit.files["boot/kernel_fdt.itb"].2, b"FIT");
    }

    #[test]
    fn a_lapsed_window_is_reopened_and_a_changed_file_refuses_the_set() {
        let theirs = |unit: &Arc<Mutex<Unit>>| String::from_utf8(unit.lock().unwrap().files[nestlocal::OPTIONS].2.clone()).unwrap()
            .replace("a.lan", "c.lan").into_bytes();
        let asked = Cell::new(0);
        let ask = |_: &str| {
            asked.set(asked.get() + 1);
            Some(CODE.to_string())
        };

        // The form outlives the window: the run opens another, asks for its
        // code and reads again, so it builds on a file changed meanwhile.
        let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
        let inputs = SetupInputs { mqtt: Some("tcp://b.lan:1883".into()), ..inputs_from(&read(&addr).settings) };
        let changed = theirs(&unit);
        unit.lock().unwrap().script = Script { lapse: Some("/update/apply"), on_lapse: Some((nestlocal::OPTIONS, changed.clone())), ..Script::default() };
        assert_eq!(send(&addr, Code::Ask(&ask), &inputs).unwrap().class, Some(Class::Restart));
        {
            let unit = unit.lock().unwrap();
            assert_eq!((asked.get(), unit.opened, unit.sets.len()), (2, 1, 1));
            assert_eq!(unit.sets[0].entries[0].old, Some(sha256_hex(&changed)));
            assert!(String::from_utf8_lossy(&unit.files[nestlocal::OPTIONS].2).contains("b.lan"));
        }

        // A file that changes after the read: the unit refuses the whole set.
        let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
        let changed = theirs(&unit);
        let mut before = unit.lock().unwrap().files.clone();
        before.get_mut(nestlocal::OPTIONS).unwrap().2 = changed.clone();
        unit.lock().unwrap().script = Script { lapse: Some("/update/apply"), on_apply: Some((nestlocal::OPTIONS, changed)), ..Script::default() };
        let error = send(&addr, Code::Ask(&ask), &inputs).err().unwrap().to_string();
        assert!(error.contains("changed since it was read"), "{error}");
        {
            let unit = unit.lock().unwrap();
            assert!(unit.sets.is_empty() && unit.files == before, "nothing changes");
        }

        // A code given cannot open another window.
        let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
        unit.lock().unwrap().script = Script { lapse: Some("/update/apply"), ..Script::default() };
        assert_eq!(send(&addr, Code::Given(CODE), &inputs).err().unwrap().to_string(), CLOSED);
    }

    #[test]
    fn a_window_that_lapses_before_the_apply_is_reopened_too() {
        let asked = Cell::new(0);
        let ask = |_: &str| {
            asked.set(asked.get() + 1);
            Some(CODE.to_string())
        };
        // While the run reads the unit, or between the status check and the
        // key exchange: it opens another window, asks its code, and reads
        // again.
        for route in ["/update/session", "/update/info", "/update/read"] {
            asked.set(0);
            let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
            let inputs = SetupInputs { mqtt: Some("tcp://b.lan:1883".into()), ..inputs_from(&read(&addr).settings) };
            unit.lock().unwrap().script = Script { lapse: Some(route), ..Script::default() };
            assert_eq!(send(&addr, Code::Ask(&ask), &inputs).unwrap().class, Some(Class::Restart), "{route}");
            let unit = unit.lock().unwrap();
            assert_eq!((asked.get(), unit.opened, unit.sets.len()), (2, 1, 1), "{route}");
            assert!(String::from_utf8_lossy(&unit.files[nestlocal::OPTIONS].2).contains("b.lan"), "{route}");
        }
        // A code given cannot open another: the run says the window closed.
        let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
        unit.lock().unwrap().script = Script { lapse: Some("/update/read"), ..Script::default() };
        assert_eq!(send(&addr, Code::Given(CODE), &mqtt("tcp://b.lan:1883")).err().unwrap().to_string(), CLOSED);
        assert!(unit.lock().unwrap().sets.is_empty());
    }

    #[test]
    fn a_set_the_unit_rolls_back_fails_the_run() {
        // Options alone: nestlocal's binary does not change, so only the
        // unit's record tells a rollback from a set kept.
        let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
        let inputs = SetupInputs { mqtt: Some("tcp://b.lan:1883".into()), ..inputs_from(&read(&addr).settings) };
        let before = unit.lock().unwrap().files.clone();
        unit.lock().unwrap().script = Script { roll_back: true, ..Script::default() };
        let error = send(&addr, Code::Given(CODE), &inputs).err().unwrap().to_string();
        assert!(error.contains("rolled the update back"), "{error}");
        let unit = unit.lock().unwrap();
        assert_eq!(paths(&unit.sets[0]), vec![nestlocal::OPTIONS, setup::RECORD]);
        assert!(unit.files == before, "the unit has its files back");
    }

    #[test]
    fn a_set_cancelled_on_the_thermostat_says_so() {
        let (addr, unit) = patched(&mqtt("tcp://a.lan:1883"));
        let inputs = SetupInputs { mqtt: Some("tcp://b.lan:1883".into()), ..inputs_from(&read(&addr).settings) };
        unit.lock().unwrap().script = Script { roll_back: true, by_thermostat: true, ..Script::default() };
        let error = send(&addr, Code::Given(CODE), &inputs).err().unwrap().to_string();
        assert_eq!(error, session::PUT_BACK);
    }

    #[test]
    fn the_wire_names_the_scratch_directory_and_nestlocal_there_as_the_setup_does() {
        use unfr0zen_core::diamond3::scratch;
        assert_eq!(update_wire::bundle::SCRATCH_DIR, scratch::DIR);
        assert_eq!(update_wire::bundle::SCRATCH_NESTLOCAL, scratch::name(update_wire::bundle::ROOT_NESTLOCAL));
        assert_eq!(update_wire::bundle::ROOT_NESTLOCAL, NESTLOCAL);
        assert!(update_wire::bundle::RESTART_PATHS.contains(&scratch::MANIFEST));
    }

    #[test]
    fn a_screen_patched_nlclient_is_sent_whole_over_the_one_read() {
        let (first, second) = (screen(b"UNFR0ZEN screen 1"), screen(b"UNFR0ZEN screen 2"));
        let with = |build_screen| Parts { nestlocal: BINARY, build_screen, new_pki: &own_pki, timing: &FAST, trusted: &TRUSTED };
        let inputs = mqtt("tcp://a.lan:1883");
        for (nlmodel, device, mut stock) in [("Display-3.4", Device::Diamond3, gen3()), ("Display-2.8", Device::J49, gen2())] {
            stock.insert(nlclient::BINARY.into(), ("0:0".into(), 0o755, NLCLIENT.to_vec()));
            let (addr, unit) = serve(Unit::new(nlmodel, Disk { files: stock, dirs: Dirs::new() }));
            // A stock nlclient gets the screen; a screened one gets a newer
            // app's, from the stock the patch kept.
            for (build_screen, sets) in [(&first, 1), (&second, 2)] {
                let (now, by_usb) = {
                    let mut unit = unit.lock().unwrap();
                    unit.open = true;
                    let now = Disk { files: unit.files.clone(), dirs: unit.dirs.clone() };
                    (now.clone(), usb_on(now, device, &options(&inputs), build_screen))
                };
                let remote = read(&addr);
                assert_eq!((remote.device, remote.settings.dark_theme), (device, Some(false)));
                let outcome = patch_at(&addr, Code::Given(CODE), &inputs, false, false, &|_| {}, &with(build_screen)).unwrap();
                assert_eq!(outcome.class, Some(Class::Reboot), "{nlmodel}");
                let unit = unit.lock().unwrap();
                assert_eq!(unit.sets.len(), sets, "{nlmodel}");
                let entry = unit.sets[sets - 1].entries.iter().find(|entry| entry.path == nlclient::BINARY).expect("nlclient sent");
                let output = &by_usb.files[nlclient::BINARY].2;
                assert_eq!((entry.kind, entry.size), (Kind::Write, output.len() as u64), "{nlmodel}");
                assert_eq!(entry.old, Some(sha256_hex(&now.files[nlclient::BINARY].2)), "{nlmodel}: the SHA read, not the stock's");
                assert!(output.ends_with(if sets == 1 { b"screen 1" } else { b"screen 2" }));
                // Each run seeds nestlocal's random source anew where it has none.
                let unseeded = |files: &Files| {
                    let mut files = files.clone();
                    files.get_mut(nestlocal::RANDOM_SEED).expect("seeded").2.clear();
                    files
                };
                assert_eq!((unseeded(&unit.files), &unit.dirs), (unseeded(&by_usb.files), &by_usb.dirs), "{nlmodel}");
            }
            // The same screen again: nothing to send.
            unit.lock().unwrap().open = true;
            let outcome = patch_at(&addr, Code::Given(CODE), &inputs, false, false, &|_| {}, &with(&second)).unwrap();
            assert_eq!(outcome.class, None, "{nlmodel}");
            assert_eq!(unit.lock().unwrap().sets.len(), 2);
        }
    }

    #[test]
    fn an_unknown_nlmodel_is_refused_before_any_secret_is_read() {
        let disk = usb(&gen3(), Device::Diamond3, &options(&mqtt("tcp://a.lan:1883")));
        let (addr, unit) = serve(Unit::new("Onyx-1", disk));
        let error = read_settings_at(&addr, Code::Given(CODE), &|_| {}, &FAST, &TRUSTED).err().unwrap().to_string();
        assert!(error.contains("Onyx-1"), "{error}");
        let error = send(&addr, Code::Given(CODE), &mqtt("tcp://a.lan:1883")).err().unwrap().to_string();
        assert!(error.contains("Onyx-1"), "{error}");
        let unit = unit.lock().unwrap();
        assert!(unit.reads.is_empty() && unit.sets.is_empty(), "{:?}", unit.reads);
    }

    #[test]
    fn the_unit_s_ca_is_sent_and_described_as_the_unit_s() {
        let inputs = SetupInputs { mqtt_tls: true, mqtt_ca_pem: Some(CA.to_vec()), ..mqtt("tcp://broker.lan:8883") };
        assert_eq!(inputs.build().unwrap().mqtt.unwrap().ca.as_deref(), Some(CA));
        assert!(inputs.describe().contains("--mqtt-ca (the unit's)"), "{}", inputs.describe());
    }
}
