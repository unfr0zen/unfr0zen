//! The initramfs's busybox shell on its USB ACM gadget (platform/diamond3/initramfs-init):
//! a raw tty without echo or prompt, the slot mounted at /mnt. Port of
//! InitramfsShell in unfr0zen.py.

use std::collections::{BTreeMap, BTreeSet};
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
    /// Replaces or creates a file by a rename, copying payload contents on the unit.
    fn write(&mut self, name: &str, data: &[u8], owner: &str, mode: u32, payload: &BTreeSet<String>) -> Result<()>;
    /// Replace a verified binary via an atomic staged copy. Implementations
    /// can transfer just the patch bytes rather than the entire executable.
    fn patch_nlclient(&mut self, patch: &crate::nlclient::Patch) -> Result<()> {
        let (owner, mode, source) = self.read(crate::nlclient::BINARY)?
            .ok_or_else(|| crate::Error::Device("missing nlclient".into()))?;
        let data = patch.apply(&source, false)?;
        self.write(crate::nlclient::BINARY, &data, &owner, mode, &BTreeSet::new())
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
        self.port.clear_input()?;
        // The script's status first, before echo resets $?. No quotes in the
        // marker line: one cut short cannot leave the shell inside a string.
        self.port.write_all(format!("{script}\nunfr0zen_rc=$?; echo; echo {marker}:$unfr0zen_rc\n").as_bytes())?;
        self.port.flush()?;
        let end = Regex::new(&format!(r"\n{marker}:(\d+)\r?\n")).expect("valid pattern");
        let mut data = Vec::new();
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            read_some(&mut self.port, &mut data)?;
            if let Some(found) = end.captures(&data) {
                let output = String::from_utf8_lossy(&data[..found.get(0).unwrap().start()]).into_owned();
                let shown = if quiet { format!("({} bytes of output)", output.len()) } else { tail(output.as_bytes(), 2000) };
                log::debug!("initramfs$ {command}\n{shown}");
                if &found[1] != b"0" {
                    let reason = if quiet { "see the unit".into() } else { tail(output.trim().as_bytes(), 300) };
                    return device(format!("initramfs command {command:?} failed: {reason}"));
                }
                return Ok(output);
            }
        }
        Err(Error::Timeout(format!("initramfs shell did not answer {command:?}")))
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

impl<S: Serial> Volume for InitramfsShell<S> {
    /// The file in chunks, each checked against the hash the unit takes of
    /// it and read again when it arrives damaged; a serial link can drop
    /// whole lines of a large transfer and still leave valid base64.
    fn read(&mut self, name: &str) -> Result<Option<File<Vec<u8>>>> {
        let path = quote(&format!("/mnt/{name}"));
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

    fn inspect(&mut self, name: &str) -> Result<Option<File<String>>> {
        let path = quote(&format!("/mnt/{name}"));
        let output = self.run(&format!("if [ -L {path} ]; then echo link; elif [ -f {path} ]; then \
                                        stat -c '%u:%g %a' {path}; sha256sum {path}; else echo absent; fi"))?;
        let mut lines = output.trim().lines();
        let Some((owner, mode)) = file_header(name, lines.next().unwrap_or(""))? else { return Ok(None) };
        let sha = lines.next().and_then(|line| line.split_whitespace().next())
            .map_or_else(|| device(format!("{name}: no sha256sum from the unit")), Ok)?;
        Ok(Some((owner, mode, sha.to_string())))
    }

    fn file_size(&mut self, name: &str) -> Result<u64> {
        let path = quote(&format!("/mnt/{name}"));
        self.run(&format!("stat -c %s {path}"))?.trim().parse()
            .map_err(|_| Error::Device(format!("{name}: no size from the unit")))
    }

    fn directory(&mut self, name: &str) -> Result<Option<String>> {
        let path = quote(&format!("/mnt/{name}"));
        let output = self.run(&format!("if [ -d {path} ] && [ ! -L {path} ]; then stat -c '%u:%g %a' {path}; \
                                        elif [ -e {path} ]; then echo other; else echo absent; fi"))?;
        match output.trim() {
            "other" => device(format!("{name} on the device is not a directory")),
            "absent" => Ok(None),
            found => Ok(Some(found.to_string())),
        }
    }

    fn free_bytes(&mut self) -> Result<u64> {
        let output = self.run("df -Pk /mnt | tail -n 1")?;
        output.split_whitespace().nth(3).and_then(|kib| kib.parse::<u64>().ok()).map(|kib| kib * 1024)
            .map_or_else(|| device(format!("unexpected df output: {}", output.trim())), Ok)
    }

    fn shell_binary(&mut self) -> Result<String> {
        Ok(self.run("for f in /mnt/bin/busybox /mnt/bin/toybox; do \
                     if [ -f $f ]; then stat -L -c '%u %a' $f; break; fi; done")?.trim().to_string())
    }

    fn write(&mut self, name: &str, data: &[u8], owner: &str, mode: u32, payload: &BTreeSet<String>) -> Result<()> {
        let path = format!("/mnt/{name}");
        let parent = format!("mkdir -p {}", quote(path.rsplit_once('/').map_or("/mnt", |(parent, _)| parent)));
        let (path, staged) = (quote(&path), quote(&format!("{path}.unfr0zen")));
        let finish = format!("chown {owner} {staged} && chmod {mode:o} {staged} && mv -f {staged} {path}");
        let sha = sha256_hex(data);
        if payload.contains(&sha) {
            self.run(&format!("{parent} && cat /payload/{sha} > {staged} && {finish}"))?;
            return Ok(());
        }
        // Through the shell: base64 at a few hundred KiB/s, so allow for size.
        let encoded = STANDARD.encode(data);
        let lines: String = encoded.as_bytes().chunks(76).map(|line| format!("{}\n", String::from_utf8_lossy(line))).collect();
        // The delimiter has a character base64 never does, so no line of data ends it.
        self.run_with(&format!("{parent} && base64 -d > {staged} <<'UNFR0ZEN_EOF' && {finish}\n{lines}UNFR0ZEN_EOF"),
                      Duration::from_secs(60 + data.len() as u64 / 50_000), false)?;
        Ok(())
    }

    fn patch_nlclient(&mut self, patch: &crate::nlclient::Patch) -> Result<()> {
        let path = format!("/mnt/{}", crate::nlclient::BINARY);
        let (path, staged) = (quote(&path), quote(&format!("{path}.unfr0zen")));
        self.run(&format!("test \"$(sha256sum {path} | cut -d' ' -f1)\" = {} && cp -p {path} {staged}", patch.stock_sha))?;
        for edit in &patch.edits {
            let encoded = STANDARD.encode(&edit.after);
            let lines: String = encoded.as_bytes().chunks(76)
                .map(|line| format!("{}\n", String::from_utf8_lossy(line))).collect();
            // dd bs=1 makes a system call per byte, so allow for size.
            self.run_with(&format!("base64 -d <<'NLCLIENT_PATCH' | dd of={staged} bs=1 seek={} conv=notrunc\n{lines}NLCLIENT_PATCH", edit.offset),
                          Duration::from_secs(60 + edit.after.len() as u64 / 10_000), true)?;
        }
        self.run(&format!("truncate -s {} {staged} && test \"$(sha256sum {staged} | cut -d' ' -f1)\" = {} && mv -f {staged} {path}", patch.output_len, patch.output_sha))?;
        Ok(())
    }
    fn make_directory(&mut self, name: &str, mode: u32) -> Result<()> {
        let path = quote(&format!("/mnt/{name}"));
        self.run(&format!("mkdir -p {path} && chown 0:0 {path} && chmod {mode:o} {path}"))?;
        Ok(())
    }

    fn remove(&mut self, name: &str) -> Result<()> {
        self.run(&format!("rm -f {}", quote(&format!("/mnt/{name}"))))?;
        Ok(())
    }

    fn remount(&mut self, writable: bool) -> Result<()> {
        // Remounting read-only commits the UBIFS journal.
        self.run(if writable { "mount -o remount,rw /mnt" } else { "sync; mount -o remount,ro /mnt" })?;
        Ok(())
    }

    fn digests(&mut self, names: &[String]) -> Result<Digests> {
        let paths: Vec<String> = names.iter().map(|name| quote(&format!("/mnt/{name}"))).collect();
        let paths = paths.join(" ");
        self.run("sync; echo 3 > /proc/sys/vm/drop_caches")?;
        let sums: BTreeMap<String, String> = self.run(&format!("sha256sum {paths}"))?.lines()
            .filter_map(|line| line.split_once("  ").map(|(sha, path)| (path.to_string(), sha.to_string()))).collect();
        let modes: BTreeMap<String, String> = self.run(&format!("stat -c '%n %u:%g %a' {paths}"))?.lines()
            .filter_map(|line| line.split_once(' ').map(|(path, rest)| (path.to_string(), rest.to_string()))).collect();
        Ok(names.iter().map(|name| {
            let path = format!("/mnt/{name}");
            (name.clone(), (sums.get(&path).cloned(), modes.get(&path).cloned()))
        }).collect())
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
    }

    impl FakeShell {
        fn new(answers: Vec<(&'static str, &'static str, u8)>) -> Self {
            let used = vec![false; answers.len()];
            FakeShell { answers, used, input: Vec::new(), output: VecDeque::new(), scripts: Vec::new() }
        }
    }

    impl Write for FakeShell {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.input.extend_from_slice(data);
            let text = String::from_utf8_lossy(&self.input).into_owned();
            let marker = regex::Regex::new(r"(?s)^(.*)\nunfr0zen_rc=\$\?; echo; echo ([0-9a-f]{16}):\$unfr0zen_rc\n$").unwrap();
            if let Some(found) = marker.captures(&text) {
                let script = found[1].to_string();
                let matching: Vec<usize> = (0..self.answers.len())
                    .filter(|&i| script.starts_with(self.answers[i].0)).collect();
                let pick = matching.iter().copied().find(|&i| !self.used[i]).or(matching.last().copied());
                let (_, output, code) = pick.map_or(("", "", 0), |i| {
                    self.used[i] = true;
                    self.answers[i]
                });
                self.output.extend(format!("{output}\n{}:{code}\n", &found[2]).bytes());
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
        assert_eq!(shell.read("etc/shadow").unwrap(), Some(("0:0".into(), 0o640, b"secret".to_vec())));
        let log = LOG.0.lock().unwrap();
        assert!(log.contains("etc/shadow"));
        assert!(!log.contains("c2VjcmV0"));
    }

    #[test]
    fn absent_files_and_links() {
        let mut shell = shell(vec![("if [ -L \'/mnt/missing\' ]", "absent", 0), ("if [ -L \'/mnt/link\' ]", "link", 0)]);
        assert_eq!(shell.read("missing").unwrap(), None);
        assert!(shell.read("link").unwrap_err().to_string().contains("symbolic link"));
    }

    #[test]
    fn failing_commands_are_errors() {
        let mut shell = shell(vec![("rm -f", "rm: busy", 1)]);
        assert!(shell.remove("etc/x").unwrap_err().to_string().contains("rm: busy"));
    }

    #[test]
    fn writes_go_by_rename_from_the_payload_or_through_base64() {
        let mut shell = shell(vec![]);
        let blob = vec![0x5au8; 100];
        let payload = BTreeSet::from([sha256_hex(&blob)]);
        shell.write("nestlabs/sbin/nestlocal", &blob, "0:0", 0o755, &payload).unwrap();
        shell.write("etc/shadow", b"root:x\n", "0:42", 0o640, &payload).unwrap();
        let scripts = &shell.port.scripts;
        assert_eq!(scripts[1], format!("mkdir -p '/mnt/nestlabs/sbin' && cat /payload/{} > \
            '/mnt/nestlabs/sbin/nestlocal.unfr0zen' && chown 0:0 '/mnt/nestlabs/sbin/nestlocal.unfr0zen' && \
            chmod 755 '/mnt/nestlabs/sbin/nestlocal.unfr0zen' && mv -f '/mnt/nestlabs/sbin/nestlocal.unfr0zen' \
            '/mnt/nestlabs/sbin/nestlocal'", sha256_hex(&blob)));
        assert_eq!(scripts[2], "mkdir -p '/mnt/etc' && base64 -d > '/mnt/etc/shadow.unfr0zen' <<'UNFR0ZEN_EOF' && \
            chown 0:42 '/mnt/etc/shadow.unfr0zen' && chmod 640 '/mnt/etc/shadow.unfr0zen' && \
            mv -f '/mnt/etc/shadow.unfr0zen' '/mnt/etc/shadow'\ncm9vdDp4Cg==\nUNFR0ZEN_EOF");
    }

    #[test]
    fn native_patch_checks_hash_before_writing_and_before_rename() {
        let patch = crate::nlclient::Patch {
            stock_len: 3, output_len: 6, stock_sha: sha256_hex(b"abc"),
            output_sha: sha256_hex(b"ayc123"), edits: vec![
                crate::nlclient::Edit { offset: 1, before: b"b".to_vec(), after: b"y".to_vec() },
                crate::nlclient::Edit { offset: 3, before: vec![], after: b"123".to_vec() },
            ],
        };
        let mut connection = shell(vec![]);
        connection.patch_nlclient(&patch).unwrap();
        let scripts = &connection.port.scripts;
        assert!(scripts.iter().any(|s| s.contains("cp -p '/mnt/nestlabs/sbin/nlclient'")));
        assert!(scripts.last().unwrap().contains(&patch.output_sha));
        assert!(scripts.last().unwrap().contains("mv -f"));
        let mut wrong = shell(vec![("test", "wrong hash", 1)]);
        assert!(wrong.patch_nlclient(&patch).is_err());
        assert_eq!(wrong.port.scripts.len(), 2); // connect + failed precondition
    }

    /// The data a script's quoted here-document carries, base64 lines decoded.
    fn here_document(script: &str, delimiter: &str) -> Vec<u8> {
        let body = script.split_once('\n').unwrap().1.strip_suffix(delimiter).unwrap();
        assert!(body.lines().all(|line| line.len() <= 76), "{body}");
        STANDARD.decode(body.lines().collect::<String>()).unwrap()
    }

    #[test]
    fn native_patch_sends_each_edit_at_its_offset_then_truncates_and_checks() {
        let payload: Vec<u8> = (0..200u32).map(|i| (i * 31) as u8).collect();
        let patch = crate::nlclient::Patch {
            stock_len: 4096, output_len: 4096 + payload.len(), stock_sha: "a".repeat(64), output_sha: "b".repeat(64),
            edits: vec![crate::nlclient::Edit { offset: 7, before: b"b".to_vec(), after: b"y".to_vec() },
                        crate::nlclient::Edit { offset: 4096, before: vec![], after: payload.clone() }],
        };
        let mut connection = shell(vec![]);
        connection.patch_nlclient(&patch).unwrap();
        let scripts = &connection.port.scripts[1..];
        assert_eq!(scripts.len(), 4);
        let staged = "'/mnt/nestlabs/sbin/nlclient.unfr0zen'";
        for (script, (seek, data)) in scripts[1..3].iter().zip([(7, &b"y"[..]), (4096, &payload[..])]) {
            assert!(script.starts_with(&format!("base64 -d <<'NLCLIENT_PATCH' | dd of={staged} bs=1 seek={seek} conv=notrunc\n")));
            assert_eq!(here_document(script, "NLCLIENT_PATCH"), data);
        }
        assert_eq!(scripts[3], format!("truncate -s 4296 {staged} && test \"$(sha256sum {staged} | cut -d' ' -f1)\" = {} \
                                        && mv -f {staged} '/mnt/nestlabs/sbin/nlclient'", "b".repeat(64)));
    }

    #[test]
    fn a_failed_edit_never_reaches_the_live_nlclient() {
        let patch = crate::nlclient::Patch {
            stock_len: 3, output_len: 3, stock_sha: sha256_hex(b"abc"), output_sha: sha256_hex(b"ayc"),
            edits: vec![crate::nlclient::Edit { offset: 1, before: b"b".to_vec(), after: b"y".to_vec() }],
        };
        let mut connection = shell(vec![("base64 -d <<'NLCLIENT_PATCH'", "dd: can't open: No space left", 1)]);
        assert!(connection.patch_nlclient(&patch).is_err());
        assert!(!connection.port.scripts.iter().any(|s| s.contains("mv -f")));
    }

    #[test]
    fn large_writes_go_as_76_column_base64_that_decodes_back() {
        let data: Vec<u8> = (0..5000u32).map(|i| (i * 7 + i / 256) as u8).collect();
        let mut connection = shell(vec![]);
        connection.write("etc/nestlocal/server.key", &data, "0:0", 0o600, &BTreeSet::new()).unwrap();
        let script = &connection.port.scripts[1];
        assert!(script.ends_with("\nUNFR0ZEN_EOF"));
        assert_eq!(here_document(script, "UNFR0ZEN_EOF"), data);
    }

    #[test]
    fn no_line_of_data_ends_the_here_document_early() {
        // The second base64 line reads UNFR0ZEN.
        let data = [vec![0u8; 57], STANDARD.decode("UNFR0ZEN").unwrap()].concat();
        let mut connection = shell(vec![]);
        connection.write("etc/x", &data, "0:0", 0o644, &BTreeSet::new()).unwrap();
        let script = &connection.port.scripts[1];
        let delimiter = script.split_once("<<'").unwrap().1.split_once('\'').unwrap().0;
        let body: Vec<&str> = script.lines().skip(1).collect();
        assert_eq!(body.iter().position(|line| *line == delimiter), Some(body.len() - 1));
        assert_eq!(here_document(script, delimiter), data);
    }

    #[test]
    fn paths_reach_the_shell_as_single_words() {
        let mut connection = shell(vec![]);
        connection.remove("etc/a;reboot").unwrap();
        connection.write("etc/$(x)/*", b"x", "0:0", 0o644, &BTreeSet::new()).unwrap();
        connection.remove("etc/it's").unwrap();
        let scripts = &connection.port.scripts[1..];
        assert_eq!(scripts[0], "rm -f '/mnt/etc/a;reboot'");
        assert!(scripts[1].starts_with("mkdir -p '/mnt/etc/$(x)' && base64 -d > '/mnt/etc/$(x)/*.unfr0zen' <<"), "{}", scripts[1]);
        assert_eq!(scripts[2], r"rm -f '/mnt/etc/it'\''s'");
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
        assert_eq!(connection.read("bin/big").unwrap(), Some(("0:0".into(), 0o755, big)));
        assert!(connection.read("etc/garbled").unwrap_err().to_string().contains("kept arriving damaged"));
        assert!(connection.read("etc/odd").unwrap_err().to_string().contains("unexpected mode"));
        assert!(connection.read("etc/nosize").unwrap_err().to_string().contains("no size"));
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
        assert_eq!(connection.read("nestlabs/sbin/nlclient").unwrap().unwrap().2, data);
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
        assert!(connection.read("etc/a").unwrap_err().to_string().contains("changed while it was read"));
    }

    fn leak(text: String) -> &'static str {
        Box::leak(text.into_boxed_str())
    }

    #[test]
    fn inspect_and_directory_report_only_what_stat_says() {
        let mut connection = shell(vec![
            ("if [ -L \'/mnt/etc/a\' ]", "0:0 644\nabcd  /mnt/etc/a", 0),
            ("if [ -L \'/mnt/etc/b\' ]", "0:0 644", 0),
            ("if [ -d \'/mnt/etc/c\' ]", "other", 0),
            ("if [ -d \'/mnt/etc/d\' ]", "absent", 0),
            ("if [ -d \'/mnt/etc/e\' ]", "0:0 755", 0),
        ]);
        assert_eq!(connection.inspect("etc/a").unwrap(), Some(("0:0".into(), 0o644, "abcd".into())));
        assert!(connection.inspect("etc/b").unwrap_err().to_string().contains("no sha256sum"));
        assert!(connection.directory("etc/c").unwrap_err().to_string().contains("not a directory"));
        assert_eq!(connection.directory("etc/d").unwrap(), None);
        assert_eq!(connection.directory("etc/e").unwrap(), Some("0:0 755".into()));
    }

    #[test]
    fn digests_pair_hashes_and_modes_by_path() {
        let mut shell = shell(vec![("sha256sum", "aa  /mnt/etc/a\nbb  /mnt/etc/b", 0),
                                   ("stat -c", "/mnt/etc/a 0:0 644\n/mnt/etc/b 0:0 600", 0)]);
        let found = shell.digests(&["etc/a".into(), "etc/b".into()]).unwrap();
        assert_eq!(found["etc/b"], (Some("bb".into()), Some("0:0 600".into())));
    }

    #[test]
    fn file_size_comes_from_stat() {
        let mut connection = shell(vec![("stat -c %s", "4199955", 0)]);
        assert_eq!(connection.file_size("boot/kernel_fdt.itb").unwrap(), 4_199_955);
        let mut connection = shell(vec![("stat -c %s", "invalid", 0)]);
        assert!(connection.file_size("boot/kernel_fdt.itb").is_err());
    }

    #[test]
    fn free_space_comes_from_df() {
        let mut shell = shell(vec![("df -Pk", "ubi0:root0 102400 61440 40960 60% /mnt", 0)]);
        assert_eq!(shell.free_bytes().unwrap(), 40960 * 1024);
    }
}
