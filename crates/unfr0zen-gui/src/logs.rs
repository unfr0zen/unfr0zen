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
    /// The log's length when read: a log only grows, so the same length is
    /// the same log.
    pub log_len: u64,
}

impl Run {
    pub fn log_path(&self) -> PathBuf {
        self.dir.join("unfr0zen.log")
    }

    /// The task alone: "Set up", "Back up NAND".
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
        stamp: stamp.into(),
        when: crate::when::stamp(stamp),
        dir: dir.into(),
        started: None,
        version: String::new(),
        task: String::new(),
        steps: Vec::new(),
        failure: None,
        log_len: text.len() as u64,
    };
    for line in text.lines() {
        // "[  12.345] message": the seconds are the process's, not the run's.
        let Some(message) = line.split_once("] ").map(|(_, m)| m).filter(|_| line.starts_with('[')) else {
            continue;
        };
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

/// Every run in the logs folder, newest first. A run in `known` whose log
/// has not grown since is taken as it is, not read again.
pub fn scan(logs: &Path, known: &[Run]) -> Vec<Run> {
    let Ok(read) = std::fs::read_dir(logs) else {
        return Vec::new();
    };
    let mut runs: Vec<Run> = read
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let log = dir.join("unfr0zen.log");
            let len = std::fs::metadata(&log).ok()?.len();
            if let Some(run) = known.iter().find(|run| run.dir == dir && run.log_len == len) {
                return Some(run.clone());
            }
            let text = std::fs::read_to_string(&log).ok()?;
            let stamp = entry.file_name().to_string_lossy().into_owned();
            let mut run = parse(&stamp, &dir, &text);
            run.started = entry.metadata().and_then(|m| m.modified()).ok();
            Some(run)
        })
        .collect();
    runs.sort_by(|a, b| b.stamp.cmp(&a.stamp));
    runs
}

/// Appends to `text` what the log at `path` holds past it: nothing is read
/// while the log has not grown, and all of it again if it shrank.
pub fn read_more(path: &Path, text: &mut String) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(len) = std::fs::metadata(path).map(|m| m.len()) else { return };
    let have = text.len() as u64;
    if len == have {
        return;
    }
    if len < have {
        *text = std::fs::read_to_string(path).unwrap_or_default();
        return;
    }
    let mut more = Vec::new();
    let read = std::fs::File::open(path).and_then(|mut file| {
        file.seek(SeekFrom::Start(have))?;
        file.read_to_end(&mut more)
    });
    if read.is_ok() {
        // Up to the last whole character: the rest comes with the next read.
        let whole = std::str::from_utf8(&more).map_or_else(|e| e.valid_up_to(), str::len);
        text.push_str(std::str::from_utf8(&more[..whole]).expect("valid up to here"));
    }
}

/// Removes the runs older than `age`, except `keep`'s; how many went.
pub fn delete_older(runs: &[Run], age: Duration, keep: Option<&str>) -> usize {
    let cutoff = SystemTime::now().checked_sub(age);
    runs.iter()
        .filter(|run| Some(run.stamp.as_str()) != keep)
        .filter(|run| matches!((run.started, cutoff), (Some(started), Some(cutoff)) if started < cutoff))
        .filter(|run| std::fs::remove_dir_all(&run.dir).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "[   3.100] unfr0zen v0.2.6-20-ge1bd43f, built for macos aarch64
[   3.101] OS: macOS 27.2 (26B123)
[   3.102] Selected: Set up, --ntp-server pool.ntp.org --ssh
[   3.150] HAB enabled
[   3.200] Load recovery U-Boot
[   3.300] USB: [\"10-3.2 15a2:0071\"], waiting on 10-3.2
[   6.400] Load recovery U-Boot: done (3.2s)
[   6.401] Set up nlclient, hosts and nestlocal
[  18.200] Set up nlclient, hosts and nestlocal: failed (11.8s): the volume has too little free space
";

    #[test]
    fn a_log_reads_back_into_its_task_steps_and_failure() {
        let run = parse("20261104-120000", Path::new("/tmp/x"), LOG);
        assert_eq!(run.version, "v0.2.6-20-ge1bd43f");
        assert_eq!(run.title(), "Set up");
        assert_eq!(run.task, "Set up, --ntp-server pool.ntp.org --ssh");
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
        let done = parse(
            "s",
            Path::new("d"),
            "[ 1.0] Selected: Boot the application\n[ 2.0] Chainload stock U-Boot from NAND: done (1.2s)\n",
        );
        assert!(done.ok() && done.steps.len() == 1);
    }

    #[test]
    fn a_growing_log_is_read_from_where_it_was_left() {
        let dir = std::env::temp_dir().join(format!("unfr0zen-logs-more-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unfr0zen.log");
        std::fs::write(&path, "one\n").unwrap();
        let mut text = String::new();
        read_more(&path, &mut text);
        assert_eq!(text, "one\n");
        // Half of a two-byte character waits for the rest.
        std::fs::write(&path, b"one\ntwo \xc2").unwrap();
        read_more(&path, &mut text);
        assert_eq!(text, "one\ntwo ");
        std::fs::write(&path, "one\ntwo °C\n").unwrap();
        read_more(&path, &mut text);
        assert_eq!(text, "one\ntwo °C\n");
        // Shorter than what was read: another log, read whole.
        std::fs::write(&path, "new\n").unwrap();
        read_more(&path, &mut text);
        assert_eq!(text, "new\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_run_whose_log_has_not_grown_is_not_read_again() {
        let logs = std::env::temp_dir().join(format!("unfr0zen-logs-known-{}", std::process::id()));
        let dir = logs.join("20261118-101500");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("unfr0zen.log"), LOG).unwrap();
        let mut known = scan(&logs, &[]);
        known[0].task = "as known".into();
        assert_eq!(scan(&logs, &known)[0].task, "as known", "the same length: the known run");
        std::fs::write(dir.join("unfr0zen.log"), format!("{LOG}[  99.000] more\n")).unwrap();
        assert_ne!(scan(&logs, &known)[0].task, "as known", "grown: read again");
        std::fs::remove_dir_all(&logs).unwrap();
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
        let runs = scan(&dir, &[]);
        assert_eq!(runs.iter().map(|r| r.stamp.as_str()).collect::<Vec<_>>(), ["20261104-120000", "20261101-080000"]);
        assert_eq!(delete_older(&runs, Duration::ZERO, Some("20261104-120000")), 1);
        assert_eq!(scan(&dir, &[]).len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
