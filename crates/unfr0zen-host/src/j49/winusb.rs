//! WinUSB for the OMAP boot ROM (0451:d00e), which x-loader and the flasher
//! keep. The ROM has no Microsoft OS descriptors, so Windows binds it no
//! driver by itself, and it gives up on USB before one could be installed
//! while it waits: the driver package is installed once, ahead of the run.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use unfr0zen_core::Result;

use crate::flow::failure;

const INF: &str = include_str!("winusb.inf");
const SCRIPT: &str = include_str!("winusb.ps1");
const INF_NAME: &str = "unfr0zen-omap-rom.inf";
/// No console window for the commands the window app runs.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn output(program: &str, args: &[&str]) -> String {
    Command::new(program).args(args).creation_flags(CREATE_NO_WINDOW).output()
        .map(|out| String::from_utf8_lossy(&out.stdout).to_ascii_lowercase()).unwrap_or_default()
}

/// Whether Windows enumerates the ROM without the BOS query and binds it
/// WinUSB: it is the driver of a ROM seen before (this package's or one
/// bound with Zadig), or this package is in the driver store for the ROM's
/// first appearance.
pub fn installed() -> bool {
    output("reg.exe", &["query", r"HKLM\SYSTEM\CurrentControlSet\Control\usbflags\0451D00E0000", "/v", "SkipBOSDescriptorQuery"])
        .contains("0x1")
        && (output("reg.exe", &["query", r"HKLM\SYSTEM\CurrentControlSet\Enum\USB\VID_0451&PID_D00E", "/s", "/v", "Service"])
            .contains("winusb")
            || output("pnputil.exe", &["/enum-drivers"]).contains(INF_NAME))
}

/// Installs the driver package through winusb.ps1, behind Windows'
/// administrator prompt.
pub fn install() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("unfr0zen-winusb-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(INF_NAME), INF)?;
    let script = dir.join("install.ps1");
    std::fs::write(&script, SCRIPT)?;
    // Inside a single-quoted PowerShell string, a quote is doubled.
    let quoted = |path: &Path| path.display().to_string().replace('\'', "''");
    let command = format!("$p = Start-Process powershell.exe -Verb RunAs -Wait -PassThru -WindowStyle Hidden \
                           -ArgumentList '-NoProfile -ExecutionPolicy Bypass -File \"{}\" -Dir \"{}\"'; exit $p.ExitCode",
                          quoted(&script), quoted(&dir));
    let status = Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &command])
        .creation_flags(CREATE_NO_WINDOW).status();
    let _ = std::fs::remove_dir_all(&dir);
    let status = status?;
    if status.success() && installed() {
        return Ok(());
    }
    Err(failure(format!("the boot ROM's USB driver (WinUSB) is not installed: the administrator prompt was declined, \
                         or the install failed ({status})")))
}
