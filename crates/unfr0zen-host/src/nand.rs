//! Whole-NAND jobs: back up, verify or restore the unit's NAND as an image,
//! each device in its own way. The OMAP boards have the same raw image size;
//! their generation is selected by the caller.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use unfr0zen_core::diamond3::nand::{BLOCK, NAND_SIZE};
use unfr0zen_core::shell::sha256_hex;
use unfr0zen_core::{Device, Result};

use crate::flow::failure;
use crate::progress::{stage, Progress};
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

/// The .sha256 written beside an image.
pub fn checksum_path(image: &Path) -> PathBuf {
    let mut name = image.as_os_str().to_owned();
    name.push(".sha256");
    PathBuf::from(name)
}

/// The .bad written beside a gen 3 image: the offsets of the blocks that hold
/// no reliable data, one per line, since the image itself cannot mark them.
pub fn bad_blocks_path(image: &Path) -> PathBuf {
    let mut name = image.as_os_str().to_owned();
    name.push(".bad");
    PathBuf::from(name)
}

pub(crate) fn write_bad_blocks(path: &Path, blocks: &[u32]) -> Result<()> {
    let text: String = blocks.iter().map(|block| format!("{block:08x}\n")).collect();
    file(path, fs::write(path, text))
}

/// The blocks a list holds, or None without a list.
pub fn read_bad_blocks(path: &Path) -> Result<Option<Vec<u32>>> {
    let text = match fs::read_to_string(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        text => file(path, text)?,
    };
    text.lines().map(|line| {
        u32::from_str_radix(line, 16).ok().filter(|block| block % BLOCK == 0 && *block < NAND_SIZE)
            .ok_or_else(|| failure(format!("{}: {line:?} is not a block offset", path.display())))
    }).collect::<Result<Vec<u32>>>().map(Some)
}

/// The SHA-256 the .sha256 beside a restore's image records.
pub(crate) fn recorded_sha256(image: &Path) -> Result<String> {
    let checksum = checksum_path(image);
    let Ok(text) = fs::read_to_string(&checksum) else {
        return Err(failure(format!("{} is missing; restore only a verified backup", checksum.display())));
    };
    Ok(text.split_whitespace().next().unwrap_or_default().to_string())
}

/// Reads a restore's image and checks it against its recorded SHA-256. This
/// takes seconds, so it runs after the wait for download mode: the prompt
/// comes first.
pub(crate) fn read_checked(progress: &mut dyn Progress, image: &Path, recorded: &str) -> Result<Vec<u8>> {
    stage(progress, "Check the image's SHA-256", || {
        let data = file(image, fs::read(image))?;
        if sha256_hex(&data) != recorded {
            return Err(failure(format!("{} does not match {}", image.display(), checksum_path(image).display())));
        }
        Ok(data)
    })
}

/// Runs the job on the unit, whichever device it is, from whatever mode it is
/// in, and boots it normally after. `confirm` is asked before a restore
/// writes anything. Returns the final status line.
pub fn run(job: &Job, wait_usb: Duration, omap: Option<Device>,
           entry_point: diamond3::usb::EntryPoint, progress: &mut dyn Progress,
           confirm: &mut dyn FnMut(&str) -> bool, cancel: &device::CancelFlag)
           -> Result<String> {
    let expected = match job {
        Job::Backup(out) if out.exists() => return Err(failure(format!("{} exists", out.display()))),
        Job::Backup(_) => None,
        Job::Verify(image) | Job::Restore(image) => {
            let len = file(image, fs::metadata(image))?.len();
            Some(image_device(len).ok_or_else(|| failure(format!("{} is not a whole-NAND image ({len} bytes)", image.display())))?)
        }
    };
    let unit = device::detect(progress, wait_usb, omap, cancel)?;
    if let Some(device) = expected.filter(|&device| unit.device != Device::Onyx && device != unit.device && !(device == Device::J49 && unit.device == Device::Diamond)) {
        return Err(failure(format!("the image is a {}'s NAND; the unit is a {}", device.name(), unit.device.name())));
    }
    match unit.device {
        Device::Diamond3 => diamond3::nand::run(job, unit, wait_usb, entry_point, progress, confirm, cancel),
        Device::Diamond | Device::J49 => j49::nand::run(job, unit, wait_usb, progress, confirm, cancel),
        Device::Onyx => Err(crate::onyx::unsupported(progress)),
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
    fn a_bad_block_list_reads_back_and_refuses_what_is_no_block() {
        let dir = std::env::temp_dir().join(format!("unfr0zen-nand-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = bad_blocks_path(&dir.join("nand.bin"));
        assert_eq!(path.file_name().unwrap(), "nand.bin.bad");
        assert_eq!(read_bad_blocks(&path).unwrap(), None);
        write_bad_blocks(&path, &[]).unwrap();
        assert_eq!(read_bad_blocks(&path).unwrap(), Some(Vec::new()));
        write_bad_blocks(&path, &[0x0c00_0000, 0x1c42_0000]).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "0c000000\n1c420000\n");
        assert_eq!(read_bad_blocks(&path).unwrap(), Some(vec![0x0c00_0000, 0x1c42_0000]));
        for wrong in ["garbled\n", "00000800\n", "20000000\n"] {
            fs::write(&path, wrong).unwrap();
            assert!(read_bad_blocks(&path).is_err(), "{wrong}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_restores_image_must_have_and_match_its_checksum() {
        struct Silent;
        impl Progress for Silent {
            fn begin(&mut self, _: &str) {}
            fn end(&mut self, _: &str, _: Duration, _: bool) {}
            fn status(&mut self, _: &str) {}
            fn waiting(&mut self, _: &str) {}
        }
        let dir = std::env::temp_dir().join(format!("unfr0zen-nand-sum-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("nand.bin");
        fs::write(&image, b"image").unwrap();
        assert!(recorded_sha256(&image).unwrap_err().to_string().contains("is missing"));
        let sha = sha256_hex(b"image");
        fs::write(checksum_path(&image), format!("{sha}  nand.bin\n")).unwrap();
        assert_eq!(recorded_sha256(&image).unwrap(), sha);
        assert_eq!(read_checked(&mut Silent, &image, &sha).unwrap(), b"image");
        assert!(read_checked(&mut Silent, &image, "other").unwrap_err().to_string().contains("does not match"));
        fs::remove_dir_all(dir).unwrap();
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
        let error = run(&Job::Verify(image), Duration::ZERO, None, Default::default(), &mut Silent, &mut |_| false, &Default::default()).err().unwrap();
        assert!(error.to_string().contains("is not a whole-NAND image"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }
}
