//! Pads the boot ROM's USB download path leaves muxed for itself: a chainload
//! keeps them, and nothing on the Thermostat E's stock boot muxes them back.

use crate::common::recovery::seconds;
use crate::console::{Serial, UBoot};
use crate::{device, Result};

/// IOMUXC_SW_MUX_CTL_PAD_SD1_CLK: the Wi-Fi chip's host wake (INT_WIFI_TO_AP)
/// on GPIO2_IO17, its reset default. Left as USDHC1_CLK the host never sees
/// the chip's interrupts and its driver times out on the first reply.
const SD1_CLK_MUX: u32 = 0x020e_01c0;
const GPIO_ALT: u32 = 5;

/// Restores the pads before the stock U-Boot runs.
pub(crate) fn restore<S: Serial>(console: &mut UBoot<S>) -> Result<()> {
    console.command(&format!("mw.l {SD1_CLK_MUX:#x} {GPIO_ALT:#x}"), seconds(5))?;
    let read = console.command(&format!("md.l {SD1_CLK_MUX:#x} 1"), seconds(5))?;
    if !read.contains(&format!("{SD1_CLK_MUX:08x}: {GPIO_ALT:08x}")) {
        return device(format!("the Wi-Fi wake pad did not take its GPIO mux: {}", read.trim()));
    }
    Ok(())
}
