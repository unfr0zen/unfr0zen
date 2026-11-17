//! The crash-loop guard: a theme that keeps taking nlclient down with it (a
//! hang no limit stops, a crash in the GPU's driver) is not started again.
//!
//! Each start of a theme is counted in a file that outlives a reboot, and
//! the count dropped once the theme has run for a while (the library's
//! GUARD_WINDOW) or failed in a way the library survived. A theme started
//! STRIKES times in a row without that is refused, and the dial says so,
//! until it is picked again.
//!
//! The run is timed from a thread of its own (watch), as nlclient draws only
//! while the display is lit: a unit nobody looks at would count every restart
//! as a crash. A frame that has not returned holds the count, as the theme
//! may never finish it.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// The starts in a row a theme may not survive.
pub const STRIKES: u32 = 3;

/// The theme the file counts starts of, and how many.
fn counted(file: &Path) -> Option<(String, u32)> {
    let text = std::fs::read_to_string(file).ok()?;
    let (name, strikes) = text.trim_end().split_once('\t')?;
    Some((name.to_string(), strikes.parse().ok()?))
}

/// Counts a start of theme `name`, on the disk before it returns: the
/// start may be the last thing this process does. Refuses the theme at
/// STRIKES. A file that cannot be written guards nothing and stops nothing.
pub fn enter(file: &Path, name: &str) -> Result<(), String> {
    let strikes = counted(file).filter(|(counted, _)| counted == name).map_or(0, |(_, strikes)| strikes);
    if strikes >= STRIKES {
        return Err(format!("Stopped the display {STRIKES} times in a row. Pick the theme again to retry."));
    }
    let _ = std::fs::File::create(file).and_then(|mut f| {
        f.write_all(format!("{name}\t{}\n", strikes + 1).as_bytes())?;
        f.sync_all()
    });
    Ok(())
}

/// Forgets the starts counted: the theme has run, or is picked again.
pub fn leave(file: &Path) {
    let _ = std::fs::remove_file(file);
}

/// The start watch times, 0 for none, and the starts so far.
static WATCHED: AtomicU64 = AtomicU64::new(0);
static STARTS: AtomicU64 = AtomicU64::new(0);
/// A frame is being drawn.
static DRAWING: AtomicBool = AtomicBool::new(false);
/// How often a frame still drawing once the window has passed is looked at.
const POLL: Duration = Duration::from_millis(250);

/// A frame starts (true) or returns (false).
pub fn drawing(now: bool) {
    DRAWING.store(now, Ordering::SeqCst);
}

/// Forgets the count once the start just entered has lasted `window` with no
/// frame left drawing, unless unwatch or another start comes first.
pub fn watch(file: &Path, window: Duration) {
    let start = STARTS.fetch_add(1, Ordering::SeqCst) + 1;
    WATCHED.store(start, Ordering::SeqCst);
    let file = file.to_path_buf();
    let spawned = std::thread::Builder::new().name("theme-guard".into()).spawn(move || {
        std::thread::sleep(window);
        while DRAWING.load(Ordering::SeqCst) {
            if WATCHED.load(Ordering::SeqCst) != start {
                return;
            }
            std::thread::sleep(POLL);
        }
        if WATCHED.compare_exchange(start, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            leave(&file);
        }
    });
    if let Err(error) = spawned {
        eprintln!("THEME guard: {error}");
    }
}

/// Stops timing the start watched: it failed, or another theme is picked.
pub fn unwatch() {
    WATCHED.store(0, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_is_refused_after_three_starts_it_did_not_survive() {
        let file = std::env::temp_dir().join(format!("nestlocal-theme-guard-{}", std::process::id()));
        leave(&file);
        for _ in 0..STRIKES {
            enter(&file, "mine").unwrap();
        }
        assert!(enter(&file, "mine").unwrap_err().contains("3 times in a row"));
        assert!(enter(&file, "mine").is_err(), "a refusal counts no start");
        // Another theme starts from none, and takes the count over.
        enter(&file, "other").unwrap();
        enter(&file, "mine").unwrap();
        // One that ran, or is picked again, starts from none.
        enter(&file, "mine").unwrap();
        leave(&file);
        for _ in 0..STRIKES {
            enter(&file, "mine").unwrap();
        }
        std::fs::write(&file, "not a count").unwrap();
        enter(&file, "mine").unwrap();
        leave(&file);
        leave(&file);
        enter(Path::new("/nonexistent/unfr0zen/theme.guard"), "mine").unwrap();
    }

    #[test]
    fn a_start_that_lasts_its_window_with_no_frame_stuck_is_forgotten() {
        let file = std::env::temp_dir().join(format!("nestlocal-theme-watch-{}", std::process::id()));
        let window = Duration::from_millis(50);
        let counted = || file.exists();

        enter(&file, "mine").unwrap();
        drawing(true);
        watch(&file, window);
        std::thread::sleep(window * 6);
        assert!(counted(), "a frame that has not returned holds the count");
        drawing(false);
        std::thread::sleep(POLL * 3);
        assert!(!counted(), "the display asleep, nothing drawing: the start lasted");

        enter(&file, "mine").unwrap();
        watch(&file, window);
        unwatch();
        std::thread::sleep(window * 4);
        assert!(counted(), "a start no longer watched is not forgotten");
        leave(&file);
    }
}
