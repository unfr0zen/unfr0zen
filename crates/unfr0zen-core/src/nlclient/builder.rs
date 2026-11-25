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
pub fn build(source: &[u8], object: &[u8], sections: &[super::menu::Section]) -> Result<Built> {
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
    let notes: Vec<_> = headers.iter().enumerate().filter(|(_, h)| h[0] == 4).map(|(i, _)| i).collect();
    if notes.len() != 1 {
        return device("expected one PT_NOTE program header");
    }
    let end = headers
        .iter()
        .filter(|h| h[0] == 1)
        .map(|h| h[2].checked_add(h[5]).ok_or_else(|| crate::Error::Device("ELF load address overflow".into())))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .max()
        .ok_or_else(|| crate::Error::Device("no ELF load segments".into()))?;
    let address = u32::try_from(align(end as usize, 0x10000)?)
        .map_err(|_| crate::Error::Device("payload address overflow".into()))?;
    let offset = align(source.len(), 0x10000)?;
    let insertion = super::menu::insertion(&elf, sections)?;
    let mut externals = discovery.native_api.clone();
    let menu_bindings = std::collections::BTreeMap::from([
        ("native_menu_lookup".to_string(), insertion.lookup),
        ("native_menu_grow".into(), insertion.grow),
        ("native_menu_append".into(), insertion.grow),
        ("layout_MENU_OBJECT_REGISTER".into(), insertion.object_register),
        ("layout_MENU_NAMES_REGISTER".into(), insertion.names_register),
        ("layout_MENU_SCREENS_REGISTER".into(), insertion.screens_register),
        ("layout_MENU_NEXT_REGISTER".into(), insertion.next_register),
        ("layout_MENU_HAS_DOOM".into(), u32::from(sections.contains(&super::menu::Section::Doom))),
        ("layout_MENU_GROW_APPENDS".into(), u32::from(insertion.appends)),
    ]);
    externals.extend(menu_bindings.clone());
    externals.entry("native_rename".into()).or_insert(0);
    let handover = discovery::handover(&discovery);
    externals.extend(discovery::handover_externals(&discovery));
    let fb = discovery.framebuffer.as_ref().filter(|_| handover == 2);
    externals.extend(discovery.layout.iter().map(|(k, &v)| (format!("layout_{k}"), v)));
    let linked = linker::link(object, address, &externals)?;
    // Each entry's stock screen builds unfr0zen's instead (custom_root).
    let factories = discovery::screen_factories(&elf)?;
    let mut branches = Vec::new();
    for name in super::menu::hosts(source).iter().copied().chain([super::menu::schedule_host(source)]) {
        let factory =
            *factories.get(name).ok_or_else(|| crate::Error::Device(format!("no {name} screen registration")))?;
        let factory_offset = elf.offset(factory)?;
        let old = bytes(source, factory_offset, 4)?;
        if word(old, 0)? >> 24 == 0xea {
            return device(format!("{name} factory is already patched"));
        }
        let delta = linked.entry as i64 - factory as i64 - 8;
        if delta % 4 != 0 || !(-(1 << 25)..(1 << 25)).contains(&delta) {
            return device("ARM factory branch out of range or unaligned");
        }
        let branch = 0xea000000 | ((delta >> 2) as u32 & 0xffffff);
        branches.push((name, factory, factory_offset, hex(old), branch));
    }
    // Room for the payload from the start: growing a copy of the source would double it.
    let mut output = Vec::with_capacity(offset.max(source.len()) + linked.bytes.len());
    output.extend_from_slice(source);
    for (_, _, factory_offset, _, branch) in &branches {
        output[*factory_offset..factory_offset + 4].copy_from_slice(&branch.to_le_bytes());
    }
    let mut settings_menu = super::menu::arrange(source, &mut output, sections)?;
    let delta = linked.symbols["custom_menu_lookup"] as i64 - insertion.call as i64 - 8;
    if delta % 4 != 0 || !(-(1 << 25)..(1 << 25)).contains(&delta) {
        return device("ARM menu insertion call out of range or unaligned");
    }
    let at = elf.offset(insertion.call)?;
    output[at..at + 4].copy_from_slice(&(0xeb000000 | ((delta >> 2) as u32 & 0xffffff)).to_le_bytes());
    settings_menu["insertion"] = serde_json::json!(&insertion);
    settings_menu["bindings"] = serde_json::json!(&menu_bindings);
    for bucket in ["device", "shared", "schedule"] {
        let slot = elf.offset(discovery.anchors[&format!("{bucket}_commit_slot")])?;
        let target = linked.symbols[&format!("custom_{bucket}_commit")];
        output[slot..slot + 4].copy_from_slice(&target.to_le_bytes());
    }
    // The gen 2's flip goes through custom_flip, which shows nothing while
    // another process owns the display.
    if let Some(fb) = fb {
        let slot = elf.offset(fb.slot)?;
        output[slot..slot + 4].copy_from_slice(&linked.symbols["custom_flip"].to_le_bytes());
    }
    // nlclient's one eglSwapBuffers call goes through custom_swap, which
    // skips presenting while another process owns the display.
    if handover == 1 {
        let swap_call = discovery.anchors["swap_call_slot"];
        let swap_offset = elf.offset(swap_call)?;
        let delta = linked.symbols["custom_swap"] as i64 - swap_call as i64 - 8;
        if delta % 4 != 0 || !(-(1 << 25)..(1 << 25)).contains(&delta) {
            return device("ARM swap call out of range or unaligned");
        }
        let call = 0xeb000000 | ((delta >> 2) as u32 & 0xffffff);
        output[swap_offset..swap_offset + 4].copy_from_slice(&call.to_le_bytes());
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
    let manifest = serde_json::json!({
        "status":discovery.validation,"reviewed":discovery.variant.is_some(),"stock_sha256":discovery.stock_sha256,
        "source_size":source.len(),"program_header_offset":phoff,"original_program_headers":hex(original_headers),
        "output_sha256":sha256_hex(&output),
        "factories":branches.iter().map(|(name, factory, offset, old, _)| (name.to_string(), serde_json::json!({
            "address":factory,"file_offset":offset,"original_bytes":old}))).collect::<serde_json::Map<_, _>>(),
        "payload_address":address,"payload_offset":offset,"payload_size":linked.bytes.len(),"entry":linked.entry,
        "symbols":linked.symbols,"native_api":discovery.native_api,"discovery":discovery,
        "image":"/nestlabs/share/images/G_unfr0zen_192px.png","runtime_directory":"/var/run/unfr0zen","wheel_trace":false,
        "handover":handover,"dial":discovery.dial,"swap_call":discovery.anchors.get("swap_call_slot"),"settings_menu":settings_menu
    });
    let journal = Patch::journal(source, &output)?;
    if !Patch::parse(&journal)?.reproduces(source, &output) {
        return device("nlclient build and journal disagree");
    }
    Ok(Built { output, journal, manifest })
}
