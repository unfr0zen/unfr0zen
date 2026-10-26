//! The diamond3's USB links: which mode the unit is in and on which physical
//! port, recovery's fastboot function, and the ROM SDP loader (hidapi).

use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

use nusb::MaybeFuture;
use unfr0zen_core::diamond3::fastboot::Protocol;
use unfr0zen_core::diamond3::recovery::RAM_BASE;
use unfr0zen_core::diamond3::sdp::{self, Hid, UBOOT_BASE};
use unfr0zen_core::{Error, Result};

use super::assets;
use crate::usb::{self, port_name, usb_error, Id};

pub const SDP: Id = (0x15A2, 0x0071);
pub const RECOVERY: Id = (0x18D1, 0x4EE0);
pub const APP_ACM: Id = (0x2464, 0x0030);
const APP: [Id; 3] = [(0x2464, 0x0030), (0x2464, 0x0031), (0x0525, 0xB4A4)];
/// The initramfs's own ACM gadget (scripts/initramfs-init).
pub const INITRAMFS: Id = (0x0525, 0xA4A7);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Sdp,
    Recovery,
    App,
    Initramfs,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Sdp => "USB download (SDP)",
            Mode::Recovery => "recovery",
            Mode::App => "app",
            Mode::Initramfs => "initramfs",
        }
    }
}

pub fn mode(id: Id) -> Option<Mode> {
    match id {
        SDP => Some(Mode::Sdp),
        RECOVERY => Some(Mode::Recovery),
        INITRAMFS => Some(Mode::Initramfs),
        _ if APP.contains(&id) => Some(Mode::App),
        _ => None,
    }
}

/// The diamond3 modes on the bus: physical port, identity.
pub fn devices() -> Result<BTreeMap<String, Id>> {
    Ok(usb::list()?.into_iter().filter(|(_, id)| mode(*id).is_some()).collect())
}

/// Waits for the unit in one of the wanted modes, on the given port if any.
pub fn wait_device(wanted: &[Mode], timeout: Duration, port: Option<&str>) -> Result<(String, Id)> {
    let until = Instant::now() + timeout;
    let mut last = None;
    while Instant::now() < until {
        let found: BTreeMap<String, Id> = devices()?.into_iter()
            .filter(|(name, id)| mode(*id).is_some_and(|m| wanted.contains(&m)) && port.is_none_or(|p| p == name))
            .collect();
        if last.as_ref() != Some(&found) {
            log::info!("USB: {:?}", found.iter().map(|(p, (v, d))| format!("{p} {v:04x}:{d:04x}")).collect::<Vec<_>>());
            last = Some(found.clone());
        }
        if found.len() > 1 {
            return Err(Error::Device("multiple matching USB devices; connect only one diamond3".into()));
        }
        if let Some((name, id)) = found.into_iter().next() {
            return Ok((name, id));
        }
        thread::sleep(Duration::from_millis(500));
    }
    let names: Vec<&str> = wanted.iter().map(|m| m.name()).collect();
    Err(Error::Timeout(format!("USB device did not enter {} mode", names.join("/"))))
}

pub fn open_fastboot(port: &str) -> Result<Protocol<usb::Pipe>> {
    let info = nusb::list_devices().wait().map_err(usb_error)?
        .find(|info| (info.vendor_id(), info.product_id()) == RECOVERY && port_name(info) == port)
        .ok_or_else(|| Error::Device("recovery USB device is gone".into()))?;
    let number = info.interfaces().find(|i| (i.class(), i.subclass(), i.protocol()) == (0xff, 0x42, 0x03))
        .map(|i| i.interface_number()).ok_or_else(|| Error::Device("recovery has no fastboot interface".into()))?;
    let hint = if cfg!(windows) {
        " (recovery's fastboot interface should have WinUSB from its OS descriptors: check Device Manager)"
    } else {
        ""
    };
    let device = info.open().wait().map_err(|e| usb_error(format!("open recovery: {e}{hint}")))?;
    let interface = device.claim_interface(number).wait().map_err(|e| usb_error(format!("claim fastboot: {e}{hint}")))?;
    Ok(Protocol::new(usb::Pipe::new(&interface, "fastboot")?))
}

struct HidDevice(hidapi::HidDevice);

impl Hid for HidDevice {
    fn write(&mut self, report: &[u8]) -> Result<usize> {
        self.0.write(report).map_err(|e| Error::Io(e.to_string()))
    }
    fn read(&mut self, timeout_ms: i32) -> Result<Vec<u8>> {
        let mut buffer = [0u8; 65];
        let n = self.0.read_timeout(&mut buffer, timeout_ms).map_err(|e| Error::Io(e.to_string()))?;
        Ok(buffer[..n].to_vec())
    }
}

fn wait_hid(api: &mut hidapi::HidApi, (vid, pid): Id, timeout: Duration) -> Result<HidDevice> {
    log::info!("Waiting up to {timeout:?} for USB HID {vid:04x}:{pid:04x}");
    let until = Instant::now() + timeout;
    while Instant::now() < until {
        let _ = api.refresh_devices();
        if let Ok(handle) = api.open(vid, pid) {
            let mut device = HidDevice(handle);
            sdp::drain(&mut device, 50);
            return Ok(device);
        }
        thread::sleep(Duration::from_millis(200));
    }
    Err(Error::Timeout(format!("No USB HID device {vid:04x}:{pid:04x} within {timeout:?}")))
}

/// The unit's stock bootloader DCD (extracted from its own NAND firmware 1,
/// IVT@0x100400) minus one write: the CCM clock gates, the IOMUX DDR pads,
/// then the single-chip-select MMDC sequence in factory order. Every address
/// lies inside the ROM's hardcoded SDP write windows, and the ROM runs this
/// same sequence at every normal boot. The stock CCM_CBCDR divider write is
/// skipped: over SDP it can kill the USB session, and the ROM's default clock
/// configuration runs the MMDC fine.
const DDR_INIT: [(u32, u32); 74] = [
    // CCM: clock gates.
    (0x020c_4068, 0xffff_ffff),
    (0x020c_406c, 0xffff_ffff),
    (0x020c_4070, 0xffff_ffff),
    (0x020c_4074, 0xffff_ffff),
    (0x020c_4078, 0xffff_ffff),
    (0x020c_407c, 0xffff_ffff),
    (0x020c_4080, 0xffff_ffff),
    (0x020c_4084, 0xffff_ffff),
    // IOMUXC: DDR pads.
    (0x020e_0618, 0x0008_0000),
    (0x020e_05fc, 0x0000_0000),
    (0x020e_032c, 0x0000_0028),
    (0x020e_0300, 0x0000_0028),
    (0x020e_02fc, 0x0000_0028),
    (0x020e_05f4, 0x0000_0028),
    (0x020e_0340, 0x0000_0028),
    (0x020e_0320, 0x0000_0000),
    (0x020e_0310, 0x0000_0000),
    (0x020e_0314, 0x0000_0000),
    (0x020e_0614, 0x0000_0028),
    (0x020e_05f8, 0x0002_0000),
    (0x020e_0330, 0x0000_3028),
    (0x020e_0334, 0x0000_3028),
    (0x020e_0338, 0x0000_3028),
    (0x020e_033c, 0x0000_3028),
    (0x020e_0608, 0x0002_0000),
    (0x020e_060c, 0x0000_0028),
    (0x020e_0610, 0x0000_0028),
    (0x020e_061c, 0x0000_0028),
    (0x020e_0620, 0x0000_0028),
    (0x020e_02ec, 0x0000_0028),
    (0x020e_02f0, 0x0000_0028),
    (0x020e_02f4, 0x0000_0028),
    (0x020e_02f8, 0x0000_0028),
    // MMDC.
    (0x021b_001c, 0x0000_8000),
    (0x021b_085c, 0x1b47_00c7),
    (0x021b_08c0, 0x2492_244a),
    (0x021b_0800, 0xa139_0003),
    (0x021b_0890, 0x0047_0000),
    (0x021b_08b8, 0x0000_0800),
    (0x021b_081c, 0x3333_3333),
    (0x021b_0820, 0x3333_3333),
    (0x021b_0824, 0x3333_3333),
    (0x021b_0828, 0x3333_3333),
    (0x021b_082c, 0xf333_3333),
    (0x021b_0830, 0xf333_3333),
    (0x021b_0834, 0xf333_3333),
    (0x021b_0838, 0xf333_3333),
    (0x021b_0848, 0x3e3e_4244),
    (0x021b_0850, 0x3632_3632),
    (0x021b_083c, 0x2000_0000),
    (0x021b_0840, 0x0000_0000),
    (0x021b_08b8, 0x0000_0800),
    (0x021b_000c, 0x3337_4135),
    (0x021b_0004, 0x0002_0036),
    (0x021b_0010, 0xb6b0_0a83),
    (0x021b_0014, 0x0000_0093),
    (0x021b_0018, 0x0000_1748),
    (0x021b_002c, 0x0f9f_26d2),
    (0x021b_0030, 0x009f_0e10),
    (0x021b_0038, 0x001a_0889),
    (0x021b_0008, 0x1227_2000),
    (0x021b_0040, 0x0000_004f),
    (0x021b_0000, 0x8311_0000),
    (0x021b_001c, 0x003f_8030),
    (0x021b_001c, 0xff0a_8030),
    (0x021b_001c, 0x8201_8030),
    (0x021b_001c, 0x0602_8030),
    (0x021b_001c, 0x0103_8030),
    (0x021b_0020, 0x0000_1800),
    (0x021b_0818, 0x0000_0000),
    (0x021b_0800, 0xa139_0003),
    (0x021b_0004, 0x0002_5536),
    (0x021b_0404, 0x0001_1006),
    (0x021b_001c, 0x0000_0000),
];

/// Brings up DDR with register writes, then loads recovery U-Boot into it and
/// the bootstrap chain into OCRAM, and enters it — one ROM SDP session, no
/// intermediate loader. The boot wrapper arms the watchdog; recovery services
/// it while its console has DTR.
pub fn load_recovery(watchdog_seconds: u32) -> Result<()> {
    let mut api = hidapi::HidApi::new().map_err(|e| Error::Io(e.to_string()))?;
    let mut rom = wait_hid(&mut api, SDP, Duration::from_secs(130))?;
    for &(address, value) in &DDR_INIT {
        sdp::write_register(&mut rom, address, value)?;
        // The MMDC start bit launches the DDR initialization state machine.
        if address == 0x021b_0000 && value & 0x8000_0000 != 0 {
            thread::sleep(Duration::from_millis(100));
        }
    }
    log::info!("DDR up: {} register writes applied", DDR_INIT.len());
    let mut nonce = [0u8; 4];
    getrandom::fill(&mut nonce).map_err(|e| Error::Io(e.to_string()))?;
    let wcr = ((watchdog_seconds * 2 - 1) << 8) | 0x34;
    let parameters = [nonce.to_vec(), wcr.to_le_bytes().to_vec()].concat();
    for (address, data) in [(UBOOT_BASE, assets::RECOVERY), (0x90BF00, parameters.as_slice()),
                            (0x90B000, assets::BOOT_WRAPPER), (0x907000, assets::ROM_STAGE)] {
        sdp::write_checked(&mut rom, address, data)?;
        log::info!("Uploaded {} bytes at {address:#x}; samples match", data.len());
    }
    // The ROM sends its HAB report before it jumps; a read error only means
    // the jump already dropped USB.
    match sdp::jump(&mut rom, 0x907000) {
        Ok(_) | Err(Error::Io(_)) => {}
        Err(e) => return Err(e),
    }
    log::info!("Recovery U-Boot started; fastboot buffer at {RAM_BASE:#x}");
    Ok(())
}
