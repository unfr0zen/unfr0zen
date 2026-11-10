//! The unit's network interfaces, for Matter's General Diagnostics: what
//! rs-matter's own give, which come with its `os` feature, and with it a
//! clock that stops while the unit sleeps (timer.rs).

use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use rs_matter::dm::clusters::gen_diag::{InterfaceTypeEnum, NetifDiag, NetifInfo};
use rs_matter::dm::networks::NetChangeNotif;
use rs_matter::error::Error;
use rs_matter::utils::sync::DynBase;

pub struct Netifs;

struct Netif {
    name: String,
    hw_addr: [u8; 8],
    ipv4: Vec<Ipv4Addr>,
    ipv6: Vec<Ipv6Addr>,
    operational: bool,
    index: u32,
}

/// The interfaces in /sys/class/net, with their addresses.
fn netifs() -> Vec<Netif> {
    let ipv4 = if_addrs::get_if_addrs().unwrap_or_default();
    let ipv6 = std::fs::read_to_string("/proc/net/if_inet6").unwrap_or_default();
    let mut names: Vec<String> = std::fs::read_dir("/sys/class/net").into_iter().flatten().flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names.into_iter().map(|name| {
        let read = |file: &str| std::fs::read_to_string(format!("/sys/class/net/{name}/{file}")).unwrap_or_default();
        let mut hw_addr = [0; 8];
        for (byte, hex) in hw_addr.iter_mut().zip(read("address").trim().split(':')) {
            *byte = u8::from_str_radix(hex, 16).unwrap_or(0);
        }
        // address, index, prefix length, scope, flags, name (all hex but the name)
        let ipv6 = ipv6.lines().filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            (f.get(5) == Some(&name.as_str())).then(|| u128::from_str_radix(f[0], 16).ok().map(Ipv6Addr::from)).flatten()
        }).collect();
        Netif {
            ipv4: ipv4.iter().filter(|a| a.name == name).filter_map(|a| match a.addr {
                if_addrs::IfAddr::V4(ref v4) => Some(v4.ip),
                _ => None,
            }).collect(),
            ipv6,
            operational: read("operstate").trim() == "up",
            index: read("ifindex").trim().parse().unwrap_or(0),
            hw_addr,
            name,
        }
    }).collect()
}

impl DynBase for Netifs {}

impl NetifDiag for Netifs {
    fn netifs(&self, f: &mut dyn FnMut(&NetifInfo) -> Result<(), Error>) -> Result<(), Error> {
        for netif in netifs() {
            f(&NetifInfo {
                name: &netif.name,
                operational: netif.operational,
                offprem_svc_reachable_ipv4: None,
                offprem_svc_reachable_ipv6: None,
                hw_addr: &netif.hw_addr,
                ipv4_addrs: &netif.ipv4,
                ipv6_addrs: &netif.ipv6,
                netif_type: InterfaceTypeEnum::Unspecified,
                netif_index: netif.index,
            })?;
        }
        Ok(())
    }
}

impl NetChangeNotif for Netifs {
    /// Callers read the interfaces again and compare: this only paces them.
    async fn wait_changed(&self) {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(Duration::from_secs(5).as_secs())).await;
    }
}
