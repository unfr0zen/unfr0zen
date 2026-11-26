//! The Thermostat E's signed-loader entry point and USB identities.
//! Recovery and setup run the i.MX6 flow (crate::common::recovery).

pub mod assets;
pub mod boot;
#[cfg(unix)]
pub mod qemu;
pub mod usb;

#[cfg(test)]
mod tests {
    use crate::common::usb::rom_silicon;
    use std::collections::VecDeque;
    use unfr0zen_core::diamond3::sdp::Hid;
    use unfr0zen_core::Error;
    use unfr0zen_core::Result;

    struct Rom {
        replies: VecDeque<Vec<u8>>,
        writes: Vec<Vec<u8>>,
    }

    impl Hid for Rom {
        fn write(&mut self, report: &[u8]) -> Result<usize> {
            self.writes.push(report.to_vec());
            Ok(report.len())
        }

        fn read(&mut self, _: i32) -> Result<Vec<u8>> {
            Ok(self.replies.pop_front().unwrap_or_default())
        }
    }

    #[test]
    fn silicon_revision_is_separate_from_the_rom_version() {
        let mut rom = Rom {
            replies: VecDeque::from([
                vec![3, 0x12, 0x34, 0x34, 0x12],
                vec![4, 1, 0, 0x64, 0],
                vec![3, 0x12, 0x34, 0x34, 0x12],
                vec![4, 0x12, 0, 0, 0],
            ]),
            writes: Vec::new(),
        };
        assert_eq!(rom_silicon(&mut rom).unwrap().silicon, "i.MX6UL rev 1.1");
        assert_eq!(
            rom.writes,
            vec![
                vec![1, 1, 1, 2, 0x0c, 0x82, 0x60, 0x20, 0, 0, 0, 4, 0, 0, 0, 0, 0],
                vec![1, 1, 1, 0, 0, 0, 0x80, 0x20, 0, 0, 0, 4, 0, 0, 0, 0, 0],
            ]
        );
    }

    #[test]
    fn silicon_is_shown_even_if_the_rom_version_is_unreadable() {
        let mut rom = Rom { replies: VecDeque::from([vec![4, 2, 0, 0x64, 0]]), writes: Vec::new() };
        assert_eq!(rom_silicon(&mut rom).unwrap().silicon, "i.MX6UL rev 1.2");
    }

    #[test]
    fn an_unreadable_version_does_not_upload_or_jump() {
        let mut rom = Rom { replies: VecDeque::new(), writes: Vec::new() };
        assert!(matches!(rom_silicon(&mut rom), Err(Error::Timeout(_))));
        assert_eq!(rom.writes.len(), 1);
        assert_eq!(&rom.writes[0][1..3], &[1, 1]);
    }
}
