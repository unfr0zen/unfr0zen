//! Pushes the app's nestlocal to a unit set up with --update-api, over Wi-Fi,
//! as a sealed update set of the one file (session.rs); what the unit reports
//! after its restart is unauthenticated.

use std::time::{Duration, Instant};

use unfr0zen_core::diamond3::scratch;
use unfr0zen_core::shell::sha256_hex;
use unfr0zen_core::Result;
use update_wire::bundle::{in_scratch, Bundle, Class, Entry, Kind, BUNDLE_VERSION, SCRATCH_NESTLOCAL};
use update_wire::read::{Found, ReadRequest};

use crate::flow::failure;
use crate::session::{self, Sent, Timing, UNIT_TIMING, UNPROVEN};
use crate::trust;
pub use crate::session::{address, is_code, Code, CLOSED};

/// Where nestlocal is installed on a unit that does not say (Status::binary).
const NESTLOCAL: &str = update_wire::bundle::ROOT_NESTLOCAL;
/// What nestlocal marks its binary with (wifi_update/mod.rs IDENTITY).
const IDENTITY: &[u8] = b"\0unfr0zen-nestlocal ";
/// Why a push is refused before the code is tried.
pub(crate) const ON_TRIAL: &str = "the unit's last update is still on trial: start this again once it is kept or rolled back";
/// How a push refused for the unit's overlay begins: a settings run over
/// Wi-Fi brings the overlay with the binary.
pub const NEEDS_OVERLAY: &str = "this app's nestlocal needs files a newer USB install puts on the unit";

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// A nestlocal binary's version and the overlay version it needs, as it
/// marks itself; none for a binary without the mark.
pub fn identity(binary: &[u8]) -> (Option<String>, Option<u32>) {
    let Some(at) = binary.windows(IDENTITY.len()).position(|w| w == IDENTITY) else {
        return (None, None);
    };
    let rest = &binary[at + IDENTITY.len()..];
    let text = String::from_utf8_lossy(&rest[..rest.iter().take(256).position(|&b| b == 0).unwrap_or(rest.len().min(256))]);
    let mut version = None;
    let mut overlay = None;
    for field in text.split_whitespace() {
        match field.split_once('=') {
            Some(("version", v)) => version = Some(v.to_string()),
            Some(("overlay", v)) => overlay = v.parse().ok(),
            _ => {}
        }
    }
    (version, overlay)
}

/// A version to order by: vX.Y.Z, and the commits after it (vX.Y.Z-N-gHASH);
/// none for one without a release tag.
fn version_key(version: &str) -> Option<[u64; 4]> {
    let (release, after) = match version.strip_prefix('v')?.split_once('-') {
        Some((release, after)) => (release, after.split('-').next()?.parse().ok()?),
        None => (version.strip_prefix('v')?, 0),
    };
    let numbers: Vec<u64> = release.split('.').map(|n| n.parse().ok()).collect::<Option<_>>()?;
    let [major, minor, patch] = numbers[..] else { return None };
    Some([major, minor, patch, after])
}

/// Sends `binary` to the unit at `host`, then waits until it runs and the unit
/// keeps it; reports each step through `progress`. Unless forced, it refuses a
/// nestlocal older than the unit's, or one needing files the unit lacks.
pub fn push(host: &str, code: Code, binary: &[u8], force: bool, progress: &dyn Fn(&str)) -> Result<String> {
    push_to(&address(host)?, code, binary, force, progress, &UNIT_TIMING, &trust::Store::default())
}

fn push_to(addr: &str, code: Code, binary: &[u8], force: bool, progress: &dyn Fn(&str), timing: &Timing, trusted: &trust::Store)
           -> Result<String> {
    if let Code::Given(code) = code {
        if !is_code(code) {
            return Err(failure("the code is the 8 digits the thermostat shows after Allow, or on Settings → UPDATE → Allow update"));
        }
    }
    let (agent, url) = (session::agent(), session::url(addr));
    let sha = sha256_hex(binary);

    progress(&format!("Connecting to nestlocal at {addr}"));
    let before = session::reach(&agent, &url, addr, timing)?;
    if before.running.as_deref() == Some(sha.as_str()) {
        if !before.pending {
            return Ok(format!("already running {}", short(&sha)));
        }
        // An earlier push of it, not yet kept: the guard may still roll it back.
        progress(&format!("nestlocal {} is running but not yet kept; waiting", short(&sha)));
    } else {
        // The unit takes no set while another is on trial.
        if before.pending {
            return Err(failure(ON_TRIAL));
        }
        check_versions(&before, binary, force, progress)?;
        let session = session::authenticate(&agent, addr, code, &before, progress, timing, trusted)?;
        progress(&format!("Sending nestlocal {} ({} bytes)", short(&sha), binary.len()));
        // Over the one installed, on the scratch volume or the root volume.
        let path = before.binary.as_deref().unwrap_or(NESTLOCAL);
        let entry = Entry { path: path.into(), kind: Kind::Write, owner: "0:0".into(), mode: 0o755,
                            old: before.installed.clone(), new: Some(sha.clone()), size: binary.len() as u64 };
        // On scratch, with the manifest the unit checks its files against.
        let manifest = if in_scratch(path) { scratch_manifest(&session, &sha)? } else { None };
        let mut entries = vec![entry];
        let mut contents = vec![binary];
        if let Some((entry, data)) = &manifest {
            entries.push(entry.clone());
            contents.push(data);
        }
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        if let Err(error) = crate::wifi_patch::check_set(&session, || session.info(), &paths) {
            session.close();
            return Err(error);
        }
        let set = Bundle { version: BUNDLE_VERSION, class: Class::Restart, force, entries };
        match session.apply(&set, &contents) {
            Ok(applied) if applied.files.iter().any(|(applied_path, _, applied)| applied_path == path && *applied == sha) => {
                progress("Installed; waiting for the new nestlocal to run");
            }
            Ok(_) => return Err(failure(UNPROVEN)),
            Err(Sent::Refused(error)) => return Err(error),
            // The unit finishes a set it has all of, client or not.
            Err(Sent::Lost(error)) => {
                if !took(&agent, &url, &sha, timing) {
                    return Err(failure(format!("sending the update failed: {error}")));
                }
                progress("The unit took the upload; waiting for the new nestlocal to run");
            }
        }
    }
    let start = Instant::now();
    let mut on_trial = false;
    let mut answered = true;
    while start.elapsed() < timing.wait {
        std::thread::sleep(timing.poll);
        // The unit drops off while nestlocal restarts.
        let Ok(now) = session::get(&agent, &url, timing)? else {
            answered = false;
            continue;
        };
        answered = true;
        on_trial = now.pending;
        if now.pending {
            continue;
        }
        if now.running.as_deref() == Some(sha.as_str()) && now.installed.as_deref() == Some(sha.as_str()) {
            return Ok(format!("nestlocal {} is running", short(&sha)));
        }
        if now.installed.as_deref() != Some(sha.as_str()) {
            // The guard puts back the binary the set replaced; another one
            // means a later update replaced this one.
            if now.cancelled_on_thermostat() {
                return Err(failure(session::PUT_BACK));
            }
            return Err(match now.installed {
                Some(other) if before.installed.as_deref() != Some(other.as_str()) =>
                    failure(format!("the unit now runs another nestlocal ({}): this update was rolled back or replaced", short(&other))),
                _ => failure("the unit rolled the update back"),
            });
        }
    }
    let minutes = timing.wait.as_secs() / 60;
    Err(failure(if !answered {
        format!("the unit stopped answering (asleep, or off the network) before it kept nestlocal {}: \
                 start this again later to see whether it did", short(&sha))
    } else if on_trial {
        format!("nestlocal {} runs on trial: nlclient has not reached it after {minutes} minutes, \
                 and the unit rolls it back if it does not", short(&sha))
    } else {
        format!("nestlocal {} was installed but is not running after {minutes} minutes", short(&sha))
    }))
}

/// The unit's scratch manifest with nestlocal's line for `sha`, as a setup
/// writes it, and the entry that writes it; none on a unit without one, or
/// whose manifest has that line already.
fn scratch_manifest(session: &session::Session, sha: &str) -> Result<Option<(Entry, Vec<u8>)>> {
    let request = ReadRequest { names: vec![scratch::MANIFEST.into()], contents: true };
    let Some(Found::File { sha256, data: Some(data), .. }) = session.read(&request)?.into_iter().next() else {
        return Ok(None);
    };
    let manifest = with_nestlocal(&String::from_utf8_lossy(&data), sha);
    if manifest == data {
        return Ok(None);
    }
    let entry = Entry { path: scratch::MANIFEST.into(), kind: Kind::Write, owner: "0:0".into(), mode: 0o644,
                        old: Some(sha256), new: Some(sha256_hex(&manifest)), size: manifest.len() as u64 };
    Ok(Some((entry, manifest)))
}

/// `manifest` (scratch::manifest's `sha256  name` lines) with nestlocal's
/// line for `sha`.
fn with_nestlocal(manifest: &str, sha: &str) -> Vec<u8> {
    let named = |name: &str| format!("{}/{name}", scratch::DIR);
    let mut files: Vec<(String, String)> = manifest.lines().filter_map(|line| line.split_once("  "))
        .map(|(sha, name)| (named(name), sha.to_string())).filter(|(name, _)| name != SCRATCH_NESTLOCAL).collect();
    files.push((SCRATCH_NESTLOCAL.into(), sha.into()));
    scratch::manifest(files.iter().map(|(name, sha)| (name.as_str(), sha.clone())))
}

/// Refuses, unless forced, a nestlocal older than the unit's or one needing
/// files a newer USB patch installs.
fn check_versions(unit: &session::Status, binary: &[u8], force: bool, progress: &dyn Fn(&str)) -> Result<()> {
    let (ours, needs) = identity(binary);
    let theirs = unit.version.as_deref().unwrap_or("unknown");
    progress(&format!("The unit runs nestlocal {theirs}; this app carries nestlocal {}", ours.as_deref().unwrap_or("unknown")));
    if force {
        return Ok(());
    }
    if let Some(needs) = needs {
        if unit.overlay.is_none_or(|have| have < needs) {
            let have = unit.overlay.map_or("none".to_string(), |have| have.to_string());
            return Err(failure(format!("{NEEDS_OVERLAY} (overlay version {needs}; the unit has {have}): patch the unit \
                                        over USB, send its settings over Wi-Fi, or force the push")));
        }
    }
    if let Some(ours) = ours.as_deref() {
        if newer_than(theirs, ours) {
            return Err(failure(format!("the unit runs nestlocal {theirs}, newer than this app's {ours}: \
                                        force the push to install the older one")));
        }
    }
    Ok(())
}

/// Whether the nestlocal `theirs` is newer than `ours`: true only when both
/// have a release tag to order by. Update refuses to install an older one.
pub fn newer_than(theirs: &str, ours: &str) -> bool {
    version_key(ours).zip(version_key(theirs)).is_some_and(|(ours, theirs)| ours < theirs)
}

/// After a lost upload: whether the unit installed it, as soon as it says.
fn took(agent: &ureq::Agent, url: &str, sha: &str, timing: &Timing) -> bool {
    let start = Instant::now();
    while start.elapsed() < timing.connect {
        if let Ok(Ok(status)) = session::get(agent, url, timing) {
            return status.installed.as_deref() == Some(sha);
        }
        std::thread::sleep(timing.poll.min(Duration::from_secs(1)));
    }
    false
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use spake2::{Ed25519Group, Identity, Password, Spake2};
    use update_wire::bundle::Applied;
    use update_wire::read::{CheckRequest, Checked, Found, Info, ReadRequest, Refused, Rule};
    use update_wire::seal;

    use update_wire::auth::{sign, verify, CLIENT, CONFIRMATION, UNIT};
    use update_wire::{hex, unhex};

    use crate::session::{ALLOW, DENIED, HELD, NO_CODE, OPENED_TOO_OFTEN, PREEMPTED, STALE, TAKEN, UNANSWERED, WRONG};

    pub(crate) const CODE: &str = "40718263";
    const NEW: &[u8] = b"NESTLOCAL";

    /// A binary marked as nestlocal marks itself.
    fn marked(version: &str, overlay: u32) -> Vec<u8> {
        format!("ELF...\0unfr0zen-nestlocal version={version} overlay={overlay}\n\0...").into_bytes()
    }

    #[test]
    fn matches_the_shared_test_vector() {
        let key: Vec<u8> = (0..32).collect();
        assert_eq!(sha256_hex(NEW), "c1b271f91fde94d0e910f18c2f21f8622688d96064c33b2525e1fa47f714753f");
        assert!(verify(&key, CONFIRMATION, "4653a539f87f1a0685eb47d8a0c86be186d5812eabf3d1b8fc734b4c32e33453"));
        assert_eq!((CLIENT, UNIT), (&b"unfr0zen"[..], &b"nestlocal"[..]));
    }

    #[test]
    fn the_unit_is_reached_on_ipv4_or_a_name() {
        assert_eq!(address("192.168.1.50").unwrap(), "192.168.1.50:8443");
        assert_eq!(address("127.0.0.1:9000").unwrap(), "127.0.0.1:9000");
        assert_eq!(address("nest.lan").unwrap(), "nest.lan:8443");
        assert_eq!(address("nest-1.lan:8443").unwrap(), "nest-1.lan:8443");
        for bad in ["fe80::1", "[fe80::1]", "[fe80::1]:8443", "http://192.168.1.50", "nest.lan/update", "a%b", "", "nest:port", "-x.lan"] {
            assert!(address(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_binary_s_mark_gives_its_version_and_the_overlay_it_needs() {
        assert_eq!(identity(&marked("v0.4.1-10-g4ef6d08", 1)), (Some("v0.4.1-10-g4ef6d08".into()), Some(1)));
        assert_eq!(identity(NEW), (None, None));
        assert_eq!(version_key("v0.4.1"), Some([0, 4, 1, 0]));
        assert_eq!(version_key("v0.4.1-10-g4ef6d08"), Some([0, 4, 1, 10]));
        assert!(version_key("v0.4.1-10-g4ef6d08") > version_key("v0.4.1-9-gabcdef0"));
        assert!(version_key("v0.10.0") > version_key("v0.9.9-30-gabcdef0"));
        for unordered in ["4ef6d08", "unknown", "v1.2", "0.4.1"] {
            assert_eq!(version_key(unordered), None, "{unordered}");
        }
    }

    /// What the fake unit does once an upload is installed.
    #[derive(Clone, Copy, PartialEq)]
    enum After {
        Runs,
        RollsBack,
        /// Stays on trial: nlclient never reaches it.
        StaysOnTrial,
        /// Another push replaces it.
        Replaced,
    }

    /// A scripted unit: nestlocal's update API, one request per connection.
    struct Unit {
        open: bool,
        installed: String,
        running: String,
        /// What the guard puts back: the binary the update replaced.
        previous: String,
        pending: bool,
        version: Option<String>,
        overlay: Option<u32>,
        /// The outstanding session and its key.
        session: Option<(String, Vec<u8>)>,
        exchanges: u32,
        after: After,
        /// Someone else makes an exchange after each of ours.
        rival: bool,
        /// The answer to the apply is not the unit's.
        forged: bool,
        /// The connection drops once the body is in.
        drops: bool,
        /// The refusal of a set that opened, sent once the body is in.
        refuses: Option<(u16, &'static str)>,
        /// The thermostat asks no more (its strikes).
        struck: bool,
        /// Prompts raised, the names they were for, the status checks while
        /// one asked, the last one's id and state ("asking", then its
        /// answer), and what its answer is.
        opened: u32,
        names: Vec<String>,
        asked: u32,
        prompt_id: String,
        prompt: Option<&'static str>,
        answer: &'static str,
        /// Another laptop's prompt replaces ours once ours is answered.
        taken: bool,
        /// Status checks since the install, and how many the restart takes.
        polls: u32,
        /// The fake rolled the update back, and who it says asked.
        rolled_back: bool,
        rolled_back_by: Option<&'static str>,
        /// Contents received, the sets they came in, and refusals sent
        /// before one.
        received: Vec<Vec<u8>>,
        sets: Vec<Bundle>,
        refused: Vec<u16>,
        /// Where it says nestlocal is installed, and its scratch manifest,
        /// replaced by the one a set brings.
        binary: Option<&'static str>,
        manifest: Option<Vec<u8>>,
        /// What it answers a check with, path and why; None for a
        /// nestlocal without one (404). The paths checked.
        refuses_check: Option<Vec<(&'static str, &'static str)>>,
        checked: Vec<String>,
    }

    impl Unit {
        fn new(open: bool, after: After) -> Unit {
            let old = sha256_hex(b"OLD");
            Unit { open, installed: old.clone(), running: old.clone(), previous: old, pending: false, version: None, overlay: Some(1),
                   session: None, exchanges: 0, after, rival: false, forged: false, drops: false, refuses: None, struck: false, opened: 0,
                   names: Vec::new(), asked: 0, prompt_id: String::new(), prompt: None, answer: "allowed", taken: false, polls: 0, rolled_back: false, rolled_back_by: None, received: Vec::new(), sets: Vec::new(),
                   refused: Vec::new(), binary: None, manifest: None, refuses_check: Some(Vec::new()), checked: Vec::new() }
        }

        fn status(&mut self) -> String {
            if self.prompt == Some("asking") {
                self.asked += 1;
                if self.asked == 2 {
                    self.prompt = Some(self.answer);
                    self.open = self.answer == "allowed";
                    if self.taken {
                        self.prompt_id = "c".repeat(32);
                    }
                }
            }
            if self.pending {
                self.polls += 1;
                if self.polls == 2 {
                    match self.after {
                        After::Runs => (self.running, self.pending) = (self.installed.clone(), false),
                        After::RollsBack => (self.installed, self.running, self.pending, self.rolled_back) =
                            (self.previous.clone(), self.previous.clone(), false, true),
                        After::StaysOnTrial => self.running = self.installed.clone(),
                        After::Replaced => {
                            let other = sha256_hex(b"OTHER");
                            (self.installed, self.running, self.pending) = (other.clone(), other, false);
                        }
                    }
                }
            }
            serde_json::json!({"open": self.open, "installed": self.installed, "running": self.running, "pending": self.pending,
                               "version": self.version, "overlay": self.overlay, "rolled_back": self.rolled_back, "binary": self.binary,
                               "rolled_back_by": self.rolled_back_by.filter(|_| self.rolled_back),
                               "prompt": self.prompt.map(|state| serde_json::json!({"id": self.prompt_id, "state": state}))}).to_string()
        }

        /// The unit's side of an exchange, as window.rs makes it.
        fn exchange(&mut self, message: &[u8]) -> String {
            self.exchanges += 1;
            let (spake, ours) = Spake2::<Ed25519Group>::start_b(&Password::new(CODE.as_bytes()), &Identity::new(CLIENT), &Identity::new(UNIT));
            let key = spake.finish(message).unwrap();
            let session = format!("{:032x}", self.exchanges);
            let confirm = sign(&key, CONFIRMATION);
            self.session = Some((session.clone(), key));
            if self.rival {
                self.session = Some(("rival".into(), Vec::new()));
            }
            serde_json::json!({"session": session, "message": hex(&ours), "confirm": confirm}).to_string()
        }
    }

    pub(crate) fn reply(stream: &mut std::net::TcpStream, status: u16, body: &str) {
        reply_bytes(stream, status, body.as_bytes());
    }

    pub(crate) fn reply_bytes(stream: &mut std::net::TcpStream, status: u16, body: &[u8]) {
        let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        let _ = stream.write_all(body);
    }

    /// The next u32 BE length and the bytes it counts, from body at at.
    fn part<'a>(body: &'a [u8], at: &mut usize) -> &'a [u8] {
        let length = u32::from_be_bytes(body[*at..*at + 4].try_into().unwrap()) as usize;
        *at += 4 + length;
        &body[*at - length..*at]
    }

    /// An apply's body opened as nestlocal opens it: the set, its contents,
    /// and the AAD its answer is sealed under; none when the set does not
    /// open under key.
    pub(crate) fn opened(body: &[u8], session: &str, key: &[u8]) -> Option<(Bundle, Vec<u8>, Vec<u8>)> {
        let keys = seal::keys(key);
        let aad = format!("nestlocal-update /update/apply\n{session}").into_bytes();
        let mut at = 0;
        let header = part(body, &mut at);
        let set: Bundle = serde_json::from_slice(&seal::open(&keys.to_unit, &aad, header).ok()?).unwrap();
        let answer_aad = [aad.as_slice(), seal::nonce_of(header).unwrap()].concat();
        let prefix: [u8; 7] = body[at..at + 7].try_into().unwrap();
        at += 7;
        let mut opener = seal::StreamOpener::new(&keys.to_unit, &answer_aad, prefix);
        let mut contents = Vec::new();
        loop {
            let (data, last) = opener.chunk(part(body, &mut at)).unwrap();
            contents.extend_from_slice(&data);
            if last {
                break;
            }
        }
        assert_eq!(at, body.len(), "nothing follows the last chunk");
        Some((set, contents, answer_aad))
    }

    /// A sealed request opened as nestlocal opens it, and its answer sealed.
    pub(crate) fn sealed_answer(body: &[u8], path: &str, session: &str, key: &[u8], mut answer: impl FnMut(&[u8]) -> Vec<u8>) -> Option<Vec<u8>> {
        let keys = seal::keys(key);
        let aad = format!("nestlocal-update {path}\n{session}").into_bytes();
        let request = seal::open(&keys.to_unit, &aad, body).ok()?;
        let answer_aad = [aad.as_slice(), seal::nonce_of(body).unwrap()].concat();
        Some(seal::seal(&keys.to_client, &answer_aad, &answer(&request)))
    }

    /// Serves `unit` on 127.0.0.1; its address, and the unit to inspect.
    fn serve(unit: Unit) -> (String, Arc<Mutex<Unit>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let unit = Arc::new(Mutex::new(unit));
        let shared = unit.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = Vec::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    head.push(line.trim_end().to_string());
                }
                let Some(request) = head.first() else { continue };
                let header = |name: &str| head.iter().find_map(|line| line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.trim().to_string()));
                let length = header("Content-Length").map_or(0, |n| n.parse().unwrap());
                let mut unit = shared.lock().unwrap();
                if request.starts_with("GET /update ") {
                    let body = unit.status();
                    reply(&mut stream, 200, &body);
                    continue;
                }
                if request.starts_with("POST /update/open ") {
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let name: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    unit.names.push(name["name"].as_str().unwrap().to_string());
                    if unit.struck {
                        reply(&mut stream, 429, "open Settings → UPDATE on the thermostat and press Allow update");
                    } else if unit.open {
                        reply(&mut stream, 200, "{\"open\":true}");
                    } else {
                        (unit.opened, unit.asked, unit.prompt) = (unit.opened + 1, 0, Some("asking"));
                        unit.prompt_id = format!("{:032x}", unit.opened);
                        let body = serde_json::json!({"prompt": "asking", "id": unit.prompt_id}).to_string();
                        reply(&mut stream, 202, &body);
                    }
                    continue;
                }
                if request.starts_with("POST /update/session ") {
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    if !unit.open {
                        reply(&mut stream, 403, "window closed");
                    } else {
                        let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let answer = unit.exchange(&unhex(request["message"].as_str().unwrap()).unwrap());
                        reply(&mut stream, 200, &answer);
                    }
                    continue;
                }
                if let Some(path) = ["/update/info", "/update/read", "/update/check", "/update/close"].into_iter()
                    .find(|path| request.starts_with(&format!("POST {path} "))) {
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let refuses_check = unit.refuses_check.clone();
                    let mut checked = Vec::new();
                    let answer = match unit.session.clone() {
                        _ if path == "/update/check" && refuses_check.is_none() => (404, b"not found".to_vec()),
                        _ if !unit.open => (403, b"window closed".to_vec()),
                        Some((session, key)) if Some(&session) == header("X-Update-Session").as_ref() =>
                            sealed_answer(&body, path, &session, &key, |request| match path {
                                "/update/close" => b"{}".to_vec(),
                                "/update/check" => {
                                    let request: CheckRequest = serde_json::from_slice(request).unwrap();
                                    checked = request.paths;
                                    let refused = refuses_check.iter().flatten()
                                        .map(|(path, why)| Refused { path: path.to_string(), why: why.to_string(), rule: Rule::NoScratch })
                                        .collect();
                                    serde_json::to_vec(&Checked { refused }).unwrap()
                                }
                                "/update/info" => serde_json::to_vec(&Info {
                                    nlmodel: "Display-3.4".into(), version: "v0.4.1".into(), overlay: Some(1),
                                    free_bytes: 1 << 20, scratch_free_bytes: None, shell_binary: "0 755".into(), pending: false,
                                }).unwrap(),
                                _ => {
                                    let read: ReadRequest = serde_json::from_slice(request).unwrap();
                                    let found: Vec<Found> = read.names.iter().map(|name| match name.as_str() {
                                        "etc/nestlocal/options" => Found::File {
                                            owner: "0:0".into(), mode: 0o644, sha256: sha256_hex(b"-mqtt x\n"), size: 8,
                                            data: read.contents.then(|| b"-mqtt x\n".to_vec()),
                                        },
                                        scratch::MANIFEST if unit.manifest.is_some() => {
                                            let manifest = unit.manifest.clone().unwrap();
                                            Found::File { owner: "0:0".into(), mode: 0o644, sha256: sha256_hex(&manifest),
                                                          size: manifest.len() as u64, data: read.contents.then_some(manifest) }
                                        }
                                        _ => Found::Absent,
                                    }).collect();
                                    serde_json::to_vec(&found).unwrap()
                                }
                            }).map_or((401, Vec::new()), |answer| (200, answer)),
                        _ => (412, b"the session was replaced or expired".to_vec()),
                    };
                    unit.checked.extend(checked);
                    reply_bytes(&mut stream, answer.0, &answer.1);
                    continue;
                }
                assert!(request.starts_with("POST /update/apply "), "{request}");
                let refusal = match unit.session.take() {
                    _ if !unit.open => Some((403, "window closed")),
                    Some((session, key)) if Some(&session) == header("X-Update-Session").as_ref() => {
                        unit.session = Some((session, key));
                        None
                    }
                    _ => Some((412, "the session was replaced or expired")),
                };
                if let Some((status, text)) = refusal {
                    unit.refused.push(status);
                    reply(&mut stream, status, text);
                    continue;
                }
                if header("Expect").is_some() {
                    stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").unwrap();
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let (session, key) = unit.session.take().unwrap();
                let Some((set, contents, answer_aad)) = opened(&body, &session, &key) else {
                    unit.refused.push(401);
                    reply(&mut stream, 401, "authentication failed");
                    continue;
                };
                // The set opened: the window is spent, whatever becomes of it.
                unit.open = false;
                if unit.pending {
                    unit.refused.push(409);
                    reply(&mut stream, 409, "the previous update is still on trial");
                    continue;
                }
                if let Some((status, text)) = unit.refuses {
                    unit.refused.push(status);
                    reply(&mut stream, status, text);
                    continue;
                }
                // The binary, and the scratch manifest with it.
                let (entry, rest) = set.entries.split_first().unwrap();
                let (binary, mut contents) = contents.split_at(entry.size as usize);
                let sha = sha256_hex(binary);
                assert_eq!((entry.path.as_str(), entry.new.as_deref(), entry.old.as_deref(), set.class),
                           (unit.binary.unwrap_or(NESTLOCAL), Some(sha.as_str()), Some(unit.installed.as_str()), Class::Restart));
                let mut files = vec![(entry.path.clone(), "0:0 755".to_string(), sha.clone())];
                for entry in rest {
                    assert_eq!((entry.path.as_str(), entry.old.as_ref()),
                               (scratch::MANIFEST, unit.manifest.as_ref().map(|manifest| sha256_hex(manifest)).as_ref()));
                    let data;
                    (data, contents) = contents.split_at(entry.size as usize);
                    assert_eq!(entry.new.as_deref(), Some(sha256_hex(data).as_str()));
                    unit.manifest = Some(data.to_vec());
                    files.push((entry.path.clone(), "0:0 644".into(), sha256_hex(data)));
                }
                assert!(contents.is_empty(), "nothing beyond the set's files");
                unit.received.push(binary.to_vec());
                unit.sets.push(set.clone());
                unit.previous = unit.installed.clone();
                (unit.installed, unit.pending, unit.polls) = (sha.clone(), true, 0);
                if unit.drops {
                    continue;
                }
                let applied = Applied { files, directories: Vec::new() };
                let to_client = if unit.forged { [0; 32] } else { seal::keys(&key).to_client };
                reply_bytes(&mut stream, 202, &seal::seal(&to_client, &answer_aad, &serde_json::to_vec(&applied).unwrap()));
            }
        });
        (addr, unit)
    }

    const FAST: Timing = Timing { connect: Duration::from_secs(2), poll: Duration::from_millis(10), wait: Duration::from_secs(5),
                                  request: Duration::from_secs(2), knock: 9 };

    fn push_binary(unit: Unit, code: Code, binary: &[u8], force: bool) -> (Result<String>, Arc<Mutex<Unit>>) {
        let (addr, unit) = serve(unit);
        (push_to(&addr, code, binary, force, &|_| {}, &FAST, &trust::Store::scratch()), unit)
    }

    fn push(unit: Unit, code: Code) -> (Result<String>, Arc<Mutex<Unit>>) {
        push_binary(unit, code, NEW, false)
    }

    #[test]
    fn pushes_and_waits_until_the_new_binary_runs() {
        let (result, unit) = push(Unit::new(true, After::Runs), Code::Given(CODE));
        assert_eq!(result.unwrap(), format!("nestlocal {} is running", &sha256_hex(NEW)[..12]));
        let unit = unit.lock().unwrap();
        assert_eq!((unit.received.as_slice(), unit.exchanges, unit.opened), (&[NEW.to_vec()][..], 1, 0));
    }

    #[test]
    fn reports_a_closed_window_for_a_given_code() {
        let (result, unit) = push(Unit::new(false, After::Runs), Code::Given(CODE));
        assert_eq!(result.unwrap_err().to_string(), CLOSED);
        let unit = unit.lock().unwrap();
        assert!(unit.exchanges == 0 && unit.opened == 0, "nothing is tried in a closed window");
    }

    #[test]
    fn reports_a_wrong_code_before_sending_anything() {
        let (result, unit) = push(Unit::new(true, After::Runs), Code::Given("12345678"));
        assert_eq!(result.unwrap_err().to_string(), WRONG);
        let unit = unit.lock().unwrap();
        assert!(unit.received.is_empty() && unit.refused.is_empty() && unit.exchanges == 1);
    }

    #[test]
    fn reports_a_session_another_client_replaced() {
        let (result, unit) = push(Unit { rival: true, ..Unit::new(true, After::Runs) }, Code::Given(CODE));
        assert_eq!(result.unwrap_err().to_string(), STALE);
        let unit = unit.lock().unwrap();
        // The check before the set already finds the session replaced.
        assert!(unit.received.is_empty() && unit.sets.is_empty() && unit.refused.is_empty());
    }

    #[test]
    fn a_cancel_on_the_thermostat_is_reported_and_not_retried() {
        let unit = Unit { refuses: Some((409, "cancelled on the thermostat")), ..Unit::new(true, After::Runs) };
        let (result, unit) = push(unit, Code::Given(CODE));
        assert_eq!(result.unwrap_err().to_string(), session::CANCELLED);
        let unit = unit.lock().unwrap();
        assert_eq!((unit.exchanges, unit.refused.as_slice(), unit.received.len()), (1, &[409][..], 0));
    }

    #[test]
    fn refuses_an_answer_the_unit_did_not_prove() {
        let (result, _) = push(Unit { forged: true, ..Unit::new(true, After::Runs) }, Code::Given(CODE));
        assert_eq!(result.unwrap_err().to_string(), UNPROVEN);
    }

    #[test]
    fn an_upload_lost_once_its_body_is_in_is_followed_to_the_end() {
        let (result, unit) = push(Unit { drops: true, ..Unit::new(true, After::Runs) }, Code::Given(CODE));
        assert_eq!(result.unwrap(), format!("nestlocal {} is running", &sha256_hex(NEW)[..12]));
        assert_eq!(unit.lock().unwrap().received, [NEW]);
    }

    #[test]
    fn the_app_waits_for_allow_then_asks_for_the_code() {
        let (addr, unit) = serve(Unit::new(false, After::Runs));
        let seen = unit.clone();
        let ask = move |prompt: &str| {
            let unit = seen.lock().unwrap();
            assert!(unit.open && unit.prompt == Some("allowed") && unit.prompt_id == format!("{:032x}", 1),
                    "asked before the thermostat allowed it");
            assert_eq!(prompt, "Enter the code the thermostat shows");
            Some(CODE.to_string())
        };
        let said = RefCell::new(Vec::new());
        let result = push_to(&addr, Code::Ask(&ask), NEW, false, &|line| said.borrow_mut().push(line.to_string()), &FAST,
                             &trust::Store::scratch());
        assert_eq!(result.unwrap(), format!("nestlocal {} is running", &sha256_hex(NEW)[..12]));
        assert!(said.borrow().iter().any(|line| line == ALLOW), "{said:?}");
        let unit = unit.lock().unwrap();
        assert!(unit.asked >= 1, "the app polled while the thermostat asked");
        assert_eq!((unit.opened, unit.exchanges, unit.received.len()), (1, 1, 1));
        assert_eq!(unit.names, [session::laptop_name()]);
    }

    #[test]
    fn a_denied_prompt_ends_the_push() {
        let unit = Unit { answer: "denied", ..Unit::new(false, After::Runs) };
        let (result, unit) = push(unit, Code::Ask(&|_| panic!("asked for a code")));
        assert_eq!(result.unwrap_err().to_string(), DENIED);
        let unit = unit.lock().unwrap();
        assert!(unit.exchanges == 0 && unit.received.is_empty() && !unit.open);
    }

    #[test]
    fn another_laptop_s_prompt_is_not_taken_for_ours() {
        let unit = Unit { taken: true, ..Unit::new(false, After::Runs) };
        let (result, unit) = push(unit, Code::Ask(&|_| panic!("asked for a code")));
        assert_eq!(result.unwrap_err().to_string(), TAKEN);
        let unit = unit.lock().unwrap();
        assert!(unit.open && unit.exchanges == 0, "the window the other laptop's Allow opened is left alone");
    }

    #[test]
    fn an_allow_while_another_update_held_the_unit_ends_the_push() {
        let unit = Unit { answer: "busy", ..Unit::new(false, After::Runs) };
        let (result, unit) = push(unit, Code::Ask(&|_| panic!("asked for a code")));
        assert_eq!(result.unwrap_err().to_string(), HELD);
        assert_eq!(unit.lock().unwrap().exchanges, 0);
    }

    #[test]
    fn a_prompt_a_trusted_session_took_the_thermostat_from_ends_the_push() {
        let unit = Unit { answer: "preempted", ..Unit::new(false, After::Runs) };
        let (result, unit) = push(unit, Code::Ask(&|_| panic!("asked for a code")));
        assert_eq!(result.unwrap_err().to_string(), PREEMPTED);
        assert_eq!(unit.lock().unwrap().exchanges, 0);
    }

    #[test]
    fn a_prompt_nobody_answers_ends_with_its_message() {
        let unit = Unit { answer: "timed_out", ..Unit::new(false, After::Runs) };
        let (result, unit) = push(unit, Code::Ask(&|_| panic!("asked for a code")));
        assert_eq!(result.unwrap_err().to_string(), UNANSWERED);
        assert_eq!(unit.lock().unwrap().exchanges, 0);
    }

    #[test]
    fn asks_again_after_a_wrong_or_malformed_code() {
        let answers = RefCell::new(vec!["4071826", "12345678", CODE].into_iter());
        let prompts = RefCell::new(Vec::new());
        let ask = |prompt: &str| {
            prompts.borrow_mut().push(prompt.to_string());
            answers.borrow_mut().next().map(str::to_string)
        };
        let (result, unit) = push(Unit::new(true, After::Runs), Code::Ask(&ask));
        assert!(result.is_ok(), "{result:?}");
        let prompts = prompts.borrow();
        // The open window showed its code before this run began.
        assert_eq!(prompts[0], "Enter the code the thermostat showed (it is still on Settings → UPDATE)");
        assert!(prompts[1].starts_with("The code is 8 digits") && prompts[2].starts_with("Wrong code"), "{prompts:?}");
        let unit = unit.lock().unwrap();
        assert_eq!((unit.opened, unit.exchanges), (0, 2), "an open window is not opened again");
    }

    #[test]
    fn stops_when_no_code_is_given() {
        let (result, _) = push(Unit::new(true, After::Runs), Code::Ask(&|_| None));
        assert_eq!(result.unwrap_err().to_string(), NO_CODE);
    }

    #[test]
    fn reports_a_unit_that_stopped_asking_after_its_strikes() {
        let (result, unit) = push(Unit { struck: true, ..Unit::new(false, After::Runs) }, Code::Ask(&|_| Some(CODE.into())));
        assert_eq!(result.unwrap_err().to_string(), OPENED_TOO_OFTEN);
        assert_eq!(unit.lock().unwrap().exchanges, 0);
    }

    #[test]
    fn refuses_an_older_nestlocal_or_one_needing_a_newer_overlay_unless_forced() {
        let older = marked("v0.4.1", 1);
        let newer_unit = || Unit { version: Some("v0.4.1-3-gabcdef0".into()), ..Unit::new(true, After::Runs) };
        let (result, unit) = push_binary(newer_unit(), Code::Given(CODE), &older, false);
        assert!(result.unwrap_err().to_string().contains("newer than this app's v0.4.1"));
        assert_eq!(unit.lock().unwrap().exchanges, 0, "refused before the code is tried");
        assert!(push_binary(newer_unit(), Code::Given(CODE), &older, true).0.is_ok(), "forced");
        let needs_more = marked("v0.5.0", 2);
        let (result, _) = push_binary(Unit::new(true, After::Runs), Code::Given(CODE), &needs_more, false);
        assert!(result.unwrap_err().to_string().contains("overlay version 2; the unit has 1"));
        let (result, _) = push_binary(Unit { overlay: None, ..Unit::new(true, After::Runs) }, Code::Given(CODE), &needs_more, false);
        assert!(result.unwrap_err().to_string().contains("the unit has none"));
        assert!(push_binary(Unit::new(true, After::Runs), Code::Given(CODE), &needs_more, true).0.is_ok(), "forced");
        // A newer nestlocal for the overlay the unit has goes through.
        let newer = marked("v0.4.2", 1);
        assert!(push_binary(newer_unit(), Code::Given(CODE), &newer, false).0.is_ok());
    }

    #[test]
    fn a_code_that_is_not_eight_digits_is_refused_before_connecting() {
        for code in ["1234567", "123456789", "1234567a", ""] {
            let error = push_to("127.0.0.1:1", Code::Given(code), NEW, false, &|_| {}, &FAST, &trust::Store::scratch()).unwrap_err().to_string();
            assert!(error.contains("8 digits"), "{code}: {error}");
        }
    }

    #[test]
    fn a_unit_without_the_update_api_is_reported_at_once() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let started = Instant::now();
        let error = push_to(&port.to_string(), Code::Given(CODE), NEW, false, &|_| {}, &UNIT_TIMING, &trust::Store::scratch())
            .unwrap_err().to_string();
        assert!(error.contains("--update-api") && started.elapsed() < Duration::from_secs(5), "{error}");
    }

    #[test]
    fn reports_a_rollback() {
        let (result, unit) = push(Unit::new(true, After::RollsBack), Code::Given(CODE));
        assert_eq!(result.unwrap_err().to_string(), "the unit rolled the update back");
        assert_eq!(unit.lock().unwrap().received, [NEW]);
    }

    #[test]
    fn reports_a_rollback_asked_for_on_the_thermostat() {
        let unit = Unit { rolled_back_by: Some("thermostat"), ..Unit::new(true, After::RollsBack) };
        let (result, _) = push(unit, Code::Given(CODE));
        assert_eq!(result.unwrap_err().to_string(), session::PUT_BACK);
    }

    #[test]
    fn reports_another_nestlocal_replacing_the_update() {
        let (result, _) = push(Unit::new(true, After::Replaced), Code::Given(CODE));
        let error = result.unwrap_err().to_string();
        assert!(error.starts_with("the unit now runs another nestlocal") && error.contains(&sha256_hex(b"OTHER")[..12]), "{error}");
    }

    #[test]
    fn says_an_update_still_on_trial_has_not_been_kept() {
        let (result, _) = push(Unit::new(true, After::StaysOnTrial), Code::Given(CODE));
        let error = result.unwrap_err().to_string();
        assert!(error.contains("runs on trial"), "{error}");
    }

    #[test]
    fn a_refusal_of_a_large_set_reaches_the_user_with_its_reason() {
        let big = vec![0x5a; 6 << 20];
        for (status, text) in [(409, "nestlabs/sbin/nestlocal changed since it was read: start again"),
                               (507, "not enough room to keep a rollback copy: use USB")] {
            let unit = Unit { refuses: Some((status, text)), ..Unit::new(true, After::Runs) };
            let (result, unit) = push_binary(unit, Code::Given(CODE), &big, false);
            let error = result.unwrap_err().to_string();
            assert_eq!(error, format!("the unit refused the update ({status}): {text}; {}", session::AGAIN));
            assert_eq!(unit.lock().unwrap().refused, [status]);
        }
    }

    #[test]
    fn refuses_a_push_while_another_update_is_on_trial() {
        let mut unit = Unit::new(true, After::RollsBack);
        (unit.installed, unit.running, unit.pending) = (sha256_hex(b"PENDING"), sha256_hex(b"PENDING"), true);
        let (result, unit) = push(unit, Code::Ask(&|_| panic!("asked for a code")));
        assert_eq!(result.unwrap_err().to_string(), ON_TRIAL);
        let unit = unit.lock().unwrap();
        assert!(unit.received.is_empty() && unit.exchanges == 0 && unit.opened == 0, "the code is left unspent");
    }

    #[test]
    fn the_push_sends_one_file_over_the_installed_one() {
        // A unit that does not say where nestlocal is: an older nestlocal,
        // which takes nothing on the scratch volume and has no check, gets
        // the root's.
        let unit = Unit { refuses_check: None, ..Unit::new(true, After::Runs) };
        let (result, unit) = push_binary(unit, Code::Given(CODE), NEW, true);
        assert!(result.is_ok(), "{result:?}");
        let unit = unit.lock().unwrap();
        let entry = Entry { path: NESTLOCAL.into(), kind: Kind::Write, owner: "0:0".into(), mode: 0o755,
                            old: Some(sha256_hex(b"OLD")), new: Some(sha256_hex(NEW)), size: NEW.len() as u64 };
        assert_eq!(unit.sets, [Bundle { version: BUNDLE_VERSION, class: Class::Restart, force: true, entries: vec![entry] }]);
    }

    #[test]
    fn a_push_to_the_scratch_volume_updates_the_scratch_manifest() {
        let line = |sha: &str, name: &str| format!("{sha}  {name}\n");
        let others = line(&sha256_hex(b"APPLY"), "bin/unfr0zen-apply") + &line(&sha256_hex(b"DOOM"), "doom/doom");
        let before = line(&sha256_hex(b"OLD"), "bin/nestlocal") + &others;
        let unit = Unit { binary: Some(SCRATCH_NESTLOCAL), manifest: Some(before.clone().into_bytes()), ..Unit::new(true, After::Runs) };
        let (result, unit) = push_binary(unit, Code::Given(CODE), NEW, true);
        assert!(result.is_ok(), "{result:?}");
        let unit = unit.lock().unwrap();
        // The same lines a setup writes: nestlocal's replaced, the others kept.
        let after = line(&sha256_hex(NEW), "bin/nestlocal") + &others;
        assert_eq!(String::from_utf8(unit.manifest.clone().unwrap()).unwrap(), after);
        let files = [(SCRATCH_NESTLOCAL, sha256_hex(NEW)), ("media/scratch/.unfr0zen/bin/unfr0zen-apply", sha256_hex(b"APPLY")),
                     ("media/scratch/.unfr0zen/doom/doom", sha256_hex(b"DOOM"))];
        assert_eq!(scratch::manifest(files), after.as_bytes());
        let paths: Vec<&str> = unit.sets[0].entries.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(paths, [SCRATCH_NESTLOCAL, scratch::MANIFEST]);
        assert_eq!(unit.checked, paths, "the unit checked the set before it went");
        assert!(paths.iter().all(|path| update_wire::bundle::RESTART_PATHS.contains(path)), "it only restarts nestlocal");
        assert_eq!(unit.received, [NEW]);
    }

    #[test]
    fn a_push_the_unit_s_check_refuses_needs_usb_and_sends_nothing() {
        let unit = Unit { binary: Some(SCRATCH_NESTLOCAL), refuses_check: Some(vec![(SCRATCH_NESTLOCAL, "no scratch volume is mounted")]),
                          ..Unit::new(true, After::Runs) };
        let (result, unit) = push(unit, Code::Given(CODE));
        let error = result.unwrap_err();
        assert!(matches!(&error, unfr0zen_core::Error::NeedsUsb(why)
                         if why.contains(&format!("{SCRATCH_NESTLOCAL} (no scratch volume is mounted)"))), "{error}");
        let unit = unit.lock().unwrap();
        assert_eq!(unit.checked, [SCRATCH_NESTLOCAL]);
        assert!(unit.sets.is_empty() && unit.received.is_empty() && unit.refused.is_empty(), "nothing was sent");
    }

    #[test]
    fn a_push_to_the_scratch_volume_without_a_manifest_sends_the_binary_alone() {
        let unit = Unit { binary: Some(SCRATCH_NESTLOCAL), ..Unit::new(true, After::Runs) };
        let (result, unit) = push_binary(unit, Code::Given(CODE), NEW, true);
        assert!(result.is_ok(), "{result:?}");
        let unit = unit.lock().unwrap();
        assert_eq!((unit.sets[0].entries.len(), unit.manifest.as_ref()), (1, None));
    }

    #[test]
    fn a_session_reads_the_unit_sealed_until_it_is_replaced() {
        let (addr, unit) = serve(Unit::new(true, After::Runs));
        let agent = session::agent();
        let open = session::tests::window_open();
        let session = session::authenticate(&agent, &addr, Code::Given(CODE), &open, &|_| {}, &FAST, &trust::Store::scratch()).unwrap();
        assert_eq!(session.info().unwrap().nlmodel, "Display-3.4");
        let request = ReadRequest { names: vec!["etc/nestlocal/options".into(), "etc/absent".into()], contents: true };
        let found = session.read(&request).unwrap();
        assert!(matches!(&found[..], [Found::File { data: Some(data), .. }, Found::Absent] if data == b"-mqtt x\n"), "{found:?}");
        // Another client's exchange replaces the session.
        let session_was = unit.lock().unwrap().session.replace(("another".into(), Vec::new()));
        assert_eq!(session.read(&request).unwrap_err().to_string(), STALE);
        // A window that lapses closes the session's too.
        let mut closed = unit.lock().unwrap();
        (closed.session, closed.open) = (session_was, false);
        drop(closed);
        assert_eq!(session.info().unwrap_err().to_string(), CLOSED);
    }

    #[test]
    fn waits_for_a_pending_update_already_running_it() {
        let rolled = format!("the unit now runs another nestlocal ({}): this update was rolled back or replaced", &sha256_hex(b"OLD")[..12]);
        for (after, expected) in [(After::Runs, Ok(format!("nestlocal {} is running", &sha256_hex(NEW)[..12]))),
                                  (After::RollsBack, Err(rolled))] {
            let mut unit = Unit::new(false, after);
            (unit.installed, unit.running, unit.pending) = (sha256_hex(NEW), sha256_hex(NEW), true);
            let (result, unit) = push(unit, Code::Given(CODE));
            assert_eq!(result.map_err(|e| e.to_string()), expected);
            let unit = unit.lock().unwrap();
            assert!(unit.received.is_empty() && unit.opened == 0, "nothing is sent again");
        }
    }

    #[test]
    fn skips_a_unit_already_running_it() {
        let mut running = Unit::new(false, After::Runs);
        (running.installed, running.running) = (sha256_hex(NEW), sha256_hex(NEW));
        let (result, unit) = push(running, Code::Ask(&|_| panic!("asked for a code")));
        assert_eq!(result.unwrap(), format!("already running {}", &sha256_hex(NEW)[..12]));
        let unit = unit.lock().unwrap();
        assert!(unit.received.is_empty() && unit.opened == 0);
    }
}
