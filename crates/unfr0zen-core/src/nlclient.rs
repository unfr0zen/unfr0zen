//! Build the native account-screen replacement from the unit's own ELF.
//! Keep hash-guarded byte edits as an undo journal for rollback and upgrades.
mod branding;
mod elf;
mod arm;
pub mod discovery;
pub mod builder;
pub mod linker;
use std::io::Read;
use sha2::Digest;
use crate::{device, Result};
use crate::shell::sha256_hex;

pub const BINARY: &str = "nestlabs/sbin/nlclient";
pub const ORIGINAL: &str = "etc/unfr0zen/nlclient.original";
pub const JOURNAL: &str = "etc/unfr0zen/nlclient.undo.gz";
pub const ICON: &str = "nestlabs/share/images/G_unfr0zen_192px.png";
pub const ICON_BYTES: &[u8] = include_bytes!("../../../platform/common/nlclient/assets/unfr0zen.png");
pub const GLYPH: &str = "nestlabs/share/images/G_unfr0zen_glyph_80px.png";
pub const GLYPH_BYTES: &[u8] = include_bytes!("../../../platform/common/nlclient/assets/unfr0zen-glyph.png");

pub struct Edit { pub offset: usize, pub before: Vec<u8>, pub after: Vec<u8> }
pub struct Patch {
    pub stock_len: usize,
    pub output_len: usize,
    pub stock_sha: String,
    pub output_sha: String,
    pub edits: Vec<Edit>,
}
impl Patch {
    pub fn parse(bundle: &[u8]) -> Result<Self> {
        let mut data = Vec::new();
        flate2::read::GzDecoder::new(bundle).take(1 << 20).read_to_end(&mut data)?;
        let mut input = data.as_slice();
        fn take(input: &mut &[u8], n: usize) -> Result<Vec<u8>> {
            if input.len() < n { return device("truncated nlclient patch"); }
            let (head, tail) = input.split_at(n); *input = tail; Ok(head.to_vec())
        }
        fn word(input: &mut &[u8]) -> Result<usize> {
            Ok(u32::from_le_bytes(take(input, 4)?.try_into().unwrap()) as usize)
        }
        if take(&mut input, 4)? != b"UFP1" { return device("invalid nlclient patch"); }
        let stock_len = word(&mut input)?;
        let output_len = word(&mut input)?;
        let hex = |v: Vec<u8>| v.iter().map(|b| format!("{b:02x}")).collect();
        let stock_sha = hex(take(&mut input, 32)?);
        let output_sha = hex(take(&mut input, 32)?);
        let count = word(&mut input)?;
        let mut edits = Vec::new();
        for _ in 0..count {
            let offset = word(&mut input)?;
            let before = word(&mut input)?;
            let after = word(&mut input)?;
            if offset.checked_add(before).is_none_or(|end| end > stock_len) ||
               offset.checked_add(after).is_none_or(|end| end > output_len) ||
               (offset != stock_len && before != after) {
                return device("invalid nlclient patch bounds");
            }
            edits.push(Edit { offset, before: take(&mut input, before)?, after: take(&mut input, after)? });
        }
        if !input.is_empty() { return device("trailing nlclient patch data"); }
        Ok(Self { stock_len, output_len, stock_sha, output_sha, edits })
    }
    /// The journal that turns `stock` into `output` and back: each run of
    /// changed bytes within the stock, then everything appended past it. The
    /// output is the stock with edits and a tail, never shorter.
    pub fn journal(stock: &[u8], output: &[u8]) -> Result<Vec<u8>> {
        use std::io::Write;
        if output.len() < stock.len() { return device("nlclient output is shorter than its stock"); }
        let mut edits = Vec::new();
        let mut pos = 0;
        while pos < stock.len() {
            if stock[pos] == output[pos] { pos += 1; continue; }
            let start = pos;
            while pos < stock.len() && stock[pos] != output[pos] { pos += 1; }
            edits.push((start, &stock[start..pos], &output[start..pos]));
        }
        edits.push((stock.len(), &[][..], &output[stock.len()..]));
        let word = |n: usize| (n as u32).to_le_bytes();
        let digest = |data: &[u8]| -> [u8; 32] { sha2::Sha256::digest(data).into() };
        let mut raw = [&b"UFP1"[..], &word(stock.len()), &word(output.len()),
                       &digest(stock), &digest(output), &word(edits.len())].concat();
        for (offset, before, after) in edits {
            raw.extend([&word(offset)[..], &word(before.len()), &word(after.len()), before, after].concat());
        }
        let mut zip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        zip.write_all(&raw)?;
        Ok(zip.finish()?)
    }
    /// Undo the recorded old edits in the staged copy, then apply our current
    /// payload. Hashes guard both ends; the live binary is renamed only once.
    pub fn upgrade(previous: &Self, current: Self) -> Result<Self> {
        if previous.stock_sha != current.stock_sha || previous.stock_len != current.stock_len {
            return device("nlclient patch journal targets different firmware");
        }
        let mut edits: Vec<Edit> = previous.edits.iter().filter(|e| e.offset < previous.stock_len)
            .map(|e| Edit { offset: e.offset, before: e.after.clone(), after: e.before.clone() }).collect();
        edits.extend(current.edits);
        Ok(Self { stock_len: previous.output_len, output_len: current.output_len,
                  stock_sha: previous.output_sha.clone(), output_sha: current.output_sha, edits })
    }
    pub fn apply(&self, source: &[u8], reverse: bool) -> Result<Vec<u8>> {
        let (expected, result_hash, length) = if reverse {
            (&self.output_sha, &self.stock_sha, self.stock_len)
        } else { (&self.stock_sha, &self.output_sha, self.output_len) };
        if sha256_hex(source) != *expected { return device("nlclient firmware hash mismatch"); }
        let mut output = source.to_vec();
        output.resize(self.output_len, 0);
        for edit in &self.edits {
            let bytes = if reverse { &edit.before } else { &edit.after };
            output[edit.offset..edit.offset + bytes.len()].copy_from_slice(bytes);
        }
        output.truncate(length);
        if sha256_hex(&output) != *result_hash { return device("nlclient patched hash mismatch"); }
        Ok(output)
    }
}

/// Discover and link the embedded screen against the executable read from
/// this unit. All work stays in memory; no external tools are launched.
pub fn build(source: &[u8], object: &[u8]) -> Result<Vec<u8>> {
    log::info!("Discovering nlclient ABI and linking UNFR0ZEN settings in memory");
    Ok(builder::build(source, object)?.journal)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// The journal for stock -> output: each run
    /// of changed bytes, then the appended tail. Output must not be shorter.
    pub(crate) fn journal(stock: &[u8], output: &[u8]) -> Vec<u8> {
        Patch::journal(stock, output).unwrap()
    }

    pub(crate) fn gzip(raw: &[u8]) -> Vec<u8> {
        let mut zip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        zip.write_all(raw).unwrap();
        zip.finish().unwrap()
    }

    fn gunzip(bundle: &[u8]) -> Vec<u8> {
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(bundle).read_to_end(&mut raw).unwrap();
        raw
    }

    #[test]
    fn a_journal_needs_an_output_at_least_as_long_as_its_stock() {
        assert!(Patch::journal(b"stock bytes", b"stock").is_err());
        let same = Patch::parse(&Patch::journal(b"stock", b"stock").unwrap()).unwrap();
        assert_eq!(same.edits.len(), 1);
        assert!(same.edits[0].before.is_empty() && same.edits[0].after.is_empty());
    }

    #[test]
    fn a_journal_patches_the_stock_bytes_and_undoes_back_to_them() {
        let (stock, output) = (b"\x7fELF stock body", b"\x7fELF STOCK bodY and a tail");
        let patch = Patch::parse(&journal(stock, output)).unwrap();
        assert_eq!((patch.stock_len, patch.output_len), (stock.len(), output.len()));
        assert_eq!((patch.stock_sha.as_str(), patch.output_sha.as_str()), (sha256_hex(stock).as_str(), sha256_hex(output).as_str()));
        assert_eq!(patch.edits.iter().map(|e| e.offset).collect::<Vec<_>>(), [5, 14, stock.len()]);
        assert_eq!(patch.apply(stock, false).unwrap(), output);
        assert_eq!(patch.apply(output, true).unwrap(), stock);
    }

    #[test]
    fn malformed_journals_are_refused() {
        let raw = gunzip(&journal(b"abcd", b"aXcd12"));
        // Header 80 bytes; the edits: (1, "b", "X"), then (4, "", "12").
        let edit = |mut raw: Vec<u8>, at: usize, value: u32| { raw[80 + at..84 + at].copy_from_slice(&value.to_le_bytes()); raw };
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            ("magic", [&b"UFP2"[..], &raw[4..]].concat(), "invalid nlclient patch"),
            ("cut short", raw[..raw.len() - 1].to_vec(), "truncated"),
            ("trailing byte", [&raw[..], b"!"].concat(), "trailing"),
            ("edit past the stock", edit(raw.clone(), 0, 4), "bounds"),
            ("offset overflow", edit(raw.clone(), 0, u32::MAX), "bounds"),
            ("in-place edit that changes length", edit(raw.clone(), 8, 2), "bounds"),
            ("append that claims stock bytes", edit(raw.clone(), 18, 4), "bounds"),
            ("append past the output", edit(raw.clone(), 22, 3), "bounds"),
        ];
        for (what, raw, error) in cases {
            let failure = Patch::parse(&gzip(&raw)).err().unwrap_or_else(|| panic!("{what} accepted"));
            assert!(failure.to_string().contains(error), "{what}: {failure}");
        }
        assert!(Patch::parse(&raw).is_err(), "not gzip");
    }

    #[test]
    fn wrong_firmware_or_wrong_edits_never_produce_output() {
        let patch = Patch::parse(&journal(b"abcd", b"aXcd12")).unwrap();
        assert!(patch.apply(b"abce", false).unwrap_err().to_string().contains("firmware hash mismatch"));
        assert!(patch.apply(b"abcd", true).unwrap_err().to_string().contains("firmware hash mismatch"));
        let mut raw = gunzip(&journal(b"abcd", b"aXcd12"));
        raw[93] = b'Y'; // the first edit's after byte
        let patch = Patch::parse(&gzip(&raw)).unwrap();
        assert!(patch.apply(b"abcd", false).unwrap_err().to_string().contains("patched hash mismatch"));
    }

    #[test]
    fn upgrades_refuse_a_journal_for_other_firmware() {
        let previous = Patch::parse(&journal(b"abcd", b"aXcd12")).unwrap();
        let other = Patch::parse(&journal(b"abce", b"aXce12")).unwrap();
        assert!(Patch::upgrade(&previous, other).err().unwrap().to_string().contains("different firmware"));
        let longer = Patch::parse(&journal(b"abcde", b"aXcde12")).unwrap();
        assert!(Patch::upgrade(&previous, longer).is_err());
    }

    #[test]
    fn upgrading_to_the_same_payload_changes_nothing() {
        let bundle = journal(b"abcdef", b"aXcdeY123");
        let update = Patch::upgrade(&Patch::parse(&bundle).unwrap(), Patch::parse(&bundle).unwrap()).unwrap();
        assert_eq!(update.stock_sha, update.output_sha);
        assert_eq!(update.apply(b"aXcdeY123", false).unwrap(), b"aXcdeY123");
    }

    #[test]
    fn upgrades_put_back_bytes_the_new_payload_no_longer_changes() {
        let previous = Patch::parse(&journal(b"abcdef", b"XbcdeY1")).unwrap();
        let current = Patch::parse(&journal(b"abcdef", b"aZcdef2345")).unwrap();
        let update = Patch::upgrade(&previous, current).unwrap();
        assert_eq!(update.apply(b"XbcdeY1", false).unwrap(), b"aZcdef2345");
    }

    #[test]
    fn upgrades_restore_original_edits_and_replace_a_longer_payload() {
        let previous = Patch { stock_len: 3, output_len: 8,
            stock_sha: sha256_hex(b"abc"), output_sha: sha256_hex(b"xbc12345"),
            edits: vec![Edit { offset: 0, before: b"a".to_vec(), after: b"x".to_vec() },
                        Edit { offset: 3, before: vec![], after: b"12345".to_vec() }] };
        let current = Patch { stock_len: 3, output_len: 6,
            stock_sha: sha256_hex(b"abc"), output_sha: sha256_hex(b"ayc678"),
            edits: vec![Edit { offset: 1, before: b"b".to_vec(), after: b"y".to_vec() },
                        Edit { offset: 3, before: vec![], after: b"678".to_vec() }] };
        let update = Patch::upgrade(&previous, current).unwrap();
        assert_eq!(update.apply(b"xbc12345", false).unwrap(), b"ayc678");
        assert!(update.apply(b"xbc12340", false).is_err());
    }
}
