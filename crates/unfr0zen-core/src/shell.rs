//! The initramfs's busybox shell on its USB ACM gadget (platform/diamond3/initramfs-init):
//! a raw tty without echo or prompt, the slot mounted at /mnt. Port of
//! InitramfsShell in unfr0zen.py.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use regex::bytes::Regex;

use crate::console::{read_some, tail, Serial};
use crate::{device, Error, Result};

/// owner as "uid:gid", mode bits, and contents or SHA-256.
pub type File<T> = (String, u32, T);
/// name: (SHA-256, "uid:gid mode") as read back from flash.
pub type Digests = BTreeMap<String, (Option<String>, Option<String>)>;

/// How much of a file one read moves: about 350 KB of base64.
const READ_CHUNK: usize = 256 * 1024;

/// How long a reply may go quiet short of its end marker before the shell is
/// asked for another line of output behind it.
const NUDGE_AFTER: Duration = Duration::from_secs(1);

/// The mounted slot, as the patch sees it.
pub trait Volume {
    /// Owner, mode and contents of a regular file; None if it is absent.
    fn read(&mut self, name: &str) -> Result<Option<File<Vec<u8>>>>;
    /// Owner, mode and SHA-256 of a regular file, without moving its contents.
    fn inspect(&mut self, name: &str) -> Result<Option<File<String>>>;
    /// Length of a regular file, without moving its contents when supported.
    fn file_size(&mut self, name: &str) -> Result<u64> {
        self.read(name)?.map(|(_, _, data)| data.len() as u64)
            .ok_or_else(|| Error::Device(format!("{name} disappeared during inspection")))
    }
    /// "uid:gid mode" of a directory; None if it is absent.
    fn directory(&mut self, name: &str) -> Result<Option<String>>;
    fn free_bytes(&mut self) -> Result<u64>;
    /// Whether a write needs free space before it lands, checked against
    /// free_bytes first. A volume patched in memory, which fails before
    /// anything reaches flash when its writes do not fit, needs no check.
    fn checks_space(&self) -> bool {
        true
    }
    /// "uid mode" of bin/busybox or bin/toybox; empty if it has neither.
    fn shell_binary(&mut self) -> Result<String>;
    /// Replaces or creates a file by a rename.
    fn write(&mut self, name: &str, data: &[u8], owner: &str, mode: u32) -> Result<()>;
    /// Replace a verified binary via an atomic staged copy. Implementations
    /// can transfer just the patch bytes rather than the entire executable.
    fn patch_nlclient(&mut self, patch: &crate::nlclient::Patch) -> Result<()> {
        let (owner, mode, source) = self.read(crate::nlclient::BINARY)?
            .ok_or_else(|| crate::Error::Device("missing nlclient".into()))?;
        let data = patch.apply(&source, false)?;
        self.write(crate::nlclient::BINARY, &data, &owner, mode)
    }
    fn make_directory(&mut self, name: &str, mode: u32) -> Result<()>;
    fn remove(&mut self, name: &str) -> Result<()>;
    fn remount(&mut self, writable: bool) -> Result<()>;
    /// SHA-256 and "uid:gid mode" of each file, read back from flash.
    fn digests(&mut self, names: &[String]) -> Result<Digests>;
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// A path as one single-quoted shell word.
fn quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', r"'\''"))
}

fn marker() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("system random source");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub struct InitramfsShell<S: Serial> {
    port: S,
}

impl<S: Serial> InitramfsShell<S> {
    /// Waits for the shell to answer after the gadget enumerates. Bytes
    /// written right after the port opens can be lost: settle first, and keep
    /// asking through whatever a cut-off first line makes the shell say.
    pub fn connect(port: S, timeout: Duration) -> Result<Self> {
        std::thread::sleep(Duration::from_millis(500));
        let mut shell = InitramfsShell { port };
        let until = Instant::now() + timeout;
        loop {
            match shell.run_with("true", Duration::from_secs(3), false) {
                Ok(_) => return Ok(shell),
                Err(error) if Instant::now() >= until => return Err(error),
                Err(error) => log::debug!("initramfs shell not ready: {error}"),
            }
        }
    }

    pub fn run(&mut self, script: &str) -> Result<String> {
        self.run_with(script, Duration::from_secs(60), false)
    }

    /// Runs script and returns its output; its last command must succeed.
    /// quiet keeps the output, which can be file contents, out of the log.
    pub fn run_with(&mut self, script: &str, timeout: Duration, quiet: bool) -> Result<String> {
        let marker = marker();
        let command = script.lines().next().unwrap_or(""); // what the log shows: no file contents
        // The script's status first, before echo resets $?. No quotes in the
        // marker lines: one cut short cannot leave the shell inside a string.
        // The start marker keeps output a nudge left from an earlier command
        // out of this one's.
        let sent = format!("echo {marker}:begin\n{script}\nunfr0zen_rc=$?; echo; echo {marker}:$unfr0zen_rc\n");
        let failed = |step: &str, e: std::io::Error| Error::Io(format!("{step} {command:?} ({} bytes): {e}", sent.len()));
        self.port.clear_input().map_err(|e| failed("clearing input for", e))?;
        self.port.write_all(sent.as_bytes()).map_err(|e| failed("sending", e))?;
        self.port.flush().map_err(|e| failed("flushing", e))?;
        let begin = Regex::new(&format!(r"{marker}:begin\r?\n")).expect("valid pattern");
        let end = Regex::new(&format!(r"\n{marker}:(\d+)\r?\n")).expect("valid pattern");
        let (mut data, mut nudges) = (Vec::new(), 0);
        let mut heard = (0, Instant::now());
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            read_some(&mut self.port, &mut data)?;
            if let Some(found) = end.captures(&data) {
                let stop = found.get(0).unwrap().start();
                let start = begin.find(&data[..stop]).map_or(0, |found| found.end());
                let output = String::from_utf8_lossy(&data[start..stop]).into_owned();
                let shown = if quiet { format!("({} bytes of output)", output.len()) } else { tail(output.as_bytes(), 2000) };
                let nudged = if nudges > 0 { format!(" (answered after {nudges} nudges)") } else { String::new() };
                log::debug!("initramfs$ {command}{nudged}\n{shown}");
                if &found[1] != b"0" {
                    let reason = if quiet { "see the unit".into() } else { tail(output.trim().as_bytes(), 300) };
                    return device(format!("initramfs command {command:?} failed: {reason}"));
                }
                return Ok(output);
            }
            // On Windows in a VM, the last USB packet of a reply can stay
            // undelivered until more output follows it: a quiet reply gets
            // a blank line behind it.
            if data.len() != heard.0 {
                heard = (data.len(), Instant::now());
            } else if heard.1.elapsed() >= NUDGE_AFTER {
                self.port.write_all(b"echo\n").and_then(|()| self.port.flush()).map_err(|e| failed("nudging", e))?;
                nudges += 1;
                heard.1 = Instant::now();
            }
        }
        Err(Error::Timeout(format!("initramfs shell did not answer {command:?} ({} bytes received, {nudges} nudges)", data.len())))
    }

    /// Chunk `index` of the file at the quoted `path`, tried up to three times.
    fn read_chunk(&mut self, name: &str, path: &str, index: usize) -> Result<Vec<u8>> {
        let chunk = format!("dd if={path} bs={READ_CHUNK} skip={index} count=1 2>/dev/null");
        for attempt in 1..=3 {
            let output = self.run_with(&format!("{chunk} | sha256sum; {chunk} | base64"), Duration::from_secs(60), true)?;
            let (first, encoded) = output.trim_start_matches('\n').split_once('\n').unwrap_or((output.trim(), ""));
            let sha = first.split_whitespace().next().unwrap_or("");
            let compact: String = encoded.split_whitespace().collect();
            match STANDARD.decode(compact) {
                Ok(data) if sha256_hex(&data) == sha => return Ok(data),
                _ => log::warn!("{name}: chunk {index} arrived damaged (attempt {attempt} of 3)"),
            }
        }
        device(format!("{name}: chunk {index} kept arriving damaged"))
    }

    /// Owner, mode and contents of the regular file at an absolute path on the
    /// unit; None if it is absent. It moves in chunks, each checked against
    /// the hash the unit takes of it and read again when it arrives damaged;
    /// a serial link can drop whole lines of a large transfer and still
    /// leave valid base64.
    pub fn read_file(&mut self, name: &str) -> Result<Option<File<Vec<u8>>>> {
        let path = quote(name);
        let output = self.run(&format!("if [ -L {path} ]; then echo link; elif [ -f {path} ]; then \
                                        stat -c '%u:%g %a' {path}; stat -c %s {path}; sha256sum {path}; else echo absent; fi"))?;
        let mut lines = output.trim().lines();
        let Some((owner, mode)) = file_header(name, lines.next().unwrap_or(""))? else { return Ok(None) };
        let size: usize = lines.next().and_then(|line| line.trim().parse().ok())
            .map_or_else(|| device(format!("{name}: no size from the unit")), Ok)?;
        let sha = lines.next().and_then(|line| line.split_whitespace().next())
            .map_or_else(|| device(format!("{name}: no sha256sum from the unit")), Ok)?.to_string();
        let mut data = Vec::with_capacity(size);
        for index in 0..size.div_ceil(READ_CHUNK) {
            data.extend(self.read_chunk(name, &path, index)?);
        }
        if sha256_hex(&data) != sha {
            return device(format!("{name} changed while it was read"));
        }
        Ok(Some((owner, mode, data)))
    }

    /// Restarts the unit from Linux: it boots its installed firmware, or lands in SDP.
    pub fn restart(mut self) -> Result<()> {
        self.port.write_all(b"sync; mount -o remount,ro /mnt; reboot -f\n")?;
        self.port.flush()?;
        Ok(())
    }
}

/// The owner and mode line stat printed, or the error for a non-file.
fn file_header(name: &str, first: &str) -> Result<Option<(String, u32)>> {
    match first {
        "absent" => Ok(None),
        "link" => device(format!("{name} on the device is a symbolic link")),
        _ => {
            let (owner, mode) = first.split_once(' ').map_or_else(|| device(format!("{name}: unexpected stat {first:?}")), Ok)?;
            let mode = u32::from_str_radix(mode, 8).or_else(|_| device(format!("{name}: unexpected mode {mode:?}")))?;
            Ok(Some((owner.to_string(), mode)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::{self, Read, Write};
    use std::sync::Mutex;

    /// A shell that answers each script from a table of first lines, then
    /// the marker line the protocol appends. Entries for the same start
    /// answer in turn, the last one from then on.
    struct FakeShell {
        answers: Vec<(&'static str, &'static str, u8)>,
        used: Vec<bool>,
        input: Vec<u8>,
        output: VecDeque<u8>,
        scripts: Vec<String>,
        /// Hold the next reply's end marker until a nudge, as a stuck last
        /// USB packet would.
        stall: bool,
        held: Option<String>,
    }

    impl FakeShell {
        fn new(answers: Vec<(&'static str, &'static str, u8)>) -> Self {
            let used = vec![false; answers.len()];
            FakeShell { answers, used, input: Vec::new(), output: VecDeque::new(), scripts: Vec::new(), stall: false, held: None }
        }
    }

    impl Write for FakeShell {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.input.extend_from_slice(data);
            let text = String::from_utf8_lossy(&self.input).into_owned();
            if text == "echo\n" {
                let held = self.held.take().unwrap_or_default();
                self.output.extend(format!("\n{held}").bytes());
                self.input.clear();
                return Ok(data.len());
            }
            let marker = regex::Regex::new(r"(?s)^echo ([0-9a-f]{16}):begin\n(.*)\nunfr0zen_rc=\$\?; echo; echo ([0-9a-f]{16}):\$unfr0zen_rc\n$").unwrap();
            if let Some(found) = marker.captures(&text) {
                assert_eq!(found[1], found[3], "one marker per command");
                let script = found[2].to_string();
                let matching: Vec<usize> = (0..self.answers.len())
                    .filter(|&i| script.starts_with(self.answers[i].0)).collect();
                let pick = matching.iter().copied().find(|&i| !self.used[i]).or(matching.last().copied());
                let (_, output, code) = pick.map_or(("", "", 0), |i| {
                    self.used[i] = true;
                    self.answers[i]
                });
                let end = format!("{}:{code}\n", &found[3]);
                self.output.extend(format!("{}:begin\n{output}\n", &found[1]).bytes());
                if std::mem::take(&mut self.stall) { self.held = Some(end) } else { self.output.extend(end.bytes()) }
                self.scripts.push(script);
                self.input.clear();
            }
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Read for FakeShell {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.output.is_empty() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let n = buffer.len().min(self.output.len());
            for (slot, byte) in buffer.iter_mut().zip(self.output.drain(..n)) {
                *slot = byte;
            }
            Ok(n)
        }
    }

    impl Serial for FakeShell {
        fn clear_input(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn shell(answers: Vec<(&'static str, &'static str, u8)>) -> InitramfsShell<FakeShell> {
        InitramfsShell::connect(FakeShell::new(answers), Duration::from_secs(1)).unwrap()
    }

    struct Capture(Mutex<String>);

    impl log::Log for Capture {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &log::Record) {
            self.0.lock().unwrap().push_str(&format!("{}\n", record.args()));
        }
        fn flush(&self) {}
    }

    static LOG: Capture = Capture(Mutex::new(String::new()));

    #[test]
    fn reads_a_file_and_keeps_its_contents_out_of_the_log() {
        let _ = log::set_logger(&LOG).map(|()| log::set_max_level(log::LevelFilter::Debug));
        let sha = sha256_hex(b"secret");
        let mut shell = shell(vec![
            ("if [ -L \'/mnt/etc/shadow\' ]", leak(format!("0:0 640\n6\n{sha}  /mnt/etc/shadow")), 0),
            ("dd if=\'/mnt/etc/shadow\' bs=262144 skip=0 count=1", leak(format!("{sha}  -\nc2VjcmV0")), 0),
        ]);
        assert_eq!(shell.read_file("/mnt/etc/shadow").unwrap(), Some(("0:0".into(), 0o640, b"secret".to_vec())));
        let log = LOG.0.lock().unwrap();
        assert!(log.contains("etc/shadow"));
        assert!(!log.contains("c2VjcmV0"));
    }

    #[test]
    fn absent_files_and_links() {
        let mut shell = shell(vec![("if [ -L \'/mnt/missing\' ]", "absent", 0), ("if [ -L \'/mnt/link\' ]", "link", 0)]);
        assert_eq!(shell.read_file("/mnt/missing").unwrap(), None);
        assert!(shell.read_file("/mnt/link").unwrap_err().to_string().contains("symbolic link"));
    }

    #[test]
    fn failing_commands_are_errors() {
        let mut shell = shell(vec![("rm -f", "rm: busy", 1)]);
        assert!(shell.run("rm -f /x").unwrap_err().to_string().contains("rm: busy"));
    }

    struct RejectingPort;

    impl Write for RejectingPort {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from_raw_os_error(87))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Read for RejectingPort {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::TimedOut.into())
        }
    }

    impl Serial for RejectingPort {
        fn clear_input(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_reply_that_stalls_short_of_its_marker_is_nudged_through() {
        let mut shell = shell(vec![("rm -f", "", 0)]);
        shell.port.stall = true;
        shell.run("rm -f /x").unwrap();
        // The nudge's blank line, left behind, stays out of the next reply.
        shell.port.output.extend(b"\n");
        assert_eq!(shell.run("true").unwrap(), "");
    }

    #[test]
    fn a_rejected_send_names_the_command_and_its_size() {
        let mut shell = InitramfsShell { port: RejectingPort };
        let error = shell.run("rm -f /x").unwrap_err().to_string();
        assert!(error.starts_with("sending \"rm -f /x\" ("), "{error}");
        assert!(error.contains(" bytes): "), "{error}");
    }

    #[test]
    fn reads_decode_wrapped_base64_and_refuse_what_they_cannot_parse() {
        let big: Vec<u8> = (0..49).collect();
        let sha = sha256_hex(&big);
        let mut connection = shell(vec![
            ("if [ -L \'/mnt/bin/big\' ]", leak(format!("0:0 755\n49\n{sha}  /mnt/bin/big")), 0),
            ("dd if=\'/mnt/bin/big\'", leak(format!("{sha}  -\nAAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKiss\nLS4vMA==")), 0),
            ("if [ -L \'/mnt/etc/garbled\' ]", "0:0 644\n9\nabcd  /mnt/etc/garbled", 0),
            ("dd if=\'/mnt/etc/garbled\'", "abcd  -\nnot base64!", 0),
            ("if [ -L \'/mnt/etc/odd\' ]", "0:0 rw-r--r--\n1\nabcd  /mnt/etc/odd", 0),
            ("if [ -L \'/mnt/etc/nosize\' ]", "0:0 644\n\nabcd  /mnt/etc/nosize", 0),
        ]);
        assert_eq!(connection.read_file("/mnt/bin/big").unwrap(), Some(("0:0".into(), 0o755, big)));
        assert!(connection.read_file("/mnt/etc/garbled").unwrap_err().to_string().contains("kept arriving damaged"));
        assert!(connection.read_file("/mnt/etc/odd").unwrap_err().to_string().contains("unexpected mode"));
        assert!(connection.read_file("/mnt/etc/nosize").unwrap_err().to_string().contains("no size"));
    }

    #[test]
    fn a_chunk_that_arrives_damaged_is_read_again() {
        let data: Vec<u8> = (0..READ_CHUNK + 10).map(|i| (i % 251) as u8).collect();
        let (first, second) = data.split_at(READ_CHUNK);
        let chunk = |part: &[u8]| format!("{}  -\n{}", sha256_hex(part), STANDARD.encode(part));
        // Valid base64 with a line missing, as a lossy link leaves it.
        let encoded = STANDARD.encode(first);
        let damaged = format!("{}  -\n{}{}", sha256_hex(first), &encoded[..76], &encoded[152..]);
        let mut connection = shell(vec![
            ("if [ -L \'/mnt/nestlabs/sbin/nlclient\' ]",
             leak(format!("0:0 755\n{}\n{}  /mnt/nestlabs/sbin/nlclient", data.len(), sha256_hex(&data))), 0),
            ("dd if=\'/mnt/nestlabs/sbin/nlclient\' bs=262144 skip=0 ", leak(damaged), 0),
            ("dd if=\'/mnt/nestlabs/sbin/nlclient\' bs=262144 skip=0 ", leak(chunk(first)), 0),
            ("dd if=\'/mnt/nestlabs/sbin/nlclient\' bs=262144 skip=1 ", leak(chunk(second)), 0),
        ]);
        assert_eq!(connection.read_file("/mnt/nestlabs/sbin/nlclient").unwrap().unwrap().2, data);
        let reads: Vec<&String> = connection.port.scripts.iter().filter(|s| s.starts_with("dd ")).collect();
        assert_eq!(reads.len(), 3);
        assert!(reads[2].contains("skip=1 count=1"), "{}", reads[2]);
    }

    #[test]
    fn a_file_that_changes_between_chunks_is_refused() {
        let mut connection = shell(vec![
            ("if [ -L \'/mnt/etc/a\' ]", leak(format!("0:0 644\n6\n{}  /mnt/etc/a", sha256_hex(b"before"))), 0),
            ("dd if=\'/mnt/etc/a\'", leak(format!("{}  -\n{}", sha256_hex(b"after!"), STANDARD.encode(b"after!"))), 0),
        ]);
        assert!(connection.read_file("/mnt/etc/a").unwrap_err().to_string().contains("changed while it was read"));
    }

    #[test]
    fn paths_reach_the_shell_as_single_words() {
        let mut connection = shell(vec![("if [ -L", "absent", 0)]);
        assert_eq!(connection.read_file("/mnt/etc/it's;reboot").unwrap(), None);
        assert!(connection.port.scripts[1].starts_with(r"if [ -L '/mnt/etc/it'\''s;reboot' ]"), "{}", connection.port.scripts[1]);
    }

    fn leak(text: String) -> &'static str {
        Box::leak(text.into_boxed_str())
    }

}
