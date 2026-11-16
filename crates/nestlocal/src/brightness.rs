//! The screen's brightness: a percentage, or automatic from the ambient
//! light sensor. nlclient's patched settings code takes a request from
//! /var/run/unfr0zen/screen.brightness ("auto", or 0 to 100) at its next
//! bucket commit, once per request, so a choice on the unit's own Brightness
//! screen stays until the next one; it writes what the unit shows to
//! screen.brightness.state ("auto", or the percentage).
//!
//! The request is two fields of the device bucket, which the store keeps and
//! the held subscribe takes to nlclient, so a change makes it commit at once:
//! the percentage, and whether brightness is automatic. Neither set, no
//! request is written and the unit keeps its own setting. What is reported
//! is the state file, while there is one, else the fields.

use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::json::{number, truthy, Fields};
use crate::store::Store;

pub const PERCENT: &str = "unfr0zen_screen_brightness";
pub const AUTO: &str = "unfr0zen_screen_brightness_auto";
pub const PERCENTS: RangeInclusive<i64> = 0..=100;
/// The percentage turning automatic off picks with none stored or shown.
const FULL: i64 = 100;
const FILE: &str = "screen.brightness";
const STATE_FILE: &str = "screen.brightness.state";
const POLL: Duration = Duration::from_secs(5);

/// What the unit shows, as its state file says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    Auto,
    Percent(i64),
}

/// The state file's text: "auto", or a percentage.
pub fn parse_state(text: &str) -> Option<Shown> {
    match text.trim() {
        "auto" => Some(Shown::Auto),
        text => text.parse::<i64>().ok().filter(|p| PERCENTS.contains(p)).map(Shown::Percent),
    }
}

/// The stored percentage, if one was set.
pub fn stored(dev: &Fields) -> Option<i64> {
    number(dev.get(PERCENT)).map(|p| p as i64)
}

/// The request for nlclient; none while neither field was set.
pub fn request(dev: &Fields) -> Option<String> {
    if dev.get(PERCENT).is_none() && dev.get(AUTO).is_none() {
        return None;
    }
    Some(if truthy(dev.get(AUTO)) { "auto".into() } else { stored(dev).unwrap_or(FULL).to_string() })
}

/// What is reported: the percentage (with automatic on, the stored one) and
/// whether brightness is automatic, from what the unit shows, else the
/// fields.
pub fn reported(dev: &Fields, shown: Option<Shown>) -> (Option<i64>, bool) {
    match shown {
        Some(Shown::Auto) => (stored(dev), true),
        Some(Shown::Percent(p)) => (Some(p), false),
        None => (stored(dev), truthy(dev.get(AUTO))),
    }
}

/// A percentage as a command gives it: a whole number in PERCENTS.
pub fn parse_percent(arg: &str) -> Result<i64, String> {
    match arg.trim().parse::<i64>() {
        Ok(p) if PERCENTS.contains(&p) => Ok(p),
        _ => Err(format!("expected a percentage, {}-{}", PERCENTS.start(), PERCENTS.end())),
    }
}

/// The fields a percentage sets: automatic goes off.
pub fn percent_change(percent: i64) -> Fields {
    crate::fields! {PERCENT => percent, AUTO => false}
}

/// The fields turning automatic on or off sets. Off takes the stored
/// percentage, else the one the unit shows, else full.
pub fn auto_change(on: bool, dev: &Fields, shown: Option<Shown>) -> Fields {
    if on {
        return crate::fields! {AUTO => true};
    }
    let shown = match shown {
        Some(Shown::Percent(p)) => Some(p),
        _ => None,
    };
    crate::fields! {AUTO => false, PERCENT => stored(dev).or(shown).unwrap_or(FULL)}
}

/// Whether `change` touches the brightness.
pub fn changes(change: &Fields) -> bool {
    change.contains_key(PERCENT) || change.contains_key(AUTO)
}

/// Publishes the request the device bucket would hold with `change` merged
/// in, before the merge reaches nlclient.
pub fn publish_with(store: &Store, serial: &str, change: &Fields) {
    let mut dev = store.get(&format!("device.{serial}")).map(|o| o.value).unwrap_or_default();
    dev.extend(change.iter().map(|(k, v)| (k.clone(), v.clone())));
    publish(&dev);
}

/// Publishes the stored request again, for a restarted nlclient's first
/// commit.
pub fn republish(store: &Store, serial: &str) {
    if let Some(dev) = store.get(&format!("device.{serial}")) {
        publish(&dev.value);
    }
}

fn publish(dev: &Fields) {
    if let Some(text) = request(dev) {
        if let Err(e) = crate::settings_ui::publish(FILE, text.as_bytes()) {
            crate::logf!("screen brightness: {e}");
        }
    }
}

type Watcher = Box<dyn Fn() + Send + Sync>;

/// What the unit shows, as last read from its state file, and who is told
/// when that changes.
#[derive(Default)]
pub struct Watch {
    shown: Mutex<Option<Shown>>,
    watchers: Mutex<Vec<Watcher>>,
}

impl Watch {
    pub fn shown(&self) -> Option<Shown> {
        *self.shown.lock().unwrap()
    }

    /// Calls f whenever what the unit shows changes.
    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        self.watchers.lock().unwrap().push(Box::new(f));
    }

    /// Takes the state file's text (none: no file), telling the watchers of
    /// a change.
    fn update(&self, text: Option<&str>) {
        self.set_shown(text.and_then(parse_state));
    }

    /// Takes what the unit shows, telling the watchers of a change.
    pub fn set_shown(&self, shown: Option<Shown>) {
        if std::mem::replace(&mut *self.shown.lock().unwrap(), shown) != shown {
            for w in self.watchers.lock().unwrap().iter() {
                w();
            }
        }
    }
}

/// Reads the state file every POLL, on a thread of its own.
pub fn watch(watch: Arc<Watch>) {
    let path: PathBuf = Path::new(crate::settings_ui::DIR).join(STATE_FILE);
    std::thread::spawn(move || loop {
        watch.update(std::fs::read_to_string(&path).ok().as_deref());
        std::thread::sleep(POLL);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn nlclient_gets_auto_or_the_percentage_and_nothing_while_unset() {
        assert_eq!(request(&fields! {}), None);
        assert_eq!(request(&fields! {PERCENT => json!(40)}).as_deref(), Some("40"));
        assert_eq!(request(&fields! {PERCENT => json!(40.0), AUTO => json!(false)}).as_deref(), Some("40"));
        assert_eq!(request(&fields! {PERCENT => json!(0)}).as_deref(), Some("0"));
        assert_eq!(request(&fields! {PERCENT => json!(40), AUTO => json!(true)}).as_deref(), Some("auto"));
        assert_eq!(request(&fields! {AUTO => json!(false)}).as_deref(), Some("100"));
    }

    #[test]
    fn percentages_are_whole_and_between_0_and_100() {
        assert_eq!(parse_percent("0"), Ok(0));
        assert_eq!(parse_percent(" 100 "), Ok(100));
        for bad in ["-1", "101", "50.5", "", "auto", "on"] {
            assert!(parse_percent(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_state_file_says_auto_or_a_percentage() {
        assert_eq!(parse_state("auto"), Some(Shown::Auto));
        assert_eq!(parse_state("74\n"), Some(Shown::Percent(74)));
        assert_eq!(parse_state("0"), Some(Shown::Percent(0)));
        for bad in ["", "101", "-3", "high", "7 4"] {
            assert_eq!(parse_state(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_percentage_turns_auto_off_and_off_takes_the_stored_then_the_shown_then_full() {
        assert_eq!(percent_change(30), fields! {PERCENT => 30, AUTO => false});
        assert_eq!(
            auto_change(true, &fields! {PERCENT => json!(30)}, None),
            fields! {AUTO => true},
            "keeps the percentage"
        );
        let off = |dev: Fields, shown| auto_change(false, &dev, shown)[PERCENT].clone();
        assert_eq!(off(fields! {PERCENT => json!(30)}, Some(Shown::Percent(60))), json!(30));
        assert_eq!(off(fields! {}, Some(Shown::Percent(60))), json!(60));
        assert_eq!(off(fields! {}, Some(Shown::Auto)), json!(100));
        assert_eq!(off(fields! {}, None), json!(100));
    }

    #[test]
    fn what_the_unit_shows_wins_over_the_fields() {
        let dev = fields! {PERCENT => json!(30), AUTO => json!(false)};
        assert_eq!(reported(&dev, None), (Some(30), false));
        assert_eq!(reported(&dev, Some(Shown::Auto)), (Some(30), true));
        assert_eq!(reported(&dev, Some(Shown::Percent(90))), (Some(90), false));
        assert_eq!(reported(&fields! {}, None), (None, false));
        assert_eq!(reported(&fields! {AUTO => json!(true)}, None), (None, true));
    }

    #[test]
    fn watchers_hear_of_changes_only() {
        let watch = Watch::default();
        let told = Arc::new(AtomicUsize::new(0));
        let count = told.clone();
        watch.on_change(move || {
            count.fetch_add(1, Ordering::Relaxed);
        });
        watch.update(Some("auto"));
        watch.update(Some("auto"));
        watch.update(Some("55"));
        watch.update(None);
        watch.update(Some("garbage"));
        assert_eq!(told.load(Ordering::Relaxed), 3);
        assert_eq!(watch.shown(), None);
    }
}
