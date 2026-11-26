//! The i.MX6 boards' USB links, shared between the gen 3 and the
//! Thermostat E: each board's USB identities and recovery loader
//! ([`Profile`]), which mode the unit is in and on which physical port,
//! recovery's fastboot function, the boot ROM's SDP over hidapi, and how the
//! ROM is entered.

use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

use nusb::MaybeFuture;
use unfr0zen_core::common::recovery::{silicon, DIGPROG};
use unfr0zen_core::diamond3::fastboot::Protocol;
use unfr0zen_core::diamond3::sdp::{self, Hid};
use unfr0zen_core::{Device, Error, Result};

use crate::usb::{self, port_name, usb_error, Id};

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

/// What one i.MX6 board's USB links and recovery flow differ in.
pub(crate) struct Profile {
    /// The boot ROM's USB download mode.
    pub sdp: Id,
    /// Recovery U-Boot's console and fastboot function.
    pub recovery: Id,
    /// The application firmware's ACM gadget, for the root shell.
    pub app_acm: Id,
    /// Every identity of the application firmware.
    pub app: &'static [Id],
    /// The initramfs's own ACM gadget (scripts/initramfs-init).
    pub initramfs: Id,
    /// Loads recovery U-Boot through the ROM; the silicon revision the ROM
    /// reported, if it did.
    pub load: fn() -> Result<Option<String>>,
    /// Reports the unit leaving and rejoining USB while its recovery loads.
    pub watch_load: bool,
    /// Ends the setup with a power cycle rather than the hand-over to the
    /// volume's own init: a warm reset after a recovery session leaves the
    /// unit's boot ROM failing its NAND boot, until a power-on reset.
    pub power_cycle: bool,
}

impl Profile {
    pub fn mode(&self, id: Id) -> Option<Mode> {
        match id {
            _ if id == self.sdp => Some(Mode::Sdp),
            _ if id == self.recovery => Some(Mode::Recovery),
            _ if id == self.initramfs => Some(Mode::Initramfs),
            _ if self.app.contains(&id) => Some(Mode::App),
            _ => None,
        }
    }
}

/// Buildroot's ACM/ECM gadget.
pub(crate) const BUILDROOT_GADGET: Id = (0x1d6b, 0x0104);

/// USB identities and waits for either i.MX6 board.
#[derive(Clone, Copy)]
pub(crate) struct Target(pub Device);

impl Target {
    pub fn profile(self) -> &'static Profile {
        match self.0 {
            Device::Onyx => &crate::onyx::usb::PROFILE,
            _ => &crate::diamond3::usb::PROFILE,
        }
    }
    pub fn sdp(self) -> Id {
        self.profile().sdp
    }
    pub fn recovery(self) -> Id {
        self.profile().recovery
    }
    pub fn app_acm(self) -> Id {
        self.profile().app_acm
    }
    pub fn initramfs(self) -> Id {
        self.profile().initramfs
    }
    pub fn mode(self, id: Id) -> Option<Mode> {
        self.profile().mode(id)
    }
    pub fn wait(
        self,
        wanted: &[Mode],
        timeout: Duration,
        port: Option<&str>,
        cancel: &crate::device::CancelFlag,
    ) -> Result<(String, Id)> {
        wait_device_ids_for(self, wanted, &[], timeout, port, cancel)
    }
    /// A restored system may use Buildroot's ACM/ECM gadget on the same USB port.
    pub fn wait_restored(self, timeout: Duration, port: &str) -> Result<(String, Id)> {
        wait_device_ids_for(self, &[Mode::App], &[BUILDROOT_GADGET], timeout, Some(port), &Default::default())
    }
    pub fn load(self) -> Result<Option<String>> {
        (self.profile().load)()
    }
}

/// Waits for the unit in one of the wanted modes, or with one of the `extra`
/// identities, on the given port if any.
pub(crate) fn wait_device_ids_for(
    target: Target,
    wanted: &[Mode],
    extra: &[Id],
    timeout: Duration,
    port: Option<&str>,
    cancel: &crate::device::CancelFlag,
) -> Result<(String, Id)> {
    let until = Instant::now() + timeout;
    let mut last = None;
    while Instant::now() < until && !cancel.load(std::sync::atomic::Ordering::Relaxed) {
        let seen: BTreeMap<String, Id> = usb::list()?
            .into_iter()
            .filter(|(_, id)| target.mode(*id).is_some_and(|mode| wanted.contains(&mode)) || extra.contains(id))
            .collect();
        if last.as_ref() != Some(&seen) {
            log::info!(
                "USB: {:?}{}",
                seen.iter().map(|(p, (v, d))| format!("{p} {v:04x}:{d:04x}")).collect::<Vec<_>>(),
                port.map(|p| format!(", waiting on {p}")).unwrap_or_default()
            );
            last = Some(seen.clone());
        }
        let found: Vec<(String, Id)> = seen.into_iter().filter(|(name, _)| port.is_none_or(|p| p == name)).collect();
        if found.len() > 1 {
            return Err(Error::Device("multiple matching USB devices; connect only one thermostat".into()));
        }
        if let Some((name, id)) = found.into_iter().next() {
            return Ok((name, id));
        }
        thread::sleep(Duration::from_millis(500));
    }
    let names: Vec<&str> = wanted.iter().map(|m| m.name()).collect();
    Err(Error::Timeout(format!("USB device did not enter {} mode", names.join("/"))))
}

pub(crate) fn open_fastboot_for(target: Target, port: &str) -> Result<Protocol<usb::Pipe>> {
    let info = nusb::list_devices()
        .wait()
        .map_err(usb_error)?
        .find(|info| (info.vendor_id(), info.product_id()) == target.recovery() && port_name(info) == port)
        .ok_or_else(|| Error::Device("recovery USB device is gone".into()))?;
    let number = info
        .interfaces()
        .find(|i| (i.class(), i.subclass(), i.protocol()) == (0xff, 0x42, 0x03))
        .map(|i| i.interface_number())
        .ok_or_else(|| Error::Device("recovery has no fastboot interface".into()))?;
    let hint = if cfg!(windows) {
        " (recovery's fastboot interface should have WinUSB from its OS descriptors: check Device Manager)"
    } else {
        ""
    };
    let device = info.open().wait().map_err(|e| usb_error(format!("open recovery: {e}{hint}")))?;
    let interface =
        device.claim_interface(number).wait().map_err(|e| usb_error(format!("claim fastboot: {e}{hint}")))?;
    Ok(Protocol::new(usb::Pipe::new(&interface, "fastboot")?))
}

pub(crate) struct HidDevice(pub(crate) hidapi::HidDevice);

/// How much of a report a HID write took. On Windows, hidapi pads a report to
/// the device's longest output report and returns the padded length, so a
/// whole 17-byte command comes back as 1025.
fn report_bytes_sent(report: &[u8], sent: usize) -> usize {
    sent.min(report.len())
}

impl Hid for HidDevice {
    fn write(&mut self, report: &[u8]) -> Result<usize> {
        let sent = self.0.write(report).map_err(|e| Error::Io(e.to_string()))?;
        Ok(report_bytes_sent(report, sent))
    }
    fn read(&mut self, timeout_ms: i32) -> Result<Vec<u8>> {
        let mut buffer = [0u8; 65];
        let n = self.0.read_timeout(&mut buffer, timeout_ms).map_err(|e| Error::Io(e.to_string()))?;
        Ok(buffer[..n].to_vec())
    }
}

/// Starts hidapi on the calling thread. On macOS hidapi starts once per
/// process and ties its HID manager to the run loop of the thread that starts
/// it, so that thread must outlive every later use: the window calls this from
/// its main thread before any run's thread opens the ROM.
pub fn init_hid() -> Result<()> {
    hidapi::HidApi::new().map(drop).map_err(|e| Error::Io(e.to_string()))
}

pub(crate) fn wait_hid(api: &mut hidapi::HidApi, (vid, pid): Id, timeout: Duration) -> Result<HidDevice> {
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

/// What the SDP ROM reports about the plugged unit: the SoC and its silicon
/// revision, and the raw DIGPROG word behind it.
#[derive(Debug)]
pub(crate) struct RomId {
    pub silicon: String,
}

/// Reads the unit's identity through the SDP ROM.
pub(crate) fn rom_silicon(rom: &mut impl Hid) -> Result<RomId> {
    let (digprog, digprog_hab) = sdp::read_word_and_hab(rom, DIGPROG)?;
    let silicon = silicon(digprog);
    log::info!("SoC: {silicon}");
    let rom_hab = match sdp::read_word_and_hab(rom, 0x80) {
        Ok((version, hab)) => {
            log::info!("Boot ROM version: {version:#010x}");
            hab
        }
        Err(error) => {
            log::warn!("Boot ROM version not read: {error}");
            None
        }
    };
    if let Some(state) = rom_hab.or(digprog_hab) {
        log::info!("HAB {state}");
    }
    Ok(RomId { silicon })
}

#[cfg(test)]
#[cfg(test)]
mod hid_write_tests {
    use super::report_bytes_sent;

    #[test]
    fn a_write_padded_to_the_longest_report_counts_as_the_whole_report() {
        assert_eq!(report_bytes_sent(&[0; 17], 1025), 17);
        assert_eq!(report_bytes_sent(&[0; 1025], 1025), 1025);
    }

    #[test]
    fn a_short_write_stays_short() {
        assert_eq!(report_bytes_sent(&[0; 17], 5), 5);
    }
}
