//! The update window: its 8-digit code is the password of a SPAKE2 exchange
//! that keys a session, which reads until the window lapses and applies once.
//! Nothing sent lets a code be tested offline, so each try at one takes an
//! exchange with the unit, MAX_ATTEMPTS a window. A window opened with trust
//! grants each exchange's client a laptop id, until a session proves itself
//! and its grant is taken (trust.rs keeps it); that session's close closes
//! the window, so the laptop's next session can use its trust.
//!
//! A laptop the unit trusts keys a session with no window: its trust key is
//! the exchange's password, with no limit on tries, since a 256-bit key is
//! not guessed. That session must prove itself within PROVE_WITHIN, then
//! lasts TRUSTED_IDLE past its last authenticated request and OPEN_FOR at
//! most, and applies once. It grants nothing. The laptop id is sent in
//! clear, so while the laptop's session holds the unit, another exchange in
//! its name waits beside it, and replaces it only once it proves itself.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use spake2::{Ed25519Group, Identity, Password, Spake2};
use update_wire::auth::{self, sign, CLIENT, UNIT};
use update_wire::{hex, random};

use super::trust::Laptop;

/// How long a window stays open.
pub const OPEN_FOR: Duration = Duration::from_secs(600);
/// Key exchanges a window takes.
pub const MAX_ATTEMPTS: u32 = 5;
/// How long a trusted session lasts past its last authenticated request.
pub const TRUSTED_IDLE: Duration = Duration::from_secs(60);
/// How long a trusted session has to authenticate a request.
pub const PROVE_WITHIN: Duration = Duration::from_secs(10);
/// The unit's confirmation of a session key: hex HMAC-SHA256 under it.
pub fn confirmation(key: &[u8]) -> String {
    sign(key, &auth::confirmation(None, None))
}

/// The unit's confirmation of a session key and the grant of laptop id
/// laptop on the unit of id unit, in place of the laptop of id replaces if
/// any (update_wire::auth::confirmation).
pub fn grant_confirmation(key: &[u8], unit: &str, laptop: &str, replaces: Option<&str>) -> String {
    sign(key, &auth::confirmation(Some((unit, laptop)), replaces))
}

/// The client's side of an exchange, as the app starts it.
#[cfg(test)]
pub fn client(code: &str) -> (Spake2<Ed25519Group>, Vec<u8>) {
    Spake2::<Ed25519Group>::start_a(&Password::new(code.as_bytes()), &Identity::new(CLIENT), &Identity::new(UNIT))
}

/// The client's side of a trusted exchange under its trust key.
#[cfg(test)]
pub fn trusted_client(key: &[u8; 32]) -> (Spake2<Ed25519Group>, Vec<u8>) {
    Spake2::<Ed25519Group>::start_a(&Password::new(key), &Identity::new(CLIENT), &Identity::new(UNIT))
}

/// Eight random decimal digits, leading zeros and all.
fn new_code() -> String {
    // 4 200 000 000 is the largest multiple of 10^8 a u32 holds: drawing
    // below it keeps every code equally likely.
    loop {
        let n = u32::from_be_bytes(random::<4>());
        if n < 4_200_000_000 {
            return format!("{:08}", n % 100_000_000);
        }
    }
}

/// What a window opened with trust grants: laptops on the unit of id unit,
/// named name, or as their exchange names them without one.
struct Trust {
    unit: String,
    name: Option<String>,
}

/// A laptop a session's client is granted: its ids, its name, the trust key
/// its session key derives, and the laptop it replaces (Granting::Replace).
pub struct Grant {
    pub unit: String,
    pub laptop: String,
    pub name: String,
    pub key: [u8; 32],
    pub replaces: Option<String>,
}

/// What an exchange in a window with trust grants, as the trusted list and
/// the request decide (Server::granting).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Granting {
    /// A laptop added to the list.
    Add,
    /// A laptop in place of the one of this id, whose trust key the client
    /// proved it holds.
    Replace(String),
    /// Nothing: the list is full.
    Withheld,
}

struct Session {
    id: String,
    key: Vec<u8>,
    kind: Kind,
}

enum Kind {
    /// Keyed by a window's code; in a window with trust, granted a laptop.
    Code { grant: Option<Grant> },
    /// Keyed by the trust key of the laptop of id laptop, named name; last
    /// is when a request in it last authenticated, started until one has.
    Trusted { laptop: String, name: String, started: Instant, last: Instant, authenticated: bool },
}

impl Kind {
    /// When a trusted session ends; none for a code session, which lasts
    /// while its window is timely.
    fn ends(&self) -> Option<Instant> {
        match self {
            Kind::Code { .. } => None,
            Kind::Trusted { started, authenticated: false, .. } => Some(*started + PROVE_WITHIN),
            Kind::Trusted { started, last, authenticated: true, .. } => Some((*started + OPEN_FOR).min(*last + TRUSTED_IDLE)),
        }
    }
}

#[derive(Default)]
struct State {
    code: Option<String>,
    opened: Option<Instant>,
    attempts: u32,
    session: Option<Session>,
    /// A trusted exchange of the laptop whose session holds the unit, which
    /// replaces that session once a request in it authenticates.
    pending: Option<Session>,
    /// Closed by an apply it authenticated, not by lapsing.
    spent: bool,
    /// Opened with trust, which no session has taken yet.
    trust: Option<Trust>,
    /// The trusted session and pending exchange Cancel on the thermostat
    /// ended, until another exchange or window replaces them.
    cancelled: Vec<String>,
    /// The laptop whose trusted session spent the window on an apply.
    applying: Option<String>,
    /// The session that took the window's grant: closing it closes the
    /// window, whose code has done its work.
    granted: Option<String>,
}

impl State {
    fn opened(code: String, now: Instant, trust: Option<Trust>) -> State {
        State { code: Some(code), opened: Some(now), trust, ..State::default() }
    }

    fn timely(&self, now: Instant) -> bool {
        self.code.is_some() && self.opened.is_some_and(|opened| now.saturating_duration_since(opened) < OPEN_FOR)
    }

    /// Open to a new exchange: time left and an exchange to give.
    fn accepting(&self, now: Instant) -> bool {
        self.timely(now) && self.attempts < MAX_ATTEMPTS
    }

    /// When the session ends: a trusted one by its limits, a code's with
    /// its window.
    fn session_ends(&self) -> Option<Instant> {
        let session = self.session.as_ref()?;
        session.kind.ends().or_else(|| self.opened.filter(|_| self.code.is_some()).map(|opened| opened + OPEN_FOR))
    }

    /// Whether the session is still to be served.
    fn live(&self, now: Instant) -> bool {
        self.session_ends().is_some_and(|ends| now < ends)
    }

    /// The pending exchange's session, if it has not ended.
    fn pending(&self, now: Instant) -> Option<&Session> {
        self.pending.as_ref().filter(|pending| pending.kind.ends().is_some_and(|ends| now < ends))
    }

    /// A trusted session has authenticated and not ended.
    fn holds_trusted(&self, now: Instant) -> bool {
        matches!(&self.session, Some(Session { kind: Kind::Trusted { authenticated: true, .. }, .. })) && self.live(now)
    }

    /// Open to an exchange, or held: by the last exchange's session, or by
    /// a trusted session that authenticated.
    fn is_open(&self, now: Instant) -> bool {
        self.accepting(now) || (self.timely(now) && self.session.is_some()) || self.holds_trusted(now)
    }
}

/// A key exchange's answer, in hex, and the unit and laptop ids of its
/// grant in a window with trust.
#[derive(Debug)]
pub struct Exchange {
    pub session: String,
    pub message: String,
    pub confirm: String,
    pub grant: Option<(String, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExchangeError {
    /// No window open, or no exchange left in it.
    Closed,
    /// Not a client's SPAKE2 message; in a window, the attempt counts.
    Malformed,
    /// A trusted exchange while a window accepts, or while another laptop's
    /// trusted session holds the unit.
    Busy,
}

#[derive(Default)]
pub struct Window {
    state: Mutex<State>,
}

impl Window {
    pub fn new() -> Window {
        Window::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Opens the window with a fresh code, in place of any open one: from
    /// the screen, or on an Allow pressed on the network's prompt.
    pub fn open(&self, now: Instant) -> String {
        self.open_with(None, now)
    }

    /// open, with trust: its exchanges grant laptops on the unit of id unit,
    /// named name, or as each exchange names its laptop without one.
    pub fn open_trusting(&self, unit: String, name: Option<String>, now: Instant) -> String {
        self.open_with(Some(Trust { unit, name }), now)
    }

    fn open_with(&self, trust: Option<Trust>, now: Instant) -> String {
        let code = new_code();
        *self.state() = State::opened(code.clone(), now, trust);
        code
    }

    /// Open to an exchange, or held by a session: the last exchange's, or
    /// a trusted one that authenticated.
    pub fn is_open(&self, now: Instant) -> bool {
        self.state().is_open(now)
    }

    /// Whether a trusted session has authenticated and not ended.
    pub fn holds_trusted(&self, now: Instant) -> bool {
        self.state().holds_trusted(now)
    }

    /// Open to a new exchange: what a client is told.
    pub fn accepting(&self, now: Instant) -> bool {
        self.state().accepting(now)
    }

    /// When the open window lapses, or the last trusted session (or
    /// pending exchange) ends; none while neither is open.
    pub fn closes_at(&self, now: Instant) -> Option<Instant> {
        let state = self.state();
        let trusted: Vec<Instant> = [&state.session, &state.pending].into_iter().flatten()
            .filter_map(|session| session.kind.ends()).collect();
        match trusted.is_empty() {
            true => state.opened.filter(|_| state.is_open(now)).map(|opened| opened + OPEN_FOR),
            false => trusted.into_iter().filter(|ends| now < *ends).max(),
        }
    }

    /// Whether the window accepts exchanges and grants trust in them.
    pub fn grants(&self, now: Instant) -> bool {
        let state = self.state();
        state.accepting(now) && state.trust.is_some()
    }

    /// The unit's side of a key exchange with the client's message, from a
    /// laptop named name, as the session in place of any outstanding one.
    /// Each counts toward MAX_ATTEMPTS. In a window with trust, it grants a
    /// new laptop id, which its confirmation covers, as granting says.
    pub fn exchange(&self, message: &[u8], name: &str, granting: &Granting, now: Instant) -> Result<Exchange, ExchangeError> {
        let mut state = self.state();
        if !state.accepting(now) {
            return Err(ExchangeError::Closed);
        }
        state.attempts += 1;
        let code = state.code.as_deref().unwrap_or_default().as_bytes();
        let (spake, ours) = Spake2::<Ed25519Group>::start_b(&Password::new(code), &Identity::new(CLIENT), &Identity::new(UNIT));
        let key = spake.finish(message).map_err(|_| ExchangeError::Malformed)?;
        let grant = state.trust.as_ref().filter(|_| *granting != Granting::Withheld).map(|trust| {
            let laptop = hex(&random::<16>());
            Grant {
                key: auth::trust_key(&key, &trust.unit, &laptop),
                unit: trust.unit.clone(),
                laptop,
                name: trust.name.clone().unwrap_or_else(|| name.to_string()),
                replaces: match granting {
                    Granting::Replace(old) => Some(old.clone()),
                    Granting::Add | Granting::Withheld => None,
                },
            }
        });
        let (confirm, granted) = match &grant {
            Some(grant) => (grant_confirmation(&key, &grant.unit, &grant.laptop, grant.replaces.as_deref()),
                            Some((grant.unit.clone(), grant.laptop.clone()))),
            None => (confirmation(&key), None),
        };
        let session = Session { id: hex(&random::<16>()), key, kind: Kind::Code { grant } };
        let exchange = Exchange { session: session.id.clone(), message: hex(&ours), confirm, grant: granted };
        state.session = Some(session);
        Ok(exchange)
    }

    /// The unit's side of a key exchange with laptop, which the unit trusts,
    /// keyed by its trust key: the session in place of any, and of a window
    /// with no exchange left; while the laptop's own session holds the unit,
    /// the pending exchange beside it. Busy while a window accepts, or
    /// another laptop's trusted session holds the unit.
    pub fn exchange_trusted(&self, laptop: &Laptop, message: &[u8], now: Instant) -> Result<Exchange, ExchangeError> {
        let mut state = self.state();
        let holder = match &state.session {
            Some(Session { kind: Kind::Trusted { laptop: holder, .. }, .. }) if state.holds_trusted(now) => Some(holder.clone()),
            _ => None,
        };
        if state.accepting(now) || holder.as_ref().is_some_and(|holder| *holder != laptop.id) {
            return Err(ExchangeError::Busy);
        }
        let (spake, ours) = Spake2::<Ed25519Group>::start_b(&Password::new(laptop.key), &Identity::new(CLIENT), &Identity::new(UNIT));
        let key = spake.finish(message).map_err(|_| ExchangeError::Malformed)?;
        let kind = Kind::Trusted { laptop: laptop.id.clone(), name: laptop.name.clone(), started: now, last: now, authenticated: false };
        let session = Session { id: hex(&random::<16>()), key, kind };
        let exchange = Exchange { session: session.id.clone(), message: hex(&ours), confirm: confirmation(&session.key), grant: None };
        match holder {
            Some(_) => state.pending = Some(session),
            None => *state = State { session: Some(session), ..State::default() },
        }
        Ok(exchange)
    }

    /// A request in session authenticated: if it is the trusted session,
    /// it lasts TRUSTED_IDLE from now; if the pending exchange's, that takes
    /// the session's place. Its laptop's name, and whether this is its first
    /// authenticated request.
    pub fn touch(&self, session: &str, now: Instant) -> Option<(String, bool)> {
        let mut state = self.state();
        if state.pending(now).is_some_and(|pending| pending.id == session) {
            state.session = state.pending.take();
        }
        if !state.live(now) {
            return None;
        }
        let Some(Session { id, kind: Kind::Trusted { name, last, authenticated, .. }, .. }) = state.session.as_mut() else {
            return None;
        };
        if id != session {
            return None;
        }
        *last = (*last).max(now);
        let first = !std::mem::replace(authenticated, true);
        Some((name.clone(), first))
    }

    /// The grant of session, while the window is timely and it is the
    /// current one, once a request in it authenticates: the window grants
    /// no more.
    pub fn take_grant(&self, session: &str, now: Instant) -> Option<Grant> {
        let mut state = self.state();
        if !state.timely(now) {
            return None;
        }
        let grant = match state.session.as_mut() {
            Some(Session { id, kind: Kind::Code { grant }, .. }) if id == session => grant.take()?,
            _ => return None,
        };
        state.trust = None;
        state.granted = Some(session.to_string());
        Some(grant)
    }

    /// The key of session while it lasts (a code's while the window is
    /// timely), if it is the latest exchange's: a later exchange replaces
    /// it. Its id is no secret; only its client holds the key, which
    /// authenticates what it sends.
    pub fn session(&self, session: &str, now: Instant) -> Option<Vec<u8>> {
        let state = self.state();
        let current = state.session.as_ref().filter(|s| s.id == session && state.live(now));
        current.or(state.pending(now).filter(|s| s.id == session)).map(|s| s.key.clone())
    }

    /// Ends session at its client's word: a pending exchange is left in
    /// its place, still to prove itself; a window stays open to exchanges,
    /// unless session took its grant: its laptop is trusted, and the window
    /// closes with it. Whether it was the session or the pending exchange.
    pub fn close(&self, session: &str) -> bool {
        let mut state = self.state();
        if state.pending.as_ref().is_some_and(|s| s.id == session) {
            state.pending = None;
            return true;
        }
        if state.session.as_ref().is_some_and(|s| s.id == session) {
            match state.granted.as_deref() == Some(session) {
                true => *state = State::default(),
                false => state.session = state.pending.take(),
            }
            return true;
        }
        false
    }

    /// Ends the trusted session and its pending exchange at a Cancel on the
    /// thermostat; a request in either is then cancelled. Whether there was
    /// one to end.
    pub fn cancel(&self) -> bool {
        self.end_trusted(|_| true)
    }

    /// Ends the trusted session and pending exchange of laptop, once the
    /// unit stops trusting it, as cancel does. Whether one ended, and
    /// whether the laptop's session spent the window on an apply (spend).
    pub fn forget(&self, laptop: &str) -> (bool, bool) {
        let ended = self.end_trusted(|of| of == laptop);
        (ended, self.state().applying.as_deref() == Some(laptop))
    }

    /// Ends the trusted session and pending exchange whose laptop ends
    /// picks; a request in either is then cancelled. Whether one ended.
    fn end_trusted(&self, ends: impl Fn(&str) -> bool) -> bool {
        let mut state = self.state();
        let picked = |session: &Option<Session>| {
            matches!(session, Some(Session { kind: Kind::Trusted { laptop, .. }, .. }) if ends(laptop))
        };
        let mut ended = Vec::new();
        if picked(&state.session) {
            ended.extend(state.session.take().map(|session| session.id));
        }
        if picked(&state.pending) {
            ended.extend(state.pending.take().map(|session| session.id));
        }
        let any = !ended.is_empty();
        state.cancelled.extend(ended);
        any
    }

    /// Whether Cancel on the thermostat ended session.
    pub fn cancelled(&self, session: &str) -> bool {
        self.state().cancelled.iter().any(|id| id == session)
    }

    /// Ends session for an apply it authenticated, and closes the window:
    /// its code is spent; a trusted session's laptop is kept as applying
    /// (forget). Until when the apply's upload may run: the window's lapse,
    /// or the trusted session's OPEN_FOR. None, and nothing
    /// changes, unless session is the current one.
    pub fn spend(&self, session: &str, now: Instant) -> Option<Instant> {
        let mut state = self.state();
        let current = state.live(now) && state.session.as_ref().is_some_and(|s| s.id == session);
        if !current {
            return None;
        }
        let (until, applying) = match state.session.as_ref().map(|s| &s.kind) {
            Some(Kind::Trusted { started, laptop, .. }) => (*started + OPEN_FOR, Some(laptop.clone())),
            _ => (state.session_ends()?, None),
        };
        *state = State { spent: true, applying, ..State::default() };
        Some(until)
    }

    /// Whether an apply closed the window, rather than its lapsing.
    pub fn spent(&self) -> bool {
        self.state().spent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use update_wire::auth::{verify, CONFIRMATION};
    use update_wire::unhex;

    /// The client's half: the session key and whether the unit confirmed
    /// it, and its grant if any.
    fn exchange(w: &Window, code: &str, t: Instant) -> (Exchange, Vec<u8>, bool) {
        let (client, message) = client(code);
        let answer = w.exchange(&message, "laptop", &Granting::Add, t).unwrap();
        let key = client.finish(&unhex(&answer.message).unwrap()).unwrap();
        let confirmed = match &answer.grant {
            Some((unit, laptop)) => verify(&key, &format!("{CONFIRMATION}\n{unit}\n{laptop}"), &answer.confirm),
            None => verify(&key, CONFIRMATION, &answer.confirm),
        };
        (answer, key, confirmed)
    }

    fn other(code: &str) -> String {
        format!("{:08}", (code.parse::<u32>().unwrap() + 1) % 100_000_000)
    }

    #[test]
    fn keyed_values_match_the_shared_test_vector() {
        let key: Vec<u8> = (0..32).collect();
        assert_eq!(confirmation(&key), "4653a539f87f1a0685eb47d8a0c86be186d5812eabf3d1b8fc734b4c32e33453");
        assert_eq!(grant_confirmation(&key, &"00".repeat(16), &"11".repeat(16), None),
                   "58c264233497b21be9737097b52339e0e9bb49c2e241e2e7b4cad2e46384fc83");
        assert_eq!(grant_confirmation(&key, &"00".repeat(16), &"11".repeat(16), Some(&"22".repeat(16))),
                   "63f9da01a33c95dc3d206c46e26b42739ece99dfdfe46598d8a4beb1f71a777f");
        assert_eq!((CLIENT, UNIT), (&b"unfr0zen"[..], &b"nestlocal"[..]));
    }

    #[test]
    fn a_window_gives_eight_digits_and_expires() {
        let (w, t) = (Window::new(), Instant::now());
        assert_eq!(w.exchange(&client("12345678").1, "laptop", &Granting::Add, t).unwrap_err(), ExchangeError::Closed, "closed until opened");
        let code = w.open(t);
        assert!(code.len() == 8 && code.bytes().all(|b| b.is_ascii_digit()));
        assert!(w.is_open(t + OPEN_FOR - Duration::from_secs(1)));
        assert!(!w.is_open(t + OPEN_FOR));
        assert_eq!(w.closes_at(t + Duration::from_secs(1)), Some(t + OPEN_FOR));
        assert_eq!(w.closes_at(t + OPEN_FOR), None);
    }

    #[test]
    fn spend_closes_the_window() {
        let (w, t) = (Window::new(), Instant::now());
        let code = w.open(t);
        let (answer, key, confirmed) = exchange(&w, &code, t);
        assert!(confirmed);
        let held = w.session(&answer.session, t).unwrap();
        assert_eq!(held, key, "the session holds the client's key");
        assert!(w.spend("unknown", t).is_none(), "only the current session spends the window");
        assert!(w.is_open(t) && !w.spent());
        assert_eq!(w.spend(&answer.session, t), Some(t + OPEN_FOR));
        assert!(!w.is_open(t) && w.spent());
        assert_eq!(w.session(&answer.session, t), None, "replay");
        assert!(w.spend(&answer.session, t).is_none());
        assert_eq!(w.exchange(&client(&code).1, "laptop", &Granting::Add, t).unwrap_err(), ExchangeError::Closed);
    }

    #[test]
    fn a_wrong_code_is_not_confirmed_and_keys_another_session() {
        let (w, t) = (Window::new(), Instant::now());
        let code = w.open(t);
        let (answer, key, confirmed) = exchange(&w, &other(&code), t);
        assert!(!confirmed);
        assert_ne!(w.session(&answer.session, t), Some(key), "the client's key is not the session's");
        assert!(w.is_open(t) && !w.spent());
    }

    #[test]
    fn a_later_exchange_makes_a_session_stale() {
        let (w, t) = (Window::new(), Instant::now());
        let code = w.open(t);
        let (first, _, _) = exchange(&w, &code, t);
        let (second, key, _) = exchange(&w, &code, t);
        assert_eq!(w.session(&first.session, t), None);
        assert!(w.spend(&first.session, t).is_none());
        assert_eq!(w.session(&second.session, t), Some(key));
        assert!(w.is_open(t), "a stale session leaves the window open");
    }

    #[test]
    fn a_session_lasts_while_the_window_is_timely() {
        let (w, t) = (Window::new(), Instant::now());
        let code = w.open(t);
        let (answer, key, _) = exchange(&w, &code, t);
        let at = |s: u64| t + Duration::from_secs(s);
        assert_eq!(w.session(&answer.session, at(59)), Some(key.clone()));
        assert_eq!(w.session(&answer.session, at(590)), Some(key));
        assert_eq!(w.session(&answer.session, at(601)), None);
        assert!(w.spend(&answer.session, at(601)).is_none());
        assert!(!w.spent(), "a lapsed window is not spent");
    }

    #[test]
    fn a_window_takes_max_attempts_exchanges() {
        let (w, t) = (Window::new(), Instant::now());
        let code = w.open(t);
        for _ in 1..MAX_ATTEMPTS {
            assert!(!exchange(&w, &other(&code), t).2);
        }
        // The last exchange's session may still read and apply.
        let (answer, _, confirmed) = exchange(&w, &code, t);
        assert!(confirmed && w.is_open(t) && !w.accepting(t), "open to the session alone");
        assert_eq!(w.exchange(&client(&code).1, "laptop", &Granting::Add, t).unwrap_err(), ExchangeError::Closed);
        assert!(w.spend(&answer.session, t).is_some());
        // A last exchange without its apply keeps the window until it lapses.
        let code = w.open(t);
        for _ in 0..MAX_ATTEMPTS {
            exchange(&w, &other(&code), t);
        }
        assert!(w.is_open(t + OPEN_FOR - Duration::from_secs(1)) && !w.is_open(t + OPEN_FOR));
    }

    #[test]
    fn a_window_with_trust_grants_each_exchange_until_a_session_takes_one() {
        let (w, t) = (Window::new(), Instant::now());
        let unit = "ab".repeat(16);
        let code = w.open_trusting(unit.clone(), None, t);
        let (first, first_key, confirmed) = exchange(&w, &code, t);
        let (granted_unit, laptop) = first.grant.clone().unwrap();
        assert!(confirmed && granted_unit == unit);
        // Another exchange replaces the session and its grant.
        let (second, key, confirmed) = exchange(&w, &code, t);
        assert!(confirmed && second.grant.as_ref().is_some_and(|(_, other)| *other != laptop));
        assert!(w.take_grant(&first.session, t).is_none(), "a replaced session's");
        assert_ne!(first_key, key);
        let grant = w.take_grant(&second.session, t).unwrap();
        assert_eq!((grant.unit.as_str(), grant.name.as_str()), (unit.as_str(), "laptop"), "the exchange's name");
        assert_eq!(grant.key, auth::trust_key(&key, &unit, &grant.laptop));
        assert!(w.take_grant(&second.session, t).is_none(), "taken once");
        let (third, _, confirmed) = exchange(&w, &code, t);
        assert!(confirmed && third.grant.is_none(), "the window grants no more");
        // A window opened from a prompt names the prompt's laptop; one
        // without trust grants none.
        let code = w.open_trusting(unit.clone(), Some("prompted".into()), t);
        let (answer, _, _) = exchange(&w, &code, t);
        assert_eq!(w.take_grant(&answer.session, t).unwrap().name, "prompted");
        let code = w.open(t);
        let (answer, _, _) = exchange(&w, &code, t);
        assert!(answer.grant.is_none() && w.take_grant(&answer.session, t).is_none());
        // Nor once the window lapses.
        let code = w.open_trusting(unit, None, t);
        let (answer, _, _) = exchange(&w, &code, t);
        assert!(w.take_grant(&answer.session, t + OPEN_FOR).is_none());
    }

    #[test]
    fn a_grant_replaces_or_is_withheld_as_granting_says() {
        let (w, t) = (Window::new(), Instant::now());
        let (unit, old) = ("ab".repeat(16), "cd".repeat(16));
        let code = w.open_trusting(unit, None, t);
        let (spake, message) = client(&code);
        let answer = w.exchange(&message, "laptop", &Granting::Replace(old.clone()), t).unwrap();
        let key = spake.finish(&unhex(&answer.message).unwrap()).unwrap();
        let (granted_unit, laptop) = answer.grant.clone().unwrap();
        assert_eq!(answer.confirm, grant_confirmation(&key, &granted_unit, &laptop, Some(&old)), "the confirmation names the replaced");
        assert_ne!(answer.confirm, grant_confirmation(&key, &granted_unit, &laptop, None));
        let grant = w.take_grant(&answer.session, t).unwrap();
        assert_eq!(grant.replaces, Some(old.clone()));
        assert_ne!(grant.laptop, old, "a fresh laptop id");
        let code = w.open_trusting("ab".repeat(16), None, t);
        let answer = w.exchange(&client(&code).1, "laptop", &Granting::Withheld, t).unwrap();
        assert!(answer.grant.is_none() && w.take_grant(&answer.session, t).is_none());
        let answer = w.exchange(&client(&code).1, "laptop", &Granting::Add, t).unwrap();
        assert!(answer.grant.is_some() && w.take_grant(&answer.session, t).unwrap().replaces.is_none());
    }

    #[test]
    fn a_window_grants_while_it_accepts_with_trust() {
        let (w, t) = (Window::new(), Instant::now());
        assert!(!w.grants(t));
        w.open(t);
        assert!(!w.grants(t), "no trust");
        let code = w.open_trusting("ab".repeat(16), None, t);
        assert!(w.grants(t) && !w.grants(t + OPEN_FOR));
        let answer = w.exchange(&client(&code).1, "laptop", &Granting::Add, t).unwrap();
        w.take_grant(&answer.session, t).unwrap();
        assert!(!w.grants(t), "its grant taken");
    }

    fn laptop(n: u8) -> Laptop {
        Laptop { id: hex(&[n; 16]), name: format!("laptop {n}"), key: [n; 32], added: String::new() }
    }

    /// The client's half of a trusted exchange for laptop under key: the
    /// session key and whether the unit confirmed it.
    fn trusted(w: &Window, laptop: &Laptop, key: &[u8; 32], t: Instant) -> Result<(Exchange, Vec<u8>, bool), ExchangeError> {
        let (client, message) = trusted_client(key);
        let answer = w.exchange_trusted(laptop, &message, t)?;
        let session_key = client.finish(&unhex(&answer.message).unwrap()).unwrap();
        let confirmed = verify(&session_key, CONFIRMATION, &answer.confirm);
        Ok((answer, session_key, confirmed))
    }

    #[test]
    fn a_trusted_session_needs_no_window_and_ends_idle_after_a_minute() {
        let (w, t) = (Window::new(), Instant::now());
        let at = |s: u64| t + Duration::from_secs(s);
        let desk = laptop(1);
        let (answer, key, confirmed) = trusted(&w, &desk, &desk.key, t).unwrap();
        assert!(confirmed && answer.grant.is_none());
        assert!(!w.accepting(t), "no window opens");
        assert_eq!(w.session(&answer.session, t), Some(key.clone()));
        assert_eq!(w.touch(&answer.session, at(5)), Some(("laptop 1".into(), true)));
        assert!(w.is_open(at(5)), "an authenticated session holds the unit");
        assert_eq!(w.closes_at(at(5)), Some(at(5) + TRUSTED_IDLE));
        // Each authenticated request moves the idle limit on.
        assert_eq!(w.touch(&answer.session, at(60)), Some(("laptop 1".into(), false)));
        assert_eq!(w.session(&answer.session, at(119)), Some(key));
        assert_eq!(w.session(&answer.session, at(120)), None, "idle for a minute");
        assert!(w.touch(&answer.session, at(120)).is_none());
        assert!(!w.is_open(at(120)) && w.closes_at(at(120)).is_none());
        // However busy, it lasts OPEN_FOR.
        let (answer, key, _) = trusted(&w, &desk, &desk.key, t).unwrap();
        for s in (5..600).step_by(50) {
            assert!(w.touch(&answer.session, at(s)).is_some(), "{s} s in");
        }
        assert_eq!(w.closes_at(at(595)), Some(t + OPEN_FOR));
        assert_eq!(w.session(&answer.session, at(599)), Some(key));
        assert_eq!(w.session(&answer.session, t + OPEN_FOR), None);
        // Another key keys a session its client cannot use.
        let (answer, key, confirmed) = trusted(&w, &desk, &[9; 32], t).unwrap();
        assert!(!confirmed);
        assert_ne!(w.session(&answer.session, t), Some(key));
        // An applied set spends it, and it grants nothing.
        let (answer, _, _) = trusted(&w, &desk, &desk.key, t).unwrap();
        w.touch(&answer.session, at(1));
        assert!(w.take_grant(&answer.session, at(1)).is_none());
        assert!(w.spend("unknown", at(1)).is_none());
        assert_eq!(w.spend(&answer.session, at(1)), Some(t + OPEN_FOR), "the upload may run to its hard limit");
        assert!(w.session(&answer.session, at(1)).is_none() && !w.is_open(at(1)) && w.spent());
    }

    #[test]
    fn cancel_ends_a_trusted_session_and_its_pending_exchange_until_the_next() {
        let (w, t) = (Window::new(), Instant::now());
        let desk = laptop(1);
        let code = w.open(t);
        let (keyed, _, _) = exchange(&w, &code, t);
        assert!(!w.cancel() && !w.cancelled(&keyed.session), "a code's session has no notice to cancel it");
        assert!(w.session(&keyed.session, t).is_some());
        w.open(t.checked_sub(OPEN_FOR).unwrap());
        let (held, _, _) = trusted(&w, &desk, &desk.key, t).unwrap();
        w.touch(&held.session, t);
        let (pending, _, _) = trusted(&w, &desk, &desk.key, t).unwrap();
        assert!(w.cancel());
        assert!(w.cancelled(&held.session) && w.cancelled(&pending.session));
        assert!(w.session(&held.session, t).is_none() && w.session(&pending.session, t).is_none());
        assert!(!w.holds_trusted(t) && w.closes_at(t).is_none());
        assert!(!w.cancel(), "nothing left to end");
        // Cancel never forgets the laptop: its next exchange starts afresh.
        let (next, _, _) = trusted(&w, &desk, &desk.key, t).unwrap();
        assert!(!w.cancelled(&held.session) && w.session(&next.session, t).is_some());
    }

    #[test]
    fn an_unauthenticated_trusted_session_is_dropped_after_ten_seconds() {
        let (w, t) = (Window::new(), Instant::now());
        let (desk, other) = (laptop(1), laptop(2));
        let (answer, key, _) = trusted(&w, &desk, &desk.key, t).unwrap();
        assert!(!w.is_open(t), "it holds nothing until it proves itself");
        assert_eq!(w.closes_at(t), Some(t + PROVE_WITHIN));
        assert_eq!(w.session(&answer.session, t + PROVE_WITHIN - Duration::from_secs(1)), Some(key));
        assert_eq!(w.session(&answer.session, t + PROVE_WITHIN), None);
        assert!(w.touch(&answer.session, t + PROVE_WITHIN).is_none());
        assert!(w.closes_at(t + PROVE_WITHIN).is_none());
        // Meanwhile another laptop's exchange replaces it.
        let (first, _, _) = trusted(&w, &desk, &desk.key, t).unwrap();
        let (second, key, _) = trusted(&w, &other, &other.key, t).unwrap();
        assert!(w.session(&first.session, t).is_none());
        assert_eq!(w.session(&second.session, t), Some(key));
        assert_eq!(w.exchange_trusted(&other, b"not spake2", t).unwrap_err(), ExchangeError::Malformed);
        assert!(w.session(&second.session, t).is_some(), "a malformed exchange replaces nothing");
    }

    #[test]
    fn a_trusted_exchange_while_a_window_accepts_is_busy() {
        let (w, t) = (Window::new(), Instant::now());
        let (desk, kitchen) = (laptop(1), laptop(2));
        let code = w.open(t);
        assert_eq!(trusted(&w, &desk, &desk.key, t).unwrap_err(), ExchangeError::Busy);
        assert!(w.accepting(t), "the window stands");
        assert!(exchange(&w, &code, t).2);
        // A window with no exchange left gives way.
        for _ in 1..MAX_ATTEMPTS {
            exchange(&w, &other(&code), t);
        }
        assert!(!w.accepting(t) && w.is_open(t));
        let (answer, _, _) = trusted(&w, &desk, &desk.key, t).unwrap();
        assert!(!w.is_open(t) && !w.accepting(t), "the window is gone");
        // Another laptop's authenticated session holds the unit; its own
        // laptop's next exchange replaces it.
        w.touch(&answer.session, t);
        assert_eq!(trusted(&w, &kitchen, &kitchen.key, t).unwrap_err(), ExchangeError::Busy);
        assert!(w.session(&answer.session, t).is_some(), "the first is unaffected");
        let (again, _, _) = trusted(&w, &desk, &desk.key, t).unwrap();
        assert!(w.session(&answer.session, t).is_some() && w.session(&again.session, t).is_some());
        assert_eq!(w.touch(&again.session, t), Some(("laptop 1".into(), true)), "once it proves itself");
        assert!(w.session(&answer.session, t).is_none() && w.holds_trusted(t));
        // A code exchange finds no window.
        assert_eq!(w.exchange(&client(&code).1, "laptop", &Granting::Add, t).unwrap_err(), ExchangeError::Closed);
    }

    #[test]
    fn an_unproven_exchange_cannot_end_a_live_trusted_session() {
        let (w, t) = (Window::new(), Instant::now());
        let at = |s: u64| t + Duration::from_secs(s);
        let desk = laptop(1);
        let (held, key, _) = trusted(&w, &desk, &desk.key, t).unwrap();
        w.touch(&held.session, t);
        // Exchanges in the laptop's name, under a key nobody else holds.
        for s in 1..=3 {
            let (forged, _, confirmed) = trusted(&w, &desk, &[9; 32], at(s)).unwrap();
            assert!(!confirmed);
            assert_eq!(w.session(&held.session, at(s)), Some(key.clone()));
            assert!(w.touch(&held.session, at(s)).is_some() && w.holds_trusted(at(s)));
            assert_ne!(w.session(&forged.session, at(s)), None, "pending beside it");
        }
        // Unproven, a pending exchange lapses and leaves the session.
        let (pending, _, _) = trusted(&w, &desk, &desk.key, at(4)).unwrap();
        assert!(w.session(&pending.session, at(4) + PROVE_WITHIN).is_none());
        assert!(w.touch(&pending.session, at(4) + PROVE_WITHIN).is_none());
        assert_eq!(w.session(&held.session, at(20)), Some(key));
        assert_eq!(w.closes_at(at(20)), Some(at(3) + TRUSTED_IDLE));
        // Closed, the session gives its place to a pending exchange, which
        // holds nothing until it proves itself.
        let (pending, _, _) = trusted(&w, &desk, &desk.key, at(21)).unwrap();
        assert!(w.close(&held.session) && !w.close(&held.session));
        assert!(w.session(&held.session, at(21)).is_none() && !w.is_open(at(21)));
        assert_eq!(w.touch(&pending.session, at(22)), Some(("laptop 1".into(), true)));
        assert!(w.close(&pending.session) && w.closes_at(at(22)).is_none());
    }

    #[test]
    fn closing_a_code_session_leaves_the_window_open() {
        let (w, t) = (Window::new(), Instant::now());
        let code = w.open(t);
        let (answer, _, _) = exchange(&w, &code, t);
        assert!(w.close(&answer.session));
        assert!(w.session(&answer.session, t).is_none() && w.accepting(t));
        assert!(!w.close("unknown"));
    }

    #[test]
    fn closing_the_session_that_took_a_grant_closes_the_window() {
        let (w, t) = (Window::new(), Instant::now());
        let code = w.open_trusting("ab".repeat(16), None, t);
        // A session yet to take its grant leaves the window open.
        let (answer, _, _) = exchange(&w, &code, t);
        assert!(w.close(&answer.session) && w.accepting(t));
        let (answer, _, _) = exchange(&w, &code, t);
        assert!(w.take_grant(&answer.session, t).is_some());
        assert!(w.close(&answer.session));
        assert!(!w.accepting(t) && !w.is_open(t) && w.closes_at(t).is_none() && !w.spent());
        // The laptop it trusted finds nothing in its way.
        let desk = laptop(1);
        let (answer, _, confirmed) = trusted(&w, &desk, &desk.key, t).unwrap();
        assert!(confirmed && w.session(&answer.session, t).is_some());
    }

    #[test]
    fn a_malformed_message_counts_as_an_attempt() {
        let (w, t) = (Window::new(), Instant::now());
        w.open(t);
        for _ in 0..MAX_ATTEMPTS {
            assert_eq!(w.exchange(b"not spake2", "laptop", &Granting::Add, t).unwrap_err(), ExchangeError::Malformed);
        }
        assert!(!w.is_open(t));
    }
}
