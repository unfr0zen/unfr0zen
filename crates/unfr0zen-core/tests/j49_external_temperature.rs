//! Private firmware checks for the opt-in gen 2 RCS input patch.
use std::path::{Path, PathBuf};
use unfr0zen_core::nlclient::{self, external_temperature, Patch};

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn offset(bytes: &[u8], address: u32) -> usize {
    let phoff = word(bytes, 28) as usize;
    let count = u16::from_le_bytes(bytes[44..46].try_into().unwrap());
    for i in 0..count as usize {
        let h = phoff + i * 32;
        let base = word(bytes, h + 8);
        if word(bytes, h) == 1 && address >= base && address - base < word(bytes, h + 16) {
            return (word(bytes, h + 4) + address - base) as usize;
        }
    }
    panic!("unmapped address {address:#x}");
}

fn target(instruction: u32, address: u32) -> u32 {
    let relative = ((instruction << 8) as i32) >> 6;
    address.wrapping_add(8).wrapping_add(relative as u32)
}

#[test]
fn rcs_object_matches_the_independent_reference_without_firmware() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = std::env::var_os("NLCLIENT_RCS_OBJECT").map(PathBuf::from)
        .unwrap_or_else(|| root.join("build/j49-rcs-screen.o"));
    let Ok(object) = std::fs::read(path) else {
        assert!(std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(), "strict checks need the j49 RCS object");
        return;
    };
    let golden: serde_json::Value = serde_json::from_str(include_str!(
        "../../../platform/common/nlclient/tests/external-temperature/j49-5.9.4-5.json")).unwrap();
    let linked = nlclient::linker::link(&object, golden["ui_address"].as_u64().unwrap() as u32,
        &serde_json::from_value(golden["ui_bindings"].clone()).unwrap()).unwrap();
    assert_eq!(linked.bytes.len(), golden["ui_size"].as_u64().unwrap() as usize);
    assert_eq!(unfr0zen_core::shell::sha256_hex(&linked.bytes), golden["ui_sha256"]);
    for (name, address) in golden["ui_symbols"].as_object().unwrap() {
        assert_eq!(linked.symbols[name], address.as_u64().unwrap() as u32);
    }
}

#[test]
fn reviewed_j49_input_is_sensor_only_mapped_and_reversible() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let firmware = std::env::var_os("NLCLIENT_FIRMWARE_DIR").map(PathBuf::from)
        .unwrap_or_else(|| root.join(".work/firmware"));
    let object = std::env::var_os("NLCLIENT_OBJECT").map(PathBuf::from)
        .unwrap_or_else(|| root.join("build/custom-screen.o"));
    let (Ok(stock), Ok(object)) = (std::fs::read(firmware.join("nlclient-gen2-2019")), std::fs::read(object)) else {
        assert!(std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(), "strict checks need the j49 sample and screen object");
        eprintln!("No private j49 sample or screen object; skipping RCS input check");
        return;
    };
    let rcs_object = std::fs::read(std::env::var_os("NLCLIENT_RCS_OBJECT").map(PathBuf::from)
        .unwrap_or_else(|| root.join("build/j49-rcs-screen.o"))).expect("build the j49 RCS screen object");
    let golden: serde_json::Value = serde_json::from_str(include_str!(
        "../../../platform/common/nlclient/tests/external-temperature/j49-5.9.4-5.json")).unwrap();
    let screen = nlclient::builder::build(&stock, &object).unwrap().output;
    assert_eq!(unfr0zen_core::shell::sha256_hex(&stock), golden["stock_sha256"]);
    assert_eq!(unfr0zen_core::shell::sha256_hex(&screen), golden["screen_sha256"]);
    assert_eq!(screen.len(), golden["screen_size"].as_u64().unwrap() as usize);
    let mut output = screen.clone();
    external_temperature::patch(&stock, &mut output, &rcs_object).unwrap();
    assert_eq!(word(&output, offset(&stock, 0x141bc4)), 0xe3560001);
    let table = offset(&stock, 0x4ef920);
    assert_eq!(word(&output, table + 4), 0x14173c);
    let command = offset(&output, word(&output, table));
    assert_eq!(&output[command..command + b"kryptonitedata\0".len()], b"kryptonitedata\0");
    let hook = word(&output, offset(&stock, 0x47e88));
    assert_eq!(hook >> 24, 0xea);
    let entry = target(hook, 0x47e88);
    let code = offset(&output, entry);
    assert!(code >= screen.len());
    assert_eq!(word(&output, code), 0xe92d500f);
    assert_eq!(target(word(&output, code + 6 * 4), entry + 6 * 4), 0x2ad84);
    assert_eq!(word(&output, code + 10 * 4), 0xe5941070); // ctor takes globals
    assert_eq!(target(word(&output, code + 11 * 4), entry + 11 * 4), 0x141b2c);
    assert_eq!(target(word(&output, code + 23 * 4), entry + 23 * 4), 0x2bf24);
    assert_eq!(target(word(&output, code + 28 * 4), entry + 28 * 4), 0x47e8c);
    // Preserve socket address construction (127.0.0.1) and native cleanup.
    for (start, end) in [(0x141c88, 0x141cd8), (0x4725c, 0x47294), (0x47384, 0x473a8)] {
        let range = offset(&stock, start)..offset(&stock, end);
        assert_eq!(output[range.clone()], stock[range]);
    }
    assert_eq!(word(&output, offset(&stock, 0x141c74)), 0xe1a00000);
    assert_eq!(target(word(&output, offset(&stock, 0x141d40)), 0x141d40), entry + 29 * 4);
    for (site, symbol) in [(0x14282c, "custom_factory"), (0x141858, "rcs_post_reading")] {
        assert_eq!(target(word(&output, offset(&stock, site)), site), golden["ui_symbols"][symbol].as_u64().unwrap() as u32);
    }
    for (site, field) in [(0x19a668, "guard_entry"), (0x14185c, "reply_entry"),
                          (0x237af4, "temperature_entry"), (0x237afc, "result_entry")] {
        assert_eq!(target(word(&output, offset(&stock, site)), site), golden[field].as_u64().unwrap() as u32);
    }
    assert_eq!(word(&output, offset(&stock, 0x47e94)), 0xe3a00000);
    assert_eq!(word(&output, offset(&stock, 0x11474c)), 0xeaffffbe);
    assert_eq!(target(0xeaffffbe, 0x11474c), 0x11464c);
    // Settings lookup/context and native screen titles retain their stock code.
    for site in [0x11463c, 0x115430, 0x19a838, 0x19a99c] {
        assert_eq!(word(&output, offset(&stock, site)), word(&stock, offset(&stock, site)));
    }
    let mut missing_payload = screen.clone();
    assert!(external_temperature::patch(&stock, &mut missing_payload, b"").is_err());
    assert_eq!(missing_payload, screen);
    let journal = Patch::journal(&stock, &output).unwrap();
    assert_eq!(output.len(), golden["output_size"].as_u64().unwrap() as usize);
    assert_eq!(unfr0zen_core::shell::sha256_hex(&output), golden["output_sha256"]);
    let mut raw_journal = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(journal.as_slice()), &mut raw_journal).unwrap();
    assert_eq!(unfr0zen_core::shell::sha256_hex(&raw_journal), golden["journal_sha256"]);
    for site in golden["sites"].as_array().unwrap() {
        let at = site["offset"].as_u64().unwrap() as usize;
        let hex = |b: &[u8]| b.iter().map(|v| format!("{v:02x}")).collect::<String>();
        assert_eq!(hex(&screen[at..at + 4]), site["before"]);
        assert_eq!(hex(&output[at..at + 4]), site["after"]);
    }
    let patch = Patch::parse(&journal).unwrap();
    assert_eq!(patch.apply(&stock, false).unwrap(), output);
    assert_eq!(patch.apply(&output, true).unwrap(), stock);
    // Deselecting the option rebuilds the ordinary screen, undoing this input.
    let off = Patch::parse(&Patch::journal(&stock, &screen).unwrap()).unwrap();
    let downgrade = Patch::upgrade(&patch, off).unwrap();
    assert_eq!(downgrade.apply(&output, false).unwrap(), screen);
    assert!(external_temperature::patch(&stock, &mut output, &rcs_object).is_err());
    for address in [0x47e88, 0x47e94, 0x141bc4, 0x141c74, 0x141d40, 0x141c7c, 0x4ef920,
                    0x14282c, 0x11474c, 0x19a668, 0x141858, 0x14185c, 0x237af4, 0x237afc] {
        let mut changed = screen.clone();
        changed[offset(&stock, address)] ^= 1;
        let before = changed.clone();
        assert!(external_temperature::patch(&stock, &mut changed, &rcs_object).is_err());
        assert_eq!(changed, before);
    }
    assert!(external_temperature::patch(&stock, &mut stock.clone(), &rcs_object).is_err());
    let mut changed_stock = stock.clone();
    changed_stock[0x1000] ^= 1;
    assert!(external_temperature::patch(&changed_stock, &mut screen.clone(), &rcs_object).is_err());
}
