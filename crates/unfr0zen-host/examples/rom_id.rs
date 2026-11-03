//! One-off bench check: read a plugged unit's identity over SDP. Read-only.

use unfr0zen_core::diamond3::recovery::{silicon, DIGPROG};
use unfr0zen_core::diamond3::sdp::{self, Hid};
use unfr0zen_core::{Error, Result};

struct Dev(hidapi::HidDevice);

impl Hid for Dev {
    fn write(&mut self, report: &[u8]) -> Result<usize> {
        self.0.write(report).map_err(|e| Error::Io(e.to_string()))
    }
    fn read(&mut self, timeout_ms: i32) -> Result<Vec<u8>> {
        let mut buffer = [0u8; 65];
        let n = self.0.read_timeout(&mut buffer, timeout_ms).map_err(|e| Error::Io(e.to_string()))?;
        Ok(buffer[..n].to_vec())
    }
}

fn main() -> Result<()> {
    let api = hidapi::HidApi::new().map_err(|e| Error::Io(e.to_string()))?;
    let device = Dev(api.open(0x15A2, 0x0071).map_err(|e| Error::Io(e.to_string()))?);
    let mut device = device;
    let (digprog, hab) = sdp::read_word_and_hab(&mut device, DIGPROG)?;
    println!("{} (DIGPROG {digprog:#010x}), HAB at read: {hab:?}", silicon(digprog));
    let (rom, _) = sdp::read_word_and_hab(&mut device, 0x80)?;
    println!("Boot ROM version: {rom:#010x}");
    Ok(())
}
