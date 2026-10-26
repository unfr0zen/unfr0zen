//! Running the recovery flow or a NAND job off the UI thread. A [`Run`] spawns
//! it and streams its progress back as [`Event`]s the window drains each frame.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use unfr0zen_host::nand::{self, Job};
use unfr0zen_host::{run, Config, Progress};

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
}

/// Asks the window and blocks until it answers; a closed window answers no.
fn confirm(tx: &Sender<Event>, question: &str) -> bool {
    let (answer, reply) = mpsc::channel();
    tx.send(Event::Confirm(question.to_string(), answer)).is_ok() && reply.recv().unwrap_or(false)
}

/// A recovery flow or NAND job running on its own thread.
pub struct Run {
    events: Receiver<Event>,
    _worker: thread::JoinHandle<()>,
}

impl Run {
    /// The recovery flow that patches the unit.
    pub fn start(config: Config) -> Self {
        Self::spawn(move |progress, _| run(&config, progress))
    }

    /// A whole-NAND backup or restore.
    pub fn nand(job: Job, wait_usb: Duration) -> Self {
        Self::spawn(move |progress, tx| nand::run(&job, wait_usb, None, progress, &mut |question| confirm(&tx, question)))
    }

    fn spawn(job: impl FnOnce(&mut ChannelProgress, Sender<Event>) -> unfr0zen_host::Result<String> + Send + 'static)
             -> Self {
        let (tx, events) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut progress = ChannelProgress { tx: tx.clone() };
            let result = job(&mut progress, tx.clone()).map_err(|e| e.to_string());
            let _ = tx.send(Event::Done(result));
        });
        Run { events, _worker: worker }
    }

    /// Every event that has arrived since the last call.
    pub fn events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}
