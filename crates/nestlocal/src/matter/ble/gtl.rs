//! DA14580 Nest peripheral protocol, carried in GTL application messages.
use std::io;

pub const MAX_VALUE: usize = 128;
const HEADER: usize = 9;
const MAX_PAYLOAD: usize = 256;

pub fn frame(op: u16, body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![5, 1, 0xc8, 0x32, 0, 0x3f, 0];
    bytes.extend_from_slice(&((body.len() + 2) as u16).to_le_bytes());
    bytes.extend_from_slice(&op.to_le_bytes());
    bytes.extend_from_slice(body);
    bytes
}

#[derive(Default)]
pub struct Decoder(Vec<u8>);

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.0.len() + bytes.len() > 4096 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "GTL receive buffer overflow"));
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }

    pub fn next(&mut self) -> io::Result<Option<(u16, Vec<u8>)>> {
        while self.0.len() >= HEADER {
            if self.0[..7] != [5, 1, 0xc8, 0x3f, 0, 0x32, 0] {
                self.0.remove(0);
                continue;
            }
            let len = u16::from_le_bytes([self.0[7], self.0[8]]) as usize;
            if !(2..=MAX_PAYLOAD).contains(&len) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid GTL payload length"));
            }
            if self.0.len() < HEADER + len { return Ok(None); }
            let op = u16::from_le_bytes([self.0[9], self.0[10]]);
            let body = self.0[11..HEADER + len].to_vec();
            self.0.drain(..HEADER + len);
            return Ok(Some((op, body)));
        }
        Ok(None)
    }
}

/// GAPM inserts Flags itself; a Flags record in this buffer prevents advertising.
pub fn configuration(service_data: &[u8], serial: &str) -> io::Result<[u8; 86]> {
    if service_data.is_empty() || service_data.len() > 16 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid Matter advertising data"));
    }
    if serial.is_empty() || serial.len() > 16 || !serial.is_ascii() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid BLE serial number"));
    }
    let mut body = [0; 86];
    body[..2].copy_from_slice(&100u16.to_le_bytes()); // 62.5 ms
    body[8] = service_data.len() as u8;
    body[9..9 + service_data.len()].copy_from_slice(service_data);
    body[37] = serial.len() as u8;
    body[38..38 + serial.len()].copy_from_slice(serial.as_bytes());
    body[54] = (6 + serial.len()) as u8;
    body[55..59].copy_from_slice(&[3, 3, 0xf6, 0xff]);
    body[59] = (1 + serial.len()) as u8;
    body[60] = 9; // Complete Local Name
    body[61..61 + serial.len()].copy_from_slice(serial.as_bytes());
    Ok(body)
}

/// Patch the supported image's service and connection states in RAM.
/// The unit supplies the firmware; its original image remains unchanged.
pub fn firmware(stock: &[u8]) -> io::Result<Vec<u8>> {
    use md5::{Digest, Md5};
    const HASH: [u8; 16] = [0x81, 0xc6, 0x54, 0x30, 0x97, 0x9e, 0x37, 0x11,
        0x25, 0xd8, 0xa1, 0xa1, 0x46, 0xe8, 0xe2, 0x9e];
    const ANCHOR: [u8; 18] = [0xaf, 0xfe, 0xb7, 0x9d, 0x9f, 0x42, 0x9c, 0x4f,
        0x9f, 0x95, 0x59, 0x45, 0x3d, 0x26, 0xf5, 0x2e, 0xee, 0x19];
    if Md5::digest(stock).as_slice() != HASH || stock.get(0x6c54..0x6c66) != Some(&ANCHOR) {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "unsupported DA14580 peripheral firmware"));
    }
    let mut patched = stock.to_vec();
    // Keep the full name in the scan response; GAPM's advertising name is truncated.
    patched[0x3e56] = 0; // movs r1, #0: no space allotted to the advertising name
    patched[0x6c54..0x6c56].copy_from_slice(&[0xf6, 0xff]);
    // Matter commissioners subscribe before writing their BTP handshake.
    // Keep the pre-handshake states when CCCD changes, preserving the profile's flags.
    for (offset, state) in [(0x7778, 20), (0x7780, 20), (0x77c8, 21), (0x77d0, 21),
        (0x7868, 23), (0x7870, 23)] {
        patched[offset..offset + 4].copy_from_slice(&0x20005f43u32.to_le_bytes());
        patched[offset + 5] = state;
    }
    Ok(patched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_responses_survive_every_uart_split() {
        let bytes = [5,1,0xc8,0x3f,0,0x32,0,2,0,7,0,
            5,1,0xc8,0x3f,0,0x32,0,4,0,4,0,1,1];
        for split in 0..=bytes.len() {
            let mut decoder = Decoder::default();
            decoder.push(&[0xff, 0]).unwrap();
            decoder.push(&bytes[..split]).unwrap();
            let mut messages = Vec::new();
            while let Some(message) = decoder.next().unwrap() { messages.push(message); }
            decoder.push(&bytes[split..]).unwrap();
            while let Some(message) = decoder.next().unwrap() { messages.push(message); }
            assert_eq!(messages, vec![(7, vec![]), (4, vec![1,1])]);
        }
    }

    #[test]
    fn malformed_length_cannot_allocate_or_wait_forever() {
        let mut decoder = Decoder::default();
        decoder.push(&[5,1,0xc8,0x3f,0,0x32,0,0xff,0xff]).unwrap();
        assert!(decoder.next().is_err());
        assert!(firmware(&[0; 31088]).is_err());
    }

    #[test]
    fn advertising_uses_stack_flags_and_nonempty_gap_fields() {
        use rs_matter::transport::network::btp::AdvData;
        let info = rs_matter::dm::clusters::basic_info::BasicInfoConfig::new();
        let adv = AdvData::new(&info, 3840);
        let service: Vec<_> = adv.service_iter().collect();
        let serial = "09AA01AC25160NMZ";
        let cfg = configuration(&service, serial).unwrap();
        assert_eq!(AdvData::parse_adv(&cfg[9..9 + cfg[8] as usize]), Some(adv));
        assert_eq!(cfg[37], 16);
        assert_eq!(&cfg[38..54], serial.as_bytes());
        assert_eq!(&cfg[55..59], &[3, 3, 0xf6, 0xff]);
        assert_eq!(cfg[54], 22);
        assert_eq!(&cfg[59..61], &[17, 9]);
        assert_eq!(&cfg[61..77], serial.as_bytes());
        assert_eq!(frame(6, &[0]), [5,1,0xc8,0x32,0,0x3f,0,3,0,6,0,0]);
    }

    #[test]
    fn captured_c1_handshake_negotiates_the_radio_buffer_limit() {
        use rs_matter::transport::network::{btp::Btp, BtAddr};
        let btp = Btp::new();
        let peer = BtAddr([0xfd,0xd8,0x18,0x57,0x2f,0x84]);
        btp.process_incoming(Some(131), peer, &[0x65,0x6c,4,0,0,0,0x83,0,5]).unwrap();
        let mut packet = [0; MAX_VALUE];
        let len = btp.process_outgoing(Some(131), &mut packet).unwrap();
        assert_eq!(&packet[..len], &[0x65,0x6c,4,0x80,0,5]);
        assert!(btp.is_session_established(peer));
        btp.reset();
        assert!(!btp.is_session_established(peer));
    }

    #[test]
    fn matter_packet_round_trip_fragments_at_128_bytes() {
        use rs_matter::transport::network::{btp::Btp, Address, BtAddr, NetworkReceive, NetworkSend};
        let peripheral = Btp::new();
        let central = Btp::new();
        central.set_initiator(true);
        let peer = BtAddr([1,2,3,4,5,6]);
        let mut segment = [0; MAX_VALUE];
        let len = central.process_outgoing(Some(131), &mut segment).unwrap();
        peripheral.process_incoming(Some(131), peer, &segment[..len]).unwrap();
        let len = peripheral.process_outgoing(Some(131), &mut segment).unwrap();
        central.process_incoming(Some(131), peer, &segment[..len]).unwrap();
        let expected: Vec<u8> = (0..300).map(|n| n as u8).collect();
        futures_lite::future::block_on(async {
            NetworkSend::send_to(&mut &peripheral, &expected, Address::Btp(peer)).await.unwrap();
            let mut count = 0;
            loop {
                let len = peripheral.process_outgoing(Some(131), &mut segment).unwrap();
                if len == 0 { break; }
                assert!(len <= MAX_VALUE);
                central.process_incoming(Some(131), peer, &segment[..len]).unwrap();
                count += 1;
            }
            assert_eq!(count, 3);
            let mut packet = [0; 512];
            let (len, address) = NetworkReceive::recv_from(&mut &central, &mut packet).await.unwrap();
            assert_eq!(address, Address::Btp(peer));
            assert_eq!(&packet[..len], expected.as_slice());
        });
    }

    #[test]
    #[ignore = "requires NEST_BLE_FIRMWARE pointing to the stock peripheral image"]
    fn supported_firmware_changes_only_name_service_and_subscription_transitions() {
        let original = std::fs::read(std::env::var("NEST_BLE_FIRMWARE").unwrap()).unwrap();
        let patched = firmware(&original).unwrap();
        let changed: Vec<_> = original.iter().zip(&patched).enumerate()
            .filter_map(|(offset, (a,b))| (a != b).then_some(offset)).collect();
        assert_eq!(changed, [0x3e56,0x6c54,0x6c55,0x7778,0x777d,0x7780,0x7785,0x77c8,0x77cd,
            0x77d0,0x77d5,0x7868,0x786d,0x7870,0x7875]);
    }
}
