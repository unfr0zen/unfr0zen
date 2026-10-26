//! How the flow reports: a stage begins, ends with its elapsed time and
//! outcome, or a one-line status. A terminal renders it as spinner lines, a
//! window as a list. The run log, timestamped, is separate and always written.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::Result;

static STARTED: OnceLock<Instant> = OnceLock::new();

/// Seconds since the process started, for the run log.
fn elapsed() -> f64 {
    STARTED.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// What the flow tells its front end as it runs.
pub trait Progress: Send {
    /// A high-level step started.
    fn begin(&mut self, label: &str);
    /// The step from the matching `begin` finished.
    fn end(&mut self, label: &str, elapsed: Duration, ok: bool);
    /// A fact worth showing, such as the slot chosen.
    fn status(&mut self, message: &str);
    /// Something the run is waiting on the person or the unit for.
    fn waiting(&mut self, message: &str);
    /// The unit, as far as it is identified: its device, then its silicon.
    /// Each call replaces the last.
    fn device(&mut self, description: &str) {
        self.status(&format!("Device: {description}"));
    }
    /// The running step moves `total` bytes: `done` so far, and the next
    /// `next` under way with nothing more said until they land. A front end
    /// may draw a bar.
    fn transfer(&mut self, _done: u64, _next: u64, _total: u64) {}
}

/// Runs one step, reporting its start, elapsed time and outcome.
pub fn stage<T>(progress: &mut dyn Progress, label: &str, step: impl FnOnce() -> Result<T>) -> Result<T> {
    stage_with(progress, label, |_| step())
}

/// [`stage`] for a step that reports into the progress itself.
pub fn stage_with<T>(progress: &mut dyn Progress, label: &str,
                     step: impl FnOnce(&mut dyn Progress) -> Result<T>) -> Result<T> {
    progress.begin(label);
    log::info!("{label}");
    let started = Instant::now();
    let result = step(progress);
    let secs = started.elapsed().as_secs_f64();
    match &result {
        Ok(_) => log::info!("{label}: done ({secs:.1}s)"),
        Err(error) => log::error!("{label}: failed ({secs:.1}s): {error}"),
    }
    progress.end(label, started.elapsed(), result.is_ok());
    result
}

/// A step name and how it ended, for a front end that keeps a list.
#[derive(Clone, Debug)]
pub struct Stage {
    pub label: String,
    pub elapsed: Duration,
    pub ok: bool,
}

/// The run log: every `log` record at or above info, and the flow's own
/// debug lines (console transcripts, USB changes), each with the seconds
/// since the process started. File contents never reach it.
struct RunLog(Mutex<Option<fs::File>>);

impl log::Log for RunLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.target().starts_with("unfr0zen") || metadata.level() <= log::Level::Info
    }
    fn log(&self, record: &log::Record) {
        if let (true, Some(file)) = (self.enabled(record.metadata()), self.0.lock().unwrap().as_mut()) {
            let _ = writeln!(file, "[{:8.3}] {}", elapsed(), record.args().to_string().trim_end());
        }
    }
    fn flush(&self) {}
}

static RUN_LOG: RunLog = RunLog(Mutex::new(None));
static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Installs the run logger once, early. Later a run points it at its file.
pub fn init_logging() {
    STARTED.get_or_init(Instant::now);
    let _ = log::set_logger(&RUN_LOG).map(|()| log::set_max_level(log::LevelFilter::Debug));
}

/// Sends the run log to `path` from now on.
pub(crate) fn set_log_file(path: &Path) -> Result<()> {
    *RUN_LOG.0.lock().unwrap() = Some(fs::File::create(path)?);
    *LOG_PATH.lock().unwrap() = Some(path.to_path_buf());
    Ok(())
}

/// The current run's log file, once a run has opened one.
pub fn log_path() -> Option<PathBuf> {
    LOG_PATH.lock().unwrap().clone()
}
