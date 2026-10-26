//! Build a patch and undo journal entirely in memory, before device writes.
use super::{
    discovery,
    elf::{align, bytes, half, word, Elf},
    linker, Patch,
};
use crate::shell::sha256_hex;
use crate::{device, Result};
pub struct Built {
    pub output: Vec<u8>,
    pub journal: Vec<u8>,
    pub manifest: serde_json::Value,
}
fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn build(source: &[u8], object: &[u8]) -> Result<Built> {
    let discovery = discovery::discover(source)?;
    let elf = Elf::parse(source)?;
    let phoff = word(source, 28)? as usize;
    let stride = half(source, 42)? as usize;
    let count = half(source, 44)? as usize;
    if stride != 32 || count == 0 {
        return device("unsupported ELF program header table");
    }
    let original_headers = bytes(source, phoff, stride * count)?;
    let mut headers = Vec::new();
    for index in 0..count {
        let mut header = [0u32; 8];
        for (i, v) in header.iter_mut().enumerate() {
            *v = word(source, phoff + index * stride + i * 4)?;
        }
        headers.push(header);
    }
    let notes: Vec<_> = headers
        .iter()
        .enumerate()
        .filter(|(_, h)| h[0] == 4)
        .map(|(i, _)| i)
        .collect();
    if notes.len() != 1 {
        return device("expected one PT_NOTE program header");
    }
    let end = headers
        .iter()
        .filter(|h| h[0] == 1)
        .map(|h| {
            h[2].checked_add(h[5])
                .ok_or_else(|| crate::Error::Device("ELF load address overflow".into()))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .max()
        .ok_or_else(|| crate::Error::Device("no ELF load segments".into()))?;
    let address = u32::try_from(align(end as usize, 0x10000)?)
        .map_err(|_| crate::Error::Device("payload address overflow".into()))?;
    let offset = align(source.len(), 0x10000)?;
    let mut externals = discovery.native_api.clone();
    externals.entry("native_rename".into()).or_insert(0);
    externals.extend(
        discovery
            .layout
            .iter()
            .map(|(k, &v)| (format!("layout_{k}"), v)),
    );
    let linked = linker::link(object, address, &externals)?;
    let factory = discovery.factory;
    let factory_offset = elf.offset(factory)?;
    let old = bytes(source, factory_offset, 4)?;
    if word(old, 0)? >> 24 == 0xea {
        return device("account factory is already patched");
    }
    let delta = linked.entry as i64 - factory as i64 - 8;
    if delta % 4 != 0 || !(-(1 << 25)..(1 << 25)).contains(&delta) {
        return device("ARM factory branch out of range or unaligned");
    }
    let branch = 0xea000000 | ((delta >> 2) as u32 & 0xffffff);
    let (menu_address, menu_names) = discovery::settings(&elf)?;
    let menu_offset = elf.offset(menu_address)?;
    bytes(source, menu_offset, menu_names.len() * 4)?;
    let mut menu_words = Vec::new();
    for i in 0..menu_names.len() {
        menu_words.push(word(source, menu_offset + i * 4)?);
    }
    let account = menu_names
        .iter()
        .position(|n| n == "account")
        .ok_or_else(|| crate::Error::Device("settings menu has no account".into()))?;
    let account = menu_words.remove(account);
    menu_words.insert(1, account);
    let mut output = source.to_vec();
    output[factory_offset..factory_offset + 4].copy_from_slice(&branch.to_le_bytes());
    for bucket in ["device", "shared", "schedule"] {
        let slot = elf.offset(discovery.anchors[&format!("{bucket}_commit_slot")])?;
        let target = linked.symbols[&format!("custom_{bucket}_commit")];
        output[slot..slot + 4].copy_from_slice(&target.to_le_bytes());
    }
    for (i, w) in menu_words.iter().enumerate() {
        output[menu_offset + i * 4..menu_offset + i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    output.resize(offset, 0);
    output.extend(&linked.bytes);
    headers.remove(notes[0]);
    headers.push([
        1,
        offset as u32,
        address,
        address,
        linked.bytes.len() as u32,
        linked.bytes.len() as u32,
        5,
        0x10000,
    ]);
    for (index, header) in headers.iter().enumerate() {
        for (i, w) in header.iter().enumerate() {
            let at = phoff + index * stride + i * 4;
            output[at..at + 4].copy_from_slice(&w.to_le_bytes());
        }
    }
    let boot_branding = super::branding::recolor(source, &mut output)?;
    let manifest = serde_json::json!({
        "boot_branding":boot_branding,
        "status":"experimental; guest validation pending","stock_sha256":discovery.stock_sha256,
        "source_size":source.len(),"program_header_offset":phoff,"original_program_headers":hex(original_headers),
        "output_sha256":sha256_hex(&output),"replaced_screen":"account","factory":factory,
        "factory_file_offset":factory_offset,"original_factory_bytes":hex(old),
        "payload_address":address,"payload_offset":offset,"payload_size":linked.bytes.len(),"entry":linked.entry,
        "symbols":linked.symbols,"native_api":discovery.native_api,"discovery":discovery,
        "image":"/nestlabs/share/images/G_unfr0zen_192px.png","runtime_directory":"/var/run/unfr0zen","wheel_trace":false,
        "settings_menu":{"address":menu_address,"file_offset":menu_offset,"original":menu_names,"unfr0zen_index":1}
    });
    let journal = Patch::journal(source, &output)?;
    if Patch::parse(&journal)?.apply(source, false)? != output {
        return device("nlclient build and journal disagree");
    }
    Ok(Built {
        output,
        journal,
        manifest,
    })
}
