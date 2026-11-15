//! Running the recovery flow or a NAND job off the UI thread. A [`Run`] spawns
//! it and streams its progress back as [`Event`]s the window drains each frame.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use unfr0zen_host::diamond3::usb::EntryPoint;
use unfr0zen_host::nand::{self, Job};
use unfr0zen_host::push::Code;
use unfr0zen_host::wifi_patch::{self, Class, Outcome, Remote};
use unfr0zen_host::{run_after_backup, CancelFlag, Config, SetupInputs, Phase, Progress};

/// One thing that happened in the flow, for the window.
pub enum Event {
    Begin(String),
    End { label: String, elapsed: Duration, ok: bool },
    Status(String),
    Waiting(String),
    /// The unit, as far as it is identified.
    Device(String),
    /// The running step has moved `done` of `total` bytes, with `next` under way.
    Transfer { done: u64, next: u64, total: u64 },
    /// The job waits for a yes or no to the question before it writes.
    Confirm(String, Sender<bool>),
    /// A setup run entered this phase; the one before it is done.
    Phase(Phase),
    /// A Wi-Fi run waits for the code the unit shows, asked with this
    /// prompt; none ends it.
    Code(String, Sender<Option<String>>),
    /// A Wi-Fi read's unit at the address read, for the form.
    Read(String, Box<Remote>),
    /// The unit kept a set sent over Wi-Fi: it runs this app's nestlocal.
    Updated,
    /// A Wi-Fi run stopped before it sent anything; with why the unit takes
    /// the set only from a setup over USB, when that is why
    /// (Error::NeedsUsb).
    NotSent(Option<String>),
    /// The run finished: the final status, or the error text.
    Done(Result<String, String>),
}

/// A [`Progress`] that forwards to the window over a channel.
struct ChannelProgress {
    tx: Sender<Event>,
}

impl Progress for ChannelProgress {
    fn begin(&mut self, label: &str) {
        let _ = self.tx.send(Event::Begin(label.to_string()));
    }
    fn end(&mut self, label: &str, elapsed: Duration, ok: bool) {
        let _ = self.tx.send(Event::End { label: label.to_string(), elapsed, ok });
    }
    fn status(&mut self, message: &str) {
        let _ = self.tx.send(Event::Status(message.to_string()));
    }
    fn waiting(&mut self, message: &str) {
        let _ = self.tx.send(Event::Waiting(message.to_string()));
    }
    fn device(&mut self, description: &str) {
        let _ = self.tx.send(Event::Device(description.to_string()));
    }
    fn transfer(&mut self, done: u64, next: u64, total: u64) {
        let _ = self.tx.send(Event::Transfer { done, next, total });
    }
    fn phase(&mut self, phase: Phase) {
        let _ = self.tx.send(Event::Phase(phase));
    }
}

/// Asks the window and blocks until it answers; a closed window answers no.
fn confirm(tx: &Sender<Event>, question: &str) -> bool {
    let (answer, reply) = mpsc::channel();
    tx.send(Event::Confirm(question.to_string(), answer)).is_ok() && reply.recv().unwrap_or(false)
}

/// Asks the window for the unit's code and blocks until it answers; a closed
/// window answers none.
fn ask_code(tx: &Sender<Event>, prompt: &str) -> Option<String> {
    let (answer, reply) = mpsc::channel();
    tx.send(Event::Code(prompt.to_string(), answer)).ok()?;
    reply.recv().ok().flatten()
}

/// What the unit does with a set.
fn does(class: Class) -> &'static str {
    match class {
        Class::Restart => crate::tr!("wifi.restart"),
        Class::Reboot => crate::tr!("wifi.reboot"),
    }
}

/// A set's changes, one a line: what happens to which file.
fn listed(outcome: &Outcome) -> String {
    outcome.changes.iter().map(|change| format!("{} {}", change.action, change.name)).collect::<Vec<_>>().join("\n")
}

/// A recovery flow or NAND job running on its own thread.
pub struct Run {
    events: Receiver<Event>,
    cancel: CancelFlag,
    _worker: thread::JoinHandle<()>,
}

impl Run {
    /// The recovery flow that sets up the unit, with the whole NAND backed
    /// up to `backup` first, in the same recovery session.
    pub fn start(config: Config, backup: Option<PathBuf>) -> Self {
        Self::spawn(move |progress, _, cancel| run_after_backup(&config, backup.as_deref(), progress, &cancel))
    }

    /// A whole-NAND backup or restore.
    pub fn nand(job: Job, wait_usb: Duration, entry_point: EntryPoint) -> Self {
        Self::spawn(move |progress, tx, cancel| nand::run(&job, wait_usb, None, entry_point, progress, &mut |question| confirm(&tx, question), &cancel))
    }

    /// Any other job that reports through a [`Progress`]: the advanced
    /// commands.
    pub fn task(job: impl FnOnce(&mut dyn Progress) -> unfr0zen_host::Result<String> + Send + 'static) -> Self {
        Self::spawn(move |progress, _, _| job(progress))
    }

    /// A job shaped as a Wi-Fi run: it asks the window for the code, and
    /// each of its reports is a step, done when the next begins; the last
    /// ends as the job does.
    #[cfg(test)]
    pub fn with_code(job: impl FnOnce(Code, &dyn Fn(&str)) -> unfr0zen_host::Result<String> + Send + 'static) -> Self {
        Self::steps(move |ask, report, _| job(Code::Ask(ask), report))
    }

    /// The settings of the unit at `host`, read over Wi-Fi for the form.
    pub fn wifi_read(host: String) -> Self {
        Self::steps(move |ask, report, tx| {
            let remote = wifi_patch::read_settings(&host, Code::Ask(ask), report)?;
            let read = crate::tr!("wifi.read", device = remote.device.description(), host = host);
            let _ = tx.send(Event::Read(host, Box::new(remote)));
            Ok(read)
        })
    }

    /// The form's settings to the unit at `host` over Wi-Fi: the changes
    /// they make, and once the window says yes to them, the set sent. The code typed for the changes answers the send's
    /// first ask; a wrong one is asked for again.
    pub fn wifi_patch(host: String, inputs: SetupInputs, force: bool) -> Self {
        Self::steps(move |ask, report, tx| {
            let typed: RefCell<Option<String>> = RefCell::new(None);
            let again: RefCell<Option<String>> = RefCell::new(None);
            let ask = |prompt: &str| again.take().or_else(|| {
                let code = ask(prompt);
                typed.replace(code.clone());
                code
            });
            // The plan sends nothing, and a run's check of its set comes
            // before it; a USB setup the unit needs stays with it, for Review.
            let not_sent = |error: &unfr0zen_host::Error| {
                let reason = match error {
                    unfr0zen_host::Error::NeedsUsb(reason) => Some(reason.clone()),
                    _ => None,
                };
                let _ = tx.send(Event::NotSent(reason));
            };
            let planned = wifi_patch::patch(&host, Code::Ask(&ask), &inputs, true, force, report).inspect_err(not_sent)?;
            let Some(class) = planned.class else { return Ok(crate::tr!("wifi.nothing").to_string()) };
            if !confirm(&tx, &crate::tr!("wifi.confirm", what = does(class), changes = listed(&planned))) {
                return Ok(crate::tr!("wifi.not_sent").to_string());
            }
            again.replace(typed.take());
            let sent = wifi_patch::patch(&host, Code::Ask(&ask), &inputs, false, force, report).inspect_err(|error| {
                if matches!(error, unfr0zen_host::Error::NeedsUsb(_)) {
                    not_sent(error);
                }
            })?;
            Ok(match sent.class {
                None => crate::tr!("wifi.nothing").to_string(),
                // A sent set comes back only once the unit kept it: a roll back
                // or a cancel on the thermostat is an error (wifi_patch::wait).
                Some(class) => {
                    let _ = tx.send(Event::Updated);
                    crate::tr!("wifi.kept", what = does(class), changes = listed(&sent))
                }
            })
        })
    }

    /// A job that asks the window for the code, through the ask it is
    /// given; each of its reports is a step, done when the next begins, and
    /// the last ends as the job does.
    fn steps(job: impl FnOnce(&dyn Fn(&str) -> Option<String>, &dyn Fn(&str), Sender<Event>) -> unfr0zen_host::Result<String>
             + Send + 'static) -> Self {
        Self::spawn(move |progress, tx, _| {
            let progress = RefCell::new(progress);
            let step: RefCell<Option<(String, Instant)>> = RefCell::new(None);
            let report = |message: &str| {
                let mut progress = progress.borrow_mut();
                if let Some((label, started)) = step.replace(Some((message.to_string(), Instant::now()))) {
                    progress.end(&label, started.elapsed(), true);
                }
                progress.begin(message);
            };
            let ask = |prompt: &str| ask_code(&tx, prompt);
            let result = job(&ask, &report, tx.clone());
            if let Some((label, started)) = step.take() {
                progress.borrow_mut().end(&label, started.elapsed(), result.is_ok());
            }
            result
        })
    }

    /// Signals the running flow to cut the USB download wait short.
    pub fn cancel_wait(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn spawn(job: impl FnOnce(&mut ChannelProgress, Sender<Event>, CancelFlag) -> unfr0zen_host::Result<String> + Send + 'static)
             -> Self {
        let (tx, events) = mpsc::channel();
        let cancel: CancelFlag = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let worker = thread::spawn(move || {
            let mut progress = ChannelProgress { tx: tx.clone() };
            let result = job(&mut progress, tx.clone(), flag).map_err(|e| e.to_string());
            let _ = tx.send(Event::Done(result));
        });
        Run { events, cancel, _worker: worker }
    }

    /// Every event that has arrived since the last call.
    pub fn events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}
