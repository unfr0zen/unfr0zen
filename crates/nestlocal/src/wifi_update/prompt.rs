//! The prompt the network raises before a window opens: the thermostat asks
//! "Update from <name> (<ip>)?" full screen, and a press answers it. Either
//! Allow opens the window and shows its code, unless an update or a session
//! holds the window: then it is answered Busy. Deny, and no answer within
//! ASK_FOR, count as a strike. STRIKES in a row stop the network asking until
//! Allow update is pressed on Settings → UPDATE or the unit reboots. Each
//! prompt has an id of its own, so that a client takes only its own
//! prompt's answer. A trusted session that takes the screen before a press
//! answers it Preempted.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::handover::Unavailable;
use crate::settings_ui;

/// How long a prompt waits for a press before it counts as Deny.
pub const ASK_FOR: Duration = Duration::from_secs(60);
/// How long GET /update reports a prompt's answer once given.
pub const REPORT_FOR: Duration = Duration::from_secs(60);
/// Denied or unanswered prompts in a row after which the network asks no more.
pub const STRIKES: u32 = 3;
/// Where the unit counts them: /var/run outlives nestlocal's restarts, so a
/// crash does not give the network its prompts back; a reboot does.
pub const STRIKES_FILE: &str = "/var/run/nestlocal-update.prompt-strikes";

/// Whether the screen may be taken (handover::available), as the prompt asks
/// it of the hand-over files in a directory.
pub type Handover = fn(&Path) -> Result<(), Unavailable>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Allowed,
    /// Allow and trust: opens the window as Allowed does.
    AllowedTrust,
    Denied,
    TimedOut,
    /// Allowed while an update or a session held the window: none opened.
    Busy,
    /// A trusted session took the screen before anyone answered.
    Preempted,
}

impl Answer {
    /// As GET /update reports it.
    fn name(self) -> &'static str {
        match self {
            Answer::Allowed | Answer::AllowedTrust => "allowed",
            Answer::Denied => "denied",
            Answer::TimedOut => "timed_out",
            Answer::Busy => "busy",
            Answer::Preempted => "preempted",
        }
    }
}

/// Why a prompt is not raised.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Another prompt is up, or an update or a session holds the window.
    Busy,
    /// STRIKES prompts in a row went denied or unanswered.
    Strikes,
    /// The screen cannot be taken.
    Screen(Unavailable),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    /// Up since since; trust is whether it offers Allow and trust: not
    /// while the trusted list is full, unless the laptop asking names one
    /// in it (update::raise).
    Asking { id: String, name: String, ip: String, since: Instant, trust: bool },
    /// The last prompt's answer, and when it was given.
    Answered { id: String, answer: Answer, at: Instant },
}

struct Inner {
    state: State,
    strikes: u32,
}

pub struct Prompt {
    inner: Mutex<Inner>,
    /// Where the strikes are kept, if anywhere (STRIKES_FILE).
    file: Option<PathBuf>,
    handover: Handover,
}

impl Prompt {
    /// A prompt that keeps its strikes in path, and starts from what it
    /// holds: none when it is missing, STRIKES when it cannot be read.
    pub fn remembering(path: PathBuf) -> Prompt {
        Prompt::with(Some(path), crate::handover::available)
    }

    /// A prompt that keeps its strikes in memory alone.
    #[cfg(test)]
    pub fn new(handover: Handover) -> Prompt {
        Prompt::with(None, handover)
    }

    fn with(file: Option<PathBuf>, handover: Handover) -> Prompt {
        let strikes = match &file {
            None => 0,
            Some(path) => match std::fs::read_to_string(path) {
                Ok(text) => text.trim().parse().unwrap_or(STRIKES),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(_) => STRIKES,
            },
        };
        Prompt { inner: Mutex::new(Inner { state: State::Idle, strikes }), file, handover }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_strikes(&self, inner: &mut Inner, strikes: u32) {
        if inner.strikes == strikes {
            return;
        }
        inner.strikes = strikes;
        if let Some(path) = &self.file {
            if let Err(e) = std::fs::write(path, format!("{strikes}\n")) {
                crate::logf!("wifi-update: {}: {e}", path.display());
            }
        }
    }

    /// Raises a prompt for the laptop name at ip, offering Allow and trust
    /// when trust, unless one is up, the network has struck out, or the
    /// screen cannot be taken; its id.
    pub fn ask(&self, name: String, ip: String, trust: bool, now: Instant) -> Result<String, Refusal> {
        let mut inner = self.inner();
        if matches!(inner.state, State::Asking { .. }) {
            return Err(Refusal::Busy);
        }
        if inner.strikes >= STRIKES {
            return Err(Refusal::Strikes);
        }
        self.screen().map_err(Refusal::Screen)?;
        let id = update_wire::hex(&update_wire::random::<16>());
        inner.state = State::Asking { id: id.clone(), name, ip, since: now, trust };
        Ok(id)
    }

    /// Answers the prompt up: either Allow clears the strikes, Deny and a
    /// timeout add one, Busy and Preempted leave them; Allow and trust on a
    /// prompt that does not offer it is Allow. The answer given and the
    /// laptop's name; none, and nothing changes, when no prompt is up.
    pub fn answer(&self, answer: Answer, now: Instant) -> Option<(Answer, String)> {
        let mut inner = self.inner();
        let State::Asking { id, name, ip, trust, .. } = &inner.state else { return None };
        let answer = if answer == Answer::AllowedTrust && !trust { Answer::Allowed } else { answer };
        crate::logf!("wifi-update: the prompt for {name} ({ip}): {}", answer.name());
        let (id, name) = (id.clone(), name.clone());
        inner.state = State::Answered { id, answer, at: now };
        let strikes = match answer {
            Answer::Allowed | Answer::AllowedTrust => 0,
            Answer::Denied | Answer::TimedOut => inner.strikes + 1,
            Answer::Busy | Answer::Preempted => inner.strikes,
        };
        self.set_strikes(&mut inner, strikes);
        Some((answer, name))
    }

    /// Whether a prompt is up.
    pub fn asking(&self) -> bool {
        matches!(self.inner().state, State::Asking { .. })
    }

    /// Whether the screen may be taken, as a prompt would take it: a trusted
    /// session's notice needs it too.
    pub fn screen(&self) -> Result<(), Unavailable> {
        (self.handover)(settings_ui::dir())
    }

    /// Whether the prompt up has waited ASK_FOR for an answer.
    pub fn due(&self, now: Instant) -> bool {
        matches!(self.inner().state, State::Asking { since, .. } if now.saturating_duration_since(since) >= ASK_FOR)
    }

    /// Lets the network ask again, as the screen's opening a window does.
    pub fn clear_strikes(&self) {
        let mut inner = self.inner();
        self.set_strikes(&mut inner, 0);
    }

    /// What GET /update reports: the prompt's id, and "asking" while it is
    /// up, then its answer for REPORT_FOR; none otherwise.
    pub fn status(&self, now: Instant) -> Option<(String, &'static str)> {
        match &self.inner().state {
            State::Idle => None,
            State::Asking { id, .. } => Some((id.clone(), "asking")),
            State::Answered { id, answer, at } => {
                (now.saturating_duration_since(*at) < REPORT_FOR).then(|| (id.clone(), answer.name()))
            }
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> State {
        self.inner().state.clone()
    }

    #[cfg(test)]
    pub fn strikes(&self) -> u32 {
        self.inner().strikes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use std::sync::atomic::Ordering;

    use crate::settings_ui;
    use crate::wifi_update::display::{Action, Display, View};
    use crate::wifi_update::trust::Store;
    use crate::wifi_update::window::{self, Window};
    use crate::wifi_update::{act, poll, raise, Raised, Shared, BUSY};

    fn handover(_: &Path) -> Result<(), Unavailable> {
        Ok(())
    }

    fn ask(prompt: &Prompt, now: Instant) -> Result<(), Refusal> {
        prompt.ask("laptop".into(), "192.168.1.20".into(), true, now).map(drop)
    }

    /// The state GET /update reports, without its id.
    fn reported(prompt: &Prompt, now: Instant) -> Option<&'static str> {
        prompt.status(now).map(|(_, state)| state)
    }

    fn raised(shared: &Shared, now: Instant) -> bool {
        matches!(raise(shared, "laptop".into(), "192.168.1.20".into(), None, now), Ok(Raised::Asking(_)))
    }

    fn strike(prompt: &Prompt, now: Instant) {
        ask(prompt, now).unwrap();
        assert!(prompt.answer(Answer::Denied, now).is_some());
    }

    fn shared() -> (Shared, std::sync::mpsc::Receiver<Option<View>>) {
        let (display, shown) = Display::recording();
        let shared = Shared { window: Arc::new(Window::new()), display: Arc::new(display), prompt: Arc::new(Prompt::new(handover)),
                              trusted: Arc::new(std::sync::Mutex::new(Store::scratch())), nudge: None };
        (shared, shown)
    }

    #[test]
    fn allow_opens_the_window_and_shows_the_code() {
        crate::wifi_update::tests::run(async {
            let now = Instant::now();
            for (action, answer) in [(Action::Allow, Answer::Allowed), (Action::AllowTrust, Answer::AllowedTrust)] {
                let (shared, shown) = shared();
                assert!(raised(&shared, now));
                let prompt = View::Prompt { name: "laptop".into(), ip: "192.168.1.20".into(), trust: true };
                assert_eq!(shown.try_recv(), Ok(Some(prompt)));
                assert_eq!(reported(&shared.prompt, now), Some("asking"));
                assert!(!shared.window.accepting(now));
                act(&shared, "lo", action, now);
                assert!(shared.window.accepting(now));
                let Ok(Some(View::Code { code, .. })) = shown.try_recv() else { panic!("no code shown") };
                let page = std::fs::read_to_string(settings_ui::dir().join("update.code")).unwrap();
                assert_eq!(page.lines().next(), Some(code.as_str()), "the Update page has it too");
                assert!(matches!(shared.prompt.state(), State::Answered { answer: a, at, .. } if a == answer && at == now));
                assert_eq!(reported(&shared.prompt, now), Some("allowed"));
                // A late press on the prompt gone changes nothing.
                act(&shared, "lo", Action::Deny, now);
                assert!(shown.try_recv().is_err());
                assert_eq!(reported(&shared.prompt, now), Some("allowed"));
            }
        });
    }

    #[test]
    fn deny_and_timeout_each_strike() {
        crate::wifi_update::tests::run(async {
            let now = Instant::now();
            let (shared, shown) = shared();
            assert!(raised(&shared, now));
            shown.try_recv().unwrap();
            act(&shared, "lo", Action::Deny, now);
            assert_eq!(shown.try_recv(), Ok(None), "the screen is given back");
            assert!(!shared.window.accepting(now));
            assert_eq!((reported(&shared.prompt, now), shared.prompt.strikes()), (Some("denied"), 1));
            // The answer is reported for a minute.
            assert_eq!(reported(&shared.prompt, now + REPORT_FOR - Duration::from_secs(1)), Some("denied"));
            assert_eq!(reported(&shared.prompt, now + REPORT_FOR), None);
            assert!(raised(&shared, now));
            shown.try_recv().unwrap();
            poll(&shared, "lo", now + ASK_FOR - Duration::from_secs(1));
            assert_eq!(reported(&shared.prompt, now), Some("asking"));
            poll(&shared, "lo", now + ASK_FOR);
            assert_eq!(shown.try_recv(), Ok(None));
            assert_eq!((reported(&shared.prompt, now + ASK_FOR), shared.prompt.strikes()), (Some("timed_out"), 2));
        });
    }

    #[test]
    fn three_strikes_refuse_until_the_screen_opens_a_window() {
        crate::wifi_update::tests::run(async {
            let now = Instant::now();
            let (shared, _shown) = shared();
            for _ in 0..STRIKES {
                strike(&shared.prompt, now);
            }
            assert_eq!(ask(&shared.prompt, now), Err(Refusal::Strikes));
            assert_eq!(reported(&shared.prompt, now), Some("denied"), "a refusal leaves the last answer");
            settings_ui::publish("update.request", b"").unwrap();
            poll(&shared, "lo", now);
            assert!(shared.window.accepting(now));
            assert_eq!(ask(&shared.prompt, now), Ok(()));
            // An Allow clears them too.
            let prompt = Prompt::new(handover);
            for _ in 1..STRIKES {
                strike(&prompt, now);
            }
            ask(&prompt, now).unwrap();
            prompt.answer(Answer::Allowed, now);
            assert_eq!(prompt.strikes(), 0);
        });
    }

    #[test]
    fn strikes_survive_a_restart_through_the_file() {
        let path = std::env::temp_dir().join(format!("nestlocal-prompt-strikes-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let now = Instant::now();
        let prompt = Prompt::with(Some(path.clone()), handover);
        for _ in 0..STRIKES {
            strike(&prompt, now);
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{STRIKES}\n"));
        let restarted = Prompt::with(Some(path.clone()), handover);
        assert_eq!(ask(&restarted, now), Err(Refusal::Strikes), "a restart gives none back");
        restarted.clear_strikes();
        assert_eq!(ask(&Prompt::with(Some(path.clone()), handover), now), Ok(()));
        std::fs::write(&path, "garbled").unwrap();
        assert_eq!(ask(&Prompt::with(Some(path.clone()), handover), now), Err(Refusal::Strikes), "unreadable counts as struck out");
        std::fs::remove_file(&path).unwrap();
        assert_eq!(ask(&Prompt::with(Some(path), handover), now), Ok(()), "none kept: a reboot cleared /var/run");
    }

    #[test]
    fn a_second_open_while_prompting_is_busy() {
        let now = Instant::now();
        let prompt = Prompt::new(handover);
        ask(&prompt, now).unwrap();
        let asking = prompt.state();
        assert_eq!(prompt.ask("other".into(), "192.168.1.30".into(), true, now), Err(Refusal::Busy));
        assert_eq!(prompt.ask("other".into(), "192.168.1.30".into(), true, now + ASK_FOR), Err(Refusal::Busy), "until it is answered");
        assert_eq!(prompt.state(), asking, "the first prompt stands");
        assert_eq!(prompt.strikes(), 0, "busy is no strike");
        assert!(prompt.answer(Answer::Allowed, now).is_some());
        assert_eq!(reported(&prompt, now), Some("allowed"));
    }

    #[test]
    fn each_prompt_has_an_id_of_its_own() {
        let now = Instant::now();
        let prompt = Prompt::new(handover);
        let first = prompt.ask("b".into(), "192.168.1.20".into(), true, now).unwrap();
        assert!(first.len() == 32 && first.bytes().all(|b| b.is_ascii_hexdigit()), "{first}");
        assert_eq!(prompt.status(now), Some((first.clone(), "asking")));
        prompt.answer(Answer::Denied, now);
        assert_eq!(prompt.status(now), Some((first.clone(), "denied")));
        // Another laptop's prompt replaces the answer: the first can tell.
        let second = prompt.ask("c".into(), "192.168.1.30".into(), true, now).unwrap();
        assert_ne!(second, first);
        assert_eq!(prompt.status(now), Some((second, "asking")));
    }

    #[test]
    fn an_open_while_an_update_or_a_session_holds_the_window_is_busy() {
        crate::wifi_update::tests::run(async {
            let now = Instant::now();
            // A session left in a window that takes no more exchanges.
            let (held, held_shown) = shared();
            let code = held.window.open(now);
            for _ in 0..window::MAX_ATTEMPTS {
                held.window.exchange(&window::client(&code).1, "laptop", &window::Granting::Add, now).unwrap();
            }
            assert!(!held.window.accepting(now) && held.window.is_open(now));
            assert_eq!(raise(&held, "laptop".into(), "192.168.1.20".into(), None, now), Err(Refusal::Busy));
            // An update under way.
            let (running, running_shown) = shared();
            BUSY.store(true, Ordering::SeqCst);
            assert_eq!(raise(&running, "laptop".into(), "192.168.1.20".into(), None, now), Err(Refusal::Busy));
            BUSY.store(false, Ordering::SeqCst);
            assert!(held_shown.try_recv().is_err() && running_shown.try_recv().is_err(), "no prompt shown");
            for prompt in [&held.prompt, &running.prompt] {
                assert_eq!((reported(prompt, now), prompt.strikes()), (None, 0));
            }
        });
    }

    #[test]
    fn an_allow_while_an_update_or_a_session_holds_the_window_opens_none() {
        crate::wifi_update::tests::run(async {
            let now = Instant::now();
            // An update under way.
            let (running, shown) = shared();
            strike(&running.prompt, now);
            assert!(raised(&running, now));
            shown.try_recv().unwrap();
            BUSY.store(true, Ordering::SeqCst);
            act(&running, "lo", Action::Allow, now);
            BUSY.store(false, Ordering::SeqCst);
            assert_eq!(shown.try_recv(), Ok(None), "the screen is given back");
            assert!(!running.window.is_open(now));
            assert_eq!((reported(&running.prompt, now), running.prompt.strikes()), (Some("busy"), 1), "no strike, none cleared");
            // A session the screen's window keyed meanwhile.
            let (held, shown) = shared();
            assert!(raised(&held, now));
            shown.try_recv().unwrap();
            let code = held.window.open(now);
            let mut last = None;
            for _ in 0..window::MAX_ATTEMPTS {
                last = Some(held.window.exchange(&window::client(&code).1, "laptop", &window::Granting::Add, now).unwrap().session);
            }
            act(&held, "lo", Action::AllowTrust, now);
            assert_eq!(shown.try_recv(), Ok(None));
            assert!(held.window.session(&last.unwrap(), now).is_some(), "the session's window stands");
            assert_eq!(reported(&held.prompt, now), Some("busy"));
        });
    }

    #[test]
    fn an_unavailable_screen_refuses_with_its_reason() {
        fn in_use(_: &Path) -> Result<(), Unavailable> {
            Err(Unavailable::InUse)
        }
        let now = Instant::now();
        let prompt = Prompt::new(in_use);
        assert_eq!(ask(&prompt, now), Err(Refusal::Screen(Unavailable::InUse)));
        assert_eq!((prompt.status(now), prompt.strikes()), (None, 0));
    }
}
