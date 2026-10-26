//! Whole-NAND jobs: back up, verify or restore the unit's NAND as an image,
//! each device in its own way. The OMAP boards have the same raw image size;
//! their generation is selected by the caller.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use unfr0zen_core::{Device, Result};

use crate::flow::failure;
use crate::progress::Progress;
use crate::{device, diamond3, j49};

pub enum Job {
    Backup(PathBuf),
    Verify(PathBuf),
    Restore(PathBuf),
}

/// nand-<date>-<time>.bin in the current directory.
pub fn default_backup() -> PathBuf {
    PathBuf::from(format!("nand-{}.bin", chrono::Local::now().format("%Y%m%d-%H%M%S")))
}

/// The format of a whole-NAND image of `len` bytes. Diamond and j49 share
/// the OMAP raw-page format, represented here by J49.
pub fn image_device(len: u64) -> Option<Device> {
    if diamond3::nand::is_image(len) {
        Some(Device::Diamond3)
    } else if j49::nand::is_image(len) {
        Some(Device::J49)
    } else {
        None
    }
}

/// A host file error, kept apart from the Io errors of a dropped USB link.
pub(crate) fn file<T>(path: &Path, result: io::Result<T>) -> Result<T> {
    result.map_err(|e| failure(format!("{}: {e}", path.display())))
}

pub(crate) fn read_at(path: &Path, offset: u64, size: usize) -> Result<Vec<u8>> {
    let mut data = vec![0; size];
    file(path, File::open(path).and_then(|mut f| {
        f.seek(SeekFrom::Start(offset))?;
        f.read_exact(&mut data)
    }))?;
    Ok(data)
}

pub(crate) fn checksum_path(image: &Path) -> PathBuf {
    let mut name = image.as_os_str().to_owned();
    name.push(".sha256");
    PathBuf::from(name)
}

/// Runs the job on the unit, whichever device it is, from whatever mode it is
/// in, and boots it normally after. `confirm` is asked before a restore
/// writes anything. Returns the final status line.
pub fn run(job: &Job, wait_usb: Duration, omap: Option<Device>, progress: &mut dyn Progress, confirm: &mut dyn FnMut(&str) -> bool)
           -> Result<String> {
    let expected = match job {
        Job::Backup(out) if out.exists() => return Err(failure(format!("{} exists", out.display()))),
        Job::Backup(_) => None,
        Job::Verify(image) | Job::Restore(image) => {
            let len = file(image, fs::metadata(image))?.len();
            Some(image_device(len).ok_or_else(|| failure(format!("{} is not a whole-NAND image ({len} bytes)", image.display())))?)
        }
    };
    let unit = device::detect(progress, wait_usb, omap)?;
    if let Some(device) = expected.filter(|&device| device != unit.device && !(device == Device::J49 && unit.device == Device::Diamond)) {
        return Err(failure(format!("the image is a {}'s NAND; the unit is a {}", device.name(), unit.device.name())));
    }
    match unit.device {
        Device::Diamond3 => diamond3::nand::run(job, unit, wait_usb, progress, confirm),
        Device::Diamond | Device::J49 => j49::nand::run(job, unit, wait_usb, progress, confirm),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checksum_sits_beside_the_image() {
        assert_eq!(checksum_path(Path::new("dir/nand.bin")), Path::new("dir/nand.bin.sha256"));
    }

    #[test]
    fn an_image_names_its_device_by_size() {
        assert_eq!(image_device(512 << 20), Some(Device::Diamond3));
        assert_eq!(image_device(276_824_064), Some(Device::J49));
        assert_eq!(image_device(553_648_128), Some(Device::J49));
        assert_eq!(image_device(16), None);
    }

    #[test]
    fn a_short_image_is_refused_before_the_unit_is_touched() {
        let dir = std::env::temp_dir().join(format!("unfr0zen-nand-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("short.bin");
        fs::write(&image, [0u8; 16]).unwrap();
        struct Silent;
        impl Progress for Silent {
            fn begin(&mut self, _: &str) {}
            fn end(&mut self, _: &str, _: Duration, _: bool) {}
            fn status(&mut self, _: &str) {}
            fn waiting(&mut self, _: &str) {}
        }
        let error = run(&Job::Verify(image), Duration::ZERO, None, &mut Silent, &mut |_| false).err().unwrap();
        assert!(error.to_string().contains("is not a whole-NAND image"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }
}
