//! The Zigbee Cluster Library as a server: frames, the data types nestlocal
//! uses, and the global commands (read, write, configure reporting,
//! discover) over whatever `Clusters` holds the attributes.

/// ZCL status codes.
pub mod status {
    pub const SUCCESS: u8 = 0x00;
    pub const MALFORMED_COMMAND: u8 = 0x80;
    pub const UNSUP_CLUSTER_COMMAND: u8 = 0x81;
    pub const UNSUP_GENERAL_COMMAND: u8 = 0x82;
    pub const UNSUP_MANUF_CLUSTER_COMMAND: u8 = 0x83;
    pub const UNSUP_MANUF_GENERAL_COMMAND: u8 = 0x84;
    pub const INVALID_FIELD: u8 = 0x85;
    pub const UNSUPPORTED_ATTRIBUTE: u8 = 0x86;
    pub const INVALID_VALUE: u8 = 0x87;
    pub const READ_ONLY: u8 = 0x88;
    pub const INVALID_DATA_TYPE: u8 = 0x8D;
    pub const UNSUPPORTED_CLUSTER: u8 = 0xC3;
}

mod command {
    pub const READ: u8 = 0x00;
    pub const READ_RESPONSE: u8 = 0x01;
    pub const WRITE: u8 = 0x02;
    pub const WRITE_RESPONSE: u8 = 0x04;
    pub const WRITE_NO_RESPONSE: u8 = 0x05;
    pub const CONFIGURE_REPORTING: u8 = 0x06;
    pub const CONFIGURE_REPORTING_RESPONSE: u8 = 0x07;
    pub const REPORT: u8 = 0x0A;
    pub const DEFAULT_RESPONSE: u8 = 0x0B;
    pub const DISCOVER: u8 = 0x0C;
    pub const DISCOVER_RESPONSE: u8 = 0x0D;
}

// Frame control.
const CLUSTER_SPECIFIC: u8 = 0x01;
const MANUFACTURER_SPECIFIC: u8 = 0x04;
const TO_CLIENT: u8 = 0x08;
const NO_DEFAULT_RESPONSE: u8 = 0x10;

/// An attribute's value, as its ZCL data type.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Bitmap8(u8),
    Bitmap16(u16),
    U8(u8),
    U16(u16),
    I8(i8),
    I16(i16),
    Enum8(u8),
    Enum16(u16),
    F32(f32),
    Str(String),
}

impl Value {
    pub fn type_id(&self) -> u8 {
        match self {
            Value::Bool(_) => 0x10,
            Value::Bitmap8(_) => 0x18,
            Value::Bitmap16(_) => 0x19,
            Value::U8(_) => 0x20,
            Value::U16(_) => 0x21,
            Value::I8(_) => 0x28,
            Value::I16(_) => 0x29,
            Value::Enum8(_) => 0x30,
            Value::Enum16(_) => 0x31,
            Value::F32(_) => 0x39,
            Value::Str(_) => 0x42,
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Value::Bool(v) => out.push(u8::from(*v)),
            Value::Bitmap8(v) | Value::U8(v) | Value::Enum8(v) => out.push(*v),
            Value::Bitmap16(v) | Value::U16(v) | Value::Enum16(v) => out.extend(v.to_le_bytes()),
            Value::F32(v) => out.extend(v.to_le_bytes()),
            Value::I8(v) => out.push(*v as u8),
            Value::I16(v) => out.extend(v.to_le_bytes()),
            Value::Str(s) => {
                let bytes = &s.as_bytes()[..s.len().min(254)];
                out.push(bytes.len() as u8);
                out.extend_from_slice(bytes);
            }
        }
    }

    /// A value of type `type_id` off the front of `data`.
    fn read(type_id: u8, data: &mut &[u8]) -> Option<Value> {
        let take = |data: &mut &[u8], n: usize| -> Option<Vec<u8>> {
            let (head, rest) = data.split_at_checked(n)?;
            *data = rest;
            Some(head.to_vec())
        };
        Some(match type_id {
            0x10 => Value::Bool(take(data, 1)?[0] != 0),
            0x18 => Value::Bitmap8(take(data, 1)?[0]),
            0x19 => Value::Bitmap16(u16::from_le_bytes(take(data, 2)?.try_into().ok()?)),
            0x20 => Value::U8(take(data, 1)?[0]),
            0x21 => Value::U16(u16::from_le_bytes(take(data, 2)?.try_into().ok()?)),
            0x28 => Value::I8(take(data, 1)?[0] as i8),
            0x29 => Value::I16(i16::from_le_bytes(take(data, 2)?.try_into().ok()?)),
            0x30 => Value::Enum8(take(data, 1)?[0]),
            0x31 => Value::Enum16(u16::from_le_bytes(take(data, 2)?.try_into().ok()?)),
            0x39 => Value::F32(f32::from_le_bytes(take(data, 4)?.try_into().ok()?)),
            0x42 => {
                let len = take(data, 1)?[0] as usize;
                Value::Str(String::from_utf8_lossy(&take(data, len)?).into_owned())
            }
            _ => return None,
        })
    }
}

/// The attributes and commands of a node's server clusters, by endpoint.
pub trait Clusters {
    /// The endpoint's clusters, and in each its attribute IDs, ascending.
    fn attributes(&self, endpoint: u8, cluster: u16) -> Option<&'static [u16]>;
    /// An attribute's current value; None if it has none right now.
    fn read(&self, endpoint: u8, cluster: u16, attribute: u16) -> Option<Value>;
    /// Writes an attribute, which the caller has checked is one of the
    /// cluster's: a ZCL status.
    fn write(&self, endpoint: u8, cluster: u16, attribute: u16, value: Value) -> u8;
    /// A cluster-specific command: a ZCL status.
    fn command(&self, endpoint: u8, cluster: u16, command: u8, payload: &[u8]) -> u8;
}

/// The answer to a ZCL frame for an endpoint's `cluster`, if one is due.
pub fn serve(clusters: &impl Clusters, endpoint: u8, cluster: u16, frame: &[u8]) -> Option<Vec<u8>> {
    let (&control, rest) = frame.split_first()?;
    if control & TO_CLIENT != 0 {
        return None; // a response or report to us: nothing to answer
    }
    let rest = if control & MANUFACTURER_SPECIFIC != 0 { rest.get(2..)? } else { rest };
    let (&sequence, rest) = rest.split_first()?;
    let (&cmd, payload) = rest.split_first()?;
    let quiet = control & NO_DEFAULT_RESPONSE != 0;
    let default = |status: u8| {
        (!quiet || status != status::SUCCESS)
            .then(|| frame_out(TO_CLIENT | NO_DEFAULT_RESPONSE, sequence, command::DEFAULT_RESPONSE, &[cmd, status]))
    };
    let specific = control & CLUSTER_SPECIFIC != 0;
    if control & MANUFACTURER_SPECIFIC != 0 {
        return default(if specific { status::UNSUP_MANUF_CLUSTER_COMMAND } else { status::UNSUP_MANUF_GENERAL_COMMAND });
    }
    let Some(ids) = clusters.attributes(endpoint, cluster) else {
        return default(status::UNSUPPORTED_CLUSTER);
    };
    if specific {
        return default(clusters.command(endpoint, cluster, cmd, payload));
    }
    let answer = |cmd, payload: &[u8]| Some(frame_out(TO_CLIENT | NO_DEFAULT_RESPONSE, sequence, cmd, payload));
    match cmd {
        command::READ => {
            let mut out = Vec::new();
            for id in payload.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)) {
                out.extend(id.to_le_bytes());
                match ids.contains(&id).then(|| clusters.read(endpoint, cluster, id)).flatten() {
                    Some(value) => {
                        out.extend([status::SUCCESS, value.type_id()]);
                        value.write(&mut out);
                    }
                    None => out.push(status::UNSUPPORTED_ATTRIBUTE),
                }
            }
            answer(command::READ_RESPONSE, &out)
        }
        command::WRITE | command::WRITE_NO_RESPONSE => {
            let mut failed = Vec::new();
            let mut data = payload;
            while data.len() >= 3 {
                let id = u16::from_le_bytes([data[0], data[1]]);
                let type_id = data[2];
                data = &data[3..];
                let Some(value) = Value::read(type_id, &mut data) else {
                    failed.push(status::INVALID_DATA_TYPE);
                    failed.extend(id.to_le_bytes());
                    break; // the rest cannot be found without its length
                };
                let status = match clusters.read(endpoint, cluster, id) {
                    _ if !ids.contains(&id) => status::UNSUPPORTED_ATTRIBUTE,
                    Some(current) if current.type_id() != type_id => status::INVALID_DATA_TYPE,
                    _ => clusters.write(endpoint, cluster, id, value),
                };
                if status != status::SUCCESS {
                    failed.push(status);
                    failed.extend(id.to_le_bytes());
                }
            }
            if cmd == command::WRITE_NO_RESPONSE {
                return None;
            }
            answer(command::WRITE_RESPONSE, if failed.is_empty() { &[status::SUCCESS] } else { &failed })
        }
        // Changes are reported to the coordinator whatever is configured.
        command::CONFIGURE_REPORTING => answer(command::CONFIGURE_REPORTING_RESPONSE, &[status::SUCCESS]),
        command::DISCOVER if payload.len() >= 3 => {
            let start = u16::from_le_bytes([payload[0], payload[1]]);
            let max = payload[2] as usize;
            let found: Vec<(u16, Value)> = ids.iter().filter(|&&id| id >= start)
                .filter_map(|&id| clusters.read(endpoint, cluster, id).map(|v| (id, v))).collect();
            let mut out = vec![u8::from(found.len() <= max)];
            for (id, value) in found.iter().take(max) {
                out.extend(id.to_le_bytes());
                out.push(value.type_id());
            }
            answer(command::DISCOVER_RESPONSE, &out)
        }
        command::DISCOVER => default(status::MALFORMED_COMMAND),
        command::DEFAULT_RESPONSE | command::REPORT => None,
        _ => default(status::UNSUP_GENERAL_COMMAND),
    }
}

/// Whether `frame`, an answer from `serve`, is a default response.
pub fn is_default_response(frame: &[u8]) -> bool {
    frame.get(2) == Some(&command::DEFAULT_RESPONSE)
}

/// An attribute report, server to client.
pub fn report(sequence: u8, attributes: &[(u16, Value)]) -> Vec<u8> {
    let mut payload = Vec::new();
    for (id, value) in attributes {
        payload.extend(id.to_le_bytes());
        payload.push(value.type_id());
        value.write(&mut payload);
    }
    frame_out(TO_CLIENT | NO_DEFAULT_RESPONSE, sequence, command::REPORT, &payload)
}

fn frame_out(control: u8, sequence: u8, cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![control, sequence, cmd];
    frame.extend_from_slice(payload);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// One cluster, 0x0201, with a writable int16 and a read-only enum.
    struct Fake(RefCell<i16>);

    impl Clusters for Fake {
        fn attributes(&self, _endpoint: u8, cluster: u16) -> Option<&'static [u16]> {
            (cluster == 0x0201).then_some(&[0x0000, 0x0012])
        }
        fn read(&self, _endpoint: u8, _cluster: u16, attribute: u16) -> Option<Value> {
            match attribute {
                0x0000 => Some(Value::Enum8(4)),
                0x0012 => Some(Value::I16(*self.0.borrow())),
                _ => None,
            }
        }
        fn write(&self, _endpoint: u8, _cluster: u16, attribute: u16, value: Value) -> u8 {
            match (attribute, value) {
                (0x0012, Value::I16(v)) => {
                    *self.0.borrow_mut() = v;
                    status::SUCCESS
                }
                _ => status::READ_ONLY,
            }
        }
        fn command(&self, _endpoint: u8, _cluster: u16, command: u8, _payload: &[u8]) -> u8 {
            if command == 0 { status::SUCCESS } else { status::UNSUP_CLUSTER_COMMAND }
        }
    }

    #[test]
    fn reads() {
        let f = Fake(RefCell::new(2150));
        // Read 0x0012 and 0x0099 (not there).
        let out = serve(&f, 1, 0x0201, &[0x00, 7, 0x00, 0x12, 0x00, 0x99, 0x00]).unwrap();
        assert_eq!(out, [0x18, 7, 0x01, 0x12, 0x00, 0x00, 0x29, 0x66, 0x08, 0x99, 0x00, 0x86]);
        // Another cluster.
        let out = serve(&f, 1, 0x0006, &[0x00, 8, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(out, [0x18, 8, 0x0B, 0x00, 0xC3]);
    }

    #[test]
    fn writes() {
        let f = Fake(RefCell::new(2150));
        let out = serve(&f, 1, 0x0201, &[0x00, 9, 0x02, 0x12, 0x00, 0x29, 0x34, 0x08]).unwrap();
        assert_eq!(out, [0x18, 9, 0x04, 0x00]);
        assert_eq!(*f.0.borrow(), 2100);
        // Read-only, and the wrong type.
        let out = serve(&f, 1, 0x0201, &[0x00, 10, 0x02, 0x00, 0x00, 0x30, 0x01, 0x12, 0x00, 0x21, 0x00, 0x00]).unwrap();
        assert_eq!(out, [0x18, 10, 0x04, 0x88, 0x00, 0x00, 0x8D, 0x12, 0x00]);
        // No response asked.
        assert!(serve(&f, 1, 0x0201, &[0x00, 11, 0x05, 0x12, 0x00, 0x29, 0x00, 0x08]).is_none());
        assert_eq!(*f.0.borrow(), 2048);
    }

    #[test]
    fn commands_and_the_rest() {
        let f = Fake(RefCell::new(0));
        // A cluster command: a default response, unless it is not wanted and all went well.
        assert_eq!(serve(&f, 1, 0x0201, &[0x01, 3, 0x00, 0x00, 0x05]).unwrap(), [0x18, 3, 0x0B, 0x00, 0x00]);
        assert!(serve(&f, 1, 0x0201, &[0x11, 4, 0x00]).is_none());
        assert_eq!(serve(&f, 1, 0x0201, &[0x11, 5, 0x07]).unwrap(), [0x18, 5, 0x0B, 0x07, 0x81]);
        // Configure reporting, discover.
        assert_eq!(serve(&f, 1, 0x0201, &[0x00, 6, 0x06, 0x00, 0x12, 0x00, 0x29]).unwrap(), [0x18, 6, 0x07, 0x00]);
        assert_eq!(serve(&f, 1, 0x0201, &[0x00, 7, 0x0C, 0x00, 0x00, 0x01]).unwrap(), [0x18, 7, 0x0D, 0x00, 0x00, 0x00, 0x30]);
        // Manufacturer-specific, responses to us.
        assert_eq!(serve(&f, 1, 0x0201, &[0x04, 0x34, 0x12, 8, 0x00]).unwrap(), [0x18, 8, 0x0B, 0x00, 0x84]);
        assert!(serve(&f, 1, 0x0201, &[0x18, 9, 0x0B, 0x00, 0x00]).is_none());
    }

    #[test]
    fn values_both_ways() {
        let values = [
            Value::Bool(true), Value::Bitmap8(0xA5), Value::Bitmap16(0xBEEF), Value::U8(0xFE), Value::U16(0xFFFE),
            Value::I8(-128), Value::I16(-32768), Value::Enum8(4), Value::Enum16(98), Value::F32(-0.5),
            Value::Str(String::new()), Value::Str("Nest".into()),
        ];
        for value in values {
            let mut out = Vec::new();
            value.write(&mut out);
            out.push(0x99); // what follows stays for the next read
            let mut data = &out[..];
            assert_eq!(Value::read(value.type_id(), &mut data), Some(value.clone()), "{value:?}");
            assert_eq!(data, [0x99], "{value:?}");
        }
        // Any nonzero byte is true.
        assert_eq!(Value::read(0x10, &mut &[0x02][..]), Some(Value::Bool(true)));
    }

    #[test]
    fn values_cut_short_or_of_unknown_types_are_not_read() {
        for (type_id, data) in [(0x10, &[][..]), (0x21, &[0x01]), (0x29, &[0x01]), (0x31, &[0x01]),
                                (0x39, &[0x00, 0x00, 0x80]), (0x42, &[]), (0x42, &[5, b'N', b'e', b's', b't']),
                                (0x23, &[0x00, 0x00, 0x00, 0x00])] {
            assert_eq!(Value::read(type_id, &mut &data[..]), None, "type {type_id:#04x}, {data:02x?}");
        }
    }

    #[test]
    fn strings_longer_than_254_bytes_are_cut_at_254() {
        let mut out = Vec::new();
        Value::Str("x".repeat(300)).write(&mut out);
        assert_eq!((out[0], out.len()), (254, 255));
    }

    #[test]
    fn frames_too_short_to_hold_a_command_get_no_answer() {
        let f = Fake(RefCell::new(0));
        for frame in [&[][..], &[0x00], &[0x00, 1], &[0x04, 0x34, 0x12, 1], &[0x04, 0x34]] {
            assert!(serve(&f, 1, 0x0201, frame).is_none(), "{frame:02x?}");
        }
    }

    #[test]
    fn reads_ignore_a_trailing_odd_byte() {
        let f = Fake(RefCell::new(0));
        let out = serve(&f, 1, 0x0201, &[0x00, 7, 0x00, 0x00, 0x00, 0x12]).unwrap();
        assert_eq!(out, [0x18, 7, 0x01, 0x00, 0x00, 0x00, 0x30, 0x04]);
        // Nothing asked: an empty response.
        assert_eq!(serve(&f, 1, 0x0201, &[0x00, 8, 0x00]).unwrap(), [0x18, 8, 0x01]);
    }

    #[test]
    fn writes_of_unknown_or_cut_short_types_stop_there() {
        let f = Fake(RefCell::new(0));
        // An unknown type before a good record: the good one cannot be found.
        let out = serve(&f, 1, 0x0201, &[0x00, 1, 0x02, 0x00, 0x00, 0xE0, 0x01, 0x12, 0x00, 0x29, 0x34, 0x08]).unwrap();
        assert_eq!(out, [0x18, 1, 0x04, 0x8D, 0x00, 0x00]);
        assert_eq!(*f.0.borrow(), 0);
        // A good record, then an int16 with one byte.
        let out = serve(&f, 1, 0x0201, &[0x00, 2, 0x02, 0x12, 0x00, 0x29, 0x34, 0x08, 0x12, 0x00, 0x29, 0x01]).unwrap();
        assert_eq!(out, [0x18, 2, 0x04, 0x8D, 0x12, 0x00]);
        assert_eq!(*f.0.borrow(), 2100);
        // An attribute the cluster does not have, and a record header cut short, which is ignored.
        let out = serve(&f, 1, 0x0201, &[0x00, 3, 0x02, 0x99, 0x00, 0x29, 0x00, 0x00, 0x12, 0x00]).unwrap();
        assert_eq!(out, [0x18, 3, 0x04, 0x86, 0x99, 0x00]);
    }

    #[test]
    fn discovery_says_whether_it_listed_everything() {
        let f = Fake(RefCell::new(0));
        // From 0x0000, one at most: there is more.
        assert_eq!(serve(&f, 1, 0x0201, &[0x00, 1, 0x0C, 0x00, 0x00, 1]).unwrap(), [0x18, 1, 0x0D, 0x00, 0x00, 0x00, 0x30]);
        // From 0x0001, both fit: the rest of the list, complete.
        assert_eq!(serve(&f, 1, 0x0201, &[0x00, 2, 0x0C, 0x01, 0x00, 5]).unwrap(), [0x18, 2, 0x0D, 0x01, 0x12, 0x00, 0x29]);
        // Past the last attribute: complete and empty.
        assert_eq!(serve(&f, 1, 0x0201, &[0x00, 3, 0x0C, 0x13, 0x00, 5]).unwrap(), [0x18, 3, 0x0D, 0x01]);
    }

    #[test]
    fn a_discovery_cut_short_is_malformed() {
        let f = Fake(RefCell::new(0));
        for (sequence, frame) in [(1, &[0x00, 1, 0x0C][..]), (2, &[0x00, 2, 0x0C, 0x00, 0x00])] {
            assert_eq!(serve(&f, 1, 0x0201, frame).unwrap(), [0x18, sequence, 0x0B, 0x0C, 0x80], "{frame:02x?}");
        }
    }

    #[test]
    fn unknown_global_commands_are_refused_even_when_no_response_is_asked() {
        let f = Fake(RefCell::new(0));
        assert_eq!(serve(&f, 1, 0x0201, &[0x10, 4, 0x40]).unwrap(), [0x18, 4, 0x0B, 0x40, 0x82]);
        // A manufacturer-specific cluster command.
        assert_eq!(serve(&f, 1, 0x0201, &[0x05, 0x34, 0x12, 5, 0x00]).unwrap(), [0x18, 5, 0x0B, 0x00, 0x83]);
        // Reports and default responses sent to the server get none.
        assert!(serve(&f, 1, 0x0201, &[0x00, 6, 0x0A, 0x00, 0x00, 0x30, 0x04]).is_none());
        assert!(serve(&f, 1, 0x0201, &[0x00, 7, 0x0B, 0x00, 0x00]).is_none());
    }

    #[test]
    fn reports() {
        assert_eq!(report(1, &[(0x0000, Value::I16(1800)), (0x001C, Value::Enum8(4))]),
                   [0x18, 1, 0x0A, 0x00, 0x00, 0x29, 0x08, 0x07, 0x1C, 0x00, 0x30, 0x04]);
        assert_eq!(report(2, &[(0x0005, Value::Str("Nest".into()))]),
                   [0x18, 2, 0x0A, 0x05, 0x00, 0x42, 4, b'N', b'e', b's', b't']);
        assert_eq!(report(3, &[(0x0055, Value::F32(30.0)), (0x0000, Value::Bool(true))]),
                   [0x18, 3, 0x0A, 0x55, 0x00, 0x39, 0x00, 0x00, 0xF0, 0x41, 0x00, 0x00, 0x10, 0x01]);
    }
}
