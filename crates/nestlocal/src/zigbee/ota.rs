//! Updates over Zigbee: the OTA Upgrade cluster's client on the thermostat's
//! endpoint, for the coordinator's OTA server (Zigbee2MQTT's). The unit asks
//! the coordinator for an image a few minutes after it starts, then daily,
//! and whenever the coordinator notifies it of one. An image newer than the
//! unfr0zen it runs (update::matter_version numbers both) is downloaded,
//! given to the update engine (update.rs) once the coordinator says to
//! apply it, and the unit restarts into it.
//!
//! The image is the Zigbee OTA image inside the release's package for
//! the device (package.rs), which the coordinator sends from its Zigbee
//! header on. Its first sub-element, in the first blocks, is the package's
//! URL, and the unit fetches the package from it over Wi-Fi, asking the
//! coordinator for a block every 10 s meanwhile so its session lasts; with
//! the package there it ends the transfer. Where the URL does not answer,
//! the whole image comes over Zigbee a page at a time, kept on the scratch
//! volume as it arrives (PART), so a transfer cut short resumes where it
//! stopped when the coordinator offers the same image again. The bundle in
//! it is then checked as package.rs checks it.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::ezsp::Ezsp;
use super::{zcl, COORDINATOR, COORDINATOR_ENDPOINT, THERMOSTAT_ENDPOINT};
use crate::logf;
use crate::package::{self, ZigbeeImage, ZIGBEE_HEAD as HEAD};
use crate::update::{self, Updates};

pub const CLUSTER: u16 = 0x0019;

mod command {
    pub const IMAGE_NOTIFY: u8 = 0x00;
    pub const QUERY_NEXT_IMAGE: u8 = 0x01;
    pub const QUERY_NEXT_IMAGE_RESPONSE: u8 = 0x02;
    pub const IMAGE_BLOCK: u8 = 0x03;
    pub const IMAGE_PAGE: u8 = 0x04;
    pub const IMAGE_BLOCK_RESPONSE: u8 = 0x05;
    pub const UPGRADE_END: u8 = 0x06;
    pub const UPGRADE_END_RESPONSE: u8 = 0x07;
}

mod status {
    pub const SUCCESS: u8 = 0x00;
    pub const ABORT: u8 = 0x95;
    pub const INVALID_IMAGE: u8 = 0x96;
    pub const WAIT_FOR_DATA: u8 = 0x97;
}

/// The most data a block holds: what an Image Block Response (17 bytes of
/// header and fields) leaves of a ZCL frame, less a source route through up
/// to 7 relays (2 bytes and 2 a relay), which the coordinator adds once the
/// unit is not its neighbour; a frame that does not fit is refused there
/// (MESSAGE_TOO_LONG), and Zigbee2MQTT tries it again for good.
const ROUTE: usize = 2 + 2 * 7;
const BLOCK: u8 = (zcl::MAX_FRAME - 17 - ROUTE) as u8;
/// The image is asked for a page at a time, the server sending its blocks
/// one after another: one request a page, not one a block. Zigbee2MQTT
/// (zigbee-herdsman 11) loses its wait for the Upgrade End Request after
/// about 32,768 requests in a session, which a whole image block by block
/// passes.
const PAGE: u16 = 4096;
/// Between a page's blocks, for a server that honours it (zigpy does;
/// Zigbee2MQTT keeps its own image_block_response_delay).
const RESPONSE_SPACING_MS: u16 = 50;
const FIRST_QUERY: Duration = Duration::from_secs(5 * 60);
const QUERY_EVERY: Duration = Duration::from_secs(24 * 3600);
/// A request unanswered this long is sent again.
const RETRY: Duration = Duration::from_secs(10);
/// Between block requests while the package comes over Wi-Fi: Zigbee2MQTT
/// ends a session after 150 s without one, zigpy (ZHA) after 30 s: one
/// request lost still leaves the next inside that.
const KEEPALIVE: Duration = Duration::from_secs(10);
/// A coordinator silent this long has ended the session.
const SILENCE: Duration = Duration::from_secs(5 * 60);
const WAIT_MAX: Duration = Duration::from_secs(3600);
/// The image as it arrives, on the scratch volume beside update::DOWNLOAD.
const PART: &str = "/media/scratch/.unfr0zen/zigbee-ota.part";

static BUSY: AtomicBool = AtomicBool::new(false);

/// Whether an update is under way, which keeps the unit awake.
pub fn busy() -> bool {
    BUSY.load(Ordering::SeqCst)
}

/// Where the bundle comes from, and how far it has come: the Wi-Fi fetch
/// and the unwrapping run off the Zigbee thread, which keeps the
/// coordinator's session alive meanwhile.
enum Bundle {
    /// The URL has not arrived yet.
    Unknown,
    /// The package is being fetched over Wi-Fi from its URL.
    Fetching(mpsc::Receiver<Result<(), String>>),
    /// No URL, or it did not answer: the image comes over Zigbee.
    OverZigbee,
    /// The image is all here, its bundle being unwrapped.
    Unwrapping(mpsc::Receiver<Result<(), String>>),
    /// The bundle is at update::DOWNLOAD.
    Ready,
}

/// An image on its way.
struct Session {
    /// Where the image is kept as it arrives.
    path: PathBuf,
    server: (u16, u8),
    version: u32,
    size: u32,
    part: File,
    offset: u32,
    head: Vec<u8>,
    bundle: Bundle,
    /// When the last request went out, and whether what it asked for (up to
    /// page_end) is still to come.
    requested: Instant,
    unanswered: bool,
    page_end: u32,
    /// A Wait For Data's end.
    not_before: Instant,
    /// When the coordinator last answered.
    heard: Instant,
    /// When the Upgrade End Request went out.
    ended: Option<Instant>,
    /// The last tenth of the image logged.
    logged: u32,
}

impl Session {
    /// A session for the image, resuming the one kept at `path` if it is this.
    fn open(path: &Path, server: (u16, u8), version: u32, size: u32) -> Result<Session, String> {
        let error = |e: std::io::Error| format!("{}: {e}", path.display());
        let mut part =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).map_err(error)?;
        let mut head = Vec::new();
        (&mut part).take(HEAD as u64).read_to_end(&mut head).map_err(error)?;
        let kept = part.metadata().map_err(error)?.len();
        let resumable = package::zigbee_header(&head).is_some_and(|h| h.version == version && h.total == size)
            && kept <= size.into();
        let offset = if resumable {
            kept as u32
        } else {
            part.set_len(0).map_err(error)?;
            head.clear();
            0
        };
        part.seek(SeekFrom::Start(offset.into())).map_err(error)?;
        let now = Instant::now();
        Ok(Session {
            path: path.to_path_buf(),
            server,
            version,
            size,
            part,
            offset,
            head,
            bundle: Bundle::Unknown,
            requested: now,
            unanswered: false,
            page_end: 0,
            not_before: now,
            heard: now,
            ended: None,
            logged: offset * 10 / size.max(1),
        })
    }

    fn name(&self) -> String {
        package::zigbee_header(&self.head)
            .map(|h| h.name)
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("{:#x}", self.version))
    }

    /// Takes a block of the image: false when it is not the next.
    fn take(&mut self, offset: u32, data: &[u8]) -> Result<bool, String> {
        if offset != self.offset || data.is_empty() {
            return Ok(false);
        }
        if u64::from(offset) + data.len() as u64 > self.size.into() {
            return Err("a block past the image's end".into());
        }
        self.part.write_all(data).map_err(|e| format!("{}: {e}", self.path.display()))?;
        if self.head.len() < HEAD {
            self.head.extend_from_slice(&data[..data.len().min(HEAD - self.head.len())]);
        }
        self.offset += data.len() as u32;
        let tenth = self.offset * 10 / self.size;
        if tenth > self.logged {
            self.logged = tenth;
            logf!("zigbee OTA: {}0% of {} over Zigbee", tenth, self.name());
        }
        Ok(true)
    }

    /// The bundle in the image unwrapped to update::DOWNLOAD on a thread of
    /// its own.
    fn unwrap(&mut self, product: u16) -> Result<mpsc::Receiver<Result<(), String>>, String> {
        self.part.flush().map_err(|e| format!("{}: {e}", self.path.display()))?;
        let (path, image) = (self.path.clone(), (package::ZIGBEE_MANUFACTURER, product, self.version, self.size));
        let (sent, result) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = sent.send(unwrap(&path, image, Path::new(update::DOWNLOAD)));
        });
        Ok(result)
    }
}

/// The bundle in the image at `path` unwrapped to `to`, checked: the image
/// must be the one offered (manufacturer, image type, version, size).
fn unwrap(path: &Path, offered: (u16, u16, u32, u32), to: &Path) -> Result<(), String> {
    let error = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut part = File::open(path).map_err(error)?;
    let mut head = vec![0; 56];
    part.read_exact(&mut head).map_err(error)?;
    let header = package::zigbee_header(&head).ok_or("not a Zigbee OTA image")?;
    if (header.manufacturer, header.image_type, header.version, header.total) != offered {
        return Err(format!(
            "an image of {:04X}:{:04X} {:#x}, {} bytes, offered as {:04X}:{:04X} {:#x}, {} bytes",
            header.manufacturer,
            header.image_type,
            header.version,
            header.total,
            offered.0,
            offered.1,
            offered.2,
            offered.3
        ));
    }
    part.rewind().map_err(error)?;
    let bundle = File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut unwrap = ZigbeeImage::new(BufWriter::new(&bundle));
    let mut buf = vec![0; 64 << 10];
    loop {
        let n = part.read(&mut buf).map_err(error)?;
        if n == 0 {
            break;
        }
        unwrap.feed(&buf[..n])?;
    }
    unwrap.finish()?;
    bundle.sync_all().map_err(|e| format!("{}: {e}", to.display()))
}

/// Where a request from `offset` for `asked` bytes ends: never past the
/// image's `size`, as Zigbee2MQTT sends a page until its size is reached,
/// and past the end its blocks are empty, for good.
fn request_end(offset: u32, size: u32, asked: u16) -> u32 {
    size.min(offset.saturating_add(asked.into()))
}

/// Where a request asks from: a keepalive with the image all here asks for
/// its last byte, as zigpy (ZHA) ends a session on a request past the end.
fn keepalive_from(offset: u32, size: u32, waiting: bool) -> u32 {
    if waiting {
        offset.min(size.saturating_sub(1))
    } else {
        offset
    }
}

/// The OTA Upgrade client.
pub struct Ota {
    /// None on a unit with no update engine, which takes no images.
    updates: Option<Arc<Updates>>,
    /// Where the Wi-Fi download runs.
    runtime: Option<tokio::runtime::Handle>,
    image_type: u16,
    running: u32,
    sequence: u8,
    next_query: Instant,
    session: Option<Session>,
    /// A Wi-Fi fetch or unwrapping still running after its session stopped:
    /// the engine stays claimed until it ends, as it writes update::DOWNLOAD.
    draining: Option<mpsc::Receiver<Result<(), String>>>,
}

impl Ota {
    pub fn new(updates: Option<Arc<Updates>>, runtime: Option<tokio::runtime::Handle>) -> Ota {
        let updates = updates.filter(|u| u.can_install());
        let running = updates.as_ref().and_then(|u| u.installed()).map_or(0, update::matter_version);
        Ota {
            updates,
            runtime,
            image_type: package::product(crate::device::current().name),
            running,
            sequence: 0,
            next_query: Instant::now() + FIRST_QUERY,
            session: None,
            draining: None,
        }
    }

    /// The image this unit runs: manufacturer, image type, version.
    fn image(&self, version: u32) -> Vec<u8> {
        let mut out = package::ZIGBEE_MANUFACTURER.to_le_bytes().to_vec();
        out.extend(self.image_type.to_le_bytes());
        out.extend(version.to_le_bytes());
        out
    }

    /// Sends a command to the OTA server: false when the radio had no room.
    fn send(&mut self, ezsp: &mut Ezsp, to: (u16, u8), cmd: u8, payload: &[u8]) -> Result<bool, String> {
        self.sequence = self.sequence.wrapping_add(1);
        super::send(ezsp, to, THERMOSTAT_ENDPOINT, CLUSTER, &zcl::client_command(self.sequence, cmd, payload))
    }

    fn query(&mut self, ezsp: &mut Ezsp, to: (u16, u8)) -> Result<(), String> {
        let mut payload = vec![0]; // field control: no hardware version
        payload.extend(self.image(self.running));
        self.send(ezsp, to, command::QUERY_NEXT_IMAGE, &payload).map(drop)
    }

    fn end_request(&mut self, ezsp: &mut Ezsp, to: (u16, u8), status: u8, version: u32) -> Result<(), String> {
        let mut payload = vec![status];
        payload.extend(self.image(version));
        self.send(ezsp, to, command::UPGRADE_END, &payload).map(drop)
    }

    /// A command from the OTA server to the thermostat's endpoint.
    pub fn message(&mut self, ezsp: &mut Ezsp, from: (u16, u8), frame: &[u8]) -> Result<(), String> {
        if self.updates.is_none() {
            return Ok(());
        }
        match zcl::server_command(frame) {
            Some((command::IMAGE_NOTIFY, _)) => self.query(ezsp, from),
            Some((command::QUERY_NEXT_IMAGE_RESPONSE, payload)) => self.offered(ezsp, from, payload),
            Some((command::IMAGE_BLOCK_RESPONSE, payload)) => {
                self.block(from, payload);
                Ok(())
            }
            Some((command::UPGRADE_END_RESPONSE, payload)) => {
                self.upgrade_end(from, payload);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The coordinator's answer to a query: an image, or none.
    fn offered(&mut self, ezsp: &mut Ezsp, from: (u16, u8), payload: &[u8]) -> Result<(), String> {
        let [status::SUCCESS, rest @ ..] = payload else {
            return Ok(());
        };
        let Some(rest) = rest.get(..12) else {
            return Ok(());
        };
        let manufacturer = u16::from_le_bytes([rest[0], rest[1]]);
        let image_type = u16::from_le_bytes([rest[2], rest[3]]);
        let version = u32::from_le_bytes([rest[4], rest[5], rest[6], rest[7]]);
        let size = u32::from_le_bytes([rest[8], rest[9], rest[10], rest[11]]);
        if (manufacturer, image_type) != (package::ZIGBEE_MANUFACTURER, self.image_type) {
            logf!(
                "zigbee OTA: offered an image for {manufacturer:04X}:{image_type:04X}, not this unit's {:04X}:{:04X}",
                package::ZIGBEE_MANUFACTURER,
                self.image_type
            );
            return self.end_request(ezsp, from, status::INVALID_IMAGE, version);
        }
        if version <= self.running {
            logf!("zigbee OTA: offered {version:#x}, which is not newer than {:#x}", self.running);
            return self.end_request(ezsp, from, status::INVALID_IMAGE, version);
        }
        if let Some(s) = &mut self.session {
            if (s.version, s.size) == (version, size) {
                // The coordinator started over: carry on where the image is.
                s.server = from;
                s.unanswered = false;
                s.heard = Instant::now();
                s.ended = None;
                return Ok(());
            }
            logf!("zigbee OTA: {version:#x} offered while {} is on its way", s.name());
            return self.end_request(ezsp, from, status::ABORT, version);
        }
        let updates = self.updates.clone().expect("checked by message");
        if size as usize > update::BUNDLE_LIMIT + HEAD {
            logf!("zigbee OTA: offered a {size}-byte image");
            return self.end_request(ezsp, from, status::INVALID_IMAGE, version);
        }
        if !updates.claim() {
            logf!("zigbee OTA: {version:#x} offered, but an update is already being installed");
            return self.end_request(ezsp, from, status::ABORT, version);
        }
        match Session::open(Path::new(PART), from, version, size) {
            Ok(s) => {
                match s.offset {
                    0 => logf!("zigbee OTA: downloading {version:#x}, {size} bytes"),
                    at => logf!("zigbee OTA: resuming {} at {at} of {size} bytes", s.name()),
                }
                BUSY.store(true, Ordering::SeqCst);
                self.session = Some(s);
                Ok(())
            }
            Err(e) => {
                logf!("zigbee OTA: {e}");
                updates.release();
                self.end_request(ezsp, from, status::ABORT, version)
            }
        }
    }

    /// An Image Block Response, heeded only from the session's server and
    /// for its image.
    fn block(&mut self, from: (u16, u8), payload: &[u8]) {
        let image_type = self.image_type;
        let Some(s) = self.session.as_mut().filter(|s| s.server == from) else {
            return;
        };
        let Some((&status, rest)) = payload.split_first() else {
            return;
        };
        let u32_at = |i: usize| rest.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        match status {
            status::SUCCESS => {
                let (Some(image), Some(version), Some(offset), Some(&n)) =
                    (rest.get(..4), u32_at(4), u32_at(8), rest.get(12))
                else {
                    return;
                };
                let Some(data) = rest.get(13..13 + n as usize) else {
                    return;
                };
                let ours = [package::ZIGBEE_MANUFACTURER.to_le_bytes(), image_type.to_le_bytes()].concat();
                if image != ours || version != s.version {
                    return;
                }
                s.heard = Instant::now();
                match s.take(offset, data) {
                    Ok(true) => s.unanswered = s.offset < s.page_end,
                    // Past a block that never came: once the page's last is
                    // here, the next page asks from the gap.
                    Ok(false) if offset > s.offset && offset as u64 + data.len() as u64 >= s.page_end.into() => {
                        s.unanswered = false
                    }
                    Ok(false) => {}
                    Err(e) => {
                        logf!("zigbee OTA: {e}");
                        self.stop(true);
                    }
                }
            }
            status::WAIT_FOR_DATA => {
                let (Some(now), Some(then)) = (u32_at(0), u32_at(4)) else {
                    return;
                };
                s.heard = Instant::now();
                s.not_before = Instant::now() + Duration::from_secs(then.saturating_sub(now).into()).min(WAIT_MAX);
                s.unanswered = false;
            }
            _ => {
                logf!("zigbee OTA: the coordinator stopped sending {} (status {status:#04x})", s.name());
                self.stop(true);
            }
        }
    }

    /// The coordinator's go-ahead: the engine applies the package after the
    /// delay it gives, and restarts the unit.
    fn upgrade_end(&mut self, from: (u16, u8), payload: &[u8]) {
        let Some(s) = self.session.as_ref().filter(|s| s.ended.is_some() && s.server == from) else {
            return;
        };
        let field = |i: usize| payload.get(i..i + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        let (Some(version), Some(now), Some(then)) = (field(4), field(8), field(12)) else {
            return;
        };
        if version != s.version {
            return;
        }
        if then == u32::MAX {
            // Until the coordinator says again.
            return logf!("zigbee OTA: the coordinator holds {} back", s.name());
        }
        let delay = Duration::from_secs(then.saturating_sub(now).into()).min(WAIT_MAX);
        let what = format!("{} over Zigbee", s.name());
        if let Some(s) = self.session.take() {
            let _ = std::fs::remove_file(s.path);
        }
        let updates = self.updates.clone().expect("a session has updates");
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            if updates.apply(&what).is_err() {
                BUSY.store(false, Ordering::SeqCst);
            }
        });
    }

    /// Ends the session, keeping what arrived to resume unless `discard`.
    /// The engine is released, and the unit let sleep, once nothing the
    /// session started still writes.
    fn stop(&mut self, discard: bool) {
        if let Some(s) = self.session.take() {
            if discard {
                let _ = std::fs::remove_file(&s.path);
            }
            if let Bundle::Fetching(result) | Bundle::Unwrapping(result) = s.bundle {
                self.draining = Some(result);
                return;
            }
        }
        self.release();
    }

    fn release(&mut self) {
        if let Some(updates) = &self.updates {
            updates.release();
        }
        BUSY.store(false, Ordering::SeqCst);
    }

    /// Releases the engine once what a stopped session started has ended.
    fn drain(&mut self) {
        if let Some(result) = &self.draining {
            if let Err(TryRecvError::Empty) = result.try_recv() {
                return;
            }
            self.draining = None;
            self.release();
        }
    }

    /// The unit is off the network, or its radio is: a session ends, to
    /// resume when the coordinator offers the image again.
    pub fn offline(&mut self) {
        self.drain();
        if let Some(s) = &self.session {
            logf!(
                "zigbee OTA: off the network at {} of {} bytes of {}; it resumes there when offered again",
                s.offset,
                s.size,
                s.name()
            );
            self.stop(false);
        }
    }

    /// Asks for what is due: the next image now and then, the next block, or
    /// the end of the transfer.
    pub fn poll(&mut self, ezsp: &mut Ezsp) -> Result<(), String> {
        self.drain();
        let now = Instant::now();
        let (Some(updates), Some(s)) = (self.updates.clone(), &mut self.session) else {
            if self.updates.is_some() && now >= self.next_query {
                self.next_query = now + QUERY_EVERY;
                self.query(ezsp, (COORDINATOR, COORDINATOR_ENDPOINT))?;
            }
            return Ok(());
        };
        match &s.bundle {
            Bundle::Fetching(result) => match result.try_recv() {
                Ok(Ok(())) => {
                    logf!("zigbee OTA: fetched {} over Wi-Fi", s.name());
                    s.bundle = Bundle::Ready;
                }
                Ok(Err(e)) => {
                    logf!("zigbee OTA: over Wi-Fi: {e}; {} comes over Zigbee", s.name());
                    s.bundle = Bundle::OverZigbee;
                }
                Err(TryRecvError::Disconnected) => s.bundle = Bundle::OverZigbee,
                Err(TryRecvError::Empty) => {}
            },
            Bundle::Unwrapping(result) => match result.try_recv() {
                Ok(Ok(())) => {
                    logf!("zigbee OTA: received {} over Zigbee", s.name());
                    s.bundle = Bundle::Ready;
                }
                Ok(Err(e)) => {
                    logf!("zigbee OTA: {}: {e}", s.name());
                    let (server, version) = (s.server, s.version);
                    self.stop(true);
                    return self.end_request(ezsp, server, status::INVALID_IMAGE, version);
                }
                Err(TryRecvError::Disconnected) => {
                    logf!("zigbee OTA: unwrapping {} stopped", s.name());
                    let (server, version) = (s.server, s.version);
                    self.stop(true);
                    return self.end_request(ezsp, server, status::INVALID_IMAGE, version);
                }
                Err(TryRecvError::Empty) => {}
            },
            _ => {}
        }
        // The whole image here with its URL still unread: it has none.
        let url = package::package_url(&s.head).or_else(|| (s.offset == s.size).then_some(None));
        if let (Bundle::Unknown, Some(url)) = (&s.bundle, url) {
            s.bundle = match (url, &self.runtime) {
                (Some(url), Some(runtime)) => {
                    logf!("zigbee OTA: fetching {} over Wi-Fi from {url}", s.name());
                    let (sent, result) = mpsc::channel();
                    let version = s.version;
                    runtime.spawn(async move {
                        let _ = sent.send(updates.download(&url, Some(version)).await);
                    });
                    Bundle::Fetching(result)
                }
                _ => Bundle::OverZigbee,
            };
        }
        if matches!(s.bundle, Bundle::OverZigbee) && s.offset == s.size {
            match s.unwrap(self.image_type) {
                Ok(result) => s.bundle = Bundle::Unwrapping(result),
                Err(e) => {
                    logf!("zigbee OTA: {}: {e}", s.name());
                    let (server, version) = (s.server, s.version);
                    self.stop(true);
                    return self.end_request(ezsp, server, status::INVALID_IMAGE, version);
                }
            }
        }
        let waiting = matches!(s.bundle, Bundle::Fetching(_) | Bundle::Unwrapping(_));
        if now.duration_since(s.heard.max(s.not_before)) > SILENCE && !waiting {
            logf!(
                "zigbee OTA: the coordinator stopped answering at {} of {} bytes of {}; it resumes there when offered again",
                s.offset,
                s.size,
                s.name()
            );
            self.stop(false);
            return Ok(());
        }
        let (server, version) = (s.server, s.version);
        if let Some(sent) = s.ended {
            if now.duration_since(sent) >= RETRY {
                s.ended = Some(now);
                self.end_request(ezsp, server, status::SUCCESS, version)?;
            }
            return Ok(());
        }
        if matches!(s.bundle, Bundle::Ready) {
            s.ended = Some(now);
            return self.end_request(ezsp, server, status::SUCCESS, version);
        }
        // While the bundle comes another way, a block now and then keeps the
        // session; otherwise the next page, or the rest of one gone quiet.
        let due = if waiting {
            now.duration_since(s.requested) >= KEEPALIVE
        } else if s.unanswered {
            now.duration_since(s.requested.max(s.heard)) >= RETRY
        } else {
            now >= s.not_before
        };
        if !due {
            return Ok(());
        }
        s.requested = now;
        s.unanswered = true;
        let asked = if waiting { BLOCK.into() } else { PAGE };
        s.page_end = request_end(s.offset, s.size, asked);
        let page = (s.page_end - s.offset) as u16;
        let offset = keepalive_from(s.offset, s.size, waiting);
        let mut payload = vec![0]; // field control: no node address or block period
        payload.extend(self.image(version));
        payload.extend(offset.to_le_bytes());
        payload.push(BLOCK);
        if waiting {
            return self.send(ezsp, server, command::IMAGE_BLOCK, &payload).map(drop);
        }
        payload.extend(page.to_le_bytes());
        payload.extend(RESPONSE_SPACING_MS.to_le_bytes());
        self.send(ezsp, server, command::IMAGE_PAGE, &payload).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::tests::zigbee_image;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nestlocal-zigbee-ota-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_image_is_nest_labs() {
        assert_eq!(package::ZIGBEE_MANUFACTURER, zcl::MANUFACTURER_CODE);
    }

    #[test]
    fn blocks_in_order_give_the_bundle_and_a_restart_resumes_them() {
        let bundle = [&[0x1F, 0x8B][..], &(0..3000u32).map(|i| i as u8).collect::<Vec<_>>()].concat();
        let image = zigbee_image(0x8001, 0x0005_0281, Some("http://nas.lan/p.ota"), &bundle);
        let dir = scratch("resume");
        let (part, out) = (dir.join("part"), dir.join("update.tgz"));
        let size = image.len() as u32;
        let mut s = Session::open(&part, (0, 1), 0x0005_0281, size).unwrap();
        let blocks: Vec<&[u8]> = image.chunks(BLOCK as usize).collect();
        for (i, block) in blocks[..20].iter().enumerate() {
            assert!(s.take(i as u32 * u32::from(BLOCK), block).unwrap());
        }
        assert!(!s.take(0, blocks[0]).unwrap(), "a block again");
        assert_eq!(s.name(), "unfr0zen v0.6.0");
        drop(s);
        // Another image offered: started afresh, what was kept dropped.
        assert_eq!(Session::open(&part, (0, 1), 0x0005_0281, size + 1).unwrap().offset, 0);
        let mut s = Session::open(&part, (0, 1), 0x0005_0281, size).unwrap();
        assert_eq!(s.offset, 0);
        for block in &blocks[..20] {
            assert!(s.take(s.offset, block).unwrap());
        }
        drop(s);
        // This one again: resumed.
        let mut s = Session::open(&part, (0, 1), 0x0005_0281, size).unwrap();
        assert_eq!(s.offset, 20 * u32::from(BLOCK));
        for block in &blocks[20..] {
            assert!(s.take(s.offset, block).unwrap());
        }
        assert_eq!((s.offset, s.head.len()), (size, HEAD.min(image.len())));
        s.part.flush().unwrap();
        let offered = (0x10D6, 0x8001, 0x0005_0281, size);
        unwrap(&part, offered, &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), bundle);
        assert!(
            unwrap(&part, (0x10D6, 0x8002, 0x0005_0281, size), &out).unwrap_err().contains("offered as"),
            "another unit's"
        );
        assert!(
            unwrap(&part, (0x10D6, 0x8001, 0x0005_0282, size), &out).unwrap_err().contains("offered as"),
            "another version"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An Image Block Response with this part of the image.
    fn block_response(version: u32, offset: u32, data: &[u8]) -> Vec<u8> {
        let mut out = vec![status::SUCCESS];
        out.extend(package::ZIGBEE_MANUFACTURER.to_le_bytes());
        out.extend(0x8001u16.to_le_bytes());
        out.extend(version.to_le_bytes());
        out.extend(offset.to_le_bytes());
        out.push(data.len() as u8);
        out.extend(data);
        out
    }

    #[test]
    fn a_page_is_asked_again_from_a_missing_block_once_its_last_has_come() {
        let image = zigbee_image(0x8001, 0x0005_0281, None, &vec![7; 20_000]);
        let dir = scratch("page");
        let size = image.len() as u32;
        let mut ota = Ota {
            updates: None,
            runtime: None,
            image_type: 0x8001,
            running: 0,
            sequence: 0,
            next_query: Instant::now(),
            session: Some(Session::open(&dir.join("part"), (0, 1), 0x0005_0281, size).unwrap()),
            draining: None,
        };
        let blocks: Vec<(u32, &[u8])> =
            image.chunks(BLOCK as usize).enumerate().map(|(i, b)| (i as u32 * u32::from(BLOCK), b)).collect();
        let page = |ota: &mut Ota| {
            let s = ota.session.as_mut().unwrap();
            s.unanswered = true;
            s.page_end = s.size.min(s.offset + u32::from(PAGE));
        };
        let in_page = usize::from(PAGE).div_ceil(usize::from(BLOCK));
        // A whole page: answered once its last block is in.
        page(&mut ota);
        for &(offset, data) in &blocks[..in_page - 1] {
            ota.block((0, 1), &block_response(0x0005_0281, offset, data));
            assert!(ota.session.as_ref().unwrap().unanswered);
        }
        let (offset, data) = blocks[in_page - 1];
        ota.block((0, 1), &block_response(0x0005_0281, offset, &data[..usize::from(PAGE) - offset as usize]));
        let s = ota.session.as_ref().unwrap();
        assert_eq!((s.offset, s.unanswered), (u32::from(PAGE), false));
        // The next with a block missing: the rest is skipped, and the page is
        // over when its last block comes, the offset at the gap.
        page(&mut ota);
        let start = u32::from(PAGE);
        let first = &image[start as usize..(start + u32::from(BLOCK)) as usize];
        ota.block((0, 1), &block_response(0x0005_0281, start, first));
        let gap = start + u32::from(BLOCK);
        let mut at = gap + u32::from(BLOCK);
        let end = 2 * u32::from(PAGE);
        while at < end {
            let n = u32::from(BLOCK).min(end - at);
            assert!(ota.session.as_ref().unwrap().unanswered, "at {at}");
            ota.block((0, 1), &block_response(0x0005_0281, at, &image[at as usize..(at + n) as usize]));
            at += n;
        }
        let s = ota.session.as_ref().unwrap();
        assert_eq!((s.offset, s.unanswered), (gap, false));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A client in a session for an image of `size` bytes from (0, 1).
    fn in_session(dir: &Path, size: u32) -> Ota {
        Ota {
            updates: None,
            runtime: None,
            image_type: 0x8001,
            running: 0,
            sequence: 0,
            next_query: Instant::now(),
            session: Some(Session::open(&dir.join("part"), (0, 1), 0x0005_0281, size).unwrap()),
            draining: None,
        }
    }

    #[test]
    fn only_the_sessions_server_is_heeded_for_its_image() {
        let image = zigbee_image(0x8001, 0x0005_0281, None, &vec![7; 1000]);
        let dir = scratch("sender");
        let mut ota = in_session(&dir, image.len() as u32);
        let first = &image[..usize::from(BLOCK)];
        // Another node, or another endpoint: nothing taken, nothing stopped.
        ota.block((0x1234, 1), &block_response(0x0005_0281, 0, first));
        ota.block((0, 2), &block_response(0x0005_0281, 0, first));
        ota.block((0x1234, 1), &[status::ABORT]);
        assert_eq!(ota.session.as_ref().unwrap().offset, 0);
        // Another image type from the server: not taken.
        let mut other = block_response(0x0005_0281, 0, first);
        other[3..5].copy_from_slice(&0x8002u16.to_le_bytes());
        ota.block((0, 1), &other);
        assert_eq!(ota.session.as_ref().unwrap().offset, 0);
        ota.block((0, 1), &block_response(0x0005_0281, 0, first));
        assert_eq!(ota.session.as_ref().unwrap().offset, u32::from(BLOCK));
        // The server's abort ends it.
        ota.block((0, 1), &[status::ABORT]);
        assert!(ota.session.is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_stopped_session_holds_the_engine_until_its_fetch_ends() {
        let dir = scratch("drain");
        let mut ota = in_session(&dir, 1000);
        let (sent, result) = mpsc::channel();
        ota.session.as_mut().unwrap().bundle = Bundle::Fetching(result);
        ota.offline();
        assert!(ota.session.is_none());
        assert!(ota.draining.is_some(), "the fetch still runs");
        ota.drain();
        assert!(ota.draining.is_some());
        sent.send(Err("timed out".into())).unwrap();
        ota.drain();
        assert!(ota.draining.is_none(), "released once it ended");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_keepalive_never_asks_past_the_image() {
        assert_eq!(keepalive_from(500, 1000, true), 500);
        assert_eq!(keepalive_from(1000, 1000, true), 999);
        assert_eq!(keepalive_from(1000, 1000, false), 1000);
    }

    #[test]
    fn no_request_reaches_past_the_image() {
        assert_eq!(request_end(0, 100_000, PAGE), u32::from(PAGE));
        assert_eq!(request_end(99_990, 100_000, PAGE), 100_000);
        assert_eq!(request_end(100_000, 100_000, BLOCK.into()), 100_000);
    }

    #[test]
    fn a_block_past_the_end_is_refused() {
        let image = zigbee_image(0x8001, 0x0005_0281, None, b"bundle");
        let dir = scratch("past");
        let mut s = Session::open(&dir.join("part"), (0, 1), 0x0005_0281, image.len() as u32).unwrap();
        assert!(s.take(0, &image[..image.len() - 1]).unwrap());
        assert!(s.take(s.offset, &image[image.len() - 2..]).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_block_fits_a_frame_with_a_source_route() {
        assert_eq!(BLOCK, 49);
        let mut response = vec![0x19, 1, command::IMAGE_BLOCK_RESPONSE, status::SUCCESS];
        response.extend([0; 12]);
        response.push(BLOCK);
        response.extend(vec![0; BLOCK as usize]);
        assert_eq!(response.len() + ROUTE, zcl::MAX_FRAME);
    }
}
