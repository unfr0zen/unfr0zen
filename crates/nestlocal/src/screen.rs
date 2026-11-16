//! How long the screen stays lit after the last interaction: nlclient's
//! sleep timeout (client.config's sleeptimeout, stock 30 s), which its
//! patched settings code sets from /var/run/unfr0zen/screen.timeout, in
//! milliseconds, at its next bucket commit.
//!
//! The setting is two fields of the device bucket, which the store keeps and
//! the held subscribe takes to nlclient, so a change makes it commit at once:
//! the timeout in seconds, and whether the screen stays on. Neither set, no
//! file is written and nlclient keeps its own.

use std::ops::RangeInclusive;

use crate::json::{number, truthy, Fields};
use crate::store::Store;

pub const TIMEOUT: &str = "unfr0zen_screen_timeout";
pub const ALWAYS_ON: &str = "unfr0zen_screen_always_on";
/// nlclient's own timeout, until one is set.
pub const DEFAULT_SECONDS: i64 = 30;
pub const SECONDS: RangeInclusive<i64> = 10..=600;
/// "Always on": a day. nlclient's timer goes through NSPR's
/// PR_MillisecondsToInterval, so a value meaning never is not safe.
pub const ALWAYS_ON_MS: i64 = 86_400_000;
const FILE: &str = "screen.timeout";

/// The timeout in seconds the device bucket holds, or nlclient's own.
pub fn seconds(dev: &Fields) -> i64 {
    number(dev.get(TIMEOUT)).map_or(DEFAULT_SECONDS, |s| s as i64)
}

pub fn always_on(dev: &Fields) -> bool {
    truthy(dev.get(ALWAYS_ON))
}

/// What nlclient is given, in milliseconds; none while neither field was set.
pub fn milliseconds(dev: &Fields) -> Option<i64> {
    if dev.get(TIMEOUT).is_none() && dev.get(ALWAYS_ON).is_none() {
        return None;
    }
    Some(if always_on(dev) { ALWAYS_ON_MS } else { seconds(dev) * 1000 })
}

/// Seconds as a command gives them: a whole number in SECONDS.
pub fn parse_seconds(arg: &str) -> Result<i64, String> {
    match arg.trim().parse::<i64>() {
        Ok(s) if SECONDS.contains(&s) => Ok(s),
        _ => Err(format!("expected seconds, {}-{}", SECONDS.start(), SECONDS.end())),
    }
}

/// Publishes the timeout the device bucket would hold with `change` merged
/// in, before the merge reaches nlclient.
pub fn publish_with(store: &Store, serial: &str, change: &Fields) {
    let mut dev = store.get(&format!("device.{serial}")).map(|o| o.value).unwrap_or_default();
    dev.extend(change.iter().map(|(k, v)| (k.clone(), v.clone())));
    publish(&dev);
}

/// Publishes the stored timeout again, for a restarted nlclient's first
/// commit.
pub fn republish(store: &Store, serial: &str) {
    if let Some(dev) = store.get(&format!("device.{serial}")) {
        publish(&dev.value);
    }
}

/// The file's contents for the device bucket `dev`, if it gets one.
fn contents(dev: &Fields) -> Option<String> {
    milliseconds(dev).map(|ms| ms.to_string())
}

fn publish(dev: &Fields) {
    if let Some(text) = contents(dev) {
        if let Err(e) = crate::settings_ui::publish(FILE, text.as_bytes()) {
            crate::logf!("screen timeout: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields;
    use serde_json::json;

    #[test]
    fn nlclient_gets_the_seconds_or_a_day_and_nothing_while_unset() {
        assert_eq!(milliseconds(&fields!{}), None);
        assert_eq!((seconds(&fields!{}), always_on(&fields!{})), (30, false));
        assert_eq!(milliseconds(&fields!{TIMEOUT => json!(60)}), Some(60_000));
        assert_eq!(milliseconds(&fields!{ALWAYS_ON => json!(false)}), Some(30_000));
        assert_eq!(milliseconds(&fields!{TIMEOUT => json!(60), ALWAYS_ON => json!(true)}), Some(86_400_000));
        assert_eq!(contents(&fields!{TIMEOUT => json!(60.0)}).as_deref(), Some("60000"), "decimal ASCII milliseconds");
        assert_eq!(contents(&fields!{ALWAYS_ON => json!(true)}).as_deref(), Some("86400000"));
        assert_eq!(contents(&fields!{}), None);
    }

    #[test]
    fn seconds_are_whole_and_between_10_and_600() {
        assert_eq!(parse_seconds("10"), Ok(10));
        assert_eq!(parse_seconds(" 600 "), Ok(600));
        for bad in ["9", "601", "30.5", "", "on", "-30"] {
            assert!(parse_seconds(bad).is_err(), "{bad}");
        }
    }
}
