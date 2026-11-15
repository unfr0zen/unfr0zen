//! Replacing nestlocal's binary over Wi-Fi while an update window is open:
//! opened on the UPDATE settings entry, or on an Allow pressed on the prompt the
//! network raises there. Allow and trust also grants the laptop a trust key
//! (trust.rs), which keys its later sessions with no window, behind a notice
//! on the screen.
//!
//!   GET  /update          {"open", "installed", "binary", "running",
//!                         "pending", "rolled_back", "rolled_back_by",
//!                         "prompt", "unit"}: binary the installed
//!                         nestlocal's path, relative to /, whose SHA-256
//!                         installed is; rolled_back once the guard put the last
//!                         set back, until another is swapped in;
//!                         rolled_back_by "thermostat" for a roll back asked
//!                         for there (Cancel, Roll back), else null; prompt
//!                         null, or {"id", "state"} of the last prompt:
//!                         "asking", then its answer for
//!                         prompt::REPORT_FOR: "allowed", "denied",
//!                         "timed_out", "busy" (allowed while an update or
//!                         a session held the window, none opened), or
//!                         "preempted" (a trusted session took the screen
//!                         before anyone answered); unit
//!                         the unit's id once it trusts a laptop, else null
//!   POST /update/open     {"name": the laptop's, and optionally "laptop":
//!                         the id it was trusted as}; answers 200 {"open":
//!                         true} while a window accepts, else raises the
//!                         prompt (prompt.rs) full screen where nlclient
//!                         hands the screen over (display.rs) and answers
//!                         202 {"prompt": "asking", "id"}; refuses 503 while
//!                         another prompt is up, an update runs or a session
//!                         holds the window, 429 once prompt::STRIKES went
//!                         denied or unanswered, and 409 when the screen
//!                         cannot be taken. The prompt offers Allow and
//!                         trust while the trusted list has room, or names
//!                         "laptop": unauthenticated, and laptop ids are no
//!                         secret on the network (they travel in clear, as
//!                         "laptop" and "replaces"), so only its exchange's
//!                         proof replaces it. Either Allow opens the window
//!                         and shows its code full screen, until a session
//!                         proves the code or the window takes no more
//!                         exchanges
//!   POST /update/session  {"message": the client's SPAKE2 message in hex,
//!                         "name": its laptop's}; answers {"session",
//!                         "message", "confirm"}, and "grant": {"unit",
//!                         "laptop"} in a window with trust while the list
//!                         has room, which the unit keeps once a sealed
//!                         request in the session authenticates (window.rs);
//!                         one it fails to keep refuses that request 500 and
//!                         ends the session. With "replaces": the id of a
//!                         laptop the unit trusts, and "proof": the hex
//!                         HMAC-SHA256 under its trust key of
//!                         "nestlocal-update replace\n<unit id>\n<that
//!                         id>\n<message>" (trust.rs), the grant takes that
//!                         laptop's place, full list or not, and "confirm"
//!                         covers the replaced id too; a proof that fails is
//!                         ignored, and logged. With "laptop": the id
//!                         of a laptop the unit trusts, the message is
//!                         keyed by its trust key and needs no window; it
//!                         is refused 404 for a laptop not trusted, 409
//!                         when the screen cannot be taken, 429 within
//!                         EXCHANGE_EVERY of the address's last, and 503
//!                         while a window accepts, a prompt is up, an
//!                         update runs or another laptop's session holds
//!                         the unit. Its first authenticated request shows
//!                         the notice full screen, in place of any prompt
//!                         (answered preempted), which follows the session's
//!                         stages until it ends. A request in it whose
//!                         notice is not on the screen is refused 409, and
//!                         the session dropped. Cancel on the notice ends
//!                         it: its requests are refused 409 "cancelled on
//!                         the thermostat", an upload in it stops at its
//!                         next chunk, nothing staged, and a set it swapped
//!                         in is rolled back in place of the restart
//!   POST /update/info     sealed {}; answers a sealed update_wire::read::Info
//!   POST /update/read     sealed ReadRequest; answers a sealed [Found], in
//!                         the request's order (files.rs)
//!   POST /update/check    sealed CheckRequest {"paths"}; answers a sealed
//!                         Checked {"refused": [{"path", "why"}]}: the paths
//!                         an apply of a set changing them would refuse
//!                         before its contents, in the request's order, by
//!                         the apply's own checks (bundle::refusals). It
//!                         writes nothing, and leaves the session as a read
//!                         does
//!   POST /update/close    sealed {}; answers a sealed {}, and ends the
//!                         session: a trusted one's notice goes. A window
//!                         stays open to other exchanges, unless the
//!                         session took its grant: then it closes, and its
//!                         code goes
//!   POST /update/apply    a sealed update_wire::bundle::Bundle and its
//!                         contents; answers 202 and a sealed Applied, then
//!                         restarts nestlocal or reboots (bundle.rs)
//!
//! A session lasts until its window lapses or a later exchange replaces it;
//! a trusted one, until it goes idle or reaches its limits (window.rs).
//! A request in a closed window or an ended trusted session is refused 403,
//! and in a session replaced 412.
//! A sealed request carries X-Update-Session; its body is
//! update_wire::seal::seal under the session's to_unit key and aad(), and
//! its answer is sealed under to_client and aad() followed by the request's
//! nonce, so it answers that request alone.
//!
//! An apply's body is the sealed set's length (u32 BE), the sealed set, a
//! stream prefix (7 bytes), then the contents of its writes in entry order
//! as stream chunks (update_wire::seal::StreamSealer), each after its
//! length (u32 BE). The stream's AAD, like the answer's, is aad() followed
//! by the sealed set's nonce. The window, or the trusted session, is spent
//! once the set opens.
//! A refusal after the body has begun is sent once the rest of it is read
//! and dropped, for a client that reads only once it has sent it all.
//!
//! While a set is on trial, a notice names who sent it, with Roll back and
//! Hide, until it is kept (commit_when); the screen file update.trial reads
//! "On trial: update from <name>" meanwhile.
//! Where nlclient hands no screen over, the prompt, a trusted session and the
//! trial notice first ask it to and wait up to HANDOVER_FOR (handed_over).
//! Settings → UPDATE's requests (poll) open a window, with trust or not, roll
//! the trial back, or forget a laptop, ending its session as Cancel would.
//!
//! The app takes the unit's address from the person (their router lists
//! it), and looks nothing up by mDNS or DNS-SD. Asleep, the Wi-Fi chip wakes
//! the unit only for frames its patterns match, and none of nlsleepd's own
//! matches a multicast query (matter/wake_patterns.rs): a query for an
//! update service would need a pattern of its own, and the chip's few go to
//! Matter first. The gen 2's WL1271 passes no multicast frame at all while
//! the unit sleeps. What wakes the unit, a knock on Matter's port 5540 or a
//! connection to the update port, is unicast to its address, so neither can
//! find one. A lookup would answer only while the unit happened to be awake.

pub mod bundle;
pub mod display;
pub mod files;
pub mod prompt;
pub mod trust;
pub mod window;

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use hyper::{Method, Request, Response, StatusCode};
use serde_json::json;

use crate::liveness::Liveness;
use crate::store::Resent;
use crate::{handover, logf, settings_ui};
use bundle::{ApplyError, Paths, Staging};
use display::{Action, Display, Stage, View};
use prompt::{Answer, Prompt, Refusal};
use trust::Store;
use update_wire::auth::{is_id, laptop_name, UNNAMED};
use update_wire::bundle::{Bundle, Class};
use update_wire::read::{CheckRequest, Info, ReadRequest};
use update_wire::seal::{self, aad, StreamOpener};
use update_wire::unhex;
use window::{Exchange, ExchangeError, Granting, Window};

/// What the unit does once a set is swapped in (restart).
pub type Restart = Arc<dyn Fn(Class) + Send + Sync>;

/// How long a view that needs the screen waits for nlclient to hand it over
/// once asked (Nudge), and how often it looks.
const HANDOVER_FOR: Duration = Duration::from_secs(3);
const HANDOVER_EVERY: Duration = Duration::from_millis(200);

/// How long nlclient has called in before the trial notice asks it for its
/// screen: a resend just after it starts could undo a change of its own not
/// yet uploaded.
#[cfg(not(test))]
const CALLING_IN_FOR: Duration = Duration::from_secs(30);
#[cfg(test)]
const CALLING_IN_FOR: Duration = Duration::from_secs(2);

/// Asks nlclient to hand its screen over: it leaves keeper.pid at its next
/// bucket commit, and this sends it one (store::Resender).
pub type Nudge = Arc<dyn Fn() -> Resent + Send + Sync>;

const PROTOCOL: &str = "update";
/// The screen's requests beside PROTOCOL's (settings_ui::take_request): a
/// window with trust; to forget the laptop whose id the request holds; to
/// roll the set on trial back.
const TRUST_REQUEST: &str = "update-trust";
const FORGET_REQUEST: &str = "update-forget";
const ROLLBACK_REQUEST: &str = "update-rollback";
const CODE: &str = "update.code";
pub const NOT_ENABLED: &str = "Update not enabled";
pub const READY: &str = "Ready";
const OPEN: &str = "Open. Enter the code in unfr0zen.";
const RECEIVING: &str = "Receiving update...";
const INSTALLING: &str = "Installing...";
const RESTARTING: &str = "Restarting...";
const CLOSED: &str = "Closed";
const STALE: &str = "the session was replaced or expired";
const BAD_EXCHANGE: &str = "a JSON object with a SPAKE2 message in hex and the laptop's name";
const BUSY_TEXT: &str = "busy: another update holds the thermostat";
const NOT_TRUSTED: &str = "not trusted";
const TOO_SOON: &str = "one trusted session a second: try again";
/// The refusal of a session whose grant the unit failed to keep.
const NOT_KEPT: &str = "the thermostat could not keep this laptop's trust";
/// The refusal of a request in a session Cancel ended on the thermostat.
const CANCELLED: &str = "cancelled on the thermostat";
/// Who a roll back asked for on the thermostat is recorded as
/// (bundle::roll_back_now), and GET /update reports as rolled_back_by.
const BY_THERMOSTAT: &str = "thermostat";
/// The screen file present while a set is on trial: "On trial: update from
/// <name>", whatever update.status says meanwhile.
const TRIAL_FILE: &str = "update.trial";
/// How often an address may make a trusted exchange.
const EXCHANGE_EVERY: Duration = Duration::from_millis(if cfg!(test) { 200 } else { 1_000 });
/// Marks the binary with its version and the overlay version it needs, for
/// the app to read from a nestlocal it would push (unfr0zen-host push.rs).
/// Bump overlay-version when nestlocal comes to need files only a newer USB
/// patch installs: init scripts, the screen, kernel modules, the wake filter;
/// and when the guard's journal format changes incompatibly (bundle.rs).
pub const IDENTITY: &str = concat!("\0unfr0zen-nestlocal version=", env!("NESTLOCAL_VERSION"),
                                   " overlay=", include_str!("../../overlay-version"), "\0");
/// How long a key exchange's request may take to arrive: it is at most
/// MESSAGE_LIMIT bytes.
const MESSAGE_FOR: Duration = Duration::from_secs(5);
/// The longest body a key exchange takes: a SPAKE2 message in hex is 66, a
/// name of 32 characters, and a laptop id and a proof in hex, in JSON.
const MESSAGE_LIMIT: usize = 1024;
/// The longest body an open takes: a name of 32 characters and a laptop id
/// in JSON.
const NAME_LIMIT: usize = 512;
const LAPSED: &str = "the update window closed during the upload";
/// The longest sealed request body taken, and an apply's longest sealed
/// set: a read names files and a set lists them; neither carries them.
const REQUEST_LIMIT: usize = 64 << 10;
/// The longest sealed chunk of an apply's contents: a CHUNK, its last flag
/// and its tag.
const SEALED_CHUNK: usize = seal::CHUNK + 1 + 16;
const TAMPERED: &str = "the update's contents failed to authenticate";
/// The most of an apply's body read to deliver its refusal: a set of
/// bundle::MAX_SIZE and its sealing.
const DRAINED: u64 = bundle::MAX_SIZE + 2 * REQUEST_LIMIT as u64;
/// How long a refused apply's body may pause before the refusal is sent
/// without the rest: a client still sending keeps it coming.
const DRAIN_IDLE: Duration = Duration::from_millis(if cfg!(test) { 500 } else { 5_000 });
/// How long a sealed request's body may take to arrive.
const REQUEST_FOR: Duration = Duration::from_secs(10);

/// An apply is being taken, or it installed and the restart is due.
static BUSY: AtomicBool = AtomicBool::new(false);
/// Cancel was pressed on the notice of the apply holding BUSY.
static CANCEL: AtomicBool = AtomicBool::new(false);
/// How long an upload may go without sending anything.
const IDLE_FOR: Duration = Duration::from_secs(30);
/// The longest an update's restart waits for a loader, in seconds.
const FLASH_FOR: u32 = 300;
/// How long a connection has to send a request's head.
const HEAD_FOR: Duration = Duration::from_millis(if cfg!(test) { 300 } else { 10_000 });
/// Connections served at once; more wait to be accepted.
const CONNECTIONS: usize = 4;
/// How long each connection the update API accepts keeps the unit awake.
const AWAKE_FOR: Duration = Duration::from_secs(60);
/// Until when the last connections hold the unit awake (connected).
static AWAKE_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
/// The update window.
static WINDOW: Mutex<Option<Arc<Window>>> = Mutex::new(None);
/// What the update shows full screen.
static DISPLAY: Mutex<Option<Arc<Display>>> = Mutex::new(None);
/// This process runs an update on trial and has not committed it.
static ON_TRIAL: AtomicBool = AtomicBool::new(false);
/// The update API's listener is bound (serve_on).
static LISTENING: AtomicBool = AtomicBool::new(false);
/// The screen shows a window's code, until poll sees the window closed.
static SHOWN: AtomicBool = AtomicBool::new(false);
/// Chips being flashed by a loader nestlocal runs (zb-loader, da-loader),
/// which an update's restart waits out: the next nestlocal resets them.
static FLASHING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Holds off an update's restart while a loader flashes a chip.
pub struct Flashing(());

/// Marks a chip as being flashed until the returned guard drops.
pub fn flashing() -> Flashing {
    FLASHING.fetch_add(1, Ordering::SeqCst);
    Flashing(())
}

impl Drop for Flashing {
    fn drop(&mut self) {
        FLASHING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The version and the overlay version IDENTITY marks.
fn identity() -> (&'static str, Option<u32>) {
    let mut version = "unknown";
    let mut overlay = None;
    for field in IDENTITY.trim_matches('\0').split_whitespace() {
        match field.split_once('=') {
            Some(("version", v)) => version = v,
            Some(("overlay", v)) => overlay = v.parse().ok(),
            _ => {}
        }
    }
    (version, overlay)
}

/// What the update API and the thread that watches the screen share.
#[derive(Clone)]
pub struct Shared {
    pub window: Arc<Window>,
    pub display: Arc<Display>,
    pub prompt: Arc<Prompt>,
    /// The laptops the unit trusts.
    pub trusted: Arc<Mutex<Store>>,
    /// Run when nlclient hands no screen over and a view needs it
    /// (handed_over); without one, the request is refused at once.
    pub nudge: Option<Nudge>,
}

fn trusted(shared: &Shared) -> std::sync::MutexGuard<'_, Store> {
    shared.trusted.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the screen may be taken (check). When nlclient hands none over
/// and there is a nudge, nlclient is asked to, once (later in the wait while
/// it has just changed its bucket), and given HANDOVER_FOR to. It blocks
/// meanwhile: run it off the runtime's thread, holding no lock.
fn handed_over(check: impl Fn() -> Result<(), handover::Unavailable>, nudge: Option<&Nudge>) -> Result<(), handover::Unavailable> {
    let checked = check();
    let (Err(handover::Unavailable::NoHandover), Some(nudge)) = (&checked, nudge) else { return checked };
    let deadline = Instant::now() + HANDOVER_FOR;
    let mut asked = None;
    loop {
        if asked.is_none_or(|resent| resent == Resent::DeviceBusy) {
            let resent = nudge();
            if asked.is_none() || resent != Resent::DeviceBusy {
                match resent {
                    Resent::Sent => logf!("wifi-update: asking nlclient to hand its screen over"),
                    Resent::Recently => logf!("wifi-update: nlclient was asked for its screen within the last minute"),
                    Resent::DeviceBusy => logf!("wifi-update: nlclient has just changed its bucket: asking it for its screen shortly"),
                    Resent::Missing => logf!("wifi-update: nlclient has no bucket yet to be asked for its screen through"),
                }
            }
            asked = Some(resent);
        }
        std::thread::sleep(HANDOVER_EVERY);
        let checked = check();
        if checked != Err(handover::Unavailable::NoHandover) || Instant::now() >= deadline {
            match &checked {
                Ok(()) => logf!("wifi-update: nlclient handed its screen over"),
                Err(why) => logf!("wifi-update: nlclient was asked for its screen: {why}"),
            }
            return checked;
        }
    }
}

/// handed_over for the prompt's check and shared's nudge, off the runtime's
/// thread.
async fn screen_handed_over(shared: &Shared) -> Result<(), handover::Unavailable> {
    let shared = shared.clone();
    tokio::task::spawn_blocking(move || handed_over(|| shared.prompt.screen(), shared.nudge.as_ref())).await
        .unwrap_or(Err(handover::Unavailable::NoHandover))
}

fn register(shared: &Shared) {
    *WINDOW.lock().unwrap_or_else(|e| e.into_inner()) = Some(shared.window.clone());
    *DISPLAY.lock().unwrap_or_else(|e| e.into_inner()) = Some(shared.display.clone());
}

/// The unit stays awake while a window is open or a trusted session holds
/// it, a view shows, an upload is in flight or an update is on trial (the
/// guard's watch and the app's wait run on it), and for AWAKE_FOR after each
/// connection the update API accepts (connected).
pub fn sleep_blocked() -> bool {
    sleep_blocked_at(Instant::now())
}

fn sleep_blocked_at(now: Instant) -> bool {
    connected_within(now) || held(now)
}

/// Whether the update itself keeps the unit awake at now, connections aside.
fn held(now: Instant) -> bool {
    BUSY.load(Ordering::SeqCst) || ON_TRIAL.load(Ordering::SeqCst)
        || WINDOW.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|w| w.is_open(now))
        || DISPLAY.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|d| d.showing())
}

/// A connection the update API accepted at now holds the unit awake until
/// AWAKE_FOR later, unless an earlier one holds it longer: the app's status
/// checks and prompt polls keep a sleeping unit reachable between them. Any
/// device on the network can extend it by connecting again, which matters
/// only on a unit without a C wire, whose battery its sleep spares.
fn connected(now: Instant) {
    let mut until = AWAKE_UNTIL.lock().unwrap_or_else(|e| e.into_inner());
    let later = now + AWAKE_FOR;
    if until.is_none_or(|until| until < later) {
        *until = Some(later);
    }
}

/// Whether a connection holds the unit awake at now.
fn connected_within(now: Instant) -> bool {
    AWAKE_UNTIL.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|until| now < until)
}

/// Each interface's IPv4 addresses, by name.
fn ipv4_addresses() -> Vec<(String, std::net::Ipv4Addr)> {
    if_addrs::get_if_addrs().unwrap_or_default().into_iter().filter_map(|i| match i.addr {
        if_addrs::IfAddr::V4(v4) => Some((i.name, v4.ip)),
        _ => None,
    }).collect()
}

/// The IPv4 address the client reaches the unit at: iface's, or else any
/// but loopback's.
fn ipv4(iface: &str) -> Option<std::net::Ipv4Addr> {
    let v4 = ipv4_addresses();
    v4.iter().find(|(name, _)| name == iface)
        .or_else(|| v4.iter().find(|(_, ip)| !ip.is_loopback()))
        .map(|(_, ip)| *ip)
}

/// iface's own IPv4 address, if it has one.
fn iface_ipv4(iface: &str) -> Option<std::net::Ipv4Addr> {
    ipv4_addresses().into_iter().find(|(name, _)| name == iface).map(|(_, ip)| ip)
}

/// Shows a newly opened window's code and the unit's address on the
/// Update page; the address.
fn show(code: &str, iface: &str, by: &str) -> String {
    let ip = ipv4(iface).map(|ip| ip.to_string()).unwrap_or_default();
    if let Err(e) = settings_ui::publish(CODE, format!("{code}\n{ip}\n").as_bytes()) {
        logf!("wifi-update: {e}");
    }
    settings_ui::status(PROTOCOL, OPEN);
    SHOWN.store(true, Ordering::SeqCst);
    logf!("wifi-update: window opened by {by} for {}s at {ip}", window::OPEN_FOR.as_secs());
    ip
}

/// Takes a window's code off the display if it shows and hide, asked under
/// the display's lock, holds: a window opened meanwhile cannot slip between
/// the two.
fn hide_code(display: &Display, why: &str, hide: impl FnOnce() -> bool) {
    let hidden = display.update(|shown| {
        let hidden = matches!(shown, Some(View::Code { .. })) && hide();
        if hidden {
            *shown = None;
        }
        hidden
    });
    if hidden {
        logf!("wifi-update: the code is off the screen: {why}");
    }
}

/// Acts on a press on the display.
fn act(shared: &Shared, iface: &str, action: Action, now: Instant) {
    match action {
        Action::Hide => {
            hide_code(&shared.display, "hidden on the thermostat", || true);
            hide_trial(&shared.display);
        }
        Action::Allow => settle(shared, iface, Answer::Allowed, now),
        Action::AllowTrust => settle(shared, iface, Answer::AllowedTrust, now),
        Action::Deny => settle(shared, iface, Answer::Denied, now),
        Action::Cancel => cancel(shared, now),
        Action::RollBack => roll_back_trial(&shared.display),
    }
}

/// Acts on Cancel on a trusted session's notice: the session ends, and a
/// request in it is refused CANCELLED. An apply under way stops at its next
/// chunk (Frames::contents), or, once its set is swapping, rolls the set
/// back in place of the restart (Server::install): a swap is never broken
/// into.
fn cancel(shared: &Shared, now: Instant) {
    let took = shared.display.update(|shown| {
        if !matches!(shown, Some(View::Notice { .. })) {
            return false;
        }
        let busy = BUSY.load(Ordering::SeqCst);
        if busy {
            CANCEL.store(true, Ordering::SeqCst);
        }
        shared.window.cancel() || busy
    });
    if took {
        logf!("wifi-update: Cancel on the notice: the trusted session is cancelled");
    }
    end_notice(shared, now);
}

/// Acts on Forget on the screen: the unit stops trusting the laptop of id,
/// and a session of the laptop's ends as Cancel on its notice ends it
/// (cancel), however far its apply went.
fn forget(shared: &Shared, id: &str, now: Instant) {
    if !is_id(id) {
        logf!("wifi-update: Forget on the thermostat: the request holds no laptop id");
        return;
    }
    let short = &id[..8];
    // The store stays locked until the session is ended: an exchange
    // installs its session under it (Server::trusted_session).
    let mut store = trusted(shared);
    let name = store.find(id).map(|laptop| laptop.name.clone());
    match (&name, store.forget(id)) {
        (Some(name), Ok(true)) => logf!("wifi-update: Forget on the thermostat: {name} (laptop {short}) is no longer trusted"),
        (_, Err(e)) => logf!("wifi-update: Forget on the thermostat: laptop {short}: {e}"),
        _ => logf!("wifi-update: Forget on the thermostat: laptop {short} is not trusted"),
    }
    let (ended, applying) = shared.window.forget(id);
    drop(store);
    let busy = applying && BUSY.load(Ordering::SeqCst);
    if busy {
        CANCEL.store(true, Ordering::SeqCst);
    }
    if ended || busy {
        logf!("wifi-update: Forget on the thermostat: laptop {short}'s trusted session is cancelled");
    }
    end_notice(shared, now);
}

/// Shows the notice of the trusted laptop name's session at stage.
fn notice(display: &Display, name: &str, stage: Stage) {
    display.update(|shown| *shown = Some(View::Notice { name: name.to_string(), stage }));
}

/// Takes the notice off the screen once its session ended and no update
/// runs: idle, past its limits, or spent on a set that is done. A set that
/// installed holds BUSY, and the notice, until the restart.
fn end_notice(shared: &Shared, now: Instant) {
    let ended = shared.display.update(|shown| {
        let ended = matches!(shown, Some(View::Notice { .. })) && !BUSY.load(Ordering::SeqCst) && !shared.window.holds_trusted(now);
        if ended {
            *shown = None;
        }
        ended
    });
    if ended {
        logf!("wifi-update: the trusted session ended: the notice is off the screen");
    }
}

/// Answers the prompt shown, in one step of the display's: either Allow
/// opens the window and shows its code in the prompt's place, unless an
/// update runs or a session holds the window, when it is answered Busy;
/// Allow and trust, where the prompt offers it (raise), opens it with trust
/// in the prompt's laptop. Busy, Deny and a timeout give the screen back.
/// Nothing happens once it is answered.
fn settle(shared: &Shared, iface: &str, answer: Answer, now: Instant) {
    shared.display.update(|shown| {
        let answer = match answer {
            Answer::Allowed | Answer::AllowedTrust if BUSY.load(Ordering::SeqCst) || shared.window.is_open(now) => Answer::Busy,
            answer => answer,
        };
        let Some((answer, name)) = shared.prompt.answer(answer, now) else { return };
        match answer {
            Answer::Allowed | Answer::AllowedTrust => {
                let trust = answer == Answer::AllowedTrust;
                let store = trusted(shared);
                let (code, by) = if trust {
                    (shared.window.open_trusting(store.unit_id().to_string(), Some(name), now), "an Allow and trust on the thermostat")
                } else {
                    (shared.window.open(now), "an Allow on the thermostat")
                };
                drop(store);
                let ip = show(&code, iface, by);
                *shown = Some(View::Code { code, ip });
            }
            Answer::Denied | Answer::TimedOut | Answer::Busy | Answer::Preempted => {
                if matches!(shown, Some(View::Prompt { .. })) {
                    *shown = None;
                }
            }
        }
    });
}

/// Acts on the screen's requests: Forget, Roll back (roll_back_trial), and
/// opening the window and showing its code, with trust while the trusted
/// list has room for Allow and trust, unless an upload is under way. Then
/// answers a prompt left ASK_FOR as Deny; takes the code off the screen
/// once no exchange is left, however the window was opened; and a trusted
/// session's notice once the session ended.
fn poll(shared: &Shared, iface: &str, now: Instant) {
    let (window, display) = (&shared.window, &shared.display);
    end_notice(shared, now);
    if let Some(id) = settings_ui::take_request_text(FORGET_REQUEST, 64) {
        forget(shared, &id, now);
    }
    if settings_ui::take_request(ROLLBACK_REQUEST) {
        roll_back_trial(display);
    }
    let allow = settings_ui::take_request(PROTOCOL);
    let trust = settings_ui::take_request(TRUST_REQUEST);
    if allow || trust {
        if BUSY.load(Ordering::SeqCst) {
            logf!("wifi-update: an update is in progress: the screen's request is dropped");
        } else {
            let store = trusted(shared);
            let code = match trust && !store.full() {
                true => window.open_trusting(store.unit_id().to_string(), None, now),
                false => window.open(now),
            };
            drop(store);
            show(&code, iface, if trust { "an Allow and trust on the screen" } else { "the screen" });
            shared.prompt.clear_strikes();
            hide_code(display, "the screen opened another window", || true);
            return;
        }
    }
    if shared.prompt.due(now) {
        settle(shared, iface, Answer::TimedOut, now);
    }
    code_gone(shared, now);
}

/// Takes a window's code off the screen and the Update page once the window
/// takes no more exchanges, however it closed.
fn code_gone(shared: &Shared, now: Instant) {
    let window = &shared.window;
    hide_code(&shared.display, "the window takes no more exchanges", || !window.accepting(now));
    if !window.accepting(now) && SHOWN.swap(false, Ordering::SeqCst) {
        settings_ui::remove(CODE);
        // An upload says how it went instead: Refused: or Restarting...
        if !window.spent() && !BUSY.load(Ordering::SeqCst) {
            settings_ui::status(PROTOCOL, CLOSED);
        }
        logf!("wifi-update: window closed");
    }
}

/// Watches for the screen's update requests every second, and acts on
/// presses on the display as they come.
pub fn watch_requests(shared: Shared, iface: String) {
    register(&shared);
    let actions = shared.display.actions();
    let spawned = std::thread::Builder::new().name("update".into()).spawn(move || loop {
        match actions.recv_timeout(Duration::from_secs(1)) {
            Ok(action) => act(&shared, &iface, action, Instant::now()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(Duration::from_secs(1)),
        }
        poll(&shared, &iface, Instant::now());
    });
    if let Err(e) = spawned {
        logf!("wifi-update: {e}");
    }
}

/// Keeps the update this process runs on trial once nlclient talks to it,
/// unless it has taken a newer update meanwhile: that one is the next
/// nestlocal's to keep (bundle::keep). A set that rebooted the unit is kept
/// only once the unit is also back on the network (on_network): otherwise
/// the guard rolls it back, rather than keep a unit no update can reach.
fn commit_when_alive(live: Arc<Liveness>, paths: Paths, iface: String, nudge: Option<Nudge>) {
    commit_when(live, paths, iface, Hooks { address: iface_ipv4, handover: handover::available, restart: Arc::new(restart), nudge })
}

/// Watches the set on trial (commit_when_alive) as nestlocal starts; with
/// none on trial, forgets who sent the last.
pub fn watch_trial(live: Arc<Liveness>, paths: Paths, iface: String, nudge: Option<Nudge>) {
    if bundle::pending(&paths) {
        commit_when_alive(live, paths, iface, nudge);
    } else {
        forget_sender(&paths);
        settings_ui::remove(TRIAL_FILE);
    }
}

/// Removes who sent the last set, once it is kept or gone.
fn forget_sender(paths: &Paths) {
    match std::fs::remove_file(paths.sender()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => logf!("wifi-update: {}: {e}", paths.sender().display()),
        _ => {}
    }
}

/// The set on trial this process runs, between commit_when's thread and
/// presses on its notice.
struct Trial {
    /// Who sent it (bundle::Paths::sender).
    name: String,
    paths: Paths,
    restart: Restart,
    /// Hide took the notice off for the rest of the trial.
    hidden: bool,
    /// Kept, or Roll back pressed: nothing more happens to the set here.
    over: bool,
}

/// The set on trial, once commit_when watches it.
static TRIAL: Mutex<Option<Arc<Mutex<Trial>>>> = Mutex::new(None);

fn trial_state(trial: &Mutex<Trial>) -> std::sync::MutexGuard<'_, Trial> {
    trial.lock().unwrap_or_else(|e| e.into_inner())
}

fn current_trial() -> Option<Arc<Mutex<Trial>>> {
    TRIAL.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The display the update API registered.
fn registered_display() -> Option<Arc<Display>> {
    DISPLAY.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// TRIAL_FILE's content: the screen offers Roll back on it under
/// Settings → UPDATE.
fn trial_text(name: &str) -> String {
    format!("On trial: update from {name}")
}

/// Shows the trial notice where nothing else shows and the screen can be
/// taken, unless Hide took it off, the trial is over, or a newer update is
/// being taken, whose restart gives the screen back first. Why the screen
/// could not be taken, when that alone kept the notice off.
fn show_trial(trial: &Mutex<Trial>, handover: prompt::Handover) -> Option<handover::Unavailable> {
    let state = trial_state(trial);
    let display = registered_display()?;
    if state.hidden || state.over || BUSY.load(Ordering::SeqCst) || display.showing() {
        return None;
    }
    if let Err(why) = handover(settings_ui::dir()) {
        return Some(why);
    }
    display.update(|shown| {
        if shown.is_none() {
            *shown = Some(View::Trial { name: state.name.clone() });
        }
    });
    None
}

/// Takes the trial notice off the screen if it shows; whether it did.
fn take_trial_off(display: &Display) -> bool {
    display.update(|shown| {
        let on = matches!(shown, Some(View::Trial { .. }));
        if on {
            *shown = None;
        }
        on
    })
}

/// Hide on the trial notice: it stays off for the rest of the trial.
fn hide_trial(display: &Display) {
    let Some(trial) = current_trial() else { return };
    let mut state = trial_state(&trial);
    if take_trial_off(display) {
        state.hidden = true;
        logf!("wifi-update: the trial notice is hidden on the thermostat");
    }
}

/// Roll back on the trial notice: the guard rolls the set back as
/// nestlocal restarts, unless the set is kept already, or a newer one is
/// being taken (BUSY), which restarts nestlocal itself.
fn roll_back_trial(display: &Display) {
    let Some(trial) = current_trial() else { return };
    let mut state = trial_state(&trial);
    if state.over {
        return;
    }
    if BUSY.load(Ordering::SeqCst) {
        logf!("wifi-update: Roll back on the thermostat: another update is being taken: not rolled back");
        return;
    }
    take_trial_off(display);
    match bundle::roll_back_now(&state.paths, BY_THERMOSTAT) {
        Ok(()) => {
            state.over = true;
            settings_ui::remove(TRIAL_FILE);
            logf!("wifi-update: Roll back on the thermostat: the update from {} is rolled back", state.name);
            if !display.release(display::CONFIRM_FOR) {
                logf!("wifi-update: the screen was not given back before the restart");
            }
            (state.restart)(Class::Restart);
        }
        Err(e) => logf!("wifi-update: {}: {e}: not rolled back", state.paths.pending.display()),
    }
}

/// Keeps the set on trial unless Roll back was pressed; its notice goes,
/// and once it is kept, who sent it.
fn keep_trial(trial: &Mutex<Trial>) {
    let mut state = trial_state(trial);
    if state.over {
        return;
    }
    state.over = true;
    let paths = &state.paths;
    let kept = match bundle::keep(paths) {
        Ok(true) => true,
        Ok(false) => {
            logf!("wifi-update: the marker is a newer update's: left to it");
            false
        }
        Err(e) => {
            logf!("wifi-update: {}: {e}", paths.pending.display());
            false
        }
    };
    if kept {
        ON_TRIAL.store(false, Ordering::SeqCst);
        forget_sender(paths);
        logf!("wifi-update: nlclient is calling in: the update is kept");
    }
    if let Some(display) = registered_display() {
        settings_ui::remove(TRIAL_FILE);
        take_trial_off(&display);
    }
}

/// What commit_when finds the unit and its screen through, and restarts
/// nestlocal with.
struct Hooks {
    /// iface's IPv4 address.
    address: fn(&str) -> Option<std::net::Ipv4Addr>,
    handover: prompt::Handover,
    restart: Restart,
    nudge: Option<Nudge>,
}

/// commit_when_alive, through hooks. Until the set is kept, its notice
/// shows (show_trial), naming who sent it, with Roll back and Hide; once
/// nlclient has called in for CALLING_IN_FOR, it is asked for its screen
/// (handed_over) the first time it hands none over.
fn commit_when(live: Arc<Liveness>, paths: Paths, iface: String, hooks: Hooks) {
    let Hooks { address, handover, restart, nudge } = hooks;
    ON_TRIAL.store(true, Ordering::SeqCst);
    let name = laptop_name(&std::fs::read_to_string(paths.sender()).unwrap_or_default());
    // The update API, if it serves, registered its display first.
    if registered_display().is_some() {
        if let Err(e) = settings_ui::publish(TRIAL_FILE, trial_text(&name).as_bytes()) {
            logf!("wifi-update: {e}");
        }
    }
    let trial = Arc::new(Mutex::new(Trial { name, paths: paths.clone(), restart, hidden: false, over: false }));
    *TRIAL.lock().unwrap_or_else(|e| e.into_inner()) = Some(trial.clone());
    let spawned = std::thread::Builder::new().name("update-commit".into()).spawn(move || {
        let reboots = bundle::journal_class(&paths) == Class::Reboot;
        let (mut waiting, mut nudged, mut calling_since) = (false, false, None);
        loop {
            if trial_state(&trial).over {
                return;
            }
            if live.alive() && !BUSY.load(Ordering::SeqCst) {
                if !reboots || on_network(&iface, address) {
                    keep_trial(&trial);
                    return;
                }
                if !waiting {
                    waiting = true;
                    logf!("wifi-update: nlclient is calling in: the update rebooted the unit, so it is kept once {iface} has an address and the update API listens");
                }
            }
            calling_since = if live.alive() { calling_since.or_else(|| Some(Instant::now())) } else { None };
            let settled = calling_since.is_some_and(|since: Instant| since.elapsed() >= CALLING_IN_FOR);
            if show_trial(&trial, handover) == Some(handover::Unavailable::NoHandover) && settled && !nudged {
                nudged = true;
                if handed_over(|| handover(settings_ui::dir()), nudge.as_ref()).is_ok() {
                    show_trial(&trial, handover);
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    if let Err(e) = spawned {
        logf!("wifi-update: {e}");
    }
}

/// Whether the unit is back on the network: iface has an IPv4 address
/// other than loopback's, and the update API listens (LISTENING).
fn on_network(iface: &str, address: fn(&str) -> Option<std::net::Ipv4Addr>) -> bool {
    LISTENING.load(Ordering::SeqCst) && address(iface).is_some_and(|ip| !ip.is_loopback())
}

/// Restarts nestlocal on the unit through its init script, from a process
/// group of its own so the restart outlives this one. The kill makes sure
/// this process stops whatever the script's stop matches. Without the
/// script, it exits, and monit starts it again.
pub fn restart_unit() {
    use std::os::unix::process::CommandExt;
    let script = format!("sleep 1; kill {} 2>/dev/null; /etc/init.d/nestlocal restart", std::process::id());
    if let Err(e) = std::process::Command::new("/bin/sh").arg("-c").arg(script).process_group(0).spawn() {
        logf!("wifi-update: restart: {e}: exiting");
        std::process::exit(1);
    }
}

/// Reboots the unit from a process group of its own: a set that changed
/// more than nestlocal's own files takes effect from boot.
pub fn reboot_unit() {
    use std::os::unix::process::CommandExt;
    if let Err(e) = std::process::Command::new("/bin/sh").arg("-c").arg("sleep 1; reboot").process_group(0).spawn() {
        logf!("wifi-update: reboot: {e}: exiting");
        std::process::exit(1);
    }
}

/// Restarts nestlocal or reboots the unit, as a set of class needs.
pub fn restart(class: Class) {
    match class {
        Class::Restart => restart_unit(),
        Class::Reboot => reboot_unit(),
    }
}

/// Gives the screen back before nestlocal is stopped: stopped while it
/// holds the screen, it would leave the console, the panel's page and its
/// backlight as the take set them.
async fn give_back(display: Arc<Display>) {
    let released = tokio::task::spawn_blocking(move || display.release(display::CONFIRM_FOR)).await.unwrap_or(false);
    if !released {
        logf!("wifi-update: the screen was not given back before the restart");
    }
}

/// Holds BUSY for one apply; lets it go when dropped, unless the set
/// installed and the restart is due.
struct Busy {
    installed: bool,
}

impl Busy {
    fn take() -> Option<Busy> {
        BUSY.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).ok().map(|_| {
            CANCEL.store(false, Ordering::SeqCst);
            Busy { installed: false }
        })
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        if !self.installed {
            BUSY.store(false, Ordering::SeqCst);
        }
    }
}

/// Why a release bundle's install (crate::update) waits: its live patch
/// removes a set's journal and backups, which would end the trial without
/// the guard's rollback.
const INSTALL_ON_TRIAL: &str = "a change sent over Wi-Fi is on trial: keep it or roll it back on the thermostat, then install again";
/// Why a release bundle's install waits while a set is taken: both write
/// the root volume.
const INSTALL_APPLYING: &str = "a change sent over Wi-Fi is being installed: install again once it has finished";

/// Holds BUSY for an install of a release bundle, as an apply does: no set
/// is taken while its live patch writes. Dropped, it lets BUSY go.
pub struct InstallHold(Busy);

impl InstallHold {
    /// The install went in: restarts the unit with reboot, holding BUSY
    /// until it goes down. A reboot that fails lets BUSY go, so the unit
    /// sleeps and takes sets again; its error is returned.
    pub fn restart(mut self, reboot: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        let rebooted = reboot();
        self.0.installed = rebooted.is_ok();
        rebooted
    }
}

/// The unit for an install of a release bundle, refused with why while an
/// apply runs or a set is on trial.
pub fn hold_install() -> Result<InstallHold, &'static str> {
    hold_install_at(&Paths::unit())
}

fn hold_install_at(paths: &Paths) -> Result<InstallHold, &'static str> {
    let busy = Busy::take().ok_or(INSTALL_APPLYING)?;
    if ON_TRIAL.load(Ordering::SeqCst) || bundle::pending(paths) {
        return Err(INSTALL_ON_TRIAL);
    }
    Ok(InstallHold(busy))
}

/// What a request to open a window came to.
#[derive(Debug, PartialEq, Eq)]
enum Raised {
    /// A window accepts already: its code stands.
    Open,
    /// The prompt is up, with its id.
    Asking(String),
}

/// Raises the prompt for the laptop name at ip and shows it, in one step of
/// the display's, unless a window accepts already. An update under way, a
/// session left in a window that takes no more exchanges, or a trusted
/// session that authenticated, holds the window: a prompt would only replace
/// it. It offers Allow and trust while the trusted list has room, or names
/// laptop, the id the asking laptop says it was trusted as: a grant that
/// proves it replaces it (Server::granting).
fn raise(shared: &Shared, name: String, ip: String, laptop: Option<&str>, now: Instant) -> Result<Raised, Refusal> {
    shared.display.update(|shown| {
        if shared.window.accepting(now) {
            return Ok(Raised::Open);
        }
        if BUSY.load(Ordering::SeqCst) || shared.window.is_open(now) {
            return Err(Refusal::Busy);
        }
        let trust = {
            let store = trusted(shared);
            let listed = laptop.filter(|id| store.find(id).is_some());
            if store.full() {
                match (laptop, listed) {
                    (_, Some(id)) => logf!("wifi-update: the trusted list is full: the prompt offers trust in place of laptop {}, \
                                            once its exchange proves it", short_id(id)),
                    (Some(id), None) => logf!("wifi-update: the trusted list is full, without laptop {}: the prompt offers no trust",
                                              short_id(id)),
                    (None, None) => logf!("wifi-update: the trusted list is full: the prompt offers no trust"),
                }
            }
            !store.full() || listed.is_some()
        };
        let id = shared.prompt.ask(name.clone(), ip.clone(), trust, now)?;
        logf!("wifi-update: asking on the thermostat for {name} ({ip})");
        *shown = Some(View::Prompt { name, ip, trust });
        Ok(Raised::Asking(id))
    })
}

/// The first 8 digits of a laptop id, as the log names it, or a mark for a
/// client's text that is not an id.
fn short_id(id: &str) -> &str {
    if is_id(id) { &id[..8] } else { "(not an id)" }
}

struct Server {
    shared: Shared,
    paths: Paths,
    restart: Restart,
    running: OnceLock<Option<String>>,
    /// The installed binary's SHA-256, once hashed: only an install
    /// changes it, and that restarts nestlocal.
    installed: Mutex<Option<String>>,
    /// When each address last made a trusted exchange, within
    /// EXCHANGE_EVERY.
    exchanged: Mutex<HashMap<IpAddr, Instant>>,
}

/// Serves the update API on listen, for good; the error is why it stopped.
pub async fn serve(listen: String, shared: Shared, paths: Paths, restart: Restart) -> std::io::Error {
    match tokio::net::TcpListener::bind(&listen).await {
        Ok(listener) => {
            logf!("wifi-update: serving {listen}");
            serve_on(listener, shared, paths, restart).await
        }
        Err(e) => e,
    }
}

async fn serve_on(listener: tokio::net::TcpListener, shared: Shared, paths: Paths, restart: Restart) -> std::io::Error {
    register(&shared);
    LISTENING.store(true, Ordering::SeqCst);
    let server = Arc::new(Server {
        shared, paths, restart, running: OnceLock::new(), installed: Mutex::new(None), exchanged: Mutex::new(HashMap::new()),
    });
    // The listener is up for good and answers anyone on the network, so
    // what a connection can hold is bounded: a request per connection, its
    // head in HEAD_FOR, and CONNECTIONS at once.
    let slots = Arc::new(tokio::sync::Semaphore::new(CONNECTIONS));
    loop {
        let Ok(slot) = slots.clone().acquire_owned().await else {
            return std::io::Error::other("connection slots closed");
        };
        let (tcp, peer) = match listener.accept().await {
            Ok(c) => {
                connected(Instant::now());
                c
            }
            Err(e) => {
                logf!("wifi-update: accept: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let server = server.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let service = hyper::service::service_fn(move |req: Request<hyper::body::Incoming>| {
                let server = server.clone();
                async move { Ok::<_, Infallible>(server.handle(req, peer).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(HEAD_FOR)
                .keep_alive(false)
                .serve_connection(hyper_util::rt::TokioIo::new(tcp), service)
                .await;
        });
    }
}

/// The refusal of a request that needs the screen when it cannot be taken.
fn unavailable(why: handover::Unavailable) -> Response<Full<Bytes>> {
    text(StatusCode::CONFLICT, match why {
        handover::Unavailable::InUse => "the thermostat's screen is in use",
        handover::Unavailable::NoHandover => "open Settings → UPDATE on the thermostat and press Allow update",
    })
}

fn reply(status: StatusCode, content_type: &'static str, body: String) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    *response.status_mut() = status;
    response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn text(status: StatusCode, message: &str) -> Response<Full<Bytes>> {
    reply(status, "text/plain", format!("{message}\n"))
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// The SHA-256 of the binary this process runs.
fn running_sha256() -> Option<String> {
    let exe = PathBuf::from("/proc/self/exe");
    let exe = if exe.exists() { exe } else { std::env::current_exe().ok()? };
    bundle::sha256_file(&exe).ok()
}

impl Server {
    fn installed_sha256(&self) -> Option<String> {
        let mut installed = self.installed.lock().unwrap_or_else(|e| e.into_inner());
        if installed.is_none() {
            *installed = bundle::sha256_file(&self.paths.binary()).ok();
        }
        installed.clone()
    }

    async fn handle(self: Arc<Self>, req: Request<hyper::body::Incoming>, peer: std::net::SocketAddr) -> Response<Full<Bytes>> {
        // A browser sends Origin with every request that could change
        // something: a web page has no business here.
        if req.headers().contains_key(hyper::header::ORIGIN) {
            return text(StatusCode::FORBIDDEN, "not for browsers");
        }
        match (req.uri().path(), req.method().clone()) {
            ("/update", Method::GET) => self.status().await,
            ("/update/apply", Method::POST) => match Busy::take() {
                Some(busy) => self.apply(req, busy).await,
                None => text(StatusCode::SERVICE_UNAVAILABLE, "an update is already in progress"),
            },
            ("/update/open", Method::POST) => self.open(req, peer).await,
            ("/update/session", Method::POST) => self.session(req, peer.ip()).await,
            (path @ ("/update/info" | "/update/read" | "/update/check" | "/update/close"), Method::POST) => {
                let path = path.to_string();
                self.sealed(req, &path).await
            }
            ("/update", _) => text(StatusCode::METHOD_NOT_ALLOWED, "GET"),
            ("/update/open" | "/update/session" | "/update/info" | "/update/read" | "/update/check" | "/update/close"
             | "/update/apply", _) => {
                text(StatusCode::METHOD_NOT_ALLOWED, "POST")
            }
            _ => text(StatusCode::NOT_FOUND, "not found"),
        }
    }

    /// Raises the prompt for the laptop the body names, at peer, unless a
    /// window accepts already.
    async fn open(&self, req: Request<hyper::body::Incoming>, peer: std::net::SocketAddr) -> Response<Full<Bytes>> {
        if BUSY.load(Ordering::SeqCst) {
            return text(StatusCode::SERVICE_UNAVAILABLE, "an update is already in progress");
        }
        let body = http_body_util::Limited::new(req.into_body(), NAME_LIMIT).collect();
        let request = match tokio::time::timeout(MESSAGE_FOR, body).await {
            Ok(Ok(body)) => serde_json::from_slice::<serde_json::Value>(&body.to_bytes()).ok().map(|request| {
                (laptop_name(request["name"].as_str().unwrap_or_default()), request["laptop"].as_str().map(str::to_string))
            }),
            _ => None,
        };
        let Some((name, laptop)) = request else {
            return text(StatusCode::BAD_REQUEST, "a JSON object with the laptop's name");
        };
        let (ip, laptop) = (peer.ip().to_string(), laptop.as_deref());
        let mut raised = raise(&self.shared, name.clone(), ip.clone(), laptop, Instant::now());
        if raised == Err(Refusal::Screen(handover::Unavailable::NoHandover)) {
            raised = match screen_handed_over(&self.shared).await {
                Ok(()) => raise(&self.shared, name, ip, laptop, Instant::now()),
                Err(why) => Err(Refusal::Screen(why)),
            };
        }
        match raised {
            Ok(Raised::Open) => reply(StatusCode::OK, "application/json", json!({"open": true}).to_string()),
            Ok(Raised::Asking(id)) => {
                reply(StatusCode::ACCEPTED, "application/json", json!({"prompt": "asking", "id": id}).to_string())
            }
            Err(Refusal::Busy) => text(StatusCode::SERVICE_UNAVAILABLE, BUSY_TEXT),
            Err(Refusal::Strikes) => {
                logf!("wifi-update: {} prompts in a row went unanswered or denied: the network asks no more", prompt::STRIKES);
                text(StatusCode::TOO_MANY_REQUESTS, "open Settings → UPDATE on the thermostat and press Allow update")
            }
            Err(Refusal::Screen(why)) => {
                logf!("wifi-update: no prompt: {why}");
                unavailable(why)
            }
        }
    }

    /// The unit's side of a key exchange from ip (window::Window::exchange),
    /// or of a trusted one when the body names a laptop.
    async fn session(self: Arc<Self>, req: Request<hyper::body::Incoming>, ip: IpAddr) -> Response<Full<Bytes>> {
        let body = http_body_util::Limited::new(req.into_body(), MESSAGE_LIMIT).collect();
        let request = match tokio::time::timeout(MESSAGE_FOR, body).await {
            Ok(Ok(body)) => serde_json::from_slice::<serde_json::Value>(&body.to_bytes()).ok().and_then(|request| {
                let message = unhex(request["message"].as_str()?)?;
                let text = |field: &str| request[field].as_str().map(str::to_string);
                let replaces = text("replaces").zip(text("proof"));
                Some((message, laptop_name(request["name"].as_str().unwrap_or_default()), text("laptop"), replaces))
            }),
            _ => None,
        };
        let Some((message, name, laptop, replaces)) = request else {
            return text(StatusCode::BAD_REQUEST, BAD_EXCHANGE);
        };
        if let Some(laptop) = laptop {
            return self.trusted_session(message, &laptop, ip).await;
        }
        let granting = self.granting(&message, replaces);
        let window = self.shared.window.clone();
        let exchanged = tokio::task::spawn_blocking(move || window.exchange(&message, &name, &granting, Instant::now())).await
            .unwrap_or(Err(ExchangeError::Closed));
        exchange_reply(exchanged)
    }

    /// What a code exchange whose client message is message grants in a
    /// window with trust: the place of the laptop replaces names, when its
    /// proof shows the client holds that laptop's trust key; else a laptop
    /// added, while the trusted list has room. In a window with trust, a
    /// proof that fails and a grant withheld are logged, by laptop id.
    fn granting(&self, message: &[u8], replaces: Option<(String, String)>) -> Granting {
        let store = trusted(&self.shared);
        let grants = self.shared.window.grants(Instant::now());
        let old = match replaces {
            Some((old, proof)) if store.proves(&old, message, &proof) => return Granting::Replace(old),
            Some((old, _)) => {
                if grants {
                    logf!("wifi-update: the exchange's proof for laptop {} fails: it replaces no laptop", short_id(&old));
                }
                Some(old)
            }
            None => None,
        };
        if !store.full() {
            return Granting::Add;
        }
        if grants {
            match old {
                Some(old) => logf!("wifi-update: the trusted list is full and the exchange proves no laptop in it \
                                    (laptop {}): no laptop is trusted", short_id(&old)),
                None => logf!("wifi-update: the trusted list is full and the exchange names no laptop in it: no laptop is trusted"),
            }
        }
        Granting::Withheld
    }

    /// A trusted exchange for the laptop of id laptop, from ip, unless ip
    /// made one within EXCHANGE_EVERY, the unit does not trust the laptop,
    /// the screen cannot show its notice, or something else holds the unit.
    async fn trusted_session(&self, message: Vec<u8>, laptop: &str, ip: IpAddr) -> Response<Full<Bytes>> {
        if !self.paced(ip, Instant::now()) {
            return text(StatusCode::TOO_MANY_REQUESTS, TOO_SOON);
        }
        let Some(laptop) = trusted(&self.shared).find(laptop).cloned() else {
            logf!("wifi-update: a trusted session for a laptop not trusted, from {ip}");
            return text(StatusCode::NOT_FOUND, NOT_TRUSTED);
        };
        if let Err(why) = screen_handed_over(&self.shared).await {
            logf!("wifi-update: no trusted session for {}: {why}", laptop.name);
            return unavailable(why);
        }
        #[cfg(test)]
        tests::between_lookup_and_exchange();
        let shared = self.shared.clone();
        let (id, name) = (laptop.id, laptop.name);
        // Under the display's lock, as a prompt is raised: neither slips
        // between the other's check and its start. The session goes in
        // under the store's, while the store trusts the laptop: a Forget
        // before it is refused here, and one after it ends it (forget).
        let exchanged = tokio::task::spawn_blocking(move || shared.display.update(|_| {
            if BUSY.load(Ordering::SeqCst) || shared.prompt.asking() {
                return Some(Err(ExchangeError::Busy));
            }
            let store = trusted(&shared);
            let laptop = store.find(&id)?;
            Some(shared.window.exchange_trusted(laptop, &message, Instant::now()))
        })).await.unwrap_or(Some(Err(ExchangeError::Closed)));
        let Some(exchanged) = exchanged else {
            logf!("wifi-update: {name} was forgotten during its exchange, from {ip}");
            return text(StatusCode::NOT_FOUND, NOT_TRUSTED);
        };
        if let Ok(exchange) = &exchanged {
            logf!("wifi-update: trusted session {} for {name} ({ip})", &exchange.session[..8]);
        }
        exchange_reply(exchanged)
    }

    /// Whether ip may make a trusted exchange now, which counts as its last.
    fn paced(&self, ip: IpAddr, now: Instant) -> bool {
        let mut exchanged = self.exchanged.lock().unwrap_or_else(|e| e.into_inner());
        exchanged.retain(|_, at| now.saturating_duration_since(*at) < EXCHANGE_EVERY);
        if exchanged.contains_key(&ip) {
            return false;
        }
        exchanged.insert(ip, now);
        true
    }

    /// A request in session authenticated: a window's code goes off the
    /// screen, the laptop its grant names is trusted, and a trusted session
    /// is kept from going idle, its first request showing its notice in
    /// place of any prompt, which is answered preempted. The trusted
    /// laptop's name; a refusal, and the session dropped, when the unit
    /// failed to keep its grant, or its notice did not reach the screen.
    async fn proved(&self, session: &str, now: Instant) -> Result<Option<String>, Box<Response<Full<Bytes>>>> {
        hide_code(&self.shared.display, "a session proved it", || true);
        if let Err(e) = self.trust(session, now) {
            // Answered, the client would keep a trust the unit does not.
            self.shared.window.close(session);
            code_gone(&self.shared, Instant::now());
            return Err(Box::new(text(StatusCode::INTERNAL_SERVER_ERROR, &format!("{NOT_KEPT}: {e}"))));
        }
        let Some((name, first)) = self.shared.window.touch(session, now) else { return Ok(None) };
        if first {
            logf!("wifi-update: trusted session {} for {name} authenticated", &session[..8]);
            let prompt = &self.shared.prompt;
            self.shared.display.update(|shown| {
                // No strike against the prompt's laptop: it asked while
                // this session had yet to prove itself.
                prompt.answer(Answer::Preempted, now);
                *shown = Some(View::Notice { name: name.clone(), stage: Stage::Connected });
            });
        }
        let (display, wanted) = (self.shared.display.clone(), name.clone());
        let on_screen = tokio::task::spawn_blocking(move || {
            display.confirmed(display::CONFIRM_FOR, |shown| matches!(shown, Some(View::Notice { name, .. }) if *name == wanted))
        }).await.unwrap_or(false);
        if !on_screen {
            logf!("wifi-update: the notice for {name} is not on the screen: its session is dropped");
            self.shared.window.close(session);
            end_notice(&self.shared, Instant::now());
            return Err(Box::new(unavailable(handover::Unavailable::InUse)));
        }
        Ok(Some(name))
    }

    /// Trusts the laptop session's grant names, in place of the one it
    /// replaces if any, once a request in session first authenticates
    /// (window::Window::take_grant); why it could not.
    fn trust(&self, session: &str, now: Instant) -> std::io::Result<()> {
        let Some(grant) = self.shared.window.take_grant(session, now) else { return Ok(()) };
        let (id, name) = (grant.laptop.clone(), grant.name.clone());
        let added = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let laptop = trust::Laptop { id: grant.laptop, name: grant.name, key: grant.key, added };
        let mut store = trusted(&self.shared);
        let kept = match &grant.replaces {
            Some(old) => store.replace(old, laptop).map(|replaced| replaced.then_some(old)),
            None => store.add(laptop).map(|()| None),
        };
        match &kept {
            Ok(Some(old)) => logf!("wifi-update: {name} is trusted, as laptop {}: replaces laptop {}", &id[..8], &old[..8]),
            Ok(None) => logf!("wifi-update: {name} is trusted, as laptop {}", &id[..8]),
            Err(e) => logf!("wifi-update: {name} is not trusted: {e}: its session is ended"),
        }
        kept.map(|_| ())
    }

    /// Answers a sealed request to path in its session, which it leaves open.
    async fn sealed(self: Arc<Self>, req: Request<hyper::body::Incoming>, path: &str) -> Response<Full<Bytes>> {
        let (session, now) = (header(req.headers(), "x-update-session").to_string(), Instant::now());
        if self.shared.window.cancelled(&session) {
            return text(StatusCode::CONFLICT, CANCELLED);
        }
        // A client whose window lapsed opens another; one replaced does not.
        if self.shared.window.closes_at(now).is_none() {
            return text(StatusCode::FORBIDDEN, "window closed");
        }
        let Some(key) = self.shared.window.session(&session, now) else {
            return text(StatusCode::PRECONDITION_FAILED, STALE);
        };
        let body = http_body_util::Limited::new(req.into_body(), REQUEST_LIMIT).collect();
        let body = match tokio::time::timeout(REQUEST_FOR, body).await {
            Ok(Ok(body)) => body.to_bytes(),
            _ => return text(StatusCode::BAD_REQUEST, "a sealed request of at most 64 KiB"),
        };
        let (keys, aad) = (seal::keys(&key), aad(path, &session));
        let (Ok(plaintext), Some(nonce)) = (seal::open(&keys.to_unit, &aad, &body), seal::nonce_of(&body)) else {
            logf!("wifi-update: a request to {path} failed to authenticate");
            return text(StatusCode::UNAUTHORIZED, "authentication failed");
        };
        if path == "/update/close" {
            self.shared.window.close(&session);
            logf!("wifi-update: session {} closed", &session[..session.len().min(8)]);
            code_gone(&self.shared, Instant::now());
            end_notice(&self.shared, Instant::now());
        } else {
            match self.proved(&session, now).await {
                Ok(Some(name)) => notice(&self.shared.display, &name, Stage::Reading),
                Ok(None) => {}
                Err(refusal) => return *refusal,
            }
        }
        let answer = match path {
            "/update/close" => Ok(b"{}".to_vec()),
            "/update/info" => serde_json::to_vec(&self.info().await),
            "/update/check" => match serde_json::from_slice::<CheckRequest>(&plaintext) {
                Ok(request) => {
                    let paths = self.paths.clone();
                    let checked = tokio::task::spawn_blocking(move || bundle::check(&paths, &request)).await;
                    match checked.map_err(std::io::Error::other) {
                        Ok(Ok(checked)) => serde_json::to_vec(&checked),
                        Ok(Err(e)) | Err(e) => return text(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
                    }
                }
                Err(e) => return text(StatusCode::BAD_REQUEST, &format!("not a check request: {e}")),
            },
            _ => match serde_json::from_slice::<ReadRequest>(&plaintext) {
                Ok(request) => {
                    let (root, mounts) = (self.paths.root.clone(), self.paths.mounts.clone());
                    let found = tokio::task::spawn_blocking(move || files::read(&root, &mounts, &request)).await;
                    match found {
                        Ok(found) => serde_json::to_vec(&found),
                        Err(e) => return text(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
                    }
                }
                Err(e) => return text(StatusCode::BAD_REQUEST, &format!("not a read request: {e}")),
            },
        };
        let answer = match answer {
            Ok(answer) => answer,
            Err(e) => return text(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        };
        let sealed = seal::seal(&keys.to_client, &[aad.as_slice(), nonce].concat(), &answer);
        let mut response = Response::new(Full::new(Bytes::from(sealed)));
        response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
        response
    }

    /// The unit as a session first sees it.
    async fn info(self: Arc<Self>) -> Info {
        let read = tokio::task::spawn_blocking(move || {
            let root = &self.paths.root;
            let free_bytes = (self.paths.free_space)(root).unwrap_or_else(|e| {
                logf!("wifi-update: {}: {e}", root.display());
                0
            });
            let scratch = root.join(files::SCRATCH_MOUNT);
            let scratch_free_bytes = files::scratch_mounted(&self.paths.mounts)
                .then(|| (self.paths.free_space)(&scratch).ok()).flatten();
            let overlay = std::fs::read_to_string(&self.paths.overlay).ok().and_then(|v| v.trim().parse::<u32>().ok());
            (files::nlmodel(), overlay, free_bytes, scratch_free_bytes, files::shell_binary(root), bundle::pending(&self.paths))
        });
        let (nlmodel, overlay, free_bytes, scratch_free_bytes, shell_binary, pending) = read.await.unwrap_or_default();
        Info {
            nlmodel,
            version: identity().0.to_string(),
            overlay,
            free_bytes,
            scratch_free_bytes,
            shell_binary,
            pending: pending || ON_TRIAL.load(Ordering::SeqCst),
        }
    }

    /// The window, the binaries, and whether an update is on trial: until
    /// this process commits the one it runs, through a rollback's restart;
    /// whether the guard put the last one back; the prompt; and the unit's
    /// id, which a laptop it trusts knows its key by.
    async fn status(self: Arc<Self>) -> Response<Full<Bytes>> {
        let open = self.shared.window.accepting(Instant::now());
        let server = self.clone();
        let (installed, binary, running, pending, overlay, (rolled_back, rolled_back_by)) = tokio::task::spawn_blocking(move || {
            let installed = server.installed_sha256();
            let binary = server.paths.installed();
            let running = server.running.get_or_init(running_sha256).clone();
            let overlay = std::fs::read_to_string(&server.paths.overlay).ok().and_then(|v| v.trim().parse::<u32>().ok());
            let rolled_back = (server.paths.rolled_back().exists(), bundle::cancelled_by(&server.paths));
            (installed, binary, running, bundle::pending(&server.paths) || ON_TRIAL.load(Ordering::SeqCst), overlay, rolled_back)
        }).await.unwrap_or_default();
        let unit = {
            let store = trusted(&self.shared);
            (!store.list().is_empty()).then(|| store.unit_id().to_string())
        };
        let body = json!({"open": open, "installed": installed, "binary": binary, "running": running, "pending": pending,
                          "version": identity().0, "overlay": overlay, "rolled_back": rolled_back,
                          "rolled_back_by": rolled_back_by,
                          "prompt": self.shared.prompt.status(Instant::now()).map(|(id, state)| json!({"id": id, "state": state})),
                          "unit": unit});
        reply(StatusCode::OK, "application/json", body.to_string())
    }

    /// Takes an apply while busy holds BUSY. The body must keep coming, and
    /// be in before the window lapses.
    async fn apply(self: Arc<Self>, req: Request<hyper::body::Incoming>, busy: Busy) -> Response<Full<Bytes>> {
        let (session, now) = (header(req.headers(), "x-update-session").to_string(), Instant::now());
        if self.shared.window.cancelled(&session) {
            return text(StatusCode::CONFLICT, CANCELLED);
        }
        let Some(closes) = self.shared.window.closes_at(now) else {
            return text(StatusCode::FORBIDDEN, "window closed");
        };
        let closes = tokio::time::Instant::from_std(closes);
        let Some(key) = self.shared.window.session(&session, now) else {
            logf!("wifi-update: an apply's session was replaced or expired");
            return text(StatusCode::PRECONDITION_FAILED, STALE);
        };
        let mut body = Frames { body: req.into_body(), buf: Vec::new(), read: 0 };
        let answer = match self.receive(&mut body, &session, &key, now, closes).await {
            Ok(received) => {
                // From here it runs to the end even if the client goes.
                let server = self.clone();
                tokio::spawn(async move { server.install(received, busy).await }).await
                    .unwrap_or_else(|e| refuse(ApplyError::Io(std::io::Error::other(e))))
            }
            Err(refusal) => {
                body.drain(closes).await;
                drop(busy);
                *refusal
            }
        };
        end_notice(&self.shared, Instant::now());
        answer
    }

    /// Opens and stages the set body brings in session, then its contents,
    /// which have until the window lapses or the trusted session's OPEN_FOR
    /// (window::Window::spend).
    async fn receive(&self, body: &mut Frames, session: &str, key: &[u8], now: Instant, closes: tokio::time::Instant)
                     -> Result<Received, Box<Response<Full<Bytes>>>> {
        // Until the set opens, a refusal leaves the session to its client,
        // which alone holds its key.
        let sealed = match body.length(REQUEST_LIMIT, closes).await {
            Ok(length) => body.take(length, closes).await,
            Err(cut) => Err(cut),
        };
        let sealed = match sealed {
            Ok(sealed) => sealed,
            Err(Cut::Lapsed) => return Err(Box::new(text(StatusCode::FORBIDDEN, LAPSED))),
            Err(_) => return Err(Box::new(text(StatusCode::BAD_REQUEST, "an update set sealed in its session, of at most 64 KiB"))),
        };
        let (keys, aad) = (seal::keys(key), aad("/update/apply", session));
        let (Ok(set), Some(nonce)) = (seal::open(&keys.to_unit, &aad, &sealed), seal::nonce_of(&sealed)) else {
            logf!("wifi-update: an apply failed to authenticate");
            return Err(Box::new(text(StatusCode::UNAUTHORIZED, "authentication failed")));
        };
        let stream_aad = [aad.as_slice(), nonce].concat();
        if CANCEL.load(Ordering::SeqCst) || self.shared.window.cancelled(session) {
            return Err(Box::new(text(StatusCode::CONFLICT, CANCELLED)));
        }
        let trusted = self.proved(session, now).await?;
        // An authentic set spends the code or the trusted session, whatever
        // becomes of it: another takes a new window or session.
        let Some(until) = self.shared.window.spend(session, now) else {
            // Cancelled while its notice was confirmed.
            if self.shared.window.cancelled(session) {
                return Err(Box::new(text(StatusCode::CONFLICT, CANCELLED)));
            }
            return Err(Box::new(text(StatusCode::PRECONDITION_FAILED, STALE)));
        };
        let closes = tokio::time::Instant::from_std(until);
        if let Some(name) = &trusted {
            notice(&self.shared.display, name, Stage::Receiving);
        }
        settings_ui::remove(CODE);
        let set: Bundle = match serde_json::from_slice(&set) {
            Ok(set) => set,
            Err(e) => return Err(Box::new(refused(StatusCode::BAD_REQUEST, &format!("not an update set: {e}")))),
        };
        let size: u64 = set.entries.iter().map(|entry| entry.size).sum();
        logf!("wifi-update: receiving {} files, {size} bytes", set.entries.len());
        settings_ui::status(PROTOCOL, RECEIVING);
        let paths = self.paths.clone();
        let begun = tokio::task::spawn_blocking(move || bundle::begin(&paths, set)).await
            .unwrap_or_else(|e| Err(ApplyError::Io(std::io::Error::other(e))));
        let mut stage = begun.map_err(|e| Box::new(refuse(e)))?;
        if let Err(cut) = body.contents(&keys.to_unit, &stream_aad, &mut stage, closes).await {
            return Err(Box::new(match cut {
                Cut::Lapsed => refused(StatusCode::FORBIDDEN, LAPSED),
                Cut::Stalled => refuse(ApplyError::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "the upload stalled"))),
                Cut::Tampered => refused(StatusCode::BAD_REQUEST, TAMPERED),
                Cut::Failed(e) => refuse(ApplyError::Io(std::io::Error::other(e))),
                Cut::Refused(e) => refuse(e),
                Cut::Cancelled => refused(StatusCode::CONFLICT, CANCELLED),
            }));
        }
        Ok(Received { stage, to_client: keys.to_client, aad: stream_aad, trusted })
    }

    /// Checks and swaps in a received set; on success, answers with what it
    /// swapped in, sealed to the apply, and has nestlocal restart or the
    /// unit reboot once no chip is flashing.
    async fn install(&self, received: Received, mut busy: Busy) -> Response<Full<Bytes>> {
        // Cancelled once the contents were in: the staging goes, unswapped.
        if CANCEL.load(Ordering::SeqCst) {
            return refused(StatusCode::CONFLICT, CANCELLED);
        }
        let Received { stage, to_client, aad, trusted } = received;
        let stage_notice = |stage| {
            if let Some(name) = &trusted {
                notice(&self.shared.display, name, stage);
            }
        };
        settings_ui::status(PROTOCOL, INSTALLING);
        stage_notice(Stage::Installing);
        // Who sent the set, for its trial notice; durable with the swap's sync.
        let (sender, name) = (self.paths.sender(), trusted.clone().unwrap_or_else(|| UNNAMED.to_string()));
        let finished = tokio::task::spawn_blocking(move || {
            if let Err(e) = std::fs::write(&sender, name) {
                logf!("wifi-update: {}: {e}", sender.display());
            }
            let finished = stage.finish();
            if finished.is_err() {
                let _ = std::fs::remove_file(&sender);
            }
            finished
        }).await.unwrap_or_else(|e| Err(ApplyError::Io(std::io::Error::other(e))));
        let (applied, class) = match finished {
            Ok(finished) => finished,
            Err(e) => return refuse(e),
        };
        busy.installed = true;
        *self.installed.lock().unwrap_or_else(|e| e.into_inner()) = None;
        settings_ui::status(PROTOCOL, RESTARTING);
        stage_notice(Stage::Restarting);
        let then = match class {
            Class::Restart => "restarting",
            Class::Reboot => "rebooting",
        };
        logf!("wifi-update: installed {} files: {then}", applied.files.len());
        let (restart, paths, display) = (self.restart.clone(), self.paths.clone(), self.shared.display.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let mut waited = 0;
            while FLASHING.load(Ordering::SeqCst) > 0 && waited < FLASH_FOR {
                if waited == 0 {
                    logf!("wifi-update: a chip is being flashed: the restart waits");
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                waited += 1;
            }
            give_back(display).await;
            if !CANCEL.load(Ordering::SeqCst) {
                restart(class);
                return;
            }
            // Cancelled while it swapped or since: the guard rolls the set
            // back as nestlocal restarts, and reboots for a reboot's set.
            let marked = tokio::task::spawn_blocking(move || bundle::roll_back_now(&paths, BY_THERMOSTAT)).await
                .unwrap_or_else(|e| Err(std::io::Error::other(e)));
            match marked {
                Ok(()) => {
                    logf!("wifi-update: cancelled on the thermostat: the update is rolled back");
                    restart(Class::Restart);
                }
                Err(e) => {
                    logf!("wifi-update: cancelled on the thermostat, but the rollback failed: {e}");
                    restart(class);
                }
            }
        });
        let answer = match serde_json::to_vec(&applied) {
            Ok(answer) => answer,
            Err(e) => return text(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        };
        let mut response = Response::new(Full::new(Bytes::from(seal::seal(&to_client, &aad, &answer))));
        *response.status_mut() = StatusCode::ACCEPTED;
        response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
        response
    }
}

/// A key exchange's answer: the session, the unit's message and its
/// confirmation, and a window's grant.
fn exchange_reply(exchanged: Result<Exchange, ExchangeError>) -> Response<Full<Bytes>> {
    match exchanged {
        Ok(exchange) => {
            logf!("wifi-update: key exchange for session {}", &exchange.session[..8]);
            let mut body = json!({"session": exchange.session, "message": exchange.message, "confirm": exchange.confirm});
            if let Some((unit, laptop)) = exchange.grant {
                body["grant"] = json!({"unit": unit, "laptop": laptop});
            }
            reply(StatusCode::OK, "application/json", body.to_string())
        }
        Err(ExchangeError::Closed) => text(StatusCode::FORBIDDEN, "window closed"),
        Err(ExchangeError::Malformed) => {
            logf!("wifi-update: a malformed key exchange");
            text(StatusCode::BAD_REQUEST, BAD_EXCHANGE)
        }
        Err(ExchangeError::Busy) => text(StatusCode::SERVICE_UNAVAILABLE, BUSY_TEXT),
    }
}

/// A set received in full: staged, the key and AAD its answer is sealed
/// under, and the trusted laptop's name when a trusted session sent it.
struct Received {
    stage: Staging,
    to_client: [u8; 32],
    aad: Vec<u8>,
    trusted: Option<String>,
}

/// Why an apply's body stopped short.
enum Cut {
    /// The window lapsed.
    Lapsed,
    /// Nothing came for IDLE_FOR.
    Stalled,
    /// It ended early, or a part did not open: the contents are not the
    /// ones sealed in the session.
    Tampered,
    Failed(hyper::Error),
    /// The staged set refused what it was sent.
    Refused(ApplyError),
    /// Cancel was pressed on the thermostat.
    Cancelled,
}

/// An apply's body, its frames gathered until a part is whole.
struct Frames {
    body: hyper::body::Incoming,
    buf: Vec<u8>,
    /// The bytes read so far.
    read: u64,
}

impl Frames {
    /// Reads another frame into buf, within IDLE_FOR and before closes;
    /// false at the body's end.
    async fn more(&mut self, closes: tokio::time::Instant) -> Result<bool, Cut> {
        let by = closes.min(tokio::time::Instant::now() + IDLE_FOR);
        match tokio::time::timeout_at(by, self.body.frame()).await {
            Ok(None) => Ok(false),
            Ok(Some(Ok(frame))) => {
                if let Ok(data) = frame.into_data() {
                    self.read += data.len() as u64;
                    self.buf.extend_from_slice(&data);
                }
                Ok(true)
            }
            Ok(Some(Err(e))) => Err(Cut::Failed(e)),
            Err(_) if by == closes => Err(Cut::Lapsed),
            Err(_) => Err(Cut::Stalled),
        }
    }

    /// Reads and drops the rest of the body, while it keeps coming within
    /// DRAIN_IDLE, up to DRAINED bytes, and until the later of closes and
    /// IDLE_FOR from now. A client may send the whole body before it reads
    /// the answer (the app's does): a refusal sent with the body unread is
    /// lost to the closed socket.
    async fn drain(&mut self, closes: tokio::time::Instant) {
        let until = closes.max(tokio::time::Instant::now() + IDLE_FOR);
        self.buf.clear();
        while self.read <= DRAINED {
            let by = until.min(tokio::time::Instant::now() + DRAIN_IDLE);
            match tokio::time::timeout_at(by, self.body.frame()).await {
                Ok(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        self.read += data.len() as u64;
                    }
                }
                _ => return,
            }
        }
    }

    /// The body's next n bytes.
    async fn take(&mut self, n: usize, closes: tokio::time::Instant) -> Result<Vec<u8>, Cut> {
        while self.buf.len() < n {
            if !self.more(closes).await? {
                return Err(Cut::Tampered);
            }
        }
        let rest = self.buf.split_off(n);
        Ok(std::mem::replace(&mut self.buf, rest))
    }

    /// The length (u32 BE) of the next part, at most limit.
    async fn length(&mut self, limit: usize, closes: tokio::time::Instant) -> Result<usize, Cut> {
        let bytes = self.take(4, closes).await?;
        let length = u32::from_be_bytes(bytes.try_into().expect("4 bytes")) as usize;
        if length > limit {
            return Err(Cut::Tampered);
        }
        Ok(length)
    }

    /// Opens the contents' chunks into stage, through the last one, after
    /// which the body ends; stops between chunks at a Cancel.
    async fn contents(&mut self, key: &[u8; 32], aad: &[u8], stage: &mut Staging, closes: tokio::time::Instant)
                      -> Result<(), Cut> {
        let prefix = self.take(7, closes).await?;
        let mut opener = StreamOpener::new(key, aad, prefix.try_into().expect("7 bytes"));
        loop {
            let length = self.length(SEALED_CHUNK, closes).await?;
            let chunk = self.take(length, closes).await?;
            if CANCEL.load(Ordering::SeqCst) {
                return Err(Cut::Cancelled);
            }
            let (data, last) = opener.chunk(&chunk).map_err(|_| Cut::Tampered)?;
            stage.write(&data).map_err(Cut::Refused)?;
            if last {
                break;
            }
        }
        while self.buf.is_empty() {
            if !self.more(closes).await? {
                return Ok(());
            }
        }
        Err(Cut::Tampered)
    }
}

fn refuse(e: ApplyError) -> Response<Full<Bytes>> {
    refused(StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), &e.to_string())
}

/// Refuses an upload the code was spent on, and says why on the screen.
fn refused(status: StatusCode, reason: &str) -> Response<Full<Bytes>> {
    logf!("wifi-update: refused: {reason}");
    settings_ui::status(PROTOCOL, &format!("Refused: {reason}"));
    text(status, reason)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use update_wire::bundle::{Applied, Entry, Kind, BUNDLE_VERSION};
    use update_wire::read::{Found, Info, ReadRequest};
    use update_wire::{hex, random};

    const NEW: &[u8] = b"#!/bin/sh\nexit 0\n";
    const OLD: &[u8] = b"#!/bin/sh\necho old\n";

    /// The update state is the process's: one test at a time.
    static LOCK: Mutex<()> = Mutex::new(());

    /// Runs once in a trusted exchange, after it found its laptop trusted
    /// and before it installs the session.
    type Hook = Box<dyn FnOnce() + Send>;
    static BETWEEN: Mutex<Option<Hook>> = Mutex::new(None);

    pub(super) fn between_lookup_and_exchange() {
        let hook = BETWEEN.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(hook) = hook {
            hook();
        }
    }

    pub(super) fn run<F: std::future::Future>(f: F) -> F::Output {
        let _serial = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        BUSY.store(false, Ordering::SeqCst);
        CANCEL.store(false, Ordering::SeqCst);
        *TRIAL.lock().unwrap() = None;
        ON_TRIAL.store(false, Ordering::SeqCst);
        LISTENING.store(false, Ordering::SeqCst);
        SHOWN.store(false, Ordering::SeqCst);
        *WINDOW.lock().unwrap() = None;
        *DISPLAY.lock().unwrap() = None;
        *AWAKE_UNTIL.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *BETWEEN.lock().unwrap_or_else(|e| e.into_inner()) = None;
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }

    fn paths(name: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("nestlocal-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("root");
        std::fs::create_dir_all(root.join("nestlabs/sbin")).unwrap();
        let paths = Paths {
            journal: root.join("etc/nestlocal/update/journal"),
            staging: root.join("etc/nestlocal/update/staging"),
            pending: dir.join("nestlocal-update.pending"),
            mounts: dir.join("mounts"),
            overlay: dir.join("nestlocal-overlay"),
            free_space: bundle::free_space,
            root,
        };
        std::fs::write(&paths.mounts, "ubi0:root0 / ubifs rw,relatime 0 0\n").unwrap();
        std::fs::write(paths.binary(), OLD).unwrap();
        std::fs::set_permissions(paths.binary(), std::fs::Permissions::from_mode(0o755)).unwrap();
        paths
    }

    /// Where an apply stages its binary.
    fn staged(paths: &Paths) -> PathBuf {
        paths.binary().with_file_name("nestlocal.unfr0zen-new")
    }

    /// The installed binary.
    fn installed(paths: &Paths) -> Vec<u8> {
        std::fs::read(paths.binary()).unwrap()
    }

    struct Unit {
        addr: std::net::SocketAddr,
        window: Arc<Window>,
        paths: Paths,
        restarts: Arc<AtomicUsize>,
        /// The class each restart was asked for, and the hook that records it.
        classes: Arc<Mutex<Vec<Class>>>,
        restart: Restart,
        display: Arc<Display>,
        prompt: Arc<Prompt>,
        trusted: Arc<Mutex<trust::Store>>,
        /// What the display was asked to show: None to hide.
        shown: std::sync::mpsc::Receiver<Option<View>>,
        nudge: Option<Nudge>,
    }

    impl Unit {
        fn shared(&self) -> Shared {
            Shared { window: self.window.clone(), display: self.display.clone(), prompt: self.prompt.clone(),
                     trusted: self.trusted.clone(), nudge: self.nudge.clone() }
        }
    }

    fn no_handover(_: &std::path::Path) -> Result<(), handover::Unavailable> {
        Err(handover::Unavailable::NoHandover)
    }

    fn handover(_: &std::path::Path) -> Result<(), handover::Unavailable> {
        Ok(())
    }

    /// A unit whose screen has no hand-over: the network cannot ask there.
    async fn unit(name: &str) -> Unit {
        unit_with(name, no_handover).await
    }

    async fn unit_with(name: &str, handover: prompt::Handover) -> Unit {
        let (display, shown) = Display::recording();
        unit_on(name, handover, display, shown).await
    }

    /// A unit showing views on display, which reports them to shown.
    async fn unit_on(name: &str, handover: prompt::Handover, display: Display, shown: std::sync::mpsc::Receiver<Option<View>>) -> Unit {
        unit_nudging(name, handover, (display, shown), None).await
    }

    /// unit_on, asking nlclient for its screen through nudge.
    async fn unit_nudging(name: &str, handover: prompt::Handover, (display, shown): (Display, std::sync::mpsc::Receiver<Option<View>>),
                          nudge: Option<Nudge>) -> Unit {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (window, paths, restarts) = (Arc::new(Window::new()), paths(name), Arc::new(AtomicUsize::new(0)));
        let (display, prompt) = (Arc::new(display), Arc::new(Prompt::new(handover)));
        let (counted, classes) = (restarts.clone(), Arc::new(Mutex::new(Vec::new())));
        let recorded = classes.clone();
        let restart: Restart = Arc::new(move |class| {
            recorded.lock().unwrap().push(class);
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let trusted = Arc::new(Mutex::new(trust::Store::scratch()));
        let unit = Unit { addr, window, paths, restarts, classes, restart: restart.clone(), display, prompt, trusted, shown, nudge };
        tokio::spawn(serve_on(listener, unit.shared(), unit.paths.clone(), restart));
        unit
    }

    /// A unit's parts with no server: the window, a recording display, and
    /// a prompt the screen may take for.
    fn parts(window: Window) -> Shared {
        Shared { window: Arc::new(window), display: Arc::new(recording()), prompt: Arc::new(Prompt::new(handover)),
                 trusted: Arc::new(Mutex::new(trust::Store::scratch())), nudge: None }
    }

    /// The code the display was last asked to show, and nothing hidden since.
    fn code_shown(unit: &Unit) -> String {
        match unit.shown.try_recv() {
            Ok(Some(View::Code { code, .. })) => code,
            other => panic!("no code shown: {other:?}"),
        }
    }

    /// One HTTP/1.1 exchange: the status and the body's bytes.
    async fn exchange_bytes(addr: std::net::SocketAddr, head: &str, body: &[u8]) -> (u16, Vec<u8>) {
        let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        tcp.write_all(format!("{head}Host: unit\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        tcp.write_all(body).await.unwrap();
        let mut out = Vec::new();
        tcp.read_to_end(&mut out).await.unwrap();
        let Some(end) = out.windows(4).position(|w| w == b"\r\n\r\n") else { return (0, Vec::new()) };
        let status = String::from_utf8_lossy(&out[..end]).split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        (status, out[end + 4..].to_vec())
    }

    /// One HTTP/1.1 exchange: the status and the body.
    async fn exchange(addr: std::net::SocketAddr, head: &str, body: &[u8]) -> (u16, String) {
        let (status, body) = exchange_bytes(addr, head, body).await;
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    /// A request sealed as the app seals it under session's key: the status,
    /// and the opened answer, or the body as sent when it is not a 200.
    async fn sealed(unit: &Unit, path: &str, session: &str, key: &[u8], plaintext: &[u8]) -> (u16, Vec<u8>) {
        let keys = update_wire::seal::keys(key);
        let aad = aad(path, session);
        let request = update_wire::seal::seal(&keys.to_unit, &aad, plaintext);
        let head = format!("POST {path} HTTP/1.1\r\nContent-Length: {}\r\nX-Update-Session: {session}\r\n", request.len());
        let (status, body) = exchange_bytes(unit.addr, &head, &request).await;
        if status != 200 {
            return (status, body);
        }
        let mut response_aad = aad;
        response_aad.extend_from_slice(update_wire::seal::nonce_of(&request).unwrap());
        (status, update_wire::seal::open(&keys.to_client, &response_aad, &body).expect("the answer opens"))
    }

    async fn info(unit: &Unit, session: &str, key: &[u8]) -> (u16, Vec<u8>) {
        sealed(unit, "/update/info", session, key, b"{}").await
    }

    async fn read(unit: &Unit, session: &str, key: &[u8], names: &[&str], contents: bool) -> (u16, Vec<u8>) {
        let request = ReadRequest { names: names.iter().map(|n| n.to_string()).collect(), contents };
        sealed(unit, "/update/read", session, key, &serde_json::to_vec(&request).unwrap()).await
    }

    fn stale() -> (u16, Vec<u8>) {
        (412, format!("{STALE}\n").into_bytes())
    }

    fn closed() -> (u16, Vec<u8>) {
        (403, b"window closed\n".to_vec())
    }

    async fn get(addr: std::net::SocketAddr) -> serde_json::Value {
        let (status, body) = exchange(addr, "GET /update HTTP/1.1\r\n", b"").await;
        assert_eq!(status, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    }

    fn sha(body: &[u8]) -> String {
        hex(ring::digest::digest(&ring::digest::SHA256, body).as_ref())
    }

    async fn post(addr: std::net::SocketAddr, path: &str, body: &str) -> (u16, String) {
        exchange(addr, &format!("POST {path} HTTP/1.1\r\nContent-Length: {}\r\n", body.len()), body.as_bytes()).await
    }

    /// A key exchange with code, as the app makes it: the session, its key,
    /// and whether the unit confirmed the key.
    async fn session(unit: &Unit, code: &str) -> (String, Vec<u8>, bool) {
        let (session, key, confirmed, _) = granted(unit, code, "laptop").await;
        (session, key, confirmed)
    }

    /// The body of a key exchange's request, as the app sends it.
    fn exchange_body(message: &[u8], name: &str) -> String {
        json!({"message": hex(message), "name": name}).to_string()
    }

    /// session, from a laptop named name, and the grant the unit made: its
    /// unit and laptop ids.
    async fn granted(unit: &Unit, code: &str, name: &str) -> (String, Vec<u8>, bool, Option<(String, String)>) {
        granted_with(unit, code, None, |message| exchange_body(message, name)).await
    }

    /// granted, asking for the place of the laptop of id old with the proof
    /// proof makes of the exchange's message; confirmed as a grant that
    /// replaces old, or one that adds a laptop when replaces is false.
    async fn granted_replacing(unit: &Unit, code: &str, old: &str, replaces: bool, proof: impl FnOnce(&[u8]) -> String)
                               -> (String, Vec<u8>, bool, Option<(String, String)>) {
        granted_with(unit, code, replaces.then_some(old), |message| {
            json!({"message": hex(message), "name": "laptop", "replaces": old, "proof": proof(message)}).to_string()
        }).await
    }

    /// granted, for the request body makes of the client's message,
    /// confirmed as a grant in place of the laptop of id replaces if any.
    async fn granted_with(unit: &Unit, code: &str, replaces: Option<&str>, body: impl FnOnce(&[u8]) -> String)
                          -> (String, Vec<u8>, bool, Option<(String, String)>) {
        let (client, message) = window::client(code);
        let (status, body) = post(unit.addr, "/update/session", &body(&message)).await;
        assert_eq!(status, 200, "{body}");
        let answer: serde_json::Value = serde_json::from_str(&body).unwrap();
        let key = client.finish(&unhex(answer["message"].as_str().unwrap()).unwrap()).unwrap();
        let grant = answer["grant"]["unit"].as_str().zip(answer["grant"]["laptop"].as_str())
            .map(|(unit, laptop)| (unit.to_string(), laptop.to_string()));
        let confirmation = match &grant {
            Some((unit, laptop)) => window::grant_confirmation(&key, unit, laptop, replaces),
            None => window::confirmation(&key),
        };
        (answer["session"].as_str().unwrap().to_string(), key, answer["confirm"] == confirmation, grant)
    }

    /// A set that replaces the installed binary with one of sha256 and size.
    fn binary_set(paths: &Paths, sha256: &str, size: usize) -> Bundle {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(paths.binary()).unwrap();
        let entry = Entry {
            path: bundle::NESTLOCAL.into(),
            kind: Kind::Write,
            owner: format!("{}:{}", meta.uid(), meta.gid()),
            mode: 0o755,
            old: Some(sha(&installed(paths))),
            new: Some(sha256.into()),
            size: size as u64,
        };
        Bundle { version: BUNDLE_VERSION, class: Class::Restart, force: false, entries: vec![entry] }
    }

    /// An apply as the app sends it in a session: its head and body, and
    /// what opens its answer.
    struct Apply {
        head: String,
        body: Vec<u8>,
        key: Vec<u8>,
        answer_aad: Vec<u8>,
    }

    impl Apply {
        /// set's header sealed in session, then parts as the contents'
        /// sealed chunks, the last flagged.
        fn new(session: &str, key: &[u8], set: &Bundle, parts: &[&[u8]]) -> Apply {
            let keys = seal::keys(key);
            let aad = aad("/update/apply", session);
            let header = seal::seal(&keys.to_unit, &aad, &serde_json::to_vec(set).unwrap());
            let answer_aad = [aad.as_slice(), seal::nonce_of(&header).unwrap()].concat();
            let (mut sealer, prefix) = seal::StreamSealer::new(&keys.to_unit, &answer_aad);
            let mut body = (header.len() as u32).to_be_bytes().to_vec();
            body.extend_from_slice(&header);
            body.extend_from_slice(&prefix);
            for (i, part) in parts.iter().enumerate() {
                let chunk = sealer.chunk(part, i + 1 == parts.len());
                body.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
                body.extend_from_slice(&chunk);
            }
            let head = format!("POST /update/apply HTTP/1.1\r\nContent-Length: {}\r\nX-Update-Session: {session}\r\n", body.len());
            Apply { head, body, key: key.to_vec(), answer_aad }
        }

        /// The body without its last part's chunk, which is part bytes long.
        fn without_last(&self, part: usize) -> &[u8] {
            &self.body[..self.body.len() - (4 + 1 + part + 16)]
        }

        /// The unit's answer, opened as the app opens it.
        fn applied(&self, answer: &[u8]) -> Applied {
            let opened = seal::open(&seal::keys(&self.key).to_client, &self.answer_aad, answer).expect("the answer opens");
            serde_json::from_slice(&opened).unwrap()
        }
    }

    /// An apply of a set that installs binary, through a key exchange with code.
    async fn apply_binary(unit: &Unit, code: &str, binary: &[u8]) -> Apply {
        let (session, key, _) = session(unit, code).await;
        Apply::new(&session, &key, &binary_set(&unit.paths, &sha(binary), binary.len()), &[binary])
    }

    fn screen(name: &str) -> String {
        std::fs::read_to_string(settings_ui::dir().join(name)).unwrap_or_default()
    }

    #[test]
    fn get_reports_the_window_and_the_binaries() {
        run(async {
            let unit = unit("get").await;
            let status = get(unit.addr).await;
            assert_eq!(status["open"], false);
            assert_eq!(status["installed"], sha(OLD));
            assert_eq!(status["pending"], false);
            assert_eq!(status["running"].as_str().map(str::len), Some(64));
            assert_eq!(status["rolled_back"], false);
            assert_eq!(status["rolled_back_by"], serde_json::Value::Null);
            // The guard records a set it put back (nestlocal-update-guard).
            std::fs::write(unit.paths.rolled_back(), "").unwrap();
            assert_eq!(get(unit.addr).await["rolled_back"], true);
            assert_eq!(get(unit.addr).await["rolled_back_by"], serde_json::Value::Null, "a set that did not come up");
            // A roll back asked for on the thermostat says so.
            std::fs::write(unit.paths.cancelled(), "thermostat\n").unwrap();
            assert_eq!(get(unit.addr).await["rolled_back_by"], "thermostat");
        });
    }

    #[test]
    fn idle_connections_are_dropped_and_do_not_lock_others_out() {
        run(async {
            let unit = unit("idle").await;
            let mut idle = Vec::new();
            for _ in 0..CONNECTIONS + 2 {
                idle.push(tokio::net::TcpStream::connect(unit.addr).await.unwrap());
            }
            let answered = tokio::time::timeout(Duration::from_secs(3), get(unit.addr)).await;
            assert!(answered.is_ok(), "a GET waited behind idle connections");
            let mut first = idle.remove(0);
            let mut out = Vec::new();
            let read = tokio::time::timeout(Duration::from_secs(3), first.read_to_end(&mut out)).await;
            assert!(read.is_ok(), "an idle connection was kept");
        });
    }

    #[test]
    fn an_apply_while_closed_is_forbidden() {
        run(async {
            let unit = unit("closed").await;
            let (status, body) = exchange(unit.addr, "POST /update/apply HTTP/1.1\r\nContent-Length: 17\r\n", b"").await;
            assert_eq!((status, body.as_str()), (403, "window closed\n"));
            assert!(!staged(&unit.paths).exists());
            assert_eq!(exchange(unit.addr, "PUT /update HTTP/1.1\r\nContent-Length: 0\r\n", b"").await.0, 405);
        });
    }

    #[test]
    fn an_apply_under_another_key_is_refused_and_does_not_spend_the_window() {
        run(async {
            let unit = unit("auth").await;
            unit.window.open(Instant::now());
            let apply = apply_binary(&unit, "00000000", NEW).await;
            let (status, body) = exchange(unit.addr, &apply.head, &apply.body).await;
            assert_eq!((status, body.as_str()), (401, "authentication failed\n"));
            assert!(!staged(&unit.paths).exists());
            assert_eq!(installed(&unit.paths), OLD);
            assert!(unit.window.is_open(Instant::now()) && !unit.window.spent());
        });
    }

    #[test]
    fn an_authenticated_apply_swaps_proves_and_restarts() {
        run(async {
            let unit = unit("swap").await;
            // What the last set's roll back left goes with the next swap.
            std::fs::write(unit.paths.rolled_back(), "").unwrap();
            std::fs::write(unit.paths.cancelled(), "thermostat\n").unwrap();
            let code = unit.window.open(Instant::now());
            let apply = apply_binary(&unit, &code, NEW).await;
            let (status, body) = exchange_bytes(unit.addr, &apply.head, &apply.body).await;
            assert_eq!(status, 202, "{}", String::from_utf8_lossy(&body));
            let applied = apply.applied(&body);
            assert_eq!(applied.files.len(), 1);
            let (path, owner_mode, sha256) = &applied.files[0];
            assert_eq!((path.as_str(), sha256.as_str()), (bundle::NESTLOCAL, sha(NEW).as_str()));
            assert!(owner_mode.ends_with(" 755"), "{owner_mode}");
            assert!(seal::open(&seal::keys(&apply.key).to_client, &aad("/update/apply", "x"), &body).is_err(),
                    "the answer is sealed to its session and request");
            assert_eq!(installed(&unit.paths), NEW);
            assert!(bundle::pending(&unit.paths));
            assert_eq!(std::fs::read_to_string(unit.paths.sender()).unwrap(), "unfr0zen", "a code's session");
            assert!(!unit.paths.rolled_back().exists() && !unit.paths.cancelled().exists());
            assert!(!unit.window.is_open(Instant::now()));
            assert_eq!(screen("update.status"), RESTARTING);
            assert_eq!(unit.restarts.load(Ordering::SeqCst), 0, "restarted before the answer went out");
            tokio::time::sleep(Duration::from_millis(1300)).await;
            assert_eq!(unit.restarts.load(Ordering::SeqCst), 1);
            let status = get(unit.addr).await;
            assert_eq!((status["installed"].as_str(), status["pending"].as_bool()), (Some(sha(NEW).as_str()), Some(true)));
        });
    }

    #[test]
    fn a_tampered_chunk_is_refused_and_nothing_swaps() {
        run(async {
            let unit = unit("tampered").await;
            let code = unit.window.open(Instant::now());
            let mut apply = apply_binary(&unit, &code, NEW).await;
            let last = apply.body.len() - 1;
            apply.body[last] ^= 1;
            let (status, body) = exchange(unit.addr, &apply.head, &apply.body).await;
            assert_eq!((status, body.as_str()), (400, format!("{TAMPERED}\n").as_str()));
            assert!(!staged(&unit.paths).exists() && !bundle::pending(&unit.paths));
            assert_eq!(installed(&unit.paths), OLD);
            assert_eq!(screen("update.status"), format!("Refused: {TAMPERED}"));
            assert_eq!(unit.restarts.load(Ordering::SeqCst), 0);

            // Contents that end before their last chunk were cut short.
            let code = unit.window.open(Instant::now());
            let (session, key, _) = session(&unit, &code).await;
            let apply = Apply::new(&session, &key, &binary_set(&unit.paths, &sha(NEW), NEW.len()), &[&NEW[..4], &NEW[4..]]);
            let cut = apply.without_last(NEW.len() - 4);
            let head = format!("POST /update/apply HTTP/1.1\r\nContent-Length: {}\r\nX-Update-Session: {session}\r\n", cut.len());
            assert_eq!(exchange(unit.addr, &head, cut).await, (400, format!("{TAMPERED}\n")));
            assert!(!staged(&unit.paths).exists());
            assert_eq!(installed(&unit.paths), OLD);
        });
    }

    #[test]
    fn an_apply_spends_the_window() {
        run(async {
            let unit = unit("spends").await;
            let code = unit.window.open(Instant::now());
            let (session, key, _) = session(&unit, &code).await;
            // A set the unit refuses spends it as well: the code took it.
            let mut set = binary_set(&unit.paths, &sha(NEW), NEW.len());
            set.entries[0].old = Some(sha(b"another binary"));
            let apply = Apply::new(&session, &key, &set, &[NEW]);
            let (status, body) = exchange(unit.addr, &apply.head, &apply.body).await;
            assert_eq!(status, 409, "{body}");
            assert!(body.contains("changed since it was read"), "{body}");
            assert!(!unit.window.is_open(Instant::now()) && unit.window.spent());
            assert_eq!(read(&unit, &session, &key, &["etc/x"], false).await, closed());
            let apply = Apply::new(&session, &key, &binary_set(&unit.paths, &sha(NEW), NEW.len()), &[NEW]);
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await.0, 403);
            assert_eq!(post(unit.addr, "/update/session", &exchange_body(&window::client(&code).1, "laptop")).await.0, 403);
            assert_eq!(installed(&unit.paths), OLD);
        });
    }

    #[test]
    fn a_refusal_reaches_a_client_that_sends_the_whole_body_first() {
        run(async {
            let unit = unit("drain").await;
            let big = vec![0x5a; 6 << 20];
            let parts: Vec<&[u8]> = big.chunks(seal::CHUNK).collect();
            // Refused once the set opens: it was read from another binary.
            let code = unit.window.open(Instant::now());
            let (changed, key, _) = session(&unit, &code).await;
            let mut set = binary_set(&unit.paths, &sha(&big), big.len());
            set.entries[0].old = Some(sha(b"another binary"));
            let apply = Apply::new(&changed, &key, &set, &parts);
            let (status, body) = exchange(unit.addr, &apply.head, &apply.body).await;
            assert_eq!(status, 409, "{body}");
            assert!(body.contains("changed since it was read"), "{body}");
            // Refused before the set opens: sealed under another key.
            let code = unit.window.open(Instant::now());
            let (forged, _, _) = session(&unit, &code).await;
            let apply = Apply::new(&forged, &[0; 32], &binary_set(&unit.paths, &sha(&big), big.len()), &parts);
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await, (401, "authentication failed\n".into()));
            assert_eq!(installed(&unit.paths), OLD);
        });
    }

    /// A request and its answer sealed once under keys((0..32)) in session
    /// SESSION; unfr0zen-host's session.rs opens the same pair.
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
    fn a_failed_upload_spends_the_code() {
        run(async {
            let unit = unit("mismatch").await;
            let now = Instant::now();
            let code = unit.window.open(now);
            settings_ui::publish(CODE, format!("{code}\n127.0.0.1\n").as_bytes()).unwrap();
            let (session, key, _) = session(&unit, &code).await;
            let apply = Apply::new(&session, &key, &binary_set(&unit.paths, &sha(OLD), NEW.len()), &[NEW]);
            let (status, _) = exchange(unit.addr, &apply.head, &apply.body).await;
            assert_eq!(status, 400);
            assert!(!unit.window.is_open(Instant::now()));
            assert!(!settings_ui::dir().join(CODE).exists());
            assert!(screen("update.status").starts_with("Refused: "));
            assert!(!staged(&unit.paths).exists() && !bundle::pending(&unit.paths));
            assert_eq!(installed(&unit.paths), OLD);
            // The screen keeps the reason, not Closed.
            SHOWN.store(true, Ordering::SeqCst);
            poll(&unit.shared(), "lo", now);
            assert!(screen("update.status").starts_with("Refused: "));
            // The same code takes no other upload: the window needs opening again.
            assert_eq!(get(unit.addr).await["open"], false);
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await.0, 403);
            assert_eq!(post(unit.addr, "/update/session", &exchange_body(&window::client(&code).1, "laptop")).await.0, 403);
            let code = unit.window.open(Instant::now());
            let apply = apply_binary(&unit, &code, NEW).await;
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await.0, 202);
        });
    }

    #[test]
    fn a_replaced_session_is_refused_and_the_window_stays_open() {
        run(async {
            let unit = unit("stale").await;
            let code = unit.window.open(Instant::now());
            settings_ui::publish(CODE, format!("{code}\n127.0.0.1\n").as_bytes()).unwrap();
            settings_ui::status(PROTOCOL, OPEN);
            let apply = apply_binary(&unit, &code, NEW).await;
            // Someone else's exchange replaces the session before the apply.
            session(&unit, "00000000").await;
            let (status, body) = exchange(unit.addr, &apply.head, &apply.body).await;
            assert_eq!((status, body.as_str()), (412, format!("{STALE}\n").as_str()));
            assert!(unit.window.is_open(Instant::now()));
            assert!(settings_ui::dir().join(CODE).exists());
            assert_eq!(screen("update.status"), OPEN);
            assert_eq!(installed(&unit.paths), OLD);
            // A new exchange with the code goes through.
            let apply = apply_binary(&unit, &code, NEW).await;
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await.0, 202);
        });
    }

    #[test]
    fn a_session_lasts_until_the_window_closes() {
        run(async {
            let unit = unit("lasts").await;
            for ago in [59, 590] {
                let code = unit.window.open(Instant::now().checked_sub(Duration::from_secs(ago)).unwrap());
                let (session, key, _) = session(&unit, &code).await;
                assert_eq!(info(&unit, &session, &key).await.0, 200, "{ago} s into the window");
                assert_eq!(read(&unit, &session, &key, &["etc/x"], false).await.0, 200, "{ago} s into the window");
            }
            let closes = Instant::now().checked_sub(window::OPEN_FOR - Duration::from_millis(800)).unwrap();
            let code = unit.window.open(closes);
            let (session, key, _) = session(&unit, &code).await;
            assert_eq!(read(&unit, &session, &key, &["etc/x"], false).await.0, 200);
            tokio::time::sleep(Duration::from_millis(1000)).await;
            assert_eq!(read(&unit, &session, &key, &["etc/x"], false).await, closed());
            assert_eq!(info(&unit, &session, &key).await, closed());
        });
    }

    #[test]
    fn info_and_reads_are_sealed_and_answer_in_order() {
        run(async {
            use std::os::unix::fs::MetadataExt;
            let unit = unit("sealed").await;
            let root = &unit.paths.root;
            std::fs::create_dir_all(root.join("etc/nestlocal")).unwrap();
            std::fs::create_dir_all(root.join("bin")).unwrap();
            std::fs::write(root.join("etc/nestlocal/options"), "-mqtt x\n").unwrap();
            std::fs::set_permissions(root.join("etc/nestlocal/options"), std::fs::Permissions::from_mode(0o640)).unwrap();
            std::fs::write(root.join("bin/busybox"), OLD).unwrap();
            std::fs::set_permissions(root.join("bin/busybox"), std::fs::Permissions::from_mode(0o755)).unwrap();
            let meta = std::fs::metadata(root.join("bin/busybox")).unwrap();
            let owner = format!("{}:{}", meta.uid(), meta.gid());
            let dir_mode = std::fs::metadata(root.join("etc/nestlocal")).unwrap().mode() & 0o7777;
            let code = unit.window.open(Instant::now());
            let (session, key, _) = session(&unit, &code).await;

            let (status, body) = info(&unit, &session, &key).await;
            assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
            let info: Info = serde_json::from_slice(&body).unwrap();
            assert_eq!(info.version, identity().0);
            assert_eq!((info.overlay, info.pending), (None, false));
            assert_eq!(info.shell_binary, format!("{} 755", meta.uid()));
            assert!(info.free_bytes > 0);

            let names = ["etc/nestlocal/options", "etc/absent", "etc/nestlocal", "bin/busybox"];
            let (status, body) = read(&unit, &session, &key, &names, true).await;
            assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
            let found: Vec<Found> = serde_json::from_slice(&body).unwrap();
            let options = b"-mqtt x\n".to_vec();
            assert_eq!(found, vec![
                Found::File { owner: owner.clone(), mode: 0o640, sha256: sha(&options), size: 8, data: Some(options) },
                Found::Absent,
                Found::Directory { owner_mode: format!("{owner} {:o}", dir_mode) },
                Found::File { owner: owner.clone(), mode: 0o755, sha256: sha(OLD), size: OLD.len() as u64, data: Some(OLD.to_vec()) },
            ]);
            let (_, body) = read(&unit, &session, &key, &["bin/busybox"], false).await;
            let found: Vec<Found> = serde_json::from_slice(&body).unwrap();
            assert_eq!(found, vec![Found::File { owner, mode: 0o755, sha256: sha(OLD), size: OLD.len() as u64, data: None }]);

            // The answer is sealed to its request: not under the request's AAD alone.
            let keys = update_wire::seal::keys(&key);
            let request = update_wire::seal::seal(&keys.to_unit, &aad("/update/info", &session), b"{}");
            let head = format!("POST /update/info HTTP/1.1\r\nContent-Length: {}\r\nX-Update-Session: {session}\r\n", request.len());
            let (status, body) = exchange_bytes(unit.addr, &head, &request).await;
            assert_eq!(status, 200);
            assert!(update_wire::seal::open(&keys.to_client, &aad("/update/info", &session), &body).is_err());
            assert!(!body.windows(b"version".len()).any(|w| w == b"version"), "the answer is not in clear");
            assert!(unit.window.is_open(Instant::now()) && !unit.window.spent(), "reads leave the window");
        });
    }

    #[test]
    fn a_check_names_what_an_apply_would_refuse_and_leaves_the_session() {
        run(async {
            let unit = unit("check").await;
            let code = unit.window.open(Instant::now());
            let (session, key, _) = session(&unit, &code).await;
            let paths = [bundle::NESTLOCAL, "boot/x", bundle::SCRATCH_NESTLOCAL, "etc/nestlocal/options"];
            let request = CheckRequest { paths: paths.iter().map(|path| path.to_string()).collect() };
            let (status, body) = sealed(&unit, "/update/check", &session, &key, &serde_json::to_vec(&request).unwrap()).await;
            assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
            let checked: update_wire::read::Checked = serde_json::from_slice(&body).unwrap();
            let refused: Vec<(&str, &str)> = checked.refused.iter().map(|r| (r.path.as_str(), r.why.as_str())).collect();
            // The test unit has no scratch volume mounted.
            assert_eq!(refused, [("boot/x", "is outside what an update may change"),
                                 (bundle::SCRATCH_NESTLOCAL, "no scratch volume is mounted at /media/scratch")]);
            let (status, body) = sealed(&unit, "/update/check", &session, &key, b"{}").await;
            assert_eq!(status, 400, "{}", String::from_utf8_lossy(&body));
            assert!(unit.window.is_open(Instant::now()) && !unit.window.spent(), "a check leaves the window");
            assert!(!unit.paths.staging.exists() && !unit.paths.journal.exists(), "a check stages nothing");
            assert_eq!(read(&unit, &session, &key, &["etc/x"], false).await.0, 200);
        });
    }

    #[test]
    fn a_bad_seal_is_refused_and_does_not_spend_the_window() {
        run(async {
            let unit = unit("badseal").await;
            let code = unit.window.open(Instant::now());
            let (session, key, _) = session(&unit, &code).await;
            assert_eq!(read(&unit, &session, &[0; 32], &["etc/x"], false).await.0, 401, "another key");
            // A request sealed for one path, sent to another.
            let keys = update_wire::seal::keys(&key);
            let request = update_wire::seal::seal(&keys.to_unit, &aad("/update/info", &session), b"{}");
            let head = format!("POST /update/read HTTP/1.1\r\nContent-Length: {}\r\nX-Update-Session: {session}\r\n", request.len());
            assert_eq!(exchange_bytes(unit.addr, &head, &request).await.0, 401);
            let empty = format!("POST /update/read HTTP/1.1\r\nContent-Length: 0\r\nX-Update-Session: {session}\r\n");
            assert_eq!(exchange_bytes(unit.addr, &empty, b"").await.0, 401, "nothing sealed");
            assert!(unit.window.is_open(Instant::now()) && !unit.window.spent());
            assert_eq!(read(&unit, &session, &key, &["etc/x"], false).await.0, 200);
        });
    }

    #[test]
    fn a_replaced_session_can_no_longer_read_or_apply() {
        run(async {
            let unit = unit("replaced").await;
            let code = unit.window.open(Instant::now());
            let (first, first_key, _) = session(&unit, &code).await;
            let (second, second_key, _) = session(&unit, &code).await;
            assert_eq!(read(&unit, &first, &first_key, &["etc/x"], false).await, stale());
            assert_eq!(info(&unit, &first, &first_key).await, stale());
            let apply = Apply::new(&first, &first_key, &binary_set(&unit.paths, &sha(NEW), NEW.len()), &[NEW]);
            let (status, body) = exchange(unit.addr, &apply.head, &apply.body).await;
            assert_eq!((status, body.as_str()), (412, format!("{STALE}\n").as_str()));
            assert_eq!(installed(&unit.paths), OLD);
            assert!(unit.window.is_open(Instant::now()) && !unit.window.spent());
            assert_eq!(read(&unit, &second, &second_key, &["etc/x"], false).await.0, 200);
        });
    }

    #[test]
    fn a_wrong_code_is_not_confirmed_and_counts_toward_closing_the_window() {
        run(async {
            let unit = unit("wrong").await;
            let code = unit.window.open(Instant::now());
            let wrong = format!("{:08}", (code.parse::<u32>().unwrap() + 1) % 100_000_000);
            for _ in 0..window::MAX_ATTEMPTS {
                assert!(!session(&unit, &wrong).await.2);
            }
            let (status, _) = post(unit.addr, "/update/session", &exchange_body(&window::client(&code).1, "laptop")).await;
            assert_eq!(status, 403, "the window takes no more exchanges");
            assert_eq!(post(unit.addr, "/update/session", "zz").await.0, 400);
        });
    }

    #[test]
    fn an_allowed_window_s_code_shows_on_the_update_page_and_takes_one_apply() {
        run(async {
            let unit = unit_with("remote", handover).await;
            let code = allowed(&unit).await;
            let shown = screen(CODE);
            assert_eq!(shown.lines().next(), Some(code.as_str()));
            assert_eq!(screen("update.status"), OPEN);
            // Asking again keeps the code someone may be reading.
            assert_eq!(open(&unit, "laptop").await.0, 200);
            assert_eq!(screen(CODE), shown);
            let apply = apply_binary(&unit, &code, NEW).await;
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await.0, 202);
            assert_eq!(installed(&unit.paths), NEW);
            // Once installed, nothing opens until the restart.
            assert_eq!(open(&unit, "laptop").await.0, 503);
        });
    }
    #[test]
    fn a_second_apply_while_one_runs_is_refused() {
        run(async {
            let unit = unit("busy").await;
            let code = unit.window.open(Instant::now());
            let (session, key, _) = session(&unit, &code).await;
            let apply = Apply::new(&session, &key, &binary_set(&unit.paths, &sha(NEW), NEW.len()), &[&NEW[..4], &NEW[4..]]);
            let mut first = tokio::net::TcpStream::connect(unit.addr).await.unwrap();
            first.write_all(format!("{}Host: unit\r\n\r\n", apply.head).as_bytes()).await.unwrap();
            first.write_all(apply.without_last(NEW.len() - 4)).await.unwrap();
            for _ in 0..100 {
                if staged(&unit.paths).exists() { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(staged(&unit.paths).exists());
            assert!(sleep_blocked());
            let (status, _) = exchange(unit.addr, "POST /update/apply HTTP/1.1\r\nContent-Length: 17\r\n", b"").await;
            assert_eq!(status, 503);
            // The first, cut off, leaves the old binary and a free slot.
            drop(first);
            for _ in 0..100 {
                if !BUSY.load(Ordering::SeqCst) { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(!BUSY.load(Ordering::SeqCst));
            assert!(!staged(&unit.paths).exists());
            assert_eq!(installed(&unit.paths), OLD);
            assert!(!unit.window.is_open(Instant::now()));
        });
    }

    #[test]
    fn an_install_finishes_when_the_client_goes() {
        run(async {
            let unit = unit("gone").await;
            let code = unit.window.open(Instant::now());
            let slow = b"#!/bin/sh\nsleep 1\nexit 0\n";
            let apply = apply_binary(&unit, &code, slow).await;
            let mut tcp = tokio::net::TcpStream::connect(unit.addr).await.unwrap();
            tcp.write_all(format!("{}Host: unit\r\n\r\n", apply.head).as_bytes()).await.unwrap();
            tcp.write_all(&apply.body).await.unwrap();
            // Gone while its -h runs.
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(screen("update.status"), INSTALLING);
            drop(tcp);
            for _ in 0..50 {
                if unit.restarts.load(Ordering::SeqCst) > 0 { break; }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert_eq!(unit.restarts.load(Ordering::SeqCst), 1);
            assert_eq!(installed(&unit.paths), slow);
            assert!(bundle::pending(&unit.paths));
            assert!(!unit.window.is_open(Instant::now()));
            assert_eq!(screen("update.status"), RESTARTING);
        });
    }

    #[test]
    fn a_stalled_upload_is_cut_off_when_the_window_lapses() {
        run(async {
            let unit = unit("stall").await;
            let opened = Instant::now().checked_sub(window::OPEN_FOR - Duration::from_millis(1500)).unwrap();
            let code = unit.window.open(opened);
            let (session, key, _) = session(&unit, &code).await;
            let apply = Apply::new(&session, &key, &binary_set(&unit.paths, &sha(NEW), NEW.len()), &[&NEW[..4], &NEW[4..]]);
            let mut tcp = tokio::net::TcpStream::connect(unit.addr).await.unwrap();
            tcp.write_all(format!("{}Host: unit\r\n\r\n", apply.head).as_bytes()).await.unwrap();
            tcp.write_all(apply.without_last(NEW.len() - 4)).await.unwrap();
            // The client stays connected and sends nothing more.
            let mut out = Vec::new();
            let read = tokio::time::timeout(Duration::from_secs(5), tcp.read_to_end(&mut out)).await;
            assert!(read.is_ok(), "no answer to a stalled upload");
            let out = String::from_utf8_lossy(&out);
            assert!(out.starts_with("HTTP/1.1 403") && out.ends_with(&format!("{LAPSED}\n")), "{out}");
            assert_eq!(screen("update.status"), format!("Refused: {LAPSED}"));
            assert!(!BUSY.load(Ordering::SeqCst));
            assert!(!held(Instant::now()));
            assert!(!staged(&unit.paths).exists());
            assert_eq!(installed(&unit.paths), OLD);
            assert!(!bundle::pending(&unit.paths));
        });
    }

    #[test]
    fn sleep_is_blocked_while_the_window_is_open() {
        run(async {
            let shared = parts(Window::new());
            let (window, display) = (shared.window.clone(), shared.display.clone());
            register(&shared);
            assert!(!sleep_blocked());
            let code = window.open(Instant::now());
            assert!(sleep_blocked());
            let (client, message) = window::client(&code);
            let answer = window.exchange(&message, "laptop", &window::Granting::Add, Instant::now()).unwrap();
            let key = client.finish(&unhex(&answer.message).unwrap()).unwrap();
            assert_eq!(window.session(&answer.session, Instant::now()), Some(key));
            assert!(window.spend(&answer.session, Instant::now()).is_some());
            assert!(!sleep_blocked());
            BUSY.store(true, Ordering::SeqCst);
            assert!(sleep_blocked());
            BUSY.store(false, Ordering::SeqCst);
            display.show(View::Code { code, ip: String::new() });
            assert!(sleep_blocked(), "while a view shows");
            display.hide();
            assert!(!sleep_blocked());
        });
    }

    #[test]
    fn a_connection_keeps_the_unit_awake_for_a_minute() {
        run(async {
            let unit = unit("awake").await;
            assert!(!sleep_blocked());
            let before = Instant::now();
            get(unit.addr).await;
            let after = Instant::now();
            assert!(sleep_blocked_at(after) && !held(after));
            assert!(sleep_blocked_at(before + AWAKE_FOR - Duration::from_millis(1)));
            assert!(!sleep_blocked_at(after + AWAKE_FOR), "a minute after the connection");
        });
    }

    #[test]
    fn a_later_connection_extends_the_hold_and_an_earlier_never_shortens_it() {
        run(async {
            let t = Instant::now();
            connected(t);
            connected(t + Duration::from_secs(30));
            assert!(sleep_blocked_at(t + Duration::from_secs(89)));
            assert!(!sleep_blocked_at(t + Duration::from_secs(90)));
            connected(t);
            assert!(sleep_blocked_at(t + Duration::from_secs(89)), "not shortened");
        });
    }

    /// A display whose views go nowhere.
    fn recording() -> Display {
        Display::recording().0
    }

    #[test]
    fn an_allowed_window_shows_its_code_full_screen_until_a_session_proves_it() {
        run(async {
            let unit = unit_with("fullscreen", handover).await;
            let code = allowed(&unit).await;
            assert_eq!(screen(CODE).lines().next(), Some(code.as_str()), "the Update page has it too");
            // A wrong code leaves it up for the window's other tries, and the
            // right one's exchange alone proves nothing.
            let wrong = format!("{:08}", (code.parse::<u32>().unwrap() + 1) % 100_000_000);
            assert!(!session(&unit, &wrong).await.2);
            let (session, key, confirmed) = session(&unit, &code).await;
            assert!(confirmed);
            assert!(unit.shown.try_recv().is_err());
            assert!(unit.display.showing());
            assert_eq!(info(&unit, &session, &key).await.0, 200);
            assert_eq!(unit.shown.try_recv(), Ok(None));
            assert!(!unit.display.showing());
            assert_eq!(read(&unit, &session, &key, &[], false).await.0, 200);
            assert!(unit.shown.try_recv().is_err(), "hidden once");
        });
    }

    #[test]
    fn an_apply_takes_the_full_screen_code_away() {
        run(async {
            let unit = unit_with("fullscreen-apply", handover).await;
            let code = allowed(&unit).await;
            let apply = apply_binary(&unit, &code, NEW).await;
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await.0, 202);
            assert_eq!(unit.shown.try_recv(), Ok(None));
        });
    }

    #[test]
    fn the_full_screen_code_goes_when_the_window_stops_accepting() {
        run(async {
            let unit = unit_with("fullscreen-lapse", handover).await;
            allowed(&unit).await;
            poll(&unit.shared(), "lo", Instant::now());
            assert!(unit.shown.try_recv().is_err(), "the window is open");
            poll(&unit.shared(), "lo", Instant::now() + window::OPEN_FOR);
            assert_eq!(unit.shown.try_recv(), Ok(None));
            assert_eq!(screen("update.status"), CLOSED);
        });
    }

    #[test]
    fn a_press_hides_the_full_screen_code_and_leaves_the_window_open() {
        run(async {
            let unit = unit_with("fullscreen-press", handover).await;
            let actions = unit.display.actions();
            let code = allowed(&unit).await;
            unit.display.press(Action::Hide);
            act(&unit.shared(), "lo", actions.try_recv().unwrap(), Instant::now());
            assert_eq!(unit.shown.try_recv(), Ok(None));
            assert!(unit.window.accepting(Instant::now()));
            assert_eq!(screen(CODE).lines().next(), Some(code.as_str()));
            assert!(session(&unit, &code).await.2);
        });
    }

    #[test]
    fn a_screen_request_opens_the_window_and_publishes_the_code() {
        run(async {
            let shared = parts(Window::new());
            let now = Instant::now();
            settings_ui::publish("update.request", b"").unwrap();
            poll(&shared, "lo", now);
            assert!(shared.window.is_open(now));
            let code = screen(CODE);
            let (digits, ip) = code.split_once('\n').unwrap();
            assert!(digits.len() == 8 && digits.bytes().all(|b| b.is_ascii_digit()), "{code:?}");
            assert!(ip.ends_with('\n'));
            assert_eq!(screen("update.status"), OPEN);
            assert!(!settings_ui::dir().join("update.request").exists());
            // It lapses: the screen is told and the code goes.
            poll(&shared, "lo", now + window::OPEN_FOR);
            assert_eq!(screen("update.status"), CLOSED);
            assert!(!settings_ui::dir().join(CODE).exists());
        });
    }

    #[test]
    fn a_window_used_up_before_the_poll_sees_it_leaves_the_screen() {
        run(async {
            let unit = unit_with("usedup", handover).await;
            allowed(&unit).await;
            assert!(settings_ui::dir().join(CODE).exists());
            for _ in 0..window::MAX_ATTEMPTS {
                post(unit.addr, "/update/session", &exchange_body(&[0], "laptop")).await;
            }
            poll(&unit.shared(), "lo", Instant::now());
            assert!(!settings_ui::dir().join(CODE).exists());
            assert_eq!(screen("update.status"), CLOSED);
        });
    }

    #[test]
    fn the_screen_cannot_replace_the_window_of_an_upload_under_way() {
        run(async {
            let shared = parts(Window::new());
            let (window, now) = (&shared.window, Instant::now());
            let code = window.open(now);
            BUSY.store(true, Ordering::SeqCst);
            settings_ui::publish("update.request", b"").unwrap();
            poll(&shared, "lo", now);
            assert!(!settings_ui::dir().join("update.request").exists(), "the request is taken");
            // The code stands: an exchange with it still works.
            let (client, message) = window::client(&code);
            let answer = window.exchange(&message, "laptop", &window::Granting::Add, now).unwrap();
            let key = client.finish(&unhex(&answer.message).unwrap()).unwrap();
            assert_eq!(answer.confirm, window::confirmation(&key));
        });
    }

    #[test]
    fn browsers_are_refused() {
        run(async {
            let unit = unit("origin").await;
            let head = "POST /update/open HTTP/1.1\r\nOrigin: http://example.com\r\nContent-Length: 0\r\n";
            assert_eq!(exchange(unit.addr, head, b"").await.0, 403);
            assert!(!unit.window.is_open(Instant::now()));
        });
    }

    #[test]
    fn a_window_with_no_exchange_left_is_reported_closed() {
        run(async {
            let unit = unit_with("noexchange", handover).await;
            let code = unit.window.open(Instant::now());
            let wrong = format!("{:08}", (code.parse::<u32>().unwrap() + 1) % 100_000_000);
            for _ in 0..window::MAX_ATTEMPTS {
                session(&unit, &wrong).await;
            }
            assert_eq!(get(unit.addr).await["open"], false);
            // The last exchange's session holds the window until it lapses;
            // then the network may ask for another, with a new code.
            assert_eq!(open(&unit, "laptop").await.0, 503);
            unit.window.open(Instant::now().checked_sub(window::OPEN_FOR).unwrap());
            allowed(&unit).await;
            assert_eq!(get(unit.addr).await["open"], true);
        });
    }

    #[test]
    fn an_update_on_trial_reports_pending_and_keeps_the_unit_awake_until_kept() {
        run(async {
            let unit = unit("trial").await;
            assert_eq!(get(unit.addr).await["pending"], false);
            std::fs::write(&unit.paths.pending, "1\n").unwrap();
            let backup = staged(&unit.paths).with_file_name("nestlocal.unfr0zen-previous");
            std::fs::write(&backup, OLD).unwrap();
            std::fs::create_dir_all(unit.paths.journal.parent().unwrap()).unwrap();
            std::fs::write(&unit.paths.journal, "unfr0zen-update 1\nclass restart\nstate swapped\n\
                                                 write nestlabs/sbin/nestlocal nestlabs/sbin/nestlocal.unfr0zen-previous\n").unwrap();
            let live = Liveness::new(Duration::from_secs(300));
            commit_when_alive(live.clone(), unit.paths.clone(), "lo".into(), None);
            // The guard takes the marker away on a rollback before the restart.
            std::fs::remove_file(&unit.paths.pending).unwrap();
            assert_eq!(get(unit.addr).await["pending"], true);
            assert!(sleep_blocked());
            std::fs::write(&unit.paths.pending, "1\n").unwrap();
            let _call = live.request();
            for _ in 0..30 {
                if !ON_TRIAL.load(Ordering::SeqCst) { break; }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert_eq!(get(unit.addr).await["pending"], false);
            assert!(!held(Instant::now()) && !backup.exists() && !unit.paths.journal.exists());
        });
    }

    #[test]
    fn a_trial_nestlocal_leaves_the_marker_of_an_update_it_installed() {
        run(async {
            let paths = paths("successor");
            std::fs::write(&paths.pending, "0\n").unwrap();
            let live = Liveness::new(Duration::from_secs(300));
            commit_when_alive(live.clone(), paths.clone(), "lo".into(), None);
            let _call = live.request();
            tokio::time::sleep(Duration::from_millis(1500)).await;
            assert!(bundle::pending(&paths), "a 0 marker is the next nestlocal's to keep");
        });
    }

    #[test]
    fn get_reports_the_versions() {
        run(async {
            let unit = unit("versions").await;
            let status = get(unit.addr).await;
            assert_eq!(status["version"], identity().0);
            assert!(status["overlay"].is_null(), "no overlay version installed");
            std::fs::write(&unit.paths.overlay, "1\n").unwrap();
            assert_eq!(get(unit.addr).await["overlay"], 1);
            assert_eq!(identity().1, include_str!("../../overlay-version").trim().parse().ok());
            assert!(IDENTITY.starts_with("\0unfr0zen-nestlocal version=") && IDENTITY.ends_with('\0'));
        });
    }

    #[test]
    fn an_update_s_restart_waits_for_a_chip_being_flashed() {
        run(async {
            let unit = unit("flashing").await;
            let code = unit.window.open(Instant::now());
            let flashing = flashing();
            let apply = apply_binary(&unit, &code, NEW).await;
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await.0, 202);
            tokio::time::sleep(Duration::from_millis(2500)).await;
            assert_eq!(unit.restarts.load(Ordering::SeqCst), 0, "restarted while flashing");
            drop(flashing);
            tokio::time::sleep(Duration::from_millis(1500)).await;
            assert_eq!(unit.restarts.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn commit_when_alive_clears_the_marker_once_nlclient_calls() {
        run(async {
            let paths = paths("commit");
            std::fs::write(&paths.pending, "1\n").unwrap();
            let live = Liveness::new(Duration::from_secs(300));
            commit_when_alive(live.clone(), paths.clone(), "lo".into(), None);
            std::thread::sleep(Duration::from_millis(1200));
            assert!(bundle::pending(&paths), "committed before nlclient called");
            let call = live.request();
            for _ in 0..30 {
                if !bundle::pending(&paths) { break; }
                std::thread::sleep(Duration::from_millis(100));
            }
            drop(call);
            assert!(!bundle::pending(&paths));
        });
    }

    /// A set on trial that reboots the unit, swapped in with its marker.
    fn reboot_trial(name: &str) -> Paths {
        let paths = paths(name);
        std::fs::write(&paths.pending, "1\n").unwrap();
        std::fs::create_dir_all(paths.journal.parent().unwrap()).unwrap();
        std::fs::write(&paths.journal, "unfr0zen-update 1\nclass reboot\nstate swapped\nwrite etc/monitrc -\n").unwrap();
        paths
    }

    /// Asserts a reboot-class set stays on trial while nlclient calls in and
    /// the update API listens, but iface's address is address's.
    async fn not_kept_with(name: &str, address: fn(&str) -> Option<std::net::Ipv4Addr>) {
        let _listening = unit(&format!("{name}-listener")).await;
        let paths = reboot_trial(name);
        let live = Liveness::new(Duration::from_secs(300));
        commit_when(live.clone(), paths.clone(), "wlan0".into(), Hooks { address, handover: no_handover, restart: Arc::new(|_| {}), nudge: None });
        let _call = live.request();
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(LISTENING.load(Ordering::SeqCst));
        assert!(paths.pending.exists() && paths.journal.exists(), "kept without an address off loopback");
        assert!(sleep_blocked());
    }

    #[test]
    fn a_reboot_class_set_is_not_kept_off_the_network() {
        run(not_kept_with("offnet", |_| None));
    }

    #[test]
    fn a_reboot_class_set_is_not_kept_on_loopback_alone() {
        run(not_kept_with("loopback", |_| Some(std::net::Ipv4Addr::LOCALHOST)));
    }

    #[test]
    fn a_reboot_class_set_is_kept_once_on_the_network() {
        run(async {
            let paths = reboot_trial("onnet");
            let live = Liveness::new(Duration::from_secs(300));
            let address = |_: &str| Some(std::net::Ipv4Addr::new(192, 168, 1, 20));
            commit_when(live.clone(), paths.clone(), "wlan0".into(), Hooks { address, handover: no_handover, restart: Arc::new(|_| {}), nudge: None });
            let _call = live.request();
            tokio::time::sleep(Duration::from_millis(2500)).await;
            assert!(paths.pending.exists(), "kept before the update listener was bound");
            let _listening = unit("onnet-listener").await;
            for _ in 0..30 {
                if !ON_TRIAL.load(Ordering::SeqCst) { break; }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(!bundle::pending(&paths) && !held(Instant::now()));
        });
    }

    /// Asks the unit to open its window as the app does, under name.
    async fn open(unit: &Unit, name: &str) -> (u16, String) {
        post(unit.addr, "/update/open", &json!({"name": name}).to_string()).await
    }

    /// The name and address of the prompt the display was last asked to show.
    fn prompt_shown(unit: &Unit) -> (String, String) {
        match unit.shown.try_recv() {
            Ok(Some(View::Prompt { name, ip, .. })) => (name, ip),
            other => panic!("no prompt shown: {other:?}"),
        }
    }

    /// Opens the window as the app does and allows it on the thermostat:
    /// the code shown.
    async fn allowed(unit: &Unit) -> String {
        assert_eq!(open(unit, "laptop").await.0, 202);
        prompt_shown(unit);
        act(&unit.shared(), "lo", Action::Allow, Instant::now());
        code_shown(unit)
    }

    #[test]
    fn open_asks_and_status_reports_the_answer() {
        run(async {
            let unit = unit_with("prompt", handover).await;
            assert_eq!(get(unit.addr).await["prompt"], serde_json::Value::Null);
            let (status, body) = open(&unit, "laptop").await;
            assert_eq!(status, 202, "{body}");
            let body: serde_json::Value = serde_json::from_str(&body).unwrap();
            let id = body["id"].as_str().unwrap().to_string();
            assert_eq!(body, json!({"prompt": "asking", "id": id}));
            assert_eq!(prompt_shown(&unit), ("laptop".into(), "127.0.0.1".into()));
            assert_eq!(get(unit.addr).await["prompt"], json!({"id": id, "state": "asking"}));
            // A second laptop asking meanwhile is busy, and the first stands.
            let (status, body) = open(&unit, "other").await;
            assert_eq!(status, 503, "{body}");
            assert!(unit.shown.try_recv().is_err());
            act(&unit.shared(), "lo", Action::Allow, Instant::now());
            let code = code_shown(&unit);
            let status = get(unit.addr).await;
            assert_eq!((&status["prompt"], &status["open"]), (&json!({"id": id, "state": "allowed"}), &json!(true)));
            assert!(session(&unit, &code).await.2);

            let unit = unit_with("prompt-deny", handover).await;
            let (_, body) = open(&unit, "b").await;
            let b = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"].clone();
            prompt_shown(&unit);
            act(&unit.shared(), "lo", Action::Deny, Instant::now());
            assert_eq!(unit.shown.try_recv(), Ok(None));
            let status = get(unit.addr).await;
            assert_eq!((&status["prompt"], &status["open"]), (&json!({"id": b, "state": "denied"}), &json!(false)));
            // Laptop c asks next: b, reading the status, finds c's prompt.
            let (_, body) = open(&unit, "c").await;
            let c = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"].clone();
            assert_ne!(c, b);
            assert_eq!(get(unit.addr).await["prompt"], json!({"id": c, "state": "asking"}));
            prompt_shown(&unit);
            poll(&unit.shared(), "lo", Instant::now() + prompt::ASK_FOR);
            assert_eq!(unit.shown.try_recv(), Ok(None));
            assert_eq!(get(unit.addr).await["prompt"]["state"], "timed_out");
            // The third strike: the network asks no more.
            assert_eq!(open(&unit, "laptop").await.0, 202);
            act(&unit.shared(), "lo", Action::Deny, Instant::now());
            let (status, body) = open(&unit, "laptop").await;
            assert_eq!((status, body.as_str()), (429, "open Settings → UPDATE on the thermostat and press Allow update\n"));
        });
    }

    #[test]
    fn an_open_while_the_window_accepts_does_not_prompt() {
        run(async {
            let unit = unit_with("prompt-open", handover).await;
            unit.window.open(Instant::now());
            let (status, body) = open(&unit, "laptop").await;
            assert_eq!((status, body.as_str()), (200, r#"{"open":true}"#));
            assert!(unit.shown.try_recv().is_err());
            assert_eq!(get(unit.addr).await["prompt"], serde_json::Value::Null);
            assert_eq!(exchange(unit.addr, "GET /update/open HTTP/1.1\r\n", b"").await.0, 405);
        });
    }

    #[test]
    fn a_long_or_unprintable_name_is_trimmed() {
        run(async {
            let unit = unit_with("prompt-name", handover).await;
            for (name, shown) in [(format!("\u{7}  {}é\n", "a".repeat(40)), "a".repeat(32)),
                                  (" \u{1b} ".into(), "unfr0zen".into()),
                                  (String::new(), "unfr0zen".into())] {
                assert_eq!(open(&unit, &name).await.0, 202);
                assert_eq!(prompt_shown(&unit).0, shown, "{name:?}");
                act(&unit.shared(), "lo", Action::Deny, Instant::now());
                assert_eq!(unit.shown.try_recv(), Ok(None));
            }
            let (status, body) = post(unit.addr, "/update/open", "laptop").await;
            assert_eq!(status, 400, "{body}");
        });
    }

    #[test]
    fn an_unavailable_screen_refuses_to_ask() {
        run(async {
            let unit = unit("prompt-nohandover").await;
            let (status, body) = open(&unit, "laptop").await;
            assert_eq!((status, body.as_str()), (409, "open Settings → UPDATE on the thermostat and press Allow update\n"));
            fn in_use(_: &std::path::Path) -> Result<(), handover::Unavailable> {
                Err(handover::Unavailable::InUse)
            }
            let unit = unit_with("prompt-inuse", in_use).await;
            let (status, body) = open(&unit, "laptop").await;
            assert_eq!((status, body.as_str()), (409, "the thermostat's screen is in use\n"));
            assert!(unit.shown.try_recv().is_err());
            assert!(!unit.window.is_open(Instant::now()));
            assert_eq!(get(unit.addr).await["prompt"], serde_json::Value::Null);
        });
    }

    /// Opens the window as the app does and allows it with trust on the
    /// thermostat: the code shown.
    async fn allowed_with_trust(unit: &Unit) -> String {
        assert_eq!(open(unit, "laptop").await.0, 202);
        prompt_shown(unit);
        act(&unit.shared(), "lo", Action::AllowTrust, Instant::now());
        code_shown(unit)
    }

    fn trusted(unit: &Unit) -> std::sync::MutexGuard<'_, trust::Store> {
        unit.trusted.lock().unwrap()
    }

    #[test]
    fn allow_and_trust_grants_once_the_session_authenticates() {
        run(async {
            let unit = unit_with("trust", handover).await;
            let code = allowed_with_trust(&unit).await;
            let unit_id = trusted(&unit).unit_id().to_string();
            let (session, key, confirmed, grant) = granted(&unit, &code, "my laptop").await;
            assert!(confirmed, "the grant's confirmation verifies under the session key");
            let (granted_unit, laptop) = grant.expect("a window allowed with trust grants");
            assert_eq!(granted_unit, unit_id);
            assert!(laptop.len() == 32 && laptop.bytes().all(|b| b.is_ascii_hexdigit()), "{laptop}");
            // The exchange alone trusts nothing.
            assert!(trusted(&unit).list().is_empty());
            assert_eq!(get(unit.addr).await["unit"], serde_json::Value::Null);
            assert_eq!(info(&unit, &session, &key).await.0, 200);
            {
                let store = trusted(&unit);
                let [kept] = store.list() else { panic!("one laptop trusted") };
                assert_eq!((kept.id.as_str(), kept.name.as_str()), (laptop.as_str(), "laptop"), "the prompt's name");
                assert_eq!(kept.key, update_wire::auth::trust_key(&key, &unit_id, &laptop));
                assert!(chrono::DateTime::parse_from_rfc3339(&kept.added).is_ok(), "{}", kept.added);
                assert_eq!(store.listed(), format!("{laptop} laptop\n"));
            }
            assert_eq!(get(unit.addr).await["unit"], unit_id);
            // Committed once: later requests and exchanges add none.
            assert_eq!(read(&unit, &session, &key, &[], false).await.0, 200);
            let (_, _, confirmed, grant) = granted(&unit, &code, "my laptop").await;
            assert!(confirmed && grant.is_none(), "the window's trust is spent");
            assert_eq!(trusted(&unit).list().len(), 1);
        });
    }

    #[test]
    fn a_wrong_code_never_adds_a_laptop() {
        run(async {
            let unit = unit_with("trust-wrong", handover).await;
            let code = allowed_with_trust(&unit).await;
            let wrong = format!("{:08}", (code.parse::<u32>().unwrap() + 1) % 100_000_000);
            let (session, key, confirmed, grant) = granted(&unit, &wrong, "laptop").await;
            assert!(!confirmed && grant.is_some());
            assert_eq!(info(&unit, &session, &key).await.0, 401);
            let apply = Apply::new(&session, &key, &binary_set(&unit.paths, &sha(NEW), NEW.len()), &[NEW]);
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await.0, 401);
            assert!(trusted(&unit).list().is_empty());
            assert_eq!(trusted(&unit).listed(), "");
            // The right code's session, once it proves itself, is trusted.
            let (session, key, confirmed, grant) = granted(&unit, &code, "laptop").await;
            assert!(confirmed);
            let apply = Apply::new(&session, &key, &binary_set(&unit.paths, &sha(NEW), NEW.len()), &[NEW]);
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await.0, 202);
            let store = trusted(&unit);
            assert_eq!(store.list().iter().map(|l| l.id.clone()).collect::<Vec<_>>(), [grant.unwrap().1]);
        });
    }

    #[test]
    fn a_full_list_offers_no_trust() {
        run(async {
            let unit = unit_with("trust-full", handover).await;
            for n in 0..trust::LIMIT as u8 {
                let laptop = trust::Laptop { id: hex(&[n; 16]), name: format!("l{n}"), key: [n; 32], added: String::new() };
                trusted(&unit).add(laptop).unwrap();
            }
            assert_eq!(open(&unit, "laptop").await.0, 202);
            let view = match unit.shown.try_recv() {
                Ok(Some(view @ View::Prompt { trust: false, .. })) => view,
                other => panic!("no prompt without trust: {other:?}"),
            };
            assert!(view.lines().iter().any(|(text, _)| text == "trusted list full"));
            assert_eq!(view.choices().iter().map(|(label, _)| *label).collect::<Vec<_>>(), ["Allow", "Deny"]);
            // An Allow and trust that reaches it anyway is an Allow.
            act(&unit.shared(), "lo", Action::AllowTrust, Instant::now());
            let code = code_shown(&unit);
            let (session, key, confirmed, grant) = granted(&unit, &code, "laptop").await;
            assert!(confirmed && grant.is_none());
            assert_eq!(info(&unit, &session, &key).await.0, 200);
            assert_eq!(trusted(&unit).list().len(), trust::LIMIT, "no fifth");
            assert_eq!(get(unit.addr).await["unit"], trusted(&unit).unit_id());
        });
    }

    /// The ids of the laptops the unit trusts.
    fn trusted_ids(unit: &Unit) -> Vec<String> {
        trusted(unit).list().iter().map(|laptop| laptop.id.clone()).collect()
    }

    /// A trusted list filled with laptops: the first's id and trust key.
    fn filled(unit: &Unit) -> (String, [u8; 32]) {
        let first = trusting(unit, "old");
        while !trusted(unit).full() {
            trusting(unit, "other");
        }
        first
    }

    /// open, from a laptop that says it was trusted as laptop.
    async fn open_as(unit: &Unit, laptop: &str) -> (u16, String) {
        post(unit.addr, "/update/open", &json!({"name": "laptop", "laptop": laptop}).to_string()).await
    }

    #[test]
    fn a_full_list_offers_trust_to_a_laptop_in_it() {
        run(async {
            let unit = unit_with("trust-full-named", handover).await;
            let (old, _) = filled(&unit);
            assert_eq!(open_as(&unit, &hex(&[9; 16])).await.0, 202);
            assert!(matches!(unit.shown.try_recv(), Ok(Some(View::Prompt { trust: false, .. }))), "a laptop not in it");
            act(&unit.shared(), "lo", Action::Deny, Instant::now());
            unit.shown.try_recv().unwrap();
            assert_eq!(open_as(&unit, &old).await.0, 202);
            assert!(matches!(unit.shown.try_recv(), Ok(Some(View::Prompt { trust: true, .. }))));
            act(&unit.shared(), "lo", Action::AllowTrust, Instant::now());
            code_shown(&unit);
            assert!(unit.window.accepting(Instant::now()));
        });
    }

    #[test]
    fn a_proven_exchange_replaces_its_laptop_in_a_full_list() {
        run(async {
            let unit = unit_with("trust-replace", handover).await;
            let (old, key) = filled(&unit);
            let unit_id = trusted(&unit).unit_id().to_string();
            assert_eq!(open_as(&unit, &old).await.0, 202);
            prompt_shown(&unit);
            act(&unit.shared(), "lo", Action::AllowTrust, Instant::now());
            let code = code_shown(&unit);
            let (session, session_key, confirmed, grant) =
                granted_replacing(&unit, &code, &old, true, |message| update_wire::auth::replace_proof(&key, &unit_id, &old, message)).await;
            let (_, laptop) = grant.expect("a grant in a full list");
            assert!(confirmed && laptop != old, "a fresh laptop id");
            assert!(trusted_ids(&unit).contains(&old), "the exchange alone replaces nothing");
            assert_eq!(info(&unit, &session, &session_key).await.0, 200);
            let ids = trusted_ids(&unit);
            assert_eq!(ids.len(), trust::LIMIT);
            assert!(!ids.contains(&old) && ids.contains(&laptop), "{ids:?}");
            assert_eq!(trusted(&unit).find(&laptop).unwrap().key, update_wire::auth::trust_key(&session_key, &unit_id, &laptop));
            let listed = trusted(&unit).listed();
            assert!(!listed.contains(&old) && listed.contains(&laptop), "{listed}");
        });
    }

    #[test]
    fn a_proof_that_fails_adds_a_laptop_instead() {
        run(async {
            let unit = unit_with("trust-replace-bad", handover).await;
            let (old, key) = trusting(&unit, "old");
            let unit_id = trusted(&unit).unit_id().to_string();
            let unknown = hex(&[9; 16]);
            type Proof<'a> = Box<dyn FnOnce(&[u8]) -> String + 'a>;
            let proofs: [(&str, Proof); 3] = [
                (&old, Box::new(|message| update_wire::auth::replace_proof(&[1; 32], &unit_id, &old, message))),
                (&old, Box::new(|_| update_wire::auth::replace_proof(&key, &unit_id, &old, &window::client("00000000").1))),
                (&unknown, Box::new(|message| update_wire::auth::replace_proof(&key, &unit_id, &unknown, message))),
            ];
            for (n, (replaces, proof)) in proofs.into_iter().enumerate() {
                let code = unit.window.open_trusting(unit_id.clone(), None, Instant::now());
                let (session, session_key, confirmed, grant) = granted_replacing(&unit, &code, replaces, false, proof).await;
                assert!(confirmed, "proof {n}: confirmed as an addition");
                assert_eq!(info(&unit, &session, &session_key).await.0, 200);
                let ids = trusted_ids(&unit);
                assert_eq!(ids.len(), n + 2, "proof {n}: added");
                assert!(ids.contains(&old) && ids.contains(&grant.unwrap().1), "proof {n}: {ids:?}");
            }
            // In a full list, a proof that fails gets no grant.
            assert!(trusted(&unit).full());
            let code = unit.window.open_trusting(unit_id.clone(), None, Instant::now());
            let (session, session_key, confirmed, grant) =
                granted_replacing(&unit, &code, &old, false, |message| update_wire::auth::replace_proof(&[1; 32], &unit_id, &old, message)).await;
            assert!(confirmed && grant.is_none());
            assert_eq!(info(&unit, &session, &session_key).await.0, 200);
            assert!(trusted_ids(&unit).contains(&old));
        });
    }

    #[test]
    fn a_grant_the_unit_fails_to_keep_refuses_its_request_and_ends_the_session() {
        run(async {
            let unit = unit_with("trust-not-kept", handover).await;
            let (old, key) = filled(&unit);
            let unit_id = trusted(&unit).unit_id().to_string();
            let code = unit.window.open_trusting(unit_id.clone(), None, Instant::now());
            let (session, session_key, confirmed, grant) =
                granted_replacing(&unit, &code, &old, true, |message| update_wire::auth::replace_proof(&key, &unit_id, &old, message)).await;
            assert!(confirmed && grant.is_some());
            let before = trusted_ids(&unit);
            trusted(&unit).fail_writes();
            let (status, body) = info(&unit, &session, &session_key).await;
            assert_eq!(status, 500);
            assert!(String::from_utf8_lossy(&body).starts_with(NOT_KEPT), "{}", String::from_utf8_lossy(&body));
            assert_eq!(trusted_ids(&unit), before, "the old laptop stays");
            assert!(!unit.window.is_open(Instant::now()), "the session and its window end");
            assert_eq!(info(&unit, &session, &session_key).await, closed());
        });
    }

    #[test]
    fn a_laptop_id_is_logged_by_its_first_8_digits_alone() {
        assert_eq!(short_id(&"ab".repeat(16)), "abababab");
        assert_eq!(short_id("é"), "(not an id)");
        assert_eq!(short_id("abc"), "(not an id)");
    }

    /// A laptop the unit trusts, named name: its id and trust key.
    fn trusting(unit: &Unit, name: &str) -> (String, [u8; 32]) {
        let (id, key) = (hex(&random::<16>()), random::<32>());
        trusted(unit).add(trust::Laptop { id: id.clone(), name: name.into(), key, added: String::new() }).unwrap();
        (id, key)
    }

    /// A trusted exchange as the app makes it, for laptop under key: the
    /// session, its key, and whether the unit confirmed the key; or the
    /// status and body of a refusal.
    async fn trusted_session(unit: &Unit, laptop: &str, key: &[u8; 32]) -> Result<(String, Vec<u8>, bool), (u16, String)> {
        let (client, message) = window::trusted_client(key);
        let body = json!({"message": hex(&message), "name": "ignored", "laptop": laptop}).to_string();
        let (status, body) = post(unit.addr, "/update/session", &body).await;
        if status != 200 {
            return Err((status, body));
        }
        let answer: serde_json::Value = serde_json::from_str(&body).unwrap();
        let session_key = client.finish(&unhex(answer["message"].as_str().unwrap()).unwrap()).unwrap();
        assert!(answer.get("grant").is_none(), "a trusted session grants nothing");
        let confirmed = answer["confirm"] == window::confirmation(&session_key);
        Ok((answer["session"].as_str().unwrap().to_string(), session_key, confirmed))
    }

    /// Waits out the limit on trusted exchanges from one address.
    async fn paced() {
        tokio::time::sleep(EXCHANGE_EVERY).await;
    }

    fn notice(name: &str, stage: Stage) -> Option<View> {
        Some(View::Notice { name: name.into(), stage })
    }

    #[test]
    fn a_trusted_laptop_reads_and_applies_without_a_code() {
        run(async {
            let unit = unit_with("trusted", handover).await;
            let (laptop, key) = trusting(&unit, "desk");
            assert_eq!(get(unit.addr).await["unit"], trusted(&unit).unit_id());
            // A prompt asking makes it busy.
            assert_eq!(open(&unit, "other").await.0, 202);
            prompt_shown(&unit);
            let (status, body) = trusted_session(&unit, &laptop, &key).await.unwrap_err();
            assert_eq!((status, body.as_str()), (503, "busy: another update holds the thermostat\n"));
            act(&unit.shared(), "lo", Action::Deny, Instant::now());
            assert_eq!(unit.shown.try_recv(), Ok(None));
            paced().await;
            // Another key is not confirmed, and its requests fail.
            let (session, session_key, confirmed) = trusted_session(&unit, &laptop, &[0; 32]).await.unwrap();
            assert!(!confirmed);
            assert_eq!(info(&unit, &session, &session_key).await.0, 401);
            paced().await;
            let (session, session_key, confirmed) = trusted_session(&unit, &laptop, &key).await.unwrap();
            assert!(confirmed);
            assert_eq!(get(unit.addr).await["open"], false, "no window opens");
            assert_eq!(info(&unit, &session, &session_key).await.0, 200);
            assert_eq!(read(&unit, &session, &session_key, &["etc/x"], false).await.0, 200);
            assert!(sleep_blocked());
            // Another laptop asking meanwhile is busy, and the session stands.
            assert_eq!(open(&unit, "other").await.0, 503);
            assert_eq!(read(&unit, &session, &session_key, &[], false).await.0, 200);
            // Send takes a session of its own, which the set spends.
            paced().await;
            let (session, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            let apply = Apply::new(&session, &session_key, &binary_set(&unit.paths, &sha(NEW), NEW.len()), &[NEW]);
            let (status, body) = exchange_bytes(unit.addr, &apply.head, &apply.body).await;
            assert_eq!(status, 202, "{}", String::from_utf8_lossy(&body));
            assert_eq!(apply.applied(&body).files.len(), 1);
            assert_eq!(installed(&unit.paths), NEW);
            assert_eq!(std::fs::read_to_string(unit.paths.sender()).unwrap(), "desk", "the trial notice names it");
            assert_eq!(read(&unit, &session, &session_key, &[], false).await, closed(), "spent");
            assert_eq!(trusted(&unit).list().len(), 1, "a trusted session grants no other laptop");
        });
    }

    #[test]
    fn the_notice_follows_the_stages() {
        run(async {
            let unit = unit_with("notice", handover).await;
            let (laptop, key) = trusting(&unit, "desk");
            let (session, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            assert!(unit.shown.try_recv().is_err(), "nothing shows before a request authenticates");
            assert_eq!(info(&unit, &session, &session_key).await.0, 200);
            assert_eq!(unit.shown.try_recv(), Ok(notice("desk", Stage::Connected)));
            assert_eq!(unit.shown.try_recv(), Ok(notice("desk", Stage::Reading)));
            assert_eq!(read(&unit, &session, &session_key, &[], false).await.0, 200);
            assert!(unit.shown.try_recv().is_err());
            poll(&unit.shared(), "lo", Instant::now());
            assert!(unit.shown.try_recv().is_err(), "the session is live");
            // Idle for a minute, it ends.
            poll(&unit.shared(), "lo", Instant::now() + window::TRUSTED_IDLE);
            assert_eq!(unit.shown.try_recv(), Ok(None));
            // A set refused once it opened ends the session.
            paced().await;
            let (session, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            let mut set = binary_set(&unit.paths, &sha(NEW), NEW.len());
            set.entries[0].old = Some(sha(b"another binary"));
            let apply = Apply::new(&session, &session_key, &set, &[NEW]);
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await.0, 409);
            let views: Vec<_> = unit.shown.try_iter().collect();
            assert_eq!(views, [notice("desk", Stage::Connected), notice("desk", Stage::Receiving), None]);
            // One installed shows until the restart.
            paced().await;
            let (session, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            let apply = Apply::new(&session, &session_key, &binary_set(&unit.paths, &sha(NEW), NEW.len()), &[NEW]);
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await.0, 202);
            let views: Vec<_> = unit.shown.try_iter().collect();
            assert_eq!(views, [notice("desk", Stage::Connected), notice("desk", Stage::Receiving),
                               notice("desk", Stage::Installing), notice("desk", Stage::Restarting)]);
            poll(&unit.shared(), "lo", Instant::now() + window::OPEN_FOR);
            assert!(unit.shown.try_recv().is_err(), "the restart takes it");
            for _ in 0..30 {
                if unit.restarts.load(Ordering::SeqCst) > 0 { break; }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert_eq!(unit.restarts.load(Ordering::SeqCst), 1);
            assert_eq!(unit.shown.try_recv(), Ok(None), "the screen is given back before the restart");
        });
    }

    #[test]
    fn an_unknown_laptop_is_not_trusted() {
        run(async {
            let unit = unit_with("untrusted", handover).await;
            let not_trusted = || (404, "not trusted\n".to_string());
            assert_eq!(trusted_session(&unit, &"ab".repeat(16), &[1; 32]).await.unwrap_err(), not_trusted());
            // Nor once forgotten.
            let (laptop, key) = trusting(&unit, "desk");
            trusted(&unit).forget(&laptop).unwrap();
            paced().await;
            assert_eq!(trusted_session(&unit, &laptop, &key).await.unwrap_err(), not_trusted());
            assert!(!unit.window.is_open(Instant::now()) && unit.shown.try_recv().is_err());
        });
    }

    #[test]
    fn trusted_exchanges_are_limited_to_one_a_second_per_address() {
        run(async {
            let unit = unit_with("paced", handover).await;
            let (laptop, key) = trusting(&unit, "desk");
            assert!(trusted_session(&unit, &laptop, &key).await.is_ok());
            let too_soon = (429, format!("{TOO_SOON}\n"));
            assert_eq!(trusted_session(&unit, &laptop, &key).await.unwrap_err(), too_soon);
            assert_eq!(trusted_session(&unit, &"ab".repeat(16), &key).await.unwrap_err(), too_soon, "whatever the laptop");
            paced().await;
            let (session, session_key, confirmed) = trusted_session(&unit, &laptop, &key).await.unwrap();
            assert!(confirmed);
            assert_eq!(info(&unit, &session, &session_key).await.0, 200);
        });
    }

    #[test]
    fn no_screen_refuses_a_trusted_session() {
        run(async {
            let unit = unit("trusted-nohandover").await;
            let (laptop, key) = trusting(&unit, "desk");
            let refused = trusted_session(&unit, &laptop, &key).await.unwrap_err();
            assert_eq!(refused, (409, "open Settings → UPDATE on the thermostat and press Allow update\n".into()));
            fn in_use(_: &std::path::Path) -> Result<(), handover::Unavailable> {
                Err(handover::Unavailable::InUse)
            }
            let unit = unit_with("trusted-inuse", in_use).await;
            let (laptop, key) = trusting(&unit, "desk");
            let refused = trusted_session(&unit, &laptop, &key).await.unwrap_err();
            assert_eq!(refused, (409, "the thermostat's screen is in use\n".into()));
            assert!(unit.window.closes_at(Instant::now()).is_none(), "no session");
        });
    }

    /// Whether nlclient has handed its screen over since a nudge asked it.
    static HANDED: AtomicBool = AtomicBool::new(false);

    /// keeper.pid names the running nlclient once HANDED.
    fn handed_when_asked(_: &std::path::Path) -> Result<(), handover::Unavailable> {
        if HANDED.load(Ordering::SeqCst) { Ok(()) } else { Err(handover::Unavailable::NoHandover) }
    }

    /// A nudge that counts its runs in nudges and, when hands, has nlclient
    /// hand its screen over 500 ms later.
    fn nudging(nudges: &Arc<AtomicUsize>, hands: bool) -> Option<Nudge> {
        HANDED.store(false, Ordering::SeqCst);
        let counted = nudges.clone();
        Some(Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            if hands {
                std::thread::spawn(|| {
                    std::thread::sleep(Duration::from_millis(500));
                    HANDED.store(true, Ordering::SeqCst);
                });
            }
            Resent::Sent
        }))
    }

    #[test]
    fn a_nudge_put_off_by_the_devices_own_change_is_retried_within_the_wait() {
        let (handed, calls) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicUsize::new(0)));
        let (hands, counted) = (handed.clone(), calls.clone());
        let nudge: Nudge = Arc::new(move || {
            if counted.fetch_add(1, Ordering::SeqCst) < 3 {
                return Resent::DeviceBusy;
            }
            hands.store(true, Ordering::SeqCst);
            Resent::Sent
        });
        let check = || if handed.load(Ordering::SeqCst) { Ok(()) } else { Err(handover::Unavailable::NoHandover) };
        assert_eq!(handed_over(check, Some(&nudge)), Ok(()));
        assert_eq!(calls.load(Ordering::SeqCst), 4, "asked again until sent, then no more");
        // Sent lately, or never possible: asked once, and the wait runs out.
        for resent in [Resent::Recently, Resent::Missing] {
            let calls = Arc::new(AtomicUsize::new(0));
            let counted = calls.clone();
            let nudge: Nudge = Arc::new(move || { counted.fetch_add(1, Ordering::SeqCst); resent });
            let started = Instant::now();
            assert_eq!(handed_over(|| Err(handover::Unavailable::NoHandover), Some(&nudge)), Err(handover::Unavailable::NoHandover));
            assert!(started.elapsed() >= HANDOVER_FOR);
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{resent:?}");
        }
    }

    #[test]
    fn a_screen_taken_meanwhile_refuses_as_in_use() {
        run(async {
            fn in_use_when_asked(_: &std::path::Path) -> Result<(), handover::Unavailable> {
                Err(if HANDED.load(Ordering::SeqCst) { handover::Unavailable::InUse } else { handover::Unavailable::NoHandover })
            }
            let nudges = Arc::new(AtomicUsize::new(0));
            let unit = unit_nudging("nudge-taken", in_use_when_asked, Display::recording(), nudging(&nudges, true)).await;
            assert_eq!(open(&unit, "laptop").await, (409, "the thermostat's screen is in use\n".into()));
            assert_eq!(nudges.load(Ordering::SeqCst), 1);
        });
    }

    const NO_HANDOVER: &str = "open Settings → UPDATE on the thermostat and press Allow update\n";

    #[test]
    fn a_missing_hand_over_is_asked_for_and_the_prompt_then_raised() {
        run(async {
            let nudges = Arc::new(AtomicUsize::new(0));
            let unit = unit_nudging("nudge-prompt", handed_when_asked, Display::recording(), nudging(&nudges, true)).await;
            let addr = unit.addr;
            let opening = tokio::spawn(async move { post(addr, "/update/open", r#"{"name": "laptop"}"#).await });
            tokio::time::sleep(Duration::from_millis(100)).await;
            let started = Instant::now();
            assert!(get(unit.addr).await.is_object());
            assert!(started.elapsed() < Duration::from_millis(300), "the wait holds up nothing else");
            let (status, body) = opening.await.unwrap();
            assert_eq!(status, 202, "{body}");
            assert_eq!(nudges.load(Ordering::SeqCst), 1);
            assert_eq!(prompt_shown(&unit).0, "laptop");
        });
    }

    #[test]
    fn a_missing_hand_over_is_asked_for_and_the_trusted_session_then_made() {
        run(async {
            let nudges = Arc::new(AtomicUsize::new(0));
            let unit = unit_nudging("nudge-trusted", handed_when_asked, Display::recording(), nudging(&nudges, true)).await;
            let (laptop, key) = trusting(&unit, "desk");
            let (session, session_key, confirmed) = trusted_session(&unit, &laptop, &key).await.unwrap();
            assert!(confirmed);
            assert_eq!(nudges.load(Ordering::SeqCst), 1);
            assert_eq!(info(&unit, &session, &session_key).await.0, 200);
        });
    }

    #[test]
    fn a_hand_over_that_never_comes_refuses_as_before() {
        run(async {
            let nudges = Arc::new(AtomicUsize::new(0));
            let unit = unit_nudging("nudge-never", no_handover, Display::recording(), nudging(&nudges, false)).await;
            let started = Instant::now();
            assert_eq!(open(&unit, "laptop").await, (409, NO_HANDOVER.into()));
            assert!(started.elapsed() >= HANDOVER_FOR, "refused before the wait was out");
            assert_eq!(nudges.load(Ordering::SeqCst), 1, "once per request");
            assert!(unit.shown.try_recv().is_err());
            let (laptop, key) = trusting(&unit, "desk");
            assert_eq!(trusted_session(&unit, &laptop, &key).await.unwrap_err(), (409, NO_HANDOVER.into()));
            assert_eq!(nudges.load(Ordering::SeqCst), 2);
            assert!(unit.window.closes_at(Instant::now()).is_none(), "no session");
        });
    }

    #[test]
    fn a_screen_in_use_asks_nlclient_nothing() {
        run(async {
            fn in_use(_: &std::path::Path) -> Result<(), handover::Unavailable> {
                Err(handover::Unavailable::InUse)
            }
            let nudges = Arc::new(AtomicUsize::new(0));
            let unit = unit_nudging("nudge-inuse", in_use, Display::recording(), nudging(&nudges, true)).await;
            assert_eq!(open(&unit, "laptop").await, (409, "the thermostat's screen is in use\n".into()));
            let (laptop, key) = trusting(&unit, "desk");
            assert_eq!(trusted_session(&unit, &laptop, &key).await.unwrap_err(), (409, "the thermostat's screen is in use\n".into()));
            assert_eq!(nudges.load(Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn a_notice_the_screen_cannot_show_refuses_the_session() {
        run(async {
            fn cannot_take(_: &std::path::Path) -> std::io::Result<handover::Taken> {
                Err(std::io::Error::other("no panel"))
            }
            let dir = std::env::temp_dir().join(format!("nestlocal-update-untaken-{}", std::process::id()));
            let display = display::start_with(dir, cannot_take);
            let unit = unit_on("untaken", handover, display, std::sync::mpsc::channel().1).await;
            let (laptop, key) = trusting(&unit, "desk");
            let (session, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            assert_eq!(info(&unit, &session, &session_key).await, (409, b"the thermostat's screen is in use\n".to_vec()));
            assert!(!unit.window.holds_trusted(Instant::now()) && !unit.display.showing(), "the session is dropped");
            assert_eq!(info(&unit, &session, &session_key).await, closed());
            // Nor does an apply run with nothing on the screen.
            paced().await;
            let (session, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            let apply = Apply::new(&session, &session_key, &binary_set(&unit.paths, &sha(NEW), NEW.len()), &[NEW]);
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await.0, 409);
            assert_eq!(installed(&unit.paths), OLD);
            assert!(!BUSY.load(Ordering::SeqCst) && !unit.display.showing());
        });
    }

    #[test]
    fn a_prompt_raised_before_a_trusted_session_proves_itself_gives_way() {
        run(async {
            let unit = unit_with("prompt-gives-way", handover).await;
            let (laptop, key) = trusting(&unit, "desk");
            let (session, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            assert_eq!(open(&unit, "other").await.0, 202);
            prompt_shown(&unit);
            assert_eq!(info(&unit, &session, &session_key).await.0, 200);
            assert_eq!(unit.shown.try_recv(), Ok(notice("desk", Stage::Connected)));
            assert_eq!(get(unit.addr).await["prompt"]["state"], "preempted");
            assert_eq!(unit.prompt.strikes(), 0, "no strike against the other laptop");
            assert!(!unit.prompt.due(Instant::now() + prompt::ASK_FOR), "it cannot time out as a strike");
            assert_eq!(unit.shown.try_recv(), Ok(notice("desk", Stage::Reading)));
            assert!(unit.shown.try_recv().is_err());
        });
    }

    #[test]
    fn close_ends_the_session_and_takes_the_notice_away() {
        run(async {
            let unit = unit_with("close", handover).await;
            let (laptop, key) = trusting(&unit, "desk");
            let (trusted_id, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            assert_eq!(info(&unit, &trusted_id, &session_key).await.0, 200);
            assert_eq!(sealed(&unit, "/update/close", &trusted_id, &session_key, b"{}").await, (200, b"{}".to_vec()));
            let views: Vec<_> = unit.shown.try_iter().collect();
            assert_eq!(views, [notice("desk", Stage::Connected), notice("desk", Stage::Reading), None]);
            assert_eq!(info(&unit, &trusted_id, &session_key).await, closed());
            assert!(!unit.window.is_open(Instant::now()));
            // A code session's close leaves its window open to exchanges.
            let code = unit.window.open(Instant::now());
            let (keyed, keyed_key, _) = session(&unit, &code).await;
            assert_eq!(sealed(&unit, "/update/close", &keyed, &keyed_key, b"{}").await.0, 200);
            assert_eq!(info(&unit, &keyed, &keyed_key).await, stale());
            assert!(unit.window.accepting(Instant::now()));
            assert_eq!(exchange(unit.addr, "GET /update/close HTTP/1.1\r\n", b"").await.0, 405);
        });
    }

    #[test]
    fn closing_the_session_that_took_a_grant_closes_its_window() {
        run(async {
            let unit = unit_with("close-granted", handover).await;
            let code = allowed_with_trust(&unit).await;
            let unit_id = trusted(&unit).unit_id().to_string();
            let (session, key, _, grant) = granted(&unit, &code, "laptop").await;
            let laptop = grant.unwrap().1;
            assert_eq!(read(&unit, &session, &key, &[], false).await.0, 200);
            assert_eq!(unit.shown.try_recv(), Ok(None), "the read proved the code");
            assert_eq!(sealed(&unit, "/update/close", &session, &key, b"{}").await.0, 200);
            let status = get(unit.addr).await;
            assert_eq!((&status["open"], &status["unit"]), (&json!(false), &json!(unit_id)));
            assert!(!settings_ui::dir().join(CODE).exists());
            assert_eq!(screen("update.status"), CLOSED);
            // The laptop's next session uses its trust at once.
            let (trusted_id, session_key, confirmed) = trusted_session(&unit, &laptop, &update_wire::auth::trust_key(&key, &unit_id, &laptop)).await.unwrap();
            assert!(confirmed);
            assert_eq!(info(&unit, &trusted_id, &session_key).await.0, 200);
        });
    }

    #[test]
    fn no_update_api_deletes_the_trusted_list() {
        let root = std::env::temp_dir().join(format!("nestlocal-revoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let file = root.join(trust::FILE.trim_start_matches('/'));
        let list = root.join(settings_ui::DIR.trim_start_matches('/')).join(trust::LIST);
        for path in [&file, &list] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        let wake = root.join("media/data/other");
        std::fs::write(&wake, "x").unwrap();
        trust::revoke(&root);
        assert!(!file.exists() && !list.exists());
        assert!(wake.exists(), "nothing else goes");
        trust::revoke(&root);
    }

    fn cancelled() -> (u16, Vec<u8>) {
        (409, format!("{CANCELLED}\n").into_bytes())
    }

    #[test]
    fn cancel_while_reading_drops_the_session() {
        run(async {
            let unit = unit_with("cancel-reading", handover).await;
            let (laptop, key) = trusting(&unit, "desk");
            let (session, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            assert_eq!(info(&unit, &session, &session_key).await.0, 200);
            act(&unit.shared(), "lo", Action::Cancel, Instant::now());
            let views: Vec<_> = unit.shown.try_iter().collect();
            assert_eq!(views, [notice("desk", Stage::Connected), notice("desk", Stage::Reading), None]);
            assert!(!unit.window.holds_trusted(Instant::now()));
            assert_eq!(read(&unit, &session, &session_key, &[], false).await, cancelled());
            assert_eq!(info(&unit, &session, &session_key).await, cancelled());
            let apply = Apply::new(&session, &session_key, &binary_set(&unit.paths, &sha(NEW), NEW.len()), &[NEW]);
            assert_eq!(exchange_bytes(unit.addr, &apply.head, &apply.body).await, cancelled());
            assert_eq!(installed(&unit.paths), OLD);
            assert!(unit.shown.try_recv().is_err());
            // Cancel never forgets the laptop.
            assert_eq!(trusted(&unit).list().len(), 1);
            paced().await;
            let (session, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            assert_eq!(info(&unit, &session, &session_key).await.0, 200);
        });
    }

    #[test]
    fn cancel_while_receiving_leaves_nothing_staged() {
        run(async {
            let unit = unit_with("cancel-receiving", handover).await;
            let (laptop, key) = trusting(&unit, "desk");
            let (session, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            let apply = Apply::new(&session, &session_key, &binary_set(&unit.paths, &sha(NEW), NEW.len()), &[&NEW[..4], &NEW[4..]]);
            let mut tcp = tokio::net::TcpStream::connect(unit.addr).await.unwrap();
            tcp.write_all(format!("{}Host: unit\r\n\r\n", apply.head).as_bytes()).await.unwrap();
            let first = apply.without_last(NEW.len() - 4);
            tcp.write_all(first).await.unwrap();
            for _ in 0..100 {
                if staged(&unit.paths).exists() { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(staged(&unit.paths).exists());
            act(&unit.shared(), "lo", Action::Cancel, Instant::now());
            tcp.write_all(&apply.body[first.len()..]).await.unwrap();
            let mut out = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), tcp.read_to_end(&mut out)).await.unwrap().unwrap();
            let out = String::from_utf8_lossy(&out);
            assert!(out.starts_with("HTTP/1.1 409") && out.ends_with(&format!("{CANCELLED}\n")), "{out}");
            assert!(!staged(&unit.paths).exists() && !bundle::pending(&unit.paths));
            assert_eq!(installed(&unit.paths), OLD);
            assert_eq!(screen("update.status"), format!("Refused: {CANCELLED}"));
            assert!(!BUSY.load(Ordering::SeqCst) && !unit.display.showing());
            assert!(unit.restarts.load(Ordering::SeqCst) == 0);
        });
    }

    #[test]
    fn cancel_during_the_swap_rolls_the_finished_set_back() {
        run(async {
            let unit = unit_with("cancel-swap", handover).await;
            let (laptop, key) = trusting(&unit, "desk");
            let (session, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            // The staged nestlocal's -h holds the swap until go exists.
            let go = unit.paths.pending.with_file_name("go");
            let gated = format!("#!/bin/sh\nwhile [ ! -e {} ]; do sleep 0.02; done\nexit 0\n", go.display()).into_bytes();
            let set = Bundle { class: Class::Reboot, ..binary_set(&unit.paths, &sha(&gated), gated.len()) };
            let apply = Apply::new(&session, &session_key, &set, &[&gated]);
            let (addr, head, body) = (unit.addr, apply.head.clone(), apply.body.clone());
            let sent = tokio::spawn(async move { exchange_bytes(addr, &head, &body).await });
            for _ in 0..500 {
                if screen("update.status") == INSTALLING { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(screen("update.status"), INSTALLING);
            act(&unit.shared(), "lo", Action::Cancel, Instant::now());
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(installed(&unit.paths), OLD, "the swap waits for its check");
            std::fs::write(&go, "").unwrap();
            let (status, body) = sent.await.unwrap();
            assert_eq!(status, 202, "{}", String::from_utf8_lossy(&body));
            assert_eq!(apply.applied(&body).files.len(), 1, "answered with what swapped in");
            assert_eq!(installed(&unit.paths), gated, "the swap ran to its end");
            for _ in 0..30 {
                if unit.restarts.load(Ordering::SeqCst) > 0 { break; }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert_eq!(*unit.classes.lock().unwrap(), [Class::Restart], "a restart into the guard's rollback, not the set's reboot");
            assert_eq!(std::fs::read_to_string(&unit.paths.pending).unwrap(), "3\n");
            assert_eq!(get(unit.addr).await["rolled_back_by"], "thermostat");
        });
    }

    /// Asks, as the screen's Forget does, for the laptop of id to be
    /// forgotten, and polls.
    fn forget_on_the_screen(unit: &Unit, id: &str) {
        settings_ui::publish("update-forget.request", id.as_bytes()).unwrap();
        poll(&unit.shared(), "lo", Instant::now());
        assert!(!settings_ui::dir().join("update-forget.request").exists(), "the request is taken");
    }

    #[test]
    fn forgetting_a_laptop_mid_upload_cancels_its_session() {
        run(async {
            let unit = unit_with("forget-receiving", handover).await;
            let (laptop, key) = trusting(&unit, "desk");
            let (other, _) = trusting(&unit, "lounge");
            let (session, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            assert_eq!(info(&unit, &session, &session_key).await.0, 200);
            // Forgetting another laptop leaves this one's session be.
            forget_on_the_screen(&unit, &other);
            assert_eq!(trusted(&unit).listed(), format!("{laptop} desk\n"));
            assert_eq!(info(&unit, &session, &session_key).await.0, 200);
            let apply = Apply::new(&session, &session_key, &binary_set(&unit.paths, &sha(NEW), NEW.len()), &[&NEW[..4], &NEW[4..]]);
            let mut tcp = tokio::net::TcpStream::connect(unit.addr).await.unwrap();
            tcp.write_all(format!("{}Host: unit\r\n\r\n", apply.head).as_bytes()).await.unwrap();
            let first = apply.without_last(NEW.len() - 4);
            tcp.write_all(first).await.unwrap();
            for _ in 0..100 {
                if staged(&unit.paths).exists() { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(staged(&unit.paths).exists());
            forget_on_the_screen(&unit, &laptop);
            tcp.write_all(&apply.body[first.len()..]).await.unwrap();
            let mut out = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), tcp.read_to_end(&mut out)).await.unwrap().unwrap();
            let out = String::from_utf8_lossy(&out);
            assert!(out.starts_with("HTTP/1.1 409") && out.ends_with(&format!("{CANCELLED}\n")), "{out}");
            assert!(!staged(&unit.paths).exists() && !bundle::pending(&unit.paths));
            assert_eq!(installed(&unit.paths), OLD);
            assert!(!BUSY.load(Ordering::SeqCst) && !unit.display.showing());
            assert!(trusted(&unit).list().is_empty() && trusted(&unit).listed().is_empty());
            paced().await;
            let (status, body) = trusted_session(&unit, &laptop, &key).await.unwrap_err();
            assert_eq!((status, body.as_str()), (404, "not trusted\n"));
        });
    }

    #[test]
    fn a_forget_during_an_exchange_leaves_no_session() {
        run(async {
            let unit = unit_with("forget-exchange", handover).await;
            let (laptop, key) = trusting(&unit, "desk");
            let (shared, id) = (unit.shared(), laptop.clone());
            *BETWEEN.lock().unwrap() = Some(Box::new(move || {
                settings_ui::publish("update-forget.request", id.as_bytes()).unwrap();
                poll(&shared, "lo", Instant::now());
            }));
            let (status, body) = trusted_session(&unit, &laptop, &key).await.unwrap_err();
            assert_eq!((status, body.as_str()), (404, "not trusted\n"));
            assert!(BETWEEN.lock().unwrap().is_none(), "the Forget landed in the exchange");
            assert!(trusted(&unit).list().is_empty());
            assert!(unit.window.closes_at(Instant::now()).is_none(), "no session");
        });
    }

    #[test]
    fn forgetting_a_laptop_ends_its_open_session() {
        run(async {
            let unit = unit_with("forget-reading", handover).await;
            let (laptop, key) = trusting(&unit, "desk");
            let (session, session_key, _) = trusted_session(&unit, &laptop, &key).await.unwrap();
            assert_eq!(info(&unit, &session, &session_key).await.0, 200);
            forget_on_the_screen(&unit, &laptop);
            assert!(!unit.window.holds_trusted(Instant::now()) && !unit.display.showing());
            assert_eq!(info(&unit, &session, &session_key).await, cancelled());
            // An id the unit does not trust changes nothing.
            let (kept, _) = trusting(&unit, "lounge");
            forget_on_the_screen(&unit, "0123456789abcdef0123456789abcdef");
            forget_on_the_screen(&unit, "not an id");
            assert_eq!(trusted(&unit).listed(), format!("{kept} lounge\n"));
        });
    }

    #[test]
    fn a_trust_request_from_the_screen_grants_with_the_exchanged_name() {
        run(async {
            let unit = unit_with("screen-trust", handover).await;
            // A strike the screen's request clears.
            assert_eq!(open(&unit, "laptop").await.0, 202);
            prompt_shown(&unit);
            act(&unit.shared(), "lo", Action::Deny, Instant::now());
            assert_eq!(unit.prompt.strikes(), 1);
            settings_ui::publish("update-trust.request", b"1").unwrap();
            poll(&unit.shared(), "lo", Instant::now());
            assert!(!settings_ui::dir().join("update-trust.request").exists(), "the request is taken");
            assert_eq!(unit.prompt.strikes(), 0);
            assert_eq!(screen("update.status"), OPEN);
            let code = screen(CODE).split_once('\n').unwrap().0.to_string();
            let unit_id = trusted(&unit).unit_id().to_string();
            let (session, key, confirmed, grant) = granted(&unit, &code, "my laptop").await;
            assert!(confirmed);
            let (granted_unit, laptop) = grant.expect("a window the screen opened with trust grants");
            assert_eq!(granted_unit, unit_id);
            assert_eq!(info(&unit, &session, &key).await.0, 200);
            assert_eq!(trusted(&unit).listed(), format!("{laptop} my laptop\n"), "the exchange's name");
        });
    }

    #[test]
    fn a_trust_request_with_the_list_full_opens_a_window_without_trust() {
        run(async {
            let unit = unit_with("screen-trust-full", handover).await;
            for n in 0..trust::LIMIT {
                trusting(&unit, &format!("l{n}"));
            }
            settings_ui::publish("update-trust.request", b"1").unwrap();
            poll(&unit.shared(), "lo", Instant::now());
            let code = screen(CODE).split_once('\n').unwrap().0.to_string();
            let (session, key, confirmed, grant) = granted(&unit, &code, "my laptop").await;
            assert!(confirmed && grant.is_none());
            assert_eq!(info(&unit, &session, &key).await.0, 200);
            assert_eq!(trusted(&unit).list().len(), trust::LIMIT, "no fifth");
        });
    }

    #[test]
    fn a_rollback_request_off_trial_does_nothing() {
        run(async {
            let unit = unit_with("screen-rollback-off", handover).await;
            register(&unit.shared());
            settings_ui::publish("update-rollback.request", b"1").unwrap();
            poll(&unit.shared(), "lo", Instant::now());
            assert!(!settings_ui::dir().join("update-rollback.request").exists(), "the request is taken");
            assert!(!unit.paths.pending.exists() && bundle::cancelled_by(&unit.paths).is_none());
            assert_eq!(unit.restarts.load(Ordering::SeqCst), 0);
            assert_eq!(get(unit.addr).await["rolled_back_by"], serde_json::Value::Null);
        });
    }

    #[test]
    fn a_rollback_request_on_trial_rolls_back_as_the_notice_does() {
        run(async {
            let unit = unit_with("screen-rollback-on", handover).await;
            register(&unit.shared());
            put_on_trial(&unit.paths, "desk");
            let live = Liveness::new(Duration::from_secs(300));
            commit_when(live.clone(), unit.paths.clone(), "lo".into(), hooks(&unit));
            assert_eq!(next_shown(&unit), trial("desk"));
            act(&unit.shared(), "lo", Action::Hide, Instant::now());
            assert_eq!(unit.shown.try_recv(), Ok(None));
            assert!(trial_file().is_some());
            settings_ui::publish("update-rollback.request", b"1").unwrap();
            poll(&unit.shared(), "lo", Instant::now());
            assert_eq!(std::fs::read_to_string(&unit.paths.pending).unwrap(), "3\n");
            assert!(trial_file().is_none());
            assert_eq!(*unit.classes.lock().unwrap(), [Class::Restart]);
            assert_eq!(get(unit.addr).await["rolled_back_by"], "thermostat");
        });
    }

    /// A set on trial that restarts nestlocal, swapped in with its marker
    /// and sent by sender.
    #[test]
    fn a_release_install_and_a_wifi_apply_never_run_together_nor_over_a_trial() {
        run(async {
            let paths = paths("install-hold");
            let hold = hold_install_at(&paths).unwrap();
            assert!(Busy::take().is_none(), "no set is taken while the install runs");
            assert_eq!(hold_install_at(&paths).err(), Some(INSTALL_APPLYING));
            drop(hold);
            assert!(!BUSY.load(Ordering::SeqCst), "a failed install lets the unit go");
            assert_eq!(hold_install_at(&paths).unwrap().restart(|| Ok(())), Ok(()));
            assert!(BUSY.load(Ordering::SeqCst), "held until the restart");
            BUSY.store(false, Ordering::SeqCst);
            let failed = hold_install_at(&paths).unwrap().restart(|| Err("reboot: exit status: 1".into()));
            assert_eq!(failed, Err("reboot: exit status: 1".into()));
            assert!(!BUSY.load(Ordering::SeqCst), "a reboot that failed lets the unit go");
            let applying = Busy::take().unwrap();
            assert_eq!(hold_install_at(&paths).err(), Some(INSTALL_APPLYING));
            drop(applying);
            put_on_trial(&paths, "desk");
            assert_eq!(hold_install_at(&paths).err(), Some(INSTALL_ON_TRIAL));
            assert!(!BUSY.load(Ordering::SeqCst));
        });
    }

    fn on_trial(name: &str, sender: &str) -> Paths {
        let paths = paths(name);
        put_on_trial(&paths, sender);
        paths
    }

    fn put_on_trial(paths: &Paths, sender: &str) {
        std::fs::write(&paths.pending, "1\n").unwrap();
        std::fs::create_dir_all(paths.journal.parent().unwrap()).unwrap();
        std::fs::write(&paths.journal, "unfr0zen-update 1\nclass restart\nstate swapped\nwrite etc/x -\n").unwrap();
        std::fs::write(paths.sender(), sender).unwrap();
    }

    fn trial_file() -> Option<String> {
        std::fs::read_to_string(settings_ui::dir().join(TRIAL_FILE)).ok()
    }

    /// commit_when's hooks on unit, whose screen hands over.
    fn hooks(unit: &Unit) -> Hooks {
        Hooks { address: |_| None, handover, restart: unit.restart.clone(), nudge: None }
    }

    /// What the display is next asked to show, within 3 s.
    fn next_shown(unit: &Unit) -> Option<Option<View>> {
        unit.shown.recv_timeout(Duration::from_secs(3)).ok()
    }

    fn trial(name: &str) -> Option<Option<View>> {
        Some(Some(View::Trial { name: name.into() }))
    }

    /// Waits up to 3 s for the set on trial at paths to be kept.
    async fn kept(paths: &Paths) {
        for _ in 0..30 {
            if !bundle::pending(paths) { break; }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!bundle::pending(paths));
    }

    #[test]
    fn the_trial_notice_shows_until_kept_and_hide_hands_the_screen_back() {
        run(async {
            let unit = unit_with("trial-notice", handover).await;
            register(&unit.shared());
            let paths = on_trial("trial-notice-set", "desk");
            let live = Liveness::new(Duration::from_secs(300));
            commit_when(live.clone(), paths.clone(), "lo".into(), hooks(&unit));
            assert_eq!(next_shown(&unit), trial("desk"));
            assert_eq!(trial_file().as_deref(), Some("On trial: update from desk"));
            let call = live.request();
            assert_eq!(next_shown(&unit), Some(None), "kept, it hands the screen back");
            drop(call);
            kept(&paths).await;
            assert!(!paths.sender().exists() && trial_file().is_none());

            let paths = on_trial("trial-hide-set", "unfr0zen");
            let live = Liveness::new(Duration::from_secs(300));
            commit_when(live.clone(), paths.clone(), "lo".into(), hooks(&unit));
            assert_eq!(next_shown(&unit), trial("unfr0zen"));
            act(&unit.shared(), "lo", Action::Hide, Instant::now());
            assert_eq!(unit.shown.try_recv(), Ok(None));
            assert_eq!(trial_file().as_deref(), Some("On trial: update from unfr0zen"), "Roll back stays on the Update page");
            tokio::time::sleep(Duration::from_millis(1500)).await;
            assert!(unit.shown.try_recv().is_err(), "hidden for the rest of the trial");
            assert!(paths.sender().exists() && bundle::pending(&paths));
            let _call = live.request();
            for _ in 0..30 {
                if !paths.sender().exists() { break; }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(!bundle::pending(&paths) && !paths.sender().exists());
            assert!(unit.shown.try_recv().is_err());
            for _ in 0..30 {
                if trial_file().is_none() { break; }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(trial_file().is_none());
        });
    }

    #[test]
    fn the_trial_notice_asks_for_a_missing_hand_over_once_nlclient_calls_in() {
        run(async {
            let nudges = Arc::new(AtomicUsize::new(0));
            let unit = unit_nudging("trial-nudge", handed_when_asked, Display::recording(), None).await;
            register(&unit.shared());
            // A reboot that is kept only on the network, which it never is here.
            let paths = reboot_trial("trial-nudge-set");
            let live = Liveness::new(Duration::from_secs(300));
            let nudge = nudging(&nudges, true);
            commit_when(live.clone(), paths.clone(), "lo".into(), Hooks { address: |_| None, handover: handed_when_asked,
                                                                          restart: unit.restart.clone(), nudge });
            tokio::time::sleep(Duration::from_millis(1500)).await;
            assert_eq!(nudges.load(Ordering::SeqCst), 0, "not before nlclient calls in");
            assert!(unit.shown.try_recv().is_err());
            let _call = live.request();
            tokio::time::sleep(CALLING_IN_FOR - Duration::from_millis(500)).await;
            assert_eq!(nudges.load(Ordering::SeqCst), 0, "not before nlclient has called in for CALLING_IN_FOR");
            assert_eq!(unit.shown.recv_timeout(Duration::from_secs(4)).ok(), trial("unfr0zen"));
            assert_eq!(nudges.load(Ordering::SeqCst), 1);
            trial_state(&current_trial().unwrap()).over = true;
            assert!(paths.pending.exists());
        });
    }

    #[test]
    fn the_trial_notice_asks_once_when_the_hand_over_never_comes() {
        run(async {
            let nudges = Arc::new(AtomicUsize::new(0));
            let unit = unit_nudging("trial-nudge-never", no_handover, Display::recording(), None).await;
            register(&unit.shared());
            let paths = reboot_trial("trial-nudge-never-set");
            let live = Liveness::new(Duration::from_secs(300));
            let nudge = nudging(&nudges, false);
            commit_when(live.clone(), paths.clone(), "lo".into(), Hooks { address: |_| None, handover: no_handover,
                                                                          restart: unit.restart.clone(), nudge });
            let _call = live.request();
            tokio::time::sleep(CALLING_IN_FOR + HANDOVER_FOR + Duration::from_secs(3)).await;
            assert_eq!(nudges.load(Ordering::SeqCst), 1, "once a trial");
            assert!(unit.shown.try_recv().is_err());
            trial_state(&current_trial().unwrap()).over = true;
        });
    }

    #[test]
    fn roll_back_from_the_notice_marks_the_trial_failed() {
        run(async {
            let unit = unit_with("trial-rollback", handover).await;
            register(&unit.shared());
            let paths = on_trial("trial-rollback-set", "desk");
            let live = Liveness::new(Duration::from_secs(300));
            commit_when(live.clone(), paths.clone(), "lo".into(), hooks(&unit));
            assert_eq!(next_shown(&unit), trial("desk"));
            assert!(trial_file().is_some());
            // Not while a newer update is being taken.
            BUSY.store(true, Ordering::SeqCst);
            act(&unit.shared(), "lo", Action::RollBack, Instant::now());
            settings_ui::publish("update-rollback.request", b"1").unwrap();
            poll(&unit.shared(), "lo", Instant::now());
            assert_eq!(std::fs::read_to_string(&paths.pending).unwrap(), "1\n");
            assert!(unit.classes.lock().unwrap().is_empty() && trial_file().is_some());
            BUSY.store(false, Ordering::SeqCst);
            act(&unit.shared(), "lo", Action::RollBack, Instant::now());
            assert_eq!(std::fs::read_to_string(&paths.pending).unwrap(), "3\n");
            assert_eq!(bundle::cancelled_by(&paths).as_deref(), Some("thermostat"));
            assert!(trial_file().is_none());
            assert_eq!(*unit.classes.lock().unwrap(), [Class::Restart]);
            assert_eq!(unit.shown.try_recv(), Ok(None));
            // nlclient calling in before the restart keeps nothing.
            let _call = live.request();
            tokio::time::sleep(Duration::from_millis(1500)).await;
            assert!(paths.journal.exists() && std::fs::read_to_string(&paths.pending).unwrap() == "3\n");
            act(&unit.shared(), "lo", Action::RollBack, Instant::now());
            assert_eq!(unit.classes.lock().unwrap().len(), 1, "once");
        });
    }

    #[test]
    fn a_start_with_no_set_on_trial_forgets_who_sent_the_last() {
        run(async {
            let paths = paths("sender");
            std::fs::write(paths.sender(), "desk").unwrap();
            settings_ui::publish(TRIAL_FILE, b"On trial: update from desk").unwrap();
            watch_trial(Liveness::new(Duration::from_secs(300)), paths.clone(), "lo".into(), None);
            assert!(!paths.sender().exists() && trial_file().is_none());
        });
    }

    #[test]
    fn the_trial_file_outlasts_window_and_apply_statuses() {
        run(async {
            let unit = unit_with("trial-file", handover).await;
            register(&unit.shared());
            put_on_trial(&unit.paths, "desk");
            let live = Liveness::new(Duration::from_secs(300));
            commit_when(live.clone(), unit.paths.clone(), "lo".into(), hooks(&unit));
            let on_trial = Some("On trial: update from desk".to_string());
            assert_eq!(trial_file(), on_trial);
            // A window opened on the screen, then lapsed.
            let now = Instant::now();
            settings_ui::publish("update.request", b"").unwrap();
            poll(&unit.shared(), "lo", now);
            assert_eq!(screen("update.status"), OPEN);
            poll(&unit.shared(), "lo", now + window::OPEN_FOR);
            assert_eq!(screen("update.status"), CLOSED);
            assert_eq!(trial_file(), on_trial);
            // An apply, refused while the set is on trial.
            let code = unit.window.open(Instant::now());
            let apply = apply_binary(&unit, &code, NEW).await;
            assert_eq!(exchange(unit.addr, &apply.head, &apply.body).await.0, 409);
            assert!(screen("update.status").starts_with("Refused: "));
            assert_eq!(trial_file(), on_trial);
            let _call = live.request();
            for _ in 0..30 {
                if trial_file().is_none() { break; }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(trial_file().is_none() && !bundle::pending(&unit.paths));
        });
    }
}
