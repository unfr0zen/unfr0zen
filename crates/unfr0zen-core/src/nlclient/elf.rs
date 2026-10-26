//! Checked ELF32 access shared by discovery and the deliberately small linker.
use crate::{device, Result};
#[derive(Clone, Debug)]
pub(super) struct Section {
    pub name: String,
    pub kind: u32,
    pub flags: u32,
    pub address: u32,
    pub offset: usize,
    pub size: usize,
    pub link: usize,
    pub info: usize,
    pub align: usize,
}
pub(super) struct Elf<'a> {
    pub data: &'a [u8],
    pub sections: Vec<Section>,
}
pub(super) fn bytes(data: &[u8], offset: usize, size: usize) -> Result<&[u8]> {
    data.get(
        offset
            ..offset
                .checked_add(size)
                .ok_or_else(|| crate::Error::Device("ELF range overflow".into()))?,
    )
    .ok_or_else(|| crate::Error::Device("truncated ELF range".into()))
}
pub(super) fn word(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes(data, offset, 4)?.try_into().unwrap(),
    ))
}
pub(super) fn half(data: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes(data, offset, 2)?.try_into().unwrap(),
    ))
}
pub(super) fn string(data: &[u8], offset: usize) -> Result<String> {
    let tail = data
        .get(offset..)
        .ok_or_else(|| crate::Error::Device("ELF string offset out of range".into()))?;
    let n = tail
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| crate::Error::Device("unterminated ELF string".into()))?;
    String::from_utf8(tail[..n].to_vec())
        .map_err(|_| crate::Error::Device("invalid ELF string".into()))
}
pub(super) fn align(value: usize, alignment: usize) -> Result<usize> {
    let alignment = alignment.max(1);
    if !alignment.is_power_of_two() {
        return device("ELF alignment is not a power of two");
    }
    value
        .checked_add(alignment - 1)
        .map(|n| n & !(alignment - 1))
        .ok_or_else(|| crate::Error::Device("ELF alignment overflow".into()))
}
impl<'a> Elf<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if bytes(data, 0, 52)?[..7] != *b"\x7fELF\x01\x01\x01" || half(data, 18)? != 40 {
            return device("nlclient requires little-endian ARM ELF32");
        }
        let offset = word(data, 32)? as usize;
        let stride = half(data, 46)? as usize;
        let count = half(data, 48)? as usize;
        if stride != 40 || count == 0 {
            return device("unsupported ELF section table");
        }
        bytes(data, offset, count * stride)?;
        let names_index = half(data, 50)? as usize;
        if names_index >= count {
            return device("ELF section-name index out of range");
        }
        let names_header = offset + names_index * stride;
        let names = bytes(
            data,
            word(data, names_header + 16)? as usize,
            word(data, names_header + 20)? as usize,
        )?;
        let mut sections = Vec::new();
        for i in 0..count {
            let o = offset + i * stride;
            let s = Section {
                name: string(names, word(data, o)? as usize)?,
                kind: word(data, o + 4)?,
                flags: word(data, o + 8)?,
                address: word(data, o + 12)?,
                offset: word(data, o + 16)? as usize,
                size: word(data, o + 20)? as usize,
                link: word(data, o + 24)? as usize,
                info: word(data, o + 28)? as usize,
                align: word(data, o + 32)? as usize,
            };
            if s.kind != 8 {
                bytes(data, s.offset, s.size)?;
            }
            if s.address
                .checked_add(
                    u32::try_from(s.size)
                        .map_err(|_| crate::Error::Device("ELF section too large".into()))?,
                )
                .is_none()
            {
                return device("ELF section address overflow");
            }
            sections.push(s);
        }
        Ok(Self { data, sections })
    }
    pub fn section(&self, name: &str) -> Result<&Section> {
        self.sections
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| crate::Error::Device(format!("missing ELF section {name}")))
    }
    pub fn contents(&self, section: &Section) -> &[u8] {
        &self.data[section.offset..section.offset + section.size]
    }
    pub fn at(&self, address: u32) -> Option<&Section> {
        self.sections.iter().find(|s| {
            s.kind != 8
                && s.address != 0
                && address >= s.address
                && (address - s.address) < s.size as u32
        })
    }
    pub fn offset(&self, address: u32) -> Result<usize> {
        self.at(address)
            .map(|s| s.offset + (address - s.address) as usize)
            .ok_or_else(|| {
                crate::Error::Device(format!("ELF address {address:#x} outside sections"))
            })
    }
    pub fn read(&self, address: u32) -> Result<u32> {
        let section = self.at(address).ok_or_else(|| {
            crate::Error::Device(format!("ELF word {address:#x} outside sections"))
        })?;
        let relative = (address - section.address) as usize;
        word(self.contents(section), relative)
    }
    pub fn text(&self, address: u32) -> bool {
        self.at(address & !1).is_some_and(|s| s.flags & 4 != 0)
    }
    pub fn ident(&self, address: u32) -> Option<String> {
        let s = self.at(address)?;
        if s.flags & 5 != 0 {
            return None;
        }
        let o = (address - s.address) as usize;
        let tail = self.contents(s).get(o..)?;
        let n = tail.iter().take(48).position(|&b| b == 0)?;
        if n < 2
            || !tail[..n]
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
        {
            return None;
        }
        String::from_utf8(tail[..n].to_vec()).ok()
    }
}
