//! Onyx (Thermostat E) recovery boot over USB SDP.
//!
//! The i.MX6UL boot ROM sends three reply frames per SDP command (HAB,
//! status, terminator). Leaving the terminator unread stalls the ROM's
//! control endpoint for the rest of the session. Every SDP call in this
//! module drains the terminator with `sdp::drain`.
//!
//! Two delivery paths, both proven on HAB-closed hardware:
//!
//! **Cold** (DDR not up): single session — DCD_WRITE initializes DDR
//! through the ROM, then WRITE_FILE uploads the loader, FIT, and IVT
//! head with zeroed DCD pointer, then JUMP.
//!
//! **Warm** (DDR up, e.g. from app-mode button hold): `uuu` applies
//! DCD_WRITE on the current session (reinitializing MMDC regardless of
//! state), uploads the loader, and jumps. The loader's watchdog fires
//! after 64 s. On the subsequent warm session, the Python-style upload
//! + DCD-pointer-zeroed head + JUMP delivers the recovery FIT.

use std::time::{Duration, Instant};

use unfr0zen_core::diamond3::sdp;
use unfr0zen_core::onyx::{fit, imx};
use unfr0zen_core::{Error, Result};

use crate::progress::{stage, Progress};

pub const RECOVERY_BASE: u32 = 0x8200_0000;

pub(crate) struct LoaderLog;

impl Progress for LoaderLog {
    fn begin(&mut self, label: &str) {
        log::info!("{label}");
    }
    fn end(&mut self, _: &str, _: Duration, _: bool) {}
    fn status(&mut self, message: &str) {
        log::info!("{message}");
    }
    fn waiting(&mut self, message: &str) {
        log::info!("{message}");
    }
}

/// Drain the i.MX6UL ROM's terminator frame after an SDP operation.
fn drain(rom: &mut impl sdp::Hid) {
    sdp::drain(rom, 200);
}

fn expect_write(outcome: sdp::WriteOutcome, label: &str) -> Result<()> {
    match outcome {
        sdp::WriteOutcome::Completed => Ok(()),
        sdp::WriteOutcome::Rejected => Err(Error::Device(format!("{label} rejected by the ROM"))),
        sdp::WriteOutcome::Silent => Err(Error::Device(format!("{label} not acknowledged"))),
    }
}

/// DCD_WRITE: sends the DCD block to the ROM which applies it internally.
/// The acknowledgement may take 30 s while DDR training completes.
pub fn apply_dcd(rom: &mut impl sdp::Hid, loader: &[u8], progress: &mut dyn Progress) -> Result<()> {
    let ivt = imx::parse_ivt(loader)?;
    let dcd_offset =
        ivt.dcd.checked_sub(ivt.self_addr).ok_or_else(|| Error::Device("DCD below the image start".into()))? as usize;
    let dcd_len = u16::from_be_bytes(loader[dcd_offset + 1..dcd_offset + 3].try_into().unwrap()) as usize;
    let dcd_block = &loader[dcd_offset..dcd_offset + dcd_len];

    stage(progress, "Apply DCD through the ROM", || {
        sdp::command(rom, 0x0A0A, 0x0091_0000, dcd_block.len() as u32)?;
        let mut packet = vec![0u8; 1 + sdp::HID_DATA_SIZE];
        packet[0] = 2;
        packet[1..1 + dcd_block.len()].copy_from_slice(dcd_block);
        rom.write(&packet)?;
        for _ in 0..10 {
            let reply = rom.read(30_000)?;
            if reply.first() == Some(&3) {
                continue;
            }
            if reply.len() >= 5 && reply[0] == 4 {
                if reply[1..5] == [0x12, 0x8a, 0x8a, 0x12] {
                    drain(rom);
                    return Ok(());
                }
                return Err(Error::Device(format!("DCD_WRITE rejected: {:02x?}", &reply[1..5])));
            }
        }
        Err(Error::Timeout("DCD_WRITE not acknowledged".into()))
    })?;
    progress.status(&format!("DCD applied: {} bytes", dcd_len));
    Ok(())
}

/// Uploads the loader, FIT, and IVT head with zeroed DCD pointer, then
/// jumps. DDR must already be initialized.
fn upload_and_jump(
    rom: &mut impl sdp::Hid,
    loader: &[u8],
    built_fit: &[u8],
    progress: &mut dyn Progress,
) -> Result<String> {
    let ivt = imx::parse_ivt(loader)?;

    stage(progress, "Upload the signed loader", || {
        let r = expect_write(sdp::write_file(rom, ivt.self_addr, loader)?, "loader upload");
        drain(rom);
        r
    })?;

    stage(progress, "Upload the recovery FIT", || {
        let r = expect_write(sdp::write_file(rom, imx::FIT_BASE, built_fit)?, "FIT upload");
        drain(rom);
        r
    })?;

    let mut head = loader[..sdp::HID_DATA_SIZE].to_vec();
    head[12..16].copy_from_slice(&[0; 4]);
    stage(progress, "Zero the IVT DCD pointer", || {
        let r = expect_write(sdp::write_file(rom, ivt.self_addr, &head)?, "IVT head upload");
        drain(rom);
        r
    })?;

    let replies = stage(progress, "Enter the signed loader", || sdp::jump(rom, ivt.self_addr))?;

    match sdp::hab_state(&replies) {
        Some(state) => {
            progress.status(&format!("HAB {state}"));
            if let Some(refusal) = sdp::jump_refusal(&replies) {
                let word: [u8; 4] = refusal[1..5].try_into().unwrap();
                return Err(Error::Device(format!("the ROM refused the jump: {}", sdp::hab_word_description(word))));
            }
            Ok(format!("recovery FIT entered through the signed loader; HAB {state}"))
        }
        None => Err(Error::Device("the ROM sent no HAB report for the jump".into())),
    }
}

/// Cold path: DCD_WRITE + upload + jump in a single session.
pub fn load_recovery_cold(
    rom: &mut impl sdp::Hid,
    loader: &[u8],
    built_fit: &[u8],
    progress: &mut dyn Progress,
) -> Result<String> {
    apply_dcd(rom, loader, progress)?;
    upload_and_jump(rom, loader, built_fit, progress)
}

/// Warm delivery: upload + DCD-pointer-zeroed head + jump.
pub fn load_recovery_warm(
    rom: &mut impl sdp::Hid,
    loader: &[u8],
    built_fit: &[u8],
    progress: &mut dyn Progress,
) -> Result<String> {
    upload_and_jump(rom, loader, built_fit, progress)
}

/// Single-session recovery load for QEMU (where HAB is unlocked and the
/// DCD does not destroy DDR). Applies the DCD through WRITE_REGISTER,
/// uploads the FIT and loader, and jumps.
pub fn load_recovery(
    rom: &mut impl sdp::Hid,
    loader: &[u8],
    donor: &[u8],
    kernel: &[u8],
    fdt: &[u8],
    progress: &mut dyn Progress,
) -> Result<String> {
    let ivt = imx::parse_ivt(loader)?;
    let commands = imx::dcd_commands(loader, &ivt)?;
    let count = commands.len();
    stage(progress, "Apply the signed loader's register program", || {
        for command in &commands {
            match command {
                imx::DcdCommand::Write(write) => {
                    let value = match write.mask {
                        Some(mask) => {
                            let current = sdp::read_word(rom, write.address)?;
                            (current & !mask) | (write.value & mask)
                        }
                        None => write.value,
                    };
                    sdp::write_register(rom, write.address, value)?;
                }
                imx::DcdCommand::Check { address, mask, set } => {
                    let deadline = Instant::now() + Duration::from_secs(30);
                    loop {
                        let value = sdp::read_word(rom, *address)? & mask;
                        if value == if *set { *mask } else { 0 } {
                            break;
                        }
                        if Instant::now() >= deadline {
                            return Err(Error::Timeout(format!("DCD check at {address:#x} did not settle")));
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            }
        }
        Ok(())
    })?;
    progress.status(&format!("loader enters at {:#x}, {count} DCD operations", ivt.entry));

    let built = stage(progress, "Build the recovery FIT", || {
        fit::build_recovery_fit(
            donor,
            &fit::RecoveryImage { kernel, fdt, load: RECOVERY_BASE, entry: RECOVERY_BASE },
            fit::TARGET_CONFIG,
        )
    })?;
    progress.status(&format!("recovery FIT: {} bytes at {:#x}", built.len(), imx::FIT_BASE));

    stage(progress, "Upload the recovery FIT", || sdp::write_checked(rom, imx::FIT_BASE, &built))?;
    stage(progress, "Upload the signed loader", || sdp::write_checked(rom, ivt.self_addr, loader))?;

    let replies = stage(progress, "Enter the signed loader", || sdp::jump(rom, ivt.self_addr))?;
    match sdp::hab_state(&replies) {
        Some(state) => {
            progress.status(&format!("HAB {state}"));
            if let Some(refusal) = sdp::jump_refusal(&replies) {
                let word: [u8; 4] = refusal[1..5].try_into().unwrap();
                return Err(Error::Device(format!("the ROM refused the jump: {}", sdp::hab_word_description(word))));
            }
            Ok(format!("recovery FIT entered through the signed loader; HAB {state}"))
        }
        None => Err(Error::Device("the ROM sent no HAB report for the jump".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ivt_dcd_pointer_is_at_byte_twelve() {
        let loader = std::fs::read(concat!(env!("OUT_DIR"), "/u-boot-sdp-onyx.hab")).unwrap();
        let ivt = imx::parse_ivt(&loader).unwrap();
        assert_eq!(
            u32::from_le_bytes(loader[12..16].try_into().unwrap()),
            ivt.dcd,
            "the DCD pointer is at IVT bytes [12,16)"
        );
    }
}
