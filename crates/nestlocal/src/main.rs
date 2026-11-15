//! nestlocal stands in for Nest's service on the thermostat itself and
//! bridges it to Home Assistant over MQTT.

// matter/'s handler chain is a deeply nested type.
#![recursion_limit = "256"]

mod bridge;
mod brightness;
mod clock;
mod cloud;
mod commands;
use nestlocal::device;
mod doom;
mod entropy;
mod theme;
mod fetch;
mod flags;
mod heatlink;
mod json;
mod liveness;
mod logger;
mod matter;
mod mqtt;
mod mqtttls;
mod naming;
mod nlsettings;
mod occupancy;
mod private_file;
mod sensor;
mod handover;
mod scratch;
mod screen;
mod store;
mod settings_ui;
mod structure;
mod thread_sleep;
mod update;
mod wifi_update;
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
    dir.flatten().filter_map(|entry| {
        let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
        let comm = std::fs::read_to_string(entry.path().join("comm")).ok()?;
        (comm.trim_end() == name).then_some(pid)
    }).collect()
}

fn server_tls(cert: &str, key: &str) -> Result<Arc<rustls::ServerConfig>, String> {
    let certs = CertificateDer::pem_file_iter(cert).map_err(|e| format!("{cert}: {e}"))?
        .collect::<Result<Vec<_>, _>>().map_err(|e| format!("{cert}: {e}"))?;
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("{key}: {e}"))?;
    let config = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(certs, key)
        .map_err(|e| format!("{cert}: {e}"))?;
    Ok(Arc::new(config))
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
    if config.check_options {
        return;
    }
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
    // Without Matter, which lists its own patterns, nlsleepd's wake list
    // holds the update API's port once run() knows the Wi-Fi interface's
    // address, or nothing.
    if !config.matter {
        let _ = std::fs::remove_file(matter::wake_patterns::LIST);
    }
    // Without the update API, no laptop stays trusted to update the unit.
    if config.update_listen.is_empty() {
        wifi_update::trust::revoke(std::path::Path::new("/"));
    }

    #[cfg(target_os = "linux")]
    if let (true, Some(ble)) = (config.external_temperature || matter_takes_ble(&config), device::current().ble.as_ref()) {
        if let Err(e) = sensor::port::start_holder(ble) {
            logf!("BLE stand-in: {e}");
        }
    }
    entropy::seed();
    install_crypto();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()
        .unwrap_or_else(|e| fatal!("{e}"));
    runtime.block_on(run(config));
}

/// Whether nestlocal lists the update API's port in nlsleepd's wake list:
/// with the update API and an interface to wake on, and without Matter,
/// whose port an app knocks on instead.
fn lists_update_port(config: &flags::Config) -> bool {
    !config.matter && !config.update_listen.is_empty() && !config.wake_iface.is_empty()
}

/// Keeps the update API's port in nlsleepd's wake list at the Wi-Fi
/// interface's address: every 5 s until it has one, then every 30 s, as
/// Matter keeps its own.
fn list_update_port(interface: String) {
    tokio::spawn(async move {
        let mut list = matter::wake_patterns::UpdatePort::new(matter::wake_patterns::LIST);
        loop {
            let listed = list.refresh(&interface, matter::mac(&interface));
            tokio::time::sleep(std::time::Duration::from_secs(if listed { 30 } else { 5 })).await;
        }
    });
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
        if let Err(e) = sensor::prepare_settings(identity, std::path::Path::new(path), &config.serial, where_id.as_deref()) {
            logf!("external temperature: {e}");
        }
    }
}

/// The stand-in sensor over gen 3 BLE or the gen 1's and gen 2's loopback RCS input, or gone
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
    let sensor = sensor::Sensor::open(identity, &config.serial, std::path::Path::new(&config.settings), config.external_temperature_timeout);
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
        let result = match device.ble.as_ref() {
            Some(ble) => sensor::port::start(Some(sensor.clone()), ble, config.matter),
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
    if let (true, false, Some(ble)) = (matter_takes_ble(&config), config.external_temperature, device::current().ble.as_ref()) {
        if let Err(e) = sensor::port::start(None, ble, true) {
            logf!("matter BLE: stand-in: {e}");
        }
    }
    // nestlocal.init holds nlclient back until here; its first commit takes
    // the screen timeout.
    if std::fs::create_dir_all(settings_ui::DIR).is_ok() {
        screen::republish(&store, &config.serial);
        brightness::republish(&store, &config.serial);
        let _ = std::fs::write(std::path::Path::new(settings_ui::DIR).join(NLCLIENT_MAY_START), b"");
    }
    handover::clear_stale(settings_ui::dir());
    if lists_update_port(&config) {
        list_update_port(config.wake_iface.clone());
    }
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
    let bridge = (!config.mqtt.is_empty() || config.matter || config.zigbee).then(|| {
        // <serial> in either name stands for the unit's serial, lowercase in entity IDs.
        let root = Some(config.ha_entity_root.replace("<serial>", &config.serial.to_lowercase()))
            .filter(|root| !root.is_empty());
        if let Some(root) = root.as_ref().filter(|root| !naming::valid_root(root)) {
            logf!("mqtt: -ha-entity-root {root:?}: not an entity ID root (lowercase letters, digits and _); keeping the last");
        }
        let root = root.filter(|root| naming::valid_root(root));
        let names = naming::Naming::load(&config.ha_store, &config.serial,
                                         naming::read_device_name(&config.device_name_file)
                                             .map(|name| name.replace("<serial>", &config.serial)), root);
        bridge::Bridge::new(&config.topic, &config.discovery, &config.serial, store.clone(), outdoor.clone(),
                            Some(live.clone()), wwn_boiler, names)
    });
    if let (Some(bridge), Some(sensor)) = (&bridge, &sensor) {
        bridge.attach_sensor(sensor.clone());
    }
    // Before MQTT starts: its first announcement carries the update entity.
    if let Some(bridge) = &bridge {
        let updates = update::Updates::new(device::current().name, &config.releases, std::path::Path::new(&config.update_store));
        bridge.attach_updates(updates.clone());
        tokio::spawn(updates.keep_checking());
    }
    if let Some(bridge) = &bridge {
        occupancy::watch(bridge.occupancy.clone(), occupancy::EVENT_LOG.into());
        brightness::watch(bridge.brightness.clone());
    }
    let mut broker_port = None;
    if let (false, Some(bridge)) = (config.mqtt.is_empty(), &bridge) {
        let password = match config.mqtt_password_file.as_str() {
            "" => String::new(),
            file => std::fs::read_to_string(file).unwrap_or_else(|e| fatal!("{file}: {e}"))
                .trim_end_matches(['\r', '\n']).to_string(),
        };
        let (broker, tls) = mqtttls::broker(&config.mqtt, config.mqtt_tls, &config.mqtt_ca).unwrap_or_else(|e| fatal!("{e}"));
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
        wake::start(broker_port, config.matter, config.zigbee,
                    config.wake_iface.clone(), move || if let Some(bridge) = &bridge { bridge.woke() });
    }
    let update = !config.update_listen.is_empty();
    if let Err(e) = settings_ui::init(config.matter, config.zigbee, config.doom, config.themes, update) { logf!("settings UI: {e}"); }
    if device::current().name == "diamond3" {
        scratch::check();
    }
    // nlclient commits the device bucket sent it again, which hands its
    // screen over and runs its downlink_activity (taking any pending
    // display.request). Being newer, the resend can undo a device-bucket
    // change nlclient has not uploaded yet (Eco, the fan timer, a hot water
    // boost): hence at most one a minute, and none just after it changed it.
    let nudge: wifi_update::Nudge = {
        let resender = store::Resender::new(store.clone(), format!("device.{}", config.serial));
        Arc::new(move || resender.resend(std::time::Instant::now()))
    };
    if update {
        let shared = wifi_update::Shared {
            window: Arc::new(wifi_update::window::Window::new()),
            display: Arc::new(wifi_update::display::start(settings_ui::dir().into())),
            prompt: Arc::new(wifi_update::prompt::Prompt::remembering(wifi_update::prompt::STRIKES_FILE.into())),
            trusted: Arc::new(std::sync::Mutex::new(wifi_update::trust::Store::open(std::path::Path::new(wifi_update::trust::FILE)))),
            nudge: Some(nudge.clone()),
        };
        wifi_update::watch_requests(shared.clone(), config.wake_iface.clone());
        let (listen, restart): (String, wifi_update::Restart) = (config.update_listen.clone(), Arc::new(wifi_update::restart));
        tokio::spawn(async move {
            let stopped = wifi_update::serve(listen, shared, wifi_update::bundle::Paths::unit(), restart).await;
            logf!("wifi-update: {stopped}");
        });
    }
    wifi_update::watch_trial(live.clone(), wifi_update::bundle::Paths::unit(), config.wake_iface.clone(), Some(nudge));
    if config.doom {
        doom::start();
    }
    if config.themes {
        theme::start(store.clone(), config.serial.clone(), outdoor.clone());
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
        store, outdoor, live: Some(live), origin: config.origin, verbose: config.verbose, hold: config.hold,
    });
    let stopped = cloud.serve(&config.listen, tls).await;
    fatal!("{stopped}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(args: &str) -> flags::Config {
        flags::parse(&args.split_whitespace().map(String::from).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn matter_off_and_the_update_api_on_writes_the_update_pattern() {
        assert!(lists_update_port(&config("-mqtt tcp://broker:1883 -update-listen 0.0.0.0:8443")));
        // Matter lists its own; without the update API, or an interface to
        // wake on, there is nothing to list.
        assert!(!lists_update_port(&config("-matter -update-listen 0.0.0.0:8443")));
        assert!(!lists_update_port(&config("-mqtt tcp://broker:1883")));
        assert!(!lists_update_port(&config("-mqtt tcp://broker:1883 -update-listen 0.0.0.0:8443 -wake-iface=")));
    }
}
