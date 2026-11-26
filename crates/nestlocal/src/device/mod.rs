//! The unit nestlocal runs on: what differs between the Nest generations,
//! each in its own module, and which one this is.

mod diamond;
mod diamond3;
mod j49;
mod onyx;

use std::sync::OnceLock;

/// The gen 1's and gen 2's: the backplate's diamond-backplate detect line,
/// and VBUS as the TWL4030's USB PHY sees it (musb's own vbus file always
/// reads off on the OMAP).
const OMAP_POWER: Power = Power {
    backplate: "/sys/devices/platform/diamond-backplate/detect#/value",
    usb: "/sys/devices/platform/omap/omap_i2c.1/i2c-1/1-0048/twl4030_usb/vbus",
};

/// The 802.15.4 radio: its wiring, and the firmware nestlocal gives it.
pub struct Radio {
    /// The SPI device the radio is a slave on.
    pub spi: &'static str,
    /// The sysfs GPIO directories of its lines: nRESET and nHOST_INT, and
    /// nWAKE where it is wired.
    pub reset: &'static str,
    pub host_int: &'static str,
    pub wake: Option<&'static str>,
    /// nSSEL as a GPIO the host drives, where the SPI controller drops chip
    /// select at the end of every spidev message.
    pub chip_select: Option<ChipSelect>,
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

/// Where the unit says it has outside power, as Nest's shutdown daemon
/// (nlshutdownd) reads it: sysfs files holding 1/0 or on/off.
pub struct Power {
    /// The backplate's detect line, active low, read as its GPIO's value.
    pub backplate: &'static str,
    /// Power on the USB port.
    pub usb: &'static str,
}

/// The GPIO that can drive the radio's nSSEL in place of the SPI controller:
/// its number, and its pad's omap_mux file, in whose mode 4 the GPIO has the pin.
pub struct ChipSelect {
    pub gpio: u32,
    pub pad: &'static str,
}

/// The DA14580 BLE peripheral, a transport for Matter commissioning (on
/// Linux only).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct Ble {
    /// The name nlclient opens the chip's UART by.
    pub uart: &'static str,
    /// The UART itself.
    pub node: &'static str,
    /// Its reset line's sysfs GPIO directory.
    pub reset: &'static str,
    /// Nest's firmware for it, which nestlocal loads into its RAM.
    pub firmware: &'static str,
}

pub struct Device {
    pub name: &'static str,
    /// The model generation: 1, 2, 3, or 4 for the Thermostat E.
    pub generation: u8,
    /// The fixed 802.15.4 radio configuration. Onyx selects its target at runtime.
    pub radio: Option<Radio>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub ble: Option<Ble>,
    /// The name of the UART hciattach brings nlclient's Bluetooth controller
    /// up from, where nlclient scans for sensors through bluez itself (the
    /// gen E's), for the stand-in sensor to take over.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub hci: Option<&'static str>,
    /// The round display's width in pixels, which the settings screen's
    /// images are drawn for.
    pub display: usize,
    pub power: Power,
}

impl Device {
    /// The Thermostat E's Broadcom controller is served by BlueZ.
    pub fn bluez(&self) -> bool {
        self.generation == 4
    }

    /// The gen 1 and gen 2: an AM3703 with a WL127x, the same nlclient and
    /// the same sleep.
    pub fn omap(&self) -> bool {
        self.generation <= 2
    }
}

/// This unit. The gen1 is told by the model its U-Boot adds to the kernel's
/// command line (nlmodel=Diamond-1.N), the gen2 by its radio's platform
/// device.
pub fn current() -> &'static Device {
    static CURRENT: OnceLock<&'static Device> = OnceLock::new();
    CURRENT.get_or_init(|| {
        let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
        let compatible = std::fs::read("/proc/device-tree/compatible").unwrap_or_default();
        if is_onyx(&cmdline, &compatible) {
            &onyx::DEVICE
        } else if gen1(&cmdline) {
            &diamond::DEVICE
        } else if j49::DEVICE.radio.as_ref().is_some_and(|radio| std::path::Path::new(radio.reset).exists()) {
            &j49::DEVICE
        } else {
            &diamond3::DEVICE
        }
    })
}

fn is_onyx(cmdline: &str, compatible: &[u8]) -> bool {
    cmdline
        .split_whitespace()
        .any(|arg| arg.strip_prefix("nlmodel=Display-4").is_some_and(|rest| rest.is_empty() || rest.starts_with('.')))
        || compatible.split(|&byte| byte == 0).any(|name| name == b"nest,onyx" || name.starts_with(b"nestlabs,onyx-"))
}

/// Whether the kernel's command line names a gen 1's model.
fn gen1(cmdline: &str) -> bool {
    cmdline
        .split_whitespace()
        .any(|arg| arg.strip_prefix("nlmodel=Diamond-1").is_some_and(|rest| rest.is_empty() || rest.starts_with('.')))
}

/// This unit's 802.15.4 radio.
pub fn radio() -> Result<&'static Radio, String> {
    if current().name == "onyx" {
        return onyx::radio();
    }
    current().radio.as_ref().ok_or_else(|| format!("the {} has no 802.15.4 radio nestlocal drives", current().name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With an image embedded (NESTLOCAL_RADIO, NESTLOCAL_RADIO_EM357), its
    /// version is platform/common/radio/Makefile's VERSION, which getXncpInfo reports from the
    /// radio; without, there is none.
    #[test]
    fn a_gen_1_is_told_by_its_model_on_the_command_line() {
        assert!(gen1("root=/dev/mtdblock7 rootfstype=jffs2 console=ttyO0,115200 nlmodel=Diamond-1.12"));
        assert!(!gen1("root=/dev/mtdblock7 rootfstype=jffs2 console=ttyO0,115200 nlmodel=Display-2.6"));
        assert!(!gen1("nlmodel=Diamond-10.1"));
        assert!(!gen1(""));
    }

    #[test]
    fn onyx_is_identified_by_its_model_or_kernel_device_tree() {
        assert!(is_onyx("nlmodel=Display-4.6", b""));
        assert!(is_onyx("", b"nestlabs,onyx-evt\0fsl,imx6ul\0"));
        assert!(!is_onyx("nlmodel=Display-40.1", b"fsl,imx6ul\0"));
        assert_eq!(onyx::DEVICE.display, 320);
        assert!(!onyx::DEVICE.omap());
    }

    #[test]
    fn each_embedded_image_carries_the_makefiles_version() {
        let makefile = include_str!(concat!(env!("UNFR0ZEN_ROOT"), "/platform/common/radio/Makefile"));
        let wanted: u16 = makefile.lines().find_map(|l| l.strip_prefix("VERSION = ")).unwrap().trim().parse().unwrap();
        for (name, radio) in [
            ("diamond3", diamond3::DEVICE.radio.as_ref().unwrap()),
            ("j49", j49::DEVICE.radio.as_ref().unwrap()),
            ("onyx EM357", &onyx::EM357),
            ("onyx EM3581", &onyx::EM3581),
        ] {
            let version = radio
                .zigbee_image
                .get(radio.version_at..radio.version_at + 2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]));
            assert_eq!(version, (!radio.zigbee_image.is_empty()).then_some(wanted), "{name}");
        }
    }
}
