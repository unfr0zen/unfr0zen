//! The gen 3's recovery U-Boot: its fastboot buffer and DRAM.

use crate::common::recovery::{check_dram_at, Board};
use crate::console::Serial;
use crate::{Device, Result};

/// Recovery's fastboot buffer (CONFIG_FASTBOOT_BUF_* in uboot/recovery.config):
/// DDR above recovery U-Boot, which relocates below it.
pub const RAM_BASE: u32 = 0x8800_0000;
pub const RAM_LIMIT: usize = 0x1000_0000;

const DRAM_END: u64 = 0xA000_0000;

/// The kernel only sees the DRAM U-Boot reports; the initramfs must lie in it.
pub fn check_ram(bdinfo: &str, end: u64) -> Result<()> {
    check_dram_at(bdinfo, RAM_BASE as u64, end)
}

pub(crate) fn board<S: Serial>() -> Board<S> {
    Board {
        device: Device::Diamond3,
        buffer: RAM_BASE,
        capacity: RAM_LIMIT,
        dram_end: DRAM_END,
        connect: |_| Ok(()),
        chain_fit_load: false,
        fit_configuration: None,
        before_chainload: |_| Ok(()),
        power_cycle: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::recovery::md_words;
    use crate::common::recovery::tests::{board, Unit};

    #[test]
    fn initramfs_must_lie_in_reported_dram() {
        let bdinfo =
            |size: u64| format!("DRAM bank   = 0x00000000\n-> start    = 0x80000000\n-> size     = {size:#010x}\n");
        assert!(check_ram(&bdinfo(0x2000_0000), RAM_BASE as u64 + 0x10_0000).is_ok());
        assert!(check_ram(&bdinfo(0x0800_0000), RAM_BASE as u64 + 0x10_0000).is_err());
        // The gen 3's U-Boot reserves from its stack to the top of DRAM.
        let reserved =
            format!("{}reserved[0]\t[0x83e4a000-0x9fffffff], 0x1c1b6000 bytes flags: 0\n", bdinfo(0x2000_0000));
        assert!(check_ram(&reserved, RAM_BASE as u64 + 0x9c_b5b9).is_ok());
    }

    #[test]
    fn ram_transfers_stay_inside_the_fastboot_buffer() {
        let (mut board, unit) = board(Unit::default());
        assert!(board.upload_ram(0).is_err());
        assert!(board.upload_ram(RAM_LIMIT + 1).is_err());
        assert!(unit.borrow().lines.iter().all(|l| l.is_empty()), "{:?}", unit.borrow().lines);
        // md.l lines at other addresses do not count towards the words asked for.
        let md = "88000010: 00000005 00000006\n88000000: 00000001 00000002\n";
        assert_eq!(md_words(md, RAM_BASE, 2).unwrap(), [1, 2]);
        assert!(md_words(md, RAM_BASE, 3).is_err());
    }
}
