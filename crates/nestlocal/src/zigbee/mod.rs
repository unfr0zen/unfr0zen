//! Zigbee (-zigbee): the thermostat on a Zigbee network (Zigbee2MQTT's, or
//! any Zigbee 3.0 coordinator's) as a router, through the unit's own
//! 802.15.4 radio, an EM3581 running EmberZNet's network coprocessor
//! firmware (platform/common/radio/ in the repository) in place of Nest's:
//! nestlocal carries it, installs it as needed, and keeps wpantund away.
//!
//! Like matter/, it reads the same buckets and runs the same commands as the
//! MQTT side. The network it joined is kept in the radio and on the unit
//! (network.rs), so the unit stays paired across restarts and radio updates;
//! with none, nestlocal joins the strongest network that permits joining, so
//! pairing is permitting joins on the coordinator. Enable pairing in the
//! settings menu leaves the previous network and forgets its credentials.
//! Changes go to the coordinator as attribute reports, whatever reporting it
//! configured; the unit's clock is set from the coordinator's Time cluster,
//! which Zigbee2MQTT and ZHA serve (clock.rs has the other sources).

mod beeper;
mod clusters;
mod ezsp;
mod network;
mod radio;
mod spi;
#[path = "../radio_wakeup.rs"]
pub mod wakeup;
mod zcl;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::bridge::Bridge;
use crate::{clock, logf};

use clusters::{Nest, THERMOSTAT_ENDPOINT, TIME};
use ezsp::{id, status, Aps, Callback, Ezsp, Incoming, Network, Reader};
use zcl::Value;

/// Home Automation, and where reports go: the coordinator's first endpoint.
const PROFILE_HA: u16 = 0x0104;
const COORDINATOR: u16 = 0x0000;
const COORDINATOR_ENDPOINT: u8 = 1;

/// As Nest's own radio set-up has it (wpantund.conf: TXPower -15); the mode
/// is the device's.
const TX_POWER_DBM: i8 = -15;

/// The Zigbee 3.0 global trust centre link key, "ZigBeeAlliance09".
const GLOBAL_LINK_KEY: &[u8; 16] = b"ZigBeeAlliance09";

/// How long after an install that left the radio silent to try another.
const INSTALL_RETRY: Duration = Duration::from_secs(3600);

/// How often a full report goes out, the first a little after joining (once
/// the coordinator has asked what the thermostat is), and how often joining
/// is tried without a network.
const FULL_REPORT: Duration = Duration::from_secs(600);
const FIRST_REPORT: Duration = Duration::from_secs(10);
const JOIN_RETRY: Duration = Duration::from_secs(30);
/// The coordinator can learn of a leave seconds after it (Zigbee2MQTT reports
/// it about 5 s later) and removes the device then, a rejoin included.
const REJOIN_AFTER_LEAVE: Duration = Duration::from_secs(10);
/// How long an answer waits for the radio to free a packet buffer.
const ANSWER_RETRY: Duration = Duration::from_secs(2);
/// UTCTime's zero, 2000-01-01 UTC, in seconds since 1970.
const ZIGBEE_EPOCH: u64 = 946_684_800;
const NO_TIME: u32 = 0xFFFF_FFFF;

#[derive(Default)]
struct PairingAttempt { deadline: Option<Instant>, stopped: bool }
impl PairingAttempt {
    fn begin(&mut self, now: Instant, request_age: Duration) {
        self.deadline = Some(now + Duration::from_secs(30).saturating_sub(request_age));
        self.stopped = false;
    }
    fn expired(&self, now: Instant) -> bool { self.deadline.is_some_and(|end| now >= end) }
    fn timeout(&mut self) { self.deadline = None; self.stopped = true; }
}

/// Runs the unit on Zigbee, on a thread of its own, once it has reported
/// (its equipment decides what it offers); again after the radio fails.
pub fn start(bridge: Arc<Bridge>, store: PathBuf) {
    let spawned = std::thread::Builder::new().name("zigbee".into()).spawn(move || {
        while !reported(&bridge) {
            std::thread::sleep(Duration::from_secs(5));
        }
        let nest = Nest::new(bridge);
        let changed = Arc::new(AtomicBool::new(true));
        let flag = changed.clone();
        nest.bridge.store.watch(move |_| flag.store(true, Ordering::Relaxed));
        let flag = changed.clone();
        nest.bridge.occupancy.on_change(move || flag.store(true, Ordering::Relaxed));
        let mut failed_install = None;
        let mut attempt = PairingAttempt::default();
        loop {
            if let Err(e) = run(&nest, &store, &changed, &mut failed_install, &mut attempt) {
                logf!("zigbee: {e}");
                crate::settings_ui::status("zigbee", "Radio unavailable. Try again.");
            }
            std::thread::sleep(JOIN_RETRY);
        }
    });
    if let Err(e) = spawned {
        logf!("zigbee: {e}");
    }
}

fn reported(bridge: &Bridge) -> bool {
    let buckets = bridge.buckets();
    buckets.contains_key("device") && buckets.contains_key("shared")
}

fn run(nest: &Nest, store: &Path, changed: &AtomicBool, failed_install: &mut Option<Instant>, attempt: &mut PairingAttempt)
       -> Result<(), String> {
    let (mut ezsp, stack) = open(failed_install)?;
    logf!("zigbee: radio up, EmberZNet {}.{}.{}.{}", stack >> 12, (stack >> 8) & 0xF, (stack >> 4) & 0xF, stack & 0xF);
    configure(&mut ezsp, nest)?;
    let mut up = match ezsp.command(id::NETWORK_INIT, &0u16.to_le_bytes())?.first() {
        Some(&status::SUCCESS) => wait_for_network(&mut ezsp)? || {
            logf!("zigbee: leaving the network the radio could not resume");
            leave(&mut ezsp)?
        },
        Some(&status::NOT_JOINED) => false,
        other => return Err(format!("networkInit: {other:02x?}")),
    };
    if !up {
        up = resume(&mut ezsp, store)?;
    }
    if up {
        keep(&mut ezsp, store);
    }
    let mut server = Server {
        sequence: 0, reported: BTreeMap::new(), next_full: Instant::now() + FIRST_REPORT, pending: false,
        time_asked: None, next_time: Instant::now() + FIRST_REPORT,
    };
    let mut asked = false;
    crate::settings_ui::status("zigbee", if up { "Paired" } else { "Not paired. Permit joining, then enable pairing." });
    loop {
        let request_age = crate::settings_ui::take_request_age("zigbee");
        if let Some(age) = request_age {
            let now = Instant::now();
            // Include time queued behind an existing scan or radio operation.
            attempt.begin(now, age);
            crate::settings_ui::status("zigbee", "Pairing... permit joining on coordinator");
            logf!("zigbee: pairing requested from settings");
            beeper::beep();
            if up {
                up = leave(&mut ezsp)?;
                let rejoin = Instant::now() + REJOIN_AFTER_LEAVE;
                while Instant::now() < rejoin {
                    pairing_progress(attempt.deadline);
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
            network::forget(store);
            asked = false;
        }
        if !up && attempt.stopped {
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        if !up {
            if attempt.expired(Instant::now()) {
                // End the actual radio attempt, not just its loading screen.
                leave(&mut ezsp)?;
                attempt.timeout();
                crate::settings_ui::status("zigbee", "Pairing timed out. Permit joining and retry.");
                continue;
            }
            if !asked {
                logf!("zigbee: on no network: permit joining on the coordinator to pair");
                asked = true;
            }
            up = join(&mut ezsp, attempt.deadline)?;
            if !up {
                let retry = attempt.deadline.map_or(Instant::now() + JOIN_RETRY, |end| end.min(Instant::now() + Duration::from_secs(1)));
                while Instant::now() < retry {
                    if std::path::Path::new(crate::settings_ui::DIR).join("zigbee.request").exists() { break; }
                    std::thread::sleep(Duration::from_millis(200));
                }
                continue;
            }
            if attempt.expired(Instant::now()) {
                leave(&mut ezsp)?;
                up = false;
                continue;
            }
            attempt.deadline = None;
            crate::settings_ui::status("zigbee", "Paired successfully");
            server.next_full = Instant::now() + FIRST_REPORT;
            server.next_time = server.next_full;
            keep(&mut ezsp, store);
        }
        if ezsp.wait(Duration::from_secs(1)) {
            while let Some(callback) = ezsp.callback()? {
                match callback.id {
                    id::STACK_STATUS_HANDLER if callback.params.first() == Some(&status::NETWORK_DOWN) => {
                        logf!("zigbee: left the network");
                        network::forget(store);
                        up = false;
                        asked = false;
                        crate::settings_ui::status("zigbee", "Network disconnected");
                    }
                    _ => server.callback(&mut ezsp, nest, callback)?,
                }
            }
        }
        if up && (changed.swap(false, Ordering::Relaxed) || server.pending) {
            server.report(&mut ezsp, nest, false)?;
        }
        if up && Instant::now() >= server.next_full {
            server.report(&mut ezsp, nest, true)?;
            keep(&mut ezsp, store);
        }
        if up && Instant::now() >= server.next_time {
            server.ask_time(&mut ezsp)?;
        }
    }
}

/// Puts back the network kept on the unit, if it is this radio's.
fn resume(ezsp: &mut Ezsp, store: &Path) -> Result<bool, String> {
    let Some(saved) = network::load(store) else {
        return Ok(false);
    };
    if saved.eui64 != network::eui64(ezsp)? {
        logf!("zigbee: {} holds another radio's network: not resumed", store.display());
        return Ok(false);
    }
    logf!("zigbee: resuming the network kept on the unit (PAN {:#06x}, channel {})", saved.pan_id, saved.channel);
    if let Err(e) = network::restore(ezsp, &saved, TX_POWER_DBM) {
        logf!("zigbee: resuming: {e}");
        return Ok(false);
    }
    wait_for_network(ezsp)
}

/// Keeps the network on the unit, as it is now.
fn keep(ezsp: &mut Ezsp, store: &Path) {
    let kept = network::take(ezsp).and_then(|saved| match network::load(store) {
        Some(old) if old == saved => Ok(()),
        _ => network::save(store, &saved),
    });
    if let Err(e) = kept {
        logf!("zigbee: keeping the network: {e}");
    }
}

/// The radio, running nestlocal's firmware: installed first if it runs
/// another, unless an install was just tried and the radio stayed silent.
fn open(failed_install: &mut Option<Instant>) -> Result<(Ezsp, u16), String> {
    radio::stop_wpantund();
    let e = match ours() {
        Ok(radio) => return Ok(radio),
        Err(e) => e,
    };
    if failed_install.is_some_and(|t| t.elapsed() < INSTALL_RETRY) {
        return Err(e);
    }
    logf!("zigbee: {e}");
    radio::install()?;
    ours().inspect(|_| *failed_install = None).inspect_err(|_| *failed_install = Some(Instant::now()))
}

/// The radio, if it runs this nestlocal's firmware (or any, when it carries none).
fn ours() -> Result<(Ezsp, u16), String> {
    let (mut ezsp, stack) = Ezsp::start()?;
    let Some(wanted) = radio::version() else {
        return Ok((ezsp, stack));
    };
    let info = ezsp.command(id::GET_XNCP_INFO, &[])?;
    let running = match info.as_slice() {
        [status::SUCCESS, _, _, low, high] => Some(u16::from_le_bytes([*low, *high])),
        _ => None,
    };
    match running {
        Some(version) if version == wanted => Ok((ezsp, stack)),
        Some(version) => Err(format!("the radio runs firmware version {version}, not {wanted}")),
        None => Err("the radio runs a firmware without a version".into()),
    }
}

/// The stack's sizes and the radio's power, then the endpoint and the
/// policies: configuration comes before anything else.
fn configure(ezsp: &mut Ezsp, nest: &Nest) -> Result<(), String> {
    ezsp.command(id::SET_MANUFACTURER_CODE, &zcl::MANUFACTURER_CODE.to_le_bytes())?;
    const STACK_PROFILE: u8 = 0x0C;
    const SECURITY_LEVEL: u8 = 0x0D;
    const TX_POWER_MODE: u8 = 0x17;
    const BINDING_TABLE_SIZE: u8 = 0x04;
    const APS_UNICAST_MESSAGE_COUNT: u8 = 0x03;
    // The coordinator binds each of the unit's clusters to itself, and a full
    // report is a message for each.
    let tx_power_mode = crate::device::current().radio.tx_power_mode;
    let configuration = [(STACK_PROFILE, 2), (SECURITY_LEVEL, 5), (TX_POWER_MODE, tx_power_mode),
                         (BINDING_TABLE_SIZE, 16), (APS_UNICAST_MESSAGE_COUNT, 20)];
    for (config, value) in configuration {
        let mut params = vec![config];
        params.extend(value.to_le_bytes());
        ezsp.expect(id::SET_CONFIGURATION_VALUE, &params).map_err(|e| format!("configuration {config:#04x}: {e}"))?;
    }
    for e in &nest.endpoints {
        // The thermostat's endpoint is the Time client that asks for the time.
        let clients: &[u16] = if e.id == THERMOSTAT_ENDPOINT { &[TIME] } else { &[] };
        let mut endpoint = vec![e.id];
        endpoint.extend(PROFILE_HA.to_le_bytes());
        endpoint.extend(e.device.to_le_bytes());
        endpoint.extend([1, e.clusters.len() as u8, clients.len() as u8]); // device version, server and client cluster counts
        e.clusters.iter().chain(clients).for_each(|c| endpoint.extend(c.to_le_bytes()));
        ezsp.expect(id::ADD_ENDPOINT, &endpoint)?;
    }
    const BINDING_MODIFICATION: u8 = 0x01;
    const ALLOW_BINDING_MODIFICATION: u8 = 0x11;
    ezsp.expect(id::SET_POLICY, &[BINDING_MODIFICATION, ALLOW_BINDING_MODIFICATION])
}

/// Joins the strongest network that permits joining, as a router, the way a
/// Zigbee 3.0 device does: the global link key, then the network key from
/// the trust centre, encrypted. Requests an individual trust-centre link key
/// so the router can authenticate other devices' joining notifications.
/// False when none permits joining.
fn join(ezsp: &mut Ezsp, ui_deadline: Option<Instant>) -> Result<bool, String> {
    const HAVE_PRECONFIGURED_KEY: u16 = 0x0100;
    const REQUIRE_ENCRYPTED_KEY: u16 = 0x0800;
    const GET_LINK_KEY_WHEN_JOINING: u16 = 0x0400;
    const TRUST_CENTER_GLOBAL_LINK_KEY: u16 = 0x0004;
    let mut security = (HAVE_PRECONFIGURED_KEY | REQUIRE_ENCRYPTED_KEY | GET_LINK_KEY_WHEN_JOINING
        | TRUST_CENTER_GLOBAL_LINK_KEY)
        .to_le_bytes().to_vec();
    security.extend(GLOBAL_LINK_KEY);
    security.extend([0; 16]); // no network key yet
    security.push(0); // its sequence number
    security.extend([0; 8]); // the trust centre's EUI64: whichever
    ezsp.expect(id::SET_INITIAL_SECURITY_STATE, &security)?;
    const EXTENDED_SECURITY_BITMASK: u8 = 0x0A;
    const JOINER_GLOBAL_LINK_KEY: u16 = 0x0010;
    let mut value = vec![EXTENDED_SECURITY_BITMASK, 2];
    value.extend(JOINER_GLOBAL_LINK_KEY.to_le_bytes());
    ezsp.expect(id::SET_VALUE, &value)?;

    // An active scan of channels 11-26.
    const ACTIVE_SCAN: u8 = 0x01;
    let mut scan = vec![ACTIVE_SCAN];
    scan.extend(0x07FF_F800u32.to_le_bytes());
    scan.push(3); // about 120 ms a channel
    ezsp.expect(id::START_SCAN, &scan)?;
    let mut found: Option<(Network, u8)> = None;
    let deadline = ui_deadline.map_or(Instant::now() + Duration::from_secs(15), |end| end.min(Instant::now() + Duration::from_secs(15)));
    'scan: while Instant::now() < deadline {
        pairing_progress(ui_deadline);
        if !ezsp.wait(Duration::from_secs(1)) {
            continue;
        }
        while let Some(callback) = ezsp.callback()? {
            match callback.id {
                id::NETWORK_FOUND_HANDLER => {
                    let mut r = Reader(&callback.params);
                    let network = Network::read(&mut r)?;
                    let lqi = r.u8()?;
                    if network.allowing_join && network.stack_profile == 2
                        && found.as_ref().is_none_or(|(_, best)| lqi > *best) {
                        found = Some((network, lqi));
                    }
                }
                // Per-channel errors come this way too; success is the end.
                id::SCAN_COMPLETE_HANDLER if callback.params.get(1) == Some(&status::SUCCESS) => break 'scan,
                _ => {}
            }
        }
    }
    let Some((network, lqi)) = found else {
        return Ok(false);
    };
    logf!("zigbee: joining PAN {:#06x} on channel {} (LQI {lqi})", network.pan_id, network.channel);
    const ROUTER: u8 = 0x02;
    let mut params = vec![ROUTER];
    params.extend(network.extended_pan_id);
    params.extend(network.pan_id.to_le_bytes());
    params.push(TX_POWER_DBM as u8);
    params.push(network.channel);
    params.push(0); // join method: MAC association
    params.extend(0u16.to_le_bytes()); // network manager
    params.push(network.update_id);
    params.extend(0u32.to_le_bytes()); // channels: the stack's
    ezsp.expect(id::JOIN_NETWORK, &params)?;
    wait_for_network_until(ezsp, ui_deadline)
}

/// Leaves the network the radio is on, or forgets one it kept but cannot
/// resume (or what Nest's own firmware left in its storage), to join afresh.
/// False: not on a network.
fn leave(ezsp: &mut Ezsp) -> Result<bool, String> {
    ezsp.command(id::LEAVE_NETWORK, &[])?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if ezsp.wait(Duration::from_secs(1)) {
            while let Some(callback) = ezsp.callback()? {
                if callback.id == id::STACK_STATUS_HANDLER && callback.params.first() == Some(&status::NETWORK_DOWN) {
                    return Ok(false);
                }
            }
        }
    }
    Ok(false)
}

/// Waits for the stack to say the network is up, or that it is not.
fn pairing_progress(deadline: Option<Instant>) {
    if let Some(end) = deadline {
        let seconds = end.saturating_duration_since(Instant::now()).as_secs();
        crate::settings_ui::status("zigbee", &format!("Pairing... {seconds}s remaining"));
    }
}
fn wait_for_network(ezsp: &mut Ezsp) -> Result<bool, String> {
    wait_for_network_until(ezsp, None)
}
fn wait_for_network_until(ezsp: &mut Ezsp, ui_deadline: Option<Instant>) -> Result<bool, String> {
    let deadline = ui_deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(30));
    while Instant::now() < deadline {
        pairing_progress(ui_deadline);
        if !ezsp.wait(Duration::from_secs(1)) {
            continue;
        }
        while let Some(callback) = ezsp.callback()? {
            if callback.id == id::STACK_STATUS_HANDLER {
                match callback.params.first().copied() {
                    Some(status::NETWORK_UP) => {
                        let node = Reader(&ezsp.command(id::GET_NODE_ID, &[])?).u16()?;
                        logf!("zigbee: on the network as {node:#06x}");
                        return Ok(true);
                    }
                    Some(s) if s != status::NETWORK_DOWN => {
                        logf!("zigbee: joining failed (status {s:#04x})");
                        return Ok(false);
                    }
                    _ => {}
                }
            }
        }
    }
    logf!("zigbee: the network did not come up");
    Ok(false)
}

/// The thermostat's ZCL server side: answers, and reports. And its one
/// client side, the time.
struct Server {
    sequence: u8,
    /// What was last reported, by endpoint, cluster and attribute.
    reported: BTreeMap<(u8, u16, u16), Value>,
    next_full: Instant,
    /// Reports the radio had no room for yet.
    pending: bool,
    /// The time read on its way: when it was sent (wall clock) and its sequence.
    time_asked: Option<(SystemTime, u8)>,
    /// When to ask for the time: a minute after a read that got no time,
    /// else clock::RECHECK.
    next_time: Instant,
}

impl Server {
    fn callback(&mut self, ezsp: &mut Ezsp, nest: &Nest, callback: Callback) -> Result<(), String> {
        if callback.id != id::INCOMING_MESSAGE_HANDLER {
            return Ok(());
        }
        let message = Incoming::read(&callback.params)?;
        if message.aps.cluster == TIME && message.aps.destination_endpoint == THERMOSTAT_ENDPOINT {
            self.time_answer(&message.payload);
        }
        if let Some((endpoint, answer)) = answer(nest, &message) {
            let to = (message.sender, message.aps.source_endpoint);
            // A read unanswered now is a failed read: wait briefly for room.
            let until = Instant::now() + ANSWER_RETRY;
            while !send(ezsp, to, endpoint, message.aps.cluster, &answer)? && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Ok(())
    }

    /// Reports to the coordinator what changed, or everything; what the
    /// radio has no room for goes on the next round.
    fn report(&mut self, ezsp: &mut Ezsp, nest: &Nest, everything: bool) -> Result<(), String> {
        use zcl::Clusters as _;
        if everything {
            self.reported.clear();
            self.next_full = Instant::now() + FULL_REPORT;
        }
        self.pending = false;
        for (endpoint, cluster, attributes) in nest.reported() {
            let changed: Vec<(u16, Value)> = attributes.iter()
                .filter_map(|&a| nest.read(endpoint, cluster, a).map(|v| (a, v)))
                .filter(|(a, v)| self.reported.get(&(endpoint, cluster, *a)) != Some(v))
                .collect();
            if changed.is_empty() {
                continue;
            }
            self.sequence = self.sequence.wrapping_add(1);
            let report = zcl::report(self.sequence, &changed);
            if !send(ezsp, (COORDINATOR, COORDINATOR_ENDPOINT), endpoint, cluster, &report)? {
                self.pending = true;
                return Ok(());
            }
            for (a, v) in changed {
                self.reported.insert((endpoint, cluster, a), v);
            }
        }
        Ok(())
    }

    /// Reads the coordinator's Time attribute.
    fn ask_time(&mut self, ezsp: &mut Ezsp) -> Result<(), String> {
        self.sequence = self.sequence.wrapping_add(1);
        let request = zcl::read_request(self.sequence, &[0x0000]);
        if send(ezsp, (COORDINATOR, COORDINATOR_ENDPOINT), THERMOSTAT_ENDPOINT, TIME, &request)? {
            self.time_asked = Some((SystemTime::now(), self.sequence));
        }
        self.next_time = Instant::now() + clock::RETRY;
        Ok(())
    }

    /// Takes the coordinator's answer to ask_time, if `payload` is one and
    /// came promptly: true when it set the clock.
    fn time_answer(&mut self, payload: &[u8]) -> bool {
        let Some((sequence, values)) = zcl::read_response(payload) else {
            return false;
        };
        let Some((asked, _)) = self.time_asked.take_if(|(_, s)| *s == sequence) else {
            return false;
        };
        let Some(Value::Utc(t)) = values.iter().find(|(id, _)| *id == 0x0000).map(|(_, v)| v) else {
            return false;
        };
        if *t == NO_TIME || !clock::answer(asked, UNIX_EPOCH + Duration::from_secs(ZIGBEE_EPOCH + u64::from(*t)), "the Zigbee coordinator") {
            return false;
        }
        self.next_time = Instant::now() + clock::RECHECK;
        true
    }
}

/// The answer to a message, if one is due, and the endpoint it is from: a
/// broadcast or multicast gets no default response.
fn answer(nest: &Nest, message: &Incoming) -> Option<(u8, Vec<u8>)> {
    // EmberIncomingMessageType: unicast, unicast reply; multicast, broadcast
    // and their loopbacks above.
    const UNICAST_REPLY: u8 = 0x01;
    let endpoint = match message.aps.destination_endpoint {
        0xFF => THERMOSTAT_ENDPOINT, // broadcast: the thermostat answers
        e => e,
    };
    if message.aps.profile != PROFILE_HA || !nest.endpoints.iter().any(|e| e.id == endpoint) {
        return None;
    }
    let answer = zcl::serve(nest, endpoint, message.aps.cluster, &message.payload)?;
    (message.kind <= UNICAST_REPLY || !zcl::is_default_response(&answer)).then_some((endpoint, answer))
}

/// A ZCL frame from one of the unit's endpoints to a node's endpoint,
/// acknowledged and routed. False when the radio has no room for it yet.
fn send(ezsp: &mut Ezsp, (node, to): (u16, u8), from: u8, cluster: u16, frame: &[u8]) -> Result<bool, String> {
    const DIRECT: u8 = 0x00;
    const RETRY: u16 = 0x0040;
    const ENABLE_ROUTE_DISCOVERY: u16 = 0x0100;
    let aps = Aps {
        profile: PROFILE_HA,
        cluster,
        source_endpoint: from,
        destination_endpoint: to,
        options: RETRY | ENABLE_ROUTE_DISCOVERY,
        ..Aps::default()
    };
    let mut params = vec![DIRECT];
    params.extend(node.to_le_bytes());
    aps.write(&mut params);
    params.push(0); // message tag
    params.push(frame.len() as u8);
    params.extend_from_slice(frame);
    match ezsp.command(id::SEND_UNICAST, &params)?.first() {
        Some(&status::SUCCESS) => Ok(true),
        Some(&status::MAX_MESSAGE_LIMIT_REACHED | &status::NO_BUFFERS) => Ok(false),
        // Not delivered now (no route yet, say): the next report brings it.
        Some(s) => {
            logf!("zigbee: sending to {node:#06x}: status {s:#04x}");
            Ok(true)
        }
        None => Err("sendUnicast: empty answer".into()),
    }
}

#[cfg(test)]
mod pairing_tests {
    use super::*;
    #[test]
    fn menu_pairing_expires_and_stays_stopped_until_another_request() {
        let now = Instant::now();
        let mut attempt = PairingAttempt::default();
        attempt.begin(now, Duration::ZERO);
        assert!(!attempt.expired(now + Duration::from_secs(29)));
        assert!(attempt.expired(now + Duration::from_secs(30)));
        attempt.timeout();
        assert!(attempt.stopped);
        assert!(attempt.deadline.is_none());
        attempt.begin(now + Duration::from_secs(60), Duration::ZERO);
        assert!(!attempt.stopped);
        assert!(!attempt.expired(now + Duration::from_secs(89)));
        assert!(attempt.expired(now + Duration::from_secs(90)));
    }

    #[test]
    fn without_a_menu_request_joining_neither_expires_nor_stops() {
        let attempt = PairingAttempt::default();
        assert!(!attempt.expired(Instant::now() + Duration::from_secs(3600)));
        assert!(!attempt.stopped);
    }

    #[test]
    fn time_queued_before_menu_request_is_handled_counts_towards_timeout() {
        let now = Instant::now();
        let mut attempt = PairingAttempt::default();
        attempt.begin(now, Duration::from_secs(20));
        assert!(!attempt.expired(now + Duration::from_secs(9)));
        assert!(attempt.expired(now + Duration::from_secs(10)));
        for age in [30, 45] {
            attempt.begin(now, Duration::from_secs(age));
            assert!(attempt.expired(now));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_coordinators_time_sets_the_clock_when_it_is_the_answer_to_the_last_ask() {
        let mut server = Server {
            sequence: 0, reported: BTreeMap::new(), next_full: Instant::now(), pending: false,
            time_asked: Some((SystemTime::now(), 9)), next_time: Instant::now(),
        };
        // Now, as the coordinator would have it: the clock is not off, and
        // the answer counts.
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let answer = |sequence: u8, t: u32| {
            let mut frame = vec![0x18, sequence, 0x01, 0x00, 0x00, 0x00, 0xE2];
            frame.extend(t.to_le_bytes());
            frame
        };
        assert!(!server.time_answer(&answer(8, (now - ZIGBEE_EPOCH) as u32)), "another sequence");
        assert!(server.time_asked.is_some());
        assert!(server.time_answer(&answer(9, (now - ZIGBEE_EPOCH) as u32)));
        assert!(server.time_asked.is_none());
        assert!(server.next_time > Instant::now() + clock::RECHECK - Duration::from_secs(5));
        // No time in the answer, or asked nothing.
        server.time_asked = Some((SystemTime::now(), 10));
        assert!(!server.time_answer(&answer(10, NO_TIME)));
        server.time_asked = Some((SystemTime::now(), 11));
        assert!(!server.time_answer(&[0x18, 11, 0x01, 0x00, 0x00, 0x86]));
        assert!(!server.time_answer(&answer(12, 0)));
        // One that waited too long is not trusted.
        server.time_asked = Some((SystemTime::now() - clock::ANSWER - Duration::from_secs(1), 13));
        assert!(!server.time_answer(&answer(13, (now - ZIGBEE_EPOCH) as u32)));
    }

    #[test]
    fn broadcasts_and_multicasts_get_answers_but_no_default_responses() {
        let nest = Nest::new(crate::bridge::tests::test_bridge());
        let message = |kind: u8, payload: &[u8]| Incoming {
            kind,
            aps: Aps { profile: PROFILE_HA, cluster: clusters::THERMOSTAT, source_endpoint: 1, destination_endpoint: 0xFF,
                       ..Aps::default() },
            sender: COORDINATOR,
            payload: payload.to_vec(),
        };
        let unknown = [0x01, 7, 0x01]; // a command the Thermostat cluster does not have
        let read = [0x00, 8, 0x00, 0x03, 0x00]; // AbsMinHeatSetpointLimit
        for kind in [0x00, 0x01] {
            assert_eq!(answer(&nest, &message(kind, &unknown)), Some((1, vec![0x18, 7, 0x0B, 0x01, 0x81])), "{kind}");
        }
        for kind in 0x02..=0x05 {
            assert_eq!(answer(&nest, &message(kind, &unknown)), None, "{kind}");
            assert_eq!(answer(&nest, &message(kind, &read)), Some((1, vec![0x18, 8, 0x01, 0x03, 0x00, 0x00, 0x29, 0x84, 0x03])),
                       "{kind}");
        }
    }
}
