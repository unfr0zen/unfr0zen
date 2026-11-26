//! unfr0zen's update package, unfr0zen-<device>-<tag>.ota, one per device
//! for every way an update comes: a Matter OTA image whose payload is a
//! Zigbee OTA image, whose sub-elements are the package's own URL and the
//! device's signed setup bundle. MQTT's updates (update.rs) download it from
//! the release and Matter's (matter/ota.rs) get it from a controller's OTA
//! provider, whole; Zigbee's (zigbee/ota.rs) get the Zigbee image from the
//! coordinator's OTA server, which sends from the Zigbee header on
//! (Zigbee2MQTT does), or the package from its URL over Wi-Fi. Either way it
//! is unwrapped as it arrives, its headers checked against the unit and the
//! Matter payload against the header's digest, and the bundle goes to the
//! update engine, which checks it against its manifest and the release key.
//!
//! Each generation is a Matter product of its own under the test vendor
//! (PRODUCTS), and the same number is its Zigbee image type under Nest
//! Labs' manufacturer code, so a provider or a server offers each unit its
//! own device's package. utils/bundle/package.py writes them, with the same
//! table.

use std::io::Write;

use rs_matter::tlv::{FromTLV, OctetStr, TLVElement, ToTLV, UtfStr};
use sha2::{Digest, Sha256};

/// Matter's test vendor, which every unit reports.
pub const VENDOR: u16 = 0xFFF1;

/// Each device's Matter product ID and Zigbee image type, by its name.
const PRODUCTS: [(&str, u16); 4] = [("diamond3", 0x8001), ("j49", 0x8002), ("diamond", 0x8003), ("onyx", 0x8004)];

/// The Matter product ID and Zigbee image type of the device named `device`.
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

/// Nest Labs, the Zigbee image's manufacturer (zigbee::zcl's code).
pub const ZIGBEE_MANUFACTURER: u16 = 0x10D6;
/// A Zigbee OTA image's file identifier, and its header's fields up to and
/// including the total image size.
const ZIGBEE_FILE_ID: u32 = 0x0BEE_F11E;
const ZIGBEE_HEADER: usize = 56;
/// The Zigbee image's sub-elements: the bundle, and the package's URL
/// (manufacturer-specific, 0xF000 up; Zigbee2MQTT reads 0xF000 as Telink's).
const UPGRADE_IMAGE: u16 = 0x0000;
const PACKAGE_URL: u16 = 0xF001;
/// The most of a Zigbee image's start that holds its header and URL.
pub const ZIGBEE_HEAD: usize = 1024;

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

/// The fields of a Zigbee OTA header unfr0zen reads.
#[derive(Debug, PartialEq)]
pub struct ZigbeeHeader {
    pub manufacturer: u16,
    pub image_type: u16,
    pub version: u32,
    pub name: String,
    pub length: usize,
    pub total: u32,
}

/// The Zigbee header at the start of `head`, if it is one and all there.
pub fn zigbee_header(head: &[u8]) -> Option<ZigbeeHeader> {
    let head = head.get(..ZIGBEE_HEADER)?;
    let u16_at = |i: usize| u16::from_le_bytes([head[i], head[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]);
    (u32_at(0) == ZIGBEE_FILE_ID).then(|| ZigbeeHeader {
        length: u16_at(6).into(),
        manufacturer: u16_at(10),
        image_type: u16_at(12),
        version: u32_at(14),
        name: String::from_utf8_lossy(&head[20..52]).trim_end_matches('\0').trim().to_string(),
        total: u32_at(52),
    })
}

/// The package's URL in a Zigbee image's start: None while more of it is
/// needed, then the URL if its first sub-element is one.
pub fn package_url(head: &[u8]) -> Option<Option<String>> {
    if head.len() < ZIGBEE_HEADER {
        return None;
    }
    let Some(header) = zigbee_header(head) else {
        return Some(None);
    };
    // The URL is all in the start the unit keeps, or there is none.
    let start = header.length + 6;
    if start > ZIGBEE_HEAD {
        return Some(None);
    }
    let element = head.get(header.length..start)?;
    let end = start as u64 + u64::from(u32::from_le_bytes([element[2], element[3], element[4], element[5]]));
    if u16::from_le_bytes([element[0], element[1]]) != PACKAGE_URL || end > ZIGBEE_HEAD as u64 {
        return Some(None);
    }
    let url = head.get(start..end as usize)?;
    Some(String::from_utf8(url.to_vec()).ok().filter(|u| u.starts_with("http://") || u.starts_with("https://")))
}

/// A Zigbee OTA image as it arrives: its bundle written to `out`.
pub struct ZigbeeImage<W> {
    out: W,
    /// A header or sub-element header still being gathered.
    gathered: Vec<u8>,
    part: Part,
    /// How far into the image, and its size once the header has said.
    at: u64,
    total: u64,
    bundles: u32,
}

enum Part {
    Header,
    Element,
    Data { tag: u16, left: u64 },
}

impl<W: Write> ZigbeeImage<W> {
    pub fn new(out: W) -> Self {
        ZigbeeImage { out, gathered: Vec::new(), part: Part::Header, at: 0, total: 0, bundles: 0 }
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.out
    }

    pub fn feed(&mut self, mut data: &[u8]) -> Result<(), String> {
        while !data.is_empty() {
            match self.part {
                Part::Header => {
                    let wanted = match zigbee_header(&self.gathered) {
                        Some(header) => header.length.max(ZIGBEE_HEADER),
                        None if self.gathered.len() < ZIGBEE_HEADER => ZIGBEE_HEADER,
                        None => return Err("not a Zigbee OTA image".into()),
                    };
                    let taken = self.gather(&mut data, wanted);
                    self.at += taken as u64;
                    if self.gathered.len() < wanted {
                        continue;
                    }
                    let Some(header) = zigbee_header(&self.gathered) else {
                        return Err("not a Zigbee OTA image".into());
                    };
                    if self.gathered.len() < header.length {
                        continue; // a longer header than the fixed fields
                    }
                    self.total = header.total.into();
                    if self.total < self.at {
                        return Err(format!("a {}-byte Zigbee image", self.total));
                    }
                    self.gathered.clear();
                    self.part = Part::Element;
                }
                Part::Element => {
                    if self.gathered.is_empty() && self.at >= self.total {
                        return Err("the Zigbee image runs past its end".into());
                    }
                    let taken = self.gather(&mut data, 6);
                    self.at += taken as u64;
                    if self.gathered.len() < 6 {
                        continue;
                    }
                    let g = &self.gathered;
                    let tag = u16::from_le_bytes([g[0], g[1]]);
                    let left = u64::from(u32::from_le_bytes([g[2], g[3], g[4], g[5]]));
                    if self.at + left > self.total {
                        return Err("a sub-element past the Zigbee image's end".into());
                    }
                    self.gathered.clear();
                    self.part = Part::Data { tag, left };
                    self.ended_data(tag, left);
                }
                Part::Data { tag, left } => {
                    let n = (left.min(data.len() as u64)) as usize;
                    if tag == UPGRADE_IMAGE {
                        self.out.write_all(&data[..n]).map_err(|e| format!("the bundle: {e}"))?;
                    }
                    data = &data[n..];
                    self.at += n as u64;
                    self.part = Part::Data { tag, left: left - n as u64 };
                    self.ended_data(tag, left - n as u64);
                }
            }
        }
        Ok(())
    }

    /// Takes up to `wanted` gathered bytes from `data`: how many it took.
    fn gather(&mut self, data: &mut &[u8], wanted: usize) -> usize {
        let taken = wanted.saturating_sub(self.gathered.len()).min(data.len());
        self.gathered.extend_from_slice(&data[..taken]);
        *data = &data[taken..];
        taken
    }

    fn ended_data(&mut self, tag: u16, left: u64) {
        if left == 0 {
            self.bundles += u32::from(tag == UPGRADE_IMAGE);
            self.part = Part::Element;
        }
    }

    /// Checks the whole image arrived, with one bundle: what it wrote to.
    pub fn finish(mut self) -> Result<W, String> {
        match self.part {
            Part::Element if self.gathered.is_empty() && self.at == self.total => {}
            _ => return Err(format!("{} of the Zigbee image's {} bytes", self.at, self.total)),
        }
        if self.bundles != 1 {
            return Err(format!("{} bundles in the Zigbee image", self.bundles));
        }
        self.out.flush().map_err(|e| format!("the bundle: {e}"))?;
        Ok(self.out)
    }
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
    image: ZigbeeImage<W>,
}

enum State {
    Head,
    Payload { size: u64, received: u64, digest: Vec<u8>, sha: Sha256 },
    Bare,
}

impl<W: Write> Unwrap<W> {
    pub fn new(expected: Expected, out: W) -> Self {
        Unwrap { expected, head: Vec::new(), state: State::Head, image: ZigbeeImage::new(out) }
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
            State::Bare => self.image.get_mut().write_all(data).map_err(|e| format!("the bundle: {e}")),
            State::Payload { size, received, sha, .. } => {
                *received += data.len() as u64;
                if *received > *size {
                    return Err("the package runs past its payload".into());
                }
                sha.update(data);
                self.image.feed(data)
            }
        }
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
            self.image.get_mut().write_all(&head).map_err(|e| format!("the bundle: {e}"))?;
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
        if header.payload_size > (crate::update::BUNDLE_LIMIT + ZIGBEE_HEAD) as u64 {
            return Err(format!("a {}-byte payload", header.payload_size));
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
            State::Head => Err("the package ended in its header".into()),
            State::Bare => self.image.get_mut().flush().map_err(|e| format!("the bundle: {e}")),
            State::Payload { size, received, digest, sha } => {
                if received != size {
                    return Err(format!("{received} of the package's {size} bytes"));
                }
                if sha.finalize().as_slice() != digest {
                    return Err("the package does not match its digest".into());
                }
                self.image.finish().map(drop)
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use rs_matter::tlv::TLVTag;
    use rs_matter::utils::storage::WriteBuf;

    /// A Zigbee image of `bundle` for this product and version, as
    /// utils/bundle/package.py writes it, its URL first when it has one.
    pub fn zigbee_image(product: u16, version: u32, url: Option<&str>, bundle: &[u8]) -> Vec<u8> {
        let mut elements = Vec::new();
        if let Some(url) = url {
            elements.extend(PACKAGE_URL.to_le_bytes());
            elements.extend((url.len() as u32).to_le_bytes());
            elements.extend(url.as_bytes());
        }
        elements.extend(UPGRADE_IMAGE.to_le_bytes());
        elements.extend((bundle.len() as u32).to_le_bytes());
        elements.extend(bundle);
        let mut out = ZIGBEE_FILE_ID.to_le_bytes().to_vec();
        out.extend(0x0100u16.to_le_bytes());
        out.extend((ZIGBEE_HEADER as u16).to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(ZIGBEE_MANUFACTURER.to_le_bytes());
        out.extend(product.to_le_bytes());
        out.extend(version.to_le_bytes());
        out.extend(2u16.to_le_bytes());
        let mut name = [0u8; 32];
        name[..15].copy_from_slice(b"unfr0zen v0.6.0");
        out.extend(name);
        out.extend(((ZIGBEE_HEADER + elements.len()) as u32).to_le_bytes());
        out.extend(elements);
        out
    }

    /// A package of `bundle` for this product and version.
    pub fn package(product: u16, version: u32, bundle: &[u8]) -> Vec<u8> {
        let payload = zigbee_image(product, version, Some("https://x/unfr0zen-diamond3-v0.6.0.ota"), bundle);
        let digest = Sha256::digest(&payload);
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
        out.extend_from_slice(&payload);
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

    /// The Zigbee image fed in pieces of `chunk` bytes: the bundle, or why not.
    fn read_zigbee(image: &[u8], chunk: usize) -> Result<Vec<u8>, String> {
        let mut reading = ZigbeeImage::new(Vec::new());
        for piece in image.chunks(chunk) {
            reading.feed(piece)?;
        }
        reading.finish()
    }

    fn gen3(version: Option<u32>) -> Expected {
        Expected { product: 0x8001, version, bare: false }
    }

    fn a_bundle() -> Vec<u8> {
        [&GZIP[..], &(0..3000u32).map(|i| i as u8).collect::<Vec<_>>()].concat()
    }

    #[test]
    fn each_generation_is_a_product_of_its_own() {
        assert_eq!([product("diamond3"), product("j49"), product("diamond")], [0x8001, 0x8002, 0x8003]);
    }

    #[test]
    fn a_package_gives_its_bundle_in_any_pieces() {
        let bundle = a_bundle();
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
    fn a_zigbee_image_gives_its_bundle_in_any_pieces_and_nothing_else() {
        let bundle = a_bundle();
        let image = zigbee_image(0x8001, 0x0006_0080, Some("https://x/p.ota"), &bundle);
        for chunk in [1, 5, 6, 56, 65, 1000, image.len()] {
            assert_eq!(read_zigbee(&image, chunk).unwrap(), bundle, "in {chunk}-byte pieces");
        }
        let bare = zigbee_image(0x8001, 0x0006_0080, None, &bundle);
        assert_eq!(read_zigbee(&bare, 65).unwrap(), bundle, "with no URL");
        assert!(read_zigbee(&image[..image.len() - 1], 65).unwrap_err().contains("bytes"), "cut short");
        assert!(read_zigbee(&[image.clone(), vec![0]].concat(), 65).unwrap_err().contains("past its end"));
        assert!(read_zigbee(&bundle, 65).unwrap_err().contains("not a Zigbee"));
        // No bundle: just the URL.
        let mut url_only = zigbee_image(0x8001, 0x0006_0080, Some("https://x/p.ota"), b"");
        url_only.truncate(url_only.len() - 6);
        let total = url_only.len() as u32;
        url_only[52..56].copy_from_slice(&total.to_le_bytes());
        assert!(read_zigbee(&url_only, 65).unwrap_err().contains("0 bundles"));
    }

    #[test]
    fn the_package_url_is_read_once_its_bytes_are_there() {
        let url = "https://github.com/unfr0zen/unfr0zen/releases/download/v0.6.0/unfr0zen-diamond3-v0.6.0.ota";
        let with = zigbee_image(0x8001, 0x0006_0080, Some(url), b"bundle");
        let end = ZIGBEE_HEADER + 6 + url.len();
        for cut in [0, ZIGBEE_HEADER - 1, ZIGBEE_HEADER, ZIGBEE_HEADER + 5, end - 1] {
            assert_eq!(package_url(&with[..cut]), None, "{cut} bytes");
        }
        assert_eq!(package_url(&with[..end]), Some(Some(url.to_string())));
        assert_eq!(package_url(&zigbee_image(0x8001, 1, None, b"bundle")), Some(None), "no URL element");
        assert_eq!(package_url(&zigbee_image(0x8001, 1, Some("ftp://x/p.ota"), b"b")), Some(None), "not http");
        assert_eq!(package_url(&[0; 64]), Some(None), "not an image");
        // A length that would run past what the unit keeps, or overflow.
        let mut huge = with.clone();
        huge[ZIGBEE_HEADER + 2..ZIGBEE_HEADER + 6].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(package_url(&huge[..end]), Some(None), "a URL past the head");
        // A header too long to leave room for the URL in the head.
        let mut long = with.clone();
        long[6..8].copy_from_slice(&(ZIGBEE_HEAD as u16).to_le_bytes());
        assert_eq!(package_url(&long[..ZIGBEE_HEADER]), Some(None), "a header filling the head");
        let header = zigbee_header(&with).unwrap();
        assert_eq!((header.manufacturer, header.image_type, header.version), (0x10D6, 0x8001, 0x0006_0080));
        assert_eq!((header.name.as_str(), header.length, header.total), ("unfr0zen v0.6.0", 56, with.len() as u32));
    }

    #[test]
    fn the_release_tool_writes_packages_each_device_and_each_way_takes() {
        let dir = std::env::temp_dir().join(format!("nestlocal-package-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = concat!(env!("CARGO_MANIFEST_DIR"), "/../../utils/bundle/package.py");
        let bundles: Vec<_> = ["diamond3", "j49", "diamond"]
            .iter()
            .map(|device| {
                let path = dir.join(format!("unfr0zen-ota-{device}-v0.6.0-rc2.tgz"));
                std::fs::write(&path, [&GZIP[..], device.as_bytes()].concat()).unwrap();
                path
            })
            .collect();
        let status = std::process::Command::new("python3").arg(tool).args(&bundles).output().unwrap();
        assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));
        let index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("unfr0zen-zigbee-ota.json")).unwrap()).unwrap();
        let version = crate::update::matter_version("v0.6.0-rc2");
        for (device, entry) in ["diamond3", "j49", "diamond"].iter().zip(index.as_array().unwrap()) {
            let name = format!("unfr0zen-{device}-v0.6.0-rc2.ota");
            let package = std::fs::read(dir.join(&name)).unwrap();
            let bundle = [&GZIP[..], device.as_bytes()].concat();
            // Whole, as MQTT and Matter take it.
            let expected = Expected { product: product(device), version: Some(version), bare: false };
            assert_eq!(read(&package, expected, 1174).unwrap(), bundle, "{device}");
            // From the Zigbee header on, as a Zigbee OTA server sends it.
            let at = package.windows(4).position(|w| w == ZIGBEE_FILE_ID.to_le_bytes()).unwrap();
            let image = &package[at..];
            let header = zigbee_header(image).unwrap();
            assert_eq!((header.manufacturer, header.image_type, header.version), (0x10D6, product(device), version));
            assert_eq!(header.total as usize, image.len());
            assert_eq!(read_zigbee(image, 65).unwrap(), bundle, "{device} over Zigbee");
            let url = format!("https://github.com/unfr0zen/unfr0zen/releases/download/v0.6.0-rc2/{name}");
            assert_eq!(package_url(&image[..ZIGBEE_HEAD.min(image.len())]), Some(Some(url.clone())));
            // Zigbee2MQTT's index entry for it.
            assert_eq!(entry["url"], url);
            assert_eq!(
                (entry["imageType"].as_u64(), entry["manufacturerCode"].as_u64()),
                (Some(product(device).into()), Some(0x10D6))
            );
            assert_eq!(
                (entry["fileVersion"].as_u64(), entry["fileSize"].as_u64()),
                (Some(version.into()), Some(package.len() as u64))
            );
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
