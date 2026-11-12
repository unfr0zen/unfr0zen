//! Running the recovery flow or a NAND job off the UI thread. A [`Run`] spawns
//! it and streams its progress back as [`Event`]s the window drains each frame.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use unfr0zen_host::diamond3::usb::EntryPoint;
use unfr0zen_host::nand::{self, Job};
use unfr0zen_host::{run_after_backup, CancelFlag, Config, Phase, Progress};

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
    /// A patch run entered this phase; the one before it is done.
    Phase(Phase),
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

/// A recovery flow or NAND job running on its own thread.
pub struct Run {
    events: Receiver<Event>,
    cancel: CancelFlag,
    _worker: thread::JoinHandle<()>,
}

impl Run {
    /// The recovery flow that patches the unit, with the whole NAND backed
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
