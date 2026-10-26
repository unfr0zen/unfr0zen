//! Matter DNS-SD registration through a dedicated OpenThread daemon on thread0.
//! The radio, dataset, interface and IPv6 routes are managed by the launcher.

use std::time::Duration;

use rs_matter::{error::{Error, ErrorCode}, Matter};

use crate::logf;

#[derive(Clone, Debug, PartialEq)]
struct Service {
    name: String,
    kind: String,
    add: String,
}

fn services(matter: &Matter<'_>) -> Result<Vec<Service>, Error> {
    let mut descriptors = Vec::new();
    matter.mdns_services(|service| { descriptors.push(service); Ok(()) })?;
    let mut result = Vec::new();
    for service in descriptors {
        let mut buffer = [0; 1024];
        let (service, _) = service.service(matter, &mut buffer)?;
        let mut kind = service.service_protocol.to_string();
        for subtype in service.service_subtypes { kind.push(','); kind.push_str(subtype); }
        let mut txt = String::new();
        for (key, value) in service.txt_kvs {
            let entry = format!("{key}={value}");
            if entry.len() > 255 { return Err(ErrorCode::NoSpace.into()); }
            txt.push_str(&format!("{:02x}", entry.len()));
            for byte in entry.bytes() { txt.push_str(&format!("{byte:02x}")); }
        }
        result.push(Service {
            name: service.name.to_string(),
            kind: service.service_protocol.to_string(),
            add: format!("srp client service add {} {kind} {} 0 0 {txt}", service.name, service.port),
        });
    }
    Ok(result)
}

pub(super) async fn command(line: &str) -> std::io::Result<String> {
    command_to(std::path::Path::new("/run/openthread-thread0.sock"), line).await
}

#[cfg(unix)]
pub(super) async fn command_to(path: &std::path::Path, line: &str) -> std::io::Result<String> {
    use futures_lite::io::{AsyncReadExt, AsyncWriteExt};
    use embassy_futures::select::{select, Either};
    use std::os::unix::net::UnixStream;
    static CLI: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = CLI.lock().await;
    let request = async {
        let mut socket = async_io::Async::<UnixStream>::connect(path).await?;
        socket.write_all(format!("{line}\n").as_bytes()).await?;
        let mut output = Vec::new();
        loop {
            let mut buffer = [0; 512];
            let n = socket.read(&mut buffer).await?;
            if n == 0 { return Err(std::io::ErrorKind::UnexpectedEof.into()); }
            output.extend_from_slice(&buffer[..n]);
            let text = String::from_utf8_lossy(&output);
            if text.contains("Done\r\n") { return Ok(text.into_owned()); }
            let limit = if line == "nest-diag" { 32768 } else { 8192 };
            if text.contains("Error ") || output.len() > limit {
                return Err(std::io::Error::other(text.into_owned()));
            }
        }
    };
    let timeout = if line == "nest-discover" { 20 } else { 3 };
    match select(request, async_io::Timer::after(Duration::from_secs(timeout))).await {
        Either::First(result) => result,
        Either::Second(_) => Err(std::io::ErrorKind::TimedOut.into()),
    }
}

#[cfg(not(unix))]
pub(super) async fn command_to(_: &std::path::Path, _: &str) -> std::io::Result<String> {
    Err(std::io::ErrorKind::Unsupported.into())
}

fn obsolete_services<'a>(listing: &'a str, wanted: &[Service]) -> Vec<(&'a str, &'a str)> {
    listing.lines().filter_map(|line| {
        let line = line.trim_start_matches("> ").strip_prefix("instance:\"")?;
        let (name, line) = line.split_once("\", name:\"")?;
        let (kind, _) = line.split_once('"')?;
        let kind = kind.split(',').next()?;
        (matches!(kind, "_matter._tcp" | "_matterc._udp") &&
         !wanted.iter().any(|s| s.name == name && s.kind == kind)).then_some((name, kind))
    }).collect()
}

/// IPv6 addresses the Thread stack owns and can advertise through SRP.
fn thread_addresses(listing: &str) -> Vec<std::net::Ipv6Addr> {
    listing.lines().filter_map(|line| line.trim().trim_start_matches("> ").parse::<std::net::Ipv6Addr>().ok())
        .filter(|address| !address.is_unspecified() && !address.is_multicast() && !address.is_unicast_link_local())
        .collect()
}

async fn sync_addresses() -> std::io::Result<()> {
    let listing = command("ipaddr").await?;
    let local = if_addrs::get_if_addrs()?;
    for address in thread_addresses(&listing) {
        if local.iter().any(|iface| iface.name == "thread0" && iface.ip() == std::net::IpAddr::V6(address)) { continue; }
        let status = std::process::Command::new("ip")
            .args(["-6", "addr", "add", &format!("{address}/128"), "dev", "thread0"])
            .status()?;
        if !status.success() { return Err(std::io::Error::other("cannot assign Thread address to Linux")); }
    }
    Ok(())
}

pub async fn run(matter: &Matter<'_>, serial: &str) -> Result<(), Error> {
    // DNS labels and CLI tokens share this deliberately restricted alphabet.
    if serial.is_empty() || serial.len() > 40 || !serial.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(ErrorCode::Invalid.into());
    }
    let hostname = format!("unfr0zen-thread-{serial}");
    let mut registered = Vec::<Service>::new();
    loop {
        let wanted = services(matter)?;
        let update = async {
            sync_addresses().await?;
            let host = command("srp client host").await?;
            if !host.contains(&format!("name:\"{hostname}\"")) {
                command(&format!("srp client host name {hostname}")).await?;
                command("srp client host address auto").await?;
                command("srp client autostart enable").await?;
                registered.clear();
            }
            let listing = command("srp client service").await?;
            for (name, kind) in obsolete_services(&listing, &wanted) {
                command(&format!("srp client service remove {name} {kind}")).await?;
            }
            for service in &wanted {
                let present = listing.contains(&format!("instance:\"{}\"", service.name));
                if !present || !registered.contains(service) {
                    if present {
                        command(&format!("srp client service clear {} {}", service.name, service.kind)).await?;
                    }
                    command(&service.add).await?;
                    logf!("matter: Thread SRP service {} {}", service.name, service.kind);
                }
            }
            Ok::<(), std::io::Error>(())
        }.await;
        match update {
            Ok(()) => registered = wanted,
            Err(error) => logf!("matter: Thread SRP: {error}; retrying"),
        }
        async_io::Timer::after(Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rs_matter::crypto::default_crypto;
    use rs_matter::dm::{AttrChangeNotifier, clusters::basic_info::BasicInfoConfig};
    use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT};
    use rs_matter::sc::pase::{Spake2pVerifierPassword, Spake2pVerifierPasswordRef};
    use rs_matter::BasicCommData;

    struct Notify;
    impl AttrChangeNotifier for Notify {
        fn notify_attr_changed(&self, _: u16, _: u32, _: u32) {}
        fn notify_cluster_changed(&self, _: u16, _: u32) {}
        fn notify_endpoint_changed(&self, _: u16) {}
        fn notify_all_changed(&self) {}
    }

    #[test]
    fn srp_addresses_exclude_cli_text_link_local_and_multicast() {
        assert_eq!(thread_addresses("> fd00::1234\r\nfe80::1\r\nff03::1\r\n::\r\nDone\r\n"),
                   vec!["fd00::1234".parse::<std::net::Ipv6Addr>().unwrap()]);
    }

    #[test]
    fn removes_stale_pairing_records_but_keeps_active_fabrics_and_other_services() {
        let listing = "> instance:\"A\", name:\"_matterc._udp,_L3840\", state:Registered\r\n\
                       instance:\"B\", name:\"_matter._tcp,_IFABRIC\", state:Registered\r\n\
                       instance:\"C\", name:\"_http._tcp\", state:Registered\r\nDone\r\n";
        let wanted = vec![Service { name: "B".into(), kind: "_matter._tcp".into(), add: String::new() }];
        assert_eq!(obsolete_services(listing, &wanted), vec![("A", "_matterc._udp")]);
        assert!(obsolete_services("Done\r\n", &wanted).is_empty());
    }

    #[test]
    fn commissioning_service_tracks_window_and_encodes_txt_for_cli() {
        let info = BasicInfoConfig { device_name: "Nest Thermostat", ..BasicInfoConfig::new() };
        let comm = BasicCommData {
            password: Spake2pVerifierPassword::new_from_ref(Spake2pVerifierPasswordRef::new(&20202021u32.to_le_bytes())),
            discriminator: 3840,
        };
        let matter = Matter::new(&info, comm, &TEST_DEV_ATT, 5540);
        assert!(services(&matter).unwrap().is_empty());
        matter.open_basic_comm_window(900, default_crypto(rand::rng(), DAC_PRIVKEY), &Notify).unwrap();
        let announced = services(&matter).unwrap();
        assert_eq!(announced.len(), 1);
        assert_eq!(announced[0].kind, "_matterc._udp");
        let command = &announced[0].add;
        assert!(command.len() < 512);
        let hex = command.split_whitespace().last().unwrap();
        let txt: Vec<u8> = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i+2], 16).unwrap()).collect();
        assert!(txt.windows(19).any(|bytes| bytes == b"\x12DN=Nest Thermostat"));
        matter.close_comm_window(&Notify).unwrap();
        assert!(services(&matter).unwrap().is_empty());
    }
}
