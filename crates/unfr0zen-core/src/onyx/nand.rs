//! The NAND ECC layout the Onyx recovery U-Boot uses: before registering
//! the NAND it tries the chip's reported layout and the other BCH strengths
//! against the UBI partition's erase counter headers, and keeps the one the
//! unit's own system wrote with. Without one it leaves the NAND read-only.

use crate::common::recovery::seconds;
use crate::console::{Serial, UBoot};
use crate::{device, Result};

/// Fails unless the recovery verified its layout against the unit's NAND.
pub(crate) fn check_geometry<S: Serial>(console: &mut UBoot<S>) -> Result<()> {
    let output = console.command_allow_failure("printenv nand_geometry", seconds(5))?;
    match reported(&output) {
        Some(state) if state.starts_with("verified ") => {
            log::info!("onyx NAND ECC layout: {state}");
            Ok(())
        }
        Some(state) => device(format!(
            "the recovery could not match this unit's NAND ECC layout ({state}); stopped before any NAND access"
        )),
        None => device("the recovery does not report its NAND ECC layout; stopped before any NAND access"),
    }
}

fn reported(output: &str) -> Option<&str> {
    output.lines().find_map(|line| line.trim().strip_prefix("nand_geometry=")).map(str::trim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_layout_the_recovery_reports() {
        assert_eq!(reported("printenv nand_geometry\nnand_geometry=verified bch4/512\n=> "), Some("verified bch4/512"));
        assert_eq!(reported("nand_geometry=unverified\r\n=> "), Some("unverified"));
        assert_eq!(reported("## Error: \"nand_geometry\" not defined\n=> "), None);
    }
}
