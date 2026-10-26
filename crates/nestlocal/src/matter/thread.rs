//! Matter's Thread network controller, backed by the dedicated OpenThread CLI socket.
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rs_matter::dm::clusters::net_comm::{NetCtl, NetCtlError, NetCtlStatus, NetworkCommissioningStatusEnum,
    NetworkScanInfo, NetworkType, WirelessCreds, ThreadCapabilitiesBitmap};
use rs_matter::dm::clusters::wifi_diag::WirelessDiag;
use rs_matter::dm::networks::NetChangeNotif;
use rs_matter::error::{Error, ErrorCode};
use rs_matter::utils::sync::DynBase;

use rs_matter::dm::networks::wireless::{WirelessNetwork, WirelessNetworks, Thread};
use rs_matter::tlv::{FromTLV, ToTLV};
use rs_matter::utils::init::{init, Init, IntoFallibleInit};

#[path = "thread_diag.rs"]
mod diagnostics;

pub type ThreadNetworks = WirelessNetworks<1, Network>;

/// Validates credentials before they enter Matter's staged network store.
#[derive(Clone, Debug, FromTLV, ToTLV)]
pub struct Network { inner: Thread }

impl WirelessNetwork for Network {
    fn id(&self) -> &[u8] { self.inner.ext_pan_id() }
    fn display_id(id: &[u8]) -> impl std::fmt::Display { Thread::display_id(id) }
    fn init_from<'a>(creds: &'a WirelessCreds<'a>) -> impl Init<Self, Error> + 'a {
        init!(Self { inner <- Thread::init() }).into_fallible().chain(move |network| network.update(creds))
    }
    fn update(&mut self, creds: &WirelessCreds<'_>) -> Result<(), Error> {
        let WirelessCreds::Thread { dataset_tlv } = creds else { return Err(ErrorCode::InvalidData.into()); };
        dataset_id(dataset_tlv)?;
        self.inner.update(creds)
    }
    fn creds(&self) -> WirelessCreds<'_> { self.inner.creds() }
}

#[derive(Default)]
struct Status {
    connected: bool,
    id: Option<[u8; 8]>,
    status: Option<NetworkCommissioningStatusEnum>,
    diagnostics: Option<diagnostics::Snapshot>,
}

pub struct Controller {
    status: Mutex<Status>,
    socket: std::path::PathBuf,
}
impl Default for Controller {
    fn default() -> Self {
        Self { status: Mutex::default(), socket: "/run/openthread-thread0.sock".into() }
    }
}
impl Controller {
    async fn command(&self, line: &str) -> std::io::Result<String> {
        super::srp::command_to(&self.socket, line).await
    }
    async fn attached(&self) -> Result<bool, Error> {
        let state = self.command("state").await.map_err(io_error)?;
        Ok(matches!(response_value(&state), Some("child" | "router" | "leader")))
    }
}

/// Remove the radio's active credentials when Matter has no configured network.
pub async fn clear_dataset() -> Result<(), Error> {
    for line in ["thread stop", "dataset clear", "dataset commit active"] {
        super::srp::command(line).await.map_err(io_error)?;
    }
    Ok(())
}

fn io_error(_: std::io::Error) -> Error { ErrorCode::StdIoError.into() }

/// Validate the complete active dataset before handing any credential to OpenThread.
fn dataset_id(mut tlvs: &[u8]) -> Result<[u8; 8], Error> {
    if tlvs.len() > 254 { return Err(ErrorCode::InvalidData.into()); }
    let mut seen = std::collections::BTreeSet::new();
    let mut id = None;
    while !tlvs.is_empty() {
        if tlvs.len() < 2 { return Err(ErrorCode::InvalidData.into()); }
        let (kind, size) = (tlvs[0], usize::from(tlvs[1]));
        if tlvs.len() < size + 2 || !seen.insert(kind) { return Err(ErrorCode::InvalidData.into()); }
        let value = &tlvs[2..size + 2];
        let valid = match kind {
            0 => size == 3 && value[0] == 0 && (11..=26).contains(&u16::from_be_bytes([value[1], value[2]])),
            1 => size == 2,
            2 => { id = value.try_into().ok(); size == 8 },
            3 => (1..=16).contains(&size) && std::str::from_utf8(value).is_ok(),
            4 | 5 => size == 16,
            7 | 14 => size == 8,
            12 => matches!(size, 3 | 4),
            _ => true,
        };
        if !valid { return Err(ErrorCode::InvalidData.into()); }
        tlvs = &tlvs[size + 2..];
    }
    if ![0, 1, 2, 3, 5, 7, 12, 14].iter().all(|kind| seen.contains(kind)) {
        return Err(ErrorCode::InvalidData.into());
    }
    id.ok_or_else(|| ErrorCode::InvalidData.into())
}

fn response_value(response: &str) -> Option<&str> {
    response.lines().map(|line| line.trim().trim_start_matches("> "))
        .find(|line| !line.is_empty() && *line != "Done")
}

impl NetCtl for Controller {
    fn net_type(&self) -> NetworkType { NetworkType::Thread }

    fn supported_thread_features(&self) -> ThreadCapabilitiesBitmap {
        ThreadCapabilitiesBitmap::IS_FULL_THREAD_DEVICE | ThreadCapabilitiesBitmap::IS_ROUTER_CAPABLE
    }

    async fn scan<F>(&self, network: Option<&[u8]>, mut f: F) -> Result<(), NetCtlError>
    where F: FnMut(&NetworkScanInfo) -> Result<(), Error> {
        let listing = self.command("nest-discover").await.map_err(io_error)?;
        for line in listing.lines() {
            let Some(scan) = Scan::parse(line) else { continue; };
            if network.is_some_and(|id| id != scan.ext_pan_id.to_be_bytes()) { continue; }
            f(&NetworkScanInfo::Thread { pan_id: scan.pan_id, ext_pan_id: scan.ext_pan_id,
                network_name: &scan.name, channel: scan.channel, version: scan.version,
                ext_addr: &scan.addr, rssi: scan.rssi, lqi: scan.lqi })?;
        }
        Ok(())
    }

    async fn connect(&self, creds: &WirelessCreds<'_>) -> Result<(), NetCtlError> {
        let WirelessCreds::Thread { dataset_tlv } = creds else { return Err(Error::from(ErrorCode::InvalidData).into()); };
        let id = dataset_id(dataset_tlv)?;
        let hex: String = dataset_tlv.iter().map(|byte| format!("{byte:02x}")).collect();
        let result = async {
            let current = self.command("dataset active -x").await.ok();
            if current.as_deref().and_then(response_value) != Some(hex.as_str()) || !self.attached().await? {
                self.command("thread stop").await.map_err(io_error)?;
                self.command(&format!("dataset set active {hex}")).await.map_err(io_error)?;
                self.command("mode rdn").await.map_err(io_error)?;
                self.command("ifconfig up").await.map_err(io_error)?;
                self.command("thread start").await.map_err(io_error)?;
            }
            let deadline = Instant::now() + Duration::from_secs(25);
            while Instant::now() < deadline {
                if self.attached().await? { return Ok(()); }
                async_io::Timer::after(Duration::from_millis(250)).await;
            }
            Err(NetCtlError::NetworkNotFound)
        }.await;
        let mut status = self.status.lock().unwrap();
        status.connected = result.is_ok();
        status.id = Some(id);
        status.status = Some(NetworkCommissioningStatusEnum::map_ctl_status(&result)
            .map_or(NetworkCommissioningStatusEnum::OtherConnectionFailure, |(status, _)| status));
        result
    }
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) || !hex.is_ascii() { return None; }
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect()
}

struct Scan {
    name: String,
    ext_pan_id: u64,
    pan_id: u16,
    addr: [u8; 8],
    channel: u16,
    rssi: i8,
    lqi: u8,
    version: u8,
}
impl Scan {
    fn parse(line: &str) -> Option<Self> {
        let fields: Vec<_> = line.trim().trim_start_matches("> ").split_whitespace().collect();
        if fields.len() != 9 || fields[0] != "NET" { return None; }
        Some(Self {
            name: String::from_utf8(decode_hex(fields[1])?).ok()?,
            ext_pan_id: u64::from_str_radix(fields[2], 16).ok()?,
            pan_id: u16::from_str_radix(fields[3], 16).ok()?,
            addr: decode_hex(fields[4])?.try_into().ok()?,
            channel: fields[5].parse().ok()?, rssi: fields[6].parse().ok()?,
            lqi: fields[7].parse().ok()?, version: fields[8].parse().ok()?,
        })
    }
}

impl NetCtlStatus for Controller {
    fn last_networking_status(&self) -> Result<Option<NetworkCommissioningStatusEnum>, Error> {
        Ok(self.status.lock().unwrap().status)
    }
    fn last_network_id<F, R>(&self, f: F) -> Result<R, Error>
    where F: FnOnce(Option<&[u8]>) -> Result<R, Error> {
        f(self.status.lock().unwrap().id.as_ref().map(|id| id.as_slice()))
    }
    fn last_connect_error_value(&self) -> Result<Option<i32>, Error> { Ok(None) }
}
impl DynBase for Controller {}
impl WirelessDiag for Controller {
    fn connected(&self) -> Result<bool, Error> { Ok(self.status.lock().unwrap().connected) }
}
impl NetChangeNotif for Controller {
    async fn wait_changed(&self) {
        async_io::Timer::after(Duration::from_secs(2)).await;
        self.refresh_diagnostics().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dataset() -> Vec<u8> {
        decode_hex("00030000140102abcd020801020304050607080304746573740510000102030405060708090a0b0c0d0e0f0708fd000000000000000c0302a0f70e080000000000010000").unwrap()
    }
    #[test]
    fn staged_networks_validate_before_mutation_and_restore_committed_credentials() {
        use rs_matter::dm::clusters::net_comm::Networks;
        let mut store = ThreadNetworks::new();
        let networks: &mut dyn Networks = &mut store;
        let original = dataset();
        networks.add_or_update(&WirelessCreds::Thread { dataset_tlv: &original }).unwrap();
        networks.set_managed(true).unwrap();
        let mut saved = [0; 1024];
        let n = networks.save(&mut saved).unwrap().unwrap();
        let mut replacement = original.clone(); replacement[11] = 42;
        networks.remove(&[1,2,3,4,5,6,7,8]).unwrap();
        networks.add_or_update(&WirelessCreds::Thread { dataset_tlv: &replacement }).unwrap();
        assert!(networks.add_or_update(&WirelessCreds::Thread { dataset_tlv: &replacement[..15] }).is_err());
        networks.load(&saved[..n]).unwrap();
        networks.creds(&[1,2,3,4,5,6,7,8], &mut |creds| {
            let WirelessCreds::Thread { dataset_tlv } = creds else { panic!() };
            assert_eq!(*dataset_tlv, original);
            Ok(())
        }).unwrap();
        assert!(networks.managed().unwrap());
    }

    #[cfg(unix)]
    pub(super) fn mock_cli(script: Vec<(String, String)>) -> (Controller, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, Write};
        use std::os::unix::net::UnixListener;
        let path = std::env::temp_dir().join(format!("thread-cli-{:x}.sock", rand::random::<u64>()));
        let listener = UnixListener::bind(&path).unwrap();
        let controller = Controller { socket: path.clone(), ..Controller::default() };
        let server = std::thread::spawn(move || {
            for (request, response) in script {
                let (mut socket, _) = listener.accept().unwrap();
                let mut line = String::new();
                std::io::BufReader::new(&socket).read_line(&mut line).unwrap();
                assert_eq!(line.trim(), request);
                socket.write_all(response.as_bytes()).unwrap();
            }
            std::fs::remove_file(path).unwrap();
        });
        (controller, server)
    }

    #[cfg(unix)]
    #[test]
    fn commissioner_credentials_reach_openthread_and_connection_status_is_reported() {
        let dataset = dataset();
        let hex: String = dataset.iter().map(|byte| format!("{byte:02x}")).collect();
        let script = vec![
            ("dataset active -x".into(), "Error 23: NotFound\r\n".into()),
            ("thread stop".into(), "Done\r\n".into()),
            (format!("dataset set active {hex}"), "Done\r\n".into()),
            ("mode rdn".into(), "Done\r\n".into()),
            ("ifconfig up".into(), "Done\r\n".into()),
            ("thread start".into(), "Done\r\n".into()),
            ("state".into(), "router\r\nDone\r\n".into()),
        ];
        let (controller, server) = mock_cli(script);
        futures_lite::future::block_on(controller.connect(&WirelessCreds::Thread { dataset_tlv: &dataset })).unwrap();
        assert!(controller.connected().unwrap());
        assert_eq!(controller.last_networking_status().unwrap(), Some(NetworkCommissioningStatusEnum::Success));
        server.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn connected_network_is_reused_without_restarting_thread() {
        let dataset = dataset();
        let hex: String = dataset.iter().map(|byte| format!("{byte:02x}")).collect();
        let (controller, server) = mock_cli(vec![
            ("dataset active -x".into(), format!("{hex}\r\nDone\r\n")),
            ("state".into(), "child\r\nDone\r\n".into()),
            ("state".into(), "child\r\nDone\r\n".into()),
        ]);
        futures_lite::future::block_on(controller.connect(&WirelessCreds::Thread { dataset_tlv: &dataset })).unwrap();
        server.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn daemon_errors_fail_connection_without_disclosing_credentials() {
        let dataset = dataset();
        let hex: String = dataset.iter().map(|byte| format!("{byte:02x}")).collect();
        let (controller, server) = mock_cli(vec![
            ("dataset active -x".into(), "Error 23: NotFound\r\n".into()),
            ("thread stop".into(), "Done\r\n".into()),
            (format!("dataset set active {hex}"), "Error 7: InvalidArgs\r\n".into()),
        ]);
        let error = futures_lite::future::block_on(controller.connect(&WirelessCreds::Thread { dataset_tlv: &dataset })).unwrap_err();
        assert!(!format!("{error:?}").contains(&hex));
        assert!(!controller.connected().unwrap());
        server.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn scan_reports_radio_fields_and_filters_extended_pan_id() {
        let (controller, server) = mock_cli(vec![("nest-discover".into(),
            "NET 74657374 0102030405060708 abcd 0011223344556677 20 -45 200 4\r\nDone\r\n".into())]);
        let mut found = false;
        futures_lite::future::block_on(controller.scan(Some(&[1,2,3,4,5,6,7,8]), |scan| {
            let NetworkScanInfo::Thread { network_name, version, rssi, .. } = scan else { panic!() };
            assert_eq!(*network_name, "test");
            assert_eq!((*version, *rssi), (4, -45));
            found = true;
            Ok(())
        })).unwrap();
        assert!(found);
        assert!(Scan::parse("NET malformed").is_none());
        server.join().unwrap();
    }

    #[test]
    fn validates_credentials_without_exposing_them() {
        let data = dataset();
        assert_eq!(dataset_id(&data).unwrap(), [1,2,3,4,5,6,7,8]);
        for n in 0..data.len() { assert!(dataset_id(&data[..n]).is_err()); }
        let mut duplicate = data.clone(); duplicate.extend_from_slice(&[2,8,0,0,0,0,0,0,0,0]);
        assert!(dataset_id(&duplicate).is_err());
        let mut bad_channel = data; bad_channel[4] = 27;
        assert!(dataset_id(&bad_channel).is_err());
    }
}
