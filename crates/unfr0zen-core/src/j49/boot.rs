//! Booting the gen 1 and gen 2 into the unfr0zen initramfs from SDRAM:
//! x-loader loads the flasher, each generation's kernel and the initramfs
//! over USB, and the flasher, once the host has read the unit's U-Boot
//! environment through it (which settles the generation), enters that
//! generation's kernel with the ATAGs a boot loader would give
//! (flasher::Flasher::boot). The initramfs then sets up the root slot on the
//! unit, as the gen 3's does.

use crate::{device, Device, Result, Slot};

/// Where x-loader puts each generation's kernel (a zImage, which
/// decompresses itself to 0x80008000), 4 MB apart, and the initramfs: above
/// the flasher's stack (0x80800000) and below what 64 MB holds.
const KERNELS: u32 = 0x8200_0000;
const KERNEL_LIMIT: usize = 4 << 20;
pub const INITRAMFS: u32 = 0x8280_0000;
/// The end of the initramfs: the kernel takes it from below 64 MB.
const INITRAMFS_END: u32 = 0x8400_0000 - (8 << 20);
/// What the flasher's boot command takes.
const ATAGS_LIMIT: usize = 1024;
const SDRAM: (u32, u32) = (0x8000_0000, 64 << 20);

/// The machine number each board's kernel takes (arch/arm/tools/mach-types).
pub fn machine(target: Device) -> Result<u32> {
    match target {
        Device::J49 => Ok(4082),
        Device::Diamond => Ok(3745),
        other => device(format!("the {} does not boot through the OMAP flasher", other.name())),
    }
}

/// Where x-loader puts the generation's kernel.
pub fn kernel(target: Device) -> Result<u32> {
    match target {
        Device::J49 => Ok(KERNELS),
        Device::Diamond => Ok(KERNELS + KERNEL_LIMIT as u32),
        other => device(format!("the {} does not boot through the OMAP flasher", other.name())),
    }
}

/// The zImage of a legacy uImage holding one (Linux, ARM, kernel, not
/// compressed by U-Boot), as build/j49/uImage is.
pub fn zimage(uimage: &[u8]) -> Result<&[u8]> {
    const HEADER: usize = 64;
    if uimage.len() <= HEADER || uimage[..4] != [0x27, 0x05, 0x19, 0x56] {
        return device("the kernel is not a U-Boot image");
    }
    let size = u32::from_be_bytes(uimage[12..16].try_into().unwrap()) as usize;
    // os 5 (Linux), arch 2 (ARM), type 2 (kernel), compression 0 (none).
    if uimage[28..32] != [5, 2, 2, 0] || HEADER + size > uimage.len() {
        return device("the U-Boot image is not an uncompressed ARM Linux kernel");
    }
    let image = &uimage[HEADER..HEADER + size];
    // A zImage's magic, at 0x24.
    if image.len() < 0x28 || image[0x24..0x28] != [0x18, 0x28, 0x6f, 0x01] {
        return device("the kernel is not a zImage");
    }
    if image.len() > KERNEL_LIMIT {
        return device(format!("the kernel ({} bytes) is larger than its {KERNEL_LIMIT} bytes of SDRAM", image.len()));
    }
    Ok(image)
}

/// The board revision U-Boot gives for an nlmodel such as "Display-2.6"
/// (0x0206); the board code reads nlmodel itself first.
fn revision(nlmodel: &str) -> u32 {
    let version = nlmodel.rsplit_once('-').map_or("", |(_, version)| version);
    match version.split_once('.').map(|(major, minor)| (major.parse::<u8>(), minor.parse::<u8>())) {
        Some((Ok(major), Ok(minor))) => u32::from(major) << 8 | u32::from(minor),
        _ => 0,
    }
}

/// The ATAGs for the initramfs of `length` bytes to set up `slot`: memory,
/// the initramfs, the board revision, and the command line the unit's own
/// U-Boot would give but for the root, with unfr0zen.slot naming it.
pub fn atags(nlmodel: &str, slot: Slot, length: usize) -> Result<Vec<u8>> {
    if INITRAMFS as usize + length > INITRAMFS_END as usize {
        return device(format!("the initramfs ({length} bytes) does not fit below {INITRAMFS_END:#x}"));
    }
    if nlmodel.is_empty() || !nlmodel.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') {
        return device(format!("the U-Boot environment's nlmodel {nlmodel:?} is not a model name"));
    }
    let mut cmdline = format!("console=ttyO0,115200 nlmodel={nlmodel} unfr0zen.slot={slot} rdinit=/init panic=5")
        .into_bytes();
    cmdline.push(0);
    cmdline.resize(cmdline.len().next_multiple_of(4), 0);
    let mut tags = Vec::new();
    let mut tag = |kind: u32, words: &[u32]| {
        tags.extend_from_slice(&(2 + words.len() as u32).to_le_bytes());
        tags.extend_from_slice(&kind.to_le_bytes());
        words.iter().for_each(|word| tags.extend_from_slice(&word.to_le_bytes()));
    };
    // ATAG_CORE: read-only root flag, page size, no root device.
    tag(0x5441_0001, &[1, 4096, 0]);
    tag(0x5441_0002, &[SDRAM.1, SDRAM.0]);
    tag(0x5442_0005, &[INITRAMFS, length as u32]);
    tag(0x5441_0007, &[revision(nlmodel)]);
    let words: Vec<u32> = cmdline.chunks(4).map(|word| u32::from_le_bytes(word.try_into().unwrap())).collect();
    tag(0x5441_0009, &words);
    // ATAG_NONE.
    tags.extend_from_slice(&[0; 8]);
    if tags.len() > ATAGS_LIMIT {
        return device("the ATAGs are longer than the flasher takes");
    }
    Ok(tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(tags: &[u8]) -> Vec<u32> {
        tags.chunks(4).map(|w| u32::from_le_bytes(w.try_into().unwrap())).collect()
    }

    #[test]
    fn the_atags_are_those_the_units_u_boot_gives_with_the_initramfs() {
        let tags = atags("Display-2.6", Slot::Root1, 0x0d_b01d).unwrap();
        let w = words(&tags);
        assert_eq!(&w[..5], [5, 0x5441_0001, 1, 4096, 0]);
        assert_eq!(&w[5..9], [4, 0x5441_0002, 64 << 20, 0x8000_0000]);
        assert_eq!(&w[9..13], [4, 0x5442_0005, INITRAMFS, 0x0d_b01d]);
        assert_eq!(&w[13..16], [3, 0x5441_0007, 0x0206]);
        assert_eq!(w[17], 0x5441_0009);
        let cmdline = &tags[18 * 4..(16 + w[16] as usize) * 4];
        let cmdline = std::str::from_utf8(cmdline).unwrap().trim_end_matches('\0');
        assert_eq!(cmdline, "console=ttyO0,115200 nlmodel=Display-2.6 unfr0zen.slot=root1 rdinit=/init panic=5");
        assert_eq!(&w[w.len() - 2..], [0, 0]);
    }

    #[test]
    fn an_initramfs_beyond_the_kernels_reach_or_a_strange_nlmodel_is_refused() {
        assert!(atags("Display-2.6", Slot::Root0, 24 << 20).is_err());
        assert!(atags("Display-2.6 init=/bin/sh", Slot::Root0, 1).is_err());
        assert!(atags("", Slot::Root0, 1).is_err());
        assert_eq!(revision("Diamond-1.10"), 0x010a);
        assert_eq!(revision("unknown"), 0);
    }

    #[test]
    fn each_generations_kernel_has_its_own_room_below_the_initramfs() {
        let (j49, diamond) = (kernel(Device::J49).unwrap(), kernel(Device::Diamond).unwrap());
        assert!(j49 + KERNEL_LIMIT as u32 <= diamond && diamond + KERNEL_LIMIT as u32 <= INITRAMFS);
        assert!(kernel(Device::Diamond3).is_err());
    }

    #[test]
    fn the_zimage_comes_out_of_its_uimage() {
        let mut image = vec![0u8; 0x40];
        image[0x24..0x28].copy_from_slice(&[0x18, 0x28, 0x6f, 0x01]);
        let mut uimage = vec![0x27, 0x05, 0x19, 0x56];
        uimage.resize(64, 0);
        uimage[12..16].copy_from_slice(&(image.len() as u32).to_be_bytes());
        uimage[28..32].copy_from_slice(&[5, 2, 2, 0]);
        uimage.extend_from_slice(&image);
        assert_eq!(zimage(&uimage).unwrap(), &image[..]);
        uimage[31] = 1;
        assert!(zimage(&uimage).is_err());
        assert!(zimage(b"not an image at all, not one").is_err());
    }
}
