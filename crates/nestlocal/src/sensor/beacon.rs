//! A Nest Temperature Sensor's beacon, as nlclient's ProcessRcsPayload reads
//! it: service data for 0xFEAF carrying an AES-128-CCM payload, delivered as
//! the BLE chip's advertisement report.

use aes::Aes128;
use ccm::aead::{AeadInOut, KeyInit};
use ccm::consts::{U13, U8};
use ccm::Ccm;

/// The advertisement's header: AD length, type 0x16 (service data), the
/// service's UUID, and nlclient's own length and type bytes for an 18-byte
/// sensor payload.
const HEADER: [u8; 6] = [0x17, 0x16, 0xaf, 0xfe, 0x13, 0x03];
const DATA_LEN: usize = 31;
/// An advertisement report: type, address type, address, length, data, RSSI.
pub const REPORT_LEN: usize = 8 + 1 + DATA_LEN + 1;
const NONCONNECTABLE: u8 = 3;
const RSSI: i8 = -50;
const BATTERY_FULL: u8 = 100;

/// The largest sequence number the 3 bytes on the air hold.
pub const MAX_SEQUENCE: u32 = 0xff_ffff;

/// What the beacon's 12-bit field holds, in tenths of a °C.
pub fn tenths(celsius: f64) -> Option<i16> {
    let tenths = (celsius * 10.0).round();
    (-2048.0..=2047.0).contains(&tenths).then_some(tenths as i16)
}

/// The sensor's beacon for one reading. Each sequence number may go out once:
/// nlclient drops a beacon whose number is not above the last it took.
pub fn report(id: u64, key: &[u8; 16], sequence: u32, tenths: i16) -> [u8; REPORT_LEN] {
    let sequence = sequence.to_le_bytes();
    // A reading and four earlier ones as 5-bit differences from it: all the
    // same, for the one reading known.
    let samples = u32::from(tenths as u16) & 0xfff;
    let [a, b, c, d] = samples.to_le_bytes();
    let mut body = [a, b, c, d, BATTERY_FULL];
    let mut nonce = [0u8; 13];
    nonce[0] = 3;
    nonce[1..9].copy_from_slice(&id.to_be_bytes());
    nonce[10..13].copy_from_slice(&[sequence[2], sequence[1], sequence[0]]);
    let cipher = Ccm::<Aes128, U8, U13>::new(key.into());
    let tag = cipher.encrypt_inout_detached((&nonce).into(), &[], (&mut body[..]).into())
        .expect("a 5-byte body is within CCM's limits");

    let mut data = [0u8; DATA_LEN];
    data[..6].copy_from_slice(&HEADER);
    data[6..8].copy_from_slice(&(id as u16).to_le_bytes());
    data[8..11].copy_from_slice(&sequence[..3]);
    data[11..16].copy_from_slice(&body);
    data[16..24].copy_from_slice(&tag);

    let mut out = [0u8; REPORT_LEN];
    out[0] = NONCONNECTABLE;
    out[2..8].copy_from_slice(&id.to_le_bytes()[..6]);
    out[8] = 24;
    out[9..9 + DATA_LEN].copy_from_slice(&data);
    out[REPORT_LEN - 1] = RSSI as u8;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: u64 = 0x18B4_3000_00A1_B2C3;
    const KEY: [u8; 16] = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];

    /// A beacon stock nlclient 6.4-8 decrypted: 2100 in the 12 bits is -199.6 °C.
    #[test]
    fn matches_a_beacon_nlclient_accepted() {
        let hex: String = report(ID, &KEY, 3, 2100).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "0300c3b2a1000030181716affe1303c3b2030000e7be27289b531299c7fd0766\
                         2200000000000000ce");
    }

    #[test]
    fn a_reading_reaches_the_twelve_bits_signed() {
        assert_eq!(tenths(20.54), Some(205));
        assert_eq!(tenths(-3.26), Some(-33));
        assert_eq!(tenths(204.7), Some(2047));
        assert_eq!(tenths(204.8), None);
    }
}
