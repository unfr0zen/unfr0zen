//! Recovery U-Boot's part of unfr0zen: the installed environment and boot
//! order, the slot choice, transfers through the fastboot buffer, and the two
//! handoffs (the initramfs boot and the stock U-Boot chainload). Port of
//! unfr0zen.py and scripts/vendor_uboot.py.

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

use crate::console::{Serial, UBoot};
use crate::{device, Result, Slot};

/// Recovery's fastboot buffer (CONFIG_FASTBOOT_BUF_* in uboot/recovery.config):
/// DDR above recovery U-Boot, which relocates below it.
pub const RAM_BASE: u32 = 0x8800_0000;
pub const RAM_LIMIT: usize = 0x1000_0000;
pub const FIT_ADDR: u32 = 0x8300_0000;
/// ANATOP's DIGPROG: the SoC type and its silicon revision.
pub const DIGPROG: u32 = 0x020c_8260;
/// Powers down the device USB PHY, so stock U-Boot and Linux start it afresh.
pub const USBPHY1_PWD: u32 = 0x020c_9004;
const ENV_SIZE: usize = 65536;

fn seconds(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid pattern")
}

/// Recovery's fastboot function, beside the console on the same USB device.
pub trait Fastboot {
    /// Sends data into the fastboot buffer at RAM_BASE ("fastboot stage").
    fn download(&mut self, data: &[u8]) -> Result<()>;
    /// Reads $fastboot_upload_size bytes of the buffer ("fastboot get_staged").
    fn upload(&mut self) -> Result<Vec<u8>>;
}

#[derive(Clone, Debug)]
pub struct Environment {
    pub generation: u8,
    pub order: [Slot; 2],
    pub volume: &'static str,
    pub values: BTreeMap<String, String>,
}

/// A 64 KiB stock environment: CRC32, generation, NUL-separated pairs.
pub fn parse_environment(image: &[u8]) -> Result<(u8, BTreeMap<String, String>)> {
    if image.len() != ENV_SIZE {
        return device("installed environment has the wrong size");
    }
    if crc32fast::hash(&image[5..]) != u32::from_le_bytes(image[..4].try_into().unwrap()) {
        return device("installed environment CRC32 mismatch");
    }
    let mut values = BTreeMap::new();
    for entry in image[5..].split(|&b| b == 0) {
        if entry.is_empty() || entry[0] == 0xff {
            break;
        }
        let text = String::from_utf8_lossy(entry);
        if let Some((key, value)) = text.split_once('=') {
            values.insert(key.to_string(), value.to_string());
        }
    }
    Ok((image[4], values))
}

/// The slots bootcmd tries, first two; each nandbootN must select rootN.
pub fn boot_order(values: &BTreeMap<String, String>) -> Result<[Slot; 2]> {
    static RUN: LazyLock<Regex> = LazyLock::new(|| regex(r"\brun nandboot([01])\b"));
    let bootcmd = values.get("bootcmd").map(String::as_str).unwrap_or("");
    let numbers: Vec<&str> = RUN.captures_iter(bootcmd).map(|c| c.get(1).unwrap().as_str()).collect();
    if numbers.len() < 2 || numbers[0] == numbers[1] {
        return device("installed boot command does not select both root slots");
    }
    let mut order = [Slot::Root0; 2];
    for (place, number) in order.iter_mut().zip(&numbers[..2]) {
        let script = values.get(&format!("nandboot{number}")).map(String::as_str).unwrap_or("");
        if !script.contains(&format!("ubifsmount ubi0:root{number}"))
            || !script.contains(&format!("root=ubi0:root{number}")) {
            return device(format!("installed nandboot{number} does not select root{number}"));
        }
        *place = Slot::parse(&format!("root{number}")).unwrap();
    }
    Ok(order)
}

/// The newest generation; generations count modulo 256.
pub fn newest(candidates: Vec<Environment>) -> Option<Environment> {
    candidates.into_iter().reduce(|newest, candidate| {
        let ahead = candidate.generation.wrapping_sub(newest.generation);
        if 0 < ahead && ahead < 128 { candidate } else { newest }
    })
}

/// The vendor FIT has one kernel and nine FDTs in 5.9.4-5, ten in 6.4-8.
/// iminfo prints one "sha256+" for each valid hash and "Bad hash in FIT
/// image!" on failure: at least the 5.9.4-5 count, and no bad one.
pub fn verified_fit(output: &str) -> bool {
    output.contains("FIT image found") && !output.contains("Bad hash") && output.matches("sha256+").count() >= 10
}

pub fn parse_crc(response: &str) -> Option<u32> {
    static CRC: LazyLock<Regex> = LazyLock::new(|| regex(r"==> ([0-9a-fA-F]{8})"));
    CRC.captures(response).map(|c| u32::from_str_radix(&c[1], 16).unwrap())
}

/// The SoC and its silicon revision, from DIGPROG, as U-Boot's get_cpu_rev reads it.
pub fn silicon(digprog: u32) -> String {
    let name = match (digprog >> 16) & 0xff {
        0x62 => "i.MX6SoloX".to_string(),
        other => format!("i.MX6 type {other:#04x}"),
    };
    format!("{name} rev {}.{}", ((digprog >> 8) & 0xff) + 1, digprog & 0xff)
}

/// `count` words of RAM at `address`, as `md.l` printed them.
pub fn md_words(response: &str, address: u32, count: usize) -> Result<Vec<u32>> {
    static LINE: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^([0-9a-fA-F]{8}):((?: [0-9a-fA-F]{8})+)"));
    let mut words = Vec::new();
    for line in LINE.captures_iter(response) {
        if u32::from_str_radix(&line[1], 16).ok() == Some(address + 4 * words.len() as u32) {
            words.extend(line[2].split_whitespace().map(|w| u32::from_str_radix(w, 16).unwrap()));
        }
    }
    if words.len() < count {
        return device(format!("md.l {address:#x} gave {} of {count} words", words.len()));
    }
    words.truncate(count);
    Ok(words)
}

/// The UBIFS image in a volume, from its superblock: leb_cnt LEBs of leb_size.
pub fn superblock_length(words: &[u32], slot: Slot) -> Result<usize> {
    if words.len() != 11 || words[0] != 0x0610_1831 || words[5] & 0xff != 6 {
        return device(format!("{slot} does not start with a UBIFS superblock"));
    }
    let (leb_size, leb_cnt) = (words[9] as usize, words[10] as usize);
    let length = leb_size * leb_cnt;
    if length == 0 || length > RAM_LIMIT {
        return device(format!("{slot} UBIFS is {leb_cnt} LEBs of {leb_size}: larger than the \
                               {RAM_LIMIT}-byte fastboot buffer"));
    }
    Ok(length)
}

/// The DRAM banks bdinfo reports: (start, size).
pub fn dram_banks(bdinfo: &str) -> Vec<(u64, u64)> {
    static BANK: LazyLock<Regex> = LazyLock::new(|| {
        regex(r"(?i)-> start\s*=\s*0x([0-9a-f]+)\s*-> size\s*=\s*0x([0-9a-f]+)")
    });
    BANK.captures_iter(bdinfo)
        .map(|c| (u64::from_str_radix(&c[1], 16).unwrap(), u64::from_str_radix(&c[2], 16).unwrap()))
        .collect()
}

/// The kernel only sees the DRAM U-Boot reports; the initramfs must lie in it.
pub fn check_ram(bdinfo: &str, end: u64) -> Result<()> {
    let banks = dram_banks(bdinfo);
    if !banks.iter().any(|&(start, size)| start <= RAM_BASE as u64 && end <= start + size) {
        return device(format!("U-Boot reports DRAM {banks:x?}; the initramfs up to {end:#x} \
                               would lie outside the memory the kernel is given"));
    }
    Ok(())
}

/// The installed nandbootN's kernel arguments, with the initramfs's added.
pub fn initramfs_bootargs(values: &BTreeMap<String, String>, slot: Slot) -> Result<String> {
    static SETENV: LazyLock<Regex> = LazyLock::new(|| regex(r"setenv bootargs (.+?) && bootm "));
    static VARIABLE: LazyLock<Regex> = LazyLock::new(|| regex(r"\$\{(\w+)\}"));
    let name = format!("nandboot{}", slot.number());
    let script = values.get(&name).map(String::as_str).unwrap_or("");
    let Some(found) = SETENV.captures(script) else {
        return device(format!("installed {name} sets no bootargs before bootm"));
    };
    let mut missing = None;
    let bootargs = VARIABLE.replace_all(&found[1], |variable: &regex::Captures| {
        values.get(&variable[1]).cloned().unwrap_or_else(|| {
            missing.get_or_insert_with(|| variable[1].to_string());
            String::new()
        })
    }).into_owned();
    if let Some(variable) = missing {
        return device(format!("installed environment has no {variable} for bootargs"));
    }
    if !bootargs.split_whitespace().any(|arg| arg == format!("root=ubi0:{slot}")) || bootargs.contains('\'') {
        return device(format!("unexpected installed bootargs for {slot}: {bootargs}"));
    }
    // loglevel=4: warnings and errors still reach the UART; the rest of a
    // boot log at 115200 baud is seconds of waiting.
    Ok(format!("{bootargs} rdinit=/init unfr0zen.slot={slot} loglevel=4"))
}

/// Where the stock U-Boot was found and loaded, for the chainload.
#[derive(Debug, PartialEq, Eq)]
pub struct VendorImage {
    pub offset: u32,
    pub load: u32,
    pub length: u32,
    pub entry: u32,
}

const PAGE: u32 = 0x800;
const FCB_FINGERPRINT: i64 = 0x2042_4346; // "FCB "
const IVT_OFFSET: u32 = 0x400;
const HEADER_READ: u32 = 0x1000;
const DDR: (u32, u64) = (0x8000_0000, 0xA000_0000);

/// The FCB fields `nandbcb dump` prints, or None without a valid FCB.
pub(crate) fn fcb_fields(response: &str) -> Option<BTreeMap<String, i64>> {
    static FIELD: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^\s+(\w+) = (0x[0-9a-fA-F]+|-?\d+)\s*$"));
    // The DBBT repeats fingerprint, checksum and version.
    let fcb = response.split("DBBT :").next().unwrap_or("");
    let fields: BTreeMap<String, i64> = FIELD.captures_iter(fcb).map(|c| {
        let value = c[2].strip_prefix("0x").map(|hex| i64::from_str_radix(hex, 16).unwrap())
            .unwrap_or_else(|| c[2].parse().unwrap());
        (c[1].to_string(), value)
    }).collect();
    (fields.get("fingerprint") == Some(&FCB_FINGERPRINT) && fields.contains_key("fw2_pages")).then_some(fields)
}

pub(crate) fn is_ivt(header: u32) -> bool {
    header & 0x00ff_ffff == 0x0020_00d1 && header >> 28 == 0x4
}

pub struct Recovery<S: Serial, F: Fastboot> {
    pub console: UBoot<S>,
    pub fastboot: F,
    /// The FIT select_slot verified at FIT_ADDR: slot, size, CRC32.
    fit: Option<(Slot, usize, u32)>,
}

impl<S: Serial, F: Fastboot> Recovery<S, F> {
    pub fn new(console: UBoot<S>, fastboot: F) -> Self {
        Recovery { console, fastboot, fit: None }
    }

    pub fn attach_ubi(&mut self) -> Result<()> {
        self.console.command("setenv mtdparts 'mtdparts=gpmi-nand:4m(boot),500m(ubipart),8m(oopsdata)'",
                             seconds(60))?;
        self.console.command("ubi part ubipart", seconds(90))?;
        Ok(())
    }

    pub fn checked_crc(&mut self, address: u32, length: usize) -> Result<u32> {
        let response = self.console.command(&format!("crc32 {address:#x} {length:#x}"), seconds(90))?;
        parse_crc(&response).map_or_else(|| device("U-Boot did not report a RAM CRC"), Ok)
    }

    /// `length` bytes of the fastboot buffer, checked against U-Boot's CRC.
    pub fn upload_ram(&mut self, length: usize) -> Result<Vec<u8>> {
        if length == 0 || length > RAM_LIMIT {
            return device(format!("RAM upload must be between 1 and {RAM_LIMIT} bytes"));
        }
        let expected = self.checked_crc(RAM_BASE, length)?;
        self.console.command(&format!("setenv fastboot_upload_size {length:#x}"), seconds(60))?;
        let data = self.fastboot.upload()?;
        if data.len() != length {
            return device(format!("fastboot upload returned {} bytes; expected {length}", data.len()));
        }
        let actual = crc32fast::hash(&data);
        if actual != expected {
            return device(format!("fastboot upload CRC mismatch: host {actual:08x}, RAM {expected:08x}"));
        }
        Ok(data)
    }

    /// The newest valid env0/env1 and the boot order it installs.
    pub fn read_installed_environment(&mut self) -> Result<Environment> {
        let mut candidates = Vec::new();
        for volume in ["env0", "env1"] {
            let mut read = || -> Result<Environment> {
                let response = self.console.command(&format!("ubi read {RAM_BASE:#x} {volume} 0x10000"), seconds(60))?;
                if !response.contains(&format!("Read 65536 bytes from volume {volume}")) {
                    return device("UBI did not return the full environment");
                }
                let (generation, values) = parse_environment(&self.upload_ram(ENV_SIZE)?)?;
                let order = boot_order(&values)?;
                Ok(Environment { generation, order, volume, values })
            };
            match read() {
                Ok(found) => {
                    log::info!("{volume}: valid generation {}, boot order {:?}", found.generation, found.order);
                    candidates.push(found);
                }
                Err(failure) => log::info!("{volume}: unavailable: {failure}"),
            }
        }
        let newest = newest(candidates).map_or_else(
            || device("could not validate either installed NAND environment"), Ok)?;
        log::info!("Using {} generation {}: {:?}", newest.volume, newest.generation, newest.order);
        Ok(newest)
    }

    /// The first slot in the stock boot order whose UBIFS mounts and whose FIT
    /// loads with every hash verified: the one the stock U-Boot would boot.
    pub fn select_slot(&mut self, order: [Slot; 2], requested: Option<Slot>) -> Result<Slot> {
        let slots = requested.map_or(order.to_vec(), |slot| vec![slot]);
        for slot in slots {
            let mounted = self.console.command_allow_failure(&format!("ubifsmount ubi0:{slot}"), seconds(60))?;
            let lower = mounted.to_lowercase();
            if ["error", "fail", "not found"].iter().any(|word| lower.contains(word)) {
                log::info!("{slot}: UBIFS mount failed; checking fallback");
                continue;
            }
            let loaded = self.console.command_allow_failure(
                &format!("ubifsload {FIT_ADDR:#x} /boot/kernel_fdt.itb"), seconds(60))?;
            // A raw `ubi read` can corrupt a live UBIFS mount's volume
            // descriptor; unmount, so nothing raw runs against a mount.
            self.console.command_allow_failure("ubifsumount", seconds(30))?;
            let lower = loaded.to_lowercase();
            if loaded.contains("Loading file '/boot/kernel_fdt.itb'") && loaded.contains("Done")
                && !lower.contains("error") && !lower.contains("fail") {
                let image = self.console.command_allow_failure(&format!("iminfo {FIT_ADDR:#x}"), seconds(30))?;
                if verified_fit(&image) {
                    // What boot_initramfs checks the FIT against, instead of
                    // loading and hashing it again.
                    let size = self.loaded_size()?;
                    self.fit = Some((slot, size, self.checked_crc(FIT_ADDR, size)?));
                    log::info!("Selected {slot}: first bootable volume in stock boot order");
                    return Ok(slot);
                }
            }
            log::info!("{slot}: FIT load or verification failed; checking fallback");
        }
        device("neither root0 nor root1 has a verified bootable FIT")
    }

    /// The size of the file the last ubifsload loaded.
    fn loaded_size(&mut self) -> Result<usize> {
        static SIZE: LazyLock<Regex> = LazyLock::new(|| regex(r"filesize=([0-9a-fA-F]+)"));
        let response = self.console.command("printenv filesize", seconds(10))?;
        SIZE.captures(&response).and_then(|c| usize::from_str_radix(&c[1], 16).ok())
            .map_or_else(|| device("U-Boot did not report the loaded file's size"), Ok)
    }

    /// The UBIFS image in the volume, sized from its superblock. Reading past
    /// it into LEBs never written left recovery U-Boot to crash.
    pub fn image_size(&mut self, slot: Slot) -> Result<usize> {
        self.console.command(&format!("ubi read {RAM_BASE:#x} {slot} 0x1000"), seconds(30))?;
        let response = self.console.command(&format!("md.l {RAM_BASE:#x} 0xb"), seconds(30))?;
        let words = md_words(&response, RAM_BASE, 11)
            .or_else(|_| device(format!("could not read the {slot} superblock from RAM")))?;
        let length = superblock_length(&words, slot)?;
        log::info!("Root volume image: {length} bytes ({} LEBs of {})", words[10], words[9]);
        Ok(length)
    }

    /// The whole slot image, for the backup.
    pub fn retrieve(&mut self, slot: Slot) -> Result<Vec<u8>> {
        let length = self.image_size(slot)?;
        let response = self.console.command(&format!("ubi read {RAM_BASE:#x} {slot} {length:#x}"), seconds(150))?;
        if !response.contains(&format!("Read {length} bytes from volume {slot}")) {
            return device(format!("UBI could not read the expected {length} bytes from {slot}"));
        }
        self.upload_ram(length)
    }

    /// Puts data in the fastboot buffer and checks it there: (length, CRC32).
    pub fn stage(&mut self, data: &[u8]) -> Result<(usize, u32)> {
        self.fastboot.download(data)?;
        let expected = crc32fast::hash(data);
        let actual = self.checked_crc(RAM_BASE, data.len())?;
        if actual != expected {
            return device(format!("fastboot RAM CRC mismatch: {actual:08x} != {expected:08x}"));
        }
        Ok((data.len(), expected))
    }

    /// Boots the slot's own verified FIT with the initramfs stage() put at
    /// RAM_BASE and checked there; nothing writes that RAM in between.
    pub fn boot_initramfs(mut self, slot: Slot, environment: &Environment, length: usize) -> Result<()> {
        static MODEL: LazyLock<Regex> = LazyLock::new(|| regex(r"^[\w.-]+$"));
        let bootargs = initramfs_bootargs(&environment.values, slot)?;
        let model = environment.values.get("nlmodel").map(String::as_str).unwrap_or("");
        if !MODEL.is_match(model) {
            return device("installed environment has no usable nlmodel");
        }
        check_ram(&self.console.command("bdinfo", seconds(30))?, RAM_BASE as u64 + length as u64)?;
        match self.fit {
            // Verified by select_slot: unchanged in RAM since, and bootm checks its hashes again.
            Some((verified, size, crc)) if verified == slot => {
                if self.checked_crc(FIT_ADDR, size)? != crc {
                    return device(format!("{slot} FIT changed in RAM since it was verified"));
                }
            }
            _ => {
                self.console.command(&format!("ubifsmount ubi0:{slot}"), seconds(60))?;
                self.console.command(&format!("ubifsload {FIT_ADDR:#x} /boot/kernel_fdt.itb"), seconds(60))?;
                self.console.command("ubifsumount", seconds(30))?;
                if !verified_fit(&self.console.command_allow_failure(&format!("iminfo {FIT_ADDR:#x}"), seconds(30))?) {
                    return device(format!("{slot} FIT failed verification before boot"));
                }
            }
        }
        // Boot the initramfs where it is: it already lies in the checked DRAM.
        self.console.command("setenv initrd_high 0xffffffff", seconds(60))?;
        self.console.command(&format!("setenv bootargs '{bootargs}'"), seconds(60))?;
        log::info!("Booting {slot} FIT #{model} with initramfs {length:#x} bytes; bootargs {bootargs}");
        // One line: the USB PHY power-down drops the ACM console.
        self.console.send_last(&format!("mw.l {USBPHY1_PWD:#x} 0x00100000; \
                                         bootm {FIT_ADDR:#x}#{model} {RAM_BASE:#x}:{length:#x}"))
    }

    fn nand_read(&mut self, address: u32, offset: u32, length: u32) -> Result<()> {
        let response = self.console.command(&format!("nand read {address:#x} {offset:#x} {length:#x}"), seconds(120))?;
        if !response.contains(&format!("{length} bytes read: OK")) {
            return device(format!("nand read {offset:#x}+{length:#x} failed"));
        }
        Ok(())
    }

    /// The SoC and its silicon revision.
    pub fn silicon(&mut self) -> Result<String> {
        Ok(silicon(self.words(DIGPROG, 1)?[0]))
    }

    fn words(&mut self, address: u32, count: usize) -> Result<Vec<u32>> {
        let response = self.console.command(&format!("md.l {address:#x} {count:#x}"), seconds(10))?;
        md_words(&response, address, count)
    }

    /// Loads the stock U-Boot where its IVT says, found through the FCB the
    /// boot ROM reads too. Fails before anything is loaded over the image's
    /// own addresses when the FCB or the IVT does not add up.
    pub fn stage_vendor(&mut self) -> Result<VendorImage> {
        // As the first NAND command after recovery starts, nandbcb sees every
        // block of the boot area as bad and prints megabytes until the
        // watchdog resets the unit; any nand command before it sets that right.
        self.console.command("nand bad", seconds(60))?;
        let response = self.console.command_allow_failure("nandbcb dump 0", seconds(60))?;
        let Some(fields) = fcb_fields(&response) else {
            return device("no valid FCB in NAND: the boot ROM could not find U-Boot either");
        };
        let (offset, room) = ((fields["fw1_start"] as u32) * PAGE, (fields["fw1_pages"] as u32) * PAGE);
        self.nand_read(RAM_BASE, offset, HEADER_READ)?;
        let ivt = self.words(RAM_BASE + IVT_OFFSET, 8)?;
        let (header, entry, boot_data, own) = (ivt[0], ivt[1], ivt[4], ivt[5]);
        if !is_ivt(header) {
            return device(format!("no i.MX IVT at NAND {:#x} (header {header:08x})", offset + IVT_OFFSET));
        }
        let load = own.wrapping_sub(IVT_OFFSET);
        // In u64: a corrupt IVT must fail here, not overflow.
        if !(load < boot_data && (boot_data as u64) < load as u64 + HEADER_READ as u64 - 12) {
            return device(format!("IVT boot data at {boot_data:#x} is outside the image header at {load:#x}"));
        }
        let boot = self.words(RAM_BASE + (boot_data - load), 3)?;
        let (start, length, plugin) = (boot[0], boot[1], boot[2]);
        if start != load || plugin != 0 {
            return device(format!("IVT boot data start {start:#x} (plugin {plugin}) does not match the IVT at {own:#x}"));
        }
        if !(HEADER_READ <= length && length <= room && DDR.0 <= load && load as u64 + length as u64 <= DDR.1
             && load <= entry && entry < load + length) {
            return device(format!("implausible U-Boot image: load {load:#x}, length {length:#x}, \
                                   entry {entry:#x}, FCB room {room:#x}"));
        }
        self.nand_read(load, offset, length)?;
        if self.words(own, 2)? != [header, entry] {
            return device(format!("the IVT at {own:#x} changed after loading the image"));
        }
        Ok(VendorImage { offset, load, length, entry })
    }

    /// Chainloads the stock U-Boot from NAND; it then runs its own bootcmd.
    pub fn boot_vendor(mut self) -> Result<VendorImage> {
        let image = self.stage_vendor()?;
        log::info!("Chainloading vendor U-Boot: NAND {:#x}, {:#x} bytes at {:#x}, entry {:#x}",
                   image.offset, image.length, image.load, image.entry);
        // One line: the USB PHY power-down drops the ACM console. A recovery
        // with its data cache on must write the loaded image out, and drop
        // instructions cached from where recovery first ran, before go.
        self.console.send_last(&format!("dcache off; icache off; mw.l {USBPHY1_PWD:#x} 0x00100000; go {:#x}",
                                        image.entry))?;
        Ok(image)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::console::fake::FakeSerial;
    use std::cell::RefCell;
    use std::collections::{BTreeSet, HashMap};
    use std::rc::Rc;

    #[test]
    fn digprog_names_the_soc_and_its_revision() {
        assert_eq!(silicon(0x0062_0101), "i.MX6SoloX rev 2.1");
        assert_eq!(silicon(0x0062_0000), "i.MX6SoloX rev 1.0");
        assert_eq!(silicon(0x0063_0002), "i.MX6 type 0x63 rev 1.2");
    }

    /// A unit behind recovery U-Boot: UBI volumes, NAND, and RAM as the latest
    /// writes, answering commands the way the real ones do.
    #[derive(Default)]
    pub(crate) struct Unit {
        volumes: HashMap<String, Vec<u8>>,
        bootable: BTreeSet<String>,
        pub(crate) nand: HashMap<u32, Vec<u8>>,
        ram: Vec<(u32, Vec<u8>)>,
        upload_size: usize,
        corrupt_uploads: bool,
        /// `nand read` reports an uncorrectable ECC error (and reads anyway).
        pub(crate) ecc_errors: bool,
        /// `nand write` flips a bit on its way to NAND.
        pub(crate) corrupt_writes: bool,
        dram: u64,
        pub(crate) lines: Vec<String>,
        loads: usize,
    }

    impl Unit {
        fn peek(&self, address: u32, length: usize) -> Vec<u8> {
            (0..length as u32).map(|i| {
                let at = address + i;
                self.ram.iter().rev().find(|(start, data)| *start <= at && at < start + data.len() as u32)
                    .map_or(0, |(start, data)| data[(at - start) as usize])
            }).collect()
        }

        fn reply(&mut self, line: &str) -> String {
            self.lines.push(line.to_string());
            let words: Vec<&str> = line.split_whitespace().collect();
            let hex = |w: &str| u32::from_str_radix(w.trim_start_matches("0x"), 16).unwrap();
            match words.as_slice() {
                ["ubi", "read", address, volume, length] => {
                    let (address, length) = (hex(address), hex(length) as usize);
                    let data = self.volumes.get(*volume).cloned().unwrap_or_default();
                    let mut read = data[..length.min(data.len())].to_vec();
                    read.resize(length, 0xff);
                    self.ram.push((address, read));
                    format!("Read {length} bytes from volume {volume} to {address:08x}\n")
                }
                ["crc32", address, length] => {
                    let crc = crc32fast::hash(&self.peek(hex(address), hex(length) as usize));
                    format!("crc32 for {} ... ==> {crc:08x}\n", address)
                }
                ["setenv", "fastboot_upload_size", size] => {
                    self.upload_size = hex(size) as usize;
                    String::new()
                }
                ["md.l", address, count] => {
                    let (address, count) = (hex(address), hex(count) as usize);
                    let data = self.peek(address, count * 4);
                    data.chunks(16).enumerate().map(|(i, chunk)| {
                        let values: String = chunk.chunks(4)
                            .map(|w| format!(" {:08x}", u32::from_le_bytes(w.try_into().unwrap()))).collect();
                        format!("{:08x}:{values}    ................\n", address + 16 * i as u32)
                    }).collect()
                }
                ["ubifsmount", volume] => {
                    let slot = volume.trim_start_matches("ubi0:");
                    if self.bootable.contains(slot) { "UBIFS mounted\n".into() } else { "UBIFS error (ubi0:0)\n".into() }
                }
                ["ubifsload", ..] => {
                    self.loads += 1;
                    self.ram.push((FIT_ADDR, b"FIT image".repeat(100)));
                    "Loading file '/boot/kernel_fdt.itb' to addr 0x83000000...\nDone\n".into()
                }
                ["printenv", "filesize"] => "filesize=384\n".into(),
                ["iminfo", _] => format!("FIT image found\n{}", "Hash(es) for Image: sha256+\n".repeat(10)),
                ["bdinfo"] => format!("DRAM bank   = 0x00000000\n-> start    = 0x80000000\n-> size     = {:#010x}\n", self.dram),
                ["nandbcb", "dump", _] => {
                    let offset = *self.nand.keys().next().unwrap();
                    let (first, pages) = (offset / PAGE, 0x80000 / PAGE);
                    format!("FCB :\n  fingerprint = 0x20424346\n  fw1_start = {first:#x}\n  fw1_pages = {pages:#x}\n  \
                             fw2_start = {:#x}\n  fw2_pages = {pages:#x}\nDBBT :\n  fingerprint = 0x54424244\n", first + pages)
                }
                ["nand", "read", address, offset, length] => {
                    let (address, offset, length) = (hex(address), hex(offset), hex(length) as usize);
                    let mut data = self.nand.get(&offset).cloned().unwrap_or_default();
                    data.resize(length, 0xff);
                    self.ram.push((address, data));
                    if self.ecc_errors {
                        return format!("\nNAND read: device 0 offset {offset:#x}, size {length:#x}\n\
                                        NAND read from offset {offset:x} failed -74\n {length} bytes read: ERROR\n");
                    }
                    format!("\nNAND read: device 0 offset {offset:#x}, size {length:#x}\n {length} bytes read: OK\n")
                }
                ["nand", "erase", offset, length] => {
                    self.nand.remove(&hex(offset));
                    format!("\nNAND erase: device 0 offset {offset}, size {length}\nErasing at {offset} -- 100% complete.\nOK\n")
                }
                ["nand", "write.trimffs", address, offset, length] => {
                    let mut data = self.peek(hex(address), hex(length) as usize);
                    if self.corrupt_writes {
                        data[0] ^= 1;
                    }
                    self.nand.insert(hex(offset), data);
                    format!("\nNAND write: device 0 offset {offset}, size {length}\n {} bytes written: OK\n", hex(length))
                }
                _ => String::new(),
            }
        }
    }

    pub(crate) struct FakeFastboot(Rc<RefCell<Unit>>);

    impl Fastboot for FakeFastboot {
        fn download(&mut self, data: &[u8]) -> Result<()> {
            self.0.borrow_mut().ram.push((RAM_BASE, data.to_vec()));
            Ok(())
        }

        fn upload(&mut self) -> Result<Vec<u8>> {
            let unit = self.0.borrow();
            let mut data = unit.peek(RAM_BASE, unit.upload_size);
            if unit.corrupt_uploads {
                data[0] ^= 1;
            }
            Ok(data)
        }
    }

    pub(crate) type Board = Recovery<FakeSerial<Box<dyn FnMut(&str) -> String>>, FakeFastboot>;

    pub(crate) fn board(unit: Unit) -> (Board, Rc<RefCell<Unit>>) {
        let unit = Rc::new(RefCell::new(unit));
        let console_unit = unit.clone();
        let respond: Box<dyn FnMut(&str) -> String> = Box::new(move |line| console_unit.borrow_mut().reply(line));
        let console = UBoot::connect(FakeSerial::new(respond), seconds(1)).unwrap();
        (Recovery::new(console, FakeFastboot(unit.clone())), unit)
    }

    fn environment(order: (u8, u8), generation: u8) -> Vec<u8> {
        let entries = [
            ("nandboot0".to_string(), "ubifsmount ubi0:root0 && root=ubi0:root0".to_string()),
            ("nandboot1".to_string(), "ubifsmount ubi0:root1 && root=ubi0:root1".to_string()),
            ("bootcmd".to_string(), format!("run nandboot{} || run nandboot{} || reset", order.0, order.1)),
        ];
        let payload: Vec<u8> = entries.iter().flat_map(|(k, v)| format!("{k}={v}\0").into_bytes()).chain([0]).collect();
        let mut image = vec![0xffu8; ENV_SIZE];
        image[4] = generation;
        image[5..5 + payload.len()].copy_from_slice(&payload);
        let crc = crc32fast::hash(&image[5..]);
        image[..4].copy_from_slice(&crc.to_le_bytes());
        image
    }

    fn installed() -> BTreeMap<String, String> {
        [("console", "ttymxc0"), ("baudrate", "115200"), ("mtdoopsdev", "3"), ("brightness", "1024"),
         ("nlmodel", "Display-3.4"), ("fit_addr", "0x83000000"),
         ("nandboot0", "ubifsmount ubi0:root0 && ubifsload ${fit_addr} /boot/kernel_fdt.itb && \
                        setenv bootargs console=${console},${baudrate} mtdoops.mtddev=${mtdoopsdev} \
                        ubi.mtd=ubipart root=ubi0:root0 rootfstype=ubifs rootwait \
                        lm3695_bl.default_brightness=${brightness} && bootm ${fit_addr}#${nlmodel}")]
            .into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn installed_environment_controls_slot_order() {
        let (_, values) = parse_environment(&environment((1, 0), 61)).unwrap();
        assert_eq!(boot_order(&values).unwrap(), [Slot::Root1, Slot::Root0]);
        let mut corrupt = environment((0, 1), 61);
        corrupt[100] ^= 1;
        assert!(parse_environment(&corrupt).is_err());
    }

    #[test]
    fn newest_environment_handles_generation_wrap() {
        let env = |generation, volume| Environment { generation, order: [Slot::Root0, Slot::Root1],
                                                     volume, values: BTreeMap::new() };
        assert_eq!(newest(vec![env(255, "env0"), env(0, "env1")]).unwrap().volume, "env1");
        assert_eq!(newest(vec![env(61, "env0"), env(60, "env1")]).unwrap().volume, "env0");
    }

    #[test]
    fn reads_the_newest_valid_environment_through_fastboot() {
        let mut unit = Unit::default();
        unit.volumes.insert("env0".into(), environment((0, 1), 60));
        unit.volumes.insert("env1".into(), environment((1, 0), 61));
        let (mut board, _) = board(unit);
        let found = board.read_installed_environment().unwrap();
        assert_eq!((found.volume, found.order), ("env1", [Slot::Root1, Slot::Root0]));
    }

    #[test]
    fn a_corrupt_upload_is_refused() {
        let mut unit = Unit { corrupt_uploads: true, ..Unit::default() };
        unit.volumes.insert("env0".into(), environment((0, 1), 60));
        let (mut board, _) = board(unit);
        assert!(board.read_installed_environment().is_err());
    }

    #[test]
    fn prefers_the_first_bootable_volume_and_falls_back() {
        let unit = Unit { bootable: ["root0".to_string(), "root1".to_string()].into(), ..Unit::default() };
        let (mut board, unit) = board(unit);
        assert_eq!(board.select_slot([Slot::Root1, Slot::Root0], None).unwrap(), Slot::Root1);
        unit.borrow_mut().bootable.remove("root1");
        assert_eq!(board.select_slot([Slot::Root1, Slot::Root0], None).unwrap(), Slot::Root0);
        assert!(board.select_slot([Slot::Root0, Slot::Root1], Some(Slot::Root1)).is_err());
    }

    #[test]
    fn fit_needs_every_hash() {
        assert!(verified_fit(&format!("FIT image found\n{}", "sha256+\n".repeat(10))));
        assert!(!verified_fit(&format!("FIT image found\n{}", "sha256+\n".repeat(9))));
        assert!(!verified_fit(&format!("FIT image found\n{}Bad hash in FIT image!", "sha256+\n".repeat(10))));
    }

    fn ubifs(lebs: u32, leb_size: u32) -> Vec<u8> {
        let mut image = vec![0u8; (lebs * leb_size) as usize];
        let words = [0x0610_1831u32, 0, 0, 0, 0, 6, 0, 0, 0, leb_size, lebs];
        for (i, word) in words.iter().enumerate() {
            image[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        image[4096] = 0x5a;
        image
    }

    #[test]
    fn backup_is_the_whole_image_its_superblock_names() {
        let mut unit = Unit::default();
        let image = ubifs(3, 4096);
        let mut volume = image.clone();
        volume.extend_from_slice(&[0xee; 8192]); // LEBs past the image are never read
        unit.volumes.insert("root0".into(), volume);
        let (mut board, _) = board(unit);
        assert_eq!(board.retrieve(Slot::Root0).unwrap(), image);
        let mut other = Unit::default();
        other.volumes.insert("root0".into(), vec![0u8; 8192]);
        assert!(board_with(other).image_size(Slot::Root0).is_err());
    }

    fn board_with(unit: Unit) -> Board {
        board(unit).0
    }

    #[test]
    fn staged_data_is_checked_in_ram() {
        let (mut board, _) = board(Unit::default());
        assert_eq!(board.stage(b"initramfs").unwrap(), (9, crc32fast::hash(b"initramfs")));
    }

    #[test]
    fn bootargs_come_from_the_installed_boot_script() {
        assert_eq!(initramfs_bootargs(&installed(), Slot::Root0).unwrap(),
                   "console=ttymxc0,115200 mtdoops.mtddev=3 ubi.mtd=ubipart root=ubi0:root0 rootfstype=ubifs \
                    rootwait lm3695_bl.default_brightness=1024 rdinit=/init unfr0zen.slot=root0 loglevel=4");
        let mut missing = installed();
        missing.remove("brightness");
        assert!(initramfs_bootargs(&missing, Slot::Root0).unwrap_err().to_string().contains("no brightness"));
        assert!(initramfs_bootargs(&installed(), Slot::Root1).unwrap_err().to_string().contains("nandboot1"));
    }

    #[test]
    fn initramfs_must_lie_in_reported_dram() {
        let bdinfo = |size: u64| format!("DRAM bank   = 0x00000000\n-> start    = 0x80000000\n-> size     = {size:#010x}\n");
        assert!(check_ram(&bdinfo(0x2000_0000), RAM_BASE as u64 + 0x10_0000).is_ok());
        assert!(check_ram(&bdinfo(0x0800_0000), RAM_BASE as u64 + 0x10_0000).is_err());
    }

    fn installed_environment() -> Environment {
        Environment { generation: 61, order: [Slot::Root0, Slot::Root1], volume: "env0", values: installed() }
    }

    #[test]
    fn boot_line_carries_the_initramfs_and_powers_down_usb() {
        let unit = Unit { dram: 0x2000_0000, bootable: ["root0".to_string()].into(), ..Unit::default() };
        let (mut board, unit) = board(unit);
        let (length, _) = board.stage(&[7u8; 0x1234]).unwrap();
        board.boot_initramfs(Slot::Root0, &installed_environment(), length).unwrap();
        let lines = &unit.borrow().lines;
        assert!(lines.contains(&"setenv initrd_high 0xffffffff".to_string()));
        assert!(lines.iter().any(|l| l.starts_with("setenv bootargs 'console=ttymxc0,115200")));
        assert_eq!(lines.last().unwrap(), "mw.l 0x20c9004 0x00100000; bootm 0x83000000#Display-3.4 0x88000000:0x1234");
    }

    #[test]
    fn boot_checks_the_verified_fit_instead_of_loading_it_again() {
        let unit = Unit { dram: 0x2000_0000, bootable: ["root0".to_string()].into(), ..Unit::default() };
        let (mut board, unit) = board(unit);
        board.select_slot([Slot::Root0, Slot::Root1], None).unwrap();
        let (length, _) = board.stage(&[7u8; 0x1234]).unwrap();
        board.boot_initramfs(Slot::Root0, &installed_environment(), length).unwrap();
        assert_eq!(unit.borrow().loads, 1);
        assert!(unit.borrow().lines.last().unwrap().starts_with("mw.l 0x20c9004 0x00100000; bootm"));
    }

    #[test]
    fn a_fit_changed_in_ram_stops_the_boot() {
        let unit = Unit { dram: 0x2000_0000, bootable: ["root0".to_string()].into(), ..Unit::default() };
        let (mut board, unit) = board(unit);
        board.select_slot([Slot::Root0, Slot::Root1], None).unwrap();
        unit.borrow_mut().ram.push((FIT_ADDR + 4, b"overwritten".to_vec()));
        let (length, _) = board.stage(&[7u8; 0x1234]).unwrap();
        let error = board.boot_initramfs(Slot::Root0, &installed_environment(), length).unwrap_err();
        assert!(error.to_string().contains("changed in RAM"), "{error}");
        assert!(!unit.borrow().lines.iter().any(|l| l.contains("bootm")));
    }

    /// A NAND boot image: the IVT 0x400 in, its boot data just after it.
    pub(crate) fn uboot(length: u32, header: u32) -> Vec<u8> {
        let (load, entry) = (0x877f_f000u32, 0x8780_0000u32);
        let mut data = vec![0u8; length as usize];
        let ivt = [header, entry, 0, 0, load + 0x420, load + 0x400, load + length - 0x2000, 0];
        for (i, word) in ivt.iter().enumerate() {
            data[0x400 + i * 4..0x404 + i * 4].copy_from_slice(&word.to_le_bytes());
        }
        for (i, word) in [load, length, 0].iter().enumerate() {
            data[0x420 + i * 4..0x424 + i * 4].copy_from_slice(&word.to_le_bytes());
        }
        data
    }

    #[test]
    fn chainload_follows_the_fcb_and_ivt() {
        let mut unit = Unit::default();
        unit.nand.insert(0x10_0000, uboot(0x66000, 0x4020_00d1));
        let (board, unit) = board(unit);
        assert_eq!(board.boot_vendor().unwrap(),
                   VendorImage { offset: 0x10_0000, load: 0x877f_f000, length: 0x66000, entry: 0x8780_0000 });
        assert_eq!(unit.borrow().lines.last().unwrap(), "dcache off; icache off; mw.l 0x20c9004 0x00100000; go 0x87800000");
        let lines = &unit.borrow().lines;
        let first = |prefix: &str| lines.iter().position(|l| l.starts_with(prefix)).unwrap();
        assert!(first("nand bad") < first("nandbcb"), "nandbcb must not be the first NAND command");
    }

    #[test]
    fn chainload_loads_nothing_over_memory_for_an_implausible_ivt() {
        let mut outside = uboot(0x66000, 0x4020_00d1);
        outside[0x410..0x414].copy_from_slice(&(0x877f_f000u32 + 0x2000).to_le_bytes());
        for (image, error) in [(uboot(0x90000, 0x4020_00d1), "implausible U-Boot image"),
                               (outside, "outside the image header")] {
            let mut unit = Unit::default();
            unit.nand.insert(0x10_0000, image);
            let (mut board, unit) = board(unit);
            assert!(board.stage_vendor().unwrap_err().to_string().contains(error));
            assert!(!unit.borrow().lines.iter().any(|l| l.starts_with("nand read 0x877ff000")));
        }
    }

    #[test]
    fn boot_order_needs_both_slots_each_selecting_its_own_root() {
        let values = |bootcmd: &str, nandboot1: &str| -> BTreeMap<String, String> {
            [("bootcmd", bootcmd), ("nandboot0", "ubifsmount ubi0:root0 && root=ubi0:root0"), ("nandboot1", nandboot1)]
                .into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        let good = "ubifsmount ubi0:root1 && root=ubi0:root1";
        assert!(boot_order(&values("run nandboot0 || run nandboot1", good)).is_ok());
        assert!(boot_order(&values("run nandboot0 || run nandboot0", good)).is_err());
        assert!(boot_order(&values("run nandboot1", good)).is_err());
        assert!(boot_order(&values("run nandboot1 || run nandboot0", "ubifsmount ubi0:root1 && root=ubi0:root0")).is_err());
        assert!(parse_environment(&environment((0, 1), 1)[..ENV_SIZE - 1]).is_err());
    }

    #[test]
    fn nothing_from_the_environment_can_break_out_of_the_boot_line() {
        let mut quoted = installed();
        quoted.insert("brightness".into(), "1024' ; nand erase.chip ; '".into());
        assert!(initramfs_bootargs(&quoted, Slot::Root0).unwrap_err().to_string().contains("unexpected installed bootargs"));
        let unit = Unit { dram: 0x2000_0000, bootable: ["root0".to_string()].into(), ..Unit::default() };
        let (board, unit) = board(unit);
        let mut environment = installed_environment();
        environment.values.insert("nlmodel".into(), "Display-3.4; nand erase.chip".into());
        assert!(board.boot_initramfs(Slot::Root0, &environment, 0x1000).unwrap_err().to_string().contains("nlmodel"));
        assert!(!unit.borrow().lines.iter().any(|l| l.contains("bootm") || l.contains("erase")));
    }

    #[test]
    fn ram_transfers_stay_inside_the_fastboot_buffer() {
        let (mut board, unit) = board(Unit::default());
        assert!(board.upload_ram(0).is_err());
        assert!(board.upload_ram(RAM_LIMIT + 1).is_err());
        assert!(unit.borrow().lines.iter().all(|l| l.is_empty()), "{:?}", unit.borrow().lines);
        let words = [0x0610_1831, 0, 0, 0, 0, 6, 0, 0, 0, 0x2_0000, 0x801];
        assert!(superblock_length(&words, Slot::Root0).unwrap_err().to_string().contains("larger than"));
        // md.l lines at other addresses do not count towards the words asked for.
        let md = "88000010: 00000005 00000006\n88000000: 00000001 00000002\n";
        assert_eq!(md_words(md, RAM_BASE, 2).unwrap(), [1, 2]);
        assert!(md_words(md, RAM_BASE, 3).is_err());
    }

    #[test]
    fn chainload_refuses_an_image_without_an_ivt() {
        let mut unit = Unit::default();
        unit.nand.insert(0x10_0000, uboot(0x66000, 0xdead_beef));
        let (mut board, _) = board(unit);
        assert!(board.stage_vendor().unwrap_err().to_string().contains("no i.MX IVT"));
    }
}
