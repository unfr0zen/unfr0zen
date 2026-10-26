//! The unfr0zen initramfs: a gzipped newc cpio with a static ARMv7 busybox,
//! scripts/initramfs-init as /init, and a payload of contents the patch can
//! copy on the unit. Port of build_initramfs in unfr0zen.py.

use std::collections::BTreeMap;
use std::io::Write;

use flate2::write::GzEncoder;
use flate2::Compression;

/// The same /init unfr0zen.py's initramfs runs.
pub const INIT: &[u8] = include_bytes!("../../../../platform/diamond3/initramfs-init");

const DIRECTORY: u32 = 0o040000;
const REGULAR: u32 = 0o100000;
const SYMLINK: u32 = 0o120000;
const CHARACTER: u32 = 0o020000;

pub struct Entry {
    pub name: String,
    pub mode: u32,
    pub data: Vec<u8>,
    pub rdev: (u32, u32),
}

impl Entry {
    fn new(name: &str, mode: u32, data: &[u8]) -> Self {
        Entry { name: name.to_string(), mode, data: data.to_vec(), rdev: (0, 0) }
    }
}

/// A newc cpio archive, as the kernel unpacks it.
pub fn cpio(entries: &[Entry]) -> Vec<u8> {
    let trailer = Entry::new("TRAILER!!!", 0, b"");
    let mut output = Vec::new();
    for (inode, entry) in entries.iter().chain([&trailer]).enumerate() {
        let name_size = entry.name.len() + 1;
        let fields = [inode as u32 + 1, entry.mode, 0, 0, 1, 0, entry.data.len() as u32, 0, 0,
                      entry.rdev.0, entry.rdev.1, name_size as u32, 0];
        output.extend_from_slice(b"070701");
        for field in fields {
            output.extend_from_slice(format!("{field:08x}").as_bytes());
        }
        output.extend_from_slice(entry.name.as_bytes());
        output.push(0);
        output.resize(output.len().next_multiple_of(4), 0);
        output.extend_from_slice(&entry.data);
        output.resize(output.len().next_multiple_of(4), 0);
    }
    output
}

/// The gzipped initramfs. payload: {sha256: contents}, as /payload/<sha256>.
pub fn build(busybox: &[u8], payload: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut entries: Vec<Entry> = ["bin", "dev", "lib", "mnt", "payload", "proc", "sys"].iter()
        .map(|name| Entry::new(name, DIRECTORY | 0o755, b"")).collect();
    // The kernel opens /dev/console for /init before devtmpfs is mounted.
    entries.push(Entry { rdev: (5, 1), ..Entry::new("dev/console", CHARACTER | 0o600, b"") });
    entries.push(Entry::new("bin/busybox", REGULAR | 0o755, busybox));
    entries.push(Entry::new("bin/sh", SYMLINK | 0o777, b"busybox"));
    entries.push(Entry::new("init", REGULAR | 0o755, INIT));
    entries.extend(payload.iter().map(|(sha, data)| Entry::new(&format!("payload/{sha}"), REGULAR | 0o644, data)));
    // No name and a zero mtime: the same inputs give the same bytes.
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(&cpio(&entries)).expect("writing to memory");
    gzip.finish().expect("writing to memory")
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::GzDecoder;
    use std::io::Read;

    type Members = BTreeMap<String, (u32, Vec<u8>, (u32, u32))>;

    /// (mode, data, rdev) of each member, as the kernel would unpack them.
    fn members(archive: &[u8]) -> Members {
        let mut found = BTreeMap::new();
        let mut offset = 0;
        loop {
            let field = |i: usize| u32::from_str_radix(
                std::str::from_utf8(&archive[offset + 6 + i * 8..offset + 14 + i * 8]).unwrap(), 16).unwrap();
            assert_eq!(&archive[offset..offset + 6], b"070701");
            let (mode, size, rdev, name_size) = (field(1), field(6) as usize, (field(9), field(10)), field(11) as usize);
            let name = String::from_utf8(archive[offset + 110..offset + 110 + name_size - 1].to_vec()).unwrap();
            offset = (offset + 110 + name_size).next_multiple_of(4);
            let data = archive[offset..offset + size].to_vec();
            offset = (offset + size).next_multiple_of(4);
            if name == "TRAILER!!!" {
                return found;
            }
            found.insert(name, (mode, data, rdev));
        }
    }

    #[test]
    fn initramfs_layout() {
        let payload = BTreeMap::from([("ab".repeat(32), b"blob".to_vec())]);
        let mut archive = Vec::new();
        GzDecoder::new(&build(b"\x7fELF busybox", &payload)[..]).read_to_end(&mut archive).unwrap();
        let members = members(&archive);
        assert_eq!(members["init"].1, INIT);
        assert_eq!(members["init"].0, REGULAR | 0o755);
        assert_eq!((members["bin/sh"].0, &members["bin/sh"].1[..]), (SYMLINK | 0o777, &b"busybox"[..]));
        assert_eq!((members["dev/console"].0, members["dev/console"].2), (CHARACTER | 0o600, (5, 1)));
        assert_eq!(members[&format!("payload/{}", "ab".repeat(32))].1, b"blob");
        for name in ["lib", "mnt", "payload", "proc", "sys"] {
            assert_eq!(members[name].0, DIRECTORY | 0o755, "{name}");
        }
    }

    #[test]
    fn the_same_inputs_give_the_same_initramfs() {
        assert_eq!(build(b"busybox", &BTreeMap::new()), build(b"busybox", &BTreeMap::new()));
    }
}
