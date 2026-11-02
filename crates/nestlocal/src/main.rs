//! nestlocal stands in for Nest's service on the thermostat itself and
//! bridges it to Home Assistant over MQTT.

// matter/'s handler chain is a deeply nested type.
#![recursion_limit = "256"]

mod bridge;
mod clock;
mod cloud;
mod commands;
mod device;
mod doom;
mod entropy;
mod flags;
mod heatlink;
mod json;
mod liveness;
mod logger;
mod matter;
mod mqtt;
mod mqtttls;
mod nlsettings;
mod store;
mod settings_ui;
mod structure;
mod thread_sleep;
mod wake;
mod weather;
mod zigbee;

use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

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
    if config.syslog {
        #[cfg(unix)]
        if let Err(e) = logger::to_syslog() {
            fatal!("{e}");
        }
    }
    if config.serial.is_empty() {
        config.serial = hostname().unwrap_or_else(|e| fatal!("{e}"));
    }
    if config.prepare_settings {
        prepare_settings(&config);
        return;
    }

    entropy::seed();
    install_crypto();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()
        .unwrap_or_else(|e| fatal!("{e}"));
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
    let store = store::Store::new(&config.state);
    let model = heatlink::model();
    let wanted = heatlink::wanted(config.zigbee || config.matter_thread, device::current().generation, model.as_deref(),
                                  heatlink::w1_wired(&store, &config.serial));
    match heatlink::prepare(std::path::Path::new(path), &heatlink::marker(&config.state), wanted) {
        Ok(Some(true)) => logf!("nestlocal: set {} in {path}: the {} has no Heat Link without its radio",
                                heatlink::KEY, model.unwrap_or_default()),
        Ok(Some(false)) => logf!("nestlocal: removed {} from {path}", heatlink::KEY),
        Ok(None) => {}
        Err(e) => fatal!("{e}"),
    }
}

async fn run(config: flags::Config) {
    logf!("nestlocal: on a {}", device::current().name);
    let store = store::Store::new(&config.state);
    structure::ensure_structure(&store, &config.serial);
    tokio::spawn(store.clone().save_changes());
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
        bridge::Bridge::new(&config.topic, &config.discovery, &config.serial, store.clone(), outdoor.clone(),
                            Some(live.clone()), wwn_boiler)
    });
    if let Some(bridge) = &bridge {
        let holds_radio = config.zigbee || config.matter_thread;
        if heatlink::heat_link_unit(holds_radio, device::current().generation, heatlink::model().as_deref()) {
            let unit = heatlink::Unit {
                serial: config.serial.clone(),
                settings: config.settings.clone().into(),
                marker: heatlink::marker(&config.state),
            };
            let bridge = bridge.clone();
            heatlink::keep(store.clone(), unit, move |on| bridge.set_wwn_boiler(on));
        }
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
    if let Err(e) = settings_ui::init(config.matter, config.zigbee, config.doom) { logf!("settings UI: {e}"); }
    if config.doom {
        doom::start();
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
