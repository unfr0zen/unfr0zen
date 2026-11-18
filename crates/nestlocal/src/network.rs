//! Whether the unit can look names up. It resolves them through connman's
//! DNS proxy (127.0.0.1 in /etc/resolv.conf), which has no server to ask
//! until one of connman's services is ready with DNS servers, the WiFi once
//! it has joined and has its DHCP lease: until then every lookup fails.
//! nestlocal starts while the WiFi is still joining, and a unit can wake
//! with its link down, so lookups wait for such a service rather than fail.
//! connman's own State does not tell: the radio's PAN (lowpan) is ready
//! long before the WiFi, with no DNS servers.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::watch;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::logf;

const CONNMAN: &str = "net.connman";
const MANAGER: &str = "net.connman.Manager";
const SERVICE: &str = "net.connman.Service";
/// connman answers at once; one that does not must not hold up the start.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

static UP: OnceLock<watch::Receiver<bool>> = OnceLock::new();

/// What matters of one of connman's services.
#[derive(Debug, PartialEq)]
struct Service {
    kind: String,
    state: String,
    nameservers: usize,
}

impl Service {
    fn from_properties(properties: &HashMap<String, OwnedValue>) -> Service {
        let text = |key| properties.get(key).and_then(|v| String::try_from(v.clone()).ok()).unwrap_or_default();
        let nameservers = match properties.get("Nameservers").map(|v| &**v) {
            Some(Value::Array(servers)) => servers.len(),
            _ => 0,
        };
        Service { kind: text("Type"), state: text("State"), nameservers }
    }
}

/// The service lookups go through: ready ("online" once connman's check of
/// the internet passed) and with DNS servers.
fn serving(services: &[Service]) -> Option<&Service> {
    services.iter().find(|s| matches!(s.state.as_str(), "ready" | "online") && s.nameservers > 0)
}

async fn services(bus: &zbus::Connection) -> Result<Vec<Service>, String> {
    let reply = bus.call_method(Some(CONNMAN), "/", Some(MANAGER), "GetServices", &());
    let reply = tokio::time::timeout(CALL_TIMEOUT, reply)
        .await
        .map_err(|_| "connman did not answer".to_string())?
        .map_err(|e| format!("connman: {e}"))?;
    let list: Vec<(OwnedObjectPath, HashMap<String, OwnedValue>)> =
        reply.body().deserialize().map_err(|e| format!("connman: {e}"))?;
    Ok(list.iter().map(|(_, properties)| Service::from_properties(properties)).collect())
}

fn describe(services: &[Service]) -> String {
    match serving(services) {
        Some(s) => format!("up ({}, {})", s.kind, s.state),
        None => "down: no connman service is ready with DNS servers".into(),
    }
}

/// connman's signals about its manager or its services.
async fn signals(bus: &zbus::Connection, interface: &'static str) -> Option<zbus::MessageStream> {
    let rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(interface).ok()?.build();
    zbus::MessageStream::for_match_rule(rule, bus, None).await.ok()
}

/// Follows connman's services from now on. Without connman on the system
/// bus, lookups do not wait.
pub async fn start() {
    let Ok(bus) = zbus::Connection::system().await else {
        return;
    };
    // Before asking, so that no change between the answer and the
    // subscription is missed.
    let (Some(mut manager), Some(mut service)) = (signals(&bus, MANAGER).await, signals(&bus, SERVICE).await) else {
        return;
    };
    let first = match services(&bus).await {
        Ok(first) => first,
        Err(e) => return logf!("network: {e}"),
    };
    logf!("network: {}", describe(&first));
    let (tx, rx) = watch::channel(serving(&first).is_some());
    let _ = UP.set(rx);
    tokio::spawn(async move {
        use futures_lite::StreamExt;
        loop {
            tokio::select! {
                Some(_) = manager.next() => {}
                Some(_) = service.next() => {}
                else => return,
            }
            let Ok(now) = services(&bus).await else {
                continue;
            };
            let up = serving(&now).is_some();
            if up != *tx.borrow() {
                logf!("network: {}", describe(&now));
                tx.send_replace(up);
            }
        }
    });
}

/// Returns once a lookup can be answered.
pub async fn up() {
    if let Some(rx) = UP.get() {
        let _ = rx.clone().wait_for(|up| *up).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(kind: &str, state: &str, nameservers: usize) -> Service {
        Service { kind: kind.into(), state: state.into(), nameservers }
    }

    #[test]
    fn lookups_go_through_a_ready_service_with_dns_servers() {
        // At start: the radio's PAN is ready, the WiFi still joining.
        let starting = [service("lowpan", "ready", 0), service("wifi", "association", 0)];
        assert_eq!(serving(&starting), None);
        // Joined, before its lease.
        assert_eq!(serving(&[service("lowpan", "ready", 0), service("wifi", "configuration", 0)]), None);
        for state in ["ready", "online"] {
            let joined = [service("lowpan", "ready", 0), service("wifi", state, 1)];
            assert_eq!(serving(&joined), Some(&joined[1]), "{state}");
        }
        assert_eq!(serving(&[service("wifi", "idle", 1), service("wifi", "failure", 2)]), None);
    }

    #[test]
    fn a_service_reads_from_connmans_properties() {
        let mut properties = HashMap::new();
        properties.insert("Type".to_string(), OwnedValue::try_from(Value::from("wifi")).unwrap());
        properties.insert("State".to_string(), OwnedValue::try_from(Value::from("online")).unwrap());
        properties.insert(
            "Nameservers".to_string(),
            OwnedValue::try_from(Value::from(vec!["10.0.0.1".to_string(), "10.0.0.2".to_string()])).unwrap(),
        );
        assert_eq!(Service::from_properties(&properties), service("wifi", "online", 2));
        properties.insert("Nameservers".to_string(), OwnedValue::try_from(Value::from(Vec::<String>::new())).unwrap());
        properties.remove("State");
        assert_eq!(Service::from_properties(&properties), service("wifi", "", 0));
    }

    #[tokio::test]
    async fn without_connman_lookups_do_not_wait() {
        tokio::time::timeout(Duration::from_secs(1), up()).await.expect("up() waited with no connman");
    }
}
