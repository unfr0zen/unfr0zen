//! The update protocol nestlocal's update API (`crates/nestlocal/src/wifi_update`)
//! and the app (`unfr0zen-host`) share, so both ends build and read the same
//! messages.

/// The port nestlocal's update API listens on, as the app starts it
/// (unfr0zen-core's nestlocal::UPDATE_PORT, which unfr0zen-host's tests hold
/// to this).
pub const PORT: u16 = 8443;

pub mod auth;
pub mod bundle;
pub mod read;
pub mod seal;

use ring::rand::{SecureRandom, SystemRandom};

/// bytes in lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The bytes text spells in hex, either case; none for anything else.
pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok()).collect()
}

/// N bytes from the system's random source.
pub fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    SystemRandom::new().fill(&mut bytes).expect("the system's random source works");
    bytes
}
