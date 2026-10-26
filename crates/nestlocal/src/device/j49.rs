//! The gen2 (j49, TI AM3703).

use super::{Device, Radio};

pub static DEVICE: Device = Device {
    name: "j49",
    // An EM357 on /dev/spidev2.0, nRESET (gpio 61) and nHOST_INT (gpio 182)
    // through the diamond-zigbee platform device; its nWAKE is not wired. It
    // holds one image at a time, this one the Zigbee NCP alone, loading at
    // 0x08002000 with its address table first, after Nest's 8 KiB
    // bootloader. Nest's wpantund.conf has ConnectIP's TXPowerMode 5: boost,
    // on the bidirectional port into its amplifier, which is EmberZNet's boost.
    radio: Radio {
        spi: "/dev/spidev2.0",
        reset: "/sys/devices/platform/diamond-zigbee/reset#",
        host_int: "/sys/devices/platform/diamond-zigbee/interrupt",
        wake: None,
        zigbee_image: include_bytes!(concat!(env!("OUT_DIR"), "/radio-em357.bin")),
        version_at: 0x58,
        selects_zigbee: false,
        tx_power_mode: 0x0001,
    },
    // No BLE.
    ble: None,
    display: 320,
};
