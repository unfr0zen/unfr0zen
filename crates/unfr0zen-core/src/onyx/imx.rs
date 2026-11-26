//! i.MX6 boot-image structures the onyx flow reads: the HAB4 IVT of a signed
//! SDP loader, and the DCD register program inside it.

use crate::{Error, Result};

/// Where the recovery FIT is placed for the loader's factory USB boot path.
pub const FIT_BASE: u32 = 0x8300_0000;

/// The HAB4 IVT header of a signed SDP loader: everything the SDP session
/// needs — where the image lands, where it enters, and where its DCD program
/// sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Iv {
    pub entry: u32,
    /// DCD block address; zero when the image carries none.
    pub dcd: u32,
    /// The address the image's boot data says it loads at.
    pub load: u32,
    pub load_size: u32,
    /// The IVT's own address: the JUMP target, where HAB re-authenticates.
    pub self_addr: u32,
}

pub const IVT_MAGIC: [u8; 4] = [0xd1, 0x00, 0x20, 0x40];

/// Parses the IVT at the start of a signed loader image. The image carries
/// its IVT and boot-data header at `boot_data - self_addr`, and the SDP
/// session writes the whole file at `self_addr`.
pub fn parse_ivt(image: &[u8]) -> Result<Iv> {
    if image.len() < 32 || image[..4] != IVT_MAGIC {
        return Err(Error::Device("not an i.MX HAB4 image (no IVT)".into()));
    }
    let words: [u32; 8] = std::array::from_fn(|i| u32::from_le_bytes(image[i * 4..i * 4 + 4].try_into().unwrap()));
    let self_addr = words[5];
    let boot_ptr = words[4];
    if boot_ptr < self_addr {
        return Err(Error::Device("IVT boot data below the image start".into()));
    }
    let header = (boot_ptr - self_addr) as usize;
    if header + 16 > image.len() {
        return Err(Error::Device("image shorter than its header".into()));
    }
    let word = |off: usize| u32::from_le_bytes(image[off..off + 4].try_into().unwrap());
    Ok(Iv { entry: words[1], dcd: words[3], load: word(header), load_size: word(header + 4), self_addr })
}

/// One DCD write command: set `address` to `value`, or, for a masked write,
/// clear the bits in `mask` and set `value`'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DcdWrite {
    pub address: u32,
    pub value: u32,
    pub mask: Option<u32>,
}

/// A 32-bit operation in the signed loader's DCD register program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DcdCommand {
    Write(DcdWrite),
    /// Poll until every masked bit is clear, or set when `set` is true.
    Check {
        address: u32,
        mask: u32,
        set: bool,
    },
}

/// Decodes every address/value pair of a HAB4 DCD. Write Data (0xCC)
/// commands can batch many pairs; Check Data (0xCF) polls a register and
/// must remain ordered between writes. Onyx's program uses 32-bit accesses.
pub fn parse_dcd(block: &[u8]) -> Result<Vec<DcdCommand>> {
    if block.len() < 4 || block[0] != 0xd2 || block[3] != 0x40 {
        return Err(Error::Device("not a HAB4 DCD block".into()));
    }
    let len = u16::from_be_bytes([block[1], block[2]]) as usize;
    if len < 4 || len > block.len() {
        return Err(Error::Device("invalid DCD length".into()));
    }
    let mut commands = Vec::new();
    let mut pos = 4;
    while pos < len {
        if len - pos < 4 {
            return Err(Error::Device("truncated DCD command header".into()));
        }
        let tag = block[pos];
        let size = u16::from_be_bytes([block[pos + 1], block[pos + 2]]) as usize;
        let flags = block[pos + 3];
        if size < 12 || size > len - pos {
            return Err(Error::Device("invalid DCD command length".into()));
        }
        let word = |at: usize| u32::from_be_bytes(block[at..at + 4].try_into().unwrap());
        match (tag, flags) {
            (0xcc, 0x04 | 0x0c) if (size - 4).is_multiple_of(8) => {
                for at in (pos + 4..pos + size).step_by(8) {
                    let (address, value) = (word(at), word(at + 4));
                    let (value, mask) = if flags == 0x0c { (0, Some(value)) } else { (value, None) };
                    commands.push(DcdCommand::Write(DcdWrite { address, value, mask }));
                }
            }
            (0xcf, 0x04 | 0x14) if size == 12 => {
                commands.push(DcdCommand::Check { address: word(pos + 4), mask: word(pos + 8), set: flags == 0x14 })
            }
            _ => return Err(Error::Device(format!("unsupported DCD command {tag:#x}/{flags:#x}, length {size}"))),
        }
        pos += size;
    }
    Ok(commands)
}

/// The complete DCD program at the address the loader's IVT names.
pub fn dcd_commands(image: &[u8], ivt: &Iv) -> Result<Vec<DcdCommand>> {
    if ivt.dcd == 0 {
        return Ok(Vec::new());
    }
    let off =
        ivt.dcd.checked_sub(ivt.self_addr).ok_or_else(|| Error::Device("DCD below the image start".into()))? as usize;
    let block = image.get(off..).ok_or_else(|| Error::Device("DCD outside the image".into()))?;
    parse_dcd(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put32(v: &mut Vec<u8>, value: u32) {
        v.extend_from_slice(&value.to_le_bytes());
    }

    fn be(v: &mut Vec<u8>, value: u32) {
        v.extend_from_slice(&value.to_be_bytes());
    }

    /// A minimal loader image: IVT, boot data, a DCD with one write and one
    /// register check, and padding.
    fn sample_image() -> Vec<u8> {
        let base: u32 = 0x8000_0000;
        let ivt_at: u32 = base;
        let dcd_at: u32 = base + 0x40;
        let mut image = Vec::new();
        put32(&mut image, u32::from_le_bytes(IVT_MAGIC));
        put32(&mut image, base + 0x100); // entry
        put32(&mut image, 0); // reserved
        put32(&mut image, dcd_at); // dcd
        put32(&mut image, base + 0x20); // boot data
        put32(&mut image, ivt_at); // self
        put32(&mut image, 0); // csf
        put32(&mut image, 0); // reserved
                              // boot data struct at self+0x20
        put32(&mut image, base);
        put32(&mut image, 0x1000);
        put32(&mut image, 0);
        put32(&mut image, 0);
        // Two writes in one command, followed by a clock-switch completion check.
        let mut dcd = Vec::new();
        dcd.push(0xd2);
        dcd.extend_from_slice(&36u16.to_be_bytes()); // 4 + 20 + 12
        dcd.push(0x40);
        dcd.extend_from_slice(&[0xcc, 0x00, 0x14, 0x04]);
        be(&mut dcd, 0x021b_0000);
        be(&mut dcd, 0x1234_5678);
        be(&mut dcd, 0x021b_0004);
        be(&mut dcd, 0x0002_5552);
        dcd.extend_from_slice(&[0xcf, 0x00, 0x0c, 0x04]);
        be(&mut dcd, 0x020c_4048);
        be(&mut dcd, 0xffff_ffff);
        while image.len() < 0x40 {
            image.push(0);
        }
        image.extend_from_slice(&dcd);
        image
    }

    #[test]
    fn ivt_fields_come_from_the_header_and_boot_data() {
        let image = sample_image();
        let ivt = parse_ivt(&image).unwrap();
        assert_eq!(ivt.entry, 0x8000_0100);
        assert_eq!(ivt.self_addr, 0x8000_0000);
        assert_eq!(ivt.dcd, 0x8000_0040);
        assert_eq!(ivt.load, 0x8000_0000);
        assert_eq!(ivt.load_size, 0x1000);
    }

    #[test]
    fn batched_writes_and_checks_preserve_the_complete_program() {
        let image = sample_image();
        let ivt = parse_ivt(&image).unwrap();
        assert_eq!(
            dcd_commands(&image, &ivt).unwrap(),
            vec![
                DcdCommand::Write(DcdWrite { address: 0x021b_0000, value: 0x1234_5678, mask: None }),
                DcdCommand::Write(DcdWrite { address: 0x021b_0004, value: 0x0002_5552, mask: None }),
                DcdCommand::Check { address: 0x020c_4048, mask: 0xffff_ffff, set: false },
            ]
        );
    }

    #[test]
    fn an_image_without_an_ivt_is_rejected() {
        assert!(parse_ivt(&[0u8; 32]).is_err());
        assert!(parse_ivt(&[0u8; 8]).is_err());
    }
}
