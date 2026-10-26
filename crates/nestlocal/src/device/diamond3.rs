//! The gen3 (diamond3, i.MX6SoloX).

use super::{Ble, Device, Radio};

pub static DEVICE: Device = Device {
    name: "diamond3",
    // An EM3581 on /dev/spidev_em35x, its lines through the em35x-gpios
    // platform device. Its image serves Thread and Zigbee alike, and loads at
    // 0x08002000 with its address table at 0x08004000, past Nest's 16 KiB
    // bootloader. Nest's wpantund.conf has TXPowerMode 2: the alternate
    // transmit path, into the board's amplifier.
    radio: Radio {
        spi: "/dev/spidev_em35x",
        reset: "/sys/bus/platform/devices/0.em35x-gpios/reset",
        host_int: "/sys/bus/platform/devices/0.em35x-gpios/irq",
        wake: Some("/sys/bus/platform/devices/0.em35x-gpios/wake"),
        zigbee_image: include_bytes!(concat!(env!("OUT_DIR"), "/radio-em3581.bin")),
        version_at: 0x2058,
        selects_zigbee: true,
        tx_power_mode: 0x0002,
    },
    ble: Some(Ble {
        uart: "/dev/ttyble0",
        reset: "/sys/bus/platform/devices/0.ble0-da14580-gpios/reset",
        firmware: "/usr/share/dialog/data/firmware/nl_ble_peripheral_app_firmware.bin",
    }),
    display: 480,
};
