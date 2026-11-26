//! Exercise discovery and reversible patching against locally supplied stock binaries.
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use unfr0zen_core::nlclient::{self, linker, menu::Section};

fn root() -> PathBuf {
    Path::new(env!("UNFR0ZEN_ROOT")).to_owned()
}
fn firmware_dir() -> PathBuf {
    std::env::var_os("NLCLIENT_FIRMWARE_DIR").map(PathBuf::from).unwrap_or_else(|| root().join(".work/firmware"))
}
fn samples() -> Vec<PathBuf> {
    let dir = firmware_dir();
    let paths = match std::fs::read_dir(&dir) {
        Ok(paths) => paths,
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                && std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_none()
                && std::env::var_os("NLCLIENT_FIRMWARE_DIR").is_none() =>
        {
            return Vec::new()
        }
        Err(e) => panic!("{}: {e}", dir.display()),
    };
    let mut files: Vec<_> = paths.map(|p| p.unwrap().path()).filter(|p| p.is_file()).collect();
    if std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_some() || std::env::var_os("NLCLIENT_FIRMWARE_DIR").is_some() {
        assert!(!files.is_empty(), "{} contains no stock binaries", dir.display());
    }
    files.sort();
    files
}
fn payload(variable: &str, name: &str) -> Option<Vec<u8>> {
    let path = std::env::var_os(variable).map(PathBuf::from).unwrap_or_else(|| root().join("build").join(name));
    match std::fs::read(&path) {
        Ok(data) => Some(data),
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                && std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_none()
                && std::env::var_os(variable).is_none() =>
        {
            None
        }
        Err(e) => panic!("{}: {e}", path.display()),
    }
}
fn object() -> Option<Vec<u8>> {
    payload("NLCLIENT_OBJECT", "custom-screen.o")
}

#[test]
fn stock_factories_branch_to_the_linked_screen_and_undo_exactly() {
    for path in samples() {
        let stock = std::fs::read(&path).unwrap();
        let (object, sections) = if nlclient::legacy_abi(&stock) {
            (payload("NLCLIENT_LEGACY_OBJECT", "custom-screen-legacy.o"), Section::LEGACY.as_slice())
        } else {
            (object(), Section::ALL.as_slice())
        };
        let Some(object) = object else { continue };
        let built =
            nlclient::builder::build(&stock, &object, sections).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let factories = built.manifest["factories"].as_object().unwrap();
        assert!(!factories.is_empty());
        for factory in factories.values() {
            let address = factory["address"].as_u64().unwrap() as u32;
            let at = file_offset(&stock, address);
            let branch = w(&built.output, at);
            assert_eq!(branch >> 24, 0xea);
            let destination = address.wrapping_add(8).wrapping_add((((branch << 8) as i32) >> 6) as u32);
            assert_eq!(destination, built.manifest["entry"].as_u64().unwrap() as u32);
        }
        let mut bindings = synthetic_bindings(&object);
        for (name, value) in &mut bindings {
            *value = built.manifest["symbols"][name].as_u64().unwrap() as u32;
        }
        let address = built.manifest["payload_address"].as_u64().unwrap() as u32;
        let offset = file_offset(&built.output, address);
        compare_lld(&object, address, &bindings, &built.output[offset..]);
        if let Some(previous) = std::env::var_os("NLCLIENT_REFERENCE_BUILDER") {
            let tmp = tempfile::tempdir().unwrap();
            let screen = tmp.path().join("screen.o");
            let reference = tmp.path().join("patched");
            std::fs::write(&screen, &object).unwrap();
            let run = std::process::Command::new(previous).arg(&path).arg(screen).arg(&reference).output().unwrap();
            assert!(run.status.success(), "{}: {}", path.display(), String::from_utf8_lossy(&run.stderr));
            assert!(
                std::fs::read(&reference).unwrap() == built.output,
                "{}: patched executables differ",
                path.display()
            );
            let decompress = |gzip: &[u8]| {
                let mut data = Vec::new();
                std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(gzip), &mut data).unwrap();
                data
            };
            assert_eq!(
                decompress(&std::fs::read(reference.with_extension("undo.gz")).unwrap()),
                decompress(&built.journal),
                "{}: undo journals differ",
                path.display()
            );
        }
        let patch = nlclient::Patch::parse(&built.journal).unwrap();
        assert_eq!(patch.apply(&stock, false).unwrap(), built.output);
        assert_eq!(patch.apply(&built.output, true).unwrap(), stock);
    }
}
#[test]
fn invalid_objects_are_rejected() {
    assert!(linker::link(b"not ELF", 0x840000, &BTreeMap::new()).is_err());
    let Some(object) = object() else { return };
    assert!(linker::link(&object, 0x840000, &BTreeMap::new()).is_err());
    let mut wrong = object.clone();
    wrong[18] = 62;
    assert!(linker::link(&wrong, 0x840000, &BTreeMap::new()).is_err());
}

#[test]
fn linker_matches_lld_using_the_same_object() {
    for (variable, name) in [
        ("NLCLIENT_OBJECT", "custom-screen.o"),
        ("NLCLIENT_LEGACY_OBJECT", "custom-screen-legacy.o"),
        ("NLCLIENT_RCS_OBJECT", "j49-rcs-screen.o"),
    ] {
        let Some(object) = payload(variable, name) else { continue };
        let address = 0x510000;
        let externals = synthetic_bindings(&object);
        let linked = linker::link(&object, address, &externals).unwrap();
        compare_lld(&object, address, &externals, &linked.bytes);
    }
}

fn compare_lld(object: &[u8], address: u32, bindings: &BTreeMap<String, u32>, expected: &[u8]) {
    let lld = std::env::var_os("NLCLIENT_REFERENCE_LLD").unwrap_or_else(|| "ld.lld".into());
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let assignments =
        bindings.iter().map(|(name, value)| format!("{name} = {value:#x};")).collect::<Vec<_>>().join("\n");
    let script = format!(
        "{assignments}
SECTIONS {{
    . = {address:#x};
    .text : {{ *(.text*) }}
    .rodata : {{ *(.rodata*) }}
    /DISCARD/ : {{ *(.comment) *(.ARM.exidx*) *(.ARM.extab*) }}
}}"
    );
    std::fs::write(dir.join("payload.ld"), script).unwrap();
    std::fs::write(dir.join("screen.o"), object).unwrap();
    let result = std::process::Command::new(lld)
        .current_dir(dir)
        .args(["-O0", "-T", "payload.ld", "-e", "custom_factory", "--oformat=binary", "screen.o", "-o", "payload.bin"])
        .output();
    let output = match result {
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                && std::env::var_os("NLCLIENT_REFERENCE_LLD").is_none()
                && std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_none() =>
        {
            return
        }
        result => result.unwrap(),
    };
    assert!(output.status.success(), "ld.lld: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(expected, std::fs::read(dir.join("payload.bin")).unwrap());
}

fn stock_sample() -> Option<Vec<u8>> {
    let p = std::env::var_os("NLCLIENT_STOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|| firmware_dir().join("nlclient-gen3-2025"));
    match std::fs::read(&p) {
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                && std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_none()
                && std::env::var_os("NLCLIENT_STOCK").is_none() =>
        {
            None
        }
        result => Some(result.unwrap_or_else(|e| panic!("{}: {e}", p.display()))),
    }
}
fn w(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}
fn put(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn sections(data: &[u8]) -> Vec<(usize, u32, u32, u32, usize, usize)> {
    let sh = w(data, 32) as usize;
    let count = u16::from_le_bytes(data[48..50].try_into().unwrap()) as usize;
    (0..count)
        .map(|i| {
            let at = sh + i * 40;
            (
                at,
                w(data, at + 4),
                w(data, at + 8),
                w(data, at + 12),
                w(data, at + 16) as usize,
                w(data, at + 20) as usize,
            )
        })
        .collect()
}
fn va(data: &[u8], address: u32) -> usize {
    let (_, _, _, base, offset, _) = sections(data)
        .into_iter()
        .find(|(_, kind, _, base, _, size)| {
            *kind != 8 && *base != 0 && address >= *base && (address - *base) < *size as u32
        })
        .unwrap();
    offset + (address - base) as usize
}
#[test]
fn discovery_moves_with_the_elf_and_rejects_corrupted_abis() {
    let Some(stock) = stock_sample() else {
        eprintln!("Private 2025 sample unavailable");
        return;
    };
    let before = nlclient::discovery::discover(&stock).unwrap();
    for bucket in ["device", "shared", "schedule"] {
        let slot = before.anchors[&format!("{bucket}_commit_slot")];
        let relocation = sections(&stock)
            .into_iter()
            .filter(|(_, kind, _, _, _, _)| *kind == 9)
            .flat_map(|(_, _, _, _, offset, size)| (offset..offset + size).step_by(8))
            .find(|&at| w(&stock, at) == slot)
            .unwrap();
        let mut changed = stock.clone();
        put(&mut changed, relocation + 4, 2); // R_ARM_ABS32 does not preserve PIE load bias.
        assert!(nlclient::discovery::discover(&changed).is_err());
    }
    let mut changed = stock.clone();
    let at = va(&changed, before.native_api["native_screen_vtable"] + before.layout["SCREEN_SECOND_VPTR"] - 16);
    put(&mut changed, at, 0xfffffff8);
    assert!(nlclient::discovery::discover(&changed).is_err());
    let shift = 0x100000;
    let mut moved = stock.clone();
    let sections = sections(&stock);
    let spans: Vec<_> = sections
        .iter()
        .filter(|(_, _, flags, address, _, _)| flags & 2 != 0 && *address != 0)
        .map(|(_, _, _, a, _, s)| (*a, *a + *s as u32))
        .collect();
    for (_, kind, flags, _, offset, size) in &sections {
        if *kind == 1 && flags & 7 == 3 {
            for at in (*offset..offset + size).step_by(4) {
                let pointer = w(&moved, at);
                if spans.iter().any(|&(a, b)| (a..b).contains(&pointer)) {
                    put(&mut moved, at, pointer + shift);
                }
            }
        }
        if *kind == 9 {
            for at in (*offset..offset + size).step_by(8) {
                let pointer = w(&moved, at);
                put(&mut moved, at, pointer + shift);
            }
        }
    }
    for (header, _, flags, address, _, _) in sections {
        if flags & 2 != 0 && address != 0 {
            put(&mut moved, header + 12, address + shift);
        }
    }
    let after = nlclient::discovery::discover(&moved).unwrap();
    assert_eq!(after.layout, before.layout);
    assert_eq!(after.fingerprints, before.fingerprints);
    assert_eq!(after.factory, before.factory + shift);
    assert_eq!(after.native_api, before.native_api.into_iter().map(|(k, v)| (k, v + shift)).collect());
}

#[test]
fn unsupported_relocations_and_branch_veneers_are_refused() {
    let Some(object) = object() else { return };
    let mut externals = synthetic_bindings(&object);
    let mut changed = object.clone();
    let reloc = sections(&object).into_iter().find(|(_, kind, _, _, _, _)| *kind == 9).unwrap();
    let at = reloc.4 + 4;
    let info = w(&changed, at);
    put(&mut changed, at, (info & !255) | 255);
    assert!(linker::link(&changed, 0x510000, &externals).err().unwrap().to_string().contains("relocation 255"));
    externals.insert("native_new".into(), 0x80000000);
    assert!(linker::link(&object, 0x510000, &externals).err().unwrap().to_string().contains("veneer"));
}

#[test]
fn layout_numbers_do_not_acquire_the_pie_load_bias() {
    let Some(object) = object() else { return };
    let all = sections(&object);
    let sym = all.iter().find(|s| s.1 == 2).unwrap();
    let strings = &all[w(&object, sym.0 + 24) as usize];
    let mut checked = std::collections::BTreeSet::new();
    for reloc in all.iter().filter(|s| s.1 == 9) {
        for at in (reloc.4..reloc.4 + reloc.5).step_by(8) {
            let info = w(&object, at + 4);
            let entry = sym.4 + (info >> 8) as usize * 16;
            let start = strings.4 + w(&object, entry) as usize;
            let len = object[start..].iter().position(|b| *b == 0).unwrap();
            let name = std::str::from_utf8(&object[start..start + len]).unwrap();
            if name.starts_with("layout_") {
                assert_eq!(info & 255, 2, "{name} needs an ABS32 numeric literal");
                checked.insert(name.to_owned());
            }
        }
    }
    // Some discovered fields are intentionally unused by the adapter.
    // Check representative live sizes, offsets and flags as well as every
    // layout relocation above, rather than requiring unused symbols.
    for name in ["PRIMARY_SIZE", "COMMON_OFFSET", "SCREEN_SECOND_VPTR", "LEGACY_CLICK"] {
        assert!(checked.contains(&format!("layout_{name}")), "missing layout {name}");
    }
}

/// Assign distinct, reachable addresses to undefined symbols in the test object.
fn synthetic_bindings(object: &[u8]) -> BTreeMap<String, u32> {
    let all = sections(object);
    let sym = all.iter().find(|s| s.1 == 2).unwrap();
    let strings = &all[w(object, sym.0 + 24) as usize];
    let mut symbols = BTreeMap::new();
    for at in (sym.4..sym.4 + sym.5).step_by(16) {
        if w(object, at) == 0 || object[at + 14..at + 16] != [0, 0] {
            continue;
        }
        let start = strings.4 + w(object, at) as usize;
        let len = object[start..].iter().position(|&b| b == 0).unwrap();
        let name = std::str::from_utf8(&object[start..start + len]).unwrap();
        symbols.insert(name.to_owned(), 0x500000 + 4 * symbols.len() as u32);
    }
    symbols
}

/// Ordinary instruction changes can preserve the screen's structural ABI.
#[test]
fn compatible_instruction_changes_preserve_the_linked_payload() {
    let (Some(stock), Some(object)) = (stock_sample(), object()) else { return };
    let before = nlclient::builder::build(&stock, &object, &[]).unwrap();
    let ctor = before.manifest["native_api"]["native_layer_ctor"].as_u64().unwrap() as u32;
    let mut changed = stock.clone();
    let at = va(&changed, ctor);
    changed[at] ^= 1;
    let after = nlclient::builder::build(&changed, &object, &[]).unwrap();
    assert_eq!(after.manifest["native_api"], before.manifest["native_api"]);
    assert_eq!(after.manifest["discovery"]["layout"], before.manifest["discovery"]["layout"]);
    assert_ne!(
        after.manifest["discovery"]["fingerprints"]["native_layer_ctor"],
        before.manifest["discovery"]["fingerprints"]["native_layer_ctor"]
    );
    let payload = before.manifest["payload_offset"].as_u64().unwrap() as usize;
    assert_eq!(after.output[payload..], before.output[payload..]);
}

/// Metadata can change without moving the text-only Legal Info layer.
#[test]
fn text_only_legal_info_resolves_without_a_stock_hash() {
    let firmware =
        std::env::var_os("NLCLIENT_FIRMWARE_DIR").map(PathBuf::from).unwrap_or_else(|| root().join(".work/firmware"));
    let Ok(mut stock) = std::fs::read(firmware.join("nlclient-gen2-5.9.1-6")) else {
        assert!(std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_none());
        return;
    };
    let before = nlclient::discovery::discover(&stock).unwrap();
    stock.extend_from_slice(b"additional build metadata");
    let changed = nlclient::discovery::discover(&stock).unwrap();
    assert_ne!(before.stock_sha256, changed.stock_sha256);
    assert_eq!(before.native_api, changed.native_api);
    assert_eq!(before.layout, changed.layout);
}

/// Reject a getter reading the wrong field and a constructor missing required calls.
#[test]
fn natives_of_another_shape_are_refused() {
    let Some(stock) = stock_sample() else { return };
    let before = nlclient::discovery::discover(&stock).unwrap();
    let mut changed = stock.clone();
    let at = va(&changed, before.native_api["native_image_width"]);
    assert_eq!(w(&changed, at), 0xe5900004, "ldr r0, [r0, #4]");
    put(&mut changed, at, 0xe590000c);
    let error = nlclient::discovery::discover(&changed).unwrap_err().to_string();
    assert!(error.contains("native_image_width does not have the native's shape"), "{error}");
    // Every call in the screen constructor made to its first callee: one
    // library function where the reviewed builds call four.
    let mut changed = stock.clone();
    let start = va(&changed, before.native_api["native_screen_ctor"]);
    let mut first = None;
    for at in (start..start + 0x200).step_by(4) {
        let word = w(&changed, at);
        if word == 0xe12fff1e || word & 0xffff8000 == 0xe8bd8000 {
            break; // bx lr, or a pop into pc: the constructor's return
        }
        if word >> 24 != 0xeb {
            continue;
        }
        let target = at as i64 + 8 + (((word << 8) as i32) >> 6) as i64;
        match first {
            None => first = Some(target),
            Some(target_first) => {
                let delta = target_first - at as i64 - 8;
                put(&mut changed, at, 0xeb000000 | ((delta >> 2) as u32 & 0xffffff));
            }
        }
    }
    let error = nlclient::discovery::discover(&changed).unwrap_err().to_string();
    assert!(error.contains("native_screen_ctor requires"), "{error}");
}
#[test]
fn without_features_the_settings_menu_and_its_loop_are_four_entries_shorter() {
    let Some(object) = object() else { return };
    for path in samples() {
        let stock = std::fs::read(&path).unwrap();
        if nlclient::legacy_abi(&stock) {
            continue;
        }
        let all = nlclient::builder::build(&stock, &object, &nlclient::menu::Section::ALL).unwrap();
        let none = nlclient::builder::build(&stock, &object, &[]).unwrap();
        let (all, none) = (&all.manifest["settings_menu"], (&none.manifest["settings_menu"], &none.output));
        let names = |menu: &Value| {
            menu["names"].as_array().unwrap().iter().map(|n| n.as_str().unwrap().to_owned()).collect::<Vec<_>>()
        };
        let shown = names(none.0);
        assert_eq!(shown.len() + 4, names(all).len(), "{}", path.display());
        assert!(!shown.iter().any(|n| ["account", "update", "proinfoscreen", "templock"].contains(&n.as_str())));
        assert_eq!(shown[shown.len() - 3..], ["legalinfo", "reset", "temperature"]);
        let bound = none.0["bound"].as_u64().unwrap() as u32;
        let at = file_offset(&stock, bound);
        let add = u32::from_le_bytes(none.1[at..at + 4].try_into().unwrap());
        assert_eq!(add & 0xfff, shown.len() as u32 * 4, "{}", path.display());
    }
}

#[test]
fn schedule_insertion_preserves_every_stock_slot_and_rejects_unknown_menu_abis() {
    let Some(object) = object() else { return };
    for path in samples() {
        let stock = std::fs::read(&path).unwrap();
        if nlclient::legacy_abi(&stock) {
            continue;
        }
        let built = nlclient::builder::build(&stock, &object, &nlclient::menu::Section::ALL).unwrap();
        let menu = &built.manifest["settings_menu"];
        let names = menu["names"].as_array().unwrap();
        let table = file_offset(&stock, menu["address"].as_u64().unwrap() as u32);
        let mut original = Vec::new();
        for i in 0..names.len() {
            let at = file_offset(&stock, w(&stock, table + i * 4));
            let end = stock[at..].iter().position(|b| *b == 0).unwrap() + at;
            original.push(std::str::from_utf8(&stock[at..end]).unwrap().to_owned());
        }
        let mut arranged: Vec<_> = names.iter().map(|n| n.as_str().unwrap().to_owned()).collect();
        original.sort();
        arranged.sort();
        assert_eq!(arranged, original, "{}: original slots retained", path.display());
        assert_eq!(names.first().unwrap(), "temperature");
        assert_eq!(names.last().unwrap(), "temperature");
        assert_eq!(names.iter().filter(|n| *n == "legalinfo").count(), 1);
        assert_eq!(names.iter().filter(|n| *n == "appsetup").count(), 0);
        assert!(built.manifest["factories"].get("appsetup").is_some());
        let call = menu["insertion"]["call"].as_u64().unwrap() as u32;
        let hook = built.manifest["symbols"]["custom_menu_lookup"].as_u64().unwrap() as u32;
        let at = file_offset(&stock, call);
        let instruction = w(&built.output, at);
        let relative = ((instruction << 8) as i32) >> 6;
        assert_eq!(call.wrapping_add(8).wrapping_add(relative as u32), hook);
        assert_eq!(instruction >> 24, 0xeb);
        let mut changed = stock.clone();
        put(&mut changed, at + 4, 0xe1a00000);
        assert!(nlclient::builder::build(&changed, &object, &[]).is_err(), "{}: corrupt insertion", path.display());
        let grow = menu["insertion"]["grow"].as_u64().unwrap() as u32;
        if grow != 0 {
            let branch = w(&stock, at + 28);
            let relative = ((branch << 8) as i32) >> 6;
            let setup = call.wrapping_add(36).wrapping_add(relative as u32);
            let mut changed = stock.clone();
            put(&mut changed, file_offset(&stock, setup) + 8, 0xffffffff);
            assert!(
                nlclient::builder::build(&changed, &object, &[]).is_err(),
                "{}: undecodable growth",
                path.display()
            );
            // A store of the new buffer's end and capacity, made to miss: the
            // gen E's append stores its end on its own.
            let stores: &[(u32, u32)] = if menu["insertion"]["appends"] == true {
                &[(0xe9860030, 0xe9860060), (0xe5864004, 0xe586400c)]
            } else {
                &[(0xe5858004, 0xe585800c)]
            };
            let start = file_offset(&stock, grow);
            let (store, corrupt) = (start..start + 512)
                .step_by(4)
                .find_map(|at| stores.iter().find(|(word, _)| w(&stock, at) == *word).map(|(_, bad)| (at, *bad)))
                .unwrap();
            let mut changed = stock.clone();
            put(&mut changed, store, corrupt);
            assert!(nlclient::builder::build(&changed, &object, &[]).is_err(), "{}: corrupt growth", path.display());
        }
    }
}

/// The file offset of a virtual address, from the ELF's load segments.
fn file_offset(elf: &[u8], address: u32) -> usize {
    let word = |at: usize| u32::from_le_bytes(elf[at..at + 4].try_into().unwrap());
    let half = |at: usize| u16::from_le_bytes(elf[at..at + 2].try_into().unwrap()) as usize;
    let (phoff, count) = (word(28) as usize, half(44));
    (0..count)
        .map(|i| phoff + i * 32)
        .find(|&h| word(h) == 1 && (word(h + 8)..word(h + 8) + word(h + 16)).contains(&address))
        .map(|h| (address - word(h + 8) + word(h + 4)) as usize)
        .unwrap()
}
