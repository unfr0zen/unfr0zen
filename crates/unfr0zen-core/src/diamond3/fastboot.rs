//! The fastboot protocol recovery U-Boot speaks beside its console, over any
//! bulk pipe: `download` ("fastboot stage") and the recovery build's `upload`
//! ("fastboot get_staged"; uboot/patches/recovery/drivers-fastboot-fb_command.c.patch).

use std::time::Duration;

pub use crate::bulk::Bulk;
use crate::diamond3::recovery::{Fastboot, RAM_LIMIT};
use crate::{device, Error, Result};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const DATA_TIMEOUT: Duration = Duration::from_secs(30);
const CHUNK: usize = 1 << 20;

pub struct Protocol<B: Bulk> {
    bulk: B,
}

impl<B: Bulk> Protocol<B> {
    pub fn new(bulk: B) -> Self {
        Protocol { bulk }
    }

    /// The device's next response other than INFO: ("OKAY" | "DATA", the rest).
    fn response(&mut self, command: &str) -> Result<(String, String)> {
        loop {
            let reply = self.bulk.receive(64, COMMAND_TIMEOUT)?;
            // Split as bytes: a reply need not be UTF-8.
            let (kind, rest) = reply.split_at(reply.len().min(4));
            let rest = String::from_utf8_lossy(rest).into_owned();
            match kind {
                b"INFO" => log::debug!("fastboot {command}: {rest}"),
                b"OKAY" | b"DATA" => return Ok((String::from_utf8_lossy(kind).into_owned(), rest)),
                b"FAIL" => return device(format!("fastboot {command} failed: {rest}")),
                _ => return device(format!("fastboot {command}: unexpected response {:?}", String::from_utf8_lossy(&reply))),
            }
        }
    }

    fn data_size(&mut self, command: &str) -> Result<usize> {
        self.bulk.send(command.as_bytes(), COMMAND_TIMEOUT)?;
        match self.response(command)? {
            (kind, size) if kind == "DATA" => usize::from_str_radix(&size, 16)
                .or_else(|_| device(format!("fastboot {command}: bad DATA size {size:?}"))),
            (kind, _) => device(format!("fastboot {command}: {kind} instead of DATA")),
        }
    }

    fn okay(&mut self, command: &str) -> Result<()> {
        match self.response(command)? {
            (kind, _) if kind == "OKAY" => Ok(()),
            (kind, rest) => device(format!("fastboot {command}: {kind}{rest} instead of OKAY")),
        }
    }
}

impl<B: Bulk> Fastboot for Protocol<B> {
    fn download(&mut self, data: &[u8]) -> Result<()> {
        let command = format!("download:{:08x}", data.len());
        let size = self.data_size(&command)?;
        if size != data.len() {
            return device(format!("fastboot download: device expects {size} bytes, not {}", data.len()));
        }
        for chunk in data.chunks(CHUNK) {
            self.bulk.send(chunk, DATA_TIMEOUT)?;
        }
        self.okay("download")
    }

    fn upload(&mut self) -> Result<Vec<u8>> {
        let size = self.data_size("upload")?;
        // upload_ram never asks for more than RAM_LIMIT.
        if size > RAM_LIMIT {
            return device(format!("fastboot upload: device announces {size} bytes, more than {RAM_LIMIT}"));
        }
        let mut data = Vec::with_capacity(size);
        while data.len() < size {
            let chunk = self.bulk.receive((size - data.len()).min(CHUNK), DATA_TIMEOUT)?;
            if chunk.is_empty() {
                return Err(Error::Timeout(format!("fastboot upload stopped at {} of {size} bytes", data.len())));
            }
            data.extend_from_slice(&chunk);
        }
        if data.len() != size {
            return device(format!("fastboot upload sent {} bytes, announced {size}", data.len()));
        }
        self.okay("upload")?;
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Recovery's fastboot function with a buffer behind it.
    #[derive(Default)]
    struct Device {
        buffer: Vec<u8>,
        expected: usize,
        replies: VecDeque<Vec<u8>>,
        info: bool,
    }

    impl Bulk for Device {
        fn send(&mut self, data: &[u8], _: Duration) -> Result<()> {
            if self.expected > 0 {
                self.buffer.extend_from_slice(data);
                if self.buffer.len() >= self.expected {
                    self.expected = 0;
                    self.replies.push_back(b"OKAY".to_vec());
                }
                return Ok(());
            }
            let command = String::from_utf8_lossy(data).into_owned();
            if self.info {
                self.replies.push_back(b"INFOworking".to_vec());
            }
            if let Some(size) = command.strip_prefix("download:") {
                self.expected = usize::from_str_radix(size, 16).unwrap();
                self.buffer.clear();
                self.replies.push_back(format!("DATA{:08x}", self.expected).into_bytes());
            } else if command == "upload" {
                self.replies.push_back(format!("DATA{:08x}", self.buffer.len()).into_bytes());
                // The recovery build streams RAM in 256 KiB requests.
                let chunks: Vec<Vec<u8>> = self.buffer.chunks(256 * 1024).map(<[u8]>::to_vec).collect();
                self.replies.extend(chunks);
                self.replies.push_back(b"OKAY".to_vec());
            } else {
                self.replies.push_back(b"FAILunknown command".to_vec());
            }
            Ok(())
        }

        fn receive(&mut self, max: usize, _: Duration) -> Result<Vec<u8>> {
            let mut reply = self.replies.pop_front().unwrap_or_default();
            if reply.len() > max {
                let rest = reply.split_off(max);
                self.replies.push_front(rest);
            }
            Ok(reply)
        }
    }

    #[test]
    fn download_then_upload_round_trips() {
        let data: Vec<u8> = (0..3 * CHUNK + 12345).map(|i| (i * 7) as u8).collect();
        let mut fastboot = Protocol::new(Device { info: true, ..Device::default() });
        fastboot.download(&data).unwrap();
        assert_eq!(fastboot.bulk.buffer, data);
        assert_eq!(fastboot.upload().unwrap(), data);
    }

    #[test]
    fn a_failing_device_is_an_error() {
        let mut fastboot = Protocol::new(Device::default());
        assert!(fastboot.data_size("flash:boot").unwrap_err().to_string().contains("failed: unknown command"));
    }

    #[test]
    fn a_short_upload_is_an_error() {
        // The device announces 16 bytes and stops after 8: the empty read ends it.
        let mut fastboot = stub(&[b"DATA00000010", &[1u8; 8]]);
        assert!(fastboot.upload().unwrap_err().to_string().contains("stopped at 8 of 16"));
    }

    #[test]
    fn a_download_the_device_sizes_differently_sends_no_data() {
        let mut fastboot = stub(&[b"DATA00000010"]);
        let error = fastboot.download(b"8 bytes!").unwrap_err();
        assert!(error.to_string().contains("expects 16 bytes, not 8"), "{error}");
        assert_eq!(fastboot.bulk.sent, [b"download:00000008".to_vec()]);
    }

    #[test]
    fn transfers_end_only_on_the_replies_the_protocol_names() {
        let failure = |replies: &[&[u8]], download: bool| {
            let mut fastboot = stub(replies);
            if download { fastboot.download(b"data").unwrap_err() } else { fastboot.upload().unwrap_err() }.to_string()
        };
        assert!(failure(&[b"DATA00000004", b"FAILflash busy"], true).contains("failed: flash busy"));
        assert!(failure(&[b"OKAY"], false).contains("OKAY instead of DATA"));
        assert!(failure(&[b"DATAnot-hex"], false).contains("bad DATA size"));
        assert!(failure(&[b"DATA00000002", b"ab", b"DATA"], false).contains("instead of OKAY"));
        assert!(failure(&[b"INFOstill here"], false).contains("unexpected response"));
    }

    #[test]
    fn replies_that_are_not_utf8_are_errors() {
        assert!(stub(&[b"OK\xff\xffxx"]).upload().unwrap_err().to_string().contains("unexpected response"));
        assert!(stub(&[b"\xff\xff"]).upload().unwrap_err().to_string().contains("unexpected response"));
    }

    #[test]
    fn an_upload_larger_than_ram_is_refused_before_any_data() {
        let mut fastboot = stub(&[b"DATAffffffff", &[1u8; 8]]);
        assert!(fastboot.upload().unwrap_err().to_string().contains("more than"));
        assert_eq!(fastboot.bulk.replies.len(), 1);
    }

    /// Replies only, in order; what is sent is kept.
    struct Stub {
        replies: VecDeque<Vec<u8>>,
        sent: Vec<Vec<u8>>,
    }

    fn stub(replies: &[&[u8]]) -> Protocol<Stub> {
        Protocol::new(Stub { replies: replies.iter().map(|r| r.to_vec()).collect(), sent: Vec::new() })
    }

    impl Bulk for Stub {
        fn send(&mut self, data: &[u8], _: Duration) -> Result<()> {
            self.sent.push(data.to_vec());
            Ok(())
        }
        fn receive(&mut self, _: usize, _: Duration) -> Result<Vec<u8>> {
            Ok(self.replies.pop_front().unwrap_or_default())
        }
    }
}
