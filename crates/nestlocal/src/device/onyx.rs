//! The Thermostat E (onyx, i.MX6UL, 320-pixel LCD).

use super::{diamond3, Device, Power, Radio};
use std::sync::OnceLock;

const STOCK_IMAGES: [&str; 2] =
    ["/usr/share/marble/data/firmware/ot-ncp-app.bin", "/usr/share/silabs/data/firmware/ip-modem-app.bin"];

pub(super) static EM357: Radio = Radio {
    zigbee_image: include_bytes!(concat!(env!("OUT_DIR"), "/radio-em357.bin")),
    version_at: 0x58,
    selects_zigbee: false,
    tx_power_mode: 0,
    ..diamond3::RADIO
};
pub(super) static EM3581: Radio = Radio { tx_power_mode: 0, ..diamond3::RADIO };

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Em357,
    Em3581,
}

/// The stock application's address table identifies its Cortex-M3 micro and
/// bootloader layout. Only the two supported targets can select an image.
fn family(image: &[u8]) -> Option<Family> {
    let header = |offset, micro| {
        image
            .get(offset..offset + 27)
            .is_some_and(|table| table[16..18] == [0xa7, 0x0a] && table[24..27] == [4, micro, 3])
    };
    if image.len() <= 0x2e000 && header(0, 3) {
        Some(Family::Em357)
    } else if image.len() <= 0x3e000 && header(0x2000, 10) && image[..0x2000].iter().all(|&byte| byte == 0) {
        Some(Family::Em3581)
    } else {
        None
    }
}

fn detect(read: impl Fn(&str) -> std::io::Result<Vec<u8>>) -> Result<&'static Radio, String> {
    let mut detected = None;
    for path in STOCK_IMAGES {
        let image = match read(path) {
            Ok(image) => image,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("Onyx radio: {path}: {e}")),
        };
        let target = family(&image).ok_or_else(|| format!("Onyx radio: unsupported application layout in {path}"))?;
        if detected.is_some_and(|previous| previous != target) {
            return Err("Onyx radio: stock firmware targets disagree".into());
        }
        detected = Some(target);
    }
    match detected {
        Some(Family::Em357) => Ok(&EM357),
        Some(Family::Em3581) => Ok(&EM3581),
        None => Err("Onyx radio: no stock firmware to identify its target".into()),
    }
}

pub(super) fn radio() -> Result<&'static Radio, String> {
    static RADIO: OnceLock<Result<&'static Radio, String>> = OnceLock::new();
    RADIO.get_or_init(|| detect(|path| std::fs::read(path))).clone()
}

pub static DEVICE: Device = Device {
    name: "onyx",
    generation: 4,
    radio: None,
    ble: None,
    hci: Some("/dev/ttyble0"),
    display: 320,
    power: Power {
        backplate: "/sys/bus/platform/devices/0.sensor-mcu-gpios/detect#/value",
        usb: "/sys/class/power_supply/imx_usb_charger/online",
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    fn image(offset: usize, micro: u8) -> Vec<u8> {
        let mut image = vec![0; offset + 27];
        image[offset + 16..offset + 18].copy_from_slice(&[0xa7, 0x0a]);
        image[offset + 24..offset + 27].copy_from_slice(&[4, micro, 3]);
        image
    }

    #[test]
    fn both_stock_radio_targets_select_their_own_image() {
        for (offset, micro, radio) in [(0, 3, &EM357), (0x2000, 10, &EM3581)] {
            let detected = detect(|_| Ok(image(offset, micro))).unwrap();
            assert!(std::ptr::eq(detected, radio));
            assert_eq!(detected.version_at, offset + 0x58);
            assert_eq!(detected.tx_power_mode, 0);
        }
        assert!(!EM357.selects_zigbee);
        assert!(EM3581.selects_zigbee);
        if !EM357.zigbee_image.is_empty() {
            assert!(family(EM357.zigbee_image) == Some(Family::Em357));
        }
        if !EM3581.zigbee_image.is_empty() {
            assert!(family(EM3581.zigbee_image) == Some(Family::Em3581));
        }
    }

    #[test]
    fn an_unknown_or_conflicting_target_cannot_select_firmware() {
        assert!(detect(|_| Err(std::io::ErrorKind::NotFound.into())).is_err());
        assert!(detect(|_| Ok(image(0, 6))).is_err());
        assert!(detect(|_| Ok(image(0, 10))).is_err());
        assert!(detect(|path| Ok(if path == STOCK_IMAGES[0] { image(0, 3) } else { image(0x2000, 10) })).is_err());
    }
}
