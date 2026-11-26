//! The gen1 (diamond, TI AM3703). Its Wi-Fi, sleep and nlclient are the
//! gen 2's; its CC2530 radio is on a UART nestlocal does not drive.

use super::{Device, OMAP_POWER};

pub static DEVICE: Device =
    Device { name: "diamond", generation: 1, radio: None, ble: None, hci: None, display: 320, power: OMAP_POWER };
