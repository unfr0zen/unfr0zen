//! Private firmware checks for the opt-in gen 2 dark theme.
use std::path::{Path, PathBuf};
use unfr0zen_core::nlclient::{self, dark_theme, external_temperature, Patch};
#[test]
fn dark_palette_edits_compose_with_both_screen_patches() {
    let root = Path::new(env!("UNFR0ZEN_ROOT"));
    let firmware =
        std::env::var_os("NLCLIENT_FIRMWARE_DIR").map(PathBuf::from).unwrap_or_else(|| root.join(".work/firmware"));
    let Ok(stock) = std::fs::read(firmware.join("nlclient-gen2-2019")) else {
        assert!(std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_none(), "strict checks need the j49 sample");
        eprintln!("No private j49 sample; skipping dark theme check");
        return;
    };
    let mut output = stock.clone();
    dark_theme::patch(&stock, &mut output).unwrap();
    assert_eq!(output.len(), stock.len());
    let edits: Vec<_> = stock
        .iter()
        .zip(&output)
        .enumerate()
        .filter(|(_, (old, new))| old != new)
        .map(|(at, (_, new))| (at, *new))
        .collect();
    assert!(!edits.is_empty());

    // Applied twice, or over a changed site, nothing is written.
    let before = output.clone();
    assert!(dark_theme::patch(&stock, &mut output).is_err());
    assert_eq!(output, before);
    let site = edits[0].0;
    let mut changed_site = stock.clone();
    changed_site[site] ^= 1;
    let untouched = changed_site.clone();
    assert!(dark_theme::patch(&stock, &mut changed_site).is_err());
    assert_eq!(changed_site, untouched);
    let mut changed_stock = stock.clone();
    changed_stock.extend(b"build metadata");
    let mut changed_output = changed_stock.clone();
    dark_theme::patch(&changed_stock, &mut changed_output).unwrap();
    assert_eq!(&changed_output[..stock.len()], output);
    assert_eq!(&changed_output[stock.len()..], b"build metadata");

    // The same palette operations occur at different addresses in these builds.
    for name in ["nlclient-gen1-2019", "nlclient-gen2-5.9.1-6"] {
        let Ok(stock) = std::fs::read(firmware.join(name)) else {
            assert!(std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_none(), "missing {name}");
            continue;
        };
        let mut dark = stock.clone();
        dark_theme::patch(&stock, &mut dark).unwrap();
        let journal = Patch::parse(&Patch::journal(&stock, &dark).unwrap()).unwrap();
        assert_eq!(journal.apply(&dark, true).unwrap(), stock);
    }

    // Over the ordinary settings screen and the external temperature patch,
    // as set_up_on_device composes them, and reversible through the journal.
    let object =
        std::env::var_os("NLCLIENT_OBJECT").map(PathBuf::from).unwrap_or_else(|| root.join("build/custom-screen.o"));
    let rcs_object = std::env::var_os("NLCLIENT_RCS_OBJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("build/j49-rcs-screen.o"));
    let (Ok(object), Ok(rcs_object)) = (std::fs::read(object), std::fs::read(rcs_object)) else {
        assert!(std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_none(), "strict checks need both screen objects");
        return;
    };
    let screen = nlclient::builder::build(&stock, &object, &[]).unwrap().output;
    let mut combined = screen.clone();
    external_temperature::patch(&stock, &mut combined, &rcs_object).unwrap();
    dark_theme::patch(&stock, &mut combined).unwrap();
    for (at, byte) in edits {
        assert_eq!(combined[at], byte);
    }
    let journal = Patch::parse(&Patch::journal(&stock, &combined).unwrap()).unwrap();
    assert_eq!(journal.apply(&stock, false).unwrap(), combined);
    assert_eq!(journal.apply(&combined, true).unwrap(), stock);
}
