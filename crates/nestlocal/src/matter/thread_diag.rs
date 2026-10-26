//! Public OpenThread snapshots for Matter's synchronous diagnostics reads.
use rs_matter::dm::clusters::thread_diag::{ThreadDiag, NeighborTable, RouteTable, SecurityPolicy,
    OperationalDatasetComponents, RoutingRoleEnum, NetworkFaultEnum, MacCounters};
use rs_matter::error::{Error, ErrorCode};
use serde::Deserialize;

use super::{Controller, decode_hex};

/// A neighbor as the producer sends it: extended address, age, RLOC16, link
/// and MLE frame counters, LQI, average and last RSSI, frame and message
/// error rates, and the rx-on-when-idle, full-thread-device, full-network-data
/// and is-child flags.
type WireNeighbor = (String, u32, u16, u32, u32, u8, i8, i8, u8, u8, u8, u8, u8, u8);
/// A route as the producer sends it: extended address, RLOC16, router ID,
/// next hop, path cost, LQI in and out, age, allocated and link established.
type WireRoute = (String, u16, u8, u8, u8, u8, u8, u8, u8, u8);

#[derive(Deserialize)]
struct Wire {
    role: u8,
    channel: u16,
    ext_address: String,
    rloc16: u16,
    leader: Option<[u32; 5]>,
    policy: Option<[u16; 2]>,
    dataset: String,
    components: u16,
    neighbors: Vec<WireNeighbor>,
    routes: Vec<WireRoute>,
    mac: Vec<u32>,
}

pub(super) struct Snapshot {
    role: RoutingRoleEnum,
    channel: u16,
    ext_address: u64,
    rloc16: u16,
    leader: Option<[u32; 5]>,
    name: Option<String>,
    pan: Option<u16>,
    ext_pan: Option<u64>,
    prefix: Option<[u8; 8]>,
    policy: Option<SecurityPolicy>,
    mask: Option<[u8; 4]>,
    components: OperationalDatasetComponents,
    neighbors: Vec<NeighborTable>,
    routes: Vec<RouteTable>,
    mac: MacCounters,
}

fn invalid() -> Error { ErrorCode::InvalidData.into() }
fn address(hex: &str) -> Result<u64, Error> {
    if hex.len() != 16 { return Err(invalid()); }
    u64::from_str_radix(hex, 16).map_err(|_| invalid())
}
fn boolean(value: u8) -> Result<bool, Error> {
    match value { 0 => Ok(false), 1 => Ok(true), _ => Err(invalid()) }
}

impl Snapshot {
    fn parse(response: &str) -> Result<Self, Error> {
        let json = response.lines().map(|line| line.trim().trim_start_matches("> "))
            .find(|line| line.starts_with('{')).ok_or_else(invalid)?;
        let wire: Wire = serde_json::from_str(json).map_err(|_| invalid())?;
        let role = match wire.role {
            0 => RoutingRoleEnum::Unspecified, 1 => RoutingRoleEnum::Unassigned,
            2 => RoutingRoleEnum::SleepyEndDevice, 3 => RoutingRoleEnum::EndDevice,
            4 => RoutingRoleEnum::REED, 5 => RoutingRoleEnum::Router, 6 => RoutingRoleEnum::Leader,
            _ => return Err(invalid()),
        };
        let c = wire.components;
        let mut snapshot = Self {
            role, channel: wire.channel, ext_address: address(&wire.ext_address)?, rloc16: wire.rloc16,
            leader: wire.leader, name: None, pan: None, ext_pan: None, prefix: None,
            policy: wire.policy.map(|p| SecurityPolicy { rotation_time: p[0], flags: p[1] }), mask: None,
            components: OperationalDatasetComponents {
                active_timestamp_present: c & 1 != 0, pending_timestamp_present: c & 2 != 0,
                master_key_present: c & 4 != 0, network_name_present: c & 8 != 0,
                extended_pan_id_present: c & 16 != 0, mesh_local_prefix_present: c & 32 != 0,
                delay_present: c & 64 != 0, pan_id_present: c & 128 != 0, channel_present: c & 256 != 0,
                pskc_present: c & 512 != 0, security_policy_present: c & 1024 != 0, channel_mask_present: c & 2048 != 0,
            },
            neighbors: Vec::new(), routes: Vec::new(), mac: mac_counters(&wire.mac)?,
        };
        let bytes = decode_hex(&wire.dataset).ok_or_else(invalid)?;
        let mut tlvs = bytes.as_slice();
        while !tlvs.is_empty() {
            if tlvs.len() < 2 || tlvs.len() < 2 + usize::from(tlvs[1]) { return Err(invalid()); }
            let size = usize::from(tlvs[1]);
            let value = &tlvs[2..2 + size];
            match tlvs[0] {
                0 if size == 3 => {},
                1 if size == 2 => snapshot.pan = Some(u16::from_be_bytes(value.try_into().unwrap())),
                2 if size == 8 => snapshot.ext_pan = Some(u64::from_be_bytes(value.try_into().unwrap())),
                3 if (1..=16).contains(&size) => snapshot.name = Some(std::str::from_utf8(value).map_err(|_| invalid())?.into()),
                7 if size == 8 => snapshot.prefix = Some(value.try_into().unwrap()),
                53 => {
                    let mut masks = value;
                    while !masks.is_empty() {
                        if masks.len() < 2 || masks.len() < 2 + usize::from(masks[1]) { return Err(invalid()); }
                        let length = usize::from(masks[1]);
                        if masks[0] == 0 {
                            if length != 4 { return Err(invalid()); }
                            // Matter's left-most bit denotes channel zero.
                            let mask = u32::from_be_bytes(masks[2..6].try_into().unwrap());
                            snapshot.mask = Some(mask.reverse_bits().to_be_bytes());
                        }
                        masks = &masks[2 + length..];
                    }
                }
                _ => return Err(invalid()), // A diagnostics response must never carry network keys.
            }
            tlvs = &tlvs[2 + size..];
        }
        for (ext, age, rloc16, link_frame_counter, mle_frame_counter, lqi, average, last,
            frame_error_rate, message_error_rate, rx, full, data, child) in wire.neighbors {
            snapshot.neighbors.push(NeighborTable { ext_address: address(&ext)?, age, rloc16, link_frame_counter,
                mle_frame_counter, lqi, average_rssi: (average != 127).then_some(average), last_rssi: (last != 127).then_some(last),
                frame_error_rate, message_error_rate, rx_on_when_idle: boolean(rx)?, full_thread_device: boolean(full)?,
                full_network_data: boolean(data)?, is_child: boolean(child)? });
        }
        for (ext, rloc16, router_id, next_hop, path_cost, lqi_in, lqi_out, age, allocated, linked) in wire.routes {
            snapshot.routes.push(RouteTable { ext_address: address(&ext)?, rloc16, router_id, next_hop, path_cost,
                lqi_in, lqi_out, age, allocated: boolean(allocated)?, link_established: boolean(linked)? });
        }
        Ok(snapshot)
    }
    fn connected(&self) -> bool {
        !matches!(self.role, RoutingRoleEnum::Unspecified | RoutingRoleEnum::Unassigned)
    }
}

impl Controller {
    pub(super) async fn refresh_diagnostics(&self) {
        let snapshot = match self.command("nest-diag").await {
            Ok(response) => Snapshot::parse(&response).ok(),
            Err(_) => None,
        };
        let mut status = self.status.lock().unwrap();
        status.connected = snapshot.as_ref().is_some_and(Snapshot::connected);
        status.diagnostics = snapshot;
    }
    fn diag<R>(&self, f: impl FnOnce(Option<&Snapshot>) -> R) -> R {
        f(self.status.lock().unwrap().diagnostics.as_ref())
    }
}

impl ThreadDiag for Controller {
    fn channel(&self) -> Result<Option<u16>, Error> { Ok(self.diag(|d| d.map(|d| d.channel))) }
    fn routing_role(&self) -> Result<Option<RoutingRoleEnum>, Error> { Ok(self.diag(|d| d.map(|d| d.role))) }
    fn network_name(&self, f: &mut dyn FnMut(Option<&str>) -> Result<(), Error>) -> Result<(), Error> {
        self.diag(|d| f(d.and_then(|d| d.name.as_deref())))
    }
    fn pan_id(&self) -> Result<Option<u16>, Error> { Ok(self.diag(|d| d.and_then(|d| d.pan))) }
    fn extended_pan_id(&self) -> Result<Option<u64>, Error> { Ok(self.diag(|d| d.and_then(|d| d.ext_pan))) }
    fn mesh_local_prefix(&self, f: &mut dyn FnMut(Option<&[u8]>) -> Result<(), Error>) -> Result<(), Error> {
        self.diag(|d| f(d.and_then(|d| d.prefix.as_ref().map(|p| p.as_slice()))))
    }
    fn neighbor_table(&self, f: &mut dyn FnMut(&NeighborTable) -> Result<(), Error>) -> Result<(), Error> {
        self.diag(|d| { if let Some(d) = d { for item in &d.neighbors { f(item)?; } } Ok(()) })
    }
    fn route_table(&self, f: &mut dyn FnMut(&RouteTable) -> Result<(), Error>) -> Result<(), Error> {
        self.diag(|d| { if let Some(d) = d { for item in &d.routes { f(item)?; } } Ok(()) })
    }
    fn partition_id(&self) -> Result<Option<u32>, Error> { Ok(self.diag(|d| d.and_then(|d| d.leader.map(|l| l[0])))) }
    fn weighting(&self) -> Result<Option<u16>, Error> { Ok(self.diag(|d| d.and_then(|d| d.leader.map(|l| l[1] as u16)))) }
    fn data_version(&self) -> Result<Option<u16>, Error> { Ok(self.diag(|d| d.and_then(|d| d.leader.map(|l| l[2] as u16)))) }
    fn stable_data_version(&self) -> Result<Option<u16>, Error> { Ok(self.diag(|d| d.and_then(|d| d.leader.map(|l| l[3] as u16)))) }
    fn leader_router_id(&self) -> Result<Option<u8>, Error> { Ok(self.diag(|d| d.and_then(|d| d.leader.map(|l| l[4] as u8)))) }
    fn ext_address(&self) -> Result<Option<u64>, Error> { Ok(self.diag(|d| d.map(|d| d.ext_address))) }
    fn rloc_16(&self) -> Result<Option<u16>, Error> { Ok(self.diag(|d| d.filter(|d| d.connected()).map(|d| d.rloc16))) }
    fn security_policy(&self) -> Result<Option<SecurityPolicy>, Error> { Ok(self.diag(|d| d.and_then(|d| d.policy.clone()))) }
    fn channel_page0_mask(&self, f: &mut dyn FnMut(Option<&[u8]>) -> Result<(), Error>) -> Result<(), Error> {
        self.diag(|d| f(d.and_then(|d| d.mask.as_ref().map(|m| m.as_slice()))))
    }
    fn operational_dataset_components(&self, f: &mut dyn FnMut(Option<&OperationalDatasetComponents>) -> Result<(), Error>) -> Result<(), Error> {
        self.diag(|d| f(d.map(|d| &d.components)))
    }
    fn active_network_faults_list(&self, f: &mut dyn FnMut(NetworkFaultEnum) -> Result<(), Error>) -> Result<(), Error> {
        self.diag(|d| { if d.is_some_and(|d| d.role == RoutingRoleEnum::Unassigned) { f(NetworkFaultEnum::LinkDown)?; } Ok(()) })
    }
    fn mac_counters(&self, f: &mut dyn FnMut(Option<&MacCounters>) -> Result<(), Error>) -> Result<(), Error> {
        self.diag(|d| f(d.map(|d| &d.mac)))
    }
}

fn mac_counters(values: &[u32]) -> Result<MacCounters, Error> {
    let v: &[u32; 34] = values.try_into().map_err(|_| invalid())?;
    Ok(MacCounters {
        tx_total_count: v[0], tx_unicast_count: v[1], tx_broadcast_count: v[2], tx_ack_requested_count: v[3], tx_acked_count: v[4],
        tx_no_ack_requested_count: v[5], tx_data_count: v[6], tx_data_poll_count: v[7], tx_beacon_count: v[8], tx_beacon_request_count: v[9],
        tx_other_count: v[10], tx_retry_count: v[11], tx_direct_max_retry_expiry_count: Some(v[12]), tx_indirect_max_retry_expiry_count: Some(v[13]),
        tx_err_cca_count: v[14], tx_err_abort_count: v[15], tx_err_busy_channel_count: v[16], rx_total_count: v[17], rx_unicast_count: v[18],
        rx_broadcast_count: v[19], rx_data_count: v[20], rx_data_poll_count: v[21], rx_beacon_count: v[22], rx_beacon_request_count: v[23],
        rx_other_count: v[24], rx_address_filtered_count: v[25], rx_dest_addr_filtered_count: v[26], rx_duplicated_count: v[27],
        rx_err_no_frame_count: v[28], rx_err_unknown_neighbor_count: v[29], rx_err_invalid_src_addr_count: v[30], rx_err_sec_count: v[31],
        rx_err_fcs_count: v[32], rx_err_other_count: v[33],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rs_matter::dm::clusters::wifi_diag::WirelessDiag;

    fn response() -> serde_json::Value {
        serde_json::json!({
            "role": 5, "channel": 20, "ext_address": "0102030405060708", "rloc16": 0x5400,
            "leader": [12345, 64, 8, 7, 21], "policy": [672, 31],
            "dataset": "00030000140102abcd020801020304050607080304746573740708fd000000000000003506000407fff800",
            "components": 4093,
            "neighbors": [["1020304050607080", 12, 0x5401, 123, 456, 3, -65, 127, 1, 2, 0, 0, 0, 1]],
            "routes": [["1122334455667788", 0x5800, 22, 22, 1, 3, 2, 9, 1, 1]],
            "mac": (0..34).collect::<Vec<u32>>()
        })
    }

    #[test]
    fn snapshot_preserves_public_network_details_and_matter_bit_order() {
        let snapshot = Snapshot::parse(&format!("> {}\r\nDone\r\n", response())).unwrap();
        assert!(snapshot.connected());
        assert_eq!(snapshot.ext_address, 0x0102030405060708);
        assert_eq!(snapshot.name.as_deref(), Some("test"));
        assert_eq!(snapshot.pan, Some(0xabcd));
        assert_eq!(snapshot.ext_pan, Some(0x0102030405060708));
        assert_eq!(snapshot.prefix, Some([0xfd, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(snapshot.mask, Some([0, 0x1f, 0xff, 0xe0]));
        assert_eq!(snapshot.policy.unwrap().flags, 31);
        assert_eq!(snapshot.neighbors[0].last_rssi, None);
        assert_eq!(snapshot.neighbors[0].average_rssi, Some(-65));
        assert!(snapshot.neighbors[0].is_child);
        assert_eq!(snapshot.routes[0].router_id, 22);
        assert_eq!(snapshot.mac.rx_err_other_count, 33);
        assert!(snapshot.components.master_key_present);
        assert!(!snapshot.components.pending_timestamp_present);
    }

    #[test]
    fn malformed_or_secret_bearing_snapshots_are_rejected() {
        for (field, value) in [
            ("role", serde_json::json!(7)),
            ("mac", serde_json::json!([1,2])),
            ("dataset", serde_json::json!("0510000102030405060708090a0b0c0d0e0f")),
            ("dataset", serde_json::json!("030874657374")),
            ("ext_address", serde_json::json!("001122")),
        ] {
            let mut wire = response(); wire[field] = value;
            assert!(Snapshot::parse(&wire.to_string()).is_err(), "{field}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn daemon_snapshots_reach_matter_and_failures_clear_stale_data() {
        let (controller, server) = super::super::tests::mock_cli(vec![
            ("nest-diag".into(), format!("{}\r\nDone\r\n", response())),
            ("nest-diag".into(), "Error 1: Failed\r\n".into()),
        ]);
        futures_lite::future::block_on(controller.refresh_diagnostics());
        assert!(controller.connected().unwrap());
        assert_eq!(controller.channel().unwrap(), Some(20));
        assert_eq!(controller.routing_role().unwrap(), Some(RoutingRoleEnum::Router));
        assert_eq!(controller.partition_id().unwrap(), Some(12345));
        controller.neighbor_table(&mut |n| { assert_eq!(n.rloc16, 0x5401); Ok(()) }).unwrap();
        controller.route_table(&mut |r| { assert_eq!(r.rloc16, 0x5800); Ok(()) }).unwrap();
        futures_lite::future::block_on(controller.refresh_diagnostics());
        assert!(!controller.connected().unwrap());
        assert_eq!(controller.channel().unwrap(), None);
        server.join().unwrap();
    }
}
