//! Private firmware checks for the opt-in gen 2 RCS input patch.
use super::*;
use crate::nlclient::{self, Patch};
use std::path::{Path, PathBuf};

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
fn reviewed_j49_input_is_sensor_only_mapped_and_reversible() {
    let root = Path::new(env!("UNFR0ZEN_ROOT"));
    let firmware =
        std::env::var_os("NLCLIENT_FIRMWARE_DIR").map(PathBuf::from).unwrap_or_else(|| root.join(".work/firmware"));
    let object =
        std::env::var_os("NLCLIENT_OBJECT").map(PathBuf::from).unwrap_or_else(|| root.join("build/custom-screen.o"));
    let (Ok(stock), Ok(object)) = (std::fs::read(firmware.join("nlclient-gen2-2019")), std::fs::read(object)) else {
        assert!(
            std::env::var_os("NLCLIENT_REQUIRE_FIRMWARE").is_none(),
            "strict checks need the j49 sample and screen object"
        );
        eprintln!("No private j49 sample or screen object; skipping RCS input check");
        return;
    };
    let rcs_object = std::fs::read(
        std::env::var_os("NLCLIENT_RCS_OBJECT")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("build/j49-rcs-screen.o")),
    )
    .expect("build the j49 RCS screen object");
    let screen = nlclient::builder::build(&stock, &object, &[]).unwrap().output;
    let mut output = screen.clone();
    super::patch(&stock, &mut output, &rcs_object).unwrap();
    assert_eq!(word(&output, offset(&stock, COMMAND_COUNT)), 0xe3560001);
    let table = offset(&stock, COMMAND_TABLE);
    assert_eq!(word(&output, table + 4), HANDLER);
    let command = offset(&output, word(&output, table));
    assert_eq!(&output[command..command + b"kryptonitedata\0".len()], b"kryptonitedata\0");
    let hook = word(&output, offset(&stock, HOOK));
    assert_eq!(hook >> 24, 0xea);
    let entry = target(hook, HOOK);
    let code = offset(&output, entry);
    assert!(code >= screen.len());
    assert_eq!(word(&output, code), 0xe92d500f);
    assert_eq!(target(word(&output, code + 6 * 4), entry + 6 * 4), OPERATOR_NEW);
    assert_eq!(word(&output, code + 10 * 4), 0xe5941070); // ctor takes globals
    assert_eq!(target(word(&output, code + 11 * 4), entry + 11 * 4), CONSTRUCTOR);
    assert_eq!(target(word(&output, code + 23 * 4), entry + 23 * 4), CREATE_THREAD);
    assert_eq!(target(word(&output, code + 28 * 4), entry + 28 * 4), HOOK + 4);
    assert_eq!(word(&output, offset(&stock, RUN_FLAG)), 0xe1a00000);
    assert_eq!(target(word(&output, offset(&stock, READ_CALL)), READ_CALL), entry + 29 * 4);
    // Branch destinations are in the appended segment. Each machine helper
    // starts by testing the argument or saving the registers it needs.
    for (site, opcode) in [
        (CONFIGURE, 0xe3500000),
        (POST_REPLY, 0xe3500000),
        (TEMPERATURE_CALL, 0xe92d4010),
        (TEMPERATURE_RESULT, 0xe3510000),
    ] {
        let destination = target(word(&output, offset(&stock, site)), site);
        let at = offset(&output, destination);
        assert!((code..command).contains(&at));
        assert_eq!(word(&output, at), opcode);
    }
    let ui_address = (word(&output, table) + COMMAND.len() as u32 + RETRY_REPLY.len() as u32).next_multiple_of(4);
    let bindings = screen_bindings(&Elf::parse(&stock).unwrap()).unwrap();
    let ui = nlclient::linker::link(&rcs_object, ui_address, &bindings).unwrap();
    assert_eq!(&output[offset(&output, ui_address)..], ui.bytes);
    for (site, name) in [(SENSOR_MENU, "custom_factory"), (POST_READING, "rcs_post_reading")] {
        let call = word(&output, offset(&stock, site));
        assert_eq!(call >> 24, 0xeb);
        assert_eq!(target(call, site), ui.symbols[name]);
    }
    assert_eq!(word(&output, offset(&stock, MOCK_SENSORS)), 0xe3a00000);
    assert_eq!(word(&output, offset(&stock, SETTINGS_ENTRY)), 0xeaffffbe);
    let edited_sites = [
        HOOK,
        MOCK_SENSORS,
        COMMAND_COUNT,
        RUN_FLAG,
        READ_CALL,
        OPEN_CALL,
        COMMAND_TABLE,
        COMMAND_TABLE + 4,
        SENSOR_MENU,
        SETTINGS_ENTRY,
        CONFIGURE,
        POST_READING,
        POST_REPLY,
        TEMPERATURE_CALL,
        TEMPERATURE_RESULT,
    ];
    let mut restored = output[..screen.len()].to_vec();
    for site in edited_sites {
        let at = offset(&stock, site);
        assert_ne!(word(&screen, at), word(&output, at));
        restored[at..at + 4].copy_from_slice(&screen[at..at + 4]);
    }
    // Only the executable payload segment grows. All other bytes, including
    // the loopback socket setup, teardown and original menu hooks, survive.
    let headers = word(&screen, 28) as usize;
    let count = u16::from_le_bytes(screen[44..46].try_into().unwrap()) as usize;
    let segment = (0..count)
        .map(|i| headers + i * 32)
        .find(|&h| {
            word(&screen, h) == 1 && word(&screen, h + 4) as usize + word(&screen, h + 16) as usize == screen.len()
        })
        .unwrap();
    for field in [16, 20] {
        let at = segment + field;
        assert_eq!(word(&output, at) as usize, output.len() - word(&screen, segment + 4) as usize);
        restored[at..at + 4].copy_from_slice(&screen[at..at + 4]);
    }
    assert_eq!(restored, screen);
    let mut missing_payload = screen.clone();
    assert!(super::patch(&stock, &mut missing_payload, b"").is_err());
    assert_eq!(missing_payload, screen);
    let journal = Patch::journal(&stock, &output).unwrap();
    let patch = Patch::parse(&journal).unwrap();
    assert_eq!(patch.apply(&stock, false).unwrap(), output);
    assert_eq!(patch.apply(&output, true).unwrap(), stock);
    // Deselecting the option rebuilds the ordinary screen, undoing this input.
    let off = Patch::parse(&Patch::journal(&stock, &screen).unwrap()).unwrap();
    let downgrade = Patch::upgrade(&patch, off).unwrap();
    assert_eq!(downgrade.apply(&output, false).unwrap(), screen);
    assert!(super::patch(&stock, &mut output, &rcs_object).is_err());
    for address in edited_sites {
        let mut changed = screen.clone();
        changed[offset(&stock, address)] ^= 1;
        let before = changed.clone();
        assert!(super::patch(&stock, &mut changed, &rcs_object).is_err());
        assert_eq!(changed, before);
    }
    assert!(super::patch(&stock, &mut stock.clone(), &rcs_object).is_err());
    let mut changed_stock = stock.clone();
    changed_stock[0x1000] ^= 1;
    assert!(super::patch(&changed_stock, &mut screen.clone(), &rcs_object).is_err());
}
