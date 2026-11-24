//! Durable schedule suspension, shared by every command transport.

use crate::{bridge::Bridge, fields, json::Fields, schedule, store::Object};
use chrono::{Datelike, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Mode {
    pub saved: Option<Fields>,
    pub until: Option<i64>,
    /// A device week with no targets while set is the suspended week arriving
    /// after the restore was sent, or brought back by a switch to this mode.
    #[serde(default)]
    pub restored: Option<Fields>,
    /// An inactive mode applies its due target when it next becomes current.
    #[serde(default)]
    pub pending_target: bool,
    /// After a dial turn during a timed hold, the saved week's next change:
    /// the hold ends there, and renewing it cannot move its end past it.
    #[serde(default)]
    pub dial_cap: Option<i64>,
}

#[derive(Clone, Copy, Debug)]
pub struct Status {
    pub available: bool,
    pub enabled: bool,
    pub hold_until: Option<i64>,
    pub hold_minutes: u16,
}

pub fn mode(bucket: &Fields) -> Option<&'static str> {
    match schedule::kind(bucket)? {
        schedule::Kind::Heat => Some("HEAT"),
        schedule::Kind::Cool => Some("COOL"),
        schedule::Kind::Range => Some("RANGE"),
    }
}

pub fn empty_days() -> Value {
    Value::Object((0..7).map(|day| (day.to_string(), Value::Object(Fields::new()))).collect())
}

/// Seconds a restored learning setting is protected from a late echo of the suspension's.
const RESTORE_GUARD: i64 = 300;
/// Seconds before the wake alarm retries an expiry that failed.
const EXPIRY_RETRY: i64 = 300;
/// Seconds the SCHEDULE page shows an error.
const ERROR_SHOWN: std::time::Duration = std::time::Duration::from_secs(10);

/// Sets learning on the device bucket; `restored` guards it against a late
/// device echo of the value it replaces.
fn set_learning(device: &mut Object, learning: Value, restored: bool) {
    device.restored_learning = (restored && !learning.is_null()).then(|| crate::store::RestoredLearning {
        value: learning.clone(),
        until: chrono::Utc::now().timestamp() + RESTORE_GUARD,
    });
    if learning.is_null() {
        device.value.remove("learning_mode");
    } else {
        device.value.insert("learning_mode".into(), learning);
    }
}

fn clock_ready(now: i64) -> bool {
    (1_577_836_800..4_102_444_800).contains(&now)
}

pub(crate) fn has_targets(bucket: &Fields) -> bool {
    match schedule::kind(bucket) {
        Some(schedule::Kind::Heat | schedule::Kind::Cool) => (0..7).any(|day| !schedule::day(bucket, day).is_empty()),
        Some(schedule::Kind::Range) => (0..7).any(|day| !schedule::range_day(bucket, day).is_empty()),
        None => false,
    }
}

/// The native editor sees an empty week while suspended, not the saved week.
/// Its writes and device echoes cannot replace that saved week; editing through
/// the native editor requires resuming first. After a resume, only a device
/// week without targets is taken for the suspended one: any other is an edit
/// on the unit, and ends the restore's protection.
pub fn reconcile(previous: &Object, incoming: &mut Object) -> bool {
    let Some(name) = mode(&incoming.value) else { return false };
    let empty = !has_targets(&incoming.value);
    let new_week = mode(&previous.value) != Some(name) || previous.value.get("days") != incoming.value.get("days");
    let Some(control) = incoming.local_schedules.as_mut().and_then(|c| c.control.get_mut(name)) else { return false };
    let days = if control.saved.is_some() {
        if !empty {
            crate::logf!(
                "schedule: rejected native week while suspended; resume before using the native schedule editor"
            );
        }
        Some(empty_days())
    } else if new_week && !empty {
        control.restored = None;
        None
    } else if new_week {
        control.restored.as_ref().and_then(|saved| saved.get("days")).cloned()
    } else {
        None
    };
    if let Some(days) = days {
        let corrected = incoming.value.get("days") != Some(&days);
        incoming.value.insert("days".into(), days);
        corrected
    } else {
        false
    }
}

impl Bridge {
    pub fn configured_schedule(&self) -> Option<Object> {
        let mut object = self.store.get(&format!("schedule.{}", self.serial))?;
        if let Some(saved) =
            mode(&object.value).and_then(|name| object.local_schedules.as_ref()?.control.get(name)?.saved.as_ref())
        {
            object.value = saved.clone();
        }
        Some(object)
    }

    pub fn schedule_status(&self) -> Status {
        let object = self.configured_schedule();
        let name = object.as_ref().and_then(|o| mode(&o.value));
        let control = object.as_ref().and_then(|o| o.local_schedules.as_ref()?.control.get(name?));
        let shared = self.store.get(&format!("shared.{}", self.serial)).unwrap_or_default();
        let available = object
            .as_ref()
            .is_some_and(|o| has_targets(&o.value) && Self::schedule_mode(&o.value, &shared.value).is_ok());
        Status {
            available,
            enabled: control.is_none_or(|c| c.saved.is_none()),
            hold_until: control.and_then(|c| c.until),
            hold_minutes: object
                .as_ref()
                .and_then(|o| o.local_schedules.as_ref())
                .and_then(|c| c.hold_minutes)
                .unwrap_or(120),
        }
    }

    /// Called with the command lock held. No target mode is changed by a hold.
    pub(crate) fn control_schedule(&self, command: &str, arg: &str, now: i64) -> Result<(), String> {
        let key = format!("schedule.{}", self.serial);
        let device_key = format!("device.{}", self.serial);
        let shared_key = format!("shared.{}", self.serial);
        let status = self.schedule_status();
        let mut minutes = status.hold_minutes;
        let mut target = Fields::new();
        let suspend = match command {
            "schedule_enabled" => !crate::commands::on_off(arg)?,
            "hold" if arg.starts_with('{') => {
                let value: Value = serde_json::from_str(arg).map_err(|e| e.to_string())?;
                let fields = value.as_object().ok_or("expected a hold object")?;
                if fields.keys().any(|k| !["minutes", "temperature", "low", "high"].contains(&k.as_str())) {
                    return Err("unknown hold field".into());
                }
                if let Some(value) = fields.get("minutes") {
                    minutes = duration(&value.to_string())?;
                }
                for (input, output) in [
                    ("temperature", "target_temperature"),
                    ("low", "target_temperature_low"),
                    ("high", "target_temperature_high"),
                ] {
                    if let Some(value) = fields.get(input) {
                        let t = crate::commands::temperature(&value.to_string(), (4.5, 35.0))?;
                        target.insert(output.into(), crate::json::num(t));
                    }
                }
                true
            }
            "hold" => crate::commands::on_off(arg)?,
            "hold_duration" => {
                minutes = duration(arg)?;
                status.hold_until.is_some()
            }
            _ => return Err("unknown schedule control".into()),
        };
        let timed = suspend && command != "schedule_enabled";
        if timed && !clock_ready(now) {
            return Err("set the unit's clock before starting a timed hold".into());
        }
        let until = timed.then_some(now + i64::from(minutes) * 60);
        if let Some(until) = until {
            if !self.configured_schedule().is_some_and(|o| has_targets(&o.value)) {
                return Err("a timed hold needs a saved schedule to resume".into());
            }
            self.arm_hold(until)?;
        }
        let result = self.store.transaction(|objects| {
            let device = objects.get(&device_key).ok_or("no device bucket yet")?.value.clone();
            let shared = objects.get(&shared_key).ok_or("no shared bucket yet")?.value.clone();
            let object = objects.get_mut(&key).ok_or("no schedule bucket yet")?;
            Self::schedule_mode(&object.value, &shared)?;
            let name = mode(&object.value).ok_or("unknown schedule mode")?;
            if !target.is_empty() {
                if name == "RANGE" {
                    if target.contains_key("target_temperature") {
                        return Err("a range hold needs low and high".into());
                    }
                    let low = crate::json::number(
                        target.get("target_temperature_low").or_else(|| shared.get("target_temperature_low")),
                    )
                    .ok_or("no low target")?;
                    let high = crate::json::number(
                        target.get("target_temperature_high").or_else(|| shared.get("target_temperature_high")),
                    )
                    .ok_or("no high target")?;
                    if high - low < schedule::RANGE_GAP {
                        return Err("hold range needs at least 1.5 °C between low and high".into());
                    }
                } else if target.keys().any(|k| k != "target_temperature") {
                    return Err("this hold needs temperature, not low/high".into());
                }
            }
            let catalog = object.local_schedules.get_or_insert_with(Default::default);
            catalog.hold_minutes = Some(minutes);
            if command == "hold_duration" && catalog.control.get(name).is_none_or(|c| c.until.is_none()) {
                return Ok(());
            }
            let control = catalog.control.entry(name.into()).or_default();
            if timed && !has_targets(control.saved.as_ref().unwrap_or(&object.value)) {
                return Err("a timed hold needs a saved schedule to resume".into());
            }
            let mut due = Fields::new();
            if suspend {
                if control.saved.is_none() {
                    control.saved = Some(object.value.clone());
                }
                control.until = until.map(|until| control.dial_cap.map_or(until, |cap| until.min(cap)));
                if control.until.is_none() {
                    control.dial_cap = None;
                }
                control.restored = None;
                control.pending_target = false;
                object.value.insert("days".into(), empty_days());
                if catalog.learning.is_none() {
                    catalog.learning = Some(device.get("learning_mode").cloned().unwrap_or(Value::Null));
                }
            } else if let Some(saved) = control.saved.take() {
                object.value = saved;
                control.until = None;
                control.dial_cap = None;
                control.restored = Some(object.value.clone());
                control.pending_target = !clock_ready(now);
                due = due_target(&object.value, now);
            }
            let suspended = catalog.control.values().any(|c| c.saved.is_some());
            let learning = if suspended { Some(Value::Bool(false)) } else { catalog.learning.take() };
            if let Some(learning) = learning {
                set_learning(objects.get_mut(&device_key).unwrap(), learning, !suspended);
            }
            let shared = &mut objects.get_mut(&shared_key).unwrap().value;
            // Setpoints are harmless in Eco/off; never change their mode selectors.
            due.extend(target.clone());
            if !due.is_empty() {
                shared.extend(due);
                shared.insert("target_change_pending".into(), true.into());
            }
            Ok(())
        });
        self.rearm_hold();
        result
    }

    fn arm_hold(&self, until: i64) -> Result<(), String> {
        let _programming = self.schedule_programming.lock().unwrap();
        #[cfg(test)]
        if self.schedule_alarm.get().is_none() {
            return Ok(());
        }
        let alarm = self.schedule_alarm.get().ok_or("timed holds require a supported wake alarm")?;
        let next = self.next_hold().map_or(until, |at| at.min(until));
        alarm.set(Some(next as f64)).map_err(|e| format!("cannot arm hold wake alarm: {e}"))
    }

    fn next_hold(&self) -> Option<i64> {
        let object = self.store.get(&format!("schedule.{}", self.serial))?;
        object.local_schedules?.control.values().filter_map(|c| c.until).min()
    }

    pub(crate) fn rearm_hold(&self) {
        let _programming = self.schedule_programming.lock().unwrap();
        if let Some(alarm) = self.schedule_alarm.get() {
            let earliest = (chrono::Utc::now().timestamp() + 1).max(*self.hold_retry.lock().unwrap());
            if let Err(e) = alarm.set(self.next_hold().map(|at| at.max(earliest) as f64)) {
                crate::logf!("schedule: wake alarm: {e}");
            }
        }
    }

    pub(crate) fn expire_hold(&self, now: i64) -> Result<(), String> {
        let _applying = self.applying.lock().unwrap();
        if !clock_ready(now) {
            self.rearm_hold();
            return Ok(());
        }
        let pending = self.store.get(&format!("schedule.{}", self.serial)).is_some_and(|o| {
            mode(&o.value)
                .and_then(|name| o.local_schedules.as_ref()?.control.get(name))
                .is_some_and(|c| c.pending_target)
        });
        let result = if pending || self.next_hold().is_some_and(|until| until <= now) {
            self.store.transaction(|objects| {
                let object = objects.get_mut(&format!("schedule.{}", self.serial)).ok_or("no schedule bucket yet")?;
                let current = mode(&object.value);
                let Some(catalog) = object.local_schedules.as_mut() else { return Ok(()) };
                let mut resume_current =
                    current.and_then(|name| catalog.control.get(name)).is_some_and(|c| c.pending_target);
                for (name, control) in &mut catalog.control {
                    if control.until.is_some_and(|until| until <= now) {
                        if Some(name.as_str()) == current {
                            resume_current = true;
                        } else {
                            if let Some(saved) = control.saved.take() {
                                if let Some(week) = catalog.active.get(name).and_then(|h| catalog.named.get_mut(h)) {
                                    week.bucket = saved.clone();
                                }
                                control.restored = Some(saved);
                                control.pending_target = true;
                            }
                            control.until = None;
                            control.dial_cap = None;
                        }
                    }
                }
                if resume_current {
                    resume(objects, &self.serial, now)?;
                } else if catalog.control.values().all(|c| c.saved.is_none()) {
                    if let Some(learning) = catalog.learning.take() {
                        let device =
                            objects.get_mut(&format!("device.{}", self.serial)).ok_or("no device bucket yet")?;
                        set_learning(device, learning, true);
                    }
                }
                Ok(())
            })
        } else {
            Ok(())
        };
        *self.hold_retry.lock().unwrap() = if result.is_err() { now + EXPIRY_RETRY } else { 0 };
        self.rearm_hold();
        result
    }

    /// Ends the current mode's timed hold at the saved week's next change.
    pub(crate) fn cap_hold(&self, now: i64) -> Result<(), String> {
        let key = format!("schedule.{}", self.serial);
        self.store.transaction(|objects| {
            let Some(object) = objects.get_mut(&key) else { return Ok(()) };
            let Some(name) = mode(&object.value) else { return Ok(()) };
            let Some(control) = object.local_schedules.as_mut().and_then(|c| c.control.get_mut(name)) else {
                return Ok(());
            };
            let (Some(until), Some(next)) = (control.until, control.saved.as_ref().and_then(|w| next_change(w, now)))
            else {
                return Ok(());
            };
            let cap = control.dial_cap.map_or(next, |cap| cap.min(next));
            control.dial_cap = Some(cap);
            control.until = Some(until.min(cap));
            Ok(())
        })?;
        self.rearm_hold();
        Ok(())
    }

    /// A target the unit changed itself, from its dial, caps a timed hold.
    pub(crate) fn watch_dial(self: &Arc<Self>) {
        let bridge = Arc::downgrade(self);
        let key = format!("shared.{}", self.serial);
        self.store.watch_device(move |before, o| {
            let moved = ["target_temperature", "target_temperature_low", "target_temperature_high"].iter().any(|t| {
                match (crate::json::number(before.get(*t)), crate::json::number(o.value.get(*t))) {
                    (Some(a), Some(b)) => (a - b).abs() >= 0.05,
                    (a, b) => a.is_some() != b.is_some(),
                }
            });
            if o.key != key || !moved {
                return;
            }
            if let Some(bridge) = bridge.upgrade() {
                if let Err(e) = bridge.cap_hold(chrono::Utc::now().timestamp()) {
                    crate::logf!("schedule: dial during hold: {e}");
                }
            }
        });
    }

    /// Independent of MQTT: the RTC alarm wakes a sleeping unit at expiry.
    pub fn start_schedule_control(self: &Arc<Self>) {
        self.watch_dial();
        match crate::wake::Alarm::new() {
            Ok(alarm) => {
                let alarm = Arc::new(alarm);
                if self.schedule_alarm.set(alarm.clone()).is_ok() {
                    let bridge = self.clone();
                    let runtime = tokio::runtime::Handle::current();
                    std::thread::spawn(move || {
                        let _runtime = runtime.enter();
                        loop {
                            if let Err(e) = alarm.wait() {
                                if e.kind() == std::io::ErrorKind::Interrupted {
                                    continue;
                                }
                                crate::logf!("schedule: alarm wait: {e}");
                                break;
                            }
                            if let Err(e) = bridge.expire_hold(chrono::Utc::now().timestamp()) {
                                crate::logf!("schedule: expiry: {e}");
                            }
                        }
                    });
                }
            }
            Err(e) => crate::logf!("schedule: timed holds unavailable: {e}"),
        }
        self.rearm_hold();
        let bridge = self.clone();
        tokio::spawn(async move {
            let (mut error, mut shown) = (String::new(), std::time::Instant::now());
            loop {
                if let Some((name, arg)) = crate::settings_ui::take_schedule_request() {
                    (error, shown) =
                        (bridge.apply("screen", &name, &arg).err().unwrap_or_default(), std::time::Instant::now());
                }
                if let Err(e) = bridge.expire_hold(chrono::Utc::now().timestamp()) {
                    (error, shown) = (e, std::time::Instant::now());
                }
                if shown.elapsed() > ERROR_SHOWN {
                    error.clear();
                }
                let s = bridge.schedule_status();
                crate::settings_ui::status(
                    "schedule",
                    &format!(
                        "{} {} {} {}\n{}",
                        u8::from(s.available),
                        u8::from(s.enabled),
                        s.hold_until.unwrap_or(0),
                        s.hold_minutes,
                        error
                    ),
                );
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
    }
}

pub(crate) fn resume(
    objects: &mut std::collections::BTreeMap<String, Object>,
    serial: &str,
    now: i64,
) -> Result<(), String> {
    let shared = objects.get(&format!("shared.{serial}")).map(|o| o.value.clone()).unwrap_or_default();
    let object = objects.get_mut(&format!("schedule.{serial}")).ok_or("no schedule bucket yet")?;
    Bridge::schedule_mode(&object.value, &shared)?;
    let name = mode(&object.value).ok_or("unknown schedule mode")?;
    let mut learning = None;
    let mut resumed = false;
    if let Some(catalog) = &mut object.local_schedules {
        if let Some(control) = catalog.control.get_mut(name) {
            if let Some(saved) = control.saved.take() {
                object.value = saved;
                control.restored = Some(object.value.clone());
                resumed = true;
            }
            resumed |= control.pending_target;
            control.until = None;
            control.dial_cap = None;
            if resumed {
                control.pending_target = !clock_ready(now);
            }
        }
        if catalog.control.values().all(|c| c.saved.is_none()) {
            learning = catalog.learning.take();
        }
    }
    let due = if resumed { due_target(&object.value, now) } else { Fields::new() };
    if let Some(learning) = learning {
        set_learning(objects.get_mut(&format!("device.{serial}")).ok_or("no device bucket yet")?, learning, true);
    }
    if !due.is_empty() {
        let shared = &mut objects.get_mut(&format!("shared.{serial}")).ok_or("no shared bucket yet")?.value;
        shared.extend(due);
        shared.insert("target_change_pending".into(), true.into());
    }
    Ok(())
}

/// The saved week's first change after `now`, at the thermostat's local wall time.
fn next_change(bucket: &Fields, now: i64) -> Option<i64> {
    let local = chrono::DateTime::from_timestamp(now, 0)?.with_timezone(&chrono::Local);
    let today = local.weekday().num_days_from_monday() as usize;
    let minute = (local.hour() * 60 + local.minute()) as u16;
    (0..=7).find_map(|ahead| {
        let day = (today + ahead) % 7;
        let times: Vec<u16> = if schedule::kind(bucket) == Some(schedule::Kind::Range) {
            schedule::range_day(bucket, day).into_iter().map(|(m, ..)| m).collect()
        } else {
            schedule::day(bucket, day).into_iter().map(|(m, _)| m).collect()
        };
        let m = times.into_iter().filter(|m| ahead > 0 || *m > minute).min()?;
        let at = (local.date_naive() + chrono::Days::new(ahead as u64)).and_hms_opt(
            u32::from(m / 60),
            u32::from(m % 60),
            0,
        )?;
        chrono::TimeZone::from_local_datetime(&chrono::Local, &at).earliest().map(|t| t.timestamp())
    })
}

fn duration(arg: &str) -> Result<u16, String> {
    arg.parse::<u16>()
        .ok()
        .filter(|m| (1..=1440).contains(m))
        .ok_or_else(|| "expected hold duration, 1-1440 minutes".into())
}

/// Last setpoint at the thermostat's local wall time, wrapping over Sunday.
fn due_target(bucket: &Fields, now: i64) -> Fields {
    if !clock_ready(now) {
        return Fields::new();
    }
    let Some(local) = chrono::DateTime::from_timestamp(now, 0).map(|t| t.with_timezone(&chrono::Local)) else {
        return Fields::new();
    };
    let today = local.weekday().num_days_from_monday() as usize;
    let minute = (local.hour() * 60 + local.minute()) as u16;
    for back in 0..=7 {
        let day = (today + 7 - back) % 7;
        let limit = if back == 0 { minute } else { 1440 };
        if schedule::kind(bucket) == Some(schedule::Kind::Range) {
            if let Some((_, low, high)) = schedule::range_day(bucket, day).into_iter().rev().find(|(m, ..)| *m <= limit)
            {
                return fields! {"target_temperature_low" => crate::json::num(low), "target_temperature_high" => crate::json::num(high)};
            }
        } else if let Some((_, temp)) = schedule::day(bucket, day).into_iter().rev().find(|(m, _)| *m <= limit) {
            return fields! {"target_temperature" => crate::json::num(temp)};
        }
    }
    Fields::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    const NOW: i64 = 1_800_000_000;

    fn week(kind: &str, temp: f64) -> Fields {
        let days: Fields = (0..7).map(|d| (d.to_string(), json!({"0":{"time":0,"temp":crate::json::num(temp),"type":kind,"entry_type":"setpoint","opaque":"keep"}}))).collect();
        fields! {"schedule_mode" => kind, "days" => days, "ver" => 2}
    }

    fn setup(bridge: &Bridge) {
        bridge.store.merge(
            "device.S",
            &fields! {"learning_mode" => true, "eco" => json!({"mode":"schedule"})},
            0,
            0,
            true,
        );
        bridge.store.merge(
            "shared.S",
            &fields! {"target_temperature_type" => "heat", "target_temperature" => 23},
            0,
            0,
            true,
        );
        bridge.store.merge("schedule.S", &week("HEAT", 19.0), 0, 0, true);
    }

    fn bridge(store: Arc<crate::store::Store>) -> Arc<Bridge> {
        Bridge::new(
            "nestlocal",
            "homeassistant",
            "S",
            store,
            Arc::new(crate::weather::Outdoor::default()),
            None,
            false,
            crate::naming::Naming::load("/nonexistent/nestlocal-hold-test.json", "S", None, None),
        )
    }

    #[test]
    fn disable_is_idempotent_and_device_echoes_cannot_replace_the_saved_week() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        let original = b.configured_schedule().unwrap().value;
        b.apply("test", "schedule_enabled", "OFF").unwrap();
        b.apply("test", "schedule_enabled", "OFF").unwrap();
        assert_eq!(b.configured_schedule().unwrap().value, original);
        assert_eq!(b.store.get("schedule.S").unwrap().value["days"], empty_days());
        b.store.merge("schedule.S", &week("HEAT", 25.0), 0, 0, true);
        b.store.merge("device.S", &fields! {"learning_mode" => true}, 0, 0, true);
        assert_eq!(b.configured_schedule().unwrap().value, original);
        assert_eq!(b.store.get("schedule.S").unwrap().value["days"], empty_days());
        assert_eq!(b.store.get("device.S").unwrap().value["learning_mode"], false);
        b.apply("test", "schedule_enabled", "ON").unwrap();
        assert_eq!(b.store.get("schedule.S").unwrap().value, original);
        assert_eq!(b.store.get("device.S").unwrap().value["learning_mode"], true);
        b.store.merge("schedule.S", &fields! {"ver" => 2}, 0, 0, true);
        b.store.merge("schedule.S", &fields! {"days" => empty_days()}, 0, 0, true);
        assert_eq!(b.store.get("schedule.S").unwrap().value, original, "late empty echo is rejected");
        b.store.merge("schedule.S", &original, 0, 0, true);
        b.store.merge("schedule.S", &week("HEAT", 20.0), 0, 0, true);
        assert_eq!(
            b.configured_schedule().unwrap().value,
            week("HEAT", 20.0),
            "native edits work after restore acknowledgement"
        );
    }

    #[test]
    fn a_rejected_subscribe_week_has_a_newer_timestamp() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        b.control_schedule("schedule_enabled", "OFF", NOW).unwrap();
        let timestamp = b.store.get("schedule.S").unwrap().timestamp + 1000;
        let (corrected, _) = b.store.merge("schedule.S", &week("HEAT", 25.0), 0, timestamp, true);
        assert!(corrected.timestamp > timestamp);
        assert_eq!(corrected.value["days"], empty_days());
    }

    #[test]
    fn restored_learning_rejects_late_suspension_echoes_until_acknowledged() {
        for expire in [false, true] {
            let b = crate::bridge::tests::test_bridge();
            setup(&b);
            b.control_schedule("hold", r#"{"minutes":1}"#, NOW).unwrap();
            if expire {
                b.expire_hold(NOW + 60).unwrap();
            } else {
                b.control_schedule("hold", "OFF", NOW).unwrap();
            }
            b.store.merge("device.S", &fields! {"learning_mode" => false}, 0, 0, true);
            assert_eq!(b.store.get("device.S").unwrap().value["learning_mode"], true);
            b.store.merge("device.S", &fields! {"learning_mode" => true}, 0, 0, true);
            b.store.merge("device.S", &fields! {"learning_mode" => false}, 0, 0, true);
            assert_eq!(b.store.get("device.S").unwrap().value["learning_mode"], false);
        }
    }

    #[test]
    fn timed_hold_renews_without_losing_the_week_and_allows_dial_changes() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        b.control_schedule("schedule_enabled", "OFF", NOW).unwrap();
        b.control_schedule("hold", r#"{"minutes":10,"temperature":21}"#, NOW).unwrap();
        assert_eq!(b.schedule_status().hold_until, Some(NOW + 600));
        b.control_schedule("hold_duration", "20", NOW + 200).unwrap();
        assert_eq!(b.schedule_status().hold_until, Some(NOW + 1400));
        b.store.merge("shared.S", &fields! {"target_temperature" => 22}, 0, 0, true);
        b.expire_hold(NOW + 1399).unwrap();
        assert_eq!(b.store.get("shared.S").unwrap().value["target_temperature"], 22);
        b.expire_hold(NOW + 1400).unwrap();
        assert!(b.schedule_status().enabled);
        assert_eq!(b.schedule_status().hold_minutes, 20);
        assert_eq!(b.store.get("shared.S").unwrap().value["target_temperature"], 19);
        assert_eq!(b.store.get("device.S").unwrap().value["learning_mode"], true);
    }

    #[test]
    fn expiry_preserves_eco_and_hvac_off() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        b.store.merge("device.S", &fields! {"eco" => json!({"mode":"manual-eco"})}, 0, 0, true);
        b.store.merge("shared.S", &fields! {"target_temperature_type" => "off"}, 0, 0, true);
        b.control_schedule("hold", "ON", NOW).unwrap();
        b.expire_hold(NOW + 120 * 60).unwrap();
        assert_eq!(b.store.get("shared.S").unwrap().value["target_temperature_type"], "off");
        assert_eq!(b.store.get("device.S").unwrap().value["eco"]["mode"], "manual-eco");
        assert_eq!(b.store.get("shared.S").unwrap().value["target_temperature"], 19);
    }

    #[test]
    fn paused_edits_change_the_saved_week_and_raw_writes_are_refused() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        b.apply("test", "schedule_enabled", "OFF").unwrap();
        b.apply("test", "heating_times", r#"{"mon":[{"time":"06:00","temp":20}]}"#).unwrap();
        assert_eq!(schedule::day(&b.configured_schedule().unwrap().value, 0), [(360, 20.0)]);
        assert_eq!(b.store.get("schedule.S").unwrap().value["days"], empty_days());
        assert!(b.apply("test", "raw/schedule", r#"{"days":{}}"#).is_err());
        assert!(b.apply("test", "learning_mode", "ON").is_err());
        b.apply("test", "hold", "OFF").unwrap();
        assert_eq!(schedule::day(&b.store.get("schedule.S").unwrap().value, 0), [(360, 20.0)]);
    }

    #[test]
    fn a_failed_durable_write_changes_neither_native_nor_saved_state() {
        let b = bridge(crate::store::Store::new("/nonexistent/nestlocal-hold-test/store.json"));
        setup(&b);
        assert!(b.apply("test", "schedule_enabled", "OFF").is_err());
        assert_eq!(b.store.get("schedule.S").unwrap().value, week("HEAT", 19.0));
        assert!(b.store.get("schedule.S").unwrap().local_schedules.is_none());
        assert_eq!(b.store.get("device.S").unwrap().value["learning_mode"], true);
    }

    #[test]
    fn restart_recovers_the_week_and_expires_an_overdue_hold() {
        let path = std::env::temp_dir().join(format!(
            "nestlocal-hold-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let b = bridge(crate::store::Store::new(&path));
        setup(&b);
        b.control_schedule("hold", r#"{"minutes":1}"#, NOW).unwrap();
        let restarted = bridge(crate::store::Store::new(&path));
        assert_eq!(restarted.configured_schedule().unwrap().value, week("HEAT", 19.0));
        restarted.expire_hold(NOW + 61).unwrap();
        assert!(restarted.schedule_status().enabled);
        assert_eq!(restarted.store.get("schedule.S").unwrap().value, week("HEAT", 19.0));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_dial_turn_ends_the_hold_at_the_next_scheduled_change_and_renewals_keep_that_end() {
        let at = |h, m| chrono::Local.with_ymd_and_hms(2026, 11, 23, h, m, 0).single().unwrap().timestamp();
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        b.apply("test", "heating_times", r#"{"mon":[{"time":"00:00","temp":17},{"time":"07:00","temp":20}]}"#).unwrap();
        b.control_schedule("hold", r#"{"minutes":240}"#, at(6, 0)).unwrap();
        assert_eq!(b.schedule_status().hold_until, Some(at(10, 0)));
        b.cap_hold(at(6, 30)).unwrap();
        assert_eq!(b.schedule_status().hold_until, Some(at(7, 0)));
        b.control_schedule("hold", "ON", at(6, 40)).unwrap();
        b.control_schedule("hold_duration", "600", at(6, 45)).unwrap();
        assert_eq!(b.schedule_status().hold_until, Some(at(7, 0)), "renewals cannot pass the scheduled change");
        b.expire_hold(at(7, 0)).unwrap();
        assert!(b.schedule_status().enabled);
        assert_eq!(b.store.get("shared.S").unwrap().value["target_temperature"], 20);
        b.control_schedule("hold", "ON", at(7, 10)).unwrap();
        assert_eq!(b.schedule_status().hold_until, Some(at(17, 10)), "a new hold starts uncapped");
        b.control_schedule("schedule_enabled", "OFF", at(7, 20)).unwrap();
        b.cap_hold(at(7, 30)).unwrap();
        assert_eq!(b.schedule_status().hold_until, None, "an indefinite suspension has no end to cap");
    }

    #[test]
    fn only_the_unit_changing_its_own_target_caps_a_hold() {
        let now = chrono::Utc::now().timestamp();
        let midnight = chrono::Local
            .from_local_datetime(
                &(chrono::Local::now().date_naive() + chrono::Days::new(1)).and_hms_opt(0, 0, 0).unwrap(),
            )
            .earliest()
            .unwrap()
            .timestamp();
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        b.watch_dial();
        b.control_schedule("hold", r#"{"minutes":1440}"#, now).unwrap();
        let until = b.schedule_status().hold_until;
        b.store.merge(
            "shared.S",
            &fields! {"target_temperature" => 23.0, "target_change_pending" => false},
            0,
            0,
            true,
        );
        b.apply("test", "target", "22").unwrap();
        b.store.merge("shared.S", &fields! {"target_temperature" => 22}, 0, 0, true);
        assert_eq!(b.schedule_status().hold_until, until, "echoes and controller targets keep the hold");
        b.store.merge("shared.S", &fields! {"target_temperature" => 24}, 0, 0, true);
        assert_eq!(b.schedule_status().hold_until, Some(midnight));
    }

    #[test]
    fn resuming_an_unsuspended_mode_changes_neither_target_nor_week_protection() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        b.control_schedule("hold", "ON", NOW).unwrap();
        b.control_schedule("hold", "OFF", NOW).unwrap();
        b.store.merge("schedule.S", &week("HEAT", 19.0), 0, 0, true);
        b.store.merge("shared.S", &fields! {"target_temperature" => 22}, 0, 0, true);
        b.store.transaction(|objects| resume(objects, "S", NOW)).unwrap();
        assert_eq!(b.store.get("shared.S").unwrap().value["target_temperature"], 22);
        b.store.merge("schedule.S", &week("HEAT", 21.0), 0, 0, true);
        assert_eq!(b.store.get("schedule.S").unwrap().value, week("HEAT", 21.0), "edits on the unit are kept");
    }

    #[test]
    fn after_a_restore_only_an_empty_device_week_is_taken_for_the_suspended_one() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        b.control_schedule("schedule_enabled", "OFF", NOW).unwrap();
        b.control_schedule("schedule_enabled", "ON", NOW).unwrap();
        b.store.merge("schedule.S", &fields! {"days" => empty_days()}, 0, 0, true);
        assert_eq!(b.store.get("schedule.S").unwrap().value, week("HEAT", 19.0), "late empty echo");
        b.store.merge("schedule.S", &week("HEAT", 21.0), 0, 0, true);
        assert_eq!(b.store.get("schedule.S").unwrap().value, week("HEAT", 21.0), "an edit on the unit");
        b.store.merge("schedule.S", &fields! {"days" => empty_days()}, 0, 0, true);
        assert_eq!(b.store.get("schedule.S").unwrap().value["days"], empty_days(), "protection ended");
    }

    #[test]
    fn restored_learning_is_protected_only_for_its_guard() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        b.control_schedule("schedule_enabled", "OFF", NOW).unwrap();
        b.control_schedule("schedule_enabled", "ON", NOW).unwrap();
        b.store.merge("device.S", &fields! {"learning_mode" => false}, 0, 0, true);
        assert_eq!(b.store.get("device.S").unwrap().value["learning_mode"], true);
        b.store
            .update("device.S", 0, 0, false, |_, o| {
                o.restored_learning.as_mut().unwrap().until = chrono::Utc::now().timestamp();
                Ok(())
            })
            .unwrap();
        b.store.merge("device.S", &fields! {"learning_mode" => false}, 0, 0, true);
        assert_eq!(b.store.get("device.S").unwrap().value["learning_mode"], false, "turned off on the unit");
        assert!(b.store.get("device.S").unwrap().restored_learning.is_none());
    }

    #[test]
    fn one_hold_duration_serves_every_mode() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        b.apply("test", "hold_duration", "30").unwrap();
        b.store.merge("shared.S", &fields! {"target_temperature_type" => "cool"}, 0, 0, true);
        b.store.merge("schedule.S", &week("COOL", 24.0), 0, 0, true);
        assert_eq!(b.schedule_status().hold_minutes, 30);
        b.control_schedule("hold", "ON", NOW).unwrap();
        assert_eq!(b.schedule_status().hold_until, Some(NOW + 30 * 60));
    }

    #[test]
    fn absent_learning_stays_absent_after_a_restart_during_suspension() {
        let path = std::env::temp_dir().join(format!(
            "nestlocal-hold-learning-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let b = bridge(crate::store::Store::new(&path));
        setup(&b);
        b.store
            .update("device.S", 0, 0, true, |_, o| {
                o.value.remove("learning_mode");
                Ok(())
            })
            .unwrap();
        b.control_schedule("schedule_enabled", "OFF", NOW).unwrap();
        assert_eq!(b.store.get("device.S").unwrap().value["learning_mode"], false);
        let restarted = bridge(crate::store::Store::new(&path));
        restarted.control_schedule("schedule_enabled", "ON", NOW).unwrap();
        assert!(!restarted.store.get("device.S").unwrap().value.contains_key("learning_mode"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn schedule_edits_without_local_state_changes_wait_for_the_batched_save() {
        let b = bridge(crate::store::Store::new("/nonexistent/nestlocal-hold-test/store.json"));
        setup(&b);
        b.apply("test", "heating_times", r#"{"mon":[{"time":"06:00","temp":20}]}"#).unwrap();
        assert_eq!(schedule::day(&b.store.get("schedule.S").unwrap().value, 0), [(360, 20.0)]);
    }

    #[test]
    fn modes_keep_independent_weeks_and_inactive_holds_expire_without_changing_targets() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        b.store
            .update("schedule.S", 0, 0, false, |_, o| {
                let catalog = o.local_schedules.get_or_insert_with(Default::default);
                catalog.native.insert("HEAT".into(), week("HEAT", 17.0));
                catalog.active.insert("HEAT".into(), "named".into());
                catalog.named.insert(
                    "named".into(),
                    schedule::SavedWeek { name: Some("Named".into()), bucket: week("HEAT", 19.0) },
                );
                Ok(())
            })
            .unwrap();
        b.control_schedule("hold", r#"{"minutes":1}"#, NOW).unwrap();
        b.store.merge(
            "shared.S",
            &fields! {"target_temperature_type" => "cool", "target_temperature" => 25},
            0,
            0,
            true,
        );
        assert!(!b.schedule_status().available);
        assert!(b.control_schedule("hold", "ON", NOW + 1).is_err());
        b.store.merge("schedule.S", &week("COOL", 24.0), 0, 0, true);
        assert!(b.schedule_status().enabled);
        b.control_schedule("schedule_enabled", "OFF", NOW + 1).unwrap();
        b.expire_hold(NOW + 61).unwrap();
        assert!(!b.schedule_status().enabled, "cool stays indefinitely disabled");
        assert_eq!(b.store.get("schedule.S").unwrap().local_schedules.unwrap().native["HEAT"], week("HEAT", 17.0));
        assert_eq!(b.store.get("shared.S").unwrap().value["target_temperature"], 25);
        b.store.merge("shared.S", &fields! {"target_temperature_type" => "heat"}, 0, 0, true);
        b.store.merge("schedule.S", &fields! {"schedule_mode" => "HEAT", "days" => empty_days()}, 0, 0, true);
        assert!(b.schedule_status().enabled, "heat expired while inactive");
        b.expire_hold(NOW + 62).unwrap();
        assert_eq!(b.store.get("shared.S").unwrap().value["target_temperature"], 19);
        b.store.merge("shared.S", &fields! {"target_temperature" => 22}, 0, 0, true);
        b.expire_hold(NOW + 63).unwrap();
        assert_eq!(
            b.store.get("shared.S").unwrap().value["target_temperature"],
            22,
            "pending target applies only once"
        );
        assert_eq!(b.configured_schedule().unwrap().value["days"], week("HEAT", 19.0)["days"]);
        b.store.merge("shared.S", &fields! {"target_temperature_type" => "cool"}, 0, 0, true);
        b.store.merge("schedule.S", &week("COOL", 26.0), 0, 0, true);
        assert_eq!(b.configured_schedule().unwrap().value, week("COOL", 24.0));
    }

    #[test]
    fn inactive_expiry_pending_target_survives_restart_and_waits_for_matching_mode() {
        let path = std::env::temp_dir().join(format!(
            "nestlocal-hold-pending-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let b = bridge(crate::store::Store::new(&path));
        setup(&b);
        b.control_schedule("hold", r#"{"minutes":1}"#, NOW).unwrap();
        b.store.merge(
            "shared.S",
            &fields! {"target_temperature_type" => "cool", "target_temperature" => 25},
            0,
            0,
            true,
        );
        b.store.merge("schedule.S", &week("COOL", 24.0), 0, 0, true);
        b.expire_hold(NOW + 61).unwrap();
        assert_eq!(b.next_hold(), None, "inactive expiry has no overdue wake deadline");
        let restarted = bridge(crate::store::Store::new(&path));
        assert!(restarted.store.get("schedule.S").unwrap().local_schedules.unwrap().control["HEAT"].pending_target);
        restarted.store.merge("schedule.S", &fields! {"schedule_mode" => "HEAT", "days" => empty_days()}, 0, 0, true);
        assert!(restarted.expire_hold(NOW + 62).is_err(), "shared still says cool");
        assert_eq!(restarted.store.get("shared.S").unwrap().value["target_temperature"], 25);
        restarted.store.merge("shared.S", &fields! {"target_temperature_type" => "heat"}, 0, 0, true);
        restarted.expire_hold(946_684_800).unwrap();
        assert_eq!(
            restarted.store.get("shared.S").unwrap().value["target_temperature"],
            25,
            "unset clock defers the target"
        );
        assert!(restarted.store.get("schedule.S").unwrap().local_schedules.unwrap().control["HEAT"].pending_target);
        restarted.expire_hold(NOW + 63).unwrap();
        assert_eq!(restarted.store.get("shared.S").unwrap().value["target_temperature"], 19);
        let again = bridge(crate::store::Store::new(&path));
        assert!(!again.store.get("schedule.S").unwrap().local_schedules.unwrap().control["HEAT"].pending_target);
        again.store.merge("shared.S", &fields! {"target_temperature" => 22}, 0, 0, true);
        again.expire_hold(NOW + 64).unwrap();
        assert_eq!(again.store.get("shared.S").unwrap().value["target_temperature"], 22);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn editing_a_timed_hold_cannot_remove_its_fallback_week() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        b.control_schedule("hold", "ON", NOW).unwrap();
        let before = b.configured_schedule().unwrap();
        let clear: Fields = schedule::DAYS.iter().map(|name| ((*name).into(), json!([]))).collect();
        assert!(b.apply("test", "heating_times", &Value::Object(clear).to_string()).is_err());
        let after = b.configured_schedule().unwrap();
        assert_eq!(after.value, before.value);
        assert_eq!(after.local_schedules, before.local_schedules);
        assert_eq!(b.store.get("schedule.S").unwrap().value["days"], empty_days());
    }

    #[test]
    fn due_target_wraps_a_week_and_range_targets_keep_both_limits() {
        let monday = chrono::Local.with_ymd_and_hms(2026, 11, 23, 6, 0, 0).single().unwrap().timestamp();
        let b = fields! {"schedule_mode" => "HEAT", "days" => json!({"0":{"0":{"time":43200,"temp":18}}})};
        assert_eq!(due_target(&b, monday)["target_temperature"], 18);
        let b = fields! {"schedule_mode" => "RANGE", "days" => json!({"6":{"0":{"time":43200,"temp-min":18,"temp-max":25}}})};
        assert_eq!(due_target(&b, monday), fields! {"target_temperature_low" => 18, "target_temperature_high" => 25});
    }

    #[test]
    fn both_alarm_programming_paths_wait_for_the_deadline_lock() {
        let b = crate::bridge::tests::test_bridge();
        let programming = b.schedule_programming.lock().unwrap();
        let (started, attempts) = std::sync::mpsc::channel();
        let (finished, completions) = std::sync::mpsc::channel();
        let threads: Vec<_> = [false, true]
            .into_iter()
            .map(|arming| {
                let b = b.clone();
                let started = started.clone();
                let finished = finished.clone();
                std::thread::spawn(move || {
                    started.send(()).unwrap();
                    if arming {
                        b.arm_hold(NOW).unwrap();
                    } else {
                        b.rearm_hold();
                    }
                    finished.send(()).unwrap();
                })
            })
            .collect();
        attempts.recv().unwrap();
        attempts.recv().unwrap();
        assert!(completions.recv_timeout(std::time::Duration::from_millis(100)).is_err());
        drop(programming);
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(completions.try_iter().count(), 2);
    }

    #[test]
    fn timed_holds_require_a_restorable_week_and_a_sane_clock() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        assert!(b.control_schedule("hold", "ON", 946_684_800).unwrap_err().contains("clock"));
        assert!(b.control_schedule("hold", "ON", i64::MAX).unwrap_err().contains("clock"));
        assert!(b.schedule_status().enabled);
        b.store.merge("schedule.S", &fields! {"days" => empty_days()}, 0, 0, true);
        assert!(!b.schedule_status().available);
        assert!(b.control_schedule("hold", "ON", NOW).unwrap_err().contains("saved schedule"));
        assert!(b.schedule_status().enabled);
    }

    #[test]
    fn failed_expiry_keeps_the_durable_backup_and_retries() {
        let path = std::env::temp_dir().join(format!(
            "nestlocal-hold-failure-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let b = bridge(crate::store::Store::new(&path));
        setup(&b);
        b.control_schedule("hold", r#"{"minutes":1}"#, NOW).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(b.expire_hold(NOW + 61).is_err());
        assert_eq!(*b.hold_retry.lock().unwrap(), NOW + 61 + EXPIRY_RETRY, "the wake alarm backs off");
        assert_eq!(b.schedule_status().hold_until, Some(NOW + 60));
        assert_eq!(b.store.get("schedule.S").unwrap().value["days"], empty_days());
        assert_eq!(b.configured_schedule().unwrap().value, week("HEAT", 19.0));
        std::fs::remove_dir(&path).unwrap();
        b.expire_hold(NOW + 62).unwrap();
        assert!(b.schedule_status().enabled);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn paused_native_editor_requires_resume_but_other_raw_device_settings_work() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        b.apply("test", "schedule_enabled", "OFF").unwrap();
        b.store.merge("schedule.S", &fields! {"days" => empty_days()}, 0, 0, true);
        b.store.merge("schedule.S", &fields! {"days" => json!({"0":{"0":{"time":43200,"temp":23}}})}, 0, 0, true);
        assert_eq!(b.configured_schedule().unwrap().value, week("HEAT", 19.0));
        assert_eq!(b.store.get("schedule.S").unwrap().value["days"], empty_days());
        b.apply("test", "raw/device", r#"{"temperature_scale":"F"}"#).unwrap();
        assert_eq!(b.store.get("device.S").unwrap().value["temperature_scale"], "F");
        assert!(b.apply("test", "raw/device", r#"{"learning_mode":true}"#).is_err());
    }

    #[test]
    fn invalid_hold_is_atomic_and_duration_without_a_hold_only_remembers() {
        let b = crate::bridge::tests::test_bridge();
        setup(&b);
        for arg in ["0", "1441", "-1", "1.5"] {
            assert!(b.apply("test", "hold_duration", arg).is_err());
        }
        assert!(b.apply("test", "hold", r#"{"minutes":5,"temperature":99}"#).is_err());
        b.apply("test", "hold_duration", "1440").unwrap();
        assert!(b.schedule_status().enabled);
        assert_eq!(b.schedule_status().hold_until, None);
        assert_eq!(b.schedule_status().hold_minutes, 1440);
        assert_eq!(b.store.get("schedule.S").unwrap().value, week("HEAT", 19.0));
    }
}
