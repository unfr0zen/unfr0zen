//! The network the unit is on, kept on the unit (-zigbee-store) besides in
//! the radio's own flash: rewriting the radio (Nest's firmware put back, an
//! update) loses that, and the network comes back from here, without
//! pairing again. A router knows everything it needs to rejoin without a
//! word to anyone: EmberZNet's "configured network state" join.
//!
//! The frame counters go back ahead of where they were saved: the network
//! drops frames whose counters it has seen before.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::ezsp::{id, status, Ezsp, Reader};

const TRUST_CENTER_LINK_KEY: u8 = 1;
const CURRENT_NETWORK_KEY: u8 = 3;
/// What the counters may have moved since the last save: a router's link
/// status every 15 s, reports, routing, ten minutes of it and then some.
const COUNTER_MARGIN: u32 = 4096;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// The radio the network is for.
    pub eui64: String,
    pub pan_id: u16,
    pub extended_pan_id: String,
    pub channel: u8,
    pub node_id: u16,
    pub manager_id: u16,
    pub update_id: u8,
    pub network_key: String,
    pub network_key_sequence: u8,
    pub network_frame_counter: u32,
    pub trust_center_link_key: String,
    pub trust_center_eui64: String,
    pub aps_frame_counter: u32,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An EUI64 as written (0x18b430…), from the radio's little-endian bytes.
fn eui_hex(bytes: &[u8]) -> String {
    hex(&bytes.iter().rev().copied().collect::<Vec<u8>>())
}

fn eui_unhex(text: &str) -> Result<[u8; 8], String> {
    let mut bytes = unhex::<8>(text)?;
    bytes.reverse();
    Ok(bytes)
}

fn unhex<const N: usize>(text: &str) -> Result<[u8; N], String> {
    let bytes: Option<Vec<u8>> = (0..text.len()).step_by(2)
        .map(|i| text.get(i..i + 2).filter(|b| b.bytes().all(|c| c.is_ascii_hexdigit()))
            .and_then(|b| u8::from_str_radix(b, 16).ok())).collect();
    bytes.and_then(|b| b.try_into().ok()).ok_or_else(|| format!("not {N} bytes of hex: {text:?}"))
}

/// A saved frame counter, put ahead of where it was; one at the end stays there.
fn ahead(counter: u32) -> u32 {
    counter.saturating_add(COUNTER_MARGIN)
}

/// The radio's EUI64, as the saved network names it.
pub fn eui64(ezsp: &mut Ezsp) -> Result<String, String> {
    let answer = ezsp.command(id::GET_EUI64, &[])?;
    let bytes = Reader(&answer).bytes(8)?;
    Ok(eui_hex(bytes))
}

/// A key and its counters (EmberKeyStruct).
struct Key {
    key: [u8; 16],
    outgoing: u32,
    sequence: u8,
    partner: [u8; 8],
}

fn key(ezsp: &mut Ezsp, kind: u8) -> Result<Key, String> {
    let answer = ezsp.command(id::GET_KEY, &[kind])?;
    let mut r = Reader(&answer);
    if r.u8()? != status::SUCCESS {
        return Err(format!("getKey {kind}: no such key"));
    }
    let (_bitmask, _type) = (r.u16()?, r.u8()?);
    let key = r.bytes(16)?.try_into().unwrap();
    let outgoing = r.u32()?;
    let _incoming = r.u32()?;
    let sequence = r.u8()?;
    let partner = r.bytes(8)?.try_into().unwrap();
    Ok(Key { key, outgoing, sequence, partner })
}

/// The network the radio is on, as of now.
pub fn take(ezsp: &mut Ezsp) -> Result<Saved, String> {
    let answer = ezsp.command(id::GET_NETWORK_PARAMETERS, &[])?;
    let mut r = Reader(&answer);
    if r.u8()? != status::SUCCESS {
        return Err("getNetworkParameters: not on a network".into());
    }
    let _node_type = r.u8()?;
    let extended_pan_id = hex(r.bytes(8)?);
    let pan_id = r.u16()?;
    let (_power, channel, _method) = (r.u8()?, r.u8()?, r.u8()?);
    let manager_id = r.u16()?;
    let update_id = r.u8()?;
    let node_id = Reader(&ezsp.command(id::GET_NODE_ID, &[])?).u16()?;
    let network = key(ezsp, CURRENT_NETWORK_KEY)?;
    let link = key(ezsp, TRUST_CENTER_LINK_KEY)?;
    Ok(Saved {
        eui64: eui64(ezsp)?,
        pan_id,
        extended_pan_id,
        channel,
        node_id,
        manager_id,
        update_id,
        network_key: hex(&network.key),
        network_key_sequence: network.sequence,
        network_frame_counter: network.outgoing,
        trust_center_link_key: hex(&link.key),
        trust_center_eui64: eui_hex(&link.partner),
        aps_frame_counter: link.outgoing,
    })
}

pub fn load(path: &Path) -> Option<Saved> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Writes the network, readable by root alone (it holds the network key).
pub fn save(path: &Path, saved: &Saved) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let text = serde_json::to_vec_pretty(saved).map_err(|e| e.to_string())?;
    let partial = path.with_extension("partial");
    // A mode applies only to a file created, not to one left over.
    let _ = std::fs::remove_file(&partial);
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&partial)
        .map_err(|e| format!("{}: {e}", partial.display()))?;
    file.write_all(&text).and_then(|()| file.sync_all()).map_err(|e| format!("{}: {e}", partial.display()))?;
    std::fs::rename(&partial, path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn forget(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Puts the network back on a radio that has none, as a router with the
/// keys, counters and address it had; the caller waits for it to come up.
pub fn restore(ezsp: &mut Ezsp, saved: &Saved, tx_power: i8) -> Result<(), String> {
    const TRUST_CENTER_GLOBAL_LINK_KEY: u16 = 0x0004;
    const HAVE_TRUST_CENTER_EUI64: u16 = 0x0040;
    const HAVE_PRECONFIGURED_KEY: u16 = 0x0100;
    const HAVE_NETWORK_KEY: u16 = 0x0200;
    const REQUIRE_ENCRYPTED_KEY: u16 = 0x0800;
    const NO_FRAME_COUNTER_RESET: u16 = 0x1000;
    let mut bitmask = TRUST_CENTER_GLOBAL_LINK_KEY | HAVE_PRECONFIGURED_KEY | HAVE_NETWORK_KEY | REQUIRE_ENCRYPTED_KEY
        | NO_FRAME_COUNTER_RESET;
    let trust_center = eui_unhex(&saved.trust_center_eui64)?;
    if trust_center != [0; 8] {
        bitmask |= HAVE_TRUST_CENTER_EUI64;
    }
    let mut security = bitmask.to_le_bytes().to_vec();
    security.extend(unhex::<16>(&saved.trust_center_link_key)?);
    security.extend(unhex::<16>(&saved.network_key)?);
    security.push(saved.network_key_sequence);
    security.extend(trust_center);
    ezsp.expect(id::SET_INITIAL_SECURITY_STATE, &security)?;

    const NWK_FRAME_COUNTER: u8 = 0x23;
    const APS_FRAME_COUNTER: u8 = 0x24;
    const NODE_SHORT_ID: u8 = 0x0B;
    let value = |id: u8, bytes: &[u8]| {
        let mut params = vec![id, bytes.len() as u8];
        params.extend_from_slice(bytes);
        params
    };
    ezsp.expect(id::SET_VALUE, &value(NWK_FRAME_COUNTER, &ahead(saved.network_frame_counter).to_le_bytes()))?;
    ezsp.expect(id::SET_VALUE, &value(APS_FRAME_COUNTER, &ahead(saved.aps_frame_counter).to_le_bytes()))?;
    ezsp.expect(id::SET_VALUE, &value(NODE_SHORT_ID, &saved.node_id.to_le_bytes()))?;

    const ROUTER: u8 = 0x02;
    const CONFIGURED_NETWORK_STATE: u8 = 0x03;
    let mut params = vec![ROUTER];
    params.extend(unhex::<8>(&saved.extended_pan_id)?);
    params.extend(saved.pan_id.to_le_bytes());
    params.push(tx_power as u8);
    params.push(saved.channel);
    params.push(CONFIGURED_NETWORK_STATE);
    params.extend(saved.manager_id.to_le_bytes());
    params.push(saved.update_id);
    params.extend((1u32 << saved.channel).to_le_bytes());
    ezsp.expect(id::JOIN_NETWORK, &params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_both_ways() {
        assert_eq!(hex(&[0x18, 0xb4, 0x30, 0x00]), "18b43000");
        assert_eq!(unhex::<4>("18b43000").unwrap(), [0x18, 0xb4, 0x30, 0x00]);
        assert!(unhex::<4>("18b430").is_err());
        assert!(unhex::<2>("zz00").is_err());
        let radio = [0x46, 0xb7, 0x84, 0x00, 0x00, 0x30, 0xb4, 0x18];
        assert_eq!(eui_hex(&radio), "18b430000084b746");
        assert_eq!(eui_unhex("18b430000084b746").unwrap(), radio);
        assert!(eui_unhex("18b430000084b7").is_err());
        assert!(eui_unhex("18b430000084b74600").is_err());
        assert!(unhex::<2>("18b").is_err());
        assert!(unhex::<1>("é0").is_err());
        for text in ["+f", "-f", " f", "f "] {
            assert!(unhex::<1>(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn frame_counters_go_ahead_but_never_wrap() {
        assert_eq!(ahead(1234), 1234 + COUNTER_MARGIN);
        assert_eq!(ahead(u32::MAX - 1), u32::MAX);
        assert_eq!(ahead(u32::MAX), u32::MAX);
    }

    fn saved() -> Saved {
        Saved {
            eui64: "18b430000084b746".into(), pan_id: 0x1a62, extended_pan_id: "dddddddddddddddd".into(),
            channel: 25, node_id: 0x545a, manager_id: 0, update_id: 0, network_key: "00".repeat(16),
            network_key_sequence: 0, network_frame_counter: 1234, trust_center_link_key: "5a".repeat(16),
            trust_center_eui64: "00".repeat(8), aps_frame_counter: 7,
        }
    }

    #[test]
    fn the_saved_network_is_for_root_alone_and_replaced_whole() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("nestlocal-zigbee-test-mode-{}.json", std::process::id()));
        std::fs::write(&path, "x".repeat(4096)).unwrap();
        let mut first = saved();
        first.network_frame_counter = 99;
        save(&path, &first).unwrap();
        save(&path, &saved()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load(&path), Some(saved()));
        assert!(!path.with_extension("partial").exists());
        forget(&path);
    }

    #[test]
    fn a_leftover_partial_file_does_not_loosen_the_saved_networks_mode() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("nestlocal-zigbee-test-leftover-{}.json", std::process::id()));
        let partial = path.with_extension("partial");
        std::fs::write(&partial, "x").unwrap();
        std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o644)).unwrap();
        save(&path, &saved()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load(&path), Some(saved()));
        forget(&path);
    }

    #[test]
    fn an_unreadable_saved_network_is_none() {
        let path = std::env::temp_dir().join(format!("nestlocal-zigbee-test-bad-{}.json", std::process::id()));
        for text in ["", "{", r#"{"eui64": "18b430000084b746"}"#] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(load(&path), None, "{text:?}");
        }
        forget(&path);
    }

    #[test]
    fn saved_round_trip() {
        let saved = saved();
        let path = std::env::temp_dir().join(format!("nestlocal-zigbee-test-{}.json", std::process::id()));
        save(&path, &saved).unwrap();
        assert_eq!(load(&path), Some(saved));
        forget(&path);
        assert_eq!(load(&path), None);
    }
}
