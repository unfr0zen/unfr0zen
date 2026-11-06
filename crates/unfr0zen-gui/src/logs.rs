//! The runs so far: every log folder the host wrote, read back into the
//! task it ran, its steps and how it ended, for the Logs page.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// One step of a run, as the log tells it: begun on its own line, ended
/// with its time.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub label: String,
    pub elapsed: f32,
    pub ok: bool,
}

/// A run's log folder and what its log says.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    /// The folder's name: the run's date and time.
    pub stamp: String,
    /// The stamp as the system writes a date and time.
    pub when: String,
    pub dir: PathBuf,
    pub started: Option<SystemTime>,
    pub version: String,
    /// What was selected: the task, then its options.
    pub task: String,
    pub steps: Vec<Step>,
    /// The error of the step that failed, if one did.
    pub failure: Option<String>,
}

impl Run {
    pub fn log_path(&self) -> PathBuf {
        self.dir.join("unfr0zen.log")
    }

    /// The task alone: "Patch", "Back up NAND".
    pub fn title(&self) -> &str {
        self.task.split(", ").next().unwrap_or(&self.task)
    }

    /// Whether every step ended well: no step failed.
    pub fn ok(&self) -> bool {
        self.failure.is_none()
    }

    /// The run's length: its steps' times added up.
    pub fn elapsed(&self) -> f32 {
        self.steps.iter().map(|s| s.elapsed).sum()
    }
}

/// Reads a log's text into a run; `stamp` and `dir` name its folder. A
/// step is a line that a later "label: done (…)" or "label: failed (…)"
/// line ends; status lines look the same when they begin and are left out.
pub fn parse(stamp: &str, dir: &Path, text: &str) -> Run {
    let mut run = Run {
        stamp: stamp.into(), when: crate::when::stamp(stamp), dir: dir.into(), started: None, version: String::new(),
        task: String::new(), steps: Vec::new(), failure: None,
    };
    for line in text.lines() {
        // "[  12.345] message": the seconds are the process's, not the run's.
        let Some(message) = line.split_once("] ").map(|(_, m)| m).filter(|_| line.starts_with('[')) else { continue };
        if run.version.is_empty() && message.starts_with("unfr0zen ") {
            run.version = message.trim_start_matches("unfr0zen ").split(',').next().unwrap_or("").to_string();
        } else if let Some(task) = message.strip_prefix("Selected: ") {
            run.task = task.to_string();
        } else if let Some((label, rest)) = message.split_once(": done (") {
            run.steps.push(Step { label: label.into(), elapsed: seconds(rest), ok: true });
        } else if let Some((label, rest)) = message.split_once(": failed (") {
            let (secs, error) = rest.split_once("): ").unwrap_or((rest, ""));
            run.steps.push(Step { label: label.into(), elapsed: seconds(secs), ok: false });
            run.failure = Some(error.to_string());
        }
    }
    run
}

/// The seconds at the start of "3.2s): …".
fn seconds(text: &str) -> f32 {
    text.split(')').next().unwrap_or("").trim_end_matches('s').parse().unwrap_or(0.0)
}

/// Every run in the logs folder, newest first.
pub fn scan(logs: &Path) -> Vec<Run> {
    let Ok(read) = std::fs::read_dir(logs) else { return Vec::new() };
    let mut runs: Vec<Run> = read.flatten().filter_map(|entry| {
        let dir = entry.path();
        let text = std::fs::read_to_string(dir.join("unfr0zen.log")).ok()?;
        let stamp = entry.file_name().to_string_lossy().into_owned();
        let mut run = parse(&stamp, &dir, &text);
        run.started = entry.metadata().and_then(|m| m.modified()).ok();
        Some(run)
    }).collect();
    runs.sort_by(|a, b| b.stamp.cmp(&a.stamp));
    runs
}

/// Removes the runs older than `age`, except `keep`'s; how many went.
pub fn delete_older(runs: &[Run], age: Duration, keep: Option<&str>) -> usize {
    let cutoff = SystemTime::now().checked_sub(age);
    runs.iter().filter(|run| Some(run.stamp.as_str()) != keep)
        .filter(|run| matches!((run.started, cutoff), (Some(started), Some(cutoff)) if started < cutoff))
        .filter(|run| std::fs::remove_dir_all(&run.dir).is_ok()).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "[   3.100] unfr0zen v0.2.6-20-ge1bd43f, built for macos aarch64
[   3.101] OS: macOS 27.2 (26B123)
[   3.102] Selected: Patch, --ntp-server pool.ntp.org --ssh
[   3.150] HAB enabled
[   3.200] Load recovery U-Boot
[   3.300] USB: [\"10-3.2 15a2:0071\"], waiting on 10-3.2
[   6.400] Load recovery U-Boot: done (3.2s)
[   6.401] Patch nlclient, hosts and nestlocal
[  18.200] Patch nlclient, hosts and nestlocal: failed (11.8s): the volume has too little free space
";

    #[test]
    fn a_log_reads_back_into_its_task_steps_and_failure() {
        let run = parse("20261104-120000", Path::new("/tmp/x"), LOG);
        assert_eq!(run.version, "v0.2.6-20-ge1bd43f");
        assert_eq!(run.title(), "Patch");
        assert_eq!(run.task, "Patch, --ntp-server pool.ntp.org --ssh");
        assert_eq!(run.steps.len(), 2, "status lines such as HAB enabled are not steps: {:?}", run.steps);
        assert_eq!(run.steps[0], Step { label: "Load recovery U-Boot".into(), elapsed: 3.2, ok: true });
        assert!(!run.steps[1].ok);
        assert_eq!(run.failure.as_deref(), Some("the volume has too little free space"));
        assert!(!run.ok());
        assert!((run.elapsed() - 15.0).abs() < 0.01);
        assert_eq!(run.log_path(), Path::new("/tmp/x/unfr0zen.log"));
        assert!(run.when.contains("12"), "{}", run.when);
    }

    #[test]
    fn a_step_cut_short_is_not_listed() {
        let run = parse("s", Path::new("d"), &LOG.lines().take(6).collect::<Vec<_>>().join("\n"));
        assert!(run.steps.is_empty(), "{:?}", run.steps);
        assert!(run.ok(), "nothing failed");
        let done = parse("s", Path::new("d"), "[ 1.0] Selected: Boot the application\n[ 2.0] Chainload stock U-Boot from NAND: done (1.2s)\n");
        assert!(done.ok() && done.steps.len() == 1);
    }

    #[test]
    fn the_folder_is_listed_newest_first_and_old_runs_go() {
        let dir = std::env::temp_dir().join(format!("unfr0zen-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for stamp in ["20261101-080000", "20261104-120000"] {
            std::fs::create_dir_all(dir.join(stamp)).unwrap();
            std::fs::write(dir.join(stamp).join("unfr0zen.log"), LOG).unwrap();
        }
        std::fs::create_dir_all(dir.join("stray")).unwrap();
        let runs = scan(&dir);
        assert_eq!(runs.iter().map(|r| r.stamp.as_str()).collect::<Vec<_>>(), ["20261104-120000", "20261101-080000"]);
        assert_eq!(delete_older(&runs, Duration::ZERO, Some("20261104-120000")), 1);
        assert_eq!(scan(&dir).len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
