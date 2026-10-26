//! Waking the unit for the broker. The unit suspends whenever nothing holds
//! it awake (nlsleepd, "nlscpm" on D-Bus), and asleep it only wakes for what
//! the WiFi chip was told to watch: before each sleep nlsleepd asks a
//! "czsniffer" for the last packet the service sent on its connection (the
//! IPv4 and TCP headers, 40 bytes) and programs the chip to wake on that
//! connection's packets. nlclient's own sniffer watches its connection to the
//! service, which is nestlocal over loopback: nothing on the air, so nothing
//! is programmed, and the broker's messages wait until the unit wakes for
//! something else, by which time the broker may have dropped the connection.
//!
//! So nestlocal is the sniffer for its MQTT connection, as nlclient is for
//! its own:
//! - it serves GetLastPacket (/com/nestlabs/czsniffer,
//!   com.nestlabs.czsniffer.main) with the last packet the broker sent it,
//!   captured on the WiFi interface;
//! - it registers the connection's remote port with nlsleepd
//!   (RegisterSniffer on gen2, RegisterWakeKa on gen3; the broker's packets
//!   come from that port) whenever the connection changes and every two minutes,
//!   and says the connection is up (GetScpmStats: uint32 dumpStats, uint32
//!   svcConnected, uint16 port) every 10 seconds and before each sleep, since
//!   nlsleepd builds the wake pattern from the last report and nlclient, its
//!   service being nestlocal, reports none;
//! - it is a sleep entity (RegisterEntity): before each sleep nlsleepd calls
//!   RequestSleep, and nestlocal reports its connection then; after each wake
//!   it calls WakeUp and counts nestlocal as holding the unit awake until it
//!   calls ReleaseResource, which nestlocal does after a few seconds: time
//!   for nlclient to fetch a change and for the answers to go out. Each wake
//!   also lets the bridge send to the broker if its keepalive is due.
//!
//! svcConnected stays 0: with it 1 the chip also keeps the connection alive
//! with TCP keepalives, and the gen 2, whose nlscpm works the same way, never
//! woke for the broker's messages with that on.
//!
//! Matter registers native IPv4 and IPv6 UDP destination-port wake patterns
//! through RegisterWakeKa. Until both registrations succeed, and while Matter
//! is starting or commissioning, nestlocal declines sleep. These unicast
//! patterns do not provide multicast discovery or scheduled subscription wakes.
//!
//! Zigbee wakes the unit itself: the radio keeps routing while the i.MX
//! sleeps and pulls nHOST_INT low for a message, which zbwake.ko makes a wake
//! source (radio_wakeup.rs). A falling edge is what wakes, so RequestSleep is
//! declined while the line is already low, until nestlocal has read what the
//! radio has. Where the module cannot load, messages wait for the next wake.
//!
//! A Thread RCP uses the same interrupt and programs host wake deadlines in
//! the radio. While thread0 exists, its daemon's idle record gates suspend.

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::logf;

const SCPM: &str = "com.nestlabs.scpm.sleepd";
const SNIFFER_PATH: &str = "/com/nestlabs/scpm/sniffer";
const SNIFFER: &str = "com.nestlabs.scpm.sniffer";
const WAKEKA_PATH: &str = "/com/nestlabs/scpm/wakeka";
const WAKEKA: &str = "com.nestlabs.scpm.wakeka";
const MANAGER_PATH: &str = "/com/nestlabs/scpm/sleepd";
const MANAGER: &str = "com.nestlabs.scpm.sleep_manager";
const CZSNIFFER_PATH: &str = "/com/nestlabs/czsniffer";
const ENTITY_PATH: &str = "/com/nestlabs/nestlocal";
/// What nlclient registers its own sniffer with.
const SNIFFER_INTERVAL: i32 = 10;
/// IPv4 and TCP headers without options: what nlsleepd takes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const HEADER_LEN: usize = 40;
/// RequestSleep waits this long before reporting, so that a report nlclient
/// sends as it is asked comes first.
const REPORT_DELAY: Duration = Duration::from_millis(300);
/// WakeUp holds the unit awake this long: a message's round trips.
const WAKE_HOLD: Duration = Duration::from_secs(3);
/// nlsleepd answers at once; one it drops must not hang the loop.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// An established TCP connection, as /proc/net/tcp lists it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct TcpConn {
    local: u16,
    remote: u16,
    remote_ip: [u8; 4],
}

impl TcpConn {
    /// An IPv4 TCP packet without IP options, from the remote end to the
    /// local port.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn matches(&self, p: &[u8]) -> bool {
        p.len() >= HEADER_LEN && p[0] == 0x45 && p[9] == 6 && p[12..16] == self.remote_ip
            && u16::from_be_bytes([p[20], p[21]]) == self.remote && u16::from_be_bytes([p[22], p[23]]) == self.local
    }
}

/// /proc/net/tcp's "0F02000A:0BB8": the address as the kernel's native
/// (little-endian) word, the port big-endian.
fn hex_addr(s: &str) -> Option<([u8; 4], u16)> {
    let (ip, port) = s.split_once(':')?;
    if ip.len() != 8 || port.len() != 4 {
        return None;
    }
    let ip = u32::from_str_radix(ip, 16).ok()?.to_le_bytes();
    Some((ip, u16::from_str_radix(port, 16).ok()?))
}

/// The first established connection to `port` in /proc/net/tcp's text.
fn broker_conn(table: &str, port: u16) -> Option<TcpConn> {
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 || fields[3] != "01" {
            return None;
        }
        let (_, local) = hex_addr(fields[1])?;
        let (remote_ip, remote) = hex_addr(fields[2])?;
        (remote == port).then_some(TcpConn { local, remote, remote_ip })
    })
}

#[derive(Default)]
struct State {
    /// The connection last registered.
    conn: Option<TcpConn>,
    /// The headers of the last packet the broker sent on it.
    last: Vec<u8>,
}

/// The sniffer for the MQTT connection to the broker on `broker_port`.
pub struct Wake {
    broker_port: u16,
    state: Mutex<State>,
}

impl Wake {
    fn new(broker_port: u16) -> Wake {
        Wake { broker_port, state: Mutex::new(State::default()) }
    }

    /// A packet seen on the WiFi interface (IPv4, no link header): kept if
    /// the broker sent it on the registered connection.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn sniffed(&self, packet: &[u8]) {
        let mut state = self.state.lock().unwrap();
        if state.conn.is_some_and(|c| c.matches(packet)) {
            state.last = packet[..HEADER_LEN].to_vec();
        }
    }

    /// The current connection to the broker, and whether it is new (a new
    /// one forgets the old one's last packet). None when there is none.
    fn update(&self, table: &str) -> Option<(TcpConn, bool)> {
        let found = broker_conn(table, self.broker_port);
        let mut state = self.state.lock().unwrap();
        let changed = found != state.conn;
        if changed {
            *state = State { conn: found, last: Vec::new() };
        }
        found.map(|c| (c, changed))
    }

    fn last_packet(&self) -> Vec<u8> {
        self.state.lock().unwrap().last.clone()
    }

    fn conn(&self) -> Option<TcpConn> {
        self.state.lock().unwrap().conn
    }
}

/// Starts waking the unit for the broker on `broker_port`, if any, watching
/// `iface`, registering UDP wake filters if `matter`, and keeping it awake while
/// the Zigbee radio has something to say if `zigbee`; `on_wake` runs on each
/// wake, before the hold.
pub fn start(broker_port: Option<u16>, matter: bool, zigbee: bool, iface: String,
             on_wake: impl Fn() + Send + Sync + 'static) {
    if broker_port.is_none() && !matter && !zigbee {
        return;
    }
    let wake = broker_port.map(|port| Arc::new(Wake::new(port)));
    if let Some(wake) = &wake {
        capture::start(iface, wake.clone());
    }
    tokio::spawn(run(wake, matter, zigbee, Arc::new(on_wake)));
}

/// Serves the sniffer and the sleep entity and keeps the registration up;
/// returns only if the system bus cannot be reached.
async fn run(wake: Option<Arc<Wake>>, matter: bool, zigbee: bool, on_wake: Arc<dyn Fn() + Send + Sync>) {
    let bus = match zbus::Connection::system().await {
        Ok(bus) => bus,
        Err(e) => {
            logf!("wake: no system bus ({e}): the unit will not wake for the broker or stay awake for Matter");
            return;
        }
    };
    let ready = Arc::new(AtomicBool::new(!matter));
    let entity = SleepClient { wake: wake.clone(), bus: bus.clone(), on_wake, matter, ready: ready.clone(), zigbee };
    let served = match &wake {
        Some(wake) => bus.object_server().at(CZSNIFFER_PATH, Sniffer(wake.clone())).await.map(|_| ()),
        None => Ok(()),
    };
    if let Err(e) = served.and(bus.object_server().at(ENTITY_PATH, entity).await.map(|_| ())) {
        logf!("wake: {e}");
        return;
    }
    // Again whenever nlsleepd (re)starts.
    if let Ok(dbus) = zbus::fdo::DBusProxy::new(&bus).await {
        if let Ok(mut changes) = dbus.receive_name_owner_changed_with_args(&[(0, SCPM)]).await {
            let bus = bus.clone();
            let ready = ready.clone();
            tokio::spawn(async move {
                use futures_lite::StreamExt;
                while let Some(change) = changes.next().await {
                    if change.args().is_ok_and(|a| a.new_owner().is_some()) {
                        enroll(&bus, matter, &ready).await;
                    }
                }
            });
        }
    }
    enroll(&bus, matter, &ready).await;
    let Some(wake) = wake else {
        // Matter or Zigbee only: nothing to sniff; the connection must stay,
        // or the entity goes with it.
        return std::future::pending().await;
    };
    let mut tick = 0u32;
    loop {
        let table = std::fs::read_to_string("/proc/net/tcp").unwrap_or_default();
        if let Some((conn, changed)) = wake.update(&table) {
            report(&bus, conn.remote).await;
            if changed || tick.is_multiple_of(12) {
                match register(&bus, conn.remote).await {
                    Ok(()) if changed => logf!("wake: registered the broker connection (local port {}, wake on port {}) \
                                                with nlsleepd", conn.local, conn.remote),
                    Ok(()) => {}
                    Err(e) => logf!("wake: registering port {}: {e}", conn.remote),
                }
            }
        }
        tick = tick.wrapping_add(1);
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

/// Registers the sleep entity and Matter filters before allowing sleep.
async fn enroll(bus: &zbus::Connection, matter: bool, ready: &Arc<AtomicBool>) {
    ready.store(!matter, Ordering::SeqCst);
    match call(bus, MANAGER_PATH, MANAGER, "RegisterEntity", &(ENTITY_PATH,)).await {
        Ok(()) => logf!("wake: registered with nlsleepd as a sleep entity"),
        Err(e) => return logf!("wake: RegisterEntity: {e}"),
    }
    if matter {
        for (name, ipv4) in [("nestlocal_matter_v4", true), ("nestlocal_matter_v6", false)] {
            // No TCP keepalive, any source port, unicast UDP to the Matter socket.
            if let Err(e) = call(bus, WAKEKA_PATH, WAKEKA, "RegisterWakeKa",
                &(name, 0u32, ipv4, false, 0u16, rs_matter::MATTER_PORT, false)).await {
                logf!("wake: {name}: {e}; keeping Matter awake");
                return;
            }
        }
        ready.store(true, Ordering::SeqCst);
        logf!("wake: registered Matter IPv4/IPv6 UDP port {} wake filters", rs_matter::MATTER_PORT);
    }
    if matter {
        release_when_idle(bus.clone(), matter, ready.clone());
    } else {
        release(bus).await;
    }
}

fn sleep_blocked(matter: bool, ready: bool, commissioning: bool) -> bool {
    matter && (!ready || commissioning)
}

fn release_when_idle(bus: zbus::Connection, matter: bool, ready: Arc<AtomicBool>) {
    tokio::spawn(async move {
        loop {
            let hold = if std::path::Path::new("/sys/class/net/thread0").exists() { Duration::from_millis(100) } else { WAKE_HOLD };
            tokio::time::sleep(hold).await;
            if !ready.load(Ordering::SeqCst) { return; }
            if !sleep_blocked(matter, true, crate::matter::sleep_blocked()) {
                release(&bus).await;
                return;
            }
        }
    });
}

async fn release(bus: &zbus::Connection) {
    if let Err(e) = call(bus, MANAGER_PATH, MANAGER, "ReleaseResource", &(ENTITY_PATH,)).await {
        logf!("wake: ReleaseResource: {e}");
    }
}

/// Tells nlsleepd the connection to the broker is up.
async fn report(bus: &zbus::Connection, port: u16) {
    let mut result = call(bus, SNIFFER_PATH, SNIFFER, "GetScpmStats", &(0u32, 0u32, port)).await;
    if result.as_ref().is_err_and(|e| unsupported(e)) {
        result = call(bus, WAKEKA_PATH, WAKEKA, "GetScpmStats", &(0u32, 0u32, port)).await;
    }
    if let Err(e) = result {
        logf!("wake: GetScpmStats for port {port}: {e}");
    }
}

fn unsupported(error: &str) -> bool {
    error.starts_with("org.freedesktop.DBus.Error.UnknownObject:")
        || error.starts_with("org.freedesktop.DBus.Error.UnknownMethod:")
}

async fn register(bus: &zbus::Connection, port: u16) -> Result<(), String> {
    let result = call(bus, SNIFFER_PATH, SNIFFER, "RegisterSniffer", &(SNIFFER_INTERVAL, port)).await;
    if !result.as_ref().is_err_and(|e| unsupported(e)) {
        return result;
    }
    // A zero keepalive interval registers the packet wake pattern without
    // claiming the chip's single TCP keepalive slot.
    call(bus, WAKEKA_PATH, WAKEKA, "RegisterWakeKa", &("nestlocal_mqtt", 0u32, true, true, port, 0u16, false)).await
}

async fn call<B>(bus: &zbus::Connection, path: &str, interface: &str, method: &str, body: &B) -> Result<(), String>
where B: serde::Serialize + zbus::zvariant::DynamicType {
    let reply = bus.call_method(Some(SCPM), path, Some(interface), method, body);
    match tokio::time::timeout(CALL_TIMEOUT, reply).await {
        Ok(Ok(reply)) if method == "RegisterWakeKa" => {
            match reply.body().deserialize::<bool>() {
                Ok(true) => Ok(()),
                Ok(false) => Err("nlsleepd rejected the wake configuration".into()),
                Err(e) => Err(e.to_string()),
            }
        }
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("no answer".into()),
    }
}

struct Sniffer(Arc<Wake>);

#[zbus::interface(name = "com.nestlabs.czsniffer.main")]
impl Sniffer {
    /// nlsleepd's call before the unit sleeps: the IPv4 and TCP headers of
    /// the last packet the broker sent, or none yet.
    #[zbus(name = "GetLastPacket")]
    fn get_last_packet(&self) -> Vec<u8> {
        self.0.last_packet()
    }
}

struct SleepClient {
    wake: Option<Arc<Wake>>,
    bus: zbus::Connection,
    on_wake: Arc<dyn Fn() + Send + Sync>,
    /// Matter needs both wake filters and a closed commissioning window.
    matter: bool,
    ready: Arc<AtomicBool>,
    /// Zigbee is on: the unit stays awake while the radio has something to say.
    zigbee: bool,
}

#[zbus::interface(name = "com.nestlabs.scpm.sleep_client")]
impl SleepClient {
    /// The unit is about to sleep: report the connection last, or decline
    /// while holding it awake or while the radio has something to say.
    #[zbus(name = "RequestSleep")]
    async fn request_sleep(&self) -> (String, bool) {
        if sleep_blocked(self.matter, self.ready.load(Ordering::SeqCst), crate::matter::sleep_blocked()) || (self.zigbee && crate::zigbee::wakeup::pending()) || crate::thread_sleep::blocked() {
            return (ENTITY_PATH.into(), false);
        }
        if self.wake.is_some() {
            tokio::time::sleep(REPORT_DELAY).await;
        }
        if let Some(conn) = self.wake.as_ref().and_then(|w| w.conn()) {
            report(&self.bus, conn.remote).await;
        }
        if crate::thread_sleep::blocked() || (self.zigbee && crate::zigbee::wakeup::pending()) {
            return (ENTITY_PATH.into(), false);
        }
        (ENTITY_PATH.into(), true)
    }

    /// The unit woke (reason or seconds asleep): hold it a moment, then let
    /// it sleep again.
    #[zbus(name = "WakeUp")]
    async fn wake_up(&self, _reason: i32) -> (String, bool) {
        let (bus, on_wake, matter, ready) = (self.bus.clone(), self.on_wake.clone(), self.matter, self.ready.clone());
        tokio::spawn(async move {
            on_wake();
            release_when_idle(bus, matter, ready);
        });
        (ENTITY_PATH.into(), true)
    }
}

#[cfg(target_os = "linux")]
mod capture {
    use std::sync::Arc;
    use std::time::Duration;

    use super::Wake;
    use crate::logf;

    /// Hands every IPv4 packet seen on `iface` (sent or received, no link
    /// header, at most its first bytes) to the sniffer. The interface comes
    /// and goes with the WiFi: the loop opens it again when it can.
    pub fn start(iface: String, wake: Arc<Wake>) {
        std::thread::spawn(move || loop {
            if let Err(e) = capture(&iface, &wake) {
                logf!("wake: capture on {iface}: {e}");
            }
            std::thread::sleep(Duration::from_secs(10));
        });
    }

    fn capture(iface: &str, wake: &Wake) -> std::io::Result<()> {
        let name = std::ffi::CString::new(iface).map_err(std::io::Error::other)?;
        let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
        if index == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let proto = (libc::ETH_P_IP as u16).to_be();
        let fd = unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_DGRAM, proto as i32) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let result = (|| {
            let mut address: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
            address.sll_family = libc::AF_PACKET as u16;
            address.sll_protocol = proto;
            address.sll_ifindex = index as i32;
            let size = std::mem::size_of::<libc::sockaddr_ll>() as u32;
            if unsafe { libc::bind(fd, &address as *const _ as *const libc::sockaddr, size) } < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // The headers are all that is kept.
            let mut buf = [0u8; 128];
            loop {
                let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
                if n < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                wake.sniffed(&buf[..n as usize]);
            }
        })();
        unsafe { libc::close(fd) };
        result
    }
}

#[cfg(not(target_os = "linux"))]
mod capture {
    use std::sync::Arc;

    use super::Wake;

    /// Packet capture is Linux's: elsewhere nothing is captured.
    pub fn start(_iface: String, _wake: Arc<Wake>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matter_requires_wake_filters_and_a_closed_pairing_window_to_sleep() {
        assert!(sleep_blocked(true, false, false));
        assert!(sleep_blocked(true, false, true));
        assert!(sleep_blocked(true, true, true));
        assert!(!sleep_blocked(true, true, false));
        // A disabled Matter service must not block MQTT or Zigbee sleep.
        assert!(!sleep_blocked(false, false, true));
    }

    #[test]
    fn only_missing_dbus_interfaces_trigger_the_compatibility_fallback() {
        assert!(unsupported("org.freedesktop.DBus.Error.UnknownObject: no such object"));
        assert!(unsupported("org.freedesktop.DBus.Error.UnknownMethod: no such method"));
        for error in ["org.freedesktop.DBus.Error.AccessDenied: denied",
                      "org.freedesktop.DBus.Error.InvalidArgs: wrong signature", "no answer"] {
            assert!(!unsupported(error));
        }
    }

    const TABLE: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:01BB 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 4318 1
   1: 2A00000A:C350 0F00000A:075B 06 00000000:00000000 00:00000000 00000000     0        0 0 1
   2: 2A00000A:D431 0F00000A:075B 01 00000000:00000000 00:00000000 00000000     0        0 5120 1
   3: 0100007F:01BB 0100007F:9A10 01 00000000:00000000 00:00000000 00000000     0        0 5200 1
";

    /// A packet from 10.0.0.15:1883 to port 54321, with TCP header and data.
    fn packet(from_port: u16, to_port: u16) -> Vec<u8> {
        let mut p = vec![0u8; 60];
        p[0] = 0x45;
        p[9] = 6;
        p[12..16].copy_from_slice(&[10, 0, 0, 15]);
        p[20..22].copy_from_slice(&from_port.to_be_bytes());
        p[22..24].copy_from_slice(&to_port.to_be_bytes());
        p[40..].fill(0xAA);
        p
    }

    #[test]
    fn proc_net_tcp_addresses_are_little_endian_words_and_big_endian_ports() {
        assert_eq!(hex_addr("0F00000A:075B"), Some(([10, 0, 0, 15], 1883)));
        assert_eq!(hex_addr("0100007F:01BB"), Some(([127, 0, 0, 1], 443)));
        for bad in ["0F00000A", "0F00000:075B", "0F00000A:75B", "XX00000A:075B", ""] {
            assert_eq!(hex_addr(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_broker_connection_is_the_established_one_to_its_port() {
        // Row 1 is in TIME_WAIT (06), row 2 established (01).
        assert_eq!(broker_conn(TABLE, 1883), Some(TcpConn { local: 0xD431, remote: 1883, remote_ip: [10, 0, 0, 15] }));
        assert_eq!(broker_conn(TABLE, 8883), None);
        assert_eq!(broker_conn("", 1883), None);
    }

    #[test]
    fn only_the_brokers_packets_on_the_connection_are_kept() {
        let wake = Wake::new(1883);
        // Nothing registered yet: nothing kept.
        wake.sniffed(&packet(1883, 0xD431));
        assert!(wake.last_packet().is_empty());
        assert_eq!(wake.update(TABLE).map(|(c, changed)| (c.local, changed)), Some((0xD431, true)));
        assert_eq!(wake.update(TABLE).map(|(_, changed)| changed), Some(false));
        for other in [packet(1883, 9999), packet(80, 0xD431), { let mut p = packet(1883, 0xD431); p[0] = 0x46; p },
                      { let mut p = packet(1883, 0xD431); p[9] = 17; p }, packet(1883, 0xD431)[..39].to_vec()] {
            wake.sniffed(&other);
        }
        assert!(wake.last_packet().is_empty());
        let mut p = packet(1883, 0xD431);
        wake.sniffed(&p);
        assert_eq!(wake.last_packet(), p[..HEADER_LEN]);
        p[30] = 7;
        wake.sniffed(&p);
        assert_eq!(wake.last_packet()[30], 7);
        // The connection gone: its last packet is forgotten.
        assert_eq!(wake.update(""), None);
        assert!(wake.last_packet().is_empty() && wake.conn().is_none());
    }
}
