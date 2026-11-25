//! Hash-guarded 5.6-7 ABI checks; proprietary firmware stays in private fixtures.
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::Read, path::PathBuf};
use unfr0zen_core::{
    nlclient::{self, builder, linker, menu::Section},
    shell::sha256_hex,
};

fn root() -> PathBuf {
    PathBuf::from(env!("UNFR0ZEN_ROOT"))
}
fn golden() -> Value {
    serde_json::from_slice(
        &std::fs::read(root().join(format!("platform/common/nlclient/tests/legacy/{}.json", nlclient::LEGACY_SHA256)))
            .unwrap(),
    )
    .unwrap()
}
fn object() -> Option<Vec<u8>> {
    let path = std::env::var_os("NLCLIENT_LEGACY_OBJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("build/custom-screen-legacy.o"));
    let result = std::fs::read(path).ok();
    assert!(result.is_some() || std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(), "legacy object required");
    result
}
fn stock() -> Option<Vec<u8>> {
    let path = std::env::var_os("NLCLIENT_LEGACY_STOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join(".work/firmware/nlclient-gen3-5.6-7"));
    let result = std::fs::read(path).ok();
    assert!(result.is_some() || std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(), "legacy firmware required");
    result
}

#[test]
fn legacy_object_links_to_the_reviewed_payload_without_firmware() {
    let Some(object) = object() else { return };
    let expected = golden();
    let discovery = serde_json::from_value(expected["discovery"].clone()).unwrap();
    let mut externals: BTreeMap<String, u32> = serde_json::from_value(expected["native_api"].clone()).unwrap();
    let layout: BTreeMap<String, u32> = serde_json::from_value(expected["discovery"]["layout"].clone()).unwrap();
    externals.extend(layout.into_iter().map(|(key, value)| (format!("layout_{key}"), value)));
    externals.extend(nlclient::discovery::handover_externals(&discovery));
    let menu: BTreeMap<String, u32> =
        serde_json::from_value(expected["patch"]["settings_menu"]["bindings"].clone()).unwrap();
    externals.extend(menu);
    externals.entry("native_rename".into()).or_insert(0);
    let linked =
        linker::link(&object, expected["patch"]["payload_address"].as_u64().unwrap() as u32, &externals).unwrap();
    assert_eq!(
        json!({"size":linked.bytes.len(), "entry":linked.entry, "symbols":linked.symbols, "sha256":sha256_hex(&linked.bytes)}),
        expected["payload"]
    );
}

#[test]
fn stock_legacy_firmware_matches_every_reviewed_stage_and_undoes_exactly() {
    let (Some(stock), Some(object)) = (stock(), object()) else { return };
    assert!(nlclient::legacy_abi(&stock));
    let expected = golden();
    let built = builder::build(&stock, &object, &Section::LEGACY).unwrap();
    for stage in ["discovery", "native_api"] {
        assert_eq!(built.manifest[stage], expected[stage], "{stage}");
    }
    for (key, value) in expected["patch"].as_object().unwrap() {
        assert_eq!(&built.manifest[key], value, "{key}");
    }
    let offset = built.manifest["payload_offset"].as_u64().unwrap() as usize;
    let size = built.manifest["payload_size"].as_u64().unwrap() as usize;
    assert_eq!(
        json!({"size":size,"entry":built.manifest["entry"],"symbols":built.manifest["symbols"],"sha256":sha256_hex(&built.output[offset..offset+size])}),
        expected["payload"]
    );
    let mut journal = Vec::new();
    flate2::read::GzDecoder::new(built.journal.as_slice()).read_to_end(&mut journal).unwrap();
    assert_eq!(
        json!({"size":built.output.len(),"sha256":sha256_hex(&built.output),"journal_sha256":sha256_hex(&journal)}),
        expected["output"]
    );
    assert_eq!(nlclient::Patch::parse(&built.journal).unwrap().apply(&built.output, true).unwrap(), stock);
}

#[test]
fn changed_legacy_executables_and_the_modern_object_are_rejected() {
    let (Some(stock), Some(object)) = (stock(), object()) else { return };
    for offset in [0, 0xc75b4, stock.len() - 1] {
        let mut changed = stock.clone();
        changed[offset] ^= 1;
        assert!(!nlclient::legacy_abi(&changed));
        assert!(builder::build(&changed, &object, &[]).is_err(), "changed byte {offset:#x}");
    }
    if let Ok(modern) = std::fs::read(root().join("build/custom-screen.o")) {
        assert!(builder::build(&stock, &modern, &[]).is_err());
    }
}

#[test]
fn legacy_tiles_keep_system_info_and_respect_the_name_array_capacity() {
    let (Some(stock), Some(object)) = (stock(), object()) else { return };
    for sections in [
        vec![],
        vec![Section::Zigbee, Section::Matter],
        vec![Section::Zigbee, Section::Doom],
        vec![Section::Matter, Section::Doom],
    ] {
        let built = builder::build(&stock, &object, &sections).unwrap();
        let names = built.manifest["settings_menu"]["names"].as_array().unwrap();
        assert!(names.len() < 27, "room for Schedule");
        assert_eq!(names.first().unwrap(), "temperature");
        assert_eq!(names.last().unwrap(), "temperature");
        assert!(names.iter().any(|name| name == "systeminfo"));
        assert!(!names.iter().any(|name| name == "update"));
        assert!(built.manifest["factories"].get("update").is_some());
        assert!(built.manifest["factories"].get("systeminfo").is_none());
    }
    let error =
        builder::build(&stock, &object, &[Section::Zigbee, Section::Matter, Section::Doom]).err().unwrap().to_string();
    assert!(error.contains("name capacity"), "{error}");
    assert!(builder::build(&stock, &object, &[Section::Themes]).is_err());
}
