//! The crash-loop guard: a theme that keeps taking nlclient down with it (a
//! hang no limit stops, a crash in the GPU's driver) is not started again.
//!
//! Each start of a theme is counted in a file that outlives a reboot, and
//! the count dropped once the theme has run for a while (the library's
//! GUARD_WINDOW) or failed in a way the library survived. A theme started
//! STRIKES times in a row without that is refused, and the dial says so,
//! until it is picked again.

use std::io::Write;
use std::path::Path;

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
}
