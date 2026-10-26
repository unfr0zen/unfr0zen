//! Recovery U-Boot's console over its USB ACM function. Port of the UBoot
//! class in unfr0zen.py.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use crate::{Error, Result};

pub const PROMPT: &[u8] = b"=> ";

/// The prompt at the start of a line: `crc32` prints `... ==> 1a2b3c4d`, and
/// a read can end right after its `=> `.
fn at_prompt(data: &[u8]) -> bool {
    data == PROMPT || (data.ends_with(PROMPT) && data[..data.len() - PROMPT.len()].ends_with(b"\n"))
}

/// What U-Boot prints when a command did not do what was asked.
const REJECTED: &[&str] = &["unknown command", "## error", "ubifs error", "error reading",
                            "command failed", "fdt_err", "no fdt memory address", "libfdt"];

/// A serial port whose reads time out instead of blocking forever.
pub trait Serial: Read + Write {
    /// Discards input that arrived before the next command.
    fn clear_input(&mut self) -> io::Result<()>;
}

/// Reads what is there; a read that times out returns nothing.
pub(crate) fn read_some(port: &mut impl Serial, data: &mut Vec<u8>) -> Result<()> {
    let mut buffer = [0u8; 65536];
    match port.read(&mut buffer) {
        Ok(n) => data.extend_from_slice(&buffer[..n]),
        Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => {}
        Err(e) => return Err(Error::Io(format!("console dropped: {e}"))),
    }
    Ok(())
}

pub struct UBoot<S: Serial> {
    port: S,
}

impl<S: Serial> UBoot<S> {
    /// Waits for the console to answer with a prompt after it enumerates.
    pub fn connect(mut port: S, timeout: Duration) -> Result<Self> {
        let mut data = Vec::new();
        let until = Instant::now() + timeout;
        let mut probe = Instant::now();
        while Instant::now() < until {
            if Instant::now() >= probe {
                port.write_all(b"\r")?;
                port.flush()?;
                probe = Instant::now() + Duration::from_millis(750);
            }
            read_some(&mut port, &mut data)?;
            if data.windows(PROMPT.len()).any(|w| w == PROMPT) {
                log::debug!("ACM console ready: {}", tail(&data, 1000));
                return Ok(UBoot { port });
            }
        }
        Err(Error::Timeout(format!("ACM appeared but U-Boot prompt did not respond within \
                                    {timeout:?}: {}", tail(&data, 300))))
    }

    /// Runs one command and returns its output up to the prompt; output that
    /// shows U-Boot rejected it is an error.
    pub fn command(&mut self, line: &str, timeout: Duration) -> Result<String> {
        let output = self.command_allow_failure(line, timeout)?;
        let lower = output.to_lowercase();
        if REJECTED.iter().any(|word| lower.contains(word)) {
            return Err(Error::Device(format!("U-Boot rejected {line:?}")));
        }
        Ok(output)
    }

    pub fn command_allow_failure(&mut self, line: &str, timeout: Duration) -> Result<String> {
        self.port.clear_input()?;
        self.port.write_all(format!("{line}\r").as_bytes())?;
        self.port.flush()?;
        let mut data = Vec::new();
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            read_some(&mut self.port, &mut data)?;
            if at_prompt(&data) {
                let output = String::from_utf8_lossy(&data).into_owned();
                log::debug!("=> {line}\n{output}");
                return Ok(output);
            }
        }
        Err(Error::Timeout(format!("U-Boot prompt timeout after {line:?}: {}", tail(&data, 300))))
    }

    /// Sends a line that ends the console, such as a handoff that powers down USB.
    pub fn send_last(mut self, line: &str) -> Result<()> {
        self.port.write_all(format!("{line}\r").as_bytes())?;
        self.port.flush()?;
        Ok(())
    }
}

pub(crate) fn tail(data: &[u8], n: usize) -> String {
    String::from_utf8_lossy(&data[data.len().saturating_sub(n)..]).into_owned()
}

#[cfg(test)]
pub(crate) mod fake {
    //! A serial port that answers each line it is sent.
    use super::*;
    use std::collections::VecDeque;

    pub struct FakeSerial<F: FnMut(&str) -> String> {
        pub respond: F,
        pending: Vec<u8>,
        pub output: VecDeque<u8>,
        pub sent: Vec<String>,
        /// The most one read returns, as USB delivers output in pieces.
        pub max_read: usize,
    }

    impl<F: FnMut(&str) -> String> FakeSerial<F> {
        pub fn new(respond: F) -> Self {
            FakeSerial { respond, pending: Vec::new(), output: VecDeque::new(), sent: Vec::new(), max_read: usize::MAX }
        }
    }

    impl<F: FnMut(&str) -> String> Write for FakeSerial<F> {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            for &byte in data {
                if byte == b'\r' {
                    let line = String::from_utf8_lossy(&self.pending).into_owned();
                    self.pending.clear();
                    let reply = (self.respond)(&line);
                    self.sent.push(line);
                    self.output.extend(reply.bytes().chain(PROMPT.iter().copied()));
                } else {
                    self.pending.push(byte);
                }
            }
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<F: FnMut(&str) -> String> Read for FakeSerial<F> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.output.is_empty() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let n = buffer.len().min(self.output.len()).min(self.max_read);
            for (slot, byte) in buffer.iter_mut().zip(self.output.drain(..n)) {
                *slot = byte;
            }
            Ok(n)
        }
    }

    impl<F: FnMut(&str) -> String> Serial for FakeSerial<F> {
        fn clear_input(&mut self) -> io::Result<()> {
            self.output.clear();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeSerial;
    use super::*;

    #[test]
    fn commands_return_output_up_to_the_prompt() {
        let port = FakeSerial::new(|line| if line == "version" { "U-Boot 2023.04\n".into() } else { String::new() });
        let mut board = UBoot::connect(port, Duration::from_secs(1)).unwrap();
        assert_eq!(board.command("version", Duration::from_secs(1)).unwrap(), "U-Boot 2023.04\n=> ");
    }

    #[test]
    fn a_read_ending_inside_crc_output_is_not_the_prompt() {
        let mut port = FakeSerial::new(|line| if line.starts_with("crc32") {
            format!("{line}\r\ncrc32 for 88000000 ... 8800ffff ==> 1a2b3c4d\r\n")
        } else {
            String::new()
        });
        port.max_read = 1;
        let mut board = UBoot::connect(port, Duration::from_secs(1)).unwrap();
        let output = board.command("crc32 0x88000000 0x10000", Duration::from_secs(1)).unwrap();
        assert!(output.contains("==> 1a2b3c4d"), "{output}");
    }

    #[test]
    fn rejected_commands_fail_unless_allowed() {
        let port = FakeSerial::new(|line| if line.starts_with("ubifsmount") { "UBIFS error (ubi0:0)\n".into() } else { String::new() });
        let mut board = UBoot::connect(port, Duration::from_secs(1)).unwrap();
        assert!(board.command("ubifsmount ubi0:root0", Duration::from_secs(1)).is_err());
        assert!(board.command_allow_failure("ubifsmount ubi0:root0", Duration::from_secs(1)).is_ok());
    }
}
