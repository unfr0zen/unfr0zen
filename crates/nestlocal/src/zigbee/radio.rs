//! The radio's firmware. nestlocal carries its own (platform/common/radio/ in the
//! repository, embedded at build time) and installs it with Nest's zb-loader
//! on a radio that runs another: Nest's own, or another build. And it keeps
//! wpantund, Nest's Thread daemon, from running: on every start it puts
//! Nest's firmware back.

use std::process::Command;

use crate::logf;

const LOADER: &str = "/nestlabs/bin/zb-loader";

/// The embedded image's version, its application address table's
/// customerApplicationVersion, if there is one.
pub fn version() -> Option<u16> {
    let radio = &crate::device::current().radio;
    radio.zigbee_image.get(radio.version_at..radio.version_at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

/// The processes running `name`.
fn pids(name: &str) -> Vec<i32> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten().filter_map(|entry| {
        let pid = entry.file_name().to_str()?.parse::<i32>().ok()?;
        let comm = std::fs::read_to_string(entry.path().join("comm")).ok()?;
        (comm.trim_end() == name).then_some(pid)
    }).collect()
}

/// Stops wpantund and its SPI bridge, and monit from starting it again.
pub fn stop_wpantund() {
    if pids("wpantund").is_empty() && pids("spi-server").is_empty() {
        return;
    }
    let _ = Command::new("monit").args(["stop", "wpantund"]).output();
    for pid in pids("wpantund").into_iter().chain(pids("spi-server")) {
        // SAFETY: kill takes any pid and signal number.
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
    logf!("zigbee: stopped wpantund, which puts Nest's radio firmware back");
}

/// Writes the embedded image to the radio.
pub fn install() -> Result<(), String> {
    let Some(version) = version() else {
        return Err("this nestlocal carries no radio firmware (built without NESTLOCAL_RADIO)".into());
    };
    logf!("zigbee: installing the radio's firmware, version {version}");
    let path = std::env::temp_dir().join("nestlocal-radio.bin");
    std::fs::write(&path, crate::device::current().radio.zigbee_image).map_err(|e| format!("{}: {e}", path.display()))?;
    let out = Command::new(LOADER).arg("--app-easyload").arg(&path).output();
    let _ = std::fs::remove_file(&path);
    let out = out.map_err(|e| format!("{LOADER}: {e}"))?;
    // zb-loader checks each block's CRC once written, then asks the radio for
    // its ConnectIP version, which this firmware does not answer: its exit
    // status says nothing.
    let text = String::from_utf8_lossy(&out.stdout) + String::from_utf8_lossy(&out.stderr);
    if text.contains("PASSED") && !text.contains("CRC check failed") && !text.contains("CRC check error") {
        Ok(())
    } else {
        let last = text.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("no output");
        let tail = last.get(last.len().saturating_sub(120)..).unwrap_or(last);
        Err(format!("zb-loader could not write the radio: {tail}"))
    }
}
