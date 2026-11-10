//! The DA14580 as nlclient sees it, without the chip: the boot ROM's firmware
//! upload, which it takes and throws away, then the GTL commands of Nest's
//! application firmware, each answered as that firmware does.

use crate::logf;

const STX: u8 = 0x02;
const SOH: u8 = 0x01;
const ACK: u8 = 0x06;
/// A GTL message: 0x05, the application's message id, destination, source.
const TO_CHIP: [u8; 7] = [5, 1, 0xc8, 0x32, 0, 0x3f, 0];
const TO_HOST: [u8; 7] = [5, 1, 0xc8, 0x3f, 0, 0x32, 0];
const HEADER: usize = 9;
const MAX_PAYLOAD: usize = 1024;
/// How often the ROM offers to take firmware, ms. The loader reads for STX in
/// this order of time, and anything else that reaches its GTL reader makes it
/// start over: the ROM speaks only after a reset.
const STX_EVERY_MS: u64 = 100;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod op {
    pub const CONFIGURE_CENTRAL: u16 = 0x01;
    pub const CONFIGURE_PERIPHERAL: u16 = 0x02;
    pub const CONFIGURED: u16 = 0x04;
    pub const ACTIVATE: u16 = 0x09;
    pub const DEACTIVATE: u16 = 0x0a;
    pub const ACTIVATED: u16 = 0x0b;
    pub const DEACTIVATED: u16 = 0x0c;
    pub const ADVERTISEMENT: u16 = 0x11;
    pub const UNRECOGNIZED: u16 = 0x29;
    pub const XTRIM_SET: u16 = 0x30;
    pub const XTRIM_DONE: u16 = 0x31;
    pub const SENSOR_FILTER: u16 = 0x3c;
    pub const SCAN_PARAMETERS: u16 = 0x3d;
}

const ROLE_CENTRAL: u8 = 0;
const ROLE_PERIPHERAL: u8 = 1;
const SUCCESS: u8 = 1;

enum Mode {
    /// Out of reset: offering to take firmware.
    Rom { last_stx: Option<u64> },
    /// Taking `need` bytes.
    Image { need: usize, got: usize, xor: u8 },
    /// Running: nlclient's ACK of the checksum may come first.
    Firmware,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct Chip {
    mode: Mode,
    input: Vec<u8>,
    role: Option<u8>,
    active: bool,
    /// The sensors nlclient scans for: the low 16 bits of their ids.
    filter: Vec<u16>,
}

fn frame(op: u16, body: &[u8]) -> Vec<u8> {
    let mut out = TO_HOST.to_vec();
    out.extend_from_slice(&((body.len() + 2) as u16).to_le_bytes());
    out.extend_from_slice(&op.to_le_bytes());
    out.extend_from_slice(body);
    out
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl Chip {
    pub fn new() -> Chip {
        Chip { mode: Mode::Firmware, input: Vec::new(), role: None, active: false, filter: Vec::new() }
    }

    /// nlclient pulsed the reset line.
    pub fn reset(&mut self) {
        self.mode = Mode::Rom { last_stx: None };
        self.input.clear();
        self.role = None;
        self.active = false;
        self.filter.clear();
    }

    /// Whether `tick` wants calling soon, and not only when something happens.
    pub fn in_rom(&self) -> bool {
        matches!(self.mode, Mode::Rom { .. })
    }

    /// What the chip sends on its own at this time (ms, from any start).
    pub fn tick(&mut self, now_ms: u64) -> Vec<u8> {
        if let Mode::Rom { last_stx } = &mut self.mode {
            if last_stx.is_none_or(|last| now_ms.saturating_sub(last) >= STX_EVERY_MS) {
                *last_stx = Some(now_ms);
                return vec![STX];
            }
        }
        Vec::new()
    }

    /// Whether the firmware is scanning for this sensor: a beacon now reaches
    /// nlclient, and only then.
    pub fn listening(&self, id: u64) -> bool {
        self.active && self.role == Some(ROLE_CENTRAL) && self.filter.contains(&(id as u16))
    }

    /// The advertisement report as the chip delivers it.
    pub fn advertisement(report: &[u8]) -> Vec<u8> {
        frame(op::ADVERTISEMENT, report)
    }

    /// Takes bytes from nlclient, returning what the chip answers.
    pub fn received(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.input.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            match self.mode {
                Mode::Rom { .. } => {
                    let Some(&[first, low, high]) = self.input.get(..3).map(|b| <&[u8; 3]>::try_from(b).unwrap()) else { break };
                    if first != SOH {
                        self.input.remove(0);
                        continue;
                    }
                    self.input.drain(..3);
                    self.mode = Mode::Image { need: usize::from(u16::from_le_bytes([low, high])), got: 0, xor: 0 };
                    out.push(ACK);
                }
                Mode::Image { need, ref mut got, ref mut xor } => {
                    let take = self.input.len().min(need - *got);
                    for byte in self.input.drain(..take) {
                        *xor ^= byte;
                    }
                    *got += take;
                    if *got < need {
                        break;
                    }
                    out.push(*xor);
                    logf!("BLE stand-in: nlclient loaded {need} bytes of BLE firmware");
                    self.mode = Mode::Firmware;
                }
                Mode::Firmware => {
                    let Some(frame) = self.next_frame() else { break };
                    out.extend(self.command(frame.0, &frame.1));
                }
            }
        }
        out
    }

    /// The next whole GTL message, past anything else (the loader's ACK).
    fn next_frame(&mut self) -> Option<(u16, Vec<u8>)> {
        loop {
            match self.input.iter().position(|&b| b == TO_CHIP[0]) {
                Some(start) => { self.input.drain(..start); }
                None => {
                    self.input.clear();
                    return None;
                }
            }
            if self.input.len() < HEADER {
                return None;
            }
            let len = usize::from(u16::from_le_bytes([self.input[7], self.input[8]]));
            if self.input[..7] != TO_CHIP || !(2..=MAX_PAYLOAD).contains(&len) {
                self.input.remove(0);
                continue;
            }
            if self.input.len() < HEADER + len {
                return None;
            }
            let op = u16::from_le_bytes([self.input[9], self.input[10]]);
            let body = self.input[HEADER + 2..HEADER + len].to_vec();
            self.input.drain(..HEADER + len);
            return Some((op, body));
        }
    }

    fn command(&mut self, command: u16, body: &[u8]) -> Vec<u8> {
        match command {
            op::XTRIM_SET => frame(op::XTRIM_DONE, &[0]),
            op::CONFIGURE_CENTRAL => {
                logf!("external temperature: nlclient scans for sensors");
                self.role = Some(ROLE_CENTRAL);
                frame(op::CONFIGURED, &[ROLE_CENTRAL, SUCCESS])
            }
            op::CONFIGURE_PERIPHERAL => {
                logf!("external temperature: nlclient advertises");
                self.role = Some(ROLE_PERIPHERAL);
                frame(op::CONFIGURED, &[ROLE_PERIPHERAL, SUCCESS])
            }
            op::ACTIVATE => {
                self.active = true;
                frame(op::ACTIVATED, &[SUCCESS])
            }
            op::DEACTIVATE => {
                self.active = false;
                frame(op::DEACTIVATED, &[SUCCESS])
            }
            // A count and the sensors' ids, answered by nothing.
            op::SENSOR_FILTER => {
                self.filter = body.get(1..).unwrap_or_default().as_chunks::<2>().0.iter()
                    .map(|id| u16::from_le_bytes(*id)).collect();
                Vec::new()
            }
            op::SCAN_PARAMETERS => Vec::new(),
            other => {
                logf!("BLE stand-in: nlclient sent BLE command {other:#04x} ({} bytes), which it does not need here", body.len());
                frame(op::UNRECOGNIZED, &[other as u8])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(op: u16, body: &[u8]) -> Vec<u8> {
        let mut out = TO_CHIP.to_vec();
        out.extend_from_slice(&((body.len() + 2) as u16).to_le_bytes());
        out.extend_from_slice(&op.to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn started() -> Chip {
        let mut chip = Chip::new();
        chip.reset();
        assert_eq!(chip.tick(0), [STX]);
        assert_eq!(chip.received(&[SOH, 4, 0]), [ACK]);
        assert_eq!(chip.received(&[1, 2, 4, 8]), [15]);
        chip
    }

    #[test]
    fn speaks_only_after_a_reset() {
        let mut chip = Chip::new();
        assert!(chip.tick(1000).is_empty());
        chip.reset();
        assert_eq!(chip.tick(1000), [STX]);
        assert!(chip.tick(1050).is_empty());
        assert_eq!(chip.tick(1100), [STX]);
        chip.received(&[SOH, 1, 0]);
        assert!(chip.tick(2000).is_empty());
    }

    #[test]
    fn takes_an_upload_in_any_pieces_and_ignores_stray_bytes_before_it() {
        let mut chip = Chip::new();
        chip.reset();
        assert!(chip.received(&[0xff, SOH]).is_empty());
        assert_eq!(chip.received(&[2, 0, 0x0f]), [ACK]);
        assert_eq!(chip.received(&[0xf0]), [0xff]);
        assert!(!chip.in_rom());
    }

    #[test]
    fn answers_the_handshake_nlclient_runs_for_a_sensor() {
        let mut chip = started();
        let id = 0x18B4_3000_00A1_B2C3;
        assert_eq!(chip.received(&command(op::XTRIM_SET, &[0x86, 0x04])), frame(op::XTRIM_DONE, &[0]));
        assert_eq!(chip.received(&command(op::CONFIGURE_CENTRAL, &[0; 21])), frame(op::CONFIGURED, &[0, 1]));
        assert!(!chip.listening(id));
        assert_eq!(chip.received(&command(op::ACTIVATE, &[])), frame(op::ACTIVATED, &[1]));
        assert!(!chip.listening(id), "no sensor in the filter yet");
        assert!(chip.received(&command(op::SENSOR_FILTER, &[1, 0xc3, 0xb2])).is_empty());
        assert!(chip.received(&command(op::SCAN_PARAMETERS, &[4, 0x29, 0x2c, 1])).is_empty());
        assert!(chip.listening(id));
        assert!(!chip.listening(0x1234));
        assert_eq!(chip.received(&command(op::DEACTIVATE, &[])), frame(op::DEACTIVATED, &[1]));
        assert!(!chip.listening(id));
    }

    #[test]
    fn a_peripheral_does_not_listen_and_a_reset_forgets_everything() {
        let mut chip = started();
        let id = 0xB2C3;
        assert_eq!(chip.received(&command(op::CONFIGURE_PERIPHERAL, &[0; 86])), frame(op::CONFIGURED, &[1, 1]));
        chip.received(&command(op::ACTIVATE, &[]));
        chip.received(&command(op::SENSOR_FILTER, &[1, 0xc3, 0xb2]));
        assert!(!chip.listening(id));
        chip.received(&command(op::CONFIGURE_CENTRAL, &[0; 21]));
        assert!(chip.listening(id));
        chip.reset();
        assert!(!chip.listening(id));
    }

    #[test]
    fn the_loaders_ack_and_split_frames_reach_the_firmware_whole() {
        let mut chip = started();
        let bytes = [&[ACK][..], &command(op::CONFIGURE_CENTRAL, &[0; 21])].concat();
        let (first, rest) = bytes.split_at(5);
        assert!(chip.received(first).is_empty());
        assert_eq!(chip.received(rest), frame(op::CONFIGURED, &[0, 1]));
    }

    #[test]
    fn an_unknown_command_is_refused_as_the_firmware_refuses_it() {
        let mut chip = started();
        assert_eq!(chip.received(&command(0x2e, &[1, 2])), frame(op::UNRECOGNIZED, &[0x2e]));
    }
}
