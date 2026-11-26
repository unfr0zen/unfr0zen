//! The Thermostat E's recovery U-Boot: its fastboot buffer and DRAM, the
//! checks before any NAND access, its FIT configurations, and the hardware it
//! hands back to the stock U-Boot.

use std::sync::LazyLock;

use regex::Regex;

use crate::common::recovery::{check_ram_at, regex, seconds, Board, CONSOLE_OFF_USB};
use crate::console::{Serial, UBoot};
use crate::{device, Device, Result};

/// Recovery's fastboot buffer, below the video memory U-Boot reserves.
const BUFFER: u32 = 0x8400_0000;
const CAPACITY: usize = 0x0200_0000;
/// 128 MiB of DRAM.
const DRAM_END: u64 = 0x8800_0000;

pub(crate) fn board<S: Serial>() -> Board<S> {
    Board {
        device: Device::Onyx,
        buffer: BUFFER,
        capacity: CAPACITY,
        dram_end: DRAM_END,
        connect,
        // The i.MX6UL's USB gadget can drop the connection at the prompt
        // after a large NAND read: U-Boot runs the commands without returning
        // to it in between.
        chain_fit_load: true,
        fit_configuration: Some(fit_configuration),
        before_chainload,
        power_cycle: Some(power_cycle),
    }
}

/// WDOG1's control register: WDA cleared asserts WDOG_B, which goes to this
/// board's PMIC (fsl,wdog_b_poweroff in its kernel's device tree). The PMIC
/// powers the unit back on, from a power-on reset, while USB or the battery
/// supplies it.
const WDOG1_WCR: u32 = 0x020b_c000;
const WCR_WDA: u16 = 1 << 5;

fn power_cycle<S: Serial>(mut console: UBoot<S>) -> Result<()> {
    static WCR: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^020bc000: ([0-9a-fA-F]{4})\b"));
    let read = console.command(&format!("md.w {WDOG1_WCR:#x} 1"), seconds(5))?;
    let Some(wcr) = WCR.captures(&read).and_then(|c| u16::from_str_radix(&c[1], 16).ok()) else {
        return device(format!("WDOG1's control register did not read: {}", read.trim()));
    };
    crate::onyx::display::shutdown(&mut console)?;
    // One line: the power goes before any reply could come.
    console.send_last(&format!("{CONSOLE_OFF_USB}; mw.w {WDOG1_WCR:#x} {:#x}", wcr & !WCR_WDA))
}

/// The buffer lies in the DRAM U-Boot reports and keeps clear of what it
/// reserves, and the recovery verified the NAND's ECC layout.
fn connect<S: Serial>(console: &mut UBoot<S>) -> Result<()> {
    let start = u64::from(BUFFER);
    check_ram_at(&console.command("bdinfo", seconds(30))?, start, start + CAPACITY as u64)?;
    crate::onyx::display::log_state(console)?;
    crate::onyx::nand::check_geometry(console)
}

fn before_chainload<S: Serial>(console: &mut UBoot<S>) -> Result<()> {
    crate::onyx::display::shutdown(console)?;
    crate::onyx::pads::restore(console)
}

/// Onyx's stock boot command uses the FIT default when its model has no node.
pub fn fit_configuration<'a>(report: &str, model: &'a str) -> Result<Option<&'a str>> {
    static CONFIG: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^\s*Configuration \d+ \(([^)]+)\)\s*$"));
    static DEFAULT: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^\s*Default Configuration: '([^']+)'\s*$"));
    let has = |name: &str| {
        CONFIG.captures_iter(report).any(|node| {
            let unit = &node[1];
            unit == name || (!name.contains('@') && unit.split_once('@').is_some_and(|(base, _)| base == name))
        })
    };
    if has(model) {
        return Ok(Some(model));
    }
    if !DEFAULT.captures(report).is_some_and(|default| has(&default[1])) {
        return device(format!("Onyx FIT has neither a {model} configuration nor a usable default"));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::recovery::tests::{board_for, installed_environment, open, uboot, Unit};

    #[test]
    fn onyx_transfers_use_its_buffer_and_refuse_larger_uploads() {
        let (mut board, unit) = board_for(Unit { dram: 0x0800_0000, ..Unit::default() }, Device::Onyx);
        let data = b"Onyx recovery payload";
        board.stage(data).unwrap();
        assert_eq!(board.upload_ram(data.len()).unwrap(), data);
        assert_eq!(board.buffer_capacity(), 32 * 1024 * 1024);
        assert!(board.upload_ram(board.buffer_capacity() + 1).is_err());
        assert!(board.stage(&[]).is_err());
        assert!(unit.borrow().lines.iter().any(|line| line.starts_with("crc32 0x84000000 ")));
        assert!(check_ram_at("DRAM bank = 0\n-> start = 0x80000000\n-> size = 0x08000000\n", 0x8400_0000, 0x8600_0000)
            .is_ok());
    }

    #[test]
    fn onyx_stops_before_nand_access_unless_the_recovery_verified_its_ecc_layout() {
        let (recovery, unit) = open(Unit { dram: 0x0800_0000, unverified_nand: true, ..Unit::default() }, Device::Onyx);
        let error = recovery.err().unwrap();
        assert!(error.to_string().contains("(unverified)"), "{error}");
        assert!(!unit.borrow().lines.iter().any(|line| line.starts_with("ubi") || line.starts_with("nand")));
    }

    #[test]
    fn onyx_rejects_fastboot_when_video_reservation_covers_its_buffer() {
        let bdinfo = "DRAM bank = 0\n-> start = 0x80000000\n-> size = 0x08000000\n";
        let bound = |reserved| format!("{bdinfo} reserved[0]\t[0x{reserved}-0x87ffffff]");
        assert!(check_ram_at(&bound("85e842b0"), 0x8400_0000, 0x8600_0000)
            .unwrap_err()
            .to_string()
            .contains("reserved RAM"));
        assert!(check_ram_at(&bound("86e842b0"), 0x8400_0000, 0x8600_0000).is_ok());
    }

    #[test]
    fn onyx_starts_its_firmware_with_a_power_cycle_not_the_chainload() {
        let mut unit = Unit { dram: 0x0800_0000, ..Unit::default() };
        unit.ram.push((WDOG1_WCR, 0x0034u16.to_le_bytes().to_vec()));
        let (board, unit) = board_for(unit, Device::Onyx);
        assert!(board.power_cycles());
        board.start_firmware().unwrap();
        let lines = &unit.borrow().lines;
        assert_eq!(lines[lines.len() - 2], "displayoff");
        assert_eq!(lines.last().unwrap(), "setenv stdout serial; setenv stderr serial; mw.w 0x20bc000 0x14");
        assert!(!lines.iter().any(|line| line.starts_with("nand") || line.starts_with("go ")));
    }

    #[test]
    fn onyx_uses_the_installed_fit_default_only_without_a_model_match() {
        for (model, wanted) in [("Display-4.8", ""), ("Display-4.5", "#Display-4.5")] {
            let unit = Unit {
                dram: 0x0800_0000,
                bootable: ["root0".to_string()].into(),
                fit_configurations: " Default Configuration: 'Display-4.6@1'\r\n\
                    Configuration 0 (Display-4.6@1)\r\n Configuration 1 (Display-4.5@2)\r\n"
                    .into(),
                ..Unit::default()
            };
            let (mut board, unit) = board_for(unit, Device::Onyx);
            let mut env = installed_environment();
            env.values.insert("nlmodel".into(), model.into());
            let slot = board.select_slot(env.order).unwrap();
            let (length, _) = board.stage(&[7; 0x1234]).unwrap();
            board.boot_initramfs(slot, &env, length).unwrap();
            assert!(unit
                .borrow()
                .lines
                .last()
                .unwrap()
                .ends_with(&format!("bootm 0x83000000{wanted} 0x84000000:0x1234")));
        }
        assert!(fit_configuration("Default Configuration: 'missing'", "Display-4.8").is_err());
    }

    #[test]
    fn onyx_turns_the_display_off_and_restores_the_wifi_wake_pad_before_the_stock_uboot_handoff() {
        let mut unit = Unit { dram: 0x0800_0000, ..Unit::default() };
        let mut image = uboot(0x66000, 0x4020_00d1);
        for offset in [0x404, 0x410, 0x414, 0x418, 0x420] {
            let value = u32::from_le_bytes(image[offset..offset + 4].try_into().unwrap()) - 0x0700_0000;
            image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        unit.nand.insert(0x10_0000, image);
        let (board, unit) = board_for(unit, Device::Onyx);
        assert_eq!(board.boot_vendor().unwrap().entry, 0x8080_0000);
        let lines = &unit.borrow().lines;
        assert_eq!(lines[lines.len() - 4..lines.len() - 1], ["displayoff", "mw.l 0x20e01c0 0x5", "md.l 0x20e01c0 1"]);
        assert!(lines.last().unwrap().ends_with("go 0x80800000"));
    }
}
