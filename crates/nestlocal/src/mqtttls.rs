//! MQTT over TLS (-mqtt-tls, -mqtt-ca, or an ssl://, tls:// or mqtts://
//! -mqtt). The unit's own trust store,
//! /etc/ssl/certs, holds only Nest's roots (and nestlocal's CA), so a broker
//! is verified against -mqtt-ca: a private CA's certificate, or for a broker
//! with a public certificate its root (ISRG Root X1 for Let's Encrypt).
//! TLS also needs the unit's clock right: a certificate is not valid
//! before it is issued.

use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};

use crate::mqtt::Broker;

/// The broker -mqtt names and, when it is dialled over TLS, how: with
/// -mqtt-tls or -mqtt-ca, or when the URL's scheme is a TLS one.
pub fn broker(url: &str, tls: bool, ca_file: &str) -> Result<(Broker, Option<Arc<ClientConfig>>), String> {
    let url = if tls || !ca_file.is_empty() { tls_broker(url) } else { url.into() };
    let broker = Broker::parse(&url).map_err(|e| format!("mqtt: {e}"))?;
    let config = if broker.tls { Some(mqtt_tls_config(ca_file)?) } else { None };
    Ok((broker, config))
}

/// Makes a tcp:// or mqtt:// broker URL an ssl:// or mqtts:// one, which is
/// dialled over TLS. A URL naming another scheme stays as it is.
pub fn tls_broker(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("tcp://") {
        return format!("ssl://{rest}");
    }
    if let Some(rest) = url.strip_prefix("mqtt://") {
        return format!("mqtts://{rest}");
    }
    if !url.contains("://") {
        return format!("ssl://{url}");
    }
    url.into()
}

/// Trusts the certificates in ca_file, or with none the system's roots.
/// TLS 1.2 at least, as rustls always is.
pub fn mqtt_tls_config(ca_file: &str) -> Result<Arc<ClientConfig>, String> {
    let mut roots = RootCertStore::empty();
    if ca_file.is_empty() {
        roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    } else {
        let pem = std::fs::read(ca_file).map_err(|e| format!("{ca_file}: {e}"))?;
        let certs: Vec<_> = CertificateDer::pem_slice_iter(&pem).filter_map(Result::ok).collect();
        if certs.is_empty() {
            return Err(format!("{ca_file}: no PEM certificate"));
        }
        let (added, _) = roots.add_parsable_certificates(certs);
        if added == 0 {
            return Err(format!("{ca_file}: no usable certificate"));
        }
    }
    Ok(Arc::new(ClientConfig::builder().with_root_certificates(roots).with_no_client_auth()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_brokers() {
        for (input, want) in [
            ("tcp://mqtt.lan:8883", "ssl://mqtt.lan:8883"),
            ("mqtt.lan:8883", "ssl://mqtt.lan:8883"),
            ("mqtt://mqtt.lan:8883", "mqtts://mqtt.lan:8883"),
            ("ssl://mqtt.lan:8883", "ssl://mqtt.lan:8883"),
            ("wss://mqtt.lan/mqtt", "wss://mqtt.lan/mqtt"),
        ] {
            assert_eq!(tls_broker(input), want, "{input}");
        }
    }

    #[test]
    fn tls_from_the_url_or_the_flags() {
        crate::install_crypto();
        for (url, flag, want) in [
            ("tcp://mqtt.lan:1883", false, false),
            ("mqtt://mqtt.lan:1883", false, false),
            ("ssl://mqtt.lan:8883", false, true),
            ("tls://mqtt.lan:8883", false, true),
            ("mqtts://mqtt.lan:8883", false, true),
            ("tcp://mqtt.lan:8883", true, true),
            ("mqtt://mqtt.lan:8883", true, true),
        ] {
            let (b, tls) = broker(url, flag, "").unwrap();
            assert_eq!((b.tls, tls.is_some()), (want, want), "{url} -mqtt-tls={flag}");
        }
        assert!(broker("wss://mqtt.lan/mqtt", false, "").unwrap_err().starts_with("mqtt: "));
    }

    #[test]
    fn tls_config_from_a_ca_file() {
        crate::install_crypto();
        let dir = std::env::temp_dir().join(format!("nestlocal-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ca = dir.join("ca.pem");
        std::fs::write(&ca, include_bytes!("testdata/ca.pem")).unwrap();
        assert!(mqtt_tls_config(ca.to_str().unwrap()).is_ok(), "with a CA");
        assert!(mqtt_tls_config("").is_ok(), "without a CA");
        let bad = dir.join("bad.pem");
        std::fs::write(&bad, "not a certificate").unwrap();
        assert!(mqtt_tls_config(bad.to_str().unwrap()).is_err(), "a file with no certificate accepted");
    }
}
