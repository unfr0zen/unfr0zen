//! What the update shows on the thermostat's screen. A thread of its own
//! takes the screen when a view arrives (handover::take), redraws it every
//! second and as the selection moves, since nlclient may still land a frame
//! on fb0 just after the take, and gives the screen back on hide. Presses
//! come back as the selected choice's action. The thread reports whether
//! each view it is sent reached the screen, which confirmed waits for.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::logf;
use crate::handover::page::{Page, Size};
use crate::handover::{self, Input, Taken};

/// How long the thread waits on the inputs before it looks for a new view.
const INPUT_WAIT: Duration = Duration::from_millis(100);
/// How often a view shown is drawn again.
const REDRAW: Duration = Duration::from_secs(1);
/// How long confirmed waits for the thread to take the screen.
pub const CONFIRM_FOR: Duration = Duration::from_secs(2);

/// Takes the screen through the hand-over files in a directory.
type Take = fn(&Path) -> std::io::Result<Taken>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    /// A window's code, and the address the unit is reached at.
    Code { code: String, ip: String },
    /// The network's prompt (prompt.rs): the laptop's name and address, and
    /// whether Allow and trust is offered; when it is not, the trusted list
    /// is full, and the page says so.
    Prompt { name: String, ip: String, trust: bool },
    /// A trusted laptop's session, named by the laptop, and how far it is.
    Notice { name: String, stage: Stage },
    /// A set on trial, named by who sent it.
    Trial { name: String },
}

/// How far a trusted session is, as its notice says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Connected,
    Reading,
    Receiving,
    Installing,
    Restarting,
}

impl Stage {
    fn text(self) -> &'static str {
        match self {
            Stage::Connected => "Connected",
            Stage::Reading => "Reading settings",
            Stage::Receiving => "Receiving update",
            Stage::Installing => "Installing",
            Stage::Restarting => "Restarting",
        }
    }
}

/// What a press on a view's choice asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Take the view off the screen.
    Hide,
    /// The prompt's answers.
    Allow,
    AllowTrust,
    Deny,
    /// The notice's: ends the trusted session, cuts its upload off, or
    /// rolls back the set it swapped in.
    Cancel,
    /// The trial notice's: rolls the set on trial back now.
    RollBack,
}

impl View {
    /// Its choices, and what each asks for.
    pub(super) fn choices(&self) -> &'static [(&'static str, Action)] {
        match self {
            View::Code { .. } => &[("Hide", Action::Hide)],
            View::Prompt { trust: true, .. } => &[("Allow", Action::Allow), ("Allow and trust", Action::AllowTrust), ("Deny", Action::Deny)],
            View::Prompt { trust: false, .. } => &[("Allow", Action::Allow), ("Deny", Action::Deny)],
            View::Notice { .. } => &[("Cancel", Action::Cancel)],
            View::Trial { .. } => &[("Roll back", Action::RollBack), ("Hide", Action::Hide)],
        }
    }

    pub(super) fn lines(&self) -> Vec<(String, Size)> {
        match self {
            View::Code { code, ip } => {
                let mut lines = vec![("Enter in unfr0zen".to_string(), Size::Normal), (code.clone(), Size::Large)];
                if !ip.is_empty() {
                    lines.push((ip.clone(), Size::Normal));
                }
                lines
            }
            View::Prompt { name, ip, trust } => {
                let mut lines = vec![("Update from".to_string(), Size::Normal), (name.clone(), Size::Normal),
                                     (format!("({ip})?"), Size::Normal)];
                if !trust {
                    lines.push(("trusted list full".to_string(), Size::Normal));
                }
                lines
            }
            View::Notice { name, stage } => {
                vec![("Update from".to_string(), Size::Normal), (name.clone(), Size::Normal), (stage.text().to_string(), Size::Normal)]
            }
            View::Trial { name } => {
                vec![("On trial".to_string(), Size::Normal), ("Update from".to_string(), Size::Normal), (name.clone(), Size::Normal)]
            }
        }
    }
}

/// A view on the screen and the choice selected.
struct Shown {
    view: View,
    selected: usize,
    /// The page's pixels as last drawn, until the selection moves.
    rendered: Option<Vec<u32>>,
}

impl Shown {
    fn new(view: View) -> Shown {
        Shown { view, selected: 0, rendered: None }
    }

    /// Draws the page on taken, rendering it only when it changed.
    fn draw(&mut self, taken: &mut Taken) {
        let rendered = match self.rendered.take() {
            Some(rendered) => rendered,
            None => taken.render(&self.page()),
        };
        taken.put(&rendered);
        self.rendered = Some(rendered);
    }

    fn page(&self) -> Page {
        let choices = self.view.choices().iter().map(|(label, _)| label.to_string()).collect();
        Page { lines: self.view.lines(), choices, selected: self.selected }
    }

    /// Moves the selection on a turn; a press is the selected choice's action.
    fn on(&mut self, input: Input) -> Option<Action> {
        let choices = self.view.choices();
        match input {
            Input::Turn(steps) => {
                let last = choices.len().saturating_sub(1) as i64;
                let selected = (self.selected as i64 + steps as i64).clamp(0, last) as usize;
                if selected != self.selected {
                    (self.selected, self.rendered) = (selected, None);
                }
                None
            }
            Input::Press => choices.get(self.selected).map(|(_, action)| *action),
        }
    }
}

pub struct Display {
    /// The view shown and the thread's channel, changed together: the
    /// thread is sent views in the order the view shown took them.
    state: Mutex<State>,
    /// What the thread has put on the screen.
    screen: Arc<Screen>,
    /// Sends presses as the thread does (press).
    #[cfg(test)]
    presses: Sender<Action>,
    /// Where presses arrive, until actions takes it.
    actions: Mutex<Option<Receiver<Action>>>,
}

struct State {
    shown: Option<View>,
    /// Views sent to the thread, each numbered.
    sent: u64,
    views: Sender<(u64, Option<View>)>,
    /// Where a display with no thread records views, as if each reached
    /// the screen.
    #[cfg(test)]
    record: Option<Sender<Option<View>>>,
}

/// The views the thread has handled, and whether one is on the screen after
/// the last.
#[derive(Default)]
struct Screen {
    handled: Mutex<(u64, bool)>,
    changed: Condvar,
}

impl Screen {
    fn handled(&self, view: u64, on: bool) {
        *self.handled.lock().unwrap_or_else(|e| e.into_inner()) = (view, on);
        self.changed.notify_all();
    }

    /// Whether a view is on the screen once the thread has handled the
    /// view numbered view, waiting up to wait for it to; false if it has not.
    fn on_after(&self, view: u64, wait: Duration) -> bool {
        self.after(view, wait).is_some_and(|on| on)
    }

    /// Whether a view is on the screen once the thread has handled the
    /// view numbered view, waiting up to wait for it to; none if it has not.
    fn after(&self, view: u64, wait: Duration) -> Option<bool> {
        let deadline = Instant::now() + wait;
        let mut handled = self.handled.lock().unwrap_or_else(|e| e.into_inner());
        while handled.0 < view {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            handled = self.changed.wait_timeout(handled, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        Some(handled.1)
    }
}

/// Starts the thread that shows views, taking the screen through the
/// hand-over files in dir.
pub fn start(dir: PathBuf) -> Display {
    start_with(dir, handover::take)
}

/// start, taking the screen with take.
pub fn start_with(dir: PathBuf, take: Take) -> Display {
    let (views, requests) = channel();
    let (presses, actions) = channel();
    let screen = Arc::new(Screen::default());
    let display = Display {
        state: Mutex::new(State { shown: None, sent: 0, views, #[cfg(test)] record: None }),
        screen: screen.clone(),
        #[cfg(test)]
        presses: presses.clone(),
        actions: Mutex::new(Some(actions)),
    };
    let spawned = std::thread::Builder::new().name("update-display".into()).spawn(move || run(&dir, requests, presses, &screen, take));
    if let Err(e) = spawned {
        logf!("wifi-update: display: {e}");
    }
    display
}

impl Display {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs f on the view shown (None: none), under the display's lock, so
    /// that what f decides from and what it leaves are one step; the thread
    /// shows what f leaves.
    pub fn update<R>(&self, f: impl FnOnce(&mut Option<View>) -> R) -> R {
        let mut state = self.state();
        let before = state.shown.clone();
        let result = f(&mut state.shown);
        if state.shown != before {
            state.sent += 1;
            let _ = state.views.send((state.sent, state.shown.clone()));
            #[cfg(test)]
            if let Some(record) = &state.record {
                let _ = record.send(state.shown.clone());
                self.screen.handled(state.sent, state.shown.is_some());
            }
        }
        result
    }

    /// Whether the view shown passes is and is on the screen: the thread
    /// took the screen for it, which this waits up to wait for.
    pub fn confirmed(&self, wait: Duration, is: impl FnOnce(&Option<View>) -> bool) -> bool {
        let (view, shown) = {
            let state = self.state();
            (state.sent, state.shown.is_some() && is(&state.shown))
        };
        shown && self.screen.on_after(view, wait)
    }

    /// Takes any view off and waits up to wait for the thread to give the
    /// screen back, restoring what the take changed: before nestlocal is
    /// stopped, which would leave that as it is. Whether it did.
    pub fn release(&self, wait: Duration) -> bool {
        self.update(|shown| *shown = None);
        let view = self.state().sent;
        self.screen.after(view, wait) == Some(false)
    }

    /// Shows view in place of any shown.
    #[cfg(test)]
    pub fn show(&self, view: View) {
        self.update(|shown| *shown = Some(view));
    }

    /// Takes any view off and gives the screen back.
    #[cfg(test)]
    pub fn hide(&self) {
        self.update(|shown| *shown = None);
    }

    /// Whether a view is shown.
    pub fn showing(&self) -> bool {
        self.state().shown.is_some()
    }

    /// The presses' actions: the first caller gets them, later ones none.
    pub fn actions(&self) -> Receiver<Action> {
        self.actions.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_else(|| channel().1)
    }

    /// A display that only records what it is asked to show (None to
    /// hide), as if each view reached the screen.
    #[cfg(test)]
    pub fn recording() -> (Display, Receiver<Option<View>>) {
        let (record, recorded) = channel();
        let (presses, actions) = channel();
        let state = State { shown: None, sent: 0, views: channel().0, record: Some(record) };
        let display = Display { state: Mutex::new(state), screen: Arc::new(Screen::default()), presses, actions: Mutex::new(Some(actions)) };
        (display, recorded)
    }

    /// A press on the screen, as the thread sends it.
    #[cfg(test)]
    pub fn press(&self, action: Action) {
        let _ = self.presses.send(action);
    }
}

/// The display's thread: holds the screen while a view shows, and tells
/// screen whether each view reached it.
fn run(dir: &Path, requests: Receiver<(u64, Option<View>)>, presses: Sender<Action>, screen: &Screen, take: Take) {
    let mut shown: Option<(Shown, Taken)> = None;
    let mut drawn = Instant::now();
    loop {
        let request = if shown.is_none() {
            match requests.recv() {
                Ok(request) => Some(request),
                Err(_) => return,
            }
        } else {
            match requests.try_recv() {
                Ok(request) => Some(request),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => return,
            }
        };
        match request {
            Some((n, None)) => {
                shown = None;
                screen.handled(n, false);
            }
            Some((n, Some(view))) => {
                let taken = match shown.take() {
                    Some((_, taken)) => Some(taken),
                    None => take(dir).map_err(|e| logf!("wifi-update: the screen: {e}")).ok(),
                };
                if let Some(mut taken) = taken {
                    let mut view = Shown::new(view);
                    view.draw(&mut taken);
                    drawn = Instant::now();
                    shown = Some((view, taken));
                }
                screen.handled(n, shown.is_some());
            }
            None => {}
        }
        if let Some((view, taken)) = &mut shown {
            let selected = view.selected;
            if let Some(input) = taken.input(INPUT_WAIT) {
                if let Some(action) = view.on(input) {
                    let _ = presses.send(action);
                }
            }
            if view.selected != selected || drawn.elapsed() >= REDRAW {
                view.draw(taken);
                drawn = Instant::now();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code() -> View {
        View::Code { code: "12345678".into(), ip: "192.168.1.50".into() }
    }

    #[test]
    fn the_code_view_shows_the_code_large_and_a_press_hides_it() {
        let mut shown = Shown::new(code());
        let page = shown.page();
        assert!(page.lines.contains(&("12345678".into(), Size::Large)), "{page:?}");
        assert!(page.lines.contains(&("192.168.1.50".into(), Size::Normal)), "{page:?}");
        assert_eq!(shown.on(Input::Turn(3)), None);
        assert_eq!(shown.selected, 0, "a single choice stays selected");
        assert_eq!(shown.on(Input::Press), Some(Action::Hide));
    }

    #[test]
    fn the_prompt_names_the_laptop_and_a_press_answers_with_the_selected_choice() {
        let prompt = |trust| View::Prompt { name: "laptop".into(), ip: "192.168.1.20".into(), trust };
        let mut shown = Shown::new(prompt(true));
        let page = shown.page();
        let lines: Vec<&str> = page.lines.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(lines, ["Update from", "laptop", "(192.168.1.20)?"]);
        assert_eq!(page.choices, ["Allow", "Allow and trust", "Deny"]);
        assert_eq!(shown.on(Input::Press), Some(Action::Allow));
        assert_eq!(shown.on(Input::Turn(1)), None);
        assert_eq!(shown.on(Input::Press), Some(Action::AllowTrust));
        assert_eq!(shown.on(Input::Turn(5)), None);
        assert_eq!(shown.on(Input::Press), Some(Action::Deny));
        let mut shown = Shown::new(prompt(false));
        let page = shown.page();
        let lines: Vec<&str> = page.lines.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(lines, ["Update from", "laptop", "(192.168.1.20)?", "trusted list full"]);
        assert_eq!(page.choices, ["Allow", "Deny"]);
        shown.on(Input::Turn(1));
        assert_eq!(shown.on(Input::Press), Some(Action::Deny));
    }

    #[test]
    fn the_notice_names_the_laptop_and_its_stage_and_a_press_cancels() {
        let stages = [(Stage::Connected, "Connected"), (Stage::Reading, "Reading settings"), (Stage::Receiving, "Receiving update"),
                      (Stage::Installing, "Installing"), (Stage::Restarting, "Restarting")];
        for (stage, said) in stages {
            let mut shown = Shown::new(View::Notice { name: "desk".into(), stage });
            let page = shown.page();
            let lines: Vec<&str> = page.lines.iter().map(|(text, _)| text.as_str()).collect();
            assert_eq!(lines, ["Update from", "desk", said]);
            assert_eq!(page.choices, ["Cancel"]);
            assert_eq!(shown.on(Input::Press), Some(Action::Cancel));
        }
    }

    #[test]
    fn the_trial_notice_names_who_sent_the_set_and_offers_roll_back_and_hide() {
        let mut shown = Shown::new(View::Trial { name: "desk".into() });
        let page = shown.page();
        let lines: Vec<&str> = page.lines.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(lines, ["On trial", "Update from", "desk"]);
        assert_eq!(page.choices, ["Roll back", "Hide"]);
        assert_eq!(shown.on(Input::Press), Some(Action::RollBack));
        shown.on(Input::Turn(1));
        assert_eq!(shown.on(Input::Press), Some(Action::Hide));
    }

    fn cannot_take(_: &Path) -> std::io::Result<Taken> {
        Err(std::io::Error::other("no panel"))
    }

    #[test]
    fn a_view_the_screen_cannot_take_is_not_confirmed() {
        let display = start_with(std::env::temp_dir().join(format!("nestlocal-display-{}", std::process::id())), cannot_take);
        display.show(code());
        assert!(!display.confirmed(CONFIRM_FOR, |shown| shown.is_some()));
        assert!(display.showing(), "asked for, though nothing reached the screen");
    }

    #[test]
    fn a_recording_display_confirms_what_it_shows() {
        let (display, _sent) = Display::recording();
        assert!(!display.confirmed(Duration::ZERO, |shown| shown.is_some()), "nothing shown");
        display.show(code());
        assert!(display.confirmed(Duration::ZERO, |shown| shown.is_some()));
        assert!(!display.confirmed(Duration::ZERO, |shown| matches!(shown, Some(View::Prompt { .. }))));
        display.hide();
        assert!(!display.confirmed(Duration::ZERO, |_| true), "nothing on the screen");
    }

    #[test]
    fn a_recording_display_records_views_and_passes_presses_on() {
        let (display, shown) = Display::recording();
        let actions = display.actions();
        display.show(code());
        assert!(display.showing());
        assert_eq!(shown.try_recv(), Ok(Some(code())));
        display.hide();
        assert!(!display.showing());
        assert_eq!(shown.try_recv(), Ok(None));
        display.press(Action::Hide);
        assert_eq!(actions.try_recv(), Ok(Action::Hide));
    }

    #[test]
    fn views_reach_the_thread_in_the_order_the_display_takes_them() {
        let (display, sent) = Display::recording();
        let display = std::sync::Arc::new(display);
        let mut last = None;
        for _ in 0..200 {
            let threads: Vec<_> = (0..4).map(|i| {
                let display = display.clone();
                std::thread::spawn(move || if i % 2 == 0 { display.show(code()) } else { display.hide() })
            }).collect();
            for thread in threads {
                thread.join().unwrap();
            }
            if let Some(view) = sent.try_iter().last() {
                last = view;
            }
            assert_eq!(last.is_some(), display.showing(), "the thread was left on another view than the display's");
        }
    }

    #[test]
    fn release_waits_for_the_thread_to_give_the_screen_back() {
        let dir = std::env::temp_dir().join(format!("nestlocal-display-release-{}", std::process::id()));
        let display = start_with(dir, cannot_take);
        display.show(code());
        assert!(display.release(CONFIRM_FOR), "the thread handled the hide");
        assert!(!display.showing());
        assert!(display.release(Duration::ZERO), "nothing to give back");
        let (display, sent) = Display::recording();
        display.show(code());
        assert!(display.release(Duration::ZERO));
        assert_eq!(sent.try_iter().collect::<Vec<_>>(), [Some(code()), None]);
    }

    #[test]
    fn an_update_decides_and_changes_the_view_in_one_step() {
        let (display, sent) = Display::recording();
        display.show(code());
        sent.try_recv().unwrap();
        assert!(!display.update(|shown| std::mem::take(shown).is_none()));
        assert_eq!(sent.try_recv(), Ok(None));
        display.update(|shown| assert!(shown.is_none()));
        assert!(sent.try_recv().is_err(), "nothing changed, nothing sent");
    }
}
