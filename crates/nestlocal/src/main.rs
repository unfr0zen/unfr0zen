//! nestlocal stands in for Nest's service on the thermostat itself and
//! bridges it to Home Assistant over MQTT.

// matter/'s handler chain is a deeply nested type.
#![recursion_limit = "256"]

mod arp;
mod bridge;
mod brightness;
mod clock;
mod cloud;
mod commands;
use nestlocal::device;
mod doom;
mod durable;
mod entropy;
mod fetch;
mod flags;
mod heatlink;
mod hotwater;
mod json;
mod liveness;
mod logger;
mod matter;
mod mqtt;
mod mqtttls;
mod naming;
mod network;
mod nlsettings;
mod occupancy;
mod package;
mod power;
mod schedule;
mod schedule_control;
mod scratch;
mod screen;
mod sensor;
mod settings_ui;
mod store;
mod structure;
mod theme;
mod thread_sleep;
mod update;
mod wake;
mod weather;
mod zigbee;

use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// What nestlocal.init waits for before it starts nlclient: the BLE chip's
/// stand-in is in place, where one is needed.
const NLCLIENT_MAY_START: &str = "nlclient-may-start";

/// Matter's BLE has the chip and nlclient's own BLE is off: nlclient still
/// loads firmware into the chip as it starts, so it must find the stand-in.
#[cfg(target_os = "linux")]
fn matter_takes_ble(config: &flags::Config) -> bool {
    config.matter && sensor::port::stock_ble_off().unwrap_or(false)
}

/// rustls's cryptography: ring, for the server and the broker alike.
pub fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// The hostname, which the unit sets to its serial.
fn hostname() -> std::io::Result<String> {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most buf.len() bytes into buf.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Ok(String::from_utf8_lossy(&buf[..len]).into_owned())
}

/// The processes running `name`.
pub fn pids(name: &str) -> Vec<i32> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
            let comm = std::fs::read_to_string(entry.path().join("comm")).ok()?;
            (comm.trim_end() == name).then_some(pid)
        })
        .collect()
}

fn server_tls(cert: &str, key: &str) -> Result<Arc<rustls::ServerConfig>, String> {
    let certs = CertificateDer::pem_file_iter(cert)
        .map_err(|e| format!("{cert}: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{cert}: {e}"))?;
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("{key}: {e}"))?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("{cert}: {e}"))?;
    Ok(Arc::new(config))
}

/// Ends nestlocal on a panic on any thread, logged, for monit to start it
/// again: otherwise a Zigbee or Matter thread gone leaves it running without it.
fn exit_on_panic() {
    std::panic::set_hook(Box::new(|info| {
        logf!("nestlocal: thread {}: {info}", std::thread::current().name().unwrap_or("unnamed"));
        std::process::exit(101);
    }));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut config = match flags::parse(&args) {
        Ok(c) => c,
        Err(None) => {
            eprint!("{}", flags::usage());
            std::process::exit(0);
        }
        Err(Some(e)) => {
            eprint!("{e}\n{}", flags::usage());
            std::process::exit(2);
        }
    };
    if config.syslog {
        #[cfg(unix)]
        if let Err(e) = logger::to_syslog() {
            fatal!("{e}");
        }
        if !config.log_file.is_empty() {
            if let Err(e) = logger::to_file(&config.log_file) {
                logf!("{}: {e}", config.log_file);
            }
        }
    }
    exit_on_panic();
    if config.serial.is_empty() {
        config.serial = hostname().unwrap_or_else(|e| fatal!("{e}"));
    }
    if config.prepare_settings {
        prepare_settings(&config);
        return;
    }
    // An init script older than the binary passes this alone, and none of
    // the options: the rest of prepare_settings would take them as off.
    if !config.seed_structure.is_empty() {
        match structure::seed_structure_id(&config.seed_structure, &config.serial) {
            Ok(true) => logf!("nestlocal: seeded structure_id in {}", config.seed_structure),
            Ok(false) => {}
            Err(e) => fatal!("{e}"),
        }
        return;
    }
    // Without Matter, nlsleepd's library must not set its patterns.
    if !config.matter {
        let _ = std::fs::remove_file(matter::wake_patterns::LIST);
    }

    #[cfg(target_os = "linux")]
    if let (true, Some(ble)) = (config.external_temperature || matter_takes_ble(&config), device::current().da14580()) {
        if let Err(e) = sensor::port::start_holder(ble.uart) {
            logf!("BLE stand-in: {e}");
        }
    }
    #[cfg(target_os = "linux")]
    if let (true, Some(device::Ble::Hci(link))) = (config.external_temperature, &device::current().ble) {
        if let Err(e) = sensor::port::start_holder(link) {
            logf!("external temperature: the stand-in controller's PTY: {e}");
        }
    }
    entropy::seed();
    install_crypto();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap_or_else(|e| fatal!("{e}"));
    runtime.block_on(run(config));
}

/// nlclient's settings file, as it should be when nlclient starts.
fn prepare_settings(config: &flags::Config) {
    let path = &config.settings;
    match structure::seed_structure_id(path, &config.serial) {
        Ok(true) => logf!("nestlocal: seeded structure_id in {path}"),
        Ok(false) => {}
        Err(e) => fatal!("{e}"),
    }
    let identity = std::path::Path::new(&config.external_temperature_store);
    if !config.external_temperature {
        if let Err(e) = sensor::forget_settings(identity, std::path::Path::new(path)) {
            logf!("external temperature: {e}");
        }
    } else if device::current().ble.is_some() || device::current().omap() {
        let store = store::Store::new(&config.state);
        let settings = device::current().omap().then_some(std::path::Path::new(path));
        let where_id = sensor::where_id(&store, &config.serial, settings);
        if let Err(e) =
            sensor::prepare_settings(identity, std::path::Path::new(path), &config.serial, where_id.as_deref())
        {
            logf!("external temperature: {e}");
        }
    }
}

/// The stand-in sensor over gen 3 BLE, the gen E's Bluetooth controller or the gen 1's and gen 2's loopback RCS input, or gone
/// from a unit that no longer has the option.
fn start_sensor(config: &flags::Config, store: &Arc<store::Store>) -> Option<Arc<sensor::Sensor>> {
    let identity = std::path::Path::new(&config.external_temperature_store);
    if !config.external_temperature {
        sensor::withdraw(identity, store, &config.serial);
        return None;
    }
    let device = device::current();
    if device.ble.is_none() && !device.omap() {
        logf!("external temperature: this unit has no supported sensor transport");
        return None;
    }
    let sensor = sensor::Sensor::open(
        identity,
        &config.serial,
        std::path::Path::new(&config.settings),
        config.external_temperature_timeout,
    );
    let sensor = match sensor {
        Ok(sensor) => sensor,
        Err(e) => {
            logf!("external temperature: {e}");
            return None;
        }
    };
    let omap = device.omap();
    let where_id = sensor::where_id(store, &config.serial, omap.then_some(std::path::Path::new(&config.settings)));
    sensor::ensure_buckets(store, &config.serial, &sensor.bucket, where_id.as_deref());
    if omap {
        // Its Sensors view lists the sensor under the room, so it follows a move.
        sensor::keep_location(store, &config.serial, &sensor.bucket);
    } else {
        sensor::keep_selected(store, &config.serial, &sensor);
    }
    let (known, shared) = (store.clone(), format!("shared.{}", config.serial));
    sensor.introduce_with(move || known.get(&shared)?.value.get("current_temperature")?.as_f64());
    #[cfg(target_os = "linux")]
    {
        let result = match &device.ble {
            Some(device::Ble::Da14580(chip)) => sensor::port::start(Some(sensor.clone()), chip, config.matter),
            Some(device::Ble::Hci(link)) => sensor::hci::start(sensor.clone(), link),
            None => sensor::socket::start(sensor.clone(), store.clone(), config.serial.clone()),
        };
        if let Err(e) = result {
            logf!("external temperature: {e}");
            return None;
        }
    }
    Some(sensor)
}

async fn run(config: flags::Config) {
    logf!("nestlocal: on a {}", device::current().name);
    let store = store::Store::new(&config.state);
    structure::ensure_structure(&store, &config.serial);
    tokio::spawn(store.clone().save_changes());
    let sensor = start_sensor(&config, &store);
    #[cfg(target_os = "linux")]
    if let (true, false, Some(ble)) =
        (matter_takes_ble(&config), config.external_temperature, device::current().da14580())
    {
        if let Err(e) = sensor::port::start(None, ble, true) {
            logf!("matter BLE: stand-in: {e}");
        }
    }
    // nestlocal.init holds nlclient back until here; its first commit takes
    // the screen timeout and brightness.
    if std::fs::create_dir_all(settings_ui::DIR).is_ok() {
        screen::republish(&store, &config.serial);
        brightness::republish(&store, &config.serial);
        let _ = std::fs::write(std::path::Path::new(settings_ui::DIR).join(NLCLIENT_MAY_START), b"");
    }
    // Before anything looks a name up: until it has read connman's services,
    // network::up() does not wait.
    network::start().await;
    if !config.ntp_server.is_empty() {
        tokio::spawn(clock::keep_clock(config.ntp_server.clone()));
    }
    let outdoor = Arc::new(weather::Outdoor::default());
    let live = liveness::Liveness::new(config.stale);
    tokio::spawn(live.clone().run());

    let wwn_boiler = heatlink::using(std::path::Path::new(&config.settings));
    if wwn_boiler {
        logf!("nestlocal: nlclient runs with {}: it heats while it reports can_heat false", heatlink::KEY);
    }
    let integrated = !config.mqtt.is_empty() || config.matter || config.zigbee;
    // Schedule control runs on every unit; the rest serves an integration.
    let bridge = Some({
        // <serial> in either name stands for the unit's serial, lowercase in entity IDs.
        let root = Some(config.ha_entity_root.replace("<serial>", &config.serial.to_lowercase()))
            .filter(|root| !root.is_empty());
        if let Some(root) = root.as_ref().filter(|root| !naming::valid_root(root)) {
            logf!("mqtt: -ha-entity-root {root:?}: not an entity ID root (lowercase letters, digits and _); keeping the last");
        }
        let root = root.filter(|root| naming::valid_root(root));
        let names = naming::Naming::load(
            &config.ha_store,
            &config.serial,
            naming::read_device_name(&config.device_name_file).map(|name| name.replace("<serial>", &config.serial)),
            root,
        );
        bridge::Bridge::new(
            &config.topic,
            &config.discovery,
            &config.serial,
            store.clone(),
            outdoor.clone(),
            Some(live.clone()),
            wwn_boiler,
            names,
        )
    });
    let integration = bridge.clone().filter(|_| integrated);
    if let (Some(bridge), Some(sensor)) = (&integration, &sensor) {
        bridge.attach_sensor(sensor.clone());
    }
    // Before MQTT starts: its first announcement carries the update entity.
    if let Some(bridge) = &integration {
        let updates =
            update::Updates::new(device::current().name, &config.releases, std::path::Path::new(&config.update_store));
        bridge.attach_updates(updates.clone());
        tokio::spawn(updates.keep_checking());
    }
    if let Some(bridge) = &integration {
        occupancy::watch(bridge.occupancy.clone(), occupancy::EVENT_LOG.into());
        brightness::watch(bridge.brightness.clone());
        power::watch(bridge.power.clone());
        bridge.wake_for_boost();
    }
    let mut broker_port = None;
    if let (false, Some(bridge)) = (config.mqtt.is_empty(), &bridge) {
        let password = match config.mqtt_password_file.as_str() {
            "" => String::new(),
            file => std::fs::read_to_string(file)
                .unwrap_or_else(|e| fatal!("{file}: {e}"))
                .trim_end_matches(['\r', '\n'])
                .to_string(),
        };
        let (broker, tls) =
            mqtttls::broker(&config.mqtt, config.mqtt_tls, &config.mqtt_ca).unwrap_or_else(|e| fatal!("{e}"));
        bridge.run(&broker, &config.mqtt_user, &password, tls);
        broker_port = Some(broker.port);
    }
    if config.zigbee {
        match zigbee::wakeup::arm() {
            Ok(()) => logf!("zigbee: the radio wakes the unit"),
            Err(e) => logf!("zigbee: {e}: while the unit sleeps, Zigbee messages wait for it to wake"),
        }
    }
    if !config.wake_iface.is_empty() {
        let bridge = bridge.clone().filter(|_| broker_port.is_some());
        let holding = bridge.clone();
        wake::start(
            broker_port,
            config.matter,
            config.zigbee,
            config.wake_iface.clone(),
            move || {
                if let Some(bridge) = &bridge {
                    bridge.woke()
                }
            },
            move || holding.as_ref().is_some_and(|bridge| bridge.holds_awake()),
        );
    }
    if let Err(e) = settings_ui::init(config.matter, config.zigbee, config.doom, config.themes) {
        logf!("settings UI: {e}");
    }
    if let Some(bridge) = &bridge {
        bridge.start_schedule_control();
    }
    if matches!(device::current().name, "diamond3" | "onyx") {
        scratch::check();
    }
    if config.doom {
        doom::start();
    }
    if config.themes {
        let themes = theme::start(store.clone(), config.serial.clone(), outdoor.clone());
        if let Some(bridge) = &integration {
            bridge.attach_themes(themes);
        }
    }
    if let (true, Some(bridge)) = (config.zigbee, &bridge) {
        zigbee::start(bridge.clone(), config.zigbee_store.clone().into());
    }
    if let (true, Some(bridge)) = (config.matter, bridge) {
        let settings = matter::Settings {
            passcode: config.matter_passcode,
            discriminator: config.matter_discriminator,
            storage: config.matter_store.clone().into(),
            thread: config.matter_thread,
        };
        matter::start(settings, bridge);
    }

    let tls = server_tls(&config.cert, &config.key).unwrap_or_else(|e| fatal!("{e}"));
    let cloud = Arc::new(cloud::Cloud {
        store,
        outdoor,
        live: Some(live),
        origin: config.origin,
        verbose: config.verbose,
        hold: config.hold,
        display_replay: std::sync::Mutex::new(Some(format!("device.{}", config.serial))),
    });
    let stopped = cloud.serve(&config.listen, tls).await;
    fatal!("{stopped}");
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_panic_on_any_thread_ends_the_process() {
        if std::env::var_os("NESTLOCAL_PANIC_CHILD").is_some() {
            super::exit_on_panic();
            let _ = std::thread::Builder::new().name("zigbee".into()).spawn(|| panic!("boom")).unwrap().join();
            std::thread::sleep(std::time::Duration::from_secs(10));
            std::process::exit(0);
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::a_panic_on_any_thread_ends_the_process", "--test-threads=1"])
            .env("NESTLOCAL_PANIC_CHILD", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(101));
    }
}
