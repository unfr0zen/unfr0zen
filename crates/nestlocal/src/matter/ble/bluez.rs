//! Matter's GATT service on the Thermostat E's Broadcom controller. BlueZ
//! owns the HCI UART and gives C2 indications a sequenced-packet socket.

use std::collections::HashMap;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixDatagram;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_channel::Sender;
use async_io::Async;
use embassy_futures::select::{select3, Either3};
use rs_matter::sc::pase::CommWindowState;
use rs_matter::transport::network::{
    btp::{AdvData, Btp},
    BtAddr,
};
use rs_matter::{BasicCommData, Matter};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

const APP: &str = "/org/unfr0zen/matter";
const SERVICE_PATH: &str = "/org/unfr0zen/matter/service";
const C1_PATH: &str = "/org/unfr0zen/matter/service/c1";
const C2_PATH: &str = "/org/unfr0zen/matter/service/c2";
const AD_PATH: &str = "/org/unfr0zen/matter/advertisement";
/// The ATT MTU every LE link has.
const MIN_MTU: u16 = 23;
const SERVICE_UUID: &str = "0000fff6-0000-1000-8000-00805f9b34fb";
const C1_UUID: &str = "18ee2ef5-263d-4559-959f-4f9c429f9d11";
const C2_UUID: &str = "18ee2ef5-263d-4559-959f-4f9c429f9d12";

enum Event {
    Write { peer: BtAddr, mtu: Option<u16>, value: Vec<u8> },
    Notify { peer: Option<BtAddr>, mtu: Option<u16>, socket: Async<UnixDatagram> },
}

fn peer(path: &ObjectPath<'_>) -> zbus::fdo::Result<BtAddr> {
    let invalid = || zbus::fdo::Error::InvalidArgs("invalid BlueZ device address".into());
    let name = path.as_str().rsplit('/').next().unwrap_or("");
    let bytes: Vec<u8> = name
        .strip_prefix("dev_")
        .ok_or_else(invalid)?
        .split('_')
        .map(|byte| u8::from_str_radix(byte, 16).map_err(|_| invalid()))
        .collect::<Result<_, _>>()?;
    Ok(BtAddr(bytes.try_into().map_err(|_| invalid())?))
}

/// The peer and the ATT MTU, where BlueZ gives them: the Thermostat E's BlueZ
/// 5.47 gives a write's device without its MTU, and a subscription's MTU (as
/// "MTU") without its device.
fn options(values: &HashMap<&str, Value<'_>>) -> zbus::fdo::Result<(Option<BtAddr>, Option<u16>)> {
    let bad = || zbus::fdo::Error::InvalidArgs("invalid device or MTU".into());
    let peer = values
        .get("device")
        .map(|path| ObjectPath::try_from(path).map_err(|_| bad()).and_then(|path| peer(&path)))
        .transpose()?;
    let mtu: Option<u16> =
        values.get("mtu").or_else(|| values.get("MTU")).map(|mtu| mtu.try_into().map_err(|_| bad())).transpose()?;
    if mtu.is_some_and(|mtu| !(23..=517).contains(&mtu)) {
        return Err(bad());
    }
    Ok((peer, mtu))
}

struct Service;
#[zbus::interface(name = "org.bluez.GattService1")]
impl Service {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> &str {
        SERVICE_UUID
    }
    #[zbus(property)]
    fn primary(&self) -> bool {
        true
    }
}

struct C1(Sender<Event>);
#[zbus::interface(name = "org.bluez.GattCharacteristic1")]
impl C1 {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> &str {
        C1_UUID
    }
    #[zbus(property)]
    fn service(&self) -> OwnedObjectPath {
        SERVICE_PATH.try_into().unwrap()
    }
    #[zbus(property)]
    fn flags(&self) -> Vec<&str> {
        vec!["write"]
    }
    async fn write_value(&self, value: Vec<u8>, args: HashMap<&str, Value<'_>>) -> zbus::fdo::Result<()> {
        let (peer, mtu) = options(&args)?;
        let peer = peer.ok_or_else(|| zbus::fdo::Error::InvalidArgs("the write names no device".into()))?;
        self.0
            .send(Event::Write { peer, mtu, value })
            .await
            .map_err(|_| zbus::fdo::Error::Failed("Matter BLE stopped".into()))
    }
}

struct C2 {
    events: Sender<Event>,
    acquired: Arc<AtomicBool>,
}
#[zbus::interface(name = "org.bluez.GattCharacteristic1")]
impl C2 {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> &str {
        C2_UUID
    }
    #[zbus(property)]
    fn service(&self) -> OwnedObjectPath {
        SERVICE_PATH.try_into().unwrap()
    }
    #[zbus(property)]
    fn flags(&self) -> Vec<&str> {
        vec!["indicate"]
    }
    #[zbus(property)]
    fn notify_acquired(&self) -> bool {
        self.acquired.load(Ordering::SeqCst)
    }
    fn confirm(&self) {}
    async fn acquire_notify(
        &self,
        args: HashMap<&str, Value<'_>>,
    ) -> zbus::fdo::Result<(zbus::zvariant::OwnedFd, u16)> {
        let (peer, mtu) = options(&args)?;
        if self.acquired.swap(true, Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed("Matter BLE already has a subscriber".into()));
        }
        let result = async {
            let (socket, remote) = sockets().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
            self.events
                .send(Event::Notify { peer, mtu, socket })
                .await
                .map_err(|_| zbus::fdo::Error::Failed("Matter BLE stopped".into()))?;
            Ok((remote.into(), mtu.unwrap_or(MIN_MTU)))
        }
        .await;
        if result.is_err() {
            self.acquired.store(false, Ordering::SeqCst);
        }
        result
    }
}

fn sockets() -> std::io::Result<(Async<UnixDatagram>, OwnedFd)> {
    let mut fds = [0; 2];
    // BlueZ requires SOCK_SEQPACKET so a closed subscriber is observable as EOF.
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: socketpair returned two new descriptors, each owned exactly once.
    let local = unsafe { UnixDatagram::from_raw_fd(fds[0]) };
    let remote = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    Ok((Async::new(local)?, remote))
}

struct Advertisement {
    name: String,
    data: Vec<u8>,
}
#[zbus::interface(name = "org.bluez.LEAdvertisement1")]
impl Advertisement {
    #[zbus(property, name = "Type")]
    fn kind(&self) -> &str {
        "peripheral"
    }
    #[zbus(property)]
    fn local_name(&self) -> &str {
        &self.name
    }
    #[zbus(property)]
    fn discoverable(&self) -> bool {
        true
    }
    #[zbus(property, name = "ServiceUUIDs")]
    fn service_uuids(&self) -> Vec<&str> {
        vec![SERVICE_UUID]
    }
    #[zbus(property)]
    fn service_data(&self) -> HashMap<String, OwnedValue> {
        HashMap::from([(SERVICE_UUID.into(), Value::from(self.data.clone()).try_into().unwrap())])
    }
    fn release(&self) {}
}

async fn confirmation(socket: Option<&Async<UnixDatagram>>) -> std::io::Result<usize> {
    match socket {
        Some(socket) => socket.read_with(|socket| socket.recv(&mut [0; 1])).await,
        None => std::future::pending().await,
    }
}

pub async fn serve(matter: &Matter<'_>, comm: &BasicCommData, btp: &Btp, serial: &str) -> Result<(), String> {
    let bus = zbus::Connection::system().await.map_err(|e| e.to_string())?;
    let om = zbus::fdo::ObjectManagerProxy::new(&bus, "org.bluez", "/").await.map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let adapter = loop {
        let objects = om.get_managed_objects().await.map_err(|e| e.to_string())?;
        if let Some((path, _)) = objects.into_iter().find(|(_, interfaces)| {
            ["org.bluez.Adapter1", "org.bluez.GattManager1", "org.bluez.LEAdvertisingManager1"]
                .iter()
                .all(|name| interfaces.contains_key(*name))
        }) {
            break path;
        }
        if Instant::now() >= deadline {
            return Err("no BlueZ adapter with GATT and advertising".into());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let adapter_proxy =
        zbus::Proxy::new(&bus, "org.bluez", adapter.as_str(), "org.bluez.Adapter1").await.map_err(|e| e.to_string())?;
    adapter_proxy.set_property("Powered", true).await.map_err(|e| e.to_string())?;
    let (sender, events) = async_channel::bounded(8);
    let acquired = Arc::new(AtomicBool::new(false));
    let server = bus.object_server();
    server.at(APP, zbus::fdo::ObjectManager).await.map_err(|e| e.to_string())?;
    server.at(SERVICE_PATH, Service).await.map_err(|e| e.to_string())?;
    server.at(C1_PATH, C1(sender.clone())).await.map_err(|e| e.to_string())?;
    server.at(C2_PATH, C2 { events: sender, acquired: acquired.clone() }).await.map_err(|e| e.to_string())?;
    let info = rs_matter::dm::clusters::basic_info::BasicInfoConfig {
        vid: matter.dev_det().vid,
        pid: matter.dev_det().pid,
        ..rs_matter::dm::clusters::basic_info::BasicInfoConfig::new()
    };
    let data = AdvData::new(&info, comm.discriminator).service_payload_iter().collect();
    server.at(AD_PATH, Advertisement { name: serial.into(), data }).await.map_err(|e| e.to_string())?;
    let manager = zbus::Proxy::new(&bus, "org.bluez", adapter.as_str(), "org.bluez.GattManager1")
        .await
        .map_err(|e| e.to_string())?;
    let advertising_manager = zbus::Proxy::new(&bus, "org.bluez", adapter.as_str(), "org.bluez.LEAdvertisingManager1")
        .await
        .map_err(|e| e.to_string())?;
    manager
        .call::<_, _, ()>(
            "RegisterApplication",
            &(ObjectPath::try_from(APP).unwrap(), HashMap::<String, OwnedValue>::new()),
        )
        .await
        .map_err(|e| e.to_string())?;
    super::READY.store(true, Ordering::SeqCst);
    crate::logf!("matter BLE: BlueZ peripheral ready");
    let mut socket: Option<Async<UnixDatagram>> = None;
    let mut connected: Option<BtAddr> = None;
    // The peer whose handshake request BTP holds: commissioners write it
    // before they subscribe for the response.
    let mut writer: Option<BtAddr> = None;
    let mut closed_since: Option<Instant> = None;
    let mut mtu = MIN_MTU;
    let mut advertising = false;
    loop {
        let open = !matches!(matter.comm_window_state(), CommWindowState::Closed);
        if open {
            closed_since = None;
        } else if connected.is_some() {
            closed_since.get_or_insert_with(Instant::now);
        }
        if connected.is_some()
            && (btp.timeout() || closed_since.is_some_and(|at: Instant| at.elapsed() >= Duration::from_secs(30)))
        {
            socket = None;
            connected = None;
            writer = None;
            acquired.store(false, Ordering::SeqCst);
            super::CONNECTED.store(false, Ordering::SeqCst);
            btp.reset();
        }
        let advertise = open && connected.is_none();
        if advertise != advertising {
            let path = ObjectPath::try_from(AD_PATH).unwrap();
            if advertise {
                advertising_manager
                    .call::<_, _, ()>("RegisterAdvertisement", &(path, HashMap::<String, OwnedValue>::new()))
                    .await
                    .map_err(|e| e.to_string())?;
            } else {
                advertising_manager
                    .call::<_, _, ()>("UnregisterAdvertisement", &(path,))
                    .await
                    .map_err(|e| e.to_string())?;
            }
            advertising = advertise;
        }
        if let Some(socket) = &socket {
            let mut packet = [0; 1024];
            let length = btp.process_outgoing(Some(mtu), &mut packet).map_err(|e| format!("BTP output: {e:?}"))?;
            if length > 0 {
                socket.send(&packet[..length]).await.map_err(|e| e.to_string())?;
            }
        }
        match select3(events.recv(), confirmation(socket.as_ref()), tokio::time::sleep(Duration::from_millis(20))).await
        {
            Either3::First(Ok(Event::Write { peer, mtu: received_mtu, value })) => {
                if connected.is_none_or(|connected| connected == peer) {
                    if socket.is_some() {
                        // The subscriber BlueZ named no device for.
                        connected = Some(peer);
                    }
                    mtu = received_mtu.unwrap_or(mtu);
                    writer = Some(peer);
                    btp.process_incoming(Some(mtu), peer, &value).map_err(|e| format!("BTP input: {e:?}"))?;
                }
            }
            Either3::First(Ok(Event::Notify { peer, mtu: received_mtu, socket: notifier })) => {
                // Without a device named, the subscriber is the peer that wrote,
                // or the next to write.
                let peer = peer.or(writer);
                if peer.is_none() || writer != peer {
                    btp.reset();
                }
                connected = peer;
                mtu = received_mtu.unwrap_or(mtu);
                socket = Some(notifier);
                super::CONNECTED.store(true, Ordering::SeqCst);
            }
            Either3::Second(Ok(0)) => {
                socket = None;
                connected = None;
                writer = None;
                mtu = MIN_MTU;
                acquired.store(false, Ordering::SeqCst);
                super::CONNECTED.store(false, Ordering::SeqCst);
                btp.reset();
            }
            Either3::Second(Err(error)) => return Err(error.to_string()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_options_accept_an_address_and_reject_invalid_mtu() {
        let path = ObjectPath::try_from("/org/bluez/hci0/dev_12_34_56_78_9A_BC").unwrap();
        let address = BtAddr([0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
        let mut args = HashMap::from([("device", Value::from(path)), ("link", Value::from("LE"))]);
        assert_eq!(options(&args).unwrap(), (Some(address), None), "BlueZ 5.47's write");
        args.insert("mtu", Value::from(247u16));
        assert_eq!(options(&args).unwrap(), (Some(address), Some(247)));
        args.insert("mtu", Value::from(22u16));
        assert!(options(&args).is_err());
        let subscription = HashMap::from([("MTU", Value::from(517u16))]);
        assert_eq!(options(&subscription).unwrap(), (None, Some(517)), "BlueZ 5.47's subscription");
        assert!(peer(&ObjectPath::try_from("/org/bluez/hci0/dev_12_34").unwrap()).is_err());
    }
}
