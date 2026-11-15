//! The key exchange's identities and keyed values: the unit's confirmation
//! of a session key and the trust it grants, the trust key a grant derives,
//! and a trusted laptop's proof that it holds its old key.

use ring::{hkdf, hmac};

use crate::{hex, unhex};

/// The exchange's identities.
pub const CLIENT: &[u8] = b"unfr0zen";
pub const UNIT: &[u8] = b"nestlocal";
/// What the unit's confirmation of a session key signs first.
pub const CONFIRMATION: &str = "nestlocal-update unit";
/// The trust key's HKDF info, before the unit's and the laptop's ids.
const TRUST_INFO: &str = "nestlocal-update trust";
/// What a replacement proof signs first, before the unit's id, the
/// laptop's, and the exchange's client message in hex (replace_message).
const REPLACE_INFO: &str = "nestlocal-update replace";
/// A laptop's name when it gives none the unit can show, and the sender's
/// name for a session keyed by a code.
pub const UNNAMED: &str = "unfr0zen";

/// The hex HMAC-SHA256 of message under key.
pub fn sign(key: &[u8], message: &str) -> String {
    hex(hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), message.as_bytes()).as_ref())
}

/// Whether tag_hex is the HMAC-SHA256 of message under key.
pub fn verify(key: &[u8], message: &str, tag_hex: &str) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    unhex(tag_hex).is_some_and(|tag| hmac::verify(&key, message.as_bytes(), &tag).is_ok())
}

/// What the unit's confirmation of a key exchange signs: with a grant, its
/// unit's and laptop's ids, then the id of the laptop it replaces if any. It
/// proves the grant the unit's too, and whether it replaces one, so that
/// nothing on the way turns a replacement into an addition unseen.
pub fn confirmation(grant: Option<(&str, &str)>, replaces: Option<&str>) -> String {
    let mut message = CONFIRMATION.to_string();
    if let Some((unit, laptop)) = grant {
        message = format!("{message}\n{unit}\n{laptop}");
        if let Some(old) = replaces {
            message = format!("{message}\n{old}");
        }
    }
    message
}

/// Whether id is in an id's form, a unit's or a laptop's: 16 bytes in
/// lowercase hex.
pub fn is_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The trust key a session key gives the laptop of id laptop on the unit of
/// id unit: HKDF-SHA256 with no salt, and TRUST_INFO and the ids as info.
pub fn trust_key(session_key: &[u8], unit: &str, laptop: &str) -> [u8; 32] {
    let info = format!("{TRUST_INFO}\n{unit}\n{laptop}");
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(session_key);
    let mut key = [0u8; 32];
    prk.expand(&[info.as_bytes()], hkdf::HKDF_SHA256).and_then(|okm| okm.fill(&mut key))
        .expect("32 bytes is an HKDF-SHA256 output length");
    key
}

/// What a laptop trusted as laptop on the unit of id unit signs, under its
/// trust key, to have the grant of the key exchange whose client message is
/// message replace it: the message is fresh to each exchange, so a proof
/// serves no other.
pub fn replace_message(unit: &str, laptop: &str, message: &[u8]) -> String {
    format!("{REPLACE_INFO}\n{unit}\n{laptop}\n{}", hex(message))
}

/// The replacement proof under key, in hex: HMAC-SHA256 of replace_message.
pub fn replace_proof(key: &[u8], unit: &str, laptop: &str, message: &[u8]) -> String {
    sign(key, &replace_message(unit, laptop, message))
}

/// A laptop's name as the unit takes it: printable ASCII, at most 32
/// characters, UNNAMED when that leaves none.
pub fn laptop_name(name: &str) -> String {
    let printable: String = name.chars().filter(|c| (' '..='~').contains(c)).collect();
    let name: String = printable.trim().chars().take(32).collect();
    match name.trim_end() {
        "" => UNNAMED.to_string(),
        name => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trust_key_matches_the_shared_test_vector() {
        let key: Vec<u8> = (0..32).collect();
        let (unit, laptop) = ("00".repeat(16), "11".repeat(16));
        assert_eq!(hex(&trust_key(&key, &unit, &laptop)), TRUST_KEY_VECTOR);
    }

    #[test]
    fn the_replacement_proof_matches_the_shared_test_vector() {
        let key: Vec<u8> = (0..32).collect();
        let message: Vec<u8> = (0..33).collect();
        assert_eq!(replace_proof(&key, &"00".repeat(16), &"11".repeat(16), &message), REPLACE_PROOF_VECTOR);
    }

    /// replace_proof((0..32), "00"*16, "11"*16, (0..33)).
    const REPLACE_PROOF_VECTOR: &str = "d6294793b2415d22cf34f97e8de2e67e7577aca30db7aa948f53cb0269be4b7b";

    /// trust_key((0..32), "00"*16, "11"*16).
    const TRUST_KEY_VECTOR: &str = "427981eb673bdcb2f3fdcefa65bc98c09f0eb436cb36dc3eb608a20f2573fc20";
}
