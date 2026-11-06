//! The NAND backups the window manages: every whole-NAND image in the app's
//! backups folder, with what the file system knows of each.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use unfr0zen_core::j49::nand::{correct_page, Environment, ENV_COPIES, ENV_SIZE, PAGE, PAGE_RAW};
use unfr0zen_host::nand;
use unfr0zen_host::Device;

/// A whole-NAND image in the folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    /// The device the image belongs to: by its size, and for the gen 1 and
    /// gen 2, which share a format, by the U-Boot environment in it.
    pub device: Option<Device>,
    pub modified: Option<SystemTime>,
    /// When it was taken, as the system writes it.
    pub taken: String,
    /// The SHA-256 in the sidecar beside it, when there is one.
    pub checksum: Option<String>,
}

impl Entry {
    pub fn name(&self) -> String {
        self.path.file_name().unwrap_or_default().to_string_lossy().into_owned()
    }

    /// The device's codename.
    pub fn device_label(&self) -> &'static str {
        self.device.map_or(crate::tr!("backup.unknown_device"), Device::name)
    }

    pub fn taken_label(&self) -> String {
        self.taken.clone()
    }
}

/// The folder and the images found in it, newest first.
#[derive(Default)]
pub struct Library {
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub selected: Option<PathBuf>,
}

impl Library {
    pub fn new(dir: PathBuf) -> Self {
        Library { dir, entries: Vec::new(), selected: None }
    }

    /// Reads the folder again. A selection that vanished is dropped; with
    /// none, the newest image is selected.
    pub fn refresh(&mut self) {
        self.entries = scan(&self.dir);
        if self.selected.as_ref().is_some_and(|s| !self.entries.iter().any(|e| &e.path == s)) {
            self.selected = None;
        }
        if self.selected.is_none() {
            self.selected = self.entries.first().map(|e| e.path.clone());
        }
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.entries.iter().find(|e| Some(&e.path) == self.selected.as_ref())
    }

    /// Where the next backup lands: a fresh, dated file in the folder.
    pub fn fresh_path(&self) -> PathBuf {
        self.dir.join(nand::default_backup())
    }

    /// Removes the image, its checksum and its bad block list.
    pub fn delete(&mut self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)?;
        for sidecar in [nand::checksum_path(path), nand::bad_blocks_path(path)] {
            match std::fs::remove_file(sidecar) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        self.refresh();
        Ok(())
    }
}

/// Every whole-NAND image in `dir`, newest first. A file is an image when
/// its size is one a supported NAND has.
pub fn scan(dir: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut entries: Vec<Entry> = read.flatten().filter_map(|file| {
        let path = file.path();
        let metadata = file.metadata().ok()?;
        let device = metadata.is_file().then(|| nand::image_device(metadata.len())).flatten()?;
        let device = match device {
            Device::J49 => omap_device(&path).unwrap_or(Device::J49),
            other => other,
        };
        Some(Entry {
            checksum: std::fs::read_to_string(nand::checksum_path(&path)).ok()
                .and_then(|text| text.split_whitespace().next().map(str::to_string)),
            path,
            size: metadata.len(),
            device: Some(device),
            taken: metadata.modified().map_or_else(|_| crate::tr!("backup.unknown_date").into(), crate::when::short),
            modified: metadata.modified().ok(),
        })
    }).collect();
    entries.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.path.cmp(&b.path)));
    entries
}

/// The generation a raw OMAP image names in its U-Boot environment: the
/// first copy that parses, its pages corrected as the flasher reads them.
fn omap_device(path: &Path) -> Option<Device> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let mut raw = vec![0u8; PAGE_RAW];
    ENV_COPIES.iter().find_map(|&offset| {
        let mut data = Vec::with_capacity(ENV_SIZE);
        for page in offset / PAGE..(offset + ENV_SIZE) / PAGE {
            file.seek(SeekFrom::Start((page * PAGE_RAW) as u64)).ok()?;
            file.read_exact(&mut raw).ok()?;
            data.extend(correct_page(&raw).map_or_else(|_| raw[..PAGE].to_vec(), |(page, _)| page));
        }
        Environment::parse(&data)?.device()
    })
}

const MIB: f64 = (1u64 << 20) as f64;

pub fn size_label(bytes: u64) -> String {
    crate::tr!("backup.size_mib", size = format!("{:.0}", bytes as f64 / MIB))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("unfr0zen-gui-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn image(dir: &Path, name: &str, len: u64) -> PathBuf {
        let path = dir.join(name);
        fs::File::create(&path).unwrap().set_len(len).unwrap();
        path
    }

    #[test]
    fn scan_lists_whole_nand_images_newest_first_with_their_checksums() {
        let dir = temp_dir("scan");
        let old = image(&dir, "nand-old.bin", 512 << 20);
        fs::write(nand::checksum_path(&old), "abc123  nand-old.bin\n").unwrap();
        let old_time = SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::File::options().write(true).open(&old).unwrap().set_modified(old_time).unwrap();
        let new = image(&dir, "kitchen.img", 276_824_064);
        image(&dir, "notes.bin", 16);
        fs::create_dir(dir.join("sub")).unwrap();

        let entries = scan(&dir);
        assert_eq!(entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>(), [new.clone(), old.clone()]);
        assert_eq!(entries[0].device, Some(Device::J49), "an OMAP image without an environment is taken as a gen 2");
        assert_eq!(entries[0].device_label(), "j49");
        assert_eq!(entries[0].checksum, None);
        assert_eq!(entries[1].device, Some(Device::Diamond3));
        assert_eq!(entries[1].checksum.as_deref(), Some("abc123"));
        assert_eq!(size_label(entries[1].size), "512 MiB");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_library_keeps_a_selection_while_it_exists_and_deletes_the_sidecars_too() {
        let dir = temp_dir("library");
        let a = image(&dir, "a.bin", 512 << 20);
        let b = image(&dir, "b.bin", 512 << 20);
        fs::write(nand::checksum_path(&b), "ff  b.bin\n").unwrap();
        fs::write(nand::bad_blocks_path(&b), "0c000000\n").unwrap();
        let mut library = Library::new(dir.clone());
        library.refresh();
        assert!(library.selected.is_some(), "the newest is selected by default");
        library.selected = Some(b.clone());
        library.refresh();
        assert_eq!(library.selected().unwrap().path, b);
        library.delete(&b).unwrap();
        assert!(!b.exists() && !nand::checksum_path(&b).exists() && !nand::bad_blocks_path(&b).exists());
        assert_eq!(library.selected().unwrap().path, a, "the selection moves to what is left");
        assert!(library.fresh_path().starts_with(&dir));
        assert!(library.fresh_path().file_name().unwrap().to_string_lossy().starts_with("nand-"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_omap_image_names_its_generation_from_its_environment() {
        let dir = temp_dir("omap");
        let path = image(&dir, "gen1.bin", 276_824_064);
        // The second environment copy, "Diamond-1.9", with its CRC, in raw
        // pages whose OOB is erased so no correction applies.
        let mut env = vec![0u8; ENV_SIZE];
        let body = b"nlmodel=Diamond-1.9\0\0";
        env[5..5 + body.len()].copy_from_slice(body);
        env[4] = 1;
        let crc = crc32fast::hash(&env[5..]).to_le_bytes();
        env[..4].copy_from_slice(&crc);
        let mut file = std::fs::File::options().write(true).open(&path).unwrap();
        for (i, chunk) in env.chunks(PAGE).enumerate() {
            use std::io::{Seek, SeekFrom, Write};
            let page = ENV_COPIES[1] / PAGE + i;
            file.seek(SeekFrom::Start((page * PAGE_RAW) as u64)).unwrap();
            file.write_all(chunk).unwrap();
            file.write_all(&[0xff; PAGE_RAW - PAGE]).unwrap();
        }
        drop(file);
        assert_eq!(omap_device(&path), Some(Device::Diamond));
        assert_eq!(scan(&dir)[0].device_label(), "diamond");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_folder_is_an_empty_library() {
        let mut library = Library::new(PathBuf::from("/nonexistent/unfr0zen-gui"));
        library.refresh();
        assert!(library.entries.is_empty() && library.selected.is_none());
    }
}
