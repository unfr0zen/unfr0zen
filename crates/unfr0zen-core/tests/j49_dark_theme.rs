//! Private firmware checks for the opt-in gen 2 dark theme.
use std::path::{Path, PathBuf};
use unfr0zen_core::nlclient::{self, dark_theme, Patch};
use unfr0zen_core::shell::sha256_hex;

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

fn hex(value: &serde_json::Value) -> u32 {
    u32::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

#[test]
fn reviewed_j49_dark_theme_matches_the_golden_and_composes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let firmware = std::env::var_os("NLCLIENT_FIRMWARE_DIR").map(PathBuf::from)
        .unwrap_or_else(|| root.join(".work/firmware"));
    let Ok(stock) = std::fs::read(firmware.join("nlclient-gen2-2019")) else {
        assert!(std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(), "strict checks need the j49 sample");
        eprintln!("No private j49 sample; skipping dark theme check");
        return;
    };
    let golden: serde_json::Value = serde_json::from_str(include_str!(
        "../../../platform/common/nlclient/tests/dark-theme/j49-5.9.4-5.json")).unwrap();
    assert_eq!(sha256_hex(&stock), golden["stock_sha256"]);

    let mut output = stock.clone();
    dark_theme::patch(&stock, &mut output).unwrap();
    assert_eq!(output.len(), golden["output_size"].as_u64().unwrap() as usize);
    assert_eq!(sha256_hex(&output), golden["output_sha256"]);
    for edit in golden["edits"].as_array().unwrap() {
        let at = offset(&stock, hex(&edit[0]));
        assert_eq!(word(&stock, at), hex(&edit[1]));
        assert_eq!(word(&output, at), hex(&edit[2]));
    }
    let changed = stock.iter().zip(&output).filter(|(a, b)| a != b).count();
    assert!(changed > 0 && changed <= 4 * golden["edits"].as_array().unwrap().len());

    // Applied twice, or over a changed site, nothing is written.
    let before = output.clone();
    assert!(dark_theme::patch(&stock, &mut output).is_err());
    assert_eq!(output, before);
    let site = offset(&stock, hex(&golden["edits"][0][0]));
    let mut changed_site = stock.clone();
    changed_site[site] ^= 1;
    let untouched = changed_site.clone();
    assert!(dark_theme::patch(&stock, &mut changed_site).is_err());
    assert_eq!(changed_site, untouched);
    let mut changed_stock = stock.clone();
    changed_stock[0x1000] ^= 1;
    assert!(dark_theme::patch(&changed_stock, &mut stock.clone()).is_err());

    // Over the ordinary settings screen, as patch_on_device composes them,
    // and reversible through the journal.
    let object = std::env::var_os("NLCLIENT_OBJECT").map(PathBuf::from)
        .unwrap_or_else(|| root.join("build/custom-screen.o"));
    let Ok(object) = std::fs::read(object) else {
        assert!(std::env::var_os("NLCLIENT_REQUIRE_GOLDENS").is_none(), "strict checks need the screen object");
        return;
    };
    let mut combined = nlclient::builder::build(&stock, &object).unwrap().output;
    dark_theme::patch(&stock, &mut combined).unwrap();
    for edit in golden["edits"].as_array().unwrap() {
        assert_eq!(word(&combined, offset(&stock, hex(&edit[0]))), hex(&edit[2]));
    }
    let journal = Patch::parse(&Patch::journal(&stock, &combined).unwrap()).unwrap();
    assert_eq!(journal.apply(&stock, false).unwrap(), combined);
    assert_eq!(journal.apply(&combined, true).unwrap(), stock);
}
