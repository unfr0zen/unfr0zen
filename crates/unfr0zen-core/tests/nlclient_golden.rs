//! Goldens contain no proprietary firmware. Local unchanged dumps opt in to
//! discovery/output checks; linking the same embedded object needs no dump.
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};
use unfr0zen_core::{
    nlclient::{self, linker},
    shell::sha256_hex,
};
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
fn goldens() -> Vec<Value> {
    let mut paths = std::fs::read_dir(root().join("platform/common/nlclient/tests/golden"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .iter()
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .collect()
}
fn object() -> Option<Vec<u8>> {
    let p = std::env::var_os("NLCLIENT_OBJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("build/custom-screen.o"));
    let object = std::fs::read(p).ok();
    if object.is_none() {
        assert!(
            std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(),
            "strict golden checks require the compiled screen object"
        );
        eprintln!("No screen object: build firmware or set NLCLIENT_OBJECT");
    }
    object
}
#[test]
fn embedded_object_links_to_golden_payloads() {
    let Some(object) = object() else { return };
    for golden in goldens() {
        let api: BTreeMap<String, u32> =
            serde_json::from_value(golden["native_api"].clone()).unwrap();
        let mut externals = api;
        externals.entry("native_rename".into()).or_insert(0);
        let layout: BTreeMap<String, u32> =
            serde_json::from_value(golden["discovery"]["layout"].clone()).unwrap();
        externals.extend(layout.into_iter().map(|(k, v)| (format!("layout_{k}"), v)));
        let linked = linker::link(
            &object,
            golden["patch"]["payload_address"].as_u64().unwrap() as u32,
            &externals,
        )
        .unwrap();
        assert_eq!(
            json!({"size":linked.bytes.len(),"entry":linked.entry,"symbols":linked.symbols,"sha256":sha256_hex(&linked.bytes)}),
            golden["payload"]
        );
    }
}
#[test]
fn stock_dumps_match_every_recorded_stage() {
    let firmware = std::env::var_os("NLCLIENT_FIRMWARE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join(".work/firmware"));
    let Some(object) = object() else { return };
    let Ok(paths) = std::fs::read_dir(&firmware) else {
        assert!(
            std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(),
            "strict golden checks require all private stock samples"
        );
        eprintln!("No private firmware directory: {}", firmware.display());
        return;
    };
    let mut checked = 0;
    for path in paths {
        let path = path.unwrap().path();
        if !path.is_file() {
            continue;
        }
        let stock = std::fs::read(&path).unwrap();
        let hash = sha256_hex(&stock);
        let golden_path = root().join(format!("platform/common/nlclient/tests/golden/{hash}.json"));
        if !golden_path.exists() {
            continue;
        }
        let golden: Value = serde_json::from_slice(&std::fs::read(golden_path).unwrap()).unwrap();
        let found = nlclient::builder::build(&stock, &object)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        checked += 1;
        for stage in ["discovery", "native_api"] {
            assert_eq!(
                found.manifest[stage],
                golden[stage],
                "{}: {stage}",
                path.display()
            );
        }
        for (key, want) in golden["patch"].as_object().unwrap() {
            assert_eq!(
                &found.manifest[key],
                want,
                "{}: patch.{key}",
                path.display()
            );
        }
        let offset = found.manifest["payload_offset"].as_u64().unwrap() as usize;
        let size = found.manifest["payload_size"].as_u64().unwrap() as usize;
        assert_eq!(
            json!({"size":size,"entry":found.manifest["entry"],"symbols":found.manifest["symbols"],"sha256":sha256_hex(&found.output[offset..offset+size])}),
            golden["payload"]
        );
        let mut journal = Vec::new();
        flate2::read::GzDecoder::new(found.journal.as_slice())
            .read_to_end(&mut journal)
            .unwrap();
        assert_eq!(
            json!({"size":found.output.len(),"sha256":sha256_hex(&found.output),"journal_sha256":sha256_hex(&journal)}),
            golden["output"]
        );
        let patch = nlclient::Patch::parse(&found.journal).unwrap();
        assert_eq!(patch.apply(&found.output, true).unwrap(), stock);
    }
    eprintln!("Checked {checked} unchanged firmware samples");
    if std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_some() {
        assert_eq!(
            checked,
            goldens().len(),
            "all private stock samples are required"
        );
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
    let Some(object) = object() else { return };
    let lld = std::env::var_os("NLCLIENT_REFERENCE_LLD").unwrap_or_else(|| "ld.lld".into());
    if std::process::Command::new(&lld)
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("Optional reference check: ld.lld not present");
        return;
    }
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).unwrap();
    let directory = std::env::temp_dir().join(format!("unfr0zen-link-{}", sha256_hex(&nonce)));
    std::fs::create_dir(&directory).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(directory.clone());
    let object_path = directory.join("screen.o");
    std::fs::write(&object_path, &object).unwrap();
    for golden in goldens() {
        let mut externals: BTreeMap<String, u32> =
            serde_json::from_value(golden["native_api"].clone()).unwrap();
        externals.entry("native_rename".into()).or_insert(0);
        let layout: BTreeMap<String, u32> =
            serde_json::from_value(golden["discovery"]["layout"].clone()).unwrap();
        externals.extend(layout.into_iter().map(|(k, v)| (format!("layout_{k}"), v)));
        let address = golden["patch"]["payload_address"].as_u64().unwrap() as u32;
        let linked = linker::link(&object, address, &externals).unwrap();
        let script=format!("{}\nSECTIONS {{ . = {address:#x}; .text : {{ *(.text*) }} .rodata : {{ *(.rodata*) }} .data : {{ *(.data*) *(.got*) }} .bss : {{ *(.bss*) *(COMMON) }} /DISCARD/ : {{ *(.comment) *(.ARM.exidx*) *(.ARM.extab*) }} }}",externals.iter().map(|(k,v)|format!("{k} = {v:#x};")).collect::<Vec<_>>().join("\n"));
        let script_path = directory.join("payload.ld");
        std::fs::write(&script_path, script).unwrap();
        let output = directory.join("screen.elf");
        let status = std::process::Command::new(&lld)
            .args(["-O0", "-T"])
            .arg(&script_path)
            .args(["-e", "custom_factory"])
            .arg(&object_path)
            .arg("-o")
            .arg(&output)
            .status()
            .unwrap();
        assert!(status.success());
        let elf = std::fs::read(output).unwrap();
        let word = |at| u32::from_le_bytes(elf[at..at + 4].try_into().unwrap()) as usize;
        let shoff = word(32);
        let count = u16::from_le_bytes(elf[48..50].try_into().unwrap()) as usize;
        let names = u16::from_le_bytes(elf[50..52].try_into().unwrap()) as usize;
        let names = word(shoff + names * 40 + 16);
        let mut reference = Vec::new();
        for i in 0..count {
            let at = shoff + i * 40;
            let name = names + word(at);
            let end = elf[name..].iter().position(|&b| b == 0).unwrap() + name;
            if &elf[name..end] != b".text" && &elf[name..end] != b".rodata" {
                continue;
            }
            let start = word(at + 12) - address as usize;
            reference.resize(start, 0);
            let offset = word(at + 16);
            reference.extend(&elf[offset..offset + word(at + 20)]);
        }
        assert_eq!(
            linked.bytes, reference,
            "same object and addresses must produce exactly ld.lld -O0's bytes"
        );
    }
}

fn stock_sample() -> Option<Vec<u8>> {
    let p = std::env::var_os("NLCLIENT_STOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join(".work/firmware/nlclient-gen3-2025"));
    std::fs::read(p).ok()
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
    for address in [
        before.native_api["native_mark_activity"],
        before.native_api["native_device_commit"],
        before.native_api["native_shared_commit"],
        before.native_api["native_schedule_commit"],
        before.native_api["native_layer_ctor"],
        before.native_api["native_menu_hidden"],
        before.native_api["native_menu_scale"],
    ] {
        let mut changed = stock.clone();
        let at = va(&changed, address);
        changed[at] ^= 1;
        assert!(
            !nlclient::discovery::discover(&changed).is_ok_and(|found| found.variant.is_some()),
            "changed native instruction named a reviewed build"
        );
    }
    for bucket in ["device", "shared", "schedule"] {
        let slot = before.anchors[&format!("{bucket}_commit_slot")];
        let relocation = sections(&stock).into_iter()
            .filter(|(_, kind, _, _, _, _)| *kind == 9)
            .flat_map(|(_, _, _, _, offset, size)| (offset..offset + size).step_by(8))
            .find(|&at| w(&stock, at) == slot).unwrap();
        let mut changed = stock.clone();
        put(&mut changed, relocation + 4, 2); // R_ARM_ABS32 does not preserve PIE load bias.
        assert!(nlclient::discovery::discover(&changed).is_err());
    }
    let mut changed = stock.clone();
    let at = va(
        &changed,
        before.native_api["native_screen_vtable"] + before.layout["SCREEN_SECOND_VPTR"] - 16,
    );
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
    assert_eq!(
        after.native_api,
        before
            .native_api
            .into_iter()
            .map(|(k, v)| (k, v + shift))
            .collect()
    );
}

#[test]
fn unsupported_relocations_and_branch_veneers_are_refused() {
    let Some(object) = object() else { return };
    let golden = goldens().remove(0);
    let mut externals: BTreeMap<String, u32> =
        serde_json::from_value(golden["native_api"].clone()).unwrap();
    externals.entry("native_rename".into()).or_insert(0);
    let layout: BTreeMap<String, u32> =
        serde_json::from_value(golden["discovery"]["layout"].clone()).unwrap();
    externals.extend(layout.into_iter().map(|(k, v)| (format!("layout_{k}"), v)));
    let mut changed = object.clone();
    let reloc = sections(&object)
        .into_iter()
        .find(|(_, kind, _, _, _, _)| *kind == 9)
        .unwrap();
    let at = reloc.4 + 4;
    let info = w(&changed, at);
    put(&mut changed, at, (info & !255) | 255);
    assert!(linker::link(&changed, 0x510000, &externals)
        .err()
        .unwrap()
        .to_string()
        .contains("relocation 255"));
    externals.insert("native_new".into(), 0x80000000);
    assert!(linker::link(&object, 0x510000, &externals)
        .err()
        .unwrap()
        .to_string()
        .contains("veneer"));
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
    for name in [
        "PRIMARY_SIZE",
        "COMMON_OFFSET",
        "SCREEN_SECOND_VPTR",
        "LEGACY_CLICK",
    ] {
        assert!(
            checked.contains(&format!("layout_{name}")),
            "missing layout {name}"
        );
    }
}

/// Every private sample, by file name, with its golden.
fn samples() -> Vec<(PathBuf, Vec<u8>, Value)> {
    let firmware = std::env::var_os("NLCLIENT_FIRMWARE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join(".work/firmware"));
    let Ok(paths) = std::fs::read_dir(&firmware) else { return Vec::new() };
    let mut found = Vec::new();
    for path in paths.flatten().map(|p| p.path()).filter(|p| p.is_file()) {
        let stock = std::fs::read(&path).unwrap();
        let golden = root().join(format!("platform/common/nlclient/tests/golden/{}.json", sha256_hex(&stock)));
        if let Ok(golden) = std::fs::read(golden) {
            found.push((path, stock, serde_json::from_slice(&golden).unwrap()));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// The imported symbols each native calls are the evidence an untested
/// build is admitted on, so they must be what survives a recompile: had any
/// reviewed build been unknown, the others' evidence would have admitted
/// all but at most one of its natives (the 2025 build's activity mark, the
/// gen 2's device commit), but five of the 6.5-14 build's: its factories
/// and event handlers also call the string destructor, delete and Release.
#[test]
fn reviewed_builds_share_their_natives_library_calls() {
    let samples = samples();
    if samples.is_empty() {
        assert!(std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(), "strict checks require the private samples");
        return;
    }
    let shapes: Value =
        serde_json::from_slice(&std::fs::read(root().join("crates/unfr0zen-core/src/nlclient/abi_shapes.json")).unwrap()).unwrap();
    for (path, stock, _) in &samples {
        let found = nlclient::discovery::discover(stock).unwrap();
        let own = found.variant.clone().unwrap();
        assert_eq!(found.matches[&own], found.evidence.len(), "{}", path.display());
        let others: Vec<_> = shapes["variants"].as_array().unwrap().iter().filter(|v| v["label"] != own).collect();
        let unmatched: Vec<_> = found
            .evidence
            .iter()
            .filter(|(name, calls)| !others.iter().any(|v| v["imports"][name.as_str()] == json!(calls)))
            .map(|(name, _)| name.as_str())
            .collect();
        let allowed = if own == "2025-6.5-14" { 5 } else { 1 };
        assert!(unmatched.len() <= allowed, "{}: no other reviewed build's natives call what these do: {unmatched:?}", path.display());
    }
}

/// A build with one instruction changed has no reviewed build's shapes:
/// discovery patches it on its structure and library calls all the same,
/// names the reviewed build it is like, and the manifest says so.
#[test]
fn a_recompiled_build_is_patched_and_named_as_no_reviewed_one() {
    let Some(stock) = stock_sample() else { return };
    let before = nlclient::discovery::discover(&stock).unwrap();
    assert_eq!(before.variant.as_deref(), Some("2025"));
    let mut changed = stock.clone();
    let at = va(&changed, before.native_api["native_layer_ctor"]);
    changed[at] ^= 1;
    let found = nlclient::discovery::discover(&changed).unwrap();
    assert_eq!(found.variant, None);
    assert_eq!(found.native_api, before.native_api);
    assert_eq!(found.layout, before.layout);
    assert_eq!(found.matches["2025"], found.evidence.len());
    assert!(found.validation.starts_with("unreviewed build: structurally consistent"), "{}", found.validation);
    let object = object().unwrap();
    assert_eq!(nlclient::builder::build(&changed, &object).unwrap().manifest["reviewed"], false);
    assert_eq!(nlclient::builder::build(&stock, &object).unwrap().manifest["reviewed"], true);
}

/// Discovery holds every native to its shape whatever its fingerprints: a
/// getter that reads another field is refused, and so is a native calling
/// other library functions than every reviewed build's, which the
/// fingerprints alone, masking call targets, would not tell apart.
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
    assert!(error.contains("no reviewed build's native makes these library calls: native_screen_ctor calls"), "{error}");
}
