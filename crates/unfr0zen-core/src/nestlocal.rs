//! The nestlocal overlay: everything nestlocal adds to or changes in a root
//! volume. Port of ha/build.py and ha/gencert.
//!
//! A unit that already has nestlocal keeps its own certificate, key and CA
//! when they belong together (the CA in its bundle signed the certificate,
//! and the key matches it), so what the unit trusts and serves stays the same
//! whichever host or tool patches it. The bundles end with exactly one
//! nestlocal CA: ones an earlier build or the desktop app added go.

use std::str::FromStr;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use rsa::pkcs1::{DecodeRsaPrivateKey, EncodeRsaPrivateKey};
use rsa::pkcs1v15::{Signature, SigningKey, VerifyingKey};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePublicKey};
use rsa::signature::Verifier;
use rsa::{RsaPrivateKey, RsaPublicKey};
use sha2::Sha256;
use x509_cert::builder::{Builder, CertificateBuilder, Profile};
use x509_cert::der::asn1::{GeneralizedTime, Ia5String, ObjectIdentifier, OctetString, UtcTime};
use x509_cert::der::pem::LineEnding;
use x509_cert::der::{Decode, DecodePem, Encode, EncodePem};
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{ExtendedKeyUsage, SubjectAltName};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::SubjectPublicKeyInfoOwned;
use x509_cert::time::{Time, Validity};
use x509_cert::Certificate;

use crate::{device, Result};

pub const STOCK_URL: &str = "https://frontdoor.nest.com/entry";
pub const LOCAL_URL: &str = "https://127.0.0.1/entry";
const RCS_BEFORE: &str = "${INITDIR}/nestlabs start\n";
const RCS_LINE: &str = "${INITDIR}/nestlocal start\n";
const MONIT_LINE: &str = "include /etc/monit.d/nestlocal.monitrc\n";
const WPANTUND_LINE: &str = "include /etc/monit.d/wpantund.monitrc\n";
const WITH_WPAN: &str = "NETWORKING_TECHNOLOGIES=\"common wifi wpan\"\n";
const WITHOUT_WPAN: &str = "NETWORKING_TECHNOLOGIES=\"common wifi\"\n";
pub const PASSWORD_FILE: &str = "etc/nestlocal/mqtt.password";
pub const CA_FILE: &str = "etc/nestlocal/mqtt-ca.pem";
const INIT: &[u8] = include_bytes!("../../nestlocal/rootfs/nestlocal.init");
const MONITRC: &[u8] = include_bytes!("../../nestlocal/rootfs/nestlocal.monitrc");

/// The stock files the overlay edits, as the unit has them.
/// nestlocal's options; present once nestlocal is installed.
pub const OPTIONS: &str = "etc/nestlocal/options";
pub const BLE_FIRMWARE: &str = "usr/share/dialog/data/firmware/nl_ble_peripheral_app_firmware.bin";
pub const STOCK_FILES: [&str; 6] = ["etc/init.d/rcS", "etc/monitrc", "etc/init.d/networking-defines",
                                    "etc/nestlabs/client.config", "etc/nestlabs/product.config",
                                    "etc/ssl/certs/ca-bundle.pem"];
/// The bundle nlclient trusts once unlocked; a j49 has none.
pub const UNLOCK_BUNDLE: &str = "etc/ssl/certs/ca-unlock-bundle.pem";

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid pattern")
}

/// A file an overlay puts in a volume: (name, mode, contents).
pub type NamedFile = (String, u32, Vec<u8>);

/// The broker unfr0zen --mqtt and its options name.
#[derive(Clone, Debug, Default)]
pub struct Mqtt {
    pub url: String,
    pub user: Option<String>,
    pub password: Option<String>,
    pub tls: bool,
    pub ca: Option<Vec<u8>>,
}

/// host, host:port or tcp://host[:port], as the tcp:// URL nestlocal takes.
pub fn mqtt_url(value: &str) -> Result<String> {
    static URL: LazyLock<Regex> = LazyLock::new(|| regex(r"^(?:tcp://)?([A-Za-z0-9.-]+)(?::([0-9]{1,5}))?$"));
    let Some(found) = URL.captures(value) else {
        return device(format!("not a tcp:// MQTT broker: {value}"));
    };
    if let Some(port) = found.get(2).filter(|port| !matches!(port.as_str().parse::<u16>(), Ok(1..))) {
        return device(format!("not a TCP port: {}", port.as_str()));
    }
    Ok(format!("tcp://{}{}", &found[1], found.get(2).map_or(String::new(), |port| format!(":{}", port.as_str()))))
}

/// The Matter pairing unfr0zen --matter sets up: nestlocal's
/// -matter-passcode and -matter-discriminator, and the code a controller
/// is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Matter {
    pub passcode: u32,
    pub discriminator: u16,
}

/// A passcode Matter accepts: 1 to 99999998, none of the trivial ones.
pub fn check_matter_passcode(passcode: u32) -> Result<()> {
    let trivial = [11111111, 22222222, 33333333, 44444444, 55555555, 66666666, 77777777, 88888888,
                   12345678, 87654321];
    if !(1..=99_999_998).contains(&passcode) || trivial.contains(&passcode) {
        return device(format!("{passcode} is not a passcode Matter accepts (1-99999998, not a trivial one)"));
    }
    Ok(())
}

/// The pairing code unfr0zen --matter gives a unit unless told another.
pub const DEFAULT_MATTER_CODE: &str = "0347-100-5534";

impl Matter {
    /// The pairing for an 11-digit manual pairing code (dashes and spaces
    /// allowed): the passcode in it, and a discriminator with the top 4 bits
    /// it carries, so the unit answers to exactly that code.
    pub fn from_pairing_code(code: &str) -> Result<Matter> {
        let digits: String = code.chars().filter(|c| !matches!(c, '-' | ' ')).collect();
        let bad = || device(format!("{code:?} is not an 11-digit Matter pairing code"));
        if digits.len() != 11 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return bad();
        }
        if verhoeff(&digits[..10]) != digits.as_bytes()[10] - b'0' {
            return device(format!("{code:?}: the check digit does not match; a digit is mistyped"));
        }
        let first = u32::from(digits.as_bytes()[0] - b'0');
        let (middle, last): (u32, u32) = (digits[1..6].parse().unwrap(), digits[6..10].parse().unwrap());
        if first >> 2 != 0 || middle >= 1 << 16 || last >= 1 << 14 {
            return bad(); // a code with vendor and product IDs in it (21 digits), or out of range
        }
        let passcode = (last << 14) | (middle & 0x3fff);
        check_matter_passcode(passcode)?;
        let short = ((first & 3) << 2) | (middle >> 14);
        Ok(Matter { passcode, discriminator: (short << 8) as u16 })
    }

    /// The 11-digit manual pairing code, as controllers ask for it
    /// (1234-567-8901): the discriminator's top 4 bits, the passcode, and a
    /// Verhoeff check digit.
    pub fn pairing_code(&self) -> String {
        let short = u32::from(self.discriminator >> 8);
        let digits = format!("{}{:05}{:04}", short >> 2, ((short & 3) << 14) | (self.passcode & 0x3fff),
                             self.passcode >> 14);
        let code = format!("{digits}{}", verhoeff(&digits));
        format!("{}-{}-{}", &code[..4], &code[4..7], &code[7..])
    }

    fn flags(&self) -> String {
        format!(" -matter -matter-passcode {} -matter-discriminator {}", self.passcode, self.discriminator)
    }
}

/// The Verhoeff check digit of a string of digits.
fn verhoeff(digits: &str) -> u8 {
    const D: [[u8; 10]; 10] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9], [1, 2, 3, 4, 0, 6, 7, 8, 9, 5], [2, 3, 4, 0, 1, 7, 8, 9, 5, 6],
        [3, 4, 0, 1, 2, 8, 9, 5, 6, 7], [4, 0, 1, 2, 3, 9, 5, 6, 7, 8], [5, 9, 8, 7, 6, 0, 4, 3, 2, 1],
        [6, 5, 9, 8, 7, 1, 0, 4, 3, 2], [7, 6, 5, 9, 8, 2, 1, 0, 4, 3], [8, 7, 6, 5, 9, 3, 2, 1, 0, 4],
        [9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
    ];
    const P: [[u8; 10]; 8] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9], [1, 5, 7, 6, 2, 8, 3, 0, 9, 4], [5, 8, 0, 3, 7, 9, 6, 1, 4, 2],
        [8, 9, 1, 6, 0, 4, 3, 5, 2, 7], [9, 4, 5, 3, 1, 2, 6, 8, 7, 0], [4, 2, 8, 6, 5, 7, 3, 9, 0, 1],
        [2, 7, 9, 3, 8, 0, 6, 4, 1, 5], [7, 0, 4, 6, 9, 1, 3, 2, 5, 8],
    ];
    const INV: [u8; 10] = [0, 4, 3, 2, 1, 5, 6, 7, 8, 9];
    let c = digits.bytes().rev().enumerate()
        .fold(0u8, |c, (i, b)| D[c as usize][P[(i + 1) % 8][(b - b'0') as usize] as usize]);
    INV[c as usize]
}

/// nestlocal's options file: MQTT's options (or none: nestlocal's own
/// default broker is not this unit's), Matter's and Zigbee's, and the files
/// they name.
pub fn options(mqtt: Option<&Mqtt>, matter: Option<&Matter>, zigbee: bool) -> Result<(Vec<u8>, Vec<NamedFile>)> {
    let (mut line, files) = match mqtt {
        Some(mqtt) => {
            let (options, files) = mqtt_options(mqtt)?;
            (String::from_utf8_lossy(&options).trim_end().to_string(), files)
        }
        None => ("-mqtt=".to_string(), Vec::new()),
    };
    if let Some(matter) = matter {
        line += &matter.flags();
    }
    if zigbee {
        line += " -zigbee";
    }
    Ok((format!("{line}\n").into_bytes(), files))
}

/// The options line, and the password and CA files' entries, if any.
pub fn mqtt_options(mqtt: &Mqtt) -> Result<(Vec<u8>, Vec<NamedFile>)> {
    static USER: LazyLock<Regex> = LazyLock::new(|| regex(r"^[A-Za-z0-9._@+-]+$"));
    let tls = mqtt.tls || mqtt.ca.is_some();
    let broker = mqtt_url(&mqtt.url)?;
    let url = if broker.matches(':').count() == 2 { broker } else { format!("{broker}:{}", if tls { 8883 } else { 1883 }) };
    // The init script word-splits the options: the user name must be one plain word.
    if let Some(user) = &mqtt.user {
        if !USER.is_match(user) {
            return device(format!("MQTT user name must be a plain word: {user:?}"));
        }
    }
    if let Some(password) = &mqtt.password {
        if password.contains('\n') || mqtt.user.is_none() {
            return device("MQTT password needs a user name, on one line");
        }
    }
    let mut options = format!("-mqtt {url}");
    if let Some(user) = &mqtt.user {
        options += &format!(" -mqtt-user {user}");
    }
    let mut files = Vec::new();
    if let Some(password) = &mqtt.password {
        options += &format!(" -mqtt-password-file /{PASSWORD_FILE}");
        files.push((PASSWORD_FILE.to_string(), 0o600, format!("{password}\n").into_bytes()));
    }
    if tls {
        options += " -mqtt-tls";
    }
    if let Some(ca) = &mqtt.ca {
        if !ca.windows(27).any(|w| w == b"-----BEGIN CERTIFICATE-----") {
            return device("--mqtt-ca: no PEM certificate in the file");
        }
        options += &format!(" -mqtt-ca /{CA_FILE}");
        files.push((CA_FILE.to_string(), 0o644, ca.clone()));
    }
    Ok((format!("{options}\n").into_bytes(), files))
}

// Each edit also accepts its own result, so a unit patched before can be patched again.

/// cloudregisterurl names Nest's frontdoor on a stock unit, and on a j49
/// NoLongerEvil installed, its own server; either way it becomes nestlocal's.
pub fn edit_client_config(text: &str) -> Result<String> {
    static URL: LazyLock<Regex> = LazyLock::new(|| regex(r#"(<a key="cloudregisterurl" value=")[^"]*("/>)"#));
    if URL.find_iter(text).count() != 1 {
        return device("client.config: expected one cloudregisterurl");
    }
    Ok(URL.replace(text, regex::NoExpand(&format!(r#"<a key="cloudregisterurl" value="{LOCAL_URL}"/>"#))).into_owned())
}

pub fn edit_rcs(text: &str) -> Result<String> {
    if text.matches(&format!("{RCS_LINE}{RCS_BEFORE}")).count() == 1 && text.matches(RCS_LINE).count() == 1 {
        return Ok(text.to_string());
    }
    if text.contains(RCS_LINE) || text.matches(RCS_BEFORE).count() != 1 {
        return device("rcS: expected one nestlabs start line and no nestlocal line");
    }
    Ok(text.replace(RCS_BEFORE, &format!("{RCS_LINE}{RCS_BEFORE}")))
}

/// monit runs nestlocal; and, with Zigbee, no longer wpantund, which
/// would put Nest's firmware back on the radio nestlocal now has.
pub fn edit_monitrc(text: &str, zigbee: bool) -> Result<String> {
    let text = match text.matches(MONIT_LINE).count() {
        1 => text.to_string(),
        0 => format!("{text}{MONIT_LINE}"),
        _ => return device("monitrc: includes nestlocal more than once"),
    };
    Ok(match (zigbee, text.contains(WPANTUND_LINE)) {
        (true, true) => text.replace(WPANTUND_LINE, ""),
        (false, false) => text.replace(MONIT_LINE, &format!("{WPANTUND_LINE}{MONIT_LINE}")),
        _ => text,
    })
}

/// The networking script starts wpantund with the Thread interface: with
/// Zigbee, without it.
pub fn edit_networking_defines(text: &str, zigbee: bool) -> Result<String> {
    let (from, to) = if zigbee { (WITH_WPAN, WITHOUT_WPAN) } else { (WITHOUT_WPAN, WITH_WPAN) };
    if text.matches(to).count() == 1 && !text.contains(from) {
        return Ok(text.to_string());
    }
    if text.matches(from).count() != 1 {
        return device("networking-defines: expected one stock NETWORKING_TECHNOLOGIES line");
    }
    Ok(text.replace(from, to))
}

/// usephoenix 1 (6.4-8) moves the unit to Nest's newer service, which
/// nestlocal does not speak; 0 keeps it on the one it does.
pub fn edit_product_config(text: &str) -> Result<String> {
    static USEPHOENIX: LazyLock<Regex> = LazyLock::new(|| regex(r#"(<a key="usephoenix" value=")([^"]*)("/>)"#));
    if USEPHOENIX.find_iter(text).count() != 1 {
        return device("product.config: expected one usephoenix setting");
    }
    Ok(USEPHOENIX.replace(text, "${1}0${3}").into_owned())
}

fn configure_matter_ble(mut text: String, enabled: bool, firmware: Option<&[u8]>) -> String {
    const MARKER: &str = "\n<!-- unfr0zen owns BLE for Matter; original ble_disabled=0 -->";
    if !enabled && text.contains(MARKER) {
        return text.replace("key=\"ble_disabled\" value=\"1\"", "key=\"ble_disabled\" value=\"0\"")
            .replace(MARKER, "");
    }
    let supported = firmware.is_some_and(|data| crate::shell::sha256_hex(data)
        == "f1a8be89252e1fab8d15f8e060ef5d74e7c60944b5ed4100d31f2eb4f4850315");
    if enabled && supported && text.matches("key=\"ble_disabled\" value=\"0\"").count() == 1 {
        text = text.replace("key=\"ble_disabled\" value=\"0\"", "key=\"ble_disabled\" value=\"1\"");
        text.push_str(MARKER);
    }
    text
}

static CERTIFICATE: LazyLock<regex::bytes::Regex> = LazyLock::new(|| {
    regex::bytes::Regex::new(r"(?s)-----BEGIN CERTIFICATE-----.*?-----END CERTIFICATE-----\n?").expect("valid pattern")
});

/// Whether a PEM certificate is a nestlocal CA: this build's, an older one's,
/// or the app's. Its subject, not its issuer, names it: the server
/// certificate a CA signed carries the CA's name too.
pub fn nestlocal_ca(pem: &[u8]) -> bool {
    Certificate::from_pem(pem).ok().and_then(|cert| cert.tbs_certificate.subject.to_der().ok())
        .is_some_and(|der| der.windows(12).any(|w| w == b"nestlocal CA"))
}

/// The bundle with ca as its one nestlocal CA, after the stock certificates.
pub fn with_ca(bundle: &[u8], ca: &[u8]) -> Vec<u8> {
    let stock = CERTIFICATE.replace_all(bundle, |found: &regex::bytes::Captures| {
        if nestlocal_ca(&found[0]) { Vec::new() } else { found[0].to_vec() }
    });
    let mut result = stock.to_vec();
    while result.ends_with(b"\n") {
        result.pop();
    }
    result.push(b'\n');
    result.extend_from_slice(ca);
    result
}

/// nestlocal's CA, the certificate it serves on 127.0.0.1, and its key (PEM).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pki {
    pub ca: Vec<u8>,
    pub server_crt: Vec<u8>,
    pub server_key: Vec<u8>,
}

fn private_key(pem: &[u8]) -> Option<RsaPrivateKey> {
    let text = std::str::from_utf8(pem).ok()?;
    RsaPrivateKey::from_pkcs1_pem(text).ok().or_else(|| RsaPrivateKey::from_pkcs8_pem(text).ok())
}

fn public_key(spki: &SubjectPublicKeyInfoOwned) -> Option<RsaPublicKey> {
    RsaPublicKey::from_public_key_der(&spki.to_der().ok()?).ok()
}

/// Whether ca signed crt (SHA-256 with RSA) and key is crt's key.
pub fn belongs_together(ca: &[u8], crt: &[u8], key: &[u8]) -> bool {
    const SHA256_WITH_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
    let (Ok(authority), Ok(leaf)) = (Certificate::from_pem(ca), Certificate::from_pem(crt)) else { return false };
    let (Some(signer), Some(served), Some(key)) = (public_key(&authority.tbs_certificate.subject_public_key_info),
                                                   public_key(&leaf.tbs_certificate.subject_public_key_info),
                                                   private_key(key)) else { return false };
    let Ok(tbs) = leaf.tbs_certificate.to_der() else { return false };
    let Some(signature) = leaf.signature.as_bytes().and_then(|bytes| Signature::try_from(bytes).ok()) else { return false };
    leaf.signature_algorithm.oid == SHA256_WITH_RSA
        && leaf.tbs_certificate.issuer == authority.tbs_certificate.subject
        && VerifyingKey::<Sha256>::new(signer).verify(&tbs, &signature).is_ok()
        && RsaPublicKey::from(&key) == served
}

/// The unit's own CA, certificate and key, if they belong together.
pub fn unit_pki(crt: Option<&[u8]>, key: Option<&[u8]>, bundle: Option<&[u8]>) -> Option<Pki> {
    let (crt, key, bundle) = (crt?, key?, bundle?);
    CERTIFICATE.find_iter(bundle).map(|found| {
        let mut ca = found.as_bytes().trim_ascii_end().to_vec();
        ca.push(b'\n');
        ca
    }).find(|ca| nestlocal_ca(ca) && belongs_together(ca, crt, key))
        .map(|ca| Pki { ca, server_crt: crt.to_vec(), server_key: key.to_vec() })
}

fn serial() -> Result<SerialNumber> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|e| crate::Error::Io(e.to_string()))?;
    bytes[0] &= 0x7f; // positive
    SerialNumber::new(&bytes).or_else(|e| device(format!("certificate serial: {e}")))
}

/// A new CA and server pair, as ha/gencert makes them: RSA 2048, valid
/// 2000-2099 (the unit checks the dates before its clock is set). The CA's
/// key is not kept: nothing else ever needs signing.
pub fn generate_pki() -> Result<Pki> {
    let failed = |what: &str, e: &dyn std::fmt::Display| crate::Error::Device(format!("nestlocal {what}: {e}"));
    let mut rng = rand_core::OsRng;
    let ca_key = RsaPrivateKey::new(&mut rng, 2048).map_err(|e| failed("CA key", &e))?;
    let key = RsaPrivateKey::new(&mut rng, 2048).map_err(|e| failed("server key", &e))?;
    let validity = Validity {
        not_before: Time::UtcTime(UtcTime::from_unix_duration(Duration::from_secs(946_684_800)).map_err(|e| failed("dates", &e))?),
        not_after: Time::GeneralTime(GeneralizedTime::from_unix_duration(Duration::from_secs(4_102_358_400)).map_err(|e| failed("dates", &e))?),
    };
    let spki = |key: &RsaPrivateKey| -> Result<SubjectPublicKeyInfoOwned> {
        let der = RsaPublicKey::from(key).to_public_key_der().map_err(|e| failed("public key", &e))?;
        SubjectPublicKeyInfoOwned::from_der(der.as_bytes()).map_err(|e| failed("public key", &e))
    };
    let ca_name = Name::from_str("CN=nestlocal CA").map_err(|e| failed("CA name", &e))?;
    let signer = SigningKey::<Sha256>::new(ca_key.clone());
    let ca = CertificateBuilder::new(Profile::Root, serial()?, validity, ca_name.clone(), spki(&ca_key)?, &signer)
        .map_err(|e| failed("CA", &e))?.build::<Signature>().map_err(|e| failed("CA", &e))?;
    let mut leaf = CertificateBuilder::new(
        Profile::Leaf { issuer: ca_name, enable_key_agreement: false, enable_key_encipherment: true },
        serial()?, validity, Name::from_str("CN=127.0.0.1").map_err(|e| failed("server name", &e))?, spki(&key)?, &signer,
    ).map_err(|e| failed("server certificate", &e))?;
    let names = vec![GeneralName::DnsName(Ia5String::new("localhost").map_err(|e| failed("server name", &e))?),
                     GeneralName::IpAddress(OctetString::new(vec![127, 0, 0, 1]).map_err(|e| failed("server name", &e))?)];
    leaf.add_extension(&SubjectAltName(names)).map_err(|e| failed("server certificate", &e))?;
    leaf.add_extension(&ExtendedKeyUsage(vec![ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.1")]))
        .map_err(|e| failed("server certificate", &e))?;
    let leaf = leaf.build::<Signature>().map_err(|e| failed("server certificate", &e))?;
    let pem = |cert: &Certificate| cert.to_pem(LineEnding::LF).map(String::into_bytes).map_err(|e| failed("PEM", &e));
    Ok(Pki {
        ca: pem(&ca)?,
        server_crt: pem(&leaf)?,
        server_key: key.to_pkcs1_pem(LineEnding::LF).map_err(|e| failed("key PEM", &e))?.as_bytes().to_vec(),
    })
}

/// Files and directories an overlay puts in a volume.
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    pub files: Vec<NamedFile>,
    pub directories: Vec<(String, u32)>,
}

/// The nestlocal overlay for a unit, from its own stock files (contents and
/// mode); the files it edits keep the unit's mode.
pub fn overlay(stock: impl Fn(&str) -> Option<(Vec<u8>, u32)>, mqtt: Option<&Mqtt>, matter: Option<&Matter>, zigbee: bool,
               binary: &[u8], pki: &Pki) -> Result<Overlay> {
    let text = |name: &str| -> Result<String> {
        let (data, _) = stock(name).map_or_else(|| device(format!("{name} is missing from the unit")), Ok)?;
        String::from_utf8(data).or_else(|_| device(format!("{name} is not text")))
    };
    let mode = |name: &str, default: u32| stock(name).map_or(default, |(_, mode)| mode);
    let (options, secrets) = options(mqtt, matter, zigbee)?;
    let product = configure_matter_ble(edit_product_config(&text("etc/nestlabs/product.config")?)?,
        matter.is_some(), stock(BLE_FIRMWARE).map(|(data, _)| data).as_deref());
    let mut files = vec![
        ("nestlabs/sbin/nestlocal".to_string(), 0o755, binary.to_vec()),
        ("etc/nestlocal/server.crt".to_string(), 0o644, pki.server_crt.clone()),
        ("etc/nestlocal/server.key".to_string(), 0o600, pki.server_key.clone()),
        (OPTIONS.to_string(), 0o644, options),
        ("etc/init.d/nestlocal".to_string(), 0o755, INIT.to_vec()),
        ("etc/monit.d/nestlocal.monitrc".to_string(), 0o600, MONITRC.to_vec()),
        ("etc/init.d/rcS".to_string(), mode("etc/init.d/rcS", 0o750), edit_rcs(&text("etc/init.d/rcS")?)?.into_bytes()),
        ("etc/monitrc".to_string(), mode("etc/monitrc", 0o600), edit_monitrc(&text("etc/monitrc")?, zigbee)?.into_bytes()),
        ("etc/init.d/networking-defines".to_string(), mode("etc/init.d/networking-defines", 0o644),
         edit_networking_defines(&text("etc/init.d/networking-defines")?, zigbee)?.into_bytes()),
        ("etc/nestlabs/client.config".to_string(), mode("etc/nestlabs/client.config", 0o644),
         edit_client_config(&text("etc/nestlabs/client.config")?)?.into_bytes()),
        ("etc/nestlabs/product.config".to_string(), mode("etc/nestlabs/product.config", 0o644),
         product.into_bytes()),
    ];
    let bundle = |path: &str| stock(path).map(|(bundle, _)| (path.to_string(), mode(path, 0o644), with_ca(&bundle, &pki.ca)));
    files.push(bundle("etc/ssl/certs/ca-bundle.pem").map_or_else(|| device("etc/ssl/certs/ca-bundle.pem is missing from the unit"), Ok)?);
    files.extend(bundle(UNLOCK_BUNDLE));
    files.extend(secrets);
    Ok(Overlay { files, directories: vec![("etc/nestlocal".to_string(), 0o755)] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::OnceLock;

    /// Key generation is slow without optimisation: two sets, made once.
    fn pkis() -> &'static (Pki, Pki) {
        static PKIS: OnceLock<(Pki, Pki)> = OnceLock::new();
        PKIS.get_or_init(|| (generate_pki().unwrap(), generate_pki().unwrap()))
    }

    const STOCK_CA: &[u8] = b"-----BEGIN CERTIFICATE-----\nMIIBLjCB4aADAgECAgEBMAUGAytlcDAYMRYwFAYDVQQDDA1OZXN0IFByaXZhdGUg\n-----END CERTIFICATE-----\n";

    #[test]
    fn installed_options_take_precedence_over_legacy_options() {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let dir = std::env::temp_dir().join(format!("nestlocal-options-{}", u64::from_le_bytes(nonce)));
        std::fs::create_dir(&dir).unwrap();
        let installed = dir.join("installed");
        let legacy = dir.join("legacy");
        std::fs::write(&installed, "-mqtt tcp://broker:1883 -zigbee\n").unwrap();
        std::fs::write(&legacy, "-mqtt tcp://10.0.2.100:1883\n").unwrap();
        let script = std::str::from_utf8(INIT).unwrap().split("case ").next().unwrap()
            .replace("/etc/nestlocal/options", installed.to_str().unwrap())
            .replace("/media/system-config/nestlocal.options", legacy.to_str().unwrap());
        let selected = || {
            let output = std::process::Command::new("sh").arg("-c")
                .arg(format!("{script}\ncat \"$OPTIONS\""))
                .output().unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap()
        };
        assert_eq!(selected(), "-mqtt tcp://broker:1883 -zigbee\n");
        std::fs::remove_file(&installed).unwrap();
        assert_eq!(selected(), "-mqtt tcp://10.0.2.100:1883\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn matter_pairing() {
        // The example Matter SDK devices print: passcode 20202021, discriminator 3840.
        assert_eq!(Matter { passcode: 20202021, discriminator: 3840 }.pairing_code(), "3497-011-2332");
        let sdk = Matter::from_pairing_code("34970112332").unwrap();
        assert_eq!((sdk.passcode, sdk.discriminator >> 8), (20202021, 3840 >> 8));
        // The default, and any code, gives the unit exactly that code back.
        let default = Matter::from_pairing_code(DEFAULT_MATTER_CODE).unwrap();
        assert_eq!(default.pairing_code(), DEFAULT_MATTER_CODE);
        for (passcode, discriminator) in [(1, 0), (58596013, 1882), (99999998, 4095), (9062294, 610)] {
            let code = Matter { passcode, discriminator }.pairing_code();
            let back = Matter::from_pairing_code(&code).unwrap();
            assert_eq!((back.passcode, back.pairing_code()), (passcode, code));
        }
        assert_eq!(Matter::from_pairing_code(" 1559 813 5763 ").unwrap().passcode, 58596013);
        // A mistyped digit, a short code, letters, and a code whose passcode Matter forbids.
        for bad in ["1559-813-5764", "1559-813-576", "1559-81a-5763",
                    &Matter { passcode: 12345678, discriminator: 0 }.pairing_code()] {
            assert!(Matter::from_pairing_code(bad).is_err(), "{bad} accepted");
        }
    }

    #[test]
    fn options_for_mqtt_matter_or_both() {
        let matter = Matter { passcode: 34970112, discriminator: 1234 };
        let mqtt = Mqtt { url: "broker".into(), ..Mqtt::default() };
        let line = |mqtt, matter| String::from_utf8(options(mqtt, matter, false).unwrap().0).unwrap();
        assert_eq!(line(Some(&mqtt), None), "-mqtt tcp://broker:1883\n");
        assert_eq!(line(Some(&mqtt), Some(&matter)),
                   "-mqtt tcp://broker:1883 -matter -matter-passcode 34970112 -matter-discriminator 1234\n");
        // Matter alone: no broker at all, not nestlocal's default one.
        assert_eq!(line(None, Some(&matter)), "-mqtt= -matter -matter-passcode 34970112 -matter-discriminator 1234\n");
        assert_eq!(String::from_utf8(options(Some(&mqtt), None, true).unwrap().0).unwrap(),
                   "-mqtt tcp://broker:1883 -zigbee\n");
    }

    #[test]
    fn zigbee_keeps_wpantund_down_and_back_without() {
        let monitrc = format!("set daemon 5\n{WPANTUND_LINE}");
        let with = edit_monitrc(&monitrc, true).unwrap();
        assert_eq!(with, format!("set daemon 5\n{MONIT_LINE}"));
        assert_eq!(edit_monitrc(&with, true).unwrap(), with);
        assert_eq!(edit_monitrc(&with, false).unwrap(), format!("set daemon 5\n{WPANTUND_LINE}{MONIT_LINE}"));
        assert_eq!(edit_monitrc(&monitrc, false).unwrap(), format!("{monitrc}{MONIT_LINE}"));
        let defines = format!("#!/bin/sh\n{WITH_WPAN}X=1\n");
        let without = edit_networking_defines(&defines, true).unwrap();
        assert_eq!(without, format!("#!/bin/sh\n{WITHOUT_WPAN}X=1\n"));
        assert_eq!(edit_networking_defines(&without, true).unwrap(), without);
        assert_eq!(edit_networking_defines(&without, false).unwrap(), defines);
        assert!(edit_networking_defines("X=1\n", true).is_err());
    }

    #[test]
    fn edits_refuse_files_they_do_not_recognise() {
        assert!(edit_rcs(&format!("a\n{RCS_LINE}b\n{RCS_BEFORE}")).is_err());
        assert!(edit_rcs(&format!("{RCS_BEFORE}{RCS_BEFORE}")).is_err());
        assert!(edit_client_config(&format!("{STOCK_URL}\n{STOCK_URL}\n")).is_err());
        assert!(edit_client_config("<a key=\"cloudregisterurl\" value=\"a\"/><a key=\"cloudregisterurl\" value=\"b\"/>").is_err());
        assert!(edit_product_config("").is_err());
        assert!(edit_monitrc(&format!("{MONIT_LINE}{MONIT_LINE}"), false).is_err());
    }

    #[test]
    fn matter_ble_preserves_unknown_firmware_and_user_disabled_radio() {
        for value in [0, 1] {
            let config = format!("<config><a key=\"ble_disabled\" value=\"{value}\"/></config>\n");
            assert_eq!(configure_matter_ble(config.clone(), true, None), config);
            assert_eq!(configure_matter_ble(config.clone(), true, Some(b"unknown")), config);
            assert_eq!(configure_matter_ble(config.clone(), false, None), config);
        }
    }

    #[test]
    #[ignore = "requires NEST_BLE_FIRMWARE pointing to the stock peripheral image"]
    fn matter_ble_ownership_is_repeatable_and_reversible() {
        let firmware = std::fs::read(std::env::var("NEST_BLE_FIRMWARE").unwrap()).unwrap();
        let original = "<config><a key=\"ble_disabled\" value=\"0\"/></config>\n";
        let configured = configure_matter_ble(original.into(), true, Some(&firmware));
        assert!(configured.contains("key=\"ble_disabled\" value=\"1\""));
        assert_eq!(configure_matter_ble(configured.clone(), true, Some(&firmware)), configured);
        assert_eq!(configure_matter_ble(configured, false, None), original);
    }

    #[test]
    fn generated_sets_belong_together_and_only_with_each_other() {
        let (one, two) = pkis();
        assert!(belongs_together(&one.ca, &one.server_crt, &one.server_key));
        assert!(!belongs_together(&two.ca, &one.server_crt, &one.server_key));
        assert!(!belongs_together(&one.ca, &one.server_crt, &two.server_key));
        assert!(nestlocal_ca(&one.ca));
        assert!(!nestlocal_ca(&one.server_crt));
        assert!(String::from_utf8_lossy(&one.server_key).starts_with("-----BEGIN RSA PRIVATE KEY-----"));
    }

    #[test]
    fn bundle_keeps_stock_and_exactly_one_nestlocal_ca() {
        let (one, two) = pkis();
        let bundle = [STOCK_CA, &two.ca, b"\n", &one.ca].concat();
        let result = with_ca(&bundle, &one.ca);
        assert_eq!(result, [STOCK_CA, &one.ca].concat());
        assert_eq!(with_ca(&result, &one.ca), result);
    }

    #[test]
    fn unit_keeps_its_own_matching_set() {
        let (one, two) = pkis();
        let bundle = [STOCK_CA, &two.ca, &one.ca].concat();
        assert_eq!(unit_pki(Some(&one.server_crt), Some(&one.server_key), Some(&bundle)), Some(one.clone()));
        let without = [STOCK_CA, &two.ca].concat();
        assert_eq!(unit_pki(Some(&one.server_crt), Some(&one.server_key), Some(&without)), None);
        assert_eq!(unit_pki(None, Some(&one.server_key), Some(&bundle)), None);
    }

    #[test]
    fn broker_urls_get_their_default_port() {
        let options = |url: &str, tls| String::from_utf8(mqtt_options(&Mqtt { url: url.into(), tls, ..Mqtt::default() })
            .unwrap().0).unwrap();
        assert_eq!(options("mqtt.lan", false), "-mqtt tcp://mqtt.lan:1883\n");
        assert_eq!(options("mqtt.lan", true), "-mqtt tcp://mqtt.lan:8883 -mqtt-tls\n");
        assert_eq!(options("tcp://10.0.0.2:1884", false), "-mqtt tcp://10.0.0.2:1884\n");
        assert!(mqtt_url("bad url!").is_err());
    }

    #[test]
    fn broker_ports_are_tcp_ports() {
        assert_eq!(mqtt_url("tcp://mqtt.lan:65535").unwrap(), "tcp://mqtt.lan:65535");
        assert_eq!(mqtt_url("mqtt.lan:1").unwrap(), "tcp://mqtt.lan:1");
        for url in ["mqtt.lan:0", "mqtt.lan:65536", "tcp://mqtt.lan:99999", "mqtt.lan:00000"] {
            assert!(mqtt_url(url).is_err(), "{url}");
        }
    }

    #[test]
    fn password_and_ca_go_in_their_own_files() {
        let mqtt = Mqtt { url: "broker".into(), user: Some("u".into()), password: Some("p".into()),
                          ca: Some(STOCK_CA.to_vec()), ..Mqtt::default() };
        let (options, files) = mqtt_options(&mqtt).unwrap();
        assert_eq!(String::from_utf8(options).unwrap(), "-mqtt tcp://broker:8883 -mqtt-user u -mqtt-password-file \
            /etc/nestlocal/mqtt.password -mqtt-tls -mqtt-ca /etc/nestlocal/mqtt-ca.pem\n");
        assert_eq!(files[0], (PASSWORD_FILE.into(), 0o600, b"p\n".to_vec()));
        assert!(mqtt_options(&Mqtt { password: Some("p".into()), ..Mqtt::default() }).is_err());
        assert!(mqtt_options(&Mqtt { url: "b".into(), user: Some("two words".into()), ..Mqtt::default() }).is_err());
    }

    fn stock() -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([
            ("etc/init.d/rcS".into(), b"a\n${INITDIR}/nestlabs start\nb\n".to_vec()),
            ("etc/monitrc".into(), format!("set daemon 30\n{WPANTUND_LINE}").into_bytes()),
            ("etc/init.d/networking-defines".into(), WITH_WPAN.as_bytes().to_vec()),
            ("etc/nestlabs/client.config".into(), format!("<a key=\"cloudregisterurl\" value=\"{STOCK_URL}\"/>\n").into_bytes()),
            ("etc/nestlabs/product.config".into(), b"<a key=\"usephoenix\" value=\"1\"/>\n".to_vec()),
            ("etc/ssl/certs/ca-bundle.pem".into(), STOCK_CA.to_vec()),
            ("etc/ssl/certs/ca-unlock-bundle.pem".into(), STOCK_CA.to_vec()),
        ])
    }

    #[test]
    fn overlay_edits_the_units_files_and_accepts_its_own_result() {
        let (one, _) = pkis();
        let mqtt = Mqtt { url: "broker".into(), ..Mqtt::default() };
        let mut files = stock();
        let first = overlay(|name| files.get(name).map(|data| (data.clone(), 0o640)), Some(&mqtt), None, true, b"ARMV7", one).unwrap();
        let by_name: BTreeMap<_, _> = first.files.iter().map(|(n, m, d)| (n.clone(), (*m, d.clone()))).collect();
        let rcs = String::from_utf8(by_name["etc/init.d/rcS"].1.clone()).unwrap();
        assert!(rcs.find(RCS_LINE).unwrap() < rcs.find(RCS_BEFORE).unwrap());
        assert!(String::from_utf8_lossy(&by_name["etc/nestlabs/client.config"].1).contains(LOCAL_URL));
        assert!(String::from_utf8_lossy(&by_name["etc/nestlabs/product.config"].1).contains("value=\"0\""));
        assert_eq!(by_name["etc/nestlocal/server.key"].0, 0o600);
        assert_eq!(by_name["etc/init.d/rcS"].0, 0o640);
        for (name, (_, data)) in &by_name {
            files.insert(name.clone(), data.clone());
        }
        assert!(!String::from_utf8_lossy(&by_name["etc/monitrc"].1).contains(WPANTUND_LINE));
        assert_eq!(by_name["etc/init.d/networking-defines"].1, WITHOUT_WPAN.as_bytes());
        let again = overlay(|name| files.get(name).map(|data| (data.clone(), 0o640)), Some(&mqtt), None, true, b"ARMV7", one).unwrap();
        assert_eq!(again.files, first.files);
    }

    #[test]
    fn a_j49_has_no_unlock_bundle_and_may_point_at_another_server() {
        let (one, _) = pkis();
        let mut files = stock();
        files.remove(UNLOCK_BUNDLE);
        files.insert("etc/nestlabs/client.config".into(),
                     b"<a key=\"cloudregisterurl\" value=\"http://homeassistant.local:9543/entry\"/>\n".to_vec());
        let made = overlay(|name| files.get(name).map(|data| (data.clone(), 0o644)), None, None, false, b"ARMV7", one).unwrap();
        let by_name: BTreeMap<_, _> = made.files.iter().map(|(n, _, d)| (n.clone(), d.clone())).collect();
        assert!(!by_name.contains_key(UNLOCK_BUNDLE));
        assert_eq!(String::from_utf8_lossy(&by_name["etc/nestlabs/client.config"]),
                   format!("<a key=\"cloudregisterurl\" value=\"{LOCAL_URL}\"/>\n"));
    }
}
