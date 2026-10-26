//! EZSP, protocol 7 (EmberZNet 6.6): the commands nestlocal sends the radio
//! and the callbacks it reads back, over the SPI protocol.
//!
//! A frame is a sequence number, a frame control byte and a frame ID, then
//! the parameters, little-endian; in the extended form the frame ID is 0xFF,
//! then come an extended control byte and the real ID. With SPI
//! nothing comes unasked: callbacks are read with the `callback` command
//! while nHOST_INT says some are waiting.

use std::time::Duration;

use super::spi::Spi;

pub mod id {
    pub const VERSION: u8 = 0x00;
    pub const ADD_ENDPOINT: u8 = 0x02;
    pub const CALLBACK: u8 = 0x06;
    pub const GET_XNCP_INFO: u8 = 0x13;
    pub const NO_CALLBACKS: u8 = 0x07;
    pub const NETWORK_INIT: u8 = 0x17;
    pub const STACK_STATUS_HANDLER: u8 = 0x19;
    pub const START_SCAN: u8 = 0x1A;
    pub const NETWORK_FOUND_HANDLER: u8 = 0x1B;
    pub const SCAN_COMPLETE_HANDLER: u8 = 0x1C;
    pub const JOIN_NETWORK: u8 = 0x1F;
    pub const LEAVE_NETWORK: u8 = 0x20;
    pub const GET_EUI64: u8 = 0x26;
    pub const GET_NODE_ID: u8 = 0x27;
    pub const GET_NETWORK_PARAMETERS: u8 = 0x28;
    pub const SEND_UNICAST: u8 = 0x34;
    pub const INCOMING_MESSAGE_HANDLER: u8 = 0x45;
    pub const SET_CONFIGURATION_VALUE: u8 = 0x53;
    pub const SET_POLICY: u8 = 0x55;
    pub const INVALID_COMMAND: u8 = 0x58;
    pub const SET_INITIAL_SECURITY_STATE: u8 = 0x68;
    pub const GET_KEY: u8 = 0x6A;
    pub const SET_VALUE: u8 = 0xAB;
}

pub const PROTOCOL: u8 = 7;
const EXTENDED: u8 = 0xFF;

/// EmberStatus values nestlocal acts on.
pub mod status {
    pub const SUCCESS: u8 = 0x00;
    pub const NETWORK_UP: u8 = 0x90;
    pub const NETWORK_DOWN: u8 = 0x91;
    pub const NOT_JOINED: u8 = 0x93;
    pub const MAX_MESSAGE_LIMIT_REACHED: u8 = 0x72;
    pub const NO_BUFFERS: u8 = 0x18;
}

pub struct Ezsp {
    spi: Spi,
    sequence: u8,
}

/// A callback the radio sent: its frame ID and parameters.
pub struct Callback {
    pub id: u8,
    pub params: Vec<u8>,
}

impl Ezsp {
    /// Resets the radio, selects its Zigbee personality and agrees on the
    /// protocol: the first EZSP command after a reset must be `version`.
    pub fn start() -> Result<(Ezsp, u16), String> {
        let mut spi = Spi::open()?;
        spi.reset().map_err(|e| format!("resetting the radio: {e}"))?;
        // An image from before the selection boots straight into EZSP and
        // refuses the frame; it still answers `version`, which names it.
        if crate::device::current().radio.selects_zigbee {
            if let Err(e) = spi.select_zigbee() {
                crate::logf!("zigbee: selecting Zigbee: {e}");
            }
        }
        let mut ezsp = Ezsp { spi, sequence: 0 };
        let answer = ezsp.command(id::VERSION, &[PROTOCOL]).map_err(|e| format!("EZSP version: {e}"))?;
        let mut r = Reader(&answer);
        let (protocol, _stack_type, stack) = (r.u8()?, r.u8()?, r.u16()?);
        if protocol != PROTOCOL {
            return Err(format!("the radio speaks EZSP {protocol}, not {PROTOCOL}"));
        }
        Ok((ezsp, stack))
    }

    /// Sends a command and returns its answer's parameters.
    pub fn command(&mut self, frame_id: u8, params: &[u8]) -> Result<Vec<u8>, String> {
        let (answered, params) = self.exchange(frame_id, params)?;
        if answered != frame_id {
            return Err(format!("EZSP {frame_id:#04x} answered with {answered:#04x} {params:02x?}"));
        }
        Ok(params)
    }

    /// A command whose answer is a single status, which must be success.
    pub fn expect(&mut self, frame_id: u8, params: &[u8]) -> Result<(), String> {
        match self.command(frame_id, params)?.first() {
            Some(&status::SUCCESS) => Ok(()),
            Some(s) => Err(format!("EZSP {frame_id:#04x}: status {s:#04x}")),
            None => Err(format!("EZSP {frame_id:#04x}: empty answer")),
        }
    }

    /// The next waiting callback, or None.
    pub fn callback(&mut self) -> Result<Option<Callback>, String> {
        if !self.spi.host_int() {
            return Ok(None);
        }
        let (frame_id, params) = self.exchange(id::CALLBACK, &[])?;
        Ok((frame_id != id::NO_CALLBACKS).then_some(Callback { id: frame_id, params }))
    }

    /// Waits up to `timeout` for the radio to have something to say.
    pub fn wait(&mut self, timeout: Duration) -> bool {
        self.spi.wait_host_int(timeout)
    }

    fn exchange(&mut self, frame_id: u8, params: &[u8]) -> Result<(u8, Vec<u8>), String> {
        self.sequence = self.sequence.wrapping_add(1);
        // version in the legacy form, the rest extended: the radio answers
        // all but version extended, and builds that answer right only for
        // an extended command.
        let mut frame = match frame_id {
            id::VERSION => vec![self.sequence, 0x00, frame_id],
            _ => vec![self.sequence, 0x00, EXTENDED, 0x00, frame_id],
        };
        frame.extend_from_slice(params);
        let answer = self.spi.ezsp(&frame)?;
        let (answered, params) = parse(&answer)?;
        if answered == id::INVALID_COMMAND {
            return Err(format!("the radio refused EZSP {frame_id:#04x} (status {params:02x?})"));
        }
        Ok((answered, params.to_vec()))
    }
}

/// A frame from the radio: its frame ID and parameters.
pub fn parse(frame: &[u8]) -> Result<(u8, &[u8]), String> {
    match frame {
        [_, _, EXTENDED, _, frame_id, params @ ..] => Ok((*frame_id, params)),
        [_, _, frame_id, params @ ..] if *frame_id != EXTENDED => Ok((*frame_id, params)),
        _ => Err(format!("short EZSP frame {frame:02x?}")),
    }
}

/// Reads parameters in order, little-endian.
pub struct Reader<'a>(pub &'a [u8]);

impl<'a> Reader<'a> {
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.0.len() < n {
            return Err(format!("EZSP parameters end early ({} of {n} bytes)", self.0.len()));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

}

/// EmberApsFrame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Aps {
    pub profile: u16,
    pub cluster: u16,
    pub source_endpoint: u8,
    pub destination_endpoint: u8,
    pub options: u16,
    pub group: u16,
    pub sequence: u8,
}

impl Aps {
    pub fn read(r: &mut Reader) -> Result<Aps, String> {
        Ok(Aps {
            profile: r.u16()?,
            cluster: r.u16()?,
            source_endpoint: r.u8()?,
            destination_endpoint: r.u8()?,
            options: r.u16()?,
            group: r.u16()?,
            sequence: r.u8()?,
        })
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend(self.profile.to_le_bytes());
        out.extend(self.cluster.to_le_bytes());
        out.extend([self.source_endpoint, self.destination_endpoint]);
        out.extend(self.options.to_le_bytes());
        out.extend(self.group.to_le_bytes());
        out.push(self.sequence);
    }
}

/// A network found by an active scan (EmberZigbeeNetwork).
#[derive(Debug, PartialEq)]
pub struct Network {
    pub channel: u8,
    pub pan_id: u16,
    pub extended_pan_id: [u8; 8],
    pub allowing_join: bool,
    pub stack_profile: u8,
    pub update_id: u8,
}

impl Network {
    pub fn read(r: &mut Reader) -> Result<Network, String> {
        Ok(Network {
            channel: r.u8()?,
            pan_id: r.u16()?,
            extended_pan_id: r.bytes(8)?.try_into().unwrap(),
            allowing_join: r.u8()? != 0,
            stack_profile: r.u8()?,
            update_id: r.u8()?,
        })
    }
}

/// incomingMessageHandler's parameters.
#[derive(Debug, PartialEq)]
pub struct Incoming {
    pub kind: u8,
    pub aps: Aps,
    pub sender: u16,
    pub payload: Vec<u8>,
}

impl Incoming {
    pub fn read(params: &[u8]) -> Result<Incoming, String> {
        let mut r = Reader(params);
        let kind = r.u8()?;
        let aps = Aps::read(&mut r)?;
        let (_lqi, _rssi) = (r.u8()?, r.u8()?);
        let sender = r.u16()?;
        let (_binding, _address) = (r.u8()?, r.u8()?);
        let len = r.u8()? as usize;
        Ok(Incoming { kind, aps, sender, payload: r.bytes(len)?.to_vec() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_in_both_forms() {
        assert_eq!(parse(&[1, 0x80, 0x26, 0x46, 0xb7]).unwrap(), (0x26, &[0x46, 0xb7][..]));
        assert_eq!(parse(&[1, 0x80, 0xFF, 0x00, 0x26, 0x46]).unwrap(), (0x26, &[0x46][..]));
        assert!(parse(&[1, 0x80]).is_err());
        // Extended, with no parameters.
        assert_eq!(parse(&[1, 0x90, 0xFF, 0x00, 0x07]).unwrap(), (0x07, &[][..]));
    }

    #[test]
    fn an_extended_frame_cut_short_is_not_read_as_a_legacy_one() {
        assert!(parse(&[1, 0x80, 0xFF]).is_err());
        assert!(parse(&[1, 0x80, 0xFF, 0x00]).is_err());
    }

    #[test]
    fn parameters_in_order_and_not_past_their_end() {
        let mut r = Reader(&[0x01, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12, 0xAA]);
        assert_eq!((r.u8().unwrap(), r.u16().unwrap(), r.u32().unwrap()), (0x01, 0x1234, 0x1234_5678));
        assert!(r.u16().is_err());
        // A failed read takes nothing.
        assert_eq!(r.u8().unwrap(), 0xAA);
        assert!(r.u8().is_err());
    }

    #[test]
    fn a_network_found_by_a_scan() {
        // networkFoundHandler: the network, then the last hop's LQI and RSSI.
        let params = [
            25, 0x62, 0x1a, 0xdd, 0xdd, 0xdd, 0xdd, 0xdd, 0xdd, 0xdd, 0x01, // channel, PAN ID, extended PAN ID
            0x01, 0x02, 0x00, // allowing join, stack profile, update ID
            0xd8, 0xc4, // LQI, RSSI
        ];
        let mut r = Reader(&params);
        assert_eq!(Network::read(&mut r).unwrap(), Network {
            channel: 25, pan_id: 0x1a62, extended_pan_id: [0xdd, 0xdd, 0xdd, 0xdd, 0xdd, 0xdd, 0xdd, 0x01],
            allowing_join: true, stack_profile: 2, update_id: 0,
        });
        assert_eq!(r.u8().unwrap(), 0xd8);
        assert!(Network::read(&mut Reader(&params[..13])).is_err());
    }

    #[test]
    fn an_incoming_message_shorter_than_its_length_says_is_refused() {
        let mut params = vec![0x00, 0x04, 0x01, 0x00, 0x00, 0x01, 0x01, 0x40, 0x00, 0x00, 0x00, 0x2a,
                              0xff, 0xd0, 0x00, 0x00, 0xff, 0xff, 0x05, 0x00, 0x07, 0x00, 0x05];
        assert!(Incoming::read(&params).is_err());
        // Bytes past the payload are not part of it.
        params.extend([0x00, 0xEE]);
        assert_eq!(Incoming::read(&params).unwrap().payload, [0x00, 0x07, 0x00, 0x05, 0x00]);
        assert!(Incoming::read(&params[..11]).is_err());
    }

    #[test]
    fn an_incoming_message() {
        // A read of the Basic cluster's model ID from the coordinator.
        let params = [
            0x00, // unicast
            0x04, 0x01, 0x00, 0x00, 0x01, 0x01, 0x40, 0x00, 0x00, 0x00, 0x2a, // APS frame
            0xff, 0xd0, 0x00, 0x00, 0xff, 0xff, // LQI, RSSI, sender 0x0000, binding, address
            0x05, 0x00, 0x07, 0x00, 0x05, 0x00, // ZCL: read attribute 0x0005
        ];
        let m = Incoming::read(&params).unwrap();
        assert_eq!(m.aps, Aps { profile: 0x0104, cluster: 0, source_endpoint: 1, destination_endpoint: 1,
                                options: 0x0040, group: 0, sequence: 0x2a });
        assert_eq!((m.sender, m.payload), (0x0000, vec![0x00, 0x07, 0x00, 0x05, 0x00]));
        let mut out = Vec::new();
        m.aps.write(&mut out);
        assert_eq!(out, params[1..12]);
    }
}
