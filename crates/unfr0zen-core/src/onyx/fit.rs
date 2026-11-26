//! Builds the recovery FIT a Thermostat E boots from its factory USB boot
//! path: the unit's own signed kernel FIT with recovery U-Boot added beside
//! the signed content, so the loader's configuration selection picks the
//! recovery configuration and every signed byte stays identical.
//!
//! The placement follows the regions the loader hashes: recovery nodes go
//! where the structure block has no hashed coverage — the recovery
//! configuration as the first child of `/configurations`, the recovery
//! images as the last children of `/images`.

use std::collections::BTreeMap;

use crate::{Error, Result};

pub const FDT_BEGIN_NODE: u32 = 1;
pub const FDT_END_NODE: u32 = 2;
pub const FDT_PROP: u32 = 3;
pub const FDT_NOP: u32 = 4;
pub const FDT_END: u32 = 9;

/// The configuration name the loader's factory USB boot path asks for
/// (`nlmodel` in the whitelisted environment).
pub const TARGET_CONFIG: &str = "Display-4.x";

/// The recovery image names inside the built FIT.
pub const RECOVERY_KERNEL: &str = "recovery";
pub const RECOVERY_FDT: &str = "recovery-fdt";

/// A parsed structure block: every node by path, with its start and end
/// (the FDT_END_NODE tag's offset). Offsets are absolute into the FIT.
struct Structure<'a> {
    data: &'a [u8],
    off_strings: usize,
    nodes: BTreeMap<String, usize>,
    ends: BTreeMap<usize, usize>,
}

impl<'a> Structure<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() < 40 || data[..4] != [0xd0, 0x0d, 0xfe, 0xed] {
            return Err(Error::Device("not a device tree (no FDT magic)".into()));
        }
        let word = |at: usize| u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        let (off_struct, off_strings) = (word(8), word(12));
        let mut nodes = BTreeMap::new();
        let mut ends = BTreeMap::new();
        let mut path = String::new();
        let mut stack: Vec<usize> = Vec::new();
        let mut off = off_struct;
        loop {
            let tag = word_at(data, off)?;
            match tag {
                FDT_BEGIN_NODE => {
                    let name_end = padded_name_end(data, off + 4)?;
                    let name = String::from_utf8_lossy(name_at(data, off + 4)).into_owned();
                    let parent = path.clone();
                    if path != "/" {
                        path.push('/');
                    }
                    path.push_str(&name);
                    nodes.insert(path.clone(), off);
                    stack.push(parent.len());
                    off = name_end;
                }
                FDT_PROP => {
                    let len = word_at(data, off + 4)? as usize;
                    off += 12 + ((len + 3) & !3);
                }
                FDT_END_NODE => {
                    if let Some(&begin) = nodes.get(&path) {
                        ends.insert(begin, off);
                    }
                    path.truncate(stack.pop().unwrap_or_default());
                    off += 4;
                }
                FDT_NOP => off += 4,
                FDT_END => {
                    return Ok(Structure { data, off_strings, nodes, ends });
                }
                tag => {
                    return Err(Error::Device(format!("bad FDT tag {tag:#x} at {off:#x}")));
                }
            }
            if off > data.len() {
                return Err(Error::Device("FDT structure block runs past the image".into()));
            }
        }
    }

    fn node(&self, path: &str) -> Result<usize> {
        self.nodes.get(path).copied().ok_or_else(|| Error::Device(format!("node not found: {path}")))
    }

    /// A node's FDT_END_NODE tag offset.
    fn end_of(&self, node: usize) -> Result<usize> {
        self.ends.get(&node).copied().ok_or_else(|| Error::Device(format!("no end recorded for node at {node:#x}")))
    }

    fn depth_of(&self, path: &str) -> usize {
        path.split('/').filter(|p| !p.is_empty()).count()
    }

    /// The immediate children of a node: (name, start offset).
    fn children(&self, path: &str) -> Result<Vec<(String, usize)>> {
        let node = self.node(path)?;
        let depth = self.depth_of(path);
        let mut names: Vec<(String, usize)> = self
            .nodes
            .iter()
            .filter(|(other, _)| {
                let start = self.depth_of(other);
                start == depth + 1
                    && self.node(other).unwrap_or(usize::MAX) > node
                    && other.rsplit('/').next() != Some("")
                    && other.starts_with(path)
                    && other.len() > path.len()
                    && other.as_bytes()[path.len()] == b'/'
                    && !other[path.len() + 1..].contains('/')
            })
            .map(|(other, at)| (other.rsplit('/').next().unwrap().to_string(), *at))
            .collect();
        names.sort_by_key(|(_, at)| *at);
        Ok(names)
    }

    /// A child node's full bytes — begin tag through end tag inclusive.
    fn child_bytes(&self, path: &str, name: &str) -> Result<Vec<u8>> {
        for (child, at) in self.children(path)? {
            if child == name {
                let end = self.end_of(at)?;
                return Ok(self.data[at..end + 4].to_vec());
            }
        }
        Err(Error::Device(format!("child node not found: {name}")))
    }

    /// A property's (value offset, padded value length) inside a node.
    fn prop_value(&self, path: &str, name: &[u8]) -> Result<(usize, usize)> {
        let node = self.node(path)?;
        let mut at = padded_name_end(self.data, node + 4)?;
        let end = self.end_of(node)?;
        while at < end {
            let tag = word_at(self.data, at)?;
            match tag {
                FDT_PROP => {
                    let len = word_at(self.data, at + 4)? as usize;
                    let nameoff = word_at(self.data, at + 8)? as usize;
                    if name_at(self.data, self.off_strings + nameoff) == name {
                        return Ok((at + 12, (len + 3) & !3));
                    }
                    at += 12 + ((len + 3) & !3);
                }
                FDT_NOP => at += 4,
                _ => break,
            }
        }
        Err(Error::Device(format!("property not found in {path}")))
    }

    /// The offset of a property name in the strings table.
    fn string_offset(&self, name: &str) -> Result<u32> {
        let table = &self.data[self.off_strings..];
        let mut pos = 0;
        while pos < table.len() {
            let end = table[pos..]
                .iter()
                .position(|b| *b == 0)
                .map(|p| pos + p)
                .ok_or_else(|| Error::Device("unterminated string".into()))?;
            if &table[pos..end] == name.as_bytes() {
                return Ok(pos as u32);
            }
            pos = end + 1;
        }
        Err(Error::Device(format!("property name not in the strings table: {name}")))
    }
}

fn word_at(data: &[u8], at: usize) -> Result<u32> {
    if at + 4 > data.len() {
        return Err(Error::Device("FDT word past the image".into()));
    }
    Ok(u32::from_be_bytes(data[at..at + 4].try_into().unwrap()))
}

fn name_at(data: &[u8], at: usize) -> &[u8] {
    let end = data[at..].iter().position(|b| *b == 0).map(|p| at + p).unwrap_or(data.len());
    &data[at..end]
}

fn padded_name_end(data: &[u8], at: usize) -> Result<usize> {
    let end = data[at..]
        .iter()
        .position(|b| *b == 0)
        .map(|p| at + p)
        .ok_or_else(|| Error::Device("unterminated node name".into()))?;
    Ok(end + 1 + (4 - (end + 1 - at) % 4) % 4)
}

fn pad4(len: usize) -> usize {
    (len + 3) & !3
}

fn push_prop(out: &mut Vec<u8>, nameoff: u32, value: &[u8]) {
    out.extend_from_slice(&FDT_PROP.to_be_bytes());
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(&nameoff.to_be_bytes());
    out.extend_from_slice(value);
    for _ in value.len()..pad4(value.len()) {
        out.push(0);
    }
}

fn push_node_name(out: &mut Vec<u8>, name: &str) {
    out.extend_from_slice(&FDT_BEGIN_NODE.to_be_bytes());
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    out.resize(pad4(out.len()), 0);
}

/// The recovery payload the built FIT boots.
pub struct RecoveryImage<'a> {
    pub kernel: &'a [u8],
    pub fdt: &'a [u8],
    pub load: u32,
    pub entry: u32,
}

/// Builds the recovery FIT: the donor with the recovery configuration and its
/// images added beside the signed content. The recovery configuration carries
/// a byte-identical copy of a genuine configuration's `signature@1`, whose
/// hashed paths resolve to the same unchanged bytes in the built FIT.
pub fn build_recovery_fit(donor: &[u8], image: &RecoveryImage, target: &str) -> Result<Vec<u8>> {
    let fdt = Structure::parse(donor)?;
    fdt.node("/configurations")?;
    let images = fdt.node("/images")?;

    let mut signature: Option<Vec<u8>> = None;
    for (name, _) in fdt.children("/configurations")? {
        if let Some(sig) = fdt.child_bytes("/configurations", &name).ok().and_then(|c| signature_of(&fdt, &c)) {
            signature = Some(sig);
            break;
        }
    }
    let signature = signature.ok_or_else(|| Error::Device("donor has no signed configuration".into()))?;

    // The recovery configuration, named so the loader's `#<target>` lookup
    // selects it: libfdt matches names up to the unit address, and the first
    // match wins.
    let mut conf = Vec::new();
    push_node_name(&mut conf, &format!("{target}@0"));
    push_prop(&mut conf, fdt.string_offset("description")?, b"unfr0zen recovery\0");
    push_prop(&mut conf, fdt.string_offset("kernel")?, format!("{RECOVERY_KERNEL}\0").as_bytes());
    push_prop(&mut conf, fdt.string_offset("fdt")?, format!("{RECOVERY_FDT}\0").as_bytes());
    conf.extend_from_slice(&signature);
    conf.extend_from_slice(&FDT_END_NODE.to_be_bytes());

    let mut kernel_image = Vec::new();
    push_node_name(&mut kernel_image, RECOVERY_KERNEL);
    push_prop(&mut kernel_image, fdt.string_offset("description")?, b"unfr0zen recovery kernel\0");
    push_prop(&mut kernel_image, fdt.string_offset("data")?, image.kernel);
    push_prop(&mut kernel_image, fdt.string_offset("type")?, b"kernel\0");
    push_prop(&mut kernel_image, fdt.string_offset("arch")?, b"arm\0");
    push_prop(&mut kernel_image, fdt.string_offset("os")?, b"linux\0");
    push_prop(&mut kernel_image, fdt.string_offset("compression")?, b"none\0");
    push_prop(&mut kernel_image, fdt.string_offset("load")?, &image.load.to_be_bytes());
    push_prop(&mut kernel_image, fdt.string_offset("entry")?, &image.entry.to_be_bytes());
    kernel_image.extend_from_slice(&FDT_END_NODE.to_be_bytes());

    let mut fdt_image = Vec::new();
    push_node_name(&mut fdt_image, RECOVERY_FDT);
    push_prop(&mut fdt_image, fdt.string_offset("description")?, b"unfr0zen recovery fdt\0");
    push_prop(&mut fdt_image, fdt.string_offset("data")?, image.fdt);
    push_prop(&mut fdt_image, fdt.string_offset("type")?, b"flat_dt\0");
    push_prop(&mut fdt_image, fdt.string_offset("arch")?, b"arm\0");
    push_prop(&mut fdt_image, fdt.string_offset("compression")?, b"none\0");
    fdt_image.extend_from_slice(&FDT_END_NODE.to_be_bytes());

    // The recovery configuration is reachable only through the unsigned
    // `default` property: the loader's `#<target>` lookup matches names
    // exactly, so the fallback `bootm ${fit_addr}` must resolve to it.
    // `default` is outside every signed region, and the replacement value is
    // the same length as the factory's. The flip happens before the
    // insertions so its offsets are the donor's.
    let (at, room) = fdt.prop_value("/configurations", b"default")?;
    let value = format!("{target}@0\0").into_bytes();
    if value.len() > room {
        return Err(Error::Device("recovery configuration name overflows the default property".into()));
    }
    let mut out = donor.to_vec();
    out[at..at + room].fill(0);
    out[at..at + value.len()].copy_from_slice(&value);

    // Insert from the highest offset down: the recovery images just before
    // /images' end tag, the recovery configuration before the first child of
    // /configurations.
    let at_images = fdt.end_of(images)?;
    let at_conf = fdt
        .children("/configurations")?
        .first()
        .map(|(_, at)| *at)
        .ok_or_else(|| Error::Device("donor has no configurations".into()))?;
    let grown = conf.len() + kernel_image.len() + fdt_image.len();

    // Insert from the highest offset down, so the earlier offsets stay valid
    // in the mutating buffer.
    if at_conf > at_images {
        out.splice(at_conf..at_conf, conf.iter().copied());
        out.splice(at_images..at_images, kernel_image.iter().copied().chain(fdt_image.iter().copied()));
    } else {
        out.splice(at_images..at_images, kernel_image.iter().copied().chain(fdt_image.iter().copied()));
        out.splice(at_conf..at_conf, conf.iter().copied());
    }

    // The structure block and the image grow by what was inserted; the
    // strings table moves past them.
    let put = |out: &mut Vec<u8>, at: usize, value: u32| out[at..at + 4].copy_from_slice(&value.to_be_bytes());
    let struct_size = u32::from_be_bytes(donor[0x24..0x28].try_into().unwrap());
    put(&mut out, 0x24, struct_size + grown as u32);
    let total = u32::from_be_bytes(donor[4..8].try_into().unwrap());
    put(&mut out, 4, total + grown as u32);
    let strings = u32::from_be_bytes(donor[0x0c..0x10].try_into().unwrap());
    put(&mut out, 0x0c, strings + grown as u32);
    Ok(out)
}

/// A configuration's `signature@1` child, or any `signature@*` when the name
/// differs.
fn signature_of(_fdt: &Structure, conf: &[u8]) -> Option<Vec<u8>> {
    for (name, _) in direct_children(conf)? {
        if name.starts_with("signature@") {
            return subtree_bytes(conf, &name);
        }
    }
    None
}

/// The immediate children of a raw subtree's bytes: (name, offset in `conf`).
fn direct_children(conf: &[u8]) -> Option<Vec<(String, usize)>> {
    let mut names = Vec::new();
    let mut depth = 0usize;
    if word_at(conf, 0).ok()? != FDT_BEGIN_NODE {
        return None;
    }
    let mut pos = padded_name_end(conf, 4).ok()?;
    loop {
        let tag = u32::from_be_bytes(conf.get(pos..pos + 4)?.try_into().ok()?);
        match tag {
            FDT_BEGIN_NODE => {
                let end = conf[pos + 4..].iter().position(|b| *b == 0)? + pos + 4;
                let name = std::str::from_utf8(&conf[pos + 4..end]).ok()?.to_string();
                if depth == 0 {
                    names.push((name, pos));
                }
                depth += 1;
                pos = end + 1 + (4 - (end + 1 - pos) % 4) % 4;
            }
            FDT_PROP => {
                let len = u32::from_be_bytes(conf.get(pos + 4..pos + 8)?.try_into().ok()?) as usize;
                pos += 12 + ((len + 3) & !3);
            }
            FDT_END_NODE => {
                if depth == 0 {
                    return Some(names);
                }
                depth -= 1;
                pos += 4;
            }
            FDT_NOP => pos += 4,
            _ => return None,
        }
        if pos > conf.len() {
            return None;
        }
    }
}

/// A named child's bytes inside a raw subtree.
fn subtree_bytes(conf: &[u8], name: &str) -> Option<Vec<u8>> {
    let children = direct_children(conf)?;
    let start = children.iter().find(|(child, _)| child == name)?.1;
    // Walk from `start` to its matching end tag.
    let mut pos = start;
    let mut depth = 0isize;
    loop {
        let tag = u32::from_be_bytes(conf.get(pos..pos + 4)?.try_into().ok()?);
        match tag {
            FDT_BEGIN_NODE => {
                let end = conf[pos + 4..].iter().position(|b| *b == 0)? + pos + 4;
                depth += 1;
                pos = end + 1 + (4 - (end + 1 - pos) % 4) % 4;
            }
            FDT_PROP => {
                let len = u32::from_be_bytes(conf.get(pos + 4..pos + 8)?.try_into().ok()?) as usize;
                pos += 12 + ((len + 3) & !3);
            }
            FDT_END_NODE => {
                depth -= 1;
                if depth == 0 {
                    return Some(conf[start..pos + 4].to_vec());
                }
                pos += 4;
            }
            FDT_NOP => pos += 4,
            _ => return None,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A donor in the shape the units carry: `/images` with a kernel and two
    /// device trees, `/configurations` with signed configurations, the first
    /// one self-signed.
    pub(crate) fn donor() -> Vec<u8> {
        let mut strings: Vec<u8> = Vec::new();
        let string = |strings: &mut Vec<u8>, name: &str| -> u32 {
            let at = strings.len() as u32;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            at
        };
        let description = string(&mut strings, "description");
        let kernel = string(&mut strings, "kernel");
        let fdt_name = string(&mut strings, "fdt");
        let data = string(&mut strings, "data");
        let kind = string(&mut strings, "type");
        let arch = string(&mut strings, "arch");
        let os = string(&mut strings, "os");
        let compression = string(&mut strings, "compression");
        let load = string(&mut strings, "load");
        let entry = string(&mut strings, "entry");
        let value = string(&mut strings, "value");
        let algo = string(&mut strings, "algo");
        let hashed_nodes = string(&mut strings, "hashed-nodes");
        let hashed_strings = string(&mut strings, "hashed-strings");
        let key_hint = string(&mut strings, "key-name-hint");
        let sign_images = string(&mut strings, "sign-images");
        let default = string(&mut strings, "default");

        let mut body = Vec::new();
        body.extend_from_slice(&FDT_BEGIN_NODE.to_be_bytes());
        body.push(0);
        body.extend_from_slice(&[0; 3]);

        let image = |name: &str, payload: &[u8], out: &mut Vec<u8>| {
            push_node_name(out, name);
            push_prop(out, description, b"a stock image\0");
            push_prop(out, data, payload);
            push_prop(out, kind, b"kernel\0");
            push_prop(out, arch, b"arm\0");
            push_prop(out, os, b"linux\0");
            push_prop(out, compression, b"none\0");
            push_prop(out, load, &0x8000_8000u32.to_be_bytes());
            push_prop(out, entry, &0x8000_8000u32.to_be_bytes());
            out.extend_from_slice(&FDT_END_NODE.to_be_bytes());
        };
        push_node_name(&mut body, "images");
        image("kernel@1", &[0xaa; 256], &mut body);
        image("fdt@1", &[0xbb; 64], &mut body);
        image("fdt@2", &[0xcc; 64], &mut body);
        body.extend_from_slice(&FDT_END_NODE.to_be_bytes());

        let conf = |name: &str, fdt_ref: &str, hashed: &[u8], out: &mut Vec<u8>| {
            push_node_name(out, name);
            push_prop(out, description, b"a stock configuration\0");
            push_prop(out, kernel, b"kernel@1\0");
            push_prop(out, fdt_name, format!("{fdt_ref}\0").as_bytes());
            // the signature subnode the loader's configuration check reads
            push_node_name(out, "signature@1");
            push_prop(out, hashed_nodes, hashed);
            push_prop(out, hashed_strings, &0u32.to_be_bytes());
            push_prop(out, value, &[0xee; 256]);
            push_prop(out, algo, b"sha256,rsa2048\0");
            push_prop(out, key_hint, b"01\0");
            push_prop(out, sign_images, b"kernel fdt\0");
            out.extend_from_slice(&FDT_END_NODE.to_be_bytes());
            out.extend_from_slice(&FDT_END_NODE.to_be_bytes());
        };
        push_node_name(&mut body, "configurations");
        push_prop(&mut body, default, b"Display-4.6@1\0");
        // the first configuration self-signs: its own node is hashed
        conf(
            "Display-4.6@1",
            "fdt@1",
            b"/\0/configurations/Display-4.6@1\0/images/kernel@1\0/images/kernel@1/hash@1\0",
            &mut body,
        );
        conf("Display-4.5@2", "fdt@2", b"/\0/images/kernel@1\0", &mut body);
        body.extend_from_slice(&FDT_END_NODE.to_be_bytes());
        body.extend_from_slice(&FDT_END_NODE.to_be_bytes());
        body.extend_from_slice(&FDT_END.to_be_bytes());

        let off_struct = 0x38usize;
        let mut out = Vec::new();
        out.extend_from_slice(&[0xd0, 0x0d, 0xfe, 0xed]);
        out.extend_from_slice(&0u32.to_be_bytes()); // totalsize, fixed below
        out.extend_from_slice(&(off_struct as u32).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // off_strings, fixed below
        out.extend_from_slice(&0x28u32.to_be_bytes());
        out.extend_from_slice(&17u32.to_be_bytes());
        out.resize(off_struct, 0);
        assert_eq!(out.len(), off_struct);
        out.extend_from_slice(&body);
        let off_strings = out.len() as u32;
        out.extend_from_slice(&strings);
        let total = out.len() as u32;
        out[4..8].copy_from_slice(&total.to_be_bytes());
        out[12..16].copy_from_slice(&off_strings.to_be_bytes());
        out
    }

    fn recovery() -> RecoveryImage<'static> {
        static KERNEL: [u8; 128] = [0x42; 128];
        static FDT: [u8; 32] = [0x43; 32];
        RecoveryImage { kernel: &KERNEL, fdt: &FDT, load: 0x8200_0000, entry: 0x8200_0000 }
    }

    #[test]
    fn the_built_fit_selects_the_recovery_configuration_first() {
        let built = build_recovery_fit(&donor(), &recovery(), TARGET_CONFIG).unwrap();
        let fdt = Structure::parse(&built).unwrap();
        let first = fdt.children("/configurations").unwrap().remove(0);
        assert_eq!(first.0, "Display-4.x@0");
        // The loader's `#<target>` lookup matches names exactly, so the
        // recovery configuration must be what the unsigned `default`
        // property resolves to.
        let fdt = Structure::parse(&built).unwrap();
        let (at, _) = fdt.prop_value("/configurations", b"default").unwrap();
        assert_eq!(&built[at..at + format!("{TARGET_CONFIG}@0\0").len()], format!("{TARGET_CONFIG}@0\0").as_bytes());
        assert_eq!(fdt.children("/images").unwrap().pop().map(|(n, _)| n), Some(RECOVERY_FDT.to_string()));
    }

    #[test]
    fn the_recovery_configuration_carries_the_genuine_signature() {
        let donor = donor();
        let fdt = Structure::parse(&donor).unwrap();
        let genuine = fdt.child_bytes("/configurations", "Display-4.6@1").unwrap();
        let genuine = signature_of(&fdt, &genuine).unwrap();
        let built = build_recovery_fit(&donor, &recovery(), TARGET_CONFIG).unwrap();
        let fdt = Structure::parse(&built).unwrap();
        let recovery = fdt.child_bytes("/configurations", "Display-4.x@0").unwrap();
        let copied = signature_of(&fdt, &recovery).unwrap();
        assert_eq!(genuine, copied);
        // And the copied signature is byte-identical inside the built FIT.
        let at = built.windows(genuine.len()).position(|w| w == genuine).unwrap();
        assert!(at > donor.len() / 2); // it lives in the inserted configuration
    }

    #[test]
    fn every_signed_byte_stays_identical_in_its_new_location() {
        // The bytes of the donor's structure block, minus its 40-byte header
        // area, reappear unchanged after the insertions; the header's counts
        // grow by exactly what was added.
        let donor = donor();
        let built = build_recovery_fit(&donor, &recovery(), TARGET_CONFIG).unwrap();
        let grown = built.len() as u32 - u32::from_be_bytes(donor[4..8].try_into().unwrap());
        assert_eq!(
            grown,
            u32::from_be_bytes(built[4..8].try_into().unwrap()) - u32::from_be_bytes(donor[4..8].try_into().unwrap())
        );
        let strings = u32::from_be_bytes(donor[0x0c..0x10].try_into().unwrap()) as usize;
        assert_eq!(&donor[strings..], &built[strings + grown as usize..]);
        // The structure block's bytes up to the first insertion are identical.
        let d = Structure::parse(&donor).unwrap();
        let at_images = d.end_of(d.node("/images").unwrap()).unwrap();
        assert_eq!(donor[0x38..at_images], built[0x38..at_images]);
    }

    #[test]
    fn the_loader_hashes_resolve_to_unchanged_bytes() {
        // The hashed paths of the copied signature must still resolve, and
        // each hashed node's bytes must match the donor's.
        let donor = donor();
        let built = build_recovery_fit(&donor, &recovery(), TARGET_CONFIG).unwrap();
        for (tree, prefix) in [(&donor, "Display-4.6@1"), (&built, "Display-4.6@1")] {
            let fdt = Structure::parse(tree).unwrap();
            assert!(fdt.node(&format!("/configurations/{prefix}")).is_ok());
            assert!(fdt.node("/images/kernel@1").is_ok());
            assert_eq!(fdt.node("/").unwrap(), 0x38);
        }
        let d = Structure::parse(&donor).unwrap();
        let b = Structure::parse(&built).unwrap();
        assert_eq!(b.child_bytes("/images", "kernel@1").unwrap(), d.child_bytes("/images", "kernel@1").unwrap());
    }
}

#[cfg(test)]
mod real_donor {
    use super::*;

    #[test]
    #[ignore = "requires ONYX_DONOR_FIT pointing to a private firmware FIT"]
    fn builds_a_recovery_fit_from_the_real_donor() {
        let path = std::env::var_os("ONYX_DONOR_FIT").expect("ONYX_DONOR_FIT is set");
        let donor = std::fs::read(path).unwrap();
        let recovery = RecoveryImage { kernel: &[0x42; 128], fdt: &[0x43; 32], load: 0x8200_0000, entry: 0x8200_0000 };
        let built = build_recovery_fit(&donor, &recovery, TARGET_CONFIG).unwrap();
        let fdt = Structure::parse(&built).unwrap();
        let first = fdt.children("/configurations").unwrap().remove(0);
        assert_eq!(first.0, "Display-4.x@0");
    }
}
