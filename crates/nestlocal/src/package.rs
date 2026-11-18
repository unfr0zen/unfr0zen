//! unfr0zen's update package, unfr0zen-ota-<device>-<tag>.ota: a Matter OTA
//! image whose payload is one device's signed setup bundle. MQTT's updates
//! (update.rs) download it from the release, Matter's (matter/ota.rs) get it
//! from a controller's OTA provider; either way it is unwrapped as it
//! arrives, its header checked against the unit and its payload against
//! the header's digest, and the bundle goes to the update engine.
//!
//! Each generation is a Matter product of its own under the test vendor
//! (PRODUCTS), so a provider offers each unit its own device's image.
//! utils/bundle/matter-ota.py writes the images, with the same table.

use std::io::Write;

use rs_matter::tlv::{FromTLV, OctetStr, TLVElement, ToTLV, UtfStr};
use sha2::{Digest, Sha256};

/// Matter's test vendor, which every unit reports.
pub const VENDOR: u16 = 0xFFF1;

/// Each device's Matter product ID, by its name in bundle names.
const PRODUCTS: [(&str, u16); 3] = [("diamond3", 0x8001), ("j49", 0x8002), ("diamond", 0x8003)];

/// The Matter product ID of the device named `device`.
pub fn product(device: &str) -> u16 {
    PRODUCTS.iter().find(|(name, _)| *name == device).map_or(0x8001, |&(_, id)| id)
}

/// A Matter OTA image's file identifier.
const FILE_ID: u32 = 0x1BEE_F11E;
/// The fixed fields before the header: identifier, total size, header size.
const PREFIX: usize = 4 + 8 + 4;
/// ImageDigestType SHA-256 (IANA Named Information Hash Algorithm 1).
const SHA256: u8 = 1;
const GZIP: [u8; 2] = [0x1F, 0x8B];

/// A Matter OTA image's header, as the TLV it is.
#[derive(Debug, FromTLV, ToTLV)]
#[tlvargs(lifetime = "'a")]
struct Header<'a> {
    vendor_id: u16,
    product_id: u16,
    software_version: u32,
    software_version_string: UtfStr<'a>,
    payload_size: u64,
    min_applicable_version: Option<u32>,
    max_applicable_version: Option<u32>,
    release_notes_url: Option<UtfStr<'a>>,
    image_digest_type: u8,
    image_digest: OctetStr<'a>,
}

/// What a package must be for this unit.
pub struct Expected {
    pub product: u16,
    /// The version offered, when one was.
    pub version: Option<u32>,
    /// A bare bundle (a .tgz at a URL given by hand) passes through as it is.
    pub bare: bool,
}

/// A package as it arrives: its bundle written to `out`.
pub struct Unwrap<W> {
    expected: Expected,
    head: Vec<u8>,
    state: State,
    out: W,
}

enum State {
    Head,
    Payload { size: u64, received: u64, digest: Vec<u8>, sha: Sha256 },
    Bare,
}

impl<W: Write> Unwrap<W> {
    pub fn new(expected: Expected, out: W) -> Self {
        Unwrap { expected, head: Vec::new(), state: State::Head, out }
    }

    pub fn feed(&mut self, mut data: &[u8]) -> Result<(), String> {
        if let State::Head = self.state {
            let taken = self.read_header(data)?;
            data = &data[taken..];
        }
        if data.is_empty() {
            return Ok(());
        }
        match &mut self.state {
            State::Head => unreachable!("a header still short takes all the data"),
            State::Bare => {}
            State::Payload { size, received, sha, .. } => {
                *received += data.len() as u64;
                if *received > *size {
                    return Err("the package runs past its payload".into());
                }
                sha.update(data);
            }
        }
        self.write(data)
    }

    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        self.out.write_all(data).map_err(|e| format!("the bundle: {e}"))
    }

    /// Takes the header's bytes from the start of `data`: how many it took.
    fn read_header(&mut self, data: &[u8]) -> Result<usize, String> {
        let before = self.head.len();
        let wanted = if before < PREFIX {
            PREFIX
        } else {
            let size = u32::from_le_bytes(self.head[12..16].try_into().unwrap()) as usize;
            if size > 4096 {
                return Err(format!("a {size}-byte package header"));
            }
            PREFIX + size
        };
        let taken = (wanted - before).min(data.len());
        self.head.extend_from_slice(&data[..taken]);
        if before < 4 && self.head.len() >= 4 && u32::from_le_bytes(self.head[..4].try_into().unwrap()) != FILE_ID {
            if !(self.expected.bare && self.head[..2] == GZIP) {
                return Err("not an unfr0zen package (a Matter OTA image)".into());
            }
            self.state = State::Bare;
            let head = std::mem::take(&mut self.head);
            self.write(&head)?;
            return Ok(taken);
        }
        if self.head.len() < wanted {
            return Ok(taken);
        }
        if before < PREFIX {
            // The header's own size is known now.
            return Ok(taken + self.read_header(&data[taken..])?);
        }
        let header = Header::from_tlv(&TLVElement::new(&self.head[PREFIX..]))
            .map_err(|e| format!("the package header: {e:?}"))?;
        let total = u64::from_le_bytes(self.head[4..12].try_into().unwrap());
        if (header.vendor_id, header.product_id) != (VENDOR, self.expected.product) {
            return Err(format!(
                "a package for {:04X}:{:04X}, not this unit's {VENDOR:04X}:{:04X}",
                header.vendor_id, header.product_id, self.expected.product
            ));
        }
        if let Some(version) = self.expected.version.filter(|&v| v != header.software_version) {
            return Err(format!("package version {:#x}, not the {version:#x} offered", header.software_version));
        }
        if header.image_digest_type != SHA256 || header.image_digest.0.len() != 32 {
            return Err(format!("digest type {}, not SHA-256", header.image_digest_type));
        }
        if header.payload_size > crate::update::BUNDLE_LIMIT as u64 {
            return Err(format!("a {}-byte bundle", header.payload_size));
        }
        if total != self.head.len() as u64 + header.payload_size {
            return Err(format!("a {total}-byte package with a {}-byte payload", header.payload_size));
        }
        self.state = State::Payload {
            size: header.payload_size,
            received: 0,
            digest: header.image_digest.0.to_vec(),
            sha: Sha256::new(),
        };
        Ok(taken)
    }

    /// How much of the payload has arrived, in percent.
    pub fn percent(&self) -> u8 {
        match &self.state {
            State::Payload { size, received, .. } => (received * 100 / (*size).max(1)) as u8,
            _ => 0,
        }
    }

    /// Checks the whole package arrived intact.
    pub fn finish(mut self) -> Result<(), String> {
        match std::mem::replace(&mut self.state, State::Bare) {
            State::Head => return Err("the package ended in its header".into()),
            State::Bare => {}
            State::Payload { size, received, digest, sha } => {
                if received != size {
                    return Err(format!("{received} of the package's {size} bytes"));
                }
                if sha.finalize().as_slice() != digest {
                    return Err("the package does not match its digest".into());
                }
            }
        }
        self.out.flush().map_err(|e| format!("the bundle: {e}"))
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use rs_matter::tlv::TLVTag;
    use rs_matter::utils::storage::WriteBuf;

    /// A package of `payload` for this product and version.
    pub fn package(product: u16, version: u32, payload: &[u8]) -> Vec<u8> {
        let digest = Sha256::digest(payload);
        let header = Header {
            vendor_id: VENDOR,
            product_id: product,
            software_version: version,
            software_version_string: "v0.6.0",
            payload_size: payload.len() as u64,
            min_applicable_version: None,
            max_applicable_version: None,
            release_notes_url: None,
            image_digest_type: SHA256,
            image_digest: OctetStr::new(&digest),
        };
        let mut buf = [0u8; 512];
        let mut wb = WriteBuf::new(&mut buf);
        header.to_tlv(&TLVTag::Anonymous, &mut wb).unwrap();
        let tlv = wb.as_slice().to_vec();
        let mut out = FILE_ID.to_le_bytes().to_vec();
        out.extend_from_slice(&((PREFIX + tlv.len() + payload.len()) as u64).to_le_bytes());
        out.extend_from_slice(&(tlv.len() as u32).to_le_bytes());
        out.extend_from_slice(&tlv);
        out.extend_from_slice(payload);
        out
    }

    /// The package fed in pieces of `chunk` bytes: the bundle, or why not.
    fn read(package: &[u8], expected: Expected, chunk: usize) -> Result<Vec<u8>, String> {
        let mut bundle = Vec::new();
        let mut reading = Unwrap::new(expected, &mut bundle);
        for piece in package.chunks(chunk) {
            reading.feed(piece)?;
        }
        reading.finish()?;
        Ok(bundle)
    }

    fn gen3(version: Option<u32>) -> Expected {
        Expected { product: 0x8001, version, bare: false }
    }

    #[test]
    fn each_generation_is_a_product_of_its_own() {
        assert_eq!([product("diamond3"), product("j49"), product("diamond")], [0x8001, 0x8002, 0x8003]);
    }

    #[test]
    fn a_package_gives_its_bundle_in_any_pieces() {
        let bundle: Vec<u8> = [&GZIP[..], &(0..3000u32).map(|i| i as u8).collect::<Vec<_>>()].concat();
        let package = package(0x8001, 0x0006_0080, &bundle);
        for chunk in [1, 3, 7, 512, 1174, package.len()] {
            assert_eq!(read(&package, gen3(Some(0x0006_0080)), chunk).unwrap(), bundle, "in {chunk}-byte pieces");
            assert_eq!(read(&package, gen3(None), chunk).unwrap(), bundle, "with no version offered");
        }
    }

    #[test]
    fn a_package_not_for_the_unit_or_the_offer_or_damaged_is_refused() {
        let bundle = [&GZIP[..], b"bundle"].concat();
        let gen2 = package(0x8002, 0x0006_0080, &bundle);
        assert!(read(&gen2, gen3(None), 64).unwrap_err().contains("FFF1:8002, not this unit's FFF1:8001"));
        let package = package(0x8001, 0x0006_0080, &bundle);
        assert!(read(&package, gen3(Some(0x0006_0180)), 64).unwrap_err().contains("not the"));
        let mut damaged = package.clone();
        let last = damaged.len() - 1;
        damaged[last] ^= 1;
        assert!(read(&damaged, gen3(None), 64).unwrap_err().contains("digest"));
        assert!(read(&package[..package.len() - 1], gen3(None), 64).is_err(), "cut short");
        assert!(read(&package[..10], gen3(None), 64).unwrap_err().contains("in its header"));
    }

    #[test]
    fn the_release_tool_writes_packages_each_device_takes() {
        let dir = std::env::temp_dir().join(format!("nestlocal-package-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = concat!(env!("CARGO_MANIFEST_DIR"), "/../../utils/bundle/matter-ota.py");
        for device in ["diamond3", "j49", "diamond"] {
            let bundle = [&GZIP[..], device.as_bytes()].concat();
            let path = dir.join(format!("unfr0zen-ota-{device}-v0.6.0-rc2.tgz"));
            std::fs::write(&path, &bundle).unwrap();
            let status = std::process::Command::new("python3").arg(tool).arg(&path).output().unwrap();
            assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
            let package = std::fs::read(path.with_extension("ota")).unwrap();
            let version = crate::update::matter_version("v0.6.0-rc2");
            let expected = Expected { product: product(device), version: Some(version), bare: false };
            assert_eq!(read(&package, expected, 1174).unwrap(), bundle, "{device}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_bare_bundle_passes_only_where_allowed() {
        let bundle = [&GZIP[..], b"a bundle from the LAN"].concat();
        let bare = Expected { product: 0x8001, version: None, bare: true };
        assert_eq!(read(&bundle, bare, 1).unwrap(), bundle);
        assert!(read(&bundle, gen3(None), 64).unwrap_err().contains("not an unfr0zen package"));
        let bare = Expected { product: 0x8001, version: None, bare: true };
        assert!(read(b"<html>not found</html>", bare, 64).is_err());
    }
}
