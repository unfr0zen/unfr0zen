//! Occupancy, from the unit's motion sensor. nlclient logs each detection
//! of its PIR sensor to its event log as it happens, a line for each
//! 30-second slot with motion, when logged then when detected (UTC):
//!
//!   2026-11-10T00:40:48 RealTimeOccupancy 2026-11-10T00:40:45
//!
//! The unit is occupied until HOLD after the last detection. The log is
//! read where nlclient writes it: its uploads come about every 20 minutes.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::logf;

/// nlclient's event log, rotated by renaming: a new file takes the name.
pub const EVENT_LOG: &str = "/nestlabs/var/log/nlevent";
/// How long a detection keeps the unit occupied, seconds.
pub const HOLD: i64 = 15 * 60;
/// How often the log is read for new lines, and the hold checked.
const POLL: Duration = Duration::from_secs(2);

type Watcher = Box<dyn Fn() + Send + Sync>;

#[derive(Default)]
pub struct Occupancy {
    /// The last detection, seconds since 1970.
    last: Mutex<Option<i64>>,
    /// What the watchers were last told.
    shown: Mutex<bool>,
    watchers: Mutex<Vec<Watcher>>,
}

impl Occupancy {
    /// Whether the last detection is under HOLD old.
    pub fn occupied(&self) -> bool {
        self.occupied_at(chrono::Utc::now().timestamp())
    }

    fn occupied_at(&self, now: i64) -> bool {
        self.last.lock().unwrap().is_some_and(|t| now - t < HOLD)
    }

    /// A detection at t, seconds since 1970, as a log line would give.
    #[cfg(test)]
    pub fn detected(&self, t: i64) {
        *self.last.lock().unwrap() = Some(t);
    }

    /// Calls f whenever occupied changes.
    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        self.watchers.lock().unwrap().push(Box::new(f));
    }

    /// Takes an event log line: a detection later than the last moves it.
    fn line(&self, line: &str) {
        if let Some(t) = detection(line) {
            let mut last = self.last.lock().unwrap();
            if last.is_none_or(|l| t > l) {
                *last = Some(t);
            }
        }
    }

    /// Tells the watchers when occupied has changed, as of now.
    fn update(&self, now: i64) {
        let occupied = self.occupied_at(now);
        let changed = std::mem::replace(&mut *self.shown.lock().unwrap(), occupied) != occupied;
        if changed {
            for w in self.watchers.lock().unwrap().iter() {
                w();
            }
        }
    }
}

/// When a RealTimeOccupancy line's motion was detected.
fn detection(line: &str) -> Option<i64> {
    let mut words = line.split_whitespace();
    let _logged = words.next()?;
    if words.next()? != "RealTimeOccupancy" {
        return None;
    }
    let t = chrono::NaiveDateTime::parse_from_str(words.next()?, "%Y-%m-%dT%H:%M:%S").ok()?;
    Some(t.and_utc().timestamp())
}

/// Follows the event log on a thread of its own, from the start of the
/// current file (its detections set the state at start), into each new one.
pub fn watch(occupancy: Arc<Occupancy>, path: PathBuf) {
    std::thread::spawn(move || {
        let mut missing = false;
        loop {
            let file = match File::open(&path) {
                Ok(f) => f,
                Err(e) => {
                    if !missing {
                        logf!("occupancy: {}: {e}", path.display());
                        missing = true;
                    }
                    occupancy.update(chrono::Utc::now().timestamp());
                    std::thread::sleep(POLL);
                    continue;
                }
            };
            missing = false;
            follow(&occupancy, file, &path);
        }
    });
}

/// Reads one file of the log until it is no longer the one at path.
fn follow(occupancy: &Occupancy, file: File, path: &PathBuf) {
    let Ok(inode) = file.metadata().map(|m| m.ino()) else { return };
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                occupancy.update(chrono::Utc::now().timestamp());
                std::thread::sleep(POLL);
                let position = reader.stream_position().unwrap_or(0);
                match std::fs::metadata(path) {
                    Ok(m) if m.ino() == inode && m.len() >= position => {}
                    _ => return, // rotated, or cut short: reopen
                }
            }
            Ok(_) if line.ends_with('\n') => {
                occupancy.line(&line);
                occupancy.update(chrono::Utc::now().timestamp());
            }
            // Half a line: nlclient is still writing it.
            Ok(n) => {
                let _ = reader.seek_relative(-(n as i64));
                std::thread::sleep(POLL);
            }
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn detections_are_read_from_realtimeoccupancy_lines_only() {
        assert_eq!(detection("2026-11-10T00:40:48 RealTimeOccupancy 2026-11-10T00:40:45\n"), Some(1_794_271_245));
        assert_eq!(detection("2026-11-10T00:40:48 %RealTimeOccupancy { \"fields\": [] }"), None);
        assert_eq!(detection("2026-11-10T00:40:00 BufferedPassiveInfrared 2026-11-10T00:40:00 100 19"), None);
        assert_eq!(detection("2026-11-10T00:40:48 RealTimeOccupancy garbage"), None);
        assert_eq!(detection(""), None);
    }

    #[test]
    fn occupied_until_the_hold_after_the_last_detection() {
        let o = Occupancy::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        o.on_change(move || { c.fetch_add(1, Ordering::SeqCst); });
        let t = 1_794_271_245;
        assert!(!o.occupied_at(t));
        o.line("2026-11-10T00:40:48 RealTimeOccupancy 2026-11-10T00:40:45");
        o.update(t + 3);
        assert!(o.occupied_at(t + HOLD - 1) && !o.occupied_at(t + HOLD));
        // An older detection, as when a file is read again, changes nothing.
        o.line("2026-11-10T00:30:00 RealTimeOccupancy 2026-11-10T00:30:00");
        o.update(t + HOLD - 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        o.update(t + HOLD);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "the hold ran out");
        o.update(t + HOLD + 60);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_log_is_followed_across_half_lines_and_rotation() {
        let dir = std::env::temp_dir().join(format!("nestlocal-occupancy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nlevent");
        let now = chrono::Utc::now();
        let line = |t: chrono::DateTime<chrono::Utc>| {
            let t = t.format("%Y-%m-%dT%H:%M:%S");
            format!("{t} RealTimeOccupancy {t}\n")
        };
        std::fs::write(&path, "2026-11-10T00:00:01 BufferedNearPir 2026-11-10T00:00:01 5 15\n").unwrap();
        let o = Arc::new(Occupancy::default());
        watch(o.clone(), path.clone());
        std::thread::sleep(Duration::from_millis(300));
        assert!(!o.occupied());

        // A detection, its line written in two parts.
        let full = line(now);
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&full.as_bytes()[..10]).unwrap();
        f.flush().unwrap();
        std::thread::sleep(POLL + Duration::from_millis(500));
        assert!(!o.occupied(), "half a line is not read");
        f.write_all(&full.as_bytes()[10..]).unwrap();
        f.flush().unwrap();
        std::thread::sleep(2 * POLL + Duration::from_millis(500));
        assert!(o.occupied());

        // Rotated: the file is renamed and a new one starts.
        *o.last.lock().unwrap() = Some(now.timestamp() - HOLD - 10);
        std::fs::rename(&path, dir.join("nlevent#1")).unwrap();
        std::fs::write(&path, line(now + chrono::Duration::seconds(30))).unwrap();
        std::thread::sleep(2 * POLL + Duration::from_millis(500));
        assert_eq!(*o.last.lock().unwrap(), Some(now.timestamp() + 30));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
