//! nlclient's settings file (/media/user-config/settings.config): one
//! `<s key="..." value="..."/>` per line inside `<settings>`. nlclient reads
//! it at start and writes it back whole, keeping every key it read; nestlocal
//! edits it only before nlclient starts (nestlocal.init).

use std::io;
use std::path::Path;

/// Every `<s key="<key>" value="..."/>` in b: its span and value.
pub fn entries<'a>(b: &'a [u8], key: &str) -> Vec<(usize, usize, &'a [u8])> {
    let open = format!(r#"<s key="{key}" value=""#).into_bytes();
    let close = br#""/>"#;
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(i) = find(&b[at..], &open) {
        let start = at + i;
        let value_start = start + open.len();
        let value_len = b[value_start..].iter().position(|&c| c == b'"').unwrap_or(b.len() - value_start);
        let end = value_start + value_len;
        if b[end..].starts_with(close) {
            found.push((start, end + close.len(), &b[value_start..end]));
            at = end + close.len();
        } else {
            at = start + 1;
        }
    }
    found
}

/// The first value key has.
pub fn value<'a>(b: &'a [u8], key: &str) -> Option<&'a [u8]> {
    entries(b, key).first().map(|(_, _, value)| *value)
}

/// b with every entry of key set to value, or with one added before the
/// last `</settings>` when it has none.
pub fn set(b: &[u8], key: &str, value: &str) -> io::Result<Vec<u8>> {
    let line = format!(r#"<s key="{key}" value="{value}"/>"#).into_bytes();
    let found = entries(b, key);
    if found.is_empty() {
        let end = b.windows(11).rposition(|w| w == b"</settings>")
            .ok_or_else(|| io::Error::other("no </settings>"))?;
        let mut out = b[..end].to_vec();
        out.extend_from_slice(b"  ");
        out.extend_from_slice(&line);
        out.push(b'\n');
        out.extend_from_slice(&b[end..]);
        return Ok(out);
    }
    let mut out = Vec::with_capacity(b.len());
    let mut at = 0;
    for (start, end, _) in &found {
        out.extend_from_slice(&b[at..*start]);
        out.extend_from_slice(&line);
        at = *end;
    }
    out.extend_from_slice(&b[at..]);
    Ok(out)
}

/// Replaces the file at path with b, through a sibling and a rename.
pub fn write(path: &Path, b: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".nestlocal");
    std::fs::write(&tmp, b)?;
    std::fs::rename(&tmp, path)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "<settings>\n  <s key=\"a\" value=\"1\"/>\n  <s key=\"b\" value=\"\"/>\n</settings>\n";

    #[test]
    fn value_reads_the_first_entry() {
        assert_eq!(value(FILE.as_bytes(), "a"), Some(&b"1"[..]));
        assert_eq!(value(FILE.as_bytes(), "b"), Some(&b""[..]));
        assert_eq!(value(FILE.as_bytes(), "c"), None);
        // A key that only ends like another is not it.
        assert_eq!(value(br#"<s key="xa" value="2"/>"#, "a"), None);
    }

    #[test]
    fn set_replaces_or_adds() {
        let out = set(FILE.as_bytes(), "a", "0").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), FILE.replace(r#""a" value="1""#, r#""a" value="0""#));
        let out = set(FILE.as_bytes(), "c", "1").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(),
                   FILE.replace("</settings>", "  <s key=\"c\" value=\"1\"/>\n</settings>"));
        assert!(set(b"<settings>\n", "c", "1").is_err());
    }
}
