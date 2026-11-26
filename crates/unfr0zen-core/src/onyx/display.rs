//! Display controls exposed by the Onyx recovery U-Boot.

use crate::common::recovery::seconds;
use crate::console::{Serial, UBoot};
use crate::Result;

pub(crate) fn log_state<S: Serial>(console: &mut UBoot<S>) -> Result<()> {
    let state = console.command_allow_failure("md.l 0x85000060 1", seconds(5))?;
    log::info!("onyx display marker: {}", state.trim());
    Ok(())
}

pub(crate) fn shutdown<S: Serial>(console: &mut UBoot<S>) -> Result<()> {
    // Blank and stop the display while the console is still connected.
    console.command("displayoff", seconds(5))?;
    Ok(())
}
