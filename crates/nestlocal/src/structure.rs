//! The unit's structure. Nest's service put every unit in a structure (a
//! home): link.<serial> names it, and structure.<id> holds it. Firmware
//! 5.9.4-5 works without one. 6.4-8 does not: it takes a unit with no
//! structure for an unpaired one and moves it to Nest's newer service,
//! Phoenix ("device was unpaired on rts - restarting pointing to
//! phoenix"), after which it no longer talks to nestlocal. It also
//! decides that at start, before it asks nestlocal anything, from its
//! structure_id setting ("Structure ID cleared, booting into phoenix"),
//! so that setting is seeded too (seed_structure_id, run by nestlocal.init
//! before nlclient starts).

use md5::{Digest, Md5};
use serde_json::Value;

use crate::fields;
use crate::store::Store;

/// A UUID made from the serial: the same every time.
pub fn structure_id(serial: &str) -> String {
    let x: String = Md5::digest(serial.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &x[0..8], &x[8..12], &x[12..16], &x[16..20], &x[20..32])
}

/// Adds the link and structure buckets when the store has none: a unit's
/// own changes to them are kept.
pub fn ensure_structure(store: &Store, serial: &str) {
    let id = format!("structure.{}", structure_id(serial));
    if store.get(&format!("link.{serial}")).is_none() {
        store.merge(&format!("link.{serial}"), &fields!{"structure" => id.clone()}, 0, 0, false);
    }
    if store.get(&id).is_none() {
        let devices = Value::Array(vec![Value::from(format!("device.{serial}"))]);
        store.merge(&id, &fields!{"name" => "Home", "devices" => devices}, 0, 0, false);
    }
}

const SETTING: &[u8] = br#"<s key="structure_id" value=""#;
const SETTING_END: &[u8] = br#""/>"#;

/// Every `<s key="structure_id" value="..."/>` in b: its span and value.
fn settings(b: &[u8]) -> Vec<(usize, usize, &[u8])> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(i) = find(&b[at..], SETTING) {
        let start = at + i;
        let value_start = start + SETTING.len();
        let value_len = b[value_start..].iter().position(|&c| c == b'"').unwrap_or(b.len() - value_start);
        let end = value_start + value_len;
        if b[end..].starts_with(SETTING_END) {
            found.push((start, end + SETTING_END.len(), &b[value_start..end]));
            at = end + SETTING_END.len();
        } else {
            at = start + 1;
        }
    }
    found
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Sets structure_id in nlclient's settings file when it is empty or
/// missing. One the unit already has (from Nest's service, or from
/// nestlocal's link bucket) stays.
pub fn seed_structure_id(path: &str, serial: &str) -> std::io::Result<bool> {
    let b = std::fs::read(path)?;
    let line = format!(r#"<s key="structure_id" value="{}"/>"#, structure_id(serial)).into_bytes();
    let found = settings(&b);
    let out = match found.first() {
        Some((_, _, value)) if !value.is_empty() => return Ok(false),
        Some(_) => {
            let mut out = Vec::with_capacity(b.len());
            let mut at = 0;
            for (start, end, _) in &found {
                out.extend_from_slice(&b[at..*start]);
                out.extend_from_slice(&line);
                at = *end;
            }
            out.extend_from_slice(&b[at..]);
            out
        }
        None => {
            let end = b.windows(11).rposition(|w| w == b"</settings>")
                .ok_or_else(|| std::io::Error::other(format!("{path}: no </settings>")))?;
            let mut out = b[..end].to_vec();
            out.extend_from_slice(b"  ");
            out.extend_from_slice(&line);
            out.push(b'\n');
            out.extend_from_slice(&b[end..]);
            out
        }
    };
    let tmp = format!("{path}.nestlocal");
    std::fs::write(&tmp, out)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::temp_store;
    use serde_json::json;

    #[test]
    fn structure_id_is_a_stable_uuid() {
        let id = structure_id("09AA01AC25160NMZ");
        assert!(id.len() == 36 && id.matches('-').count() == 4 && id == structure_id("09AA01AC25160NMZ"), "{id}");
    }

    #[test]
    fn ensure_structure_keeps_the_units_changes() {
        let s = temp_store();
        ensure_structure(&s, "S");
        let sid = format!("structure.{}", structure_id("S"));
        assert_eq!(s.get("link.S").unwrap().value["structure"], json!(sid));
        assert_eq!(s.get(&sid).unwrap().value["devices"], json!(["device.S"]));
        // What the unit changed stays.
        s.merge(&sid, &fields!{"away" => true}, 0, 0, true);
        ensure_structure(&s, "S");
        assert_eq!(s.get(&sid).unwrap().value["away"], json!(true));
    }

    #[test]
    fn seed_structure_id_cases() {
        let want = format!(r#"<s key="structure_id" value="{}"/>"#, structure_id("S"));
        let dir = std::env::temp_dir().join(format!("nestlocal-seed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (i, (input, output)) in [
            ("<settings>\n  <s key=\"structure_id\" value=\"\"/>\n</settings>\n".to_string(),
             format!("<settings>\n  {want}\n</settings>\n")),
            ("<settings>\n  <s key=\"a\" value=\"1\"/>\n</settings>\n".to_string(),
             format!("<settings>\n  <s key=\"a\" value=\"1\"/>\n  {want}\n</settings>\n")),
            ("<settings>\n  <s key=\"structure_id\" value=\"nest-cloud-id\"/>\n</settings>\n".to_string(),
             "<settings>\n  <s key=\"structure_id\" value=\"nest-cloud-id\"/>\n</settings>\n".to_string()),
        ].into_iter().enumerate() {
            let path = dir.join(format!("settings{i}.config"));
            std::fs::write(&path, &input).unwrap();
            let changed = seed_structure_id(path.to_str().unwrap(), "S").unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), output);
            assert_eq!(changed, input != output);
        }
    }

    #[test]
    fn seed_structure_id_odd_files() {
        let want = format!(r#"<s key="structure_id" value="{}"/>"#, structure_id("S"));
        let dir = std::env::temp_dir().join(format!("nestlocal-seed-odd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.config");
        let seed = |input: &str| {
            std::fs::write(&path, input).unwrap();
            let r = seed_structure_id(path.to_str().unwrap(), "S");
            (r.map_err(|e| e.to_string()), std::fs::read_to_string(&path).unwrap())
        };
        // Every empty one is filled, not just the first.
        let (r, out) = seed("<settings><s key=\"structure_id\" value=\"\"/><s key=\"structure_id\" value=\"\"/></settings>");
        assert_eq!((r, out), (Ok(true), format!("<settings>{want}{want}</settings>")));
        // One cut short is no setting: a whole one goes before the last </settings>.
        let (r, out) = seed("<settings>\n<s key=\"structure_id\" value=\"x</settings>\n");
        assert_eq!((r, out), (Ok(true), format!("<settings>\n<s key=\"structure_id\" value=\"x  {want}\n</settings>\n")));
        // No </settings>: an error, and the file as it was.
        let (r, out) = seed("<settings>\n");
        assert!(r.unwrap_err().ends_with("no </settings>"));
        assert_eq!(out, "<settings>\n");
        assert!(seed_structure_id(dir.join("missing").to_str().unwrap(), "S").is_err());
    }

    #[test]
    fn ensure_structure_keeps_a_link_the_unit_has() {
        let s = temp_store();
        s.merge("link.S", &fields!{"structure" => "structure.from-nest"}, 5, 100, true);
        ensure_structure(&s, "S");
        let link = s.get("link.S").unwrap();
        assert_eq!((link.revision, link.value["structure"].clone()), (5, json!("structure.from-nest")));
        // The structure nestlocal would name is still made: the unit's own may be missing.
        assert!(s.get(&format!("structure.{}", structure_id("S"))).is_some());
    }
}
