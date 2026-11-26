//! The unfr0zen initramfs's run, on every generation: the files it carries
//! (unfr0zen_core::initramfs builds it with the device's busybox and /init),
//! and running the bundle's unfr0zen-apply there over the USB shell.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use unfr0zen_core::bundle;
use unfr0zen_core::shell::InitramfsShell;
use unfr0zen_core::{Device, Result};

use crate::flow::{failure, seconds};
use crate::usb::Port;

/// Where the initramfs holds the bundle, the run's options, the host's
/// random bytes, and what unfr0zen-apply leaves for the host.
const BUNDLE: &str = "bundle.tgz";
const OPTIONS: &str = "options.json";
const ENTROPY: &str = "entropy";
const OUT: &str = "/unfr0zen";

/// The files the initramfs carries for a run: the device's bundle, the run's
/// options (bundle::Settings, serialized), and on the gen 1 and gen 2 64
/// random bytes from the host for unfr0zen-apply to credit the kernel with:
/// their Linux 2.6.37 gathers too little entropy in the initramfs for the
/// setup's random seed and salts.
pub fn files(device: Device, version: &str, settings: Vec<u8>) -> Result<BTreeMap<String, Vec<u8>>> {
    // The gen 1's and gen 2's is one bundle, the j49's; apply is told which.
    let bundled = if device == Device::Diamond { Device::J49 } else { device };
    let mut files = BTreeMap::from([
        (BUNDLE.to_string(), crate::bundle::pack(bundled, version, None)?),
        (OPTIONS.to_string(), settings),
    ]);
    if device == Device::Onyx {
        files.insert("recovery-logo.bgrx.gz".into(), crate::onyx::assets::RECOVERY_LOGO.to_vec());
        files.insert("board-init.sh".into(), crate::onyx::assets::INITRAMFS_BOARD.to_vec());
    } else if device == Device::Diamond3 {
        files.insert("board-init.sh".into(), crate::diamond3::assets::INITRAMFS_BOARD.to_vec());
    } else if matches!(device, Device::J49 | Device::Diamond) {
        let mut entropy = vec![0; 64];
        getrandom::fill(&mut entropy).map_err(|e| failure(format!("random bytes for the unit: {e}")))?;
        files.insert(ENTROPY.to_string(), entropy);
    }
    Ok(files)
}

/// Runs the bundle's unfr0zen-apply on the volume the initramfs mounted, and
/// keeps what it reports in the run directory: nlclient.json, and the unit's
/// stock nlclient when no review covers it. Returns initramfs-changes.txt.
/// `device`: the generation, which the gen 1's and gen 2's shared bundle
/// does not name.
pub fn apply(shell: &mut InitramfsShell<Port>, run_dir: &Path, device: Device) -> Result<String> {
    let omap = if matches!(device, Device::J49 | Device::Diamond) {
        format!(" --device={} --entropy=/{ENTROPY}", device.name())
    } else {
        String::new()
    };
    // No input: the shell's own (the host's nudges among it) stays out of the
    // hook scripts. The bundle leaves the initramfs's RAM once apply has read it.
    let run = shell.run_with(
        &format!(
            "tar -xzf /{BUNDLE} -C / {} && /{} /{BUNDLE} /{OPTIONS} /mnt {OUT} --remove-bundle{omap} < /dev/null 2>&1",
            bundle::APPLY,
            bundle::APPLY
        ),
        seconds(900),
        false,
    );
    match &run {
        Ok(output) => output.lines().for_each(|line| log::info!("unit: {line}")),
        Err(error) => log::error!("unfr0zen-apply failed: {error}"),
    }
    // A port that failed reaches nothing more on the unit: its error is the run's.
    if let Err(error @ unfr0zen_core::Error::Io(_)) = run {
        return Err(error);
    }
    let stock = shell.read_file(&format!("{OUT}/nlclient.stock"))?.map(|(_, _, data)| data);
    if let Some(data) = &stock {
        fs::write(run_dir.join("nlclient.stock"), data)?;
    }
    let saved = || {
        format!(
            "The unit's stock nlclient is saved as {}: attach it to a bug report",
            run_dir.join("nlclient.stock").display()
        )
    };
    if let Err(error) = run {
        return Err(if stock.is_some() { failure(format!("{error}. {}", saved())) } else { error });
    }
    let (_, _, result) =
        shell.read_file(&format!("{OUT}/result.json"))?.ok_or_else(|| failure("unfr0zen-apply left no result.json"))?;
    let result: serde_json::Value =
        serde_json::from_slice(&result).map_err(|e| failure(format!("unfr0zen-apply's result.json: {e}")))?;
    if !result["nlclient"].is_null() {
        fs::write(run_dir.join("nlclient.json"), format!("{:#}\n", result["nlclient"]))?;
    }
    if stock.is_some() {
        log::info!("{}", saved());
    }
    let changes =
        result["changes"].as_array().ok_or_else(|| failure("unfr0zen-apply's result.json lists no changes"))?;
    Ok(changes
        .iter()
        .map(|change| {
            let field = |key: &str| change[key].as_str().unwrap_or("-").to_string();
            format!("{} {}: {} -> {}\n", field("action"), field("name"), field("old"), field("new"))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imx6_board_files_match_the_selected_device() {
        let gen3 = files(Device::Diamond3, "0.0.0", Vec::new()).unwrap();
        assert_eq!(gen3["board-init.sh"], crate::diamond3::assets::INITRAMFS_BOARD);
        assert!(!gen3.contains_key("recovery-logo.bgrx.gz"));

        let onyx = files(Device::Onyx, "0.0.0", Vec::new()).unwrap();
        assert_eq!(onyx["board-init.sh"], crate::onyx::assets::INITRAMFS_BOARD);
        assert_eq!(onyx["recovery-logo.bgrx.gz"], crate::onyx::assets::RECOVERY_LOGO);

        let gen2 = files(Device::J49, "0.0.0", Vec::new()).unwrap();
        assert!(!gen2.contains_key("board-init.sh"));
        assert_eq!(gen2[ENTROPY].len(), 64);
    }
}
