//! What the app reads from the unit before it builds a set.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Files to read, answered in the same order.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    /// Paths relative to `/`.
    pub names: Vec<String>,
    /// Whether files come with their contents, or only their digests.
    pub contents: bool,
}

/// One read path. A path outside the allow-list reads as absent.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Found {
    Absent,
    File {
        /// "uid:gid".
        owner: String,
        mode: u32,
        sha256: String,
        size: u64,
        /// The contents, when the request asked for them; base64 in JSON.
        #[serde(with = "base64")]
        data: Option<Vec<u8>>,
    },
    Directory {
        /// "uid:gid mode-octal".
        owner_mode: String,
    },
}

/// The unit as the session first sees it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// The U-Boot environment's model, which names the device.
    pub nlmodel: String,
    /// The running nestlocal's version.
    pub version: String,
    /// The installed overlay-version, None without one.
    pub overlay: Option<u32>,
    /// Free bytes on the root filesystem.
    pub free_bytes: u64,
    /// Free bytes on the scratch volume (media/scratch), None without one.
    #[serde(default)]
    pub scratch_free_bytes: Option<u64>,
    pub shell_binary: String,
    /// Whether a set is still on trial.
    pub pending: bool,
}

/// The paths a set would change, for the unit to say before the set is
/// sent which of them its apply would refuse.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CheckRequest {
    /// Paths relative to `/`: files written or removed, and directories.
    pub paths: Vec<String>,
}

/// The unit's answer to a CheckRequest: the paths it would refuse, in the
/// request's order; none when it takes them all.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub refused: Vec<Refused>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub path: String,
    /// Why, without the path.
    pub why: String,
    /// The rule that refused it.
    pub rule: Rule,
}

/// The rules a unit refuses a path by, before a set's contents.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// The filesystem the path is on is mounted read-only.
    ReadOnly,
    /// The path is on the scratch volume, which is not mounted.
    NoScratch,
    /// The allow-list refuses it (bundle::allowed).
    NotAllowed,
    /// It is reached through a symlink or another filesystem.
    Reached,
    /// A rule this end does not know.
    #[serde(other)]
    Other,
}

/// Standard base64 with padding, which keeps a file's contents about a
/// third larger in JSON rather than four times as a number array.
mod base64 {
    use super::*;

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    /// What DECODE holds for a byte not in ALPHABET.
    const NOT_BASE64: u8 = 0xff;
    /// Each byte's value in ALPHABET, else NOT_BASE64.
    const DECODE: [u8; 256] = {
        let mut table = [NOT_BASE64; 256];
        let mut i = 0;
        while i < ALPHABET.len() {
            table[ALPHABET[i] as usize] = i as u8;
            i += 1;
        }
        table
    };

    pub fn encode(data: &[u8]) -> String {
        let mut text = Vec::with_capacity(data.len().div_ceil(3) * 4);
        for group in data.chunks(3) {
            let bits = group.iter().enumerate().fold(0u32, |bits, (i, &b)| bits | u32::from(b) << (16 - 8 * i));
            for i in 0..4 {
                text.push(if i <= group.len() { ALPHABET[(bits >> (18 - 6 * i) & 63) as usize] } else { b'=' });
            }
        }
        String::from_utf8(text).expect("base64 is ASCII")
    }

    pub fn decode(text: &str) -> Option<Vec<u8>> {
        let text = text.as_bytes();
        if !text.len().is_multiple_of(4) {
            return None;
        }
        let mut data = Vec::with_capacity(text.len() / 4 * 3);
        for (n, group) in text.chunks(4).enumerate() {
            let last = n + 1 == text.len() / 4;
            let padding = group.iter().rev().take_while(|&&c| c == b'=').count();
            if padding > 2 || (padding > 0 && !last) {
                return None;
            }
            let mut bits = 0u32;
            for (i, &c) in group[..4 - padding].iter().enumerate() {
                let value = DECODE[usize::from(c)];
                if value == NOT_BASE64 {
                    return None;
                }
                bits |= u32::from(value) << (18 - 6 * i);
            }
            data.extend_from_slice(&bits.to_be_bytes()[1..4 - padding]);
        }
        Some(data)
    }

    pub fn serialize<S: Serializer>(data: &Option<Vec<u8>>, serializer: S) -> Result<S::Ok, S::Error> {
        data.as_deref().map(encode).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|text| decode(&text).ok_or_else(|| serde::de::Error::custom("data is not base64")))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_round_trip_through_json() {
        let request = ReadRequest { names: vec!["etc/nestlocal/options".into(), "etc/monitrc".into()], contents: true };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<ReadRequest>(&json).unwrap(), request);

        // Every length modulo 3, and every byte value.
        let found = vec![
            Found::Absent,
            Found::File { owner: "0:0".into(), mode: 0o644, sha256: "ab".repeat(32), size: 0, data: None },
            Found::File { owner: "0:0".into(), mode: 0o755, sha256: "ab".repeat(32), size: 1, data: Some(vec![0xff]) },
            Found::File { owner: "0:0".into(), mode: 0o600, sha256: "ab".repeat(32), size: 2, data: Some(vec![0, 1]) },
            Found::File { owner: "0:0".into(), mode: 0o600, sha256: "ab".repeat(32), size: 256, data: Some((0..=255).collect()) },
            Found::File { owner: "0:0".into(), mode: 0o600, sha256: "ab".repeat(32), size: 0, data: Some(vec![]) },
            Found::Directory { owner_mode: "0:0 755".into() },
        ];
        let json = serde_json::to_string(&found).unwrap();
        assert_eq!(serde_json::from_str::<Vec<Found>>(&json).unwrap(), found);

        let info = Info {
            nlmodel: "Display-3.4".into(),
            version: "v1.2.3".into(),
            overlay: Some(1),
            free_bytes: 12 << 20,
            scratch_free_bytes: Some(100 << 20),
            shell_binary: "/bin/busybox".into(),
            pending: false,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(serde_json::from_str::<Info>(&json).unwrap(), info);

        let check = CheckRequest { paths: vec!["etc/nestlocal/options".into(), "media/scratch/.unfr0zen".into()] };
        let json = serde_json::to_string(&check).unwrap();
        assert_eq!(serde_json::from_str::<CheckRequest>(&json).unwrap(), check);
        let checked = Checked { refused: vec![Refused { path: "boot/x".into(), why: "is outside what an update may change".into(),
                                                        rule: Rule::NotAllowed }] };
        let json = serde_json::to_string(&checked).unwrap();
        assert_eq!(json, r#"{"refused":[{"path":"boot/x","why":"is outside what an update may change","rule":"not_allowed"}]}"#);
        assert_eq!(serde_json::from_str::<Checked>(&json).unwrap(), checked);
        let later = json.replace("not_allowed", "a_later_rule");
        assert_eq!(serde_json::from_str::<Checked>(&later).unwrap().refused[0].rule, Rule::Other);
    }

    #[test]
    fn file_contents_travel_as_base64() {
        let found = Found::File { owner: "0:0".into(), mode: 0o644, sha256: String::new(), size: 4, data: Some(b"Man\0".to_vec()) };
        let json = serde_json::to_string(&found).unwrap();
        assert!(json.contains(r#""data":"TWFuAA==""#), "{json}");
        let bad = json.replace("TWFuAA==", "TWFu!A==");
        assert!(serde_json::from_str::<Found>(&bad).is_err());
    }
}
