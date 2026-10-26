//! Matter (-matter): the thermostat served on the local network to Home
//! Assistant, or any other Matter controller, beside MQTT or instead of it.
//!
//! It reads the same buckets and runs the same commands as the MQTT side
//! (Bridge::state, Bridge::apply), so both show and do the same. The unit
//! is already on its network (joined from its own menu), so it is
//! commissioned over IP or supported Bluetooth hardware with its passcode;
//! until it has a controller, its commissioning window is kept open. It uses the Matter test vendor and its
//! test attestation: controllers accept it as an uncertified device.
//!
//! rs-matter runs on a thread of its own, on its own executor; the store's
//! watcher wakes it to report changes to subscribed controllers.

mod extras;
mod ble;
mod pairing;
mod srp;
mod thermostat;
mod thread;

use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use embassy_futures::select::{select3, select4};
use rs_matter::crypto::{default_crypto, Crypto};
use rs_matter::dm::clusters::decl::thermostat as thermostat_cluster;
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter::dm::clusters::identify::{self, IdentifyHandler};
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_PID, TEST_VID};
use rs_matter::dm::clusters::decl::{
    fan_control, fixed_label, mode_select, on_off, power_source, relative_humidity_measurement,
    temperature_measurement, thermostat_user_interface_configuration as ui_config,
};
// Every cluster's handler trait, for their CLUSTER constants.
use rs_matter::dm::clusters::decl::fan_control::ClusterHandler as _;
use rs_matter::dm::clusters::decl::fixed_label::ClusterHandler as _;
use rs_matter::dm::clusters::decl::mode_select::ClusterHandler as _;
use rs_matter::dm::clusters::decl::on_off::ClusterHandler as _;
use rs_matter::dm::clusters::decl::power_source::ClusterHandler as _;
use rs_matter::dm::clusters::decl::relative_humidity_measurement::ClusterHandler as _;
use rs_matter::dm::clusters::decl::temperature_measurement::ClusterHandler as _;
use rs_matter::dm::clusters::decl::thermostat::ClusterHandler as _;
use rs_matter::dm::clusters::decl::thermostat_user_interface_configuration::ClusterHandler as _;
use rs_matter::dm::devices::{DEV_TYPE_FAN, DEV_TYPE_MODE_SELECT, DEV_TYPE_POWER_SOURCE, DEV_TYPE_THERMOSTAT};
use rs_matter::dm::endpoints;
use rs_matter::dm::networks::eth::EthNetwork;
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::{Async, DataModel, Dataver, DeviceType, Endpoint, Node};
use rs_matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter::error::{Error, ErrorCode};
use rs_matter::dm::AttrChangeNotifier;
use rs_matter::dm::HandlerContext;
use rs_matter::im::{InteractionModelState, InteractionModel};
use rs_matter::dm::clusters::{net_comm::{Networks, NetworksAccess, NetCtl, NetCtlStatus, NetworkType}, thread_diag::ThreadDiag};
use rs_matter::dm::networks::{NetChangeNotif, wireless::NoopWirelessNetCtl};
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::persist::DirKvBlobStore;
use rs_matter::respond::DefaultResponder;
use rs_matter::sc::pase::{
    CommWindowState, Spake2pVerifierPassword, Spake2pVerifierPasswordRef, MAX_COMM_WINDOW_TIMEOUT_SECS,
};
use rs_matter::transport::exchange::MatterBuffers;
use rs_matter::transport::MATTER_SOCKET_BIND_ADDR;
use rs_matter::utils::select::Coalesce;
use rs_matter::{clusters, devices, root_endpoint, BasicCommData, Matter, MATTER_PORT};
use serde_json::Value;

use crate::bridge::{self, Bridge};
use crate::json::{number, sprint, truthy, Fields};
use crate::logf;

use extras::{Backplate, Battery, Display, Fan, FanTimer, Humidity, Labels, Switch, Switches};
use thermostat::Thermostat;

/// The endpoint the thermostat is on.
pub const THERMOSTAT: u16 = 1;

pub struct Settings {
    pub passcode: u32,
    pub discriminator: u16,
    /// Where the commissioning (fabrics, keys) is kept.
    pub storage: PathBuf,
    /// Enables Thread network commissioning and SRP alongside Wi-Fi discovery.
    pub thread: bool,
}

/// A passcode Matter accepts: 1 to 99999998, none of the trivial ones.
pub fn check_passcode(passcode: u32) -> Result<(), String> {
    let trivial = [11111111, 22222222, 33333333, 44444444, 55555555, 66666666, 77777777, 88888888,
                   12345678, 87654321];
    if !(1..=99_999_998).contains(&passcode) || trivial.contains(&passcode) {
        return Err(format!("{passcode} is not a passcode Matter accepts"));
    }
    Ok(())
}

/// The unit as the cluster handlers see it.
pub struct Nest {
    bridge: Arc<Bridge>,
}

impl Nest {
    /// The buckets and the computed state, as of now.
    pub fn view(&self) -> View {
        let buckets = self.bridge.buckets();
        let dev = buckets.get("device").cloned().unwrap_or_default();
        let sh = buckets.get("shared").cloned().unwrap_or_default();
        let state = bridge::state(&dev, &sh);
        View { dev, sh, state }
    }

    /// Runs a command, as MQTT's set/<name> would.
    pub fn apply(&self, name: &str, arg: &str) -> Result<(), String> {
        self.bridge.apply("matter", name, arg)
    }

    pub fn outdoor_temperature(&self) -> Option<f64> {
        self.bridge.outdoor.temperature()
    }

    fn reported(&self) -> bool {
        let buckets = self.bridge.buckets();
        buckets.contains_key("device") && buckets.contains_key("shared")
    }
}

pub struct View {
    dev: Fields,
    sh: Fields,
    state: Fields,
}

impl View {
    /// off, heat, cool or heat_cool.
    pub fn mode(&self) -> &str {
        self.state_str("mode")
    }

    pub fn state_str(&self, key: &str) -> &str {
        self.state.get(key).and_then(Value::as_str).unwrap_or("")
    }

    pub fn state_flag(&self, key: &str) -> bool {
        truthy(self.state.get(key))
    }

    pub fn shared_flag(&self, key: &str) -> bool {
        truthy(self.sh.get(key))
    }

    /// A temperature as the unit's screen shows it (to half degrees in °C,
    /// whole degrees in °F), as MQTT gives it: the computed state's `key`.
    pub fn shown_centi(&self, key: &str) -> Option<i16> {
        number(self.state.get(key)).map(centi)
    }

    pub fn device_centi(&self, key: &str) -> Option<i16> {
        number(self.dev.get(key)).map(centi)
    }

    pub fn device_str(&self, key: &str) -> String {
        sprint(self.dev.get(key))
    }

    pub fn device_flag(&self, key: &str) -> bool {
        truthy(self.dev.get(key))
    }

    pub fn device_number(&self, key: &str) -> Option<f64> {
        number(self.dev.get(key))
    }
}

/// °C as Matter's hundredths of a degree.
pub fn centi(celsius: f64) -> i16 {
    (celsius * 100.0).round() as i16
}

/// A command the unit's buckets refused, as a Matter status.
pub fn failed(_reason: String) -> Error {
    ErrorCode::ConstraintError.into()
}

// Stay awake until the service is operational and its pairing window is closed.
static SLEEP_BLOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
static THREAD_REQUIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn sleep_blocked() -> bool {
    SLEEP_BLOCKED.load(std::sync::atomic::Ordering::SeqCst)
        || ble::sleep_blocked()
        || (THREAD_REQUIRED.load(std::sync::atomic::Ordering::SeqCst) && !crate::thread_sleep::ready())
}

/// Serves the unit over Matter, on a thread of its own, once it has
/// reported (its equipment decides what it offers).
pub fn start(settings: Settings, bridge: Arc<Bridge>) {
    THREAD_REQUIRED.store(settings.thread, std::sync::atomic::Ordering::SeqCst);
    let _ = log::set_logger(&LOG).map(|()| log::set_max_level(log::LevelFilter::Info));
    let spawned = std::thread::Builder::new().name("matter".into()).stack_size(16 << 20).spawn(move || {
        let nest = Arc::new(Nest { bridge });
        while !nest.reported() {
            std::thread::sleep(Duration::from_secs(5));
        }
        let result = run(&settings, nest);
        SLEEP_BLOCKED.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Err(e) = result {
            logf!("matter: stopped: {e:?}");
        }
    });
    if let Err(e) = spawned {
        logf!("matter: {e}");
    }
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn run(settings: &Settings, nest: Arc<Nest>) -> Result<(), Error> {
    if settings.thread {
        run_network(settings, nest, thread::ThreadNetworks::new(), thread::Controller::default())
    } else {
        run_network(settings, nest, EthNetwork::new_default(), NoopWirelessNetCtl::new(NetworkType::Ethernet))
    }
}

fn run_network<N: Networks, C: NetCtl + NetCtlStatus + ThreadDiag + NetChangeNotif>(
    settings: &Settings, nest: Arc<Nest>, networks: N, controller: C,
) -> Result<(), Error> {
    let view = nest.view();
    let info: &'static BasicInfoConfig = Box::leak(Box::new(BasicInfoConfig {
        vid: TEST_VID,
        pid: TEST_PID,
        hw_ver: 3,
        hw_ver_str: "3",
        sw_ver: 1,
        sw_ver_str: leak(view.device_str("current_version")),
        serial_no: leak(nest.bridge.serial.clone()),
        manufacturing_date: "20151201",
        product_name: "Nest Learning Thermostat",
        vendor_name: "Google Nest",
        device_name: "Nest Thermostat",
        ..BasicInfoConfig::new()
    }));
    let comm = BasicCommData {
        password: Spake2pVerifierPassword::new_from_ref(Spake2pVerifierPasswordRef::new(
            &settings.passcode.to_le_bytes(),
        )),
        discriminator: settings.discriminator,
    };
    let matter = Matter::new(info, comm.clone(), &TEST_DEV_ATT, MATTER_PORT);
    std::fs::create_dir_all(&settings.storage).map_err(|_| ErrorCode::StdIoError)?;
    #[cfg(unix)]
    if settings.thread {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&settings.storage, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| ErrorCode::StdIoError)?;
    }
    let kv = matter.kv(DirKvBlobStore::new(settings.storage.clone()));
    matter.startup(&kv)?;
    if let Err(e) = crate::settings_ui::matter_codes(&comm, DiscoveryCapabilities::IP) { logf!("matter UI: {e}"); }

    let buffers: MatterBuffers = MatterBuffers::new();
    let state: InteractionModelState<N> = InteractionModelState::new(networks);
    let crypto = default_crypto(rand::rng(), DAC_PRIVKEY);
    let rand = crypto.rand()?;
    let switches = extras::switches(&nest);
    let node = node_for_network(view.device_flag("has_fan"), &switches, settings.thread);
    let im = InteractionModel::new_with_net_ctl(&matter, &crypto, &buffers,
        data_model(node, rand, nest.clone(), switches, &controller, settings.thread), &kv, &controller, &state);
    futures_lite::future::block_on(im.startup())?;

    let responder = DefaultResponder::new(&im);
    let mut respond = pin!(responder.run::<4, 4>());
    let mut im_job = pin!(im.run());
    let socket = async_io::Async::<std::net::UdpSocket>::bind(MATTER_SOCKET_BIND_ADDR)?;
    let btp = rs_matter::transport::network::btp::Btp::new();
    let network = || rs_matter::transport::network::ChainedNetwork::new(
        |addr: &rs_matter::transport::network::Address| addr.btp().is_some(), &btp, &socket);
    let mut transport = pin!(async {
        match embassy_futures::select::select(
            matter.run(&crypto, network(), network(), &socket),
            ble::run(&matter, &comm, &btp, info.serial_no),
        ).await {
            embassy_futures::select::Either::First(result) => result,
            embassy_futures::select::Either::Second(()) => unreachable!(),
        }
    });
    let (refresh_mdns, mdns_refreshes) = async_channel::bounded::<()>(1);
    let mut mdns = pin!(async {
        if settings.thread {
            embassy_futures::select::select(srp::run(&matter, &nest.bridge.serial),
                mdns(&matter, &crypto, &nest.bridge.serial, &mdns_refreshes)).coalesce().await
        } else {
            mdns(&matter, &crypto, &nest.bridge.serial, &mdns_refreshes).await
        }
    });

    // Changes to the buckets, reported to subscribed controllers.
    let (changed, changes) = async_channel::bounded::<()>(1);
    nest.bridge.store.watch(move |_| {
        let _ = changed.try_send(());
    });
    let mut report = pin!(async {
        while changes.recv().await.is_ok() {
            im.notify_all_changed();
        }
        Ok::<(), Error>(())
    });

    // Initial enrollment is commissionable by default; paired units open on request.
    matter.print_standard_qr_text(DiscoveryCapabilities::IP)?;
    let mut replacement = pairing::Replacement::load(&settings.storage)?;
    let mut commissionable = pin!(async {
        let mut thread_cleared = false;
        let mut ble_ready = false;
        loop {
            if ble_ready != ble::ready() {
                ble_ready = ble::ready();
                let capabilities = if ble_ready { DiscoveryCapabilities::IP | DiscoveryCapabilities::BLE }
                    else { DiscoveryCapabilities::IP };
                if let Err(e) = crate::settings_ui::matter_codes(&comm, capabilities) { logf!("matter UI: {e}"); }
            }
            match replacement.finish(&matter, &kv) {
                Ok(removed) => {
                    if !removed.is_empty() {
                        im.notify_all_changed();
                        let _ = refresh_mdns.try_send(());
                    }
                    for index in removed {
                        im.notify_fabric_removed(index);
                        logf!("matter: replaced previous fabric {index}");
                    }
                }
                Err(e) => logf!("matter: pairing replacement cleanup will retry: {e:?}"),
            }
            if settings.thread {
                let configured = state.networks().access(|networks| {
                    let mut any = false;
                    networks.networks(&mut |_| { any = true; Ok(()) })?;
                    Ok::<_, Error>(any)
                })?;
                if configured { thread_cleared = false; }
                if !configured && !thread_cleared && !matter.is_icd_busy() {
                    match thread::clear_dataset().await {
                        Ok(()) => thread_cleared = true,
                        Err(_) => logf!("matter: cannot clear unconfigured Thread network; retrying"),
                    }
                }
            }
            let requested = crate::settings_ui::take_request("matter");
            if requested {
                SLEEP_BLOCKED.store(true, std::sync::atomic::Ordering::SeqCst);
                replacement.begin(&matter, &kv)?;
            }
            if requested { matter.close_comm_window(&im)?; }
            if (requested || !matter.has_fabrics()) && matches!(matter.comm_window_state(), CommWindowState::Closed) {
                matter.open_basic_comm_window(MAX_COMM_WINDOW_TIMEOUT_SECS, &crypto, &im)?;
                logf!("matter: open for commissioning (passcode {}, discriminator {})", settings.passcode,
                      settings.discriminator);
            }
            SLEEP_BLOCKED.store(!matches!(matter.comm_window_state(), CommWindowState::Closed),
                                std::sync::atomic::Ordering::SeqCst);
            let paired = pairing::has_committed_fabrics(&matter, &kv)?;
            crate::settings_ui::status("matter", if matches!(matter.comm_window_state(), CommWindowState::Closed) {
                if paired { "Paired. Enable pairing to replace the controller." } else { "Ready to pair." }
            } else if paired {
                "Pairing open. Already paired; scan to replace the controller."
            } else { "Pairing open. Scan in your Matter app." });
            async_io::Timer::after(Duration::from_secs(1)).await;
        }
        #[allow(unreachable_code)]
        Ok::<(), Error>(())
    });

    logf!("matter: serving on port {MATTER_PORT}{}", if matter.has_fabrics() { ", commissioned" } else { "" });
    let all = select4(
        &mut transport,
        &mut mdns,
        select3(&mut respond, &mut im_job, &mut report).coalesce(),
        &mut commissionable,
    );
    futures_lite::future::block_on(all.coalesce())
}

// Device types rs-matter does not name.
const DEV_TYPE_TEMPERATURE_SENSOR: DeviceType = DeviceType { dtype: 0x0302, drev: 2 };
const DEV_TYPE_ON_OFF_PLUG_IN_UNIT: DeviceType = DeviceType { dtype: 0x010A, drev: 3 };

/// The root endpoint, for a node already on its network.
const ROOT: Endpoint<'static> = root_endpoint!(eth);
const THREAD_ROOT: Endpoint<'static> = Endpoint::new(
    endpoints::ROOT_ENDPOINT_ID,
    devices!(rs_matter::dm::devices::DEV_TYPE_ROOT_NODE),
    clusters!(sys;
        NetworkType::Thread.cluster(),
        rs_matter::dm::clusters::thread_diag::FULL_CLUSTER
            .with_features(rs_matter::dm::clusters::thread_diag::Feature::MAC_COUNTS.bits())
            .with_attrs(|attr, _, _| !attr.quality.contains(rs_matter::dm::Quality::OPTIONAL)
                || (22..=55).contains(&attr.id))
            .with_cmds(rs_matter::with!()),
    ),
);

/// The node: the thermostat, and the endpoints beside it that this unit's
/// equipment calls for (a fan, its switches).
#[cfg(test)]
fn node(has_fan: bool, switches: &[Switch]) -> Node<'static> {
    node_for_network(has_fan, switches, false)
}

fn node_for_network(has_fan: bool, switches: &[Switch], thread: bool) -> Node<'static> {
    use extras::{BACKPLATE, BATTERY, FAN, FAN_TIMER};
    let mut endpoints = vec![
        if thread { THREAD_ROOT } else { ROOT },
        Endpoint::new(
            THERMOSTAT,
            devices!(DEV_TYPE_THERMOSTAT),
            clusters!(desc::DescHandler::CLUSTER, identify::CLUSTER, Thermostat::CLUSTER, Humidity::CLUSTER,
                      Display::CLUSTER),
        ),
        Endpoint::new(
            BACKPLATE,
            devices!(DEV_TYPE_TEMPERATURE_SENSOR),
            clusters!(desc::DescHandler::CLUSTER, identify::CLUSTER, Backplate::CLUSTER, Labels::CLUSTER),
        ),
        Endpoint::new(
            BATTERY,
            devices!(DEV_TYPE_POWER_SOURCE),
            clusters!(desc::DescHandler::CLUSTER, Battery::CLUSTER, Labels::CLUSTER),
        ),
    ];
    if has_fan {
        endpoints.push(Endpoint::new(
            FAN,
            devices!(DEV_TYPE_FAN),
            clusters!(desc::DescHandler::CLUSTER, identify::CLUSTER, Fan::CLUSTER, Labels::CLUSTER),
        ));
        endpoints.push(Endpoint::new(
            FAN_TIMER,
            devices!(DEV_TYPE_MODE_SELECT),
            clusters!(desc::DescHandler::CLUSTER, FanTimer::CLUSTER, Labels::CLUSTER),
        ));
    }
    for switch in switches {
        endpoints.push(Endpoint::new(
            switch.endpoint,
            devices!(DEV_TYPE_ON_OFF_PLUG_IN_UNIT),
            clusters!(desc::DescHandler::CLUSTER, identify::CLUSTER, Switches::CLUSTER, Labels::CLUSTER),
        ));
    }
    Node { endpoints: Box::leak(endpoints.into_boxed_slice()) }
}

/// The cluster handlers, by cluster; each serves every endpoint it is on.
fn data_model<'a, C: NetCtl + NetCtlStatus + ThreadDiag>(node: Node<'static>, mut rand: impl rand::Rng + Copy + 'a, nest: Arc<Nest>, switches: Vec<Switch>, controller: &'a C, thread: bool)
              -> impl DataModel + 'a {
    use extras::{BACKPLATE, BATTERY, FAN, FAN_TIMER};
    let mut names = vec![(BACKPLATE, "Backplate temperature"), (BATTERY, "Battery"), (FAN, "Fan"),
                         (FAN_TIMER, "Fan timer duration")];
    names.extend(switches.iter().map(|s| (s.endpoint, s.name)));
    (
        node,
        endpoints::EthSysHandlerBuilder::new()
            .netif_diag(&SysNetifs)
            .build(rand)
            .chain(if thread { |e, _| e == 0 } else { |_, _| false },
                endpoints::ThreadSysHandlerBuilder::new(controller, controller).netif_diag(&SysNetifs).build(rand))
            .chain(
                |e, c| e != 0 && c == desc::DescHandler::CLUSTER.id,
                Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                |e, c| e != 0 && c == identify::CLUSTER.id,
                Async(IdentifyHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                |e, c| e == THERMOSTAT && c == Thermostat::CLUSTER.id,
                Async(thermostat_cluster::HandlerAdaptor(Thermostat::new(Dataver::new_rand(&mut rand), nest.clone()))),
            )
            .chain(
                |e, c| e == THERMOSTAT && c == Humidity::CLUSTER.id,
                Async(relative_humidity_measurement::HandlerAdaptor(Humidity {
                    dataver: Dataver::new_rand(&mut rand), nest: nest.clone() })),
            )
            .chain(
                |e, c| e == THERMOSTAT && c == Display::CLUSTER.id,
                Async(ui_config::HandlerAdaptor(Display { dataver: Dataver::new_rand(&mut rand), nest: nest.clone() })),
            )
            .chain(
                |e, c| e == BACKPLATE && c == Backplate::CLUSTER.id,
                Async(temperature_measurement::HandlerAdaptor(Backplate {
                    dataver: Dataver::new_rand(&mut rand), nest: nest.clone() })),
            )
            .chain(
                |e, c| e == BATTERY && c == Battery::CLUSTER.id,
                Async(power_source::HandlerAdaptor(Battery { dataver: Dataver::new_rand(&mut rand), nest: nest.clone() })),
            )
            .chain(
                |e, c| e == FAN && c == Fan::CLUSTER.id,
                Async(fan_control::HandlerAdaptor(Fan { dataver: Dataver::new_rand(&mut rand), nest: nest.clone() })),
            )
            .chain(
                |e, c| e == FAN_TIMER && c == FanTimer::CLUSTER.id,
                Async(mode_select::HandlerAdaptor(FanTimer { dataver: Dataver::new_rand(&mut rand), nest: nest.clone() })),
            )
            .chain(
                |e, c| e >= extras::SWITCHES && c == Switches::CLUSTER.id,
                Async(on_off::HandlerAdaptor(Switches { dataver: Dataver::new_rand(&mut rand), nest, all: switches })),
            )
            .chain(
                |e, c| e > THERMOSTAT && c == Labels::CLUSTER.id,
                Async(fixed_label::HandlerAdaptor(Labels { dataver: Dataver::new_rand(&mut rand), names })),
            ),
    )
}

/// Announces the unit on the network (DNS-SD over mDNS) from its interface
/// with an IPv6 link-local address: commissionable, then operational. Again
/// whenever its addresses change (the unit joins another network, the
/// router renumbers), or controllers would keep calling the old ones.
async fn mdns<C: Crypto + Copy>(matter: &Matter<'_>, crypto: C, serial: &str,
                              refreshes: &async_channel::Receiver<()>) -> Result<(), Error> {
    use embassy_futures::select::{select3, Either3};
    use rs_matter::transport::network::mdns::builtin::{BuiltinMdns, Host};
    use rs_matter::transport::network::mdns::{
        MDNS_IPV4_BROADCAST_ADDR, MDNS_IPV6_BROADCAST_ADDR, MDNS_SOCKET_DEFAULT_BIND_ADDR,
    };
    use rs_matter::transport::network::{Ipv4Addr, Ipv6Addr};
    use socket2::{Domain, Protocol, Socket, Type};

    let hostname = leak(serial.to_string());
    let mut waited = false;
    loop {
        // The network may come up after nestlocal does.
        let Some(on) = addresses() else {
            if !waited {
                logf!("matter: waiting for an interface with an IPv6 link-local address");
                waited = true;
            }
            async_io::Timer::after(Duration::from_secs(5)).await;
            continue;
        };
        logf!("matter: announcing on {} ({}, {} IPv6 addresses)", on.name, on.ipv4, on.ipv6.len());

        let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        socket.set_only_v6(false)?;
        socket.bind(&MDNS_SOCKET_DEFAULT_BIND_ADDR.into())?;
        let socket = async_io::Async::<std::net::UdpSocket>::new_nonblocking(socket.into())?;
        socket.get_ref().join_multicast_v6(&MDNS_IPV6_BROADCAST_ADDR, on.index)?;
        if !on.ipv4.is_unspecified() {
            socket.get_ref().join_multicast_v4(&MDNS_IPV4_BROADCAST_ADDR, &on.ipv4)?;
        }
        let ipv4: Ipv4Addr = on.ipv4.octets().into();
        let ipv6: Vec<Ipv6Addr> = on.ipv6.iter().map(|a| a.octets().into()).collect();
        let host = Host { hostname, ip: ipv4, ipv6: &ipv6 };
        let mut responder = BuiltinMdns::new();
        let announce = responder
            .run(&socket, &socket, &host, (!on.ipv4.is_unspecified()).then_some(ipv4), Some(on.index), matter, crypto);
        let changed = async {
            loop {
                async_io::Timer::after(Duration::from_secs(30)).await;
                if addresses().as_ref() != Some(&on) {
                    break;
                }
            }
        };
        match select3(announce, changed, refreshes.recv()).await {
            Either3::First(result) => return result,
            Either3::Second(()) => logf!("matter: the unit's addresses changed"),
            Either3::Third(_) => logf!("matter: refreshing announcements after pairing replacement"),
        }
    }
}

/// Where the unit is on its network.
#[derive(Debug, PartialEq)]
struct Addresses {
    name: String,
    index: u32,
    ipv4: std::net::Ipv4Addr,
    ipv6: Vec<std::net::Ipv6Addr>,
}

/// The first interface with an IPv6 link-local address, and its addresses.
fn addresses() -> Option<Addresses> {
    let (name, index, ipv6) = link_local()?;
    let ipv4 = if_addrs::get_if_addrs().unwrap_or_default().into_iter()
        .filter(|i| i.name == name)
        .find_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) => Some(v4.ip),
            _ => None,
        })
        .unwrap_or(std::net::Ipv4Addr::UNSPECIFIED);
    Some(Addresses { name, index, ipv4, ipv6 })
}

/// The first interface with an IPv6 link-local address: its name, index and
/// lasting IPv6 addresses (not the temporary, deprecated or still tentative
/// ones). From the kernel's own list: if_addrs leaves link-local addresses
/// out, and on a home network they may be the only ones there are.
fn link_local() -> Option<(String, u32, Vec<std::net::Ipv6Addr>)> {
    const PASSING: u32 = 0x01 | 0x20 | 0x40; // IFA_F_TEMPORARY, _DEPRECATED, _TENTATIVE
    let table = std::fs::read_to_string("/proc/net/if_inet6").ok()?;
    // address, index, prefix length, scope, flags, name (all hex but the name)
    let rows: Vec<(std::net::Ipv6Addr, u32, u32, &str)> = table.lines().filter_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        let address = u128::from_str_radix(f.first()?, 16).ok()?;
        let index = u32::from_str_radix(f.get(1)?, 16).ok()?;
        let scope = u32::from_str_radix(f.get(3)?, 16).ok()?;
        let flags = u32::from_str_radix(f.get(4)?, 16).ok()?;
        let name = *f.get(5)?;
        (flags & PASSING == 0).then_some((std::net::Ipv6Addr::from(address), index, scope, name))
    }).collect();
    let &(_, index, _, name) = rows.iter().find(|(_, _, scope, name)| *scope == 0x20 && *name != "lo")?;
    let addresses = rows.iter().filter(|r| r.3 == name).map(|r| r.0).collect();
    Some((name.to_string(), index, addresses))
}

/// rs-matter's own log lines into nestlocal's log: warnings, and its pairing
/// code and QR text (logged at info by the crate root). Not the interaction
/// model's per-request errors: a controller reading a cluster the unit does
/// not have is routine, and logs one line per attribute.
struct Log;

static LOG: Log = Log;

impl log::Log for Log {
    fn enabled(&self, m: &log::Metadata) -> bool {
        (m.level() <= log::Level::Warn && !m.target().starts_with("rs_matter::im"))
            || (m.level() == log::Level::Info && m.target() == "rs_matter")
    }

    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            logf!("matter: {}", r.args());
        }
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::Fields;
    use rs_matter::dm::{AttrDetails, CmdDetails, EmptyHandler, EventId, EventNumber, Metadata, MatchContext,
                        OperationContext, OwnAttrChangeNotifier, OwnEventEmitter, ReadContext, WriteContext,
                        InvokeContext, AttrId, ClusterId, EndptId};
    use rs_matter::im::{DeviceLoad, EventPriority, IMBuffer, ImStats};
    use rs_matter::im::events::EventTLVWrite;
    use rs_matter::tlv::TLVElement;
    use rs_matter::transport::exchange::Exchange;
    use rs_matter::utils::storage::pooled::Buffers;

    /// The unit with these buckets, as the cluster handlers see it.
    pub(super) fn nest(dev: Fields, sh: Fields) -> Arc<Nest> {
        let bridge = crate::bridge::tests::test_bridge();
        bridge.store.merge(&format!("device.{}", bridge.serial), &dev, 0, 0, true);
        bridge.store.merge(&format!("shared.{}", bridge.serial), &sh, 0, 0, true);
        Arc::new(Nest { bridge })
    }

    /// A bucket's field, as a command left it.
    pub(super) fn field(nest: &Nest, bucket: &str, key: &str) -> Value {
        nest.bridge.buckets().get(bucket).and_then(|b| b.get(key)).cloned().unwrap_or(Value::Null)
    }

    /// The interaction model's read, write and invoke contexts, for the one
    /// thing the handlers take from them: the endpoint of the operation.
    pub(super) struct Ctx {
        attr: AttrDetails,
        cmd: CmdDetails,
    }

    pub(super) fn ctx(endpoint: u16) -> Ctx {
        Ctx {
            attr: AttrDetails {
                endpoint_id: endpoint, cluster_id: 0, attr_id: 0, list_index: None, list_chunked: false, fab_idx: 1,
                fab_filter: false, dataver: None, wildcard: false, array: false, cluster_status: Default::default(),
            },
            cmd: CmdDetails::new(endpoint, 0, 0, 1, false, None),
        }
    }

    struct NoStats;

    impl ImStats for NoStats {
        fn device_load(&self, _: Option<std::num::NonZeroU8>) -> DeviceLoad { unreachable!() }
        fn has_subscription_for(&self, _: std::num::NonZeroU8, _: u64) -> bool { unreachable!() }
        async fn wait_report_failed(&self) { unreachable!() }
    }

    struct NoBuffers;

    impl Buffers<IMBuffer> for NoBuffers {
        type Buffer<'a> = &'a mut IMBuffer;
        async fn get(&self) -> Option<Self::Buffer<'_>> { unreachable!() }
        fn get_immediate(&self) -> Option<Self::Buffer<'_>> { unreachable!() }
    }

    #[allow(unreachable_code)]
    impl HandlerContext for Ctx {
        fn matter(&self) -> &Matter<'_> { unreachable!() }
        fn crypto(&self) -> impl Crypto + '_ { unreachable!() as rs_matter::crypto::backend::dummy::DummyCrypto }
        fn kv(&self) -> impl rs_matter::persist::KvBlobStoreAccess + '_ {
            unreachable!() as &rs_matter::persist::MatterKvBlobStoreAccess<'static, DirKvBlobStore>
        }
        fn networks(&self) -> impl rs_matter::dm::clusters::net_comm::NetworksAccess + '_ {
            rs_matter::dm::clusters::net_comm::DummyNetworkAccess
        }
        fn metadata(&self) -> impl Metadata + '_ { unreachable!() as Node<'static> }
        fn handler(&self) -> impl rs_matter::dm::AsyncHandler + '_ { EmptyHandler }
        fn buffers(&self) -> impl Buffers<IMBuffer> + '_ { NoBuffers }
        fn im_stats(&self) -> impl ImStats + '_ { NoStats }
        fn notify_fabric_removed(&self, _: std::num::NonZeroU8) {}
    }

    impl AttrChangeNotifier for Ctx {
        fn notify_attr_changed(&self, _: EndptId, _: ClusterId, _: AttrId) {}
        fn notify_cluster_changed(&self, _: EndptId, _: ClusterId) {}
        fn notify_endpoint_changed(&self, _: EndptId) {}
        fn notify_all_changed(&self) {}
    }

    impl OwnAttrChangeNotifier for Ctx {
        fn notify_own_attr_changed(&self, _: AttrId) {}
        fn notify_own_cluster_changed(&self) {}
        fn notify_own_endpoint_changed(&self) {}
    }

    impl rs_matter::dm::EventEmitter for Ctx {
        fn emit_event<F>(&self, _: EndptId, _: ClusterId, _: EventId, _: EventPriority, _: F)
            -> Result<EventNumber, Error>
        where F: FnOnce(EventTLVWrite<'_>) -> Result<(), Error> { unreachable!() }
    }

    impl OwnEventEmitter for Ctx {
        fn emit_own_event<F>(&self, _: EventId, _: EventPriority, _: F) -> Result<EventNumber, Error>
        where F: FnOnce(EventTLVWrite<'_>) -> Result<(), Error> { unreachable!() }
    }

    impl MatchContext for Ctx {
        fn endpt(&self) -> Option<EndptId> { Some(self.attr.endpoint_id) }
        fn cluster(&self) -> Option<ClusterId> { None }
    }

    impl OperationContext for Ctx {
        fn exchange(&self) -> &Exchange<'_> { unreachable!() }
    }

    impl ReadContext for Ctx {
        fn attr(&self) -> &AttrDetails { &self.attr }
    }

    impl WriteContext for Ctx {
        fn attr(&self) -> &AttrDetails { &self.attr }
        fn data(&self) -> &TLVElement<'_> { unreachable!() }
    }

    impl InvokeContext for Ctx {
        fn cmd(&self) -> &CmdDetails { &self.cmd }
        fn data(&self) -> &TLVElement<'_> { unreachable!() }
    }

    #[test]
    fn thread_root_advertises_thread_network_commissioning() {
        let network = THREAD_ROOT.clusters.iter().find(|cluster| cluster.id == 0x31).unwrap();
        assert_eq!(network.feature_map & 7, 2);
        assert!(THREAD_ROOT.clusters.iter().any(|cluster| cluster.id == 0x35));
        let diagnostics = THREAD_ROOT.clusters.iter().find(|cluster| cluster.id == 0x35).unwrap();
        assert_eq!(diagnostics.feature_map, rs_matter::dm::clusters::thread_diag::Feature::MAC_COUNTS.bits());
        let counter = diagnostics.attributes.iter().find(|attr| attr.id == 22).unwrap();
        assert!((diagnostics.with_attrs)(counter, diagnostics.revision, diagnostics.feature_map));
        assert!(!THREAD_ROOT.clusters.iter().any(|cluster| cluster.id == 0x37));
        assert_eq!(ROOT.clusters.iter().find(|cluster| cluster.id == 0x31).unwrap().feature_map & 7, 4);
    }

    #[test]
    fn temperatures_as_the_screen_shows_them() {
        let dev = crate::fields!{"temperature_scale" => "C", "away_temperature_high" => 24.44443};
        let sh = crate::fields!{"target_temperature" => 21.38377, "current_temperature" => 18.20557};
        let view = View { state: bridge::state(&dev, &sh), dev, sh };
        assert_eq!(view.shown_centi("target"), Some(2150));
        assert_eq!(view.shown_centi("current_temperature"), Some(1800));
        assert_eq!(view.shown_centi("away_temperature_high"), Some(2450));
        // In °F, whole degrees: 70.49 °F shows as 70 °F, 21.11 °C.
        let dev = crate::fields!{"temperature_scale" => "F"};
        let sh = crate::fields!{"target_temperature" => 21.38377};
        let view = View { state: bridge::state(&dev, &sh), dev, sh };
        assert_eq!(view.shown_centi("target"), Some(2111));
    }

    #[test]
    fn endpoints_follow_the_equipment() {
        let ids = |has_fan: bool, dev: Fields, sh: Fields| {
            let unit = nest(dev, sh);
            node(has_fan, &extras::switches(&unit)).endpoints.iter().map(|e| e.id).collect::<Vec<_>>()
        };
        assert_eq!(ids(false, crate::fields!{}, crate::fields!{"can_heat" => true}), [0, 1, 2, 3, 10, 11, 12, 13, 14]);
        assert_eq!(ids(true, crate::fields!{"has_humidifier" => true}, crate::fields!{"can_heat" => true, "can_cool" => true}),
                   [0, 1, 2, 3, 4, 5, 10, 11, 12, 13, 14, 15, 16]);
    }

    #[test]
    fn logs_rs_matters_warnings_and_pairing_codes_only() {
        use log::{Level, Log as _, Metadata};
        let enabled = |level: Level, target: &str| LOG.enabled(&Metadata::builder().level(level).target(target).build());
        assert!(enabled(Level::Warn, "rs_matter::transport"));
        assert!(enabled(Level::Error, "rs_matter::sc::pase"));
        assert!(enabled(Level::Info, "rs_matter"));
        // A controller reading a cluster the unit lacks is routine.
        assert!(!enabled(Level::Error, "rs_matter::im::busy"));
        assert!(!enabled(Level::Info, "rs_matter::transport"));
        assert!(!enabled(Level::Debug, "rs_matter"));
    }

    #[test]
    fn passcodes() {
        assert!(check_passcode(20202021).is_ok());
        for bad in [0, 11111111, 12345678, 87654321, 99999999, 100_000_000] {
            assert!(check_passcode(bad).is_err(), "{bad} accepted");
        }
    }
}
