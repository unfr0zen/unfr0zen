//! The unit nestlocal runs on: what differs between the Nest generations,
//! each in its own module, and which one this is.

mod diamond3;
mod j49;

use std::sync::OnceLock;

/// The 802.15.4 radio: its wiring, and the firmware nestlocal gives it.
pub struct Radio {
    /// The SPI device the radio is a slave on.
    pub spi: &'static str,
    /// The sysfs GPIO directories of its lines: nRESET and nHOST_INT, and
    /// nWAKE where it is wired.
    pub reset: &'static str,
    pub host_int: &'static str,
    pub wake: Option<&'static str>,
    /// The Zigbee image nestlocal installs (platform/common/radio/, embedded at build time,
    /// empty in a build without it), and where in it its address table's
    /// customerApplicationVersion is.
    pub zigbee_image: &'static [u8],
    pub version_at: usize,
    /// The image boots as the Thread RCP, whose command 6 reboots it into the
    /// Zigbee NCP.
    pub selects_zigbee: bool,
    /// EmberZNet's transmit power mode for the board's front end.
    pub tx_power_mode: u16,
}

/// The DA14580 BLE peripheral, a transport for Matter commissioning (on
/// Linux only).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct Ble {
    pub uart: &'static str,
    /// Its reset line's sysfs GPIO directory.
    pub reset: &'static str,
    /// Nest's firmware for it, which nestlocal loads into its RAM.
    pub firmware: &'static str,
}

pub struct Device {
    pub name: &'static str,
    pub radio: Radio,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub ble: Option<Ble>,
    /// The round display's width in pixels, which the settings screen's
    /// images are drawn for.
    pub display: usize,
}

/// This unit. The gen2 is told by its radio's platform device.
pub fn current() -> &'static Device {
    static CURRENT: OnceLock<&'static Device> = OnceLock::new();
    CURRENT.get_or_init(|| {
        if std::path::Path::new(j49::DEVICE.radio.reset).exists() { &j49::DEVICE } else { &diamond3::DEVICE }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With an image embedded (NESTLOCAL_RADIO, NESTLOCAL_RADIO_EM357), its
    /// version is platform/common/radio/Makefile's VERSION, which getXncpInfo reports from the
    /// radio; without, there is none.
    #[test]
    fn each_embedded_image_carries_the_makefiles_version() {
        let makefile = include_str!("../../../../platform/common/radio/Makefile");
        let wanted: u16 = makefile.lines().find_map(|l| l.strip_prefix("VERSION = ")).unwrap().trim().parse().unwrap();
        for device in [&diamond3::DEVICE, &j49::DEVICE] {
            let radio = &device.radio;
            let version = radio.zigbee_image.get(radio.version_at..radio.version_at + 2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]));
            assert_eq!(version, (!radio.zigbee_image.is_empty()).then_some(wanted), "{}", device.name);
        }
    }
}
