//! A session with a unit's update window (nestlocal's wifi_update/window.rs): the
//! code its screen shows keys a SPAKE2 exchange, whose key seals the reads and
//! the one apply after it, and their answers. GET /update is unauthenticated.
//! Asked to, the unit opens the window once someone presses Allow on the
//! prompt it shows (nestlocal's wifi_update/prompt.rs); after Allow and trust, the
//! exchange grants this computer a trust key, which trust.rs keeps. With that
//! key, a session needs no window: the key keys the exchange in the code's
//! place, and the thermostat shows a notice while the session runs.

use std::time::{Duration, Instant};

use spake2::{Ed25519Group, Identity, Password, Spake2};
use unfr0zen_core::nestlocal::UPDATE_PORT;
use unfr0zen_core::Result;
use update_wire::auth::{confirmation, is_id, laptop_name as sanitized, trust_key, verify, CLIENT, UNIT};
use update_wire::bundle::{Applied, Bundle};
use update_wire::read::{CheckRequest, Checked, Found, Info, ReadRequest, Refused};
use update_wire::seal::{self, aad, Keys, StreamSealer, CHUNK};
use update_wire::{hex, unhex};

use crate::flow::failure;
use crate::trust;

/// What a push given a code says when the unit's window is not open.
pub const CLOSED: &str = "the update window is closed: open it on the thermostat (Settings → UPDATE → Allow update) \
                          for a code, or leave the code out to have the thermostat ask";
/// What a push that asked for the code says when the window closed meanwhile.
pub(crate) const CLOSED_ASKED: &str = "the update window closed: start again for a new code";
/// Advice once the unit has spent the code on the upload.
pub(crate) const AGAIN: &str = "open the update window again for a new code";
pub(crate) const WRONG: &str = "wrong code";
/// The code was right (the unit confirmed the key), so the upload changed on the way.
pub(crate) const ALTERED: &str = "the unit refused the upload's authentication, which changed on the way; try again";
pub(crate) const STALE: &str = "the unit dropped this upload's session: something else started an update at the same time, \
                                or the upload came too late; try again";
pub(crate) const OPENED_TOO_OFTEN: &str = "the thermostat stopped asking after 3 prompts in a row were denied or went unanswered: \
                                           open Settings → UPDATE on the thermostat and press Allow update";
/// The unit's busy refusal, with what to do when no other update is the
/// cause: five wrong codes leave its window shut to a new one for up to ten
/// minutes.
fn busy(body: &str) -> String {
    format!("{}; if nobody else is updating, wait up to 10 minutes or press Allow update on the thermostat (Settings → UPDATE)",
            body.trim())
}
/// What the user is told while a trust the unit gave keys the session.
pub(crate) const TRUSTING: &str = "Using this laptop's trust with the thermostat";
/// Why the unit's trust went unused: it said it does not trust this laptop,
/// or did not confirm the key this laptop holds; or its window is open, whose
/// code a trusted session would wait out.
pub(crate) const NOT_TRUSTED_NOW: &str = "The thermostat does not trust this laptop now";
pub(crate) const UNCONFIRMED: &str = "The thermostat did not confirm this laptop's key";
pub(crate) const WINDOW_OPEN: &str = "The thermostat's update window is open: using its code instead of this laptop's trust";
/// What a trusted session's request says once the unit ended the session.
pub(crate) const ENDED: &str = "the thermostat ended this laptop's session (a minute idle, or ten minutes in): start again";
/// Cancel was pressed on the thermostat's notice of this laptop's session.
pub(crate) const CANCELLED: &str = "Cancelled on the thermostat";
/// A set the unit rolled back because someone asked on its screen.
pub(crate) const PUT_BACK: &str = "Cancelled on the thermostat: the unit put its files back";
/// The unit's refusal of a request in a session cancelled on its screen.
const CANCELLED_TEXT: &str = "cancelled on the thermostat";
/// The unit's refusal of a request in a closed window or an ended trusted
/// session (nestlocal's wifi_update/mod.rs), the same from every version.
const CLOSED_TEXT: &str = "window closed";
/// The code asked for: the one an Allow just put on the thermostat's screen,
/// or the one a window open already showed.
pub(crate) const SHOWS: &str = "the code the thermostat shows";
pub(crate) const SHOWED: &str = "the code the thermostat showed (it is still on Settings → UPDATE)";
/// What the user is told to do while the thermostat asks.
pub(crate) const ALLOW: &str = "On the thermostat, press Allow (or Allow and trust), then enter the code it shows";
pub(crate) const DENIED: &str = "Denied on the thermostat";
pub(crate) const UNANSWERED: &str = "Nobody answered on the thermostat within a minute";
/// Allow was pressed while another update held the unit's window.
pub(crate) const HELD: &str = "Allowed on the thermostat while another update held it: try again once that one is done";
/// The unit's grant added this laptop where it was asked to replace the old
/// one: its proof of the old key failed, or something on the way dropped it.
pub(crate) const ADDED: &str = "The thermostat added this laptop rather than replacing its old entry";
/// A trusted laptop's session took the thermostat while it asked.
pub(crate) const PREEMPTED: &str = "Another update took the thermostat before anyone answered: try again once that one is done";
/// The unit's prompt is another client's: its answer is not this one's.
pub(crate) const TAKEN: &str = "another update request took the thermostat's prompt";
/// How long the app waits on a prompt: the unit answers it for the user
/// after a minute.
const PROMPT_FOR: Duration = Duration::from_secs(90);
pub(crate) const NO_CODE: &str = "no code was given";
pub(crate) const UNPROVEN: &str = "the answer to the upload was not the unit's (it is not proven under the session key): \
                                   something on the network answered for it";

/// The most a sealed answer may hold: reads carry whole files.
const ANSWER_LIMIT: u64 = 256 << 20;
/// How long closing a session waits for the unit.
const CLOSE_FOR: Duration = Duration::from_secs(5);

/// How long a client waits, at each step.
pub(crate) struct Timing {
    /// For the unit to answer at all.
    pub connect: Duration,
    /// Between status checks while the unit installs and restarts.
    pub poll: Duration,
    /// For an update to run and be kept.
    pub wait: Duration,
    /// For one status check or key exchange.
    pub request: Duration,
    /// The unit's port a knock goes to before each try to reach it: Matter's,
    /// whose wake patterns wake a sleeping unit for any UDP packet to it.
    pub knock: u16,
}

pub(crate) const UNIT_TIMING: Timing = Timing { connect: Duration::from_secs(30), poll: Duration::from_secs(5),
                                                wait: Duration::from_secs(15 * 60), request: Duration::from_secs(10),
                                                knock: 5540 };

/// The code the unit's screen shows: given, or asked for once the window is
/// open, which the client then opens itself if need be.
#[derive(Clone, Copy)]
pub enum Code<'a> {
    Given(&'a str),
    /// Asks for the code, again after a wrong one; none ends the session. The
    /// window is opened first, so only a client that can ask should.
    Ask(&'a dyn Fn(&str) -> Option<String>),
    /// None to be had: only a trust this laptop holds keys the session, and
    /// where a code would be needed the run fails with this reason, before
    /// the window is asked for.
    Unavailable(&'a str),
}

/// Whether code is what the thermostat shows: 8 digits.
pub fn is_code(code: &str) -> bool {
    code.len() == 8 && code.bytes().all(|b| b.is_ascii_digit())
}

/// The unit's update API address: HOST, its IPv4 address or name, with a
/// port of its own or the update port. nestlocal listens on IPv4 alone.
pub fn address(host: &str) -> Result<String> {
    let bad = || failure(format!("{host:?} is not a unit's address: give its IPv4 address or name, as 192.168.1.50"));
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        return Ok(format!("{ip}:{UPDATE_PORT}"));
    }
    if let Ok(socket) = host.parse::<std::net::SocketAddrV4>() {
        return Ok(socket.to_string());
    }
    let (name, port) = match host.rsplit_once(':') {
        Some((name, port)) => (name, Some(port.parse::<u16>().map_err(|_| bad())?)),
        None => (host, None),
    };
    let label = |l: &str| !l.is_empty() && l.len() <= 63 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !l.starts_with('-') && !l.ends_with('-');
    if name.is_empty() || name.parse::<std::net::IpAddr>().is_ok() || !name.split('.').all(label) {
        return Err(bad());
    }
    Ok(format!("{name}:{}", port.unwrap_or(UPDATE_PORT)))
}

/// The agent a client talks to the unit through.
pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // The unit is on the local network: no proxy the environment names.
        .proxy(None)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(5)))
        // The unit answers an apply's head once it has checked it.
        .timeout_await_100(Some(Duration::from_secs(30)))
        .timeout_global(Some(Duration::from_secs(300)))
        .build()
        .into()
}

/// The update API's root at addr.
pub(crate) fn url(addr: &str) -> String {
    format!("http://{addr}/update")
}

/// GET /update's answer.
#[derive(Clone, Debug, Default)]
pub struct Status {
    /// The window takes a key exchange.
    pub open: bool,
    /// The SHA-256 of the installed nestlocal, and of the running one.
    pub installed: Option<String>,
    /// Where the installed nestlocal is, relative to /; None from a
    /// nestlocal that does not say, whose is on the root volume.
    pub binary: Option<String>,
    pub running: Option<String>,
    /// An update is on trial.
    pub pending: bool,
    /// The running nestlocal's version, and the installed overlay version.
    pub version: Option<String>,
    pub overlay: Option<u32>,
    /// The unit put the last update back, which no newer one followed.
    pub rolled_back: bool,
    /// Who asked for that: "thermostat" for Cancel or Roll back pressed
    /// there; none for a set that did not come up.
    pub rolled_back_by: Option<String>,
    /// The last prompt, for a minute after its answer.
    pub prompt: Option<Prompt>,
    /// The unit's id, once it trusts a laptop: what a trust key it gave is
    /// kept under (trust.rs).
    pub unit: Option<String>,
}

impl Status {
    /// The unit put the last update back at a Cancel or Roll back pressed
    /// on the thermostat.
    pub fn cancelled_on_thermostat(&self) -> bool {
        self.rolled_back && self.rolled_back_by.as_deref() == Some("thermostat")
    }
}

/// A prompt the unit raised, as GET /update reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prompt {
    /// Its own: the open that raised it was answered with it.
    pub id: String,
    /// "asking", then its answer: "allowed", "denied", "timed_out", "busy"
    /// (allowed while another update held the window), or "preempted" (a
    /// trusted laptop's session took the thermostat before anyone answered).
    pub state: String,
}

/// The status of the unit at host, once it answers.
pub fn status(host: &str) -> Result<Status> {
    let addr = address(host)?;
    reach(&agent(), &url(&addr), &addr, &UNIT_TIMING)
}

/// Sends one datagram of one zero byte to `to`, from a port of its own, and
/// ignores whatever fails: a knock that wakes a sleeping unit, which Matter
/// discards.
pub(crate) fn knock(to: std::net::SocketAddr) {
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        let _ = socket.send_to(&[0], to);
    }
}

/// Where knocks on the unit at addr go: its host's IPv4 address, at port.
fn knock_address(addr: &str, port: u16) -> Option<std::net::SocketAddr> {
    use std::net::ToSocketAddrs;
    let host = addr.rsplit_once(':').map_or(addr, |(host, _)| host);
    (host, port).to_socket_addrs().ok()?.find(std::net::SocketAddr::is_ipv4)
}

/// The unit's status, waiting up to timing.connect for it to answer, with a
/// knock on its timing.knock port before each try. The host's name is
/// looked up for the knock until it resolves, then not again.
pub(crate) fn reach(agent: &ureq::Agent, url: &str, addr: &str, timing: &Timing) -> Result<Status> {
    let start = Instant::now();
    let mut knock_at = None;
    loop {
        knock_at = knock_at.or_else(|| knock_address(addr, timing.knock));
        if let Some(to) = knock_at {
            knock(to);
        }
        match get(agent, url, timing)? {
            Ok(status) => return Ok(status),
            Err(error) if refused(&error) => {
                return Err(failure(format!("nothing at {addr} takes updates: was the unit set up with \
                                            nestlocal updates over Wi-Fi (--update-api)?")));
            }
            Err(error) if start.elapsed() >= timing.connect => {
                return Err(failure(format!("no answer from nestlocal at {addr} (wake the thermostat if it sleeps): {error}")));
            }
            Err(_) => std::thread::sleep(timing.poll.min(Duration::from_secs(1))),
        }
    }
}

/// Whether a request failed because nothing listens there.
fn refused(error: &ureq::Error) -> bool {
    matches!(error, ureq::Error::Io(e) if e.kind() == std::io::ErrorKind::ConnectionRefused)
}

/// The unit's status; the inner error is a unit that did not answer, which
/// a client waits out, and the outer one an answer that is not a status.
pub(crate) fn get(agent: &ureq::Agent, url: &str, timing: &Timing) -> Result<std::result::Result<Status, ureq::Error>> {
    let mut response = match agent.get(url).config().timeout_global(Some(timing.request)).build().call() {
        Ok(response) => response,
        Err(error) => return Ok(Err(error)),
    };
    let code = response.status().as_u16();
    let body = match response.body_mut().read_to_string() {
        Ok(body) => body,
        Err(error) => return Ok(Err(error)),
    };
    if code != 200 {
        return Err(failure(format!("nestlocal's update API answered {code}: {}", body.trim())));
    }
    let value: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| failure(format!("nestlocal's update API sent an unreadable status: {e}")))?;
    let text = |name: &str| value[name].as_str().map(str::to_string);
    Ok(Ok(Status {
        open: value["open"].as_bool().unwrap_or(false),
        installed: text("installed"),
        binary: text("binary"),
        running: text("running"),
        pending: value["pending"].as_bool().unwrap_or(false),
        version: text("version"),
        overlay: value["overlay"].as_u64().and_then(|v| u32::try_from(v).ok()),
        rolled_back: value["rolled_back"].as_bool().unwrap_or(false),
        rolled_back_by: text("rolled_back_by"),
        prompt: value["prompt"]["id"].as_str().zip(value["prompt"]["state"].as_str())
            .map(|(id, state)| Prompt { id: id.to_string(), state: state.to_string() }),
        unit: text("unit"),
    }))
}

/// A session with the window of the unit at host, keyed with the code, and
/// the unit's status before it.
pub fn connect(host: &str, code: Code, progress: &dyn Fn(&str)) -> Result<(Session, Status)> {
    connect_at(&address(host)?, code, progress, &UNIT_TIMING, &trust::Store::default())
}

/// connect, to the unit's update API at addr, waiting as timing says, and
/// keeping any trust the unit grants in trusted.
pub(crate) fn connect_at(addr: &str, code: Code, progress: &dyn Fn(&str), timing: &Timing, trusted: &trust::Store)
                         -> Result<(Session, Status)> {
    if let Code::Given(code) = code {
        if !is_code(code) {
            return Err(failure("the code is the 8 digits the thermostat shows after Allow, or on Settings → UPDATE → Allow update"));
        }
    }
    let (agent, url) = (agent(), url(addr));
    progress(&format!("Connecting to nestlocal at {addr}"));
    let status = reach(&agent, &url, addr, timing)?;
    let session = authenticate(&agent, addr, code, &status, progress, timing, trusted)?;
    Ok((session, status))
}

/// Whether error is a session's window having closed under it.
pub(crate) fn lapsed(error: &unfr0zen_core::Error) -> bool {
    matches!(error, unfr0zen_core::Error::Device(message) if message == CLOSED || message == CLOSED_ASKED)
}

/// A session with the unit's update API at addr, whose status is status:
/// keyed with the trust key trusted holds for the unit, if any, no window is
/// open and the unit takes it; else with the code, the window opened first
/// when the code is to be asked for. A trust the unit grants in it is kept in
/// trusted, with addr, in place of the laptop trusted held on the unit, once
/// the unit has kept it: it does at the session's first request, before it
/// answers, which is made here. Until then the old entry stays.
pub(crate) fn authenticate(agent: &ureq::Agent, addr: &str, code: Code, status: &Status, progress: &dyn Fn(&str), timing: &Timing,
                           trusted: &trust::Store) -> Result<Session> {
    let url = &url(addr);
    let entry = status.unit.as_deref().and_then(|unit| trusted.get(unit));
    let session = |keyed: Keyed, via| -> Result<Session> {
        let session = Session { agent: agent.clone(), url: url.to_string(), id: keyed.session, keys: seal::keys(&keyed.key), via };
        if let Some((unit, laptop)) = keyed.grant {
            if entry.is_some() && !keyed.replaced {
                progress(ADDED);
            }
            session.info()?;
            let entry = trust::Entry { key: trust_key(&keyed.key, &unit, &laptop), unit, laptop, address: addr.to_string() };
            if let Err(e) = trusted.put(&entry) {
                progress(&format!("The thermostat trusts this computer, but keeping its trust failed: {e}"));
            }
        }
        Ok(session)
    };
    match &entry {
        Some(_) if status.open => progress(WINDOW_OPEN),
        Some(entry) => {
            progress(TRUSTING);
            let why = match trusted_exchange(agent, url, entry, timing)? {
                Trusted::Keyed(keyed) => return session(keyed, Via::Trusted),
                Trusted::Unknown => NOT_TRUSTED_NOW.to_string(),
                Trusted::Unconfirmed => UNCONFIRMED.to_string(),
                Trusted::NoNotice(why) => format!("The thermostat cannot show its notice for this laptop ({why})"),
            };
            progress(&match code {
                Code::Ask(_) => format!("{why}: asking on the thermostat instead"),
                Code::Given(_) | Code::Unavailable(_) => why,
            });
        }
        None => {}
    }
    let ask = match code {
        Code::Given(code) if status.open => {
            return session(exchange(agent, url, code, entry.as_ref(), CLOSED, timing)?.ok_or_else(|| failure(WRONG))?, Via::Given);
        }
        Code::Given(_) => return Err(failure(CLOSED)),
        Code::Unavailable(why) => return Err(failure(why)),
        Code::Ask(ask) => ask,
    };
    // A window open already showed its code, which went full screen once a
    // session proved it, but stays on the Update page.
    let shown = match !status.open && open_window(agent, url, entry.as_ref(), progress, timing)? {
        true => SHOWS,
        false => SHOWED,
    };
    let mut prompt = format!("Enter {shown}");
    loop {
        let code = ask(&prompt).ok_or_else(|| failure(NO_CODE))?;
        let code = code.trim();
        if !is_code(code) {
            prompt = format!("The code is 8 digits: enter {shown}");
            continue;
        }
        if let Some(keyed) = exchange(agent, url, code, entry.as_ref(), CLOSED_ASKED, timing)? {
            return session(keyed, Via::Asked);
        }
        prompt = format!("Wrong code: enter {shown}");
    }
}

/// Asks the unit to open its window under this laptop's name, and the id
/// of entry's laptop, which a full trusted list still offers trust to; and
/// waits, polling every second, while the thermostat asks someone to allow
/// it. Only the answer to the prompt this open raised counts. Whether it
/// raised one: none when a window was open already.
fn open_window(agent: &ureq::Agent, url: &str, entry: Option<&trust::Entry>, progress: &dyn Fn(&str), timing: &Timing) -> Result<bool> {
    let mut name = serde_json::json!({"name": laptop_name()});
    if let Some(entry) = entry {
        name["laptop"] = entry.laptop.clone().into();
    }
    let name = name.to_string();
    let mut response = agent.post(&format!("{url}/open")).config().timeout_global(Some(timing.request)).build()
        .send(name).map_err(|e| failure(format!("asking the unit to open its update window failed: {e}")))?;
    let status = response.status().as_u16();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    match status {
        200 => return Ok(false),
        202 => {}
        429 => return Err(failure(OPENED_TOO_OFTEN)),
        409 => return Err(failure(body.trim())),
        503 => return Err(failure(busy(&body))),
        status => return Err(failure(format!("the unit did not open its update window ({status}): {}", body.trim()))),
    }
    let raised: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let id = raised["id"].as_str().ok_or_else(|| failure("the unit's answer to the open names no prompt"))?.to_string();
    progress(ALLOW);
    let start = Instant::now();
    loop {
        std::thread::sleep(timing.poll.min(Duration::from_secs(1)));
        let status = match get(agent, url, timing)? {
            Ok(status) => status,
            Err(_) if start.elapsed() < PROMPT_FOR => continue,
            Err(error) => return Err(failure(format!("no answer from the unit while the thermostat asked: {error}"))),
        };
        let Some(prompt) = status.prompt else {
            return Err(failure("the thermostat stopped asking without an answer"));
        };
        if prompt.id != id {
            return Err(failure(TAKEN));
        }
        match prompt.state.as_str() {
            "asking" if start.elapsed() < PROMPT_FOR => {}
            "allowed" => return Ok(true),
            "denied" => return Err(failure(DENIED)),
            "busy" => return Err(failure(HELD)),
            "preempted" => return Err(failure(PREEMPTED)),
            "timed_out" | "asking" => return Err(failure(UNANSWERED)),
            other => return Err(failure(format!("the thermostat's prompt is in a state this app does not know: {other}"))),
        }
    }
}

/// This computer's name, as the thermostat's prompt names it.
pub fn laptop_name() -> String {
    sanitized(&hostname())
}

#[cfg(windows)]
fn hostname() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

#[cfg(not(windows))]
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").ok()
        .or_else(|| std::process::Command::new("hostname").output().ok().map(|out| String::from_utf8_lossy(&out.stdout).into_owned()))
        .unwrap_or_default()
}

/// A key exchange's outcome: the session, its key, the unit's and laptop's
/// ids of the trust the unit granted in it, and whether the grant replaces
/// the laptop the exchange asked it to.
struct Keyed {
    session: String,
    key: Vec<u8>,
    grant: Option<(String, String)>,
    replaced: bool,
}

/// A key exchange with the unit's window, under this laptop's name: the
/// session, or none for a wrong code, which the unit's confirmation shows.
/// The confirmation covers a grant too: one changed on the way, like a wrong
/// code, confirms nothing. With entry, a grant replaces its laptop on the
/// unit, which its proof (trust::replace_proof) shows this laptop held, and
/// the confirmation shows whether it did.
fn exchange(agent: &ureq::Agent, url: &str, code: &str, entry: Option<&trust::Entry>, closed: &str, timing: &Timing)
            -> Result<Option<Keyed>> {
    let (spake, message, mut body) = start_exchange(code.as_bytes());
    if let Some(entry) = entry {
        body["replaces"] = entry.laptop.clone().into();
        body["proof"] = trust::replace_proof(entry, &message).into();
    }
    let (status, body) = post_exchange(agent, url, body, timing)?;
    match status {
        200 => keyed(spake, &body, entry.map(|entry| entry.laptop.as_str())),
        403 => Err(failure(closed)),
        status => Err(failure(format!("the unit refused the key exchange ({status}): {}", body.trim()))),
    }
}

/// What a trusted exchange came to.
enum Trusted {
    Keyed(Keyed),
    /// The unit says it does not trust this laptop.
    Unknown,
    /// The unit did not confirm the key this laptop holds.
    Unconfirmed,
    /// The unit cannot show the notice a trusted session needs, for the
    /// reason given; a window opened on the thermostat can still be used.
    NoNotice(String),
}

/// A key exchange keyed by the trust key entry holds for its unit. Neither
/// "not trusted" nor a key unconfirmed is authenticated, so the entry stays.
/// The unit's other refusals end the run with its reason.
fn trusted_exchange(agent: &ureq::Agent, url: &str, entry: &trust::Entry, timing: &Timing) -> Result<Trusted> {
    let (spake, _, mut body) = start_exchange(&entry.key);
    body["laptop"] = entry.laptop.clone().into();
    let (status, body) = post_exchange(agent, url, body, timing)?;
    match status {
        200 => Ok(keyed(spake, &body, None)?.map_or(Trusted::Unconfirmed, Trusted::Keyed)),
        404 => Ok(Trusted::Unknown),
        409 => Ok(Trusted::NoNotice(body.trim().to_string())),
        429 => Err(failure(body.trim())),
        503 => Err(failure(busy(&body))),
        status => Err(failure(format!("the unit refused the trusted session ({status}): {}", body.trim()))),
    }
}

/// The client's side of a key exchange under password, its message, and the
/// request's body: the message in hex and this laptop's name.
fn start_exchange(password: &[u8]) -> (Spake2<Ed25519Group>, Vec<u8>, serde_json::Value) {
    let (spake, message) = Spake2::<Ed25519Group>::start_a(&Password::new(password), &Identity::new(CLIENT), &Identity::new(UNIT));
    let body = serde_json::json!({"message": hex(&message), "name": laptop_name()});
    (spake, message, body)
}

/// Sends a key exchange's request: the status and body of the answer.
fn post_exchange(agent: &ureq::Agent, url: &str, body: serde_json::Value, timing: &Timing) -> Result<(u16, String)> {
    let mut response = agent.post(&format!("{url}/session")).config().timeout_global(Some(timing.request)).build()
        .send(body.to_string()).map_err(|e| failure(format!("the key exchange failed: {e}")))?;
    let status = response.status().as_u16();
    Ok((status, response.body_mut().read_to_string().unwrap_or_default()))
}

/// The session a key exchange's answer keys, or none when the unit's
/// confirmation does not verify; a grant confirmed as one in place of the
/// laptop of id replaces, or as one that adds a laptop.
fn keyed(spake: Spake2<Ed25519Group>, body: &str, replaces: Option<&str>) -> Result<Option<Keyed>> {
    let unreadable = || failure("the unit's key exchange is unreadable");
    let answer: serde_json::Value = serde_json::from_str(body).map_err(|_| unreadable())?;
    let text = |name: &str| answer[name].as_str().ok_or_else(unreadable);
    let theirs = unhex(text("message")?).ok_or_else(unreadable)?;
    let key = spake.finish(&theirs).map_err(|_| unreadable())?;
    let grant = answer["grant"]["unit"].as_str().zip(answer["grant"]["laptop"].as_str())
        .map(|(unit, laptop)| (unit.to_string(), laptop.to_string()));
    // Ids are hex, never newlines: the confirmation's fields then cannot
    // be shifted to make a replacing grant read as one that adds.
    if grant.as_ref().is_some_and(|(unit, laptop)| !is_id(unit) || !is_id(laptop)) {
        return Err(unreadable());
    }
    let (confirm, ids) = (text("confirm")?, grant.as_ref().map(|(unit, laptop)| (unit.as_str(), laptop.as_str())));
    let replaced = ids.is_some() && replaces.is_some_and(|old| verify(&key, &confirmation(ids, Some(old)), confirm));
    if !replaced && !verify(&key, &confirmation(ids, None), confirm) {
        return Ok(None);
    }
    Ok(Some(Keyed { session: text("session")?.to_string(), key, grant, replaced }))
}

/// How an apply went wrong: refused by the unit, or lost on the way, when
/// the unit may still have taken it.
pub enum Sent {
    Refused(unfr0zen_core::Error),
    Lost(ureq::Error),
}

/// How a session was keyed.
#[derive(Clone, Copy)]
enum Via {
    /// The code was given.
    Given,
    /// The code was asked for.
    Asked,
    /// A trust key.
    Trusted,
}

impl Via {
    /// What a request says when the unit refuses it as closed.
    fn closed(self) -> &'static str {
        match self {
            Via::Given => CLOSED,
            Via::Asked => CLOSED_ASKED,
            Via::Trusted => ENDED,
        }
    }
}

/// A keyed session with the unit's window, or a trusted one.
pub struct Session {
    agent: ureq::Agent,
    /// The update API's root (url).
    url: String,
    id: String,
    keys: Keys,
    via: Via,
}

impl Session {
    /// The unit as the session first sees it.
    pub fn info(&self) -> Result<Info> {
        let answer = self.sealed("info", b"{}")?;
        serde_json::from_slice(&answer).map_err(|e| failure(format!("the unit sent unreadable information: {e}")))
    }

    /// The files request names, in its order.
    pub fn read(&self, request: &ReadRequest) -> Result<Vec<Found>> {
        let request = serde_json::to_vec(request).map_err(failure)?;
        let answer = self.sealed("read", &request)?;
        serde_json::from_slice(&answer).map_err(|e| failure(format!("the unit sent an unreadable read: {e}")))
    }

    /// What the unit's apply would refuse of a set changing paths, by its
    /// own rules, before anything of it is sent; None from a unit whose
    /// nestlocal has no check (404: older than it).
    pub fn check(&self, paths: &[&str]) -> Result<Option<Vec<Refused>>> {
        let request = CheckRequest { paths: paths.iter().map(|path| path.to_string()).collect() };
        let request = serde_json::to_vec(&request).map_err(failure)?;
        let Some(answer) = self.sealed_unless_missing("check", &request, None)? else {
            return Ok(None);
        };
        let checked: Checked = serde_json::from_slice(&answer).map_err(|e| failure(format!("the unit sent an unreadable check: {e}")))?;
        Ok(Some(checked.refused))
    }

    /// Ends the session on the unit, which takes a trusted session's notice
    /// off its screen, and closes a window that granted this laptop trust;
    /// one left open ends idle, so a failure is no matter, and CLOSE_FOR is
    /// all it waits.
    pub fn close(&self) {
        let _ = self.sealed_within("close", b"{}", Some(CLOSE_FOR));
    }

    /// Sends plaintext sealed to /update/name; the answer, opened.
    fn sealed(&self, name: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
        self.sealed_within(name, plaintext, None)
    }

    /// sealed, waiting up to within for the answer, or as long as the
    /// agent does.
    fn sealed_within(&self, name: &str, plaintext: &[u8], within: Option<Duration>) -> Result<Vec<u8>> {
        self.sealed_unless_missing(name, plaintext, within)?.ok_or_else(|| {
            failure(format!("the unit refused the request (404): its nestlocal has no /update/{name}"))
        })
    }

    /// sealed_within; None when the unit has no such request (404).
    fn sealed_unless_missing(&self, name: &str, plaintext: &[u8], within: Option<Duration>) -> Result<Option<Vec<u8>>> {
        let aad = aad(&format!("/update/{name}"), &self.id);
        let request = seal::seal(&self.keys.to_unit, &aad, plaintext);
        let post = self.agent.post(&format!("{}/{name}", self.url));
        let post = match within {
            Some(within) => post.config().timeout_global(Some(within)).build(),
            None => post,
        };
        let mut response = post
            .header("X-Update-Session", &self.id)
            .send(&request[..])
            .map_err(|e| failure(format!("asking the unit failed: {e}")))?;
        let status = response.status().as_u16();
        let body = response.body_mut().with_config().limit(ANSWER_LIMIT).read_to_vec()
            .map_err(|e| failure(format!("the unit's answer was cut off: {e}")))?;
        match status {
            200 => {}
            404 => return Ok(None),
            401 => return Err(failure(ALTERED)),
            403 => return Err(failure(self.via.closed())),
            412 => return Err(failure(STALE)),
            409 if String::from_utf8_lossy(&body).trim() == CANCELLED_TEXT => return Err(failure(CANCELLED)),
            status => return Err(failure(format!("the unit refused the request ({status}): {}", String::from_utf8_lossy(&body).trim()))),
        }
        let answer_aad = [aad.as_slice(), seal::nonce_of(&request).expect("a sealed request has a nonce")].concat();
        seal::open(&self.keys.to_client, &answer_aad, &body).map(Some).map_err(|_| failure(UNPROVEN))
    }

    /// Sends set and its writes' contents, in entry order, as the session's
    /// one apply; the unit's answer must open under the session key.
    pub fn apply(&self, set: &Bundle, contents: &[&[u8]]) -> std::result::Result<Applied, Sent> {
        let refused = |message: String| Err(Sent::Refused(failure(message)));
        let aad = aad("/update/apply", &self.id);
        let header = seal::seal(&self.keys.to_unit, &aad, &serde_json::to_vec(set).map_err(|e| Sent::Refused(failure(e)))?);
        let answer_aad = [aad.as_slice(), seal::nonce_of(&header).expect("a sealed set has a nonce")].concat();
        let body = apply_body(&self.keys, &header, &answer_aad, contents);
        let mut response = self.agent.post(&format!("{}/apply", self.url))
            // The unit refuses a stale session or closed window before the body.
            .header("Expect", "100-continue")
            .header("X-Update-Session", &self.id)
            .send(&body[..])
            .map_err(Sent::Lost)?;
        let status = response.status().as_u16();
        let answer = response.body_mut().read_to_vec().map_err(Sent::Lost)?;
        if (200..=299).contains(&status) {
            return seal::open(&self.keys.to_client, &answer_aad, &answer).ok()
                .and_then(|applied| serde_json::from_slice(&applied).ok())
                .map_or_else(|| refused(UNPROVEN.to_string()), Ok);
        }
        refused(self.refusal(status, String::from_utf8_lossy(&answer).trim()))
    }

    /// What the unit's refusal of an apply, status and text, tells the
    /// user. A set that authenticated spent its code, so another needs a new
    /// one; a trusted session needs none.
    fn refusal(&self, status: u16, text: &str) -> String {
        let again = match self.via {
            Via::Trusted => String::new(),
            Via::Given | Via::Asked => format!("; {AGAIN}"),
        };
        match status {
            401 => ALTERED.to_string(),
            403 if text.contains("during the upload") => format!("{text}{again}"),
            403 if text == CLOSED_TEXT => self.via.closed().to_string(),
            412 => STALE.to_string(),
            409 if text == CANCELLED_TEXT => CANCELLED.to_string(),
            400 | 403 | 409 | 413 | 422 | 500 | 507 => format!("the unit refused the update ({status}): {text}{again}"),
            status => format!("the unit refused the update ({status}): {text}"),
        }
    }
}

/// An apply's body (nestlocal's wifi_update/mod.rs): the sealed set's length and
/// the sealed set, then the contents, one after another, as a stream sealed
/// under aad, in CHUNK pieces, each after its length.
fn apply_body(keys: &Keys, header: &[u8], aad: &[u8], contents: &[&[u8]]) -> Vec<u8> {
    let (mut sealer, prefix) = StreamSealer::new(&keys.to_unit, aad);
    let total: usize = contents.iter().map(|data| data.len()).sum();
    let pieces = total.div_ceil(CHUNK).max(1);
    let mut body = Vec::with_capacity(4 + header.len() + prefix.len() + total + pieces * (4 + 1 + 16));
    body.extend_from_slice(&(header.len() as u32).to_be_bytes());
    body.extend_from_slice(header);
    body.extend_from_slice(&prefix);
    // Each piece gathered from the contents it spans.
    let (mut rest, mut current): (_, &[u8]) = (contents.iter().copied(), &[]);
    let mut piece = Vec::with_capacity(CHUNK.min(total));
    for i in 0..pieces {
        let size = CHUNK.min(total - i * CHUNK);
        piece.clear();
        while piece.len() < size {
            if current.is_empty() {
                current = rest.next().expect("the contents hold total bytes");
            }
            let (taken, left) = current.split_at((size - piece.len()).min(current.len()));
            piece.extend_from_slice(taken);
            current = left;
        }
        let chunk = sealer.chunk(&piece, i + 1 == pieces);
        body.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
        body.extend_from_slice(&chunk);
    }
    body
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use update_wire::auth::{sign, CONFIRMATION};

    /// A request and its answer sealed once under keys((0..32)) in session
    /// SESSION; nestlocal's wifi_update/mod.rs opens the same pair.
    const SESSION: &str = "000102030405060708090a0b0c0d0e0f";
    const VECTOR_REQUEST: &str = "907ea381ff67406c3ee2d26305f8e3ba54726abd3a22d1d7ab6d3ca3d53ba5cab4c5dbd67424745aa3";
    const VECTOR_ANSWER: &str = "073623e277666dd1f73edb97877e4eb94a0277f84b8a20923c02e0a3e54b7b69354320e1c49386af";

    #[test]
    fn apply_matches_the_shared_test_vector() {
        let keys = seal::keys(&(0..32).collect::<Vec<u8>>());
        let aad = aad("/update/apply", SESSION);
        let request = unhex(VECTOR_REQUEST).unwrap();
        assert_eq!(seal::open(&keys.to_unit, &aad, &request).as_deref(), Ok(&b"{\"version\":1}"[..]));
        let answer_aad = [aad.as_slice(), seal::nonce_of(&request).unwrap()].concat();
        let answer = unhex(VECTOR_ANSWER).unwrap();
        assert_eq!(seal::open(&keys.to_client, &answer_aad, &answer).as_deref(), Ok(&b"{\"files\":[]}"[..]));
    }

    #[test]
    fn the_update_port_is_the_one_nestlocal_lists_to_wake_the_unit() {
        assert_eq!(UPDATE_PORT, update_wire::PORT);
    }

    #[test]
    fn reach_knocks_on_the_matter_port_before_each_try() {
        use std::io::Read;
        let knocks = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let timing = Timing { knock: knocks.local_addr().unwrap().port(), ..FAST };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let tries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = tries.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let _ = stream.read(&mut [0; 4096]);
                // Asleep for two tries: each connection drops unanswered.
                if counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 2 {
                    crate::push::tests::reply(&mut stream, 200, "{\"open\":true}");
                }
            }
        });
        assert!(reach(&agent(), &url(&addr), &addr, &timing).unwrap().open);
        let tries = tries.load(std::sync::atomic::Ordering::SeqCst);
        assert!(tries >= 3, "{tries}");
        knocks.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let mut datagram = [0xff; 8];
        for _ in 0..tries {
            let (length, _) = knocks.recv_from(&mut datagram).unwrap();
            assert_eq!(&datagram[..length], [0]);
        }
        assert!(knocks.recv_from(&mut datagram).is_err(), "a knock for each try, no more");
    }

    #[test]
    fn a_laptop_name_is_at_most_32_printable_characters() {
        assert_eq!(sanitized("kitchen-laptop.local"), "kitchen-laptop.local");
        assert_eq!(sanitized(&format!(" \u{7}{}é\n", "b".repeat(40))), "b".repeat(32));
        assert_eq!(sanitized("\t\u{1b} "), "unfr0zen");
        assert_eq!(sanitized(""), "unfr0zen");
        let name = laptop_name();
        assert!(!name.is_empty() && name.len() <= 32 && name.bytes().all(|b| (0x20..=0x7e).contains(&b)), "{name:?}");
    }

    const CODE: &str = "40718263";
    /// The status of a unit whose window is open.
    pub(crate) fn window_open() -> Status {
        Status { open: true, ..Status::default() }
    }
    const FAST: Timing = Timing { connect: Duration::from_secs(2), poll: Duration::from_millis(10), wait: Duration::from_secs(5),
                                  request: Duration::from_secs(2), knock: 9 };

    /// How a scripted unit answers a key exchange in a window with trust.
    #[derive(Clone, Copy)]
    enum Grant {
        /// Grants, but confirms the session key alone.
        Unconfirmed,
        /// Something on the way changes the laptop id the unit granted.
        Tampered,
        /// Something on the way changes the unit id the unit granted.
        TamperedUnit,
        /// The unit grants in place of a laptop, and something on the way
        /// moves the replaced id into the laptop's, so the confirmation
        /// reads as one that adds.
        Shifted,
    }

    /// A unit that answers key exchanges alone: its address, and each
    /// exchange's body and session key, once it is made.
    fn exchanging(grant: Grant) -> (String, std::sync::mpsc::Receiver<(serde_json::Value, Vec<u8>)>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (sent, exchanges) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((_, value)) = line.split_once(':').filter(|(key, _)| key.eq_ignore_ascii_case("content-length")) {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                let message = unhex(request["message"].as_str().unwrap()).unwrap();
                let (spake, ours) = Spake2::<Ed25519Group>::start_b(&Password::new(CODE.as_bytes()), &Identity::new(CLIENT), &Identity::new(UNIT));
                let key = spake.finish(&message).unwrap();
                let (unit, laptop) = ("ab".repeat(16), "cd".repeat(16));
                let confirm = match grant {
                    Grant::Unconfirmed => sign(&key, CONFIRMATION),
                    Grant::Shifted => sign(&key, &format!("{CONFIRMATION}\n{unit}\n{laptop}\n{}", "22".repeat(16))),
                    _ => sign(&key, &format!("{CONFIRMATION}\n{unit}\n{laptop}")),
                };
                let (unit, laptop) = match grant {
                    Grant::Tampered => (unit, "ef".repeat(16)),
                    Grant::TamperedUnit => ("ef".repeat(16), laptop),
                    Grant::Shifted => (unit, format!("{laptop}\n{}", "22".repeat(16))),
                    _ => (unit, laptop),
                };
                let answer = serde_json::json!({"session": "01".repeat(16), "message": hex(&ours), "confirm": confirm,
                                                "grant": {"unit": unit, "laptop": laptop}}).to_string();
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", answer.len());
                let _ = sent.send((request, key));
            }
        });
        (addr, exchanges)
    }

    #[test]
    fn a_grant_is_stored_only_when_its_confirmation_verifies() {
        let (unit, _, _) = ids();
        let (addr, served) = trusting(Trusting { prompt: Some("allowed"), grants: Some(true), ..Trusting::new(&unit, None) });
        let store = trust::Store::scratch();
        connect_at(&addr, Code::Given(CODE), &|_| {}, &FAST, &store).unwrap();
        let served = served.lock().unwrap();
        assert_eq!(served.bodies[0]["name"], laptop_name(), "the exchange names the laptop");
        let key = served.session.clone().unwrap().1;
        assert_eq!(store.get(&unit), Some(trust::Entry { unit: unit.clone(), laptop: GRANTED.into(),
                                                          key: trust_key(&key, &unit, GRANTED), address: addr.clone() }));
        // A grant the unit did not confirm is a wrong code's answer.
        let (addr, _) = exchanging(Grant::Unconfirmed);
        let store = trust::Store::scratch();
        let error = authenticate(&agent(), &addr, Code::Given(CODE), &window_open(), &|_| {}, &FAST, &store).err().unwrap();
        assert_eq!(error.to_string(), WRONG);
        assert!(store.get(&unit).is_none());
    }

    #[test]
    fn a_tampered_grant_is_not_stored() {
        for tampered in [Grant::Tampered, Grant::TamperedUnit] {
            let (addr, _) = exchanging(tampered);
            let store = trust::Store::scratch();
            let error = authenticate(&agent(), &addr, Code::Given(CODE), &window_open(), &|_| {}, &FAST, &store).err().unwrap();
            assert_eq!(error.to_string(), WRONG);
            assert!(store.get(&"ab".repeat(16)).is_none() && store.get(&"ef".repeat(16)).is_none());
        }
    }

    #[test]
    fn a_grant_whose_ids_are_not_ids_is_unreadable() {
        let (addr, _) = exchanging(Grant::Shifted);
        let store = trust::Store::scratch();
        let error = authenticate(&agent(), &addr, Code::Given(CODE), &window_open(), &|_| {}, &FAST, &store).err().unwrap();
        assert_eq!(error.to_string(), "the unit's key exchange is unreadable");
        assert!(store.get(&"ab".repeat(16)).is_none());
    }

    /// A unit with an id, which trusts a laptop or not: it answers the
    /// status, trusted and code exchanges, the prompt, and info, as
    /// nestlocal does.
    struct Trusting {
        unit: String,
        /// The laptop it trusts, and the key it holds for it.
        trusts: Option<(String, [u8; 32])>,
        /// Its answer to any trusted exchange instead.
        refuses: Option<(u16, &'static str)>,
        /// The laptops trusted exchanges named, prompts raised, and code
        /// exchanges made.
        laptops: Vec<String>,
        opened: u32,
        exchanges: u32,
        /// The prompt's state: allowed from the first status check after it
        /// asks, which opens the window.
        prompt: Option<&'static str>,
        session: Option<(String, Vec<u8>)>,
        /// Sessions closed.
        closed: u32,
        /// Cancel was pressed on its notice: sealed requests are refused.
        cancelled: bool,
        /// The bodies of the opens and code exchanges it took.
        bodies: Vec<serde_json::Value>,
        /// Its code exchanges grant GRANTED: in place of the laptop they ask
        /// to replace when true, else as one added.
        grants: Option<bool>,
        /// Info requests it answered.
        infos: u32,
    }

    /// The laptop id a Trusting unit grants.
    const GRANTED: &str = "efefefefefefefefefefefefefefefef";

    impl Trusting {
        fn new(unit: &str, trusts: Option<(String, [u8; 32])>) -> Trusting {
            Trusting { unit: unit.into(), trusts, refuses: None, laptops: Vec::new(), opened: 0, exchanges: 0, prompt: None,
                       session: None, closed: 0, cancelled: false, bodies: Vec::new(), grants: None, infos: 0 }
        }

        /// The unit's side of an exchange under password, which asks to
        /// replace the laptop of id replaces if any: its answer, with its
        /// grant for a code exchange (code).
        fn keyed(&mut self, password: &[u8], message: &[u8], code: bool, replaces: Option<&str>) -> String {
            let (spake, ours) = Spake2::<Ed25519Group>::start_b(&Password::new(password), &Identity::new(CLIENT), &Identity::new(UNIT));
            let key = spake.finish(message).unwrap();
            let session = format!("{:032x}", self.laptops.len() as u32 + self.exchanges);
            let mut answer = serde_json::json!({"session": session, "message": hex(&ours), "confirm": sign(&key, CONFIRMATION)});
            if let Some(replace) = self.grants.filter(|_| code) {
                let replaces = replaces.filter(|_| replace);
                answer["grant"] = serde_json::json!({"unit": self.unit, "laptop": GRANTED});
                answer["confirm"] = sign(&key, &confirmation(Some((&self.unit, GRANTED)), replaces)).into();
            }
            self.session = Some((session, key));
            answer.to_string()
        }
    }

    /// Serves unit on 127.0.0.1: its address, and the unit to inspect.
    fn trusting(unit: Trusting) -> (String, std::sync::Arc<std::sync::Mutex<Trusting>>) {
        use std::io::{BufRead, BufReader, Read};
        use crate::push::tests::{reply, reply_bytes, sealed_answer};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let unit = std::sync::Arc::new(std::sync::Mutex::new(unit));
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
                let header = |name: &str| head.iter().find_map(|line| line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.trim().to_string()));
                let mut body = vec![0; header("Content-Length").map_or(0, |n| n.parse().unwrap())];
                reader.read_exact(&mut body).unwrap();
                let request = head.first().cloned().unwrap_or_default();
                let mut unit = shared.lock().unwrap();
                let prompt_id = "01".repeat(16);
                if request.starts_with("GET /update ") {
                    if unit.prompt == Some("asking") {
                        unit.prompt = Some("allowed");
                    }
                    let status = serde_json::json!({"open": unit.prompt == Some("allowed"), "unit": unit.unit,
                                                    "prompt": unit.prompt.map(|state| serde_json::json!({"id": prompt_id, "state": state}))});
                    reply(&mut stream, 200, &status.to_string());
                } else if request.starts_with("POST /update/open ") {
                    (unit.opened, unit.prompt) = (unit.opened + 1, Some("asking"));
                    unit.bodies.push(serde_json::from_slice(&body).unwrap());
                    reply(&mut stream, 202, &serde_json::json!({"prompt": "asking", "id": prompt_id}).to_string());
                } else if request.starts_with("POST /update/session ") {
                    let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    let message = unhex(request["message"].as_str().unwrap()).unwrap();
                    match request["laptop"].as_str() {
                        Some(laptop) => {
                            unit.laptops.push(laptop.to_string());
                            match (unit.refuses, unit.trusts.clone()) {
                                (Some((status, text)), _) => reply(&mut stream, status, text),
                                (None, Some((trusted, key))) if trusted == laptop => {
                                    let answer = unit.keyed(&key, &message, false, None);
                                    reply(&mut stream, 200, &answer);
                                }
                                _ => reply(&mut stream, 404, "not trusted"),
                            }
                        }
                        None if unit.prompt == Some("allowed") => {
                            unit.exchanges += 1;
                            unit.bodies.push(request.clone());
                            let answer = unit.keyed(CODE.as_bytes(), &message, true, request["replaces"].as_str());
                            reply(&mut stream, 200, &answer);
                        }
                        None => reply(&mut stream, 403, "window closed"),
                    }
                } else {
                    let path = ["/update/info", "/update/close"].into_iter().find(|path| request.starts_with(&format!("POST {path} ")))
                        .unwrap_or_else(|| panic!("{request}"));
                    let answer = match unit.session.clone() {
                        _ if unit.cancelled => (409, b"cancelled on the thermostat".to_vec()),
                        Some((session, key)) if Some(&session) == header("X-Update-Session").as_ref() => {
                            match path {
                                "/update/close" => (unit.closed, unit.session) = (unit.closed + 1, None),
                                _ => unit.infos += 1,
                            }
                            sealed_answer(&body, path, &session, &key, |_| match path {
                                "/update/close" => b"{}".to_vec(),
                                _ => serde_json::to_vec(&Info {
                                    nlmodel: "Display-3.4".into(), version: "v0.4.1".into(), overlay: Some(1), free_bytes: 1 << 20, scratch_free_bytes: None,
                                    shell_binary: "0 755".into(), pending: false,
                                }).unwrap(),
                            }).map_or((401, Vec::new()), |answer| (200, answer))
                        }
                        _ => (403, b"window closed".to_vec()),
                    };
                    reply_bytes(&mut stream, answer.0, &answer.1);
                }
            }
        });
        (addr, unit)
    }

    /// A unit's id, the laptop id it trusts, and its trust key.
    fn ids() -> (String, String, [u8; 32]) {
        ("ab".repeat(16), "cd".repeat(16), [7; 32])
    }

    #[test]
    fn a_trusted_laptop_connects_without_asking() {
        let (unit, laptop, key) = ids();
        let (addr, served) = trusting(Trusting::new(&unit, Some((laptop.clone(), key))));
        let store = trust::Store::scratch();
        store.put(&trust::Entry { unit: unit.clone(), laptop: laptop.clone(), key, address: "192.168.1.50:8443".into() }).unwrap();
        let said = std::cell::RefCell::new(Vec::new());
        // A code given is not tried while the trust holds.
        for code in [Code::Ask(&|_| panic!("asked for a code")), Code::Given(CODE)] {
            let (session, status) = connect_at(&addr, code, &|line| said.borrow_mut().push(line.to_string()), &FAST, &store).unwrap();
            assert_eq!(status.unit.as_deref(), Some(unit.as_str()));
            assert_eq!(session.info().unwrap().nlmodel, "Display-3.4");
            // The unit ends a trusted session: idle, or at its limit.
            served.lock().unwrap().session = None;
            assert_eq!(session.info().unwrap_err().to_string(), ENDED);
        }
        assert!(said.borrow().iter().any(|line| line == TRUSTING), "{said:?}");
        let served = served.lock().unwrap();
        assert_eq!((served.laptops.clone(), served.opened, served.exchanges), (vec![laptop.clone(), laptop], 0, 0));
    }

    #[test]
    fn not_trusted_falls_back_to_the_prompt_and_keeps_the_entry() {
        let (unit, laptop, key) = ids();
        let entry = trust::Entry { unit: unit.clone(), laptop: laptop.clone(), key, address: "192.168.1.50:8443".into() };
        // Not trusted, or trusted under another key: the unit's answer is
        // not authenticated, so the entry stays.
        for (trusts, why) in [(None, NOT_TRUSTED_NOW), (Some((laptop.clone(), [8; 32])), UNCONFIRMED)] {
            let (addr, served) = trusting(Trusting::new(&unit, trusts));
            let store = trust::Store::scratch();
            store.put(&entry).unwrap();
            let said = std::cell::RefCell::new(Vec::new());
            let progress = |line: &str| said.borrow_mut().push(line.to_string());
            let (session, _) = connect_at(&addr, Code::Ask(&|_| Some(CODE.into())), &progress, &FAST, &store).unwrap();
            assert_eq!(session.info().unwrap().nlmodel, "Display-3.4");
            assert!(said.borrow().contains(&format!("{why}: asking on the thermostat instead")), "{said:?}");
            let served = served.lock().unwrap();
            assert_eq!((served.laptops.clone(), served.opened, served.exchanges), (vec![laptop.clone()], 1, 1));
            assert_eq!(store.get(&unit), Some(entry.clone()));
        }
        // A code given in a closed window fails as it would without trust.
        let (addr, _) = trusting(Trusting::new(&unit, None));
        let store = trust::Store::scratch();
        store.put(&entry).unwrap();
        let said = std::cell::RefCell::new(Vec::new());
        let error = connect_at(&addr, Code::Given(CODE), &|line| said.borrow_mut().push(line.to_string()), &FAST, &store).err().unwrap();
        assert_eq!(error.to_string(), CLOSED);
        assert!(said.borrow().contains(&NOT_TRUSTED_NOW.to_string()), "{said:?}");
        assert!(!said.borrow().iter().any(|line| line.contains("asking")), "{said:?}");
        // Its other refusals end the run with the unit's reason.
        for (status, text) in [(429, "one trusted session a second: try again"), (503, "busy: another update holds the thermostat")] {
            let (addr, served) = trusting(Trusting { refuses: Some((status, text)), ..Trusting::new(&unit, None) });
            let store = trust::Store::scratch();
            store.put(&entry).unwrap();
            let error = connect_at(&addr, Code::Ask(&|_| panic!("asked for a code")), &|_| {}, &FAST, &store).err().unwrap();
            assert_eq!(error.to_string(), if status == 503 { busy(text) } else { text.to_string() });
            assert_eq!(served.lock().unwrap().opened, 0);
        }
    }

    #[test]
    fn a_laptop_trusted_before_asks_for_its_old_entry_to_be_replaced() {
        let (unit, laptop, key) = ids();
        let entry = trust::Entry { unit: unit.clone(), laptop: laptop.clone(), key, address: "192.168.1.50:8443".into() };
        let (addr, served) = trusting(Trusting::new(&unit, None));
        let store = trust::Store::scratch();
        store.put(&entry).unwrap();
        connect_at(&addr, Code::Ask(&|_| Some(CODE.into())), &|_| {}, &FAST, &store).unwrap();
        let [open, exchange] = served.lock().unwrap().bodies.clone().try_into().unwrap();
        assert_eq!((open["name"].as_str(), open["laptop"].as_str()), (Some(laptop_name().as_str()), Some(laptop.as_str())));
        let message = unhex(exchange["message"].as_str().unwrap()).unwrap();
        assert_eq!(exchange["replaces"].as_str(), Some(laptop.as_str()));
        assert_eq!(exchange["proof"].as_str(), Some(trust::replace_proof(&entry, &message).as_str()));
        // A code given in a window open already asks the same.
        let (addr, served) = trusting(Trusting { prompt: Some("allowed"), ..Trusting::new(&unit, None) });
        connect_at(&addr, Code::Given(CODE), &|_| {}, &FAST, &store).unwrap();
        let [exchange] = served.lock().unwrap().bodies.clone().try_into().unwrap();
        assert_eq!(exchange["replaces"].as_str(), Some(laptop.as_str()));
        // With no entry for the unit, neither asks.
        let (addr, served) = trusting(Trusting::new(&unit, None));
        connect_at(&addr, Code::Ask(&|_| Some(CODE.into())), &|_| {}, &FAST, &trust::Store::scratch()).unwrap();
        let [open, exchange] = served.lock().unwrap().bodies.clone().try_into().unwrap();
        assert!(open.get("laptop").is_none() && exchange.get("replaces").is_none() && exchange.get("proof").is_none(),
                "{open} {exchange}");
    }

    #[test]
    fn a_grant_replaces_the_old_entry_only_once_the_unit_answers_the_first_request() {
        let (unit, laptop, key) = ids();
        let entry = trust::Entry { unit: unit.clone(), laptop: laptop.clone(), key, address: "192.168.1.50:8443".into() };
        let store = trust::Store::scratch();
        store.put(&entry).unwrap();
        // The session's first request fails: the unit may not have kept the
        // grant, so the old entry stays.
        let (addr, served) = trusting(Trusting { prompt: Some("allowed"), grants: Some(true), cancelled: true,
                                                 ..Trusting::new(&unit, None) });
        let error = connect_at(&addr, Code::Given(CODE), &|_| {}, &FAST, &store).err().unwrap();
        assert_eq!(error.to_string(), CANCELLED);
        assert_eq!((served.lock().unwrap().exchanges, store.get(&unit)), (1, Some(entry.clone())));
        // Answered, it replaces the old entry, in the request made before the
        // session is handed over.
        let (addr, served) = trusting(Trusting { prompt: Some("allowed"), grants: Some(true), ..Trusting::new(&unit, None) });
        let said = std::cell::RefCell::new(Vec::new());
        connect_at(&addr, Code::Given(CODE), &|line| said.borrow_mut().push(line.to_string()), &FAST, &store).unwrap();
        assert_eq!(served.lock().unwrap().infos, 1);
        assert_eq!(store.get(&unit).map(|kept| kept.laptop), Some(GRANTED.to_string()));
        assert!(!said.borrow().iter().any(|line| line == ADDED), "{said:?}");
    }

    #[test]
    fn a_grant_confirmed_as_an_addition_says_the_old_entry_stays_on_the_unit() {
        let (unit, laptop, key) = ids();
        let entry = trust::Entry { unit: unit.clone(), laptop, key, address: "192.168.1.50:8443".into() };
        let store = trust::Store::scratch();
        store.put(&entry).unwrap();
        let (addr, _) = trusting(Trusting { prompt: Some("allowed"), grants: Some(false), ..Trusting::new(&unit, None) });
        let said = std::cell::RefCell::new(Vec::new());
        let (session, _) = connect_at(&addr, Code::Given(CODE), &|line| said.borrow_mut().push(line.to_string()), &FAST, &store).unwrap();
        assert_eq!(session.info().unwrap().nlmodel, "Display-3.4");
        assert!(said.borrow().iter().any(|line| line == ADDED), "{said:?}");
        assert_eq!(store.get(&unit).map(|kept| kept.laptop), Some(GRANTED.to_string()));
        // With no entry, an addition is what was asked for: nothing to say.
        let (addr, _) = trusting(Trusting { prompt: Some("allowed"), grants: Some(false), ..Trusting::new(&unit, None) });
        let said = std::cell::RefCell::new(Vec::new());
        connect_at(&addr, Code::Given(CODE), &|line| said.borrow_mut().push(line.to_string()), &FAST, &trust::Store::scratch()).unwrap();
        assert!(!said.borrow().iter().any(|line| line == ADDED), "{said:?}");
    }

    #[test]
    fn the_grant_confirmations_match_the_shared_test_vectors() {
        let key: Vec<u8> = (0..32).collect();
        let (unit, laptop) = ("00".repeat(16), "11".repeat(16));
        assert!(verify(&key, &confirmation(Some((&unit, &laptop)), None),
                       "58c264233497b21be9737097b52339e0e9bb49c2e241e2e7b4cad2e46384fc83"));
        assert!(verify(&key, &confirmation(Some((&unit, &laptop)), Some(&"22".repeat(16))),
                       "63f9da01a33c95dc3d206c46e26b42739ece99dfdfe46598d8a4beb1f71a777f"));
        assert_eq!(confirmation(None, Some(&laptop)), CONFIRMATION);
    }

    #[test]
    fn an_open_window_s_code_is_used_without_a_trusted_exchange() {
        let (unit, laptop, key) = ids();
        let entry = trust::Entry { unit: unit.clone(), laptop: laptop.clone(), key, address: "192.168.1.50:8443".into() };
        for code in [Code::Ask(&|_| Some(CODE.into())), Code::Given(CODE)] {
            let (addr, served) = trusting(Trusting { prompt: Some("allowed"), ..Trusting::new(&unit, Some((laptop.clone(), key))) });
            let store = trust::Store::scratch();
            store.put(&entry).unwrap();
            let said = std::cell::RefCell::new(Vec::new());
            let (session, _) = connect_at(&addr, code, &|line| said.borrow_mut().push(line.to_string()), &FAST, &store).unwrap();
            assert_eq!(session.info().unwrap().nlmodel, "Display-3.4");
            assert!(said.borrow().contains(&WINDOW_OPEN.to_string()), "{said:?}");
            let served = served.lock().unwrap();
            assert_eq!((served.laptops.len(), served.opened, served.exchanges), (0, 0, 1), "no trusted exchange");
            assert_eq!(store.get(&unit), Some(entry.clone()));
        }
    }

    #[test]
    fn a_notice_the_thermostat_cannot_show_falls_back_to_the_prompt() {
        let (unit, laptop, key) = ids();
        let entry = trust::Entry { unit: unit.clone(), laptop: laptop.clone(), key, address: "192.168.1.50:8443".into() };
        let refusal = "open Settings → UPDATE on the thermostat and press Allow update";
        let (addr, served) = trusting(Trusting { refuses: Some((409, refusal)), ..Trusting::new(&unit, Some((laptop.clone(), key))) });
        let store = trust::Store::scratch();
        store.put(&entry).unwrap();
        let said = std::cell::RefCell::new(Vec::new());
        let progress = |line: &str| said.borrow_mut().push(line.to_string());
        let (session, _) = connect_at(&addr, Code::Ask(&|_| Some(CODE.into())), &progress, &FAST, &store).unwrap();
        assert_eq!(session.info().unwrap().nlmodel, "Display-3.4");
        let why = format!("The thermostat cannot show its notice for this laptop ({refusal}): asking on the thermostat instead");
        assert!(said.borrow().contains(&why), "{said:?}");
        let served = served.lock().unwrap();
        assert_eq!((served.laptops.clone(), served.opened, served.exchanges), (vec![laptop], 1, 1));
        assert_eq!(store.get(&unit), Some(entry));
    }

    #[test]
    fn with_no_code_to_be_had_only_the_trust_keys_a_session() {
        let (unit, laptop, key) = ids();
        let (addr, served) = trusting(Trusting::new(&unit, Some((laptop.clone(), key))));
        let store = trust::Store::scratch();
        store.put(&trust::Entry { unit: unit.clone(), laptop: laptop.clone(), key, address: addr.clone() }).unwrap();
        let (session, _) = connect_at(&addr, Code::Unavailable("no code"), &|_| {}, &FAST, &store).unwrap();
        assert_eq!(session.info().unwrap().nlmodel, "Display-3.4");
        // Not trusted, a window open, or no trust held: the reason, and no
        // prompt raised.
        for (trusts, prompt, store) in [(None, None, &store), (Some((laptop.clone(), key)), Some("allowed"), &store),
                                        (Some((laptop.clone(), key)), None, &trust::Store::scratch())] {
            let (addr, served) = trusting(Trusting { prompt, ..Trusting::new(&unit, trusts) });
            let error = connect_at(&addr, Code::Unavailable("no code"), &|_| {}, &FAST, store).err().unwrap();
            assert_eq!(error.to_string(), "no code");
            let served = served.lock().unwrap();
            assert_eq!((served.opened, served.exchanges), (0, 0));
        }
        assert_eq!(served.lock().unwrap().opened, 0);
    }

    #[test]
    fn a_session_closes_at_its_client_s_word() {
        let (unit, laptop, key) = ids();
        let (addr, served) = trusting(Trusting::new(&unit, Some((laptop.clone(), key))));
        let store = trust::Store::scratch();
        store.put(&trust::Entry { unit: unit.clone(), laptop, key, address: addr.clone() }).unwrap();
        let (session, _) = connect_at(&addr, Code::Ask(&|_| panic!("asked for a code")), &|_| {}, &FAST, &store).unwrap();
        session.close();
        assert_eq!(served.lock().unwrap().closed, 1);
        // A failure to close is no failure of the run.
        session.close();
        assert_eq!(served.lock().unwrap().closed, 1);
    }

    #[test]
    fn a_request_in_a_session_cancelled_on_the_thermostat_says_so() {
        let (unit, laptop, key) = ids();
        let (addr, served) = trusting(Trusting::new(&unit, Some((laptop.clone(), key))));
        let store = trust::Store::scratch();
        store.put(&trust::Entry { unit: unit.clone(), laptop, key, address: addr.clone() }).unwrap();
        let (session, _) = connect_at(&addr, Code::Ask(&|_| panic!("asked for a code")), &|_| {}, &FAST, &store).unwrap();
        assert_eq!(session.info().unwrap().nlmodel, "Display-3.4");
        served.lock().unwrap().cancelled = true;
        assert_eq!(session.info().unwrap_err().to_string(), CANCELLED);
        assert!(store.get(&unit).is_some(), "the trust stays");
    }

    #[test]
    fn a_trusted_session_s_refused_apply_asks_for_no_new_code() {
        let session = |via| Session { agent: agent(), url: String::new(), id: String::new(), keys: seal::keys(&[0; 32]), via };
        for status in [400, 409, 507] {
            assert_eq!(session(Via::Trusted).refusal(status, "why"), format!("the unit refused the update ({status}): why"));
            assert_eq!(session(Via::Asked).refusal(status, "why"), format!("the unit refused the update ({status}): why; {AGAIN}"));
        }
        let lapsed = "the update window closed during the upload";
        assert_eq!(session(Via::Trusted).refusal(403, lapsed), lapsed);
        assert_eq!(session(Via::Given).refusal(403, lapsed), format!("{lapsed}; {AGAIN}"));
        assert_eq!(session(Via::Trusted).refusal(403, "window closed"), ENDED);
        assert_eq!(session(Via::Asked).refusal(403, "window closed"), CLOSED_ASKED);
        assert_eq!(session(Via::Given).refusal(403, "window closed"), CLOSED);
        // Any other 403 is the unit's own reason, as a set outside what an
        // older nestlocal may change.
        let outside = "media/scratch/.unfr0zen is outside what an update may change";
        assert_eq!(session(Via::Trusted).refusal(403, outside), format!("the unit refused the update (403): {outside}"));
        assert_eq!(session(Via::Asked).refusal(403, outside), format!("the unit refused the update (403): {outside}; {AGAIN}"));
        for via in [Via::Trusted, Via::Asked] {
            assert_eq!(session(via).refusal(409, "cancelled on the thermostat"), CANCELLED);
        }
    }

    #[test]
    fn an_apply_body_streams_its_contents_in_chunks() {
        let keys = seal::keys(&[7; 32]);
        for size in [0, 1, CHUNK, CHUNK + 1] {
            let contents: Vec<u8> = (0..size).map(|i| i as u8).collect();
            // In pieces that do not line up with the chunks.
            let body = apply_body(&keys, b"HEADER", b"aad", &contents.chunks(CHUNK / 3 + 1).collect::<Vec<_>>());
            assert_eq!((&body[..4], &body[4..10]), (&6u32.to_be_bytes()[..], &b"HEADER"[..]));
            let mut opener = seal::StreamOpener::new(&keys.to_unit, b"aad", body[10..17].try_into().unwrap());
            let (mut at, mut opened, mut chunks) = (17, Vec::new(), 0);
            loop {
                let length = u32::from_be_bytes(body[at..at + 4].try_into().unwrap()) as usize;
                let (data, last) = opener.chunk(&body[at + 4..at + 4 + length]).unwrap();
                (at, chunks) = (at + 4 + length, chunks + 1);
                opened.extend_from_slice(&data);
                if last {
                    break;
                }
            }
            assert_eq!((opened, at, chunks), (contents, body.len(), size.div_ceil(CHUNK).max(1)), "{size}");
        }
    }
}
