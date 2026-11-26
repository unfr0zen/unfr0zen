//! Factory boot configuration signatures and image hashes. The original
//! strings table and property offsets are part of the signed representation.

use serde::Deserialize;

#[derive(Deserialize)]
struct Metadata {
    version: u32,
    last_compatible_version: u32,
    boot_cpu: u32,
    strings: String,
    structure: Vec<Token>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Token {
    Node { node: String },
    Property { property: String, name_offset: u32, value: String },
    End { end: bool },
    Nop { nop: bool },
}

fn word(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(value);
    out.resize(out.len().next_multiple_of(4), 0);
}

/// The factory FIT's signed metadata, retaining the representation its
/// configuration signatures cover. Recovery supplies its own image data.
pub fn template() -> Vec<u8> {
    let metadata: Metadata = serde_json::from_str(include_str!("boot_metadata.json")).expect("boot metadata");
    let mut structure = Vec::new();
    for token in metadata.structure {
        match token {
            Token::Node { node } => {
                word(&mut structure, 1);
                bytes(&mut structure, format!("{node}\0").as_bytes());
            }
            Token::Property { property, name_offset, value } => {
                assert!(metadata.strings[name_offset as usize..].starts_with(&format!("{property}\0")));
                let value: Vec<u8> = value
                    .as_bytes()
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                    .collect();
                word(&mut structure, 3);
                word(&mut structure, value.len() as u32);
                word(&mut structure, name_offset);
                bytes(&mut structure, &value);
            }
            Token::End { end } => {
                assert!(end);
                word(&mut structure, 2);
            }
            Token::Nop { nop } => {
                assert!(nop);
                word(&mut structure, 4);
            }
        }
    }
    word(&mut structure, 9);
    let strings_at = 56 + structure.len();
    let mut out = Vec::new();
    for value in [
        0xd00d_feed,
        (strings_at + metadata.strings.len()) as u32,
        56,
        strings_at as u32,
        40,
        metadata.version,
        metadata.last_compatible_version,
        metadata.boot_cpu,
        metadata.strings.len() as u32,
        structure.len() as u32,
        0,
        0,
        0,
        0,
    ] {
        word(&mut out, value);
    }
    out.extend(structure);
    out.extend(metadata.strings.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn factory_metadata_preserves_its_signed_representation() {
        assert_eq!(
            crate::shell::sha256_hex(&super::template()),
            "14157a9d0a9e83dc669ef46d615313df0411526bdfa976f1f04d1d46c128965a"
        );
    }
}
