//! The unit's power: whether it has outside power, from its backplate or
//! USB, or runs on its battery alone, read where Nest's shutdown daemon
//! (nlshutdownd) reads it (device::Power); and the battery's charge, from
//! its voltage, the only measure the unit gives.

use std::sync::{Arc, Mutex};
use std::time::Duration;

const POLL: Duration = Duration::from_secs(5);

/// The voltages read as an empty and a full battery: at 3.5 V the battery
/// is critical, and a charged unit on its wiring holds 3.86-3.93 V.
pub const EMPTY: f64 = 3.5;
const FULL: f64 = 3.9;
/// Below this the battery is low: the unit starts to shut things down.
pub const LOW: f64 = 3.6;

/// The battery's charge in whole percent, linear between EMPTY and FULL.
pub fn percent(volts: f64) -> u8 {
    ((volts - EMPTY) / (FULL - EMPTY) * 100.0).clamp(0.0, 100.0).round() as u8
}

/// A sysfs line: 1 or on, 0 or off.
fn line(text: &str) -> Option<bool> {
    match text.trim() {
        "1" | "on" => Some(true),
        "0" | "off" => Some(false),
        _ => None,
    }
}

/// Whether any of `lines` says there is outside power; none when none of
/// them can be read.
fn outside(lines: &[Option<bool>]) -> Option<bool> {
    let read: Vec<bool> = lines.iter().flatten().copied().collect();
    (!read.is_empty()).then(|| read.contains(&true))
}

type Watcher = Box<dyn Fn() + Send + Sync>;

/// Whether the unit has outside power, as last read, and who is told when
/// that changes.
#[derive(Default)]
pub struct Watch {
    external: Mutex<Option<bool>>,
    watchers: Mutex<Vec<Watcher>>,
}

impl Watch {
    /// Outside power; none where the unit doesn't say.
    pub fn external(&self) -> Option<bool> {
        *self.external.lock().unwrap()
    }

    /// Calls f whenever outside power comes or goes.
    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        self.watchers.lock().unwrap().push(Box::new(f));
    }

    pub(crate) fn set(&self, external: Option<bool>) {
        if std::mem::replace(&mut *self.external.lock().unwrap(), external) != external {
            for w in self.watchers.lock().unwrap().iter() {
                w();
            }
        }
    }
}

/// Reads the unit's power lines every POLL, on a thread of its own.
pub fn watch(watch: Arc<Watch>) {
    let power = &crate::device::current().power;
    let read = |path: &str| std::fs::read_to_string(path).ok().as_deref().and_then(line);
    std::thread::spawn(move || loop {
        watch.set(outside(&[read(power.backplate), read(power.usb)]));
        std::thread::sleep(POLL);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_is_linear_from_empty_to_full_and_clamped() {
        for (volts, want) in [(3.0, 0), (3.5, 0), (3.6, 25), (3.7, 50), (3.864, 91), (3.9, 100), (4.2, 100)] {
            assert_eq!(percent(volts), want, "{volts}");
        }
    }

    #[test]
    fn either_line_gives_outside_power_and_none_read_says_nothing() {
        for (text, want) in [("1\n", Some(true)), ("on\n", Some(true)), ("0", Some(false)), ("off", Some(false))] {
            assert_eq!(line(text), want, "{text:?}");
        }
        assert_eq!(line("Vbus off, timeout 1100"), None);
        assert_eq!(outside(&[Some(false), Some(true)]), Some(true), "USB alone");
        assert_eq!(outside(&[Some(true), None]), Some(true), "backplate alone");
        assert_eq!(outside(&[Some(false), Some(false)]), Some(false), "on the battery");
        assert_eq!(outside(&[None, None]), None);
    }

    #[test]
    fn watchers_hear_only_changes() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let w = Watch::default();
        let counted = calls.clone();
        w.on_change(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        for external in [Some(true), Some(true), Some(false), None, None] {
            w.set(external);
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 3);
        assert_eq!(w.external(), None);
    }
}
