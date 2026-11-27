//! <base>/set/<name>: Home Assistant's commands, as bucket fields that the
//! store merges and the held subscribe pushes to nlclient.

use serde_json::Value;

use crate::bridge::Bridge;
use crate::fields;
use crate::json::{num, unmarshal_object, Fields};
use crate::logf;
use crate::weather::HAWeather;

/// Marks a change as made by the unit's own owner, as the Nest app's
/// changes were (eco.touched_by and friends).
const TOUCHED_BY_LOCAL: i64 = 4;

/// Setpoints, °C.
const SETPOINT: (f64, f64) = (4.5, 35.0);
/// Eco's heat and cool limits, °C: what the commands take and Home
/// Assistant's number entities offer.
pub const ECO_LOW: (f64, f64) = (4.5, 21.0);
pub const ECO_HIGH: (f64, f64) = (24.0, 32.0);

/// What a command does: change a bucket, or nothing more (the outdoor
/// weather, which is no bucket).
pub type Change = Option<(String, Fields)>;

pub(crate) fn on_off(arg: &str) -> Result<bool, String> {
    match arg.to_uppercase().as_str() {
        "ON" | "TRUE" | "1" => Ok(true),
        "OFF" | "FALSE" | "0" => Ok(false),
        _ => Err("expected ON or OFF".into()),
    }
}

/// A finite float, as strconv.ParseFloat read the numbers that matter here.
fn float(arg: &str) -> Option<f64> {
    arg.parse::<f64>().ok().filter(|f| f.is_finite())
}

/// A temperature in °C within (low, high), to the unit's half degrees.
pub(crate) fn temperature(arg: &str, (low, high): (f64, f64)) -> Result<f64, String> {
    match float(arg) {
        Some(t) if (low..=high).contains(&t) => Ok((t * 2.0).round() / 2.0),
        _ => Err(format!("expected a temperature, {low}-{high} °C")),
    }
}

/// An hour of the day: "7", "07" or "07:00".
fn hour(arg: &str) -> Result<i64, String> {
    let h = arg.strip_suffix(":00").unwrap_or(arg);
    match h.parse::<i64>() {
        Ok(h) if (0..24).contains(&h) => Ok(h),
        _ => Err("expected an hour, 00:00-23:00".into()),
    }
}

fn shared(fields: Fields) -> Result<Change, String> {
    Ok(Some(("shared".into(), fields)))
}

fn device(fields: Fields) -> Result<Change, String> {
    Ok(Some(("device".into(), fields)))
}

impl Bridge {
    /// Handles one command: the change it makes is merged into this unit's
    /// bucket, which then reaches nlclient on its held subscribe.
    pub fn command(&self, topic: &str, payload: &[u8]) {
        let name = topic.strip_prefix(&format!("{}/set/", self.base)).unwrap_or(topic);
        let arg = String::from_utf8_lossy(payload).trim().to_string();
        let _ = self.apply("mqtt", name, &arg);
    }

    /// Applies one command, from MQTT, Matter or Zigbee (`from`, for the log).
    pub fn apply(&self, from: &str, name: &str, arg: &str) -> Result<(), String> {
        self.apply_in_mode(from, name, arg, None)
    }

    pub fn apply_in_mode(
        &self,
        from: &str,
        name: &str,
        arg: &str,
        expected: Option<crate::schedule::Kind>,
    ) -> Result<(), String> {
        let _applying = self.applying.lock().unwrap();
        if matches!(name, "schedule_enabled" | "hold" | "hold_duration") {
            return self
                .control_schedule(name, arg, chrono::Utc::now().timestamp())
                .inspect_err(|e| logf!("{from}: {name} {arg:?}: {e}"));
        }
        let raw_learning = name == "raw/device"
            && unmarshal_object(arg.as_bytes()).ok().flatten().is_some_and(|f| f.contains_key("learning_mode"));
        if matches!(name, "raw/schedule" | "clear_schedule" | "learning_mode") || raw_learning {
            let suspended = self
                .store
                .get(&format!("schedule.{}", self.serial))
                .and_then(|o| o.local_schedules)
                .is_some_and(|c| c.control.values().any(|s| s.saved.is_some()));
            if suspended {
                let error = "resume the schedule before changing raw schedule or learning settings";
                logf!("{from}: {name}: {error}");
                return Err(error.into());
            }
        }
        let schedule = if matches!(name, "heating_times" | "schedule") {
            let snapshot = self.configured_schedule().ok_or("no schedule bucket yet")?;
            if expected.is_some() && crate::schedule::kind(&snapshot.value) != expected {
                return Err("schedule mode changed during the write".into());
            }
            Some(snapshot)
        } else {
            None
        };
        let (bucket, fields) = match self.command_fields(from, name, arg, schedule.as_ref().map(|s| &s.value)) {
            Err(e) => {
                logf!("{from}: {name} {arg:?}: {e}");
                return Err(e);
            }
            Ok(None) => return Ok(()),
            Ok(Some(change)) => change,
        };
        if let Some(snapshot) = schedule {
            return self
                .replace_schedule(&snapshot, fields.get("days"), snapshot.local_schedules.clone())
                .inspect_err(|e| logf!("{from}: {name}: {e}"));
        }
        // The hot water schedule's bucket is named whole (custom_schedule.<name>),
        // not after the serial; every other command is the serial's.
        let key = if name == "hot_water_times" { bucket.clone() } else { format!("{bucket}.{}", self.serial) };
        let mut changes = vec![(key, fields)];
        if name == "clear_schedule" {
            // Learning off too, or the unit would rebuild the schedule.
            changes.push((format!("device.{}", self.serial), fields! {"learning_mode" => false}));
        }
        if let Some((key, _)) = changes.iter().find(|(key, _)| self.store.get(key).is_none()) {
            logf!("{from}: {name}: no {key} bucket yet");
            return Err(format!("no {key} bucket yet"));
        }
        // nlclient takes the screen timeout and brightness from their files
        // when the change makes it commit: the files come first.
        let screen = &changes[0].1;
        if bucket == "device"
            && (screen.contains_key(crate::screen::TIMEOUT) || screen.contains_key(crate::screen::ALWAYS_ON))
        {
            crate::screen::publish_with(&self.store, &self.serial, screen);
        }
        if bucket == "device" && crate::brightness::changes(screen) {
            crate::brightness::publish_with(&self.store, &self.serial, screen);
        }
        if changes.iter().any(|(key, fields)| self.display_change(key, fields)) {
            // Publish before notifying held subscribes: nlclient consumes the
            // request when its bucket parser has applied the new settings.
            if let Err(e) = crate::settings_ui::request_wake() {
                logf!("display wake: {e}");
            }
        }
        for (key, fields) in &changes {
            logf!("{from}: {name} {arg:?} -> {key} {}", Value::Object(fields.clone()));
            self.store.merge(key, fields, 0, 0, false);
        }
        Ok(())
    }

    /// Installs the days and the Matter handles naming them as one store change.
    pub fn replace_schedule(
        &self,
        expected: &crate::store::Object,
        days: Option<&Value>,
        catalog: Option<crate::schedule::Catalog>,
    ) -> Result<(), String> {
        self.replace_schedule_inner(expected, days, catalog, false)
    }

    /// Selecting a Matter schedule installs and resumes it in one durable write.
    pub fn replace_schedule_active(
        &self,
        expected: &crate::store::Object,
        days: Option<&Value>,
        catalog: Option<crate::schedule::Catalog>,
    ) -> Result<(), String> {
        self.replace_schedule_inner(expected, days, catalog, true)
    }

    fn replace_schedule_inner(
        &self,
        expected: &crate::store::Object,
        days: Option<&Value>,
        mut catalog: Option<crate::schedule::Catalog>,
        resume: bool,
    ) -> Result<(), String> {
        let shared_key = format!("shared.{}", self.serial);
        self.store.transaction(|objects| {
            let shared = objects.get(&shared_key).map(|o| o.value.clone()).unwrap_or_default();
            let o = objects.get_mut(&expected.key).ok_or("no schedule bucket yet")?;
            if o.revision != expected.revision || o.local_schedules != expected.local_schedules {
                return Err("schedule changed during the write".into());
            }
            Self::schedule_mode(&o.value, &shared)?;
            let native = o.value.clone();
            let name = crate::schedule_control::mode(&o.value).ok_or("unknown schedule mode")?;
            if let Some(previous) = &o.local_schedules {
                if !previous.control.is_empty() || previous.learning.is_some() {
                    let c = catalog.get_or_insert_with(Default::default);
                    c.control = previous.control.clone();
                    c.learning = previous.learning.clone();
                    for (mode, handle) in &previous.active {
                        if mode == name {
                            continue;
                        }
                        let Some(week) = c.named.get(handle) else { continue };
                        let Some(control) = c.control.get_mut(mode) else { continue };
                        if previous.named.get(handle).map(|w| &w.bucket) == Some(&week.bucket) {
                            continue;
                        }
                        if control.saved.is_some() {
                            if control.until.is_some() && !crate::schedule_control::has_targets(&week.bucket) {
                                return Err(
                                    "resume the schedule before removing its last target during a timed hold".into()
                                );
                            }
                            control.saved = Some(week.bucket.clone());
                        }
                        if control.restored.is_some() {
                            control.restored = Some(week.bucket.clone());
                        }
                    }
                }
            }
            o.local_schedules = catalog;
            if let Some(days) = days {
                let days = crate::json::canonical(days.clone());
                let timed =
                    o.local_schedules.as_ref().and_then(|c| c.control.get(name)).is_some_and(|c| c.until.is_some());
                let saved =
                    o.local_schedules.as_mut().and_then(|c| c.control.get_mut(name)).and_then(|c| c.saved.as_mut());
                if let Some(saved) = saved {
                    saved.insert("days".into(), days);
                    if timed && !resume && !crate::schedule_control::has_targets(saved) {
                        return Err("resume the schedule before removing its last target during a timed hold".into());
                    }
                } else {
                    o.value.insert("days".into(), days);
                    if let Some(control) = o.local_schedules.as_mut().and_then(|c| c.control.get_mut(name)) {
                        control.restored = None;
                    }
                }
            }
            if resume {
                crate::schedule_control::resume(objects, &self.serial, chrono::Utc::now().timestamp())?;
            }
            if objects[&expected.key].value != native || objects.get(&shared_key).is_some_and(|o| o.value != shared) {
                let _ = crate::settings_ui::request_wake();
            }
            Ok(())
        })?;
        self.rearm_hold();
        Ok(())
    }

    pub(crate) fn schedule_mode(bucket: &Fields, shared: &Fields) -> Result<&'static str, String> {
        let kind = crate::schedule::kind(bucket).ok_or("unknown schedule mode")?;
        let (mode, needs_heat, needs_cool) = match kind {
            crate::schedule::Kind::Heat => ("heat", true, false),
            crate::schedule::Kind::Cool => ("cool", false, true),
            crate::schedule::Kind::Range => ("range", true, true),
        };
        if let Some(running @ ("heat" | "cool" | "range")) =
            shared.get("target_temperature_type").and_then(Value::as_str)
        {
            if mode != running {
                return Err(format!("waiting for the {running} schedule"));
            }
        }
        let heat = crate::json::truthy(shared.get("can_heat"));
        let cool = crate::json::truthy(shared.get("can_cool"));
        if (heat || cool) && ((needs_heat && !heat) || (needs_cool && !cool)) {
            return Err(format!("the equipment cannot run the {mode} schedule"));
        }
        Ok(mode)
    }

    fn display_change(&self, key: &str, fields: &Fields) -> bool {
        if !["device.", "shared.", "schedule."].iter().any(|prefix| key.starts_with(prefix)) {
            return false;
        }
        let Some(current) = self.store.get(key) else {
            return false;
        };
        fields.iter().any(|(name, value)| {
            if name == "eco" {
                current.value.get(name).and_then(|v| v.get("mode")) != value.get("mode")
            } else {
                name != "target_change_pending"
                    && current.value.get(name) != Some(&crate::json::canonical(value.clone()))
            }
        })
    }

    #[cfg(test)]
    pub fn fields(&self, name: &str, arg: &str) -> Result<Change, String> {
        let bucket = self.store.get(&format!("schedule.{}", self.serial));
        self.command_fields("mqtt", name, arg, bucket.as_ref().map(|s| &s.value))
    }

    fn command_fields(&self, from: &str, name: &str, arg: &str, schedule: Option<&Fields>) -> Result<Change, String> {
        let now = chrono::Utc::now().timestamp();
        if let Some(bucket) = name.strip_prefix("raw/") {
            return match unmarshal_object(arg.as_bytes()) {
                Ok(Some(f)) => Ok(Some((bucket.into(), f))),
                Ok(None) => Ok(None),
                Err(_) => Err("expected a JSON object".into()),
            };
        }

        match name {
            "mode" => {
                let t = match arg {
                    "off" => "off",
                    "heat" => "heat",
                    "cool" => "cool",
                    "heat_cool" => "range",
                    _ => return Err("unknown mode".into()),
                };
                shared(fields! {"target_temperature_type" => t, "target_change_pending" => true})
            }

            "target" | "target_low" | "target_high" => {
                let t = temperature(arg, SETPOINT)?;
                let field = match name {
                    "target" => "target_temperature",
                    "target_low" => "target_temperature_low",
                    _ => "target_temperature_high",
                };
                shared(fields! {field => num(t), "target_change_pending" => true})
            }

            "fan_mode" => match arg {
                "on" => {
                    let duration = self
                        .store
                        .get(&format!("device.{}", self.serial))
                        .and_then(|d| d.value.get("fan_timer_duration").and_then(Value::as_f64))
                        .filter(|d| *d > 0.0)
                        .unwrap_or(900.0);
                    device(fields! {"fan_timer_timeout" => num(now as f64 + duration)})
                }
                "auto" => device(fields! {"fan_timer_timeout" => num(0.0)}),
                _ => Err("expected on or auto".into()),
            },

            "fan_timer_duration" => match arg.parse::<i64>() {
                Ok(m) if (15..=1440).contains(&m) => device(fields! {"fan_timer_duration" => num((m * 60) as f64)}),
                _ => Err("expected minutes, 15-1440".into()),
            },

            // The fan's daily schedule: it runs fan_duty_cycle seconds of
            // each hour from its start to its end (the same: all day).
            "fan_schedule" => device(fields! {"fan_mode" => if on_off(arg)? { "duty-cycle" } else { "auto" }}),

            "fan_schedule_minutes" => match arg.parse::<i64>() {
                Ok(m) if crate::bridge::FAN_SCHEDULE_MINUTES.contains(&m) => {
                    device(fields! {"fan_duty_cycle" => num((m * 60) as f64)})
                }
                _ => Err("expected 15, 30, 45 or 60".into()),
            },

            "fan_schedule_start" | "fan_schedule_end" => {
                let field = if name == "fan_schedule_start" { "fan_duty_start_time" } else { "fan_duty_end_time" };
                device(fields! {field => num((hour(arg)? * 3600) as f64)})
            }

            // Whole minutes: zero cancels. On/Off endpoints keep their default boost.
            "hot_water_boost" => {
                let longest = crate::bridge::HOT_WATER_BOOST_MAX_MINUTES;
                let minutes = match arg.to_uppercase().as_str() {
                    "ON" => crate::bridge::HOT_WATER_BOOST_MINUTES[0],
                    "OFF" => 0,
                    other => match other.parse() {
                        Ok(m) if (0..=longest).contains(&m) => m,
                        _ => return Err(format!("expected whole minutes 0-{longest}, or off")),
                    },
                };
                let end = if minutes == 0 { 0 } else { now + minutes * 60 };
                device(fields! {"hot_water_boost_time_to_end" => num(end as f64)})
            }

            "hot_water_schedule" => device(fields! {"hot_water_mode" => if on_off(arg)? { "schedule" } else { "off" }}),

            "preset" => {
                let mode = match arg {
                    "eco" => "manual-eco",
                    "none" | "" => "schedule",
                    _ => return Err("expected eco or none".into()),
                };
                let eco = fields! {
                    "mode" => mode,
                    "mode_update_timestamp" => num(now as f64),
                    "touched_by" => num(TOUCHED_BY_LOCAL as f64),
                };
                device(fields! {"eco" => Value::Object(eco)})
            }

            "learning_mode"
            | "auto_away_enable"
            | "sunlight_correction_enabled"
            | "preconditioning_enabled"
            | "away_temperature_low_enabled"
            | "away_temperature_high_enabled"
            | "target_humidity_enabled"
            | "filter_reminder_enabled" => device(fields! {name => on_off(arg)?}),

            "away_temperature_low" => device(fields! {name => num(temperature(arg, ECO_LOW)?)}),
            "away_temperature_high" => device(fields! {name => num(temperature(arg, ECO_HIGH)?)}),

            "target_humidity" => match float(arg) {
                Some(h) if (10.0..=60.0).contains(&h) => device(fields! {"target_humidity" => num(h)}),
                _ => Err("expected 10-60 %".into()),
            },

            "screen_timeout" => device(fields! {crate::screen::TIMEOUT => crate::screen::parse_seconds(arg)?}),
            "screen_always_on" => device(fields! {crate::screen::ALWAYS_ON => on_off(arg)?}),
            "screen_brightness" => device(crate::brightness::percent_change(crate::brightness::parse_percent(arg)?)),
            "screen_brightness_auto" => {
                let dev = self.store.get(&format!("device.{}", self.serial)).map(|o| o.value).unwrap_or_default();
                device(crate::brightness::auto_change(on_off(arg)?, &dev, self.brightness.shown()))
            }

            "temperature_scale" => match arg {
                "C" | "F" => device(fields! {"temperature_scale" => arg}),
                _ => Err("expected C or F".into()),
            },

            "outdoor_temperature" => {
                // Not a bucket: /weather answers with it.
                match float(arg) {
                    Some(v) if (-60.0..=60.0).contains(&v) => {
                        self.outdoor.set(Some(v), None);
                        logf!("mqtt: {name} {v}");
                        Ok(None)
                    }
                    _ => Err("expected a temperature, -60-60 °C".into()),
                }
            }

            // Not a bucket: the stand-in sensor's reading (sensor/).
            "external_temperature" => {
                let Some(sensor) = self.sensor.get() else {
                    return Err("this unit runs without -external-temperature".into());
                };
                let v = float(arg).ok_or("expected a temperature")?;
                let first = !sensor.has_reading();
                sensor.set(v)?;
                if first && !crate::sensor::paired_chosen(&self.store, &self.serial, &sensor.bucket) {
                    sensor.select_when_ready(&self.store, &self.serial);
                }
                logf!("external temperature: reading {v} °C");
                Ok(None)
            }

            "outdoor_humidity" => match float(arg) {
                Some(v) if (0.0..=100.0).contains(&v) => {
                    self.outdoor.set(None, Some(v));
                    logf!("mqtt: {name} {v}");
                    Ok(None)
                }
                _ => Err("expected 0-100 %".into()),
            },

            "weather" => match serde_json::from_str::<HAWeather>(arg) {
                Ok(w) => {
                    self.outdoor.set_weather(&w);
                    logf!(
                        "mqtt: weather {:?}, {} hourly, {} daily",
                        w.condition,
                        w.hourly.as_ref().map_or(0, Vec::len),
                        w.daily.as_ref().map_or(0, Vec::len)
                    );
                    Ok(None)
                }
                Err(e) => Err(format!("expected Home Assistant's weather as JSON: {e}")),
            },

            // Not a bucket: the unit's clock (clock.rs).
            "time" => self.time_answer(arg).map(|()| None),
            // Not a bucket either: nlclient takes it from the weather answer.
            "time_zone" => {
                if self.outdoor.set_zone(arg)? {
                    logf!("mqtt: time zone {arg}");
                }
                Ok(None)
            }

            // The filter was changed now: nlclient zeroes its runtime and
            // clears the reminder. OFF does nothing, for On/Off endpoints.
            "filter_reset" => {
                if arg.eq_ignore_ascii_case("OFF") {
                    return Ok(None);
                }
                device(fields! {"filter_changed_date" => num(now as f64)})
            }

            "heating_times" | "schedule" => {
                // The days the command names replace theirs (schedule.rs).
                let empty = Fields::new();
                let bucket = schedule.unwrap_or(&empty);
                let dev = self.store.get(&format!("device.{}", self.serial)).map(|o| o.value).unwrap_or_default();
                let shared = self.store.get(&format!("shared.{}", self.serial)).map(|o| o.value).unwrap_or_default();
                let mode = Self::schedule_mode(bucket, &shared)?;
                let fahrenheit = dev.get("temperature_scale").and_then(Value::as_str) == Some("F");
                let limits = if matches!(from, "zigbee" | "matter") { (9., 32.) } else { (4.5, 35.) };
                let translated;
                let arg = if name == "schedule" {
                    translated = crate::schedule::schedule_days(arg, mode, bucket)?;
                    &translated
                } else {
                    arg
                };
                Ok(Some(("schedule".into(), crate::schedule::write(bucket, arg, now, fahrenheit, limits)?)))
            }

            "hot_water_times" => {
                // The days the command names replace theirs (hotwater.rs).
                let dev = self.store.get(&format!("device.{}", self.serial)).map(|o| o.value).unwrap_or_default();
                if !crate::json::truthy(dev.get("has_hot_water_control")) {
                    return Err("no hot water control".into());
                }
                let hw = self.hot_water_schedule().ok_or("no hot water schedule yet")?;
                Ok(Some((hw.key, crate::hotwater::write(&hw.value, arg, now)?)))
            }

            "clear_schedule" => {
                // Every day empty: the unit keeps whatever setpoint it has
                // until told otherwise, so Home Assistant does the scheduling.
                // apply turns learning off too.
                let mut days = Fields::new();
                for d in 0..7 {
                    days.insert(d.to_string(), Value::Object(Fields::new()));
                }
                Ok(Some(("schedule".into(), fields! {"days" => Value::Object(days)})))
            }

            // Not a bucket change: nlclient lights the display, as for a
            // press, when it commits the device bucket sent again.
            "wake_screen" => {
                let key = format!("device.{}", self.serial);
                if self.store.get(&key).is_none() {
                    return Err(format!("no {key} bucket yet"));
                }
                // The request first: nlclient takes it as it commits.
                if let Err(e) = crate::settings_ui::request_wake() {
                    logf!("display wake: {e}");
                }
                self.store.resend(&key);
                Ok(None)
            }

            _ => Err("unknown command".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::bridge::tests::test_bridge;
    use crate::fields;
    use crate::json::{canonical, num};
    use serde_json::{json, Value};

    #[test]
    fn an_external_temperature_needs_the_sensor_and_a_sane_number() {
        let b = test_bridge();
        assert!(b.fields("external_temperature", "21.5").is_err(), "no -external-temperature");
        let dir = std::env::temp_dir().join(format!("nestlocal-commands-sensor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sensor = crate::sensor::Sensor::open(
            &dir.join("sensor.json"),
            "S",
            &dir.join("none"),
            std::time::Duration::from_secs(1800),
        )
        .unwrap();
        b.attach_sensor(sensor.clone());
        assert!(!crate::sensor::selected(&b.store, "S", &sensor.bucket));
        assert_eq!(b.fields("external_temperature", "21.5"), Ok(None));
        assert_eq!(sensor.reading(), Some(21.5));
        assert!(crate::sensor::selected(&b.store, &b.serial, &sensor.bucket), "the first reading selects it");
        let other = test_bridge();
        let sensor = crate::sensor::Sensor::open(
            &std::env::temp_dir().join(format!("nestlocal-first-{}.json", std::process::id())),
            "S",
            std::path::Path::new("/nonexistent"),
            std::time::Duration::from_secs(1800),
        )
        .unwrap();
        other.attach_sensor(sensor.clone());
        let fields = fields! {"active_rcs_sensors" => vec!["kryptonite.18B43000AAAA1111"]};
        other.store.merge(&format!("rcs_settings.{}", other.serial), &fields, 0, 0, true);
        assert_eq!(other.fields("external_temperature", "21.5"), Ok(None));
        assert!(!crate::sensor::selected(&other.store, &other.serial, &sensor.bucket), "a paired one chosen stays");
        for bad in ["unavailable", "", "NaN", "95"] {
            assert!(b.fields("external_temperature", bad).is_err(), "{bad}");
        }
        assert_eq!(sensor.reading(), Some(21.5));
    }

    #[test]
    fn a_filter_reset_dates_the_change_now_and_off_does_nothing() {
        let b = test_bridge();
        assert_eq!(b.fields("filter_reset", "off"), Ok(None));
        let (bucket, fields) = b.fields("filter_reset", "PRESS").unwrap().unwrap();
        let changed = fields["filter_changed_date"].as_f64().unwrap() as i64;
        assert_eq!(bucket, "device");
        assert!((chrono::Utc::now().timestamp() - changed).abs() <= 2);
    }

    #[test]
    fn wake_screen_sends_the_device_bucket_again_unchanged() {
        let b = test_bridge();
        assert!(b.apply("mqtt", "wake_screen", "PRESS").is_err(), "no device bucket yet");
        b.store.merge("device.S", &fields! {"temperature_scale" => "C"}, 0, 0, true);
        let before = b.store.get("device.S").unwrap();
        let (mut rx, _sub) = b.store.subscribe();
        assert_eq!(b.apply("mqtt", "wake_screen", "PRESS"), Ok(()));
        let sent = rx.try_recv().expect("the device bucket, sent again");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].value, before.value);
        assert!(sent[0].revision > before.revision);
    }

    #[test]
    fn display_wake_requires_a_changed_setting() {
        let b = test_bridge();
        b.store.merge(
            "shared.S",
            &fields! {"target_temperature" => 21.0,
            "target_temperature_type" => "heat", "target_change_pending" => false},
            0,
            0,
            true,
        );
        b.store.merge(
            "device.S",
            &fields! {"fan_timer_timeout" => 0.0,
            "eco" => json!({"mode":"schedule","mode_update_timestamp":0,"touched_by":4})},
            0,
            0,
            true,
        );
        for (name, arg, expected) in [
            ("target", "22", true),
            ("target", "21", false),
            ("mode", "cool", true),
            ("mode", "heat", false),
            ("fan_mode", "on", true),
            ("fan_mode", "auto", false),
            ("preset", "eco", true),
            ("preset", "none", false),
        ] {
            let (bucket, fields) = b.fields(name, arg).unwrap().unwrap();
            assert_eq!(b.display_change(&format!("{bucket}.S"), &fields), expected, "{name} {arg}");
        }
        assert!(!b.display_change("shared.missing", &fields! {"target_temperature" => 22.0}));
        b.store.merge("structure.S", &fields! {"away" => false}, 0, 0, true);
        assert!(!b.display_change("structure.S", &fields! {"away" => true}));
        assert!(b.fields("target", "99").is_err());
        assert!(b.fields("outdoor_temperature", "10").unwrap().is_none());
    }

    #[test]
    fn remote_protocols_share_the_display_change_path() {
        for source in ["mqtt", "matter", "zigbee"] {
            for (name, arg) in [("target", "22"), ("mode", "cool"), ("preset", "eco"), ("fan_mode", "on")] {
                let b = test_bridge();
                b.store.merge(
                    "device.S",
                    &fields! {"fan_timer_timeout" => 0.0,
                    "eco" => json!({"mode":"schedule"})},
                    0,
                    0,
                    true,
                );
                b.store.merge(
                    "shared.S",
                    &fields! {"target_temperature" => 21.0,
                    "target_temperature_type" => "heat"},
                    0,
                    0,
                    true,
                );
                let (bucket, fields) = b.fields(name, arg).unwrap().unwrap();
                let key = format!("{bucket}.S");
                assert!(b.display_change(&key, &fields), "{source} {name}");
                if source == "mqtt" {
                    b.command(&format!("{}/set/{name}", b.base), arg.as_bytes());
                } else {
                    b.apply(source, name, arg).unwrap();
                }
                assert!(!b.display_change(&key, &fields), "{source} {name} was not applied");
            }
        }
    }

    #[test]
    fn commands() {
        let b = test_bridge();
        b.store.merge("device.S", &fields! {"fan_timer_duration" => 1800.0}, 0, 0, true);
        for (name, arg, bucket, want) in [
            ("mode", "heat_cool", "shared", json!({"target_temperature_type": "range", "target_change_pending": true})),
            ("target", "20.3", "shared", json!({"target_temperature": 20.5, "target_change_pending": true})),
            ("fan_mode", "auto", "device", json!({"fan_timer_timeout": 0.0})),
            ("learning_mode", "OFF", "device", json!({"learning_mode": false})),
            ("raw/device", r#"{"x":1}"#, "device", json!({"x": 1.0})),
        ] {
            let (got_bucket, got) = b.fields(name, arg).unwrap().unwrap();
            assert_eq!((got_bucket.as_str(), Value::Object(got)), (bucket, canonical(want)), "{name} {arg}");
        }
        let (_, f) = b.fields("fan_mode", "on").unwrap().unwrap();
        let ahead = f["fan_timer_timeout"].as_f64().unwrap() - chrono::Utc::now().timestamp() as f64;
        assert!((1795.0..=1801.0).contains(&ahead), "fan on: timeout {ahead} s from now, want the unit's 1800");
        for (name, arg) in [
            ("mode", "auto"),
            ("target", "99"),
            ("preset", "away"),
            ("nope", ""),
            ("outdoor_temperature", "unavailable"),
            ("outdoor_humidity", "101"),
            ("time_zone", "No/Such_Zone"),
            ("time_zone", "../UTC"),
        ] {
            assert!(b.fields(name, arg).is_err(), "{name} {arg} accepted");
        }
    }

    #[test]
    fn weather() {
        let b = test_bridge();
        let get = |query: &str| b.outdoor.answer().map(|a| json!({query: Value::Object(a)})).unwrap_or(json!({}));
        b.fields("outdoor_temperature", "-3.4").unwrap();
        // The unit rejects an answer without humidity.
        assert_eq!(get("94304,US"), json!({}));
        b.fields("outdoor_humidity", "81.6").unwrap();
        assert_eq!(get("94304,US"), canonical(json!({"94304,US": {"current": {"temp_c": -3.4, "humidity": 82.0}}})));
    }

    #[test]
    fn weather_forecast() {
        let b = test_bridge();
        b.fields("outdoor_temperature", "22.5").unwrap();
        b.fields("outdoor_humidity", "64").unwrap();
        // As Home Assistant sends it: by day, next_rising is tomorrow's.
        b.fields("weather", r#"{"condition": "partlycloudy",
            "next_rising": "2026-09-30T11:02:03.718805+00:00", "next_setting": "2026-09-29T22:55:04+00:00",
            "hourly": [{"datetime": "2026-09-29T12:00:00-04:00", "condition": "cloudy", "temperature": 22.8, "humidity": 66},
                {"datetime": "2026-09-29T13:00:00-04:00", "temperature": 23.1, "humidity": null}],
            "daily": [{"datetime": "2026-09-29T11:00:00-04:00", "is_daytime": true, "temperature": 23.3, "humidity": null},
                {"datetime": "2026-09-29T18:00:00-04:00", "is_daytime": false, "temperature": 15.0},
                {"datetime": "2026-09-30T06:00:00-04:00", "is_daytime": true, "temperature": 27.8}]}"#).unwrap();
        let want = canonical(json!({
            "current": {"temp_c": 22.5, "humidity": 64.0, "icon": "partlycloudy",
                        "sunrise": 1790679723_i64, "sunset": 1790722504_i64}, // today's 11:02 and 22:55 UTC
            "forecast": {
                "hourly": [
                    {"hour": 1, "time": 1790697600_i64, "temp_c": 22.8, "humidity": 66.0},
                    {"hour": 2, "time": 1790701200_i64, "temp_c": 23.1},
                ],
                "daily": [
                    {"date": 1790694000_i64, "temp_high_c": 23.3, "temp_low_c": 15.0},
                    {"date": 1790762400_i64, "temp_high_c": 27.8},
                ],
            },
        }));
        assert_eq!(Value::Object(b.outdoor.answer().unwrap()), want);

        // By night both are ahead, and pass as they are; an unknown condition
        // leaves the icon out (the unit's default image).
        b.fields(
            "weather",
            r#"{"condition": "windy", "next_rising": "2026-09-30T11:02:03+00:00",
            "next_setting": "2026-09-30T22:53:00+00:00"}"#,
        )
        .unwrap();
        let current = b.outdoor.answer().unwrap()["current"].clone();
        assert!(
            current.get("icon").is_none()
                && current["sunrise"] == num(1790766123.0)
                && current["sunset"] == num(1790808780.0),
            "night: {current}"
        );
        assert!(b.fields("weather", "unavailable").is_err(), "non-JSON weather accepted");
    }

    #[test]
    fn arguments_at_and_past_their_limits() {
        let b = test_bridge();
        let field = |name: &str, arg: &str, key: &str| b.fields(name, arg).map(|c| c.unwrap().1[key].clone());
        for (arg, want) in [
            ("4.5", Some(4.5)),
            ("35", Some(35.0)),
            ("4.74", Some(4.5)),
            ("4.75", Some(5.0)),
            ("21.25", Some(21.5)),
            ("4.4", None),
            ("35.1", None),
            ("NaN", None),
            ("inf", None),
            ("20,5", None),
            ("", None),
            ("20°C", None),
        ] {
            assert_eq!(field("target_high", arg, "target_temperature_high").ok(), want.map(num), "target_high {arg:?}");
        }
        for (name, arg, want) in [
            ("away_temperature_low", "4.5", Some(4.5)),
            ("away_temperature_low", "21", Some(21.0)),
            ("away_temperature_low", "4.4", None),
            ("away_temperature_low", "21.1", None),
            ("away_temperature_high", "24", Some(24.0)),
            ("away_temperature_high", "32", Some(32.0)),
            ("away_temperature_high", "23.9", None),
            ("away_temperature_high", "32.1", None),
        ] {
            assert_eq!(field(name, arg, name).ok(), want.map(num), "{name} {arg:?}");
        }
        for (arg, want) in
            [("15", Some(900)), ("1440", Some(86_400)), ("14", None), ("1441", None), ("30.0", None), ("-15", None)]
        {
            assert_eq!(
                field("fan_timer_duration", arg, "fan_timer_duration").ok(),
                want.map(Value::from),
                "fan_timer_duration {arg:?}"
            );
        }
        for (arg, want) in [("ON", Some("duty-cycle")), ("OFF", Some("auto")), ("duty-cycle", None)] {
            assert_eq!(field("fan_schedule", arg, "fan_mode").ok(), want.map(Value::from), "fan_schedule {arg:?}");
        }
        for (arg, want) in [("15", Some(900)), ("60", Some(3600)), ("20", None), ("0", None), ("90", None)] {
            assert_eq!(
                field("fan_schedule_minutes", arg, "fan_duty_cycle").ok(),
                want.map(Value::from),
                "fan_schedule_minutes {arg:?}"
            );
        }
        for (arg, want) in [
            ("00:00", Some(0)),
            ("23:00", Some(82_800)),
            ("7", Some(25_200)),
            ("07", Some(25_200)),
            ("24:00", None),
            ("07:30", None),
            ("-1", None),
            ("", None),
        ] {
            assert_eq!(
                field("fan_schedule_start", arg, "fan_duty_start_time").ok(),
                want.map(Value::from),
                "start {arg:?}"
            );
            assert_eq!(field("fan_schedule_end", arg, "fan_duty_end_time").ok(), want.map(Value::from), "end {arg:?}");
        }
        for (arg, want) in [("10", true), ("60", true), ("42.5", true), ("9.9", false), ("60.5", false)] {
            assert_eq!(b.fields("target_humidity", arg).is_ok(), want, "target_humidity {arg:?}");
        }
        for (arg, want) in [("-60", true), ("60", true), ("-60.1", false), ("60.1", false)] {
            assert_eq!(b.fields("outdoor_temperature", arg).is_ok(), want, "outdoor_temperature {arg:?}");
        }
        for (arg, want) in [
            ("on", Some(true)),
            ("True", Some(true)),
            ("1", Some(true)),
            ("off", Some(false)),
            ("0", Some(false)),
            ("yes", None),
            ("", None),
        ] {
            assert_eq!(
                field("sunlight_correction_enabled", arg, "sunlight_correction_enabled").ok(),
                want.map(Value::from),
                "{arg:?}"
            );
        }
        assert!(b.fields("temperature_scale", "f").is_err() && b.fields("mode", "HEAT").is_err());
        // raw/: a JSON object; null is no change; anything else is refused.
        assert_eq!(b.fields("raw/schedule", "null"), Ok(None));
        assert_eq!(b.fields("raw/schedule", "{}"), Ok(Some(("schedule".into(), fields! {}))));
        assert!(b.fields("raw/schedule", "[1]").is_err() && b.fields("raw/schedule", "{").is_err());
    }

    #[test]
    fn presets_mark_eco_as_the_owners() {
        let b = test_bridge();
        let eco = |arg: &str| b.fields("preset", arg).unwrap().unwrap().1["eco"].clone();
        let now = chrono::Utc::now().timestamp() as f64;
        let e = eco("eco");
        assert_eq!((&e["mode"], &e["touched_by"]), (&json!("manual-eco"), &json!(4)));
        assert!((e["mode_update_timestamp"].as_f64().unwrap() - now).abs() <= 2.0, "{e}");
        assert_eq!(eco("none")["mode"], json!("schedule"));
        assert_eq!(eco("")["mode"], json!("schedule"));
    }

    #[test]
    fn hot_water_commands_set_the_fields_nlclient_takes() {
        let b = test_bridge();
        let field = |name: &str, arg: &str, key: &str| b.fields(name, arg).unwrap().unwrap().1[key].clone();
        let left = |arg: &str| {
            field("hot_water_boost", arg, "hot_water_boost_time_to_end").as_f64().unwrap()
                - chrono::Utc::now().timestamp() as f64
        };
        for (arg, seconds) in [("ON", 1800.0), ("1", 60.0), ("37", 2220.0), ("75", 4500.0), ("120", 7200.0)] {
            assert!((seconds - 2.0..=seconds).contains(&left(arg)), "{arg}");
        }
        for arg in ["0", "off", "OFF"] {
            assert_eq!(field("hot_water_boost", arg, "hot_water_boost_time_to_end").as_f64(), Some(0.0), "{arg}");
        }
        for arg in ["121", "-1", "30.5", "NaN", "", "maybe"] {
            assert!(b.fields("hot_water_boost", arg).is_err(), "{arg}");
        }
        assert_eq!(field("hot_water_schedule", "ON", "hot_water_mode"), json!("schedule"));
        assert_eq!(field("hot_water_schedule", "OFF", "hot_water_mode"), json!("off"));
        assert!(b.fields("hot_water_schedule", "maybe").is_err());
    }

    #[test]
    fn apply_merges_into_buckets_the_unit_has() {
        let b = test_bridge();
        assert_eq!(b.apply("mqtt", "target", "21"), Err("no shared.S bucket yet".into()));
        assert!(b.store.get("shared.S").is_none(), "a command made the bucket");
        b.store.merge("shared.S", &fields! {"target_temperature" => 19}, 3, 100, true);
        let (mut changes, _sub) = b.store.subscribe();
        b.apply("mqtt", "target", "21").unwrap();
        // A server-side change: it goes to the held subscribe.
        let pushed = changes.try_recv().unwrap();
        assert_eq!((pushed[0].revision, &pushed[0].value["target_temperature"]), (4, &json!(21)));
        // A bad argument changes nothing.
        assert!(b.apply("mqtt", "target", "99").is_err());
        assert_eq!(b.store.get("shared.S").unwrap().revision, 4);
        // Commands that are no bucket change: nothing merged, no error.
        assert_eq!(b.apply("mqtt", "outdoor_humidity", "50"), Ok(()));
        assert!(changes.try_recv().is_err());
    }

    #[test]
    fn clear_schedule_empties_every_day_and_stops_learning() {
        let b = test_bridge();
        b.store.merge("device.S", &fields! {"learning_mode" => true}, 0, 0, true);
        b.store.merge("schedule.S", &fields! {"days" => json!({"0": {"0": {"temp": 20}}}), "ver" => 2}, 0, 0, true);
        b.apply("mqtt", "clear_schedule", "PRESS").unwrap();
        let schedule = b.store.get("schedule.S").unwrap().value;
        assert_eq!(schedule["days"], json!({"0": {}, "1": {}, "2": {}, "3": {}, "4": {}, "5": {}, "6": {}}));
        assert_eq!(schedule["ver"], json!(2));
        assert_eq!(b.store.get("device.S").unwrap().value["learning_mode"], json!(false));
    }

    #[test]
    fn heating_times_write_the_named_days_and_keep_the_learning_preference() {
        let b = test_bridge();
        b.store.merge("device.S", &fields! {"learning_mode" => true}, 0, 0, true);
        assert!(b.apply("mqtt", "heating_times", r#"{"mon": []}"#).is_err(), "no schedule bucket yet");
        b.store.merge("schedule.S", &fields! {"days" => json!({}), "schedule_mode" => "HEAT", "ver" => 2}, 0, 0, true);
        b.apply("mqtt", "heating_times", r#"{"tue": [{"time": "06:30", "temp": 20}]}"#).unwrap();
        let schedule = b.store.get("schedule.S").unwrap().value;
        assert_eq!(crate::schedule::day(&schedule, 1), [(390, 20.0)]);
        assert_eq!(b.store.get("device.S").unwrap().value["learning_mode"], json!(true));
        assert!(b.apply("mqtt", "heating_times", r#"{"tue": [{"time": "06:30", "temp": 99}]}"#).is_err());
    }

    #[test]
    fn a_stale_mode_or_unsupported_equipment_cannot_overwrite_a_week() {
        let b = test_bridge();
        b.store.merge("schedule.S", &fields! {"schedule_mode"=>"HEAT","days"=>json!({})}, 0, 0, true);
        let edit = r#"{"mon":[{"time":"07:00","temp":21}]}"#;
        b.store.merge(
            "shared.S",
            &fields! {"can_heat"=>true,"can_cool"=>true,"target_temperature_type"=>"cool"},
            0,
            0,
            true,
        );
        assert!(b.apply("mqtt", "heating_times", edit).is_err());
        b.store.merge("shared.S", &fields! {"can_heat"=>false,"target_temperature_type"=>"off"}, 0, 0, true);
        assert!(b.apply("zigbee", "heating_times", edit).is_err());
        b.store.merge("shared.S", &fields! {"can_heat"=>true}, 0, 0, true);
        assert!(b.apply("mqtt", "schedule", r#"{"mode":"cool","days":{"monday":[]}}"#).is_err());
        b.apply("mqtt", "schedule", r#"{"mode":"heat","days":{"monday":[{"time":"07:00","temperature":21}]}}"#)
            .unwrap();
        assert_eq!(crate::schedule::day(&b.store.get("schedule.S").unwrap().value, 0), [(420, 21.)]);
    }

    #[test]
    fn hot_water_times_write_the_hot_water_schedules_bucket() {
        let b = test_bridge();
        let cmd = r#"{"tue": [{"on": "06:30", "off": "07:30"}]}"#;
        assert_eq!(b.apply("mqtt", "hot_water_times", cmd), Err("no hot water control".into()));
        b.store.merge("device.S", &fields! {"has_hot_water_control" => true}, 0, 0, true);
        assert_eq!(b.apply("mqtt", "hot_water_times", cmd), Err("no hot water schedule yet".into()));
        b.store.merge("custom_schedule.hotwater", &fields! {"mode" => "hotwater", "name" => "HOTWATER"}, 0, 0, true);
        b.apply("mqtt", "hot_water_times", cmd).unwrap();
        let hw = b.store.get("custom_schedule.hotwater").unwrap().value;
        assert_eq!(crate::hotwater::day(&hw, 1), [(390, 450)]);
        assert_eq!(hw["name"], json!("HOTWATER"), "the rest of the bucket stays");
    }

    #[test]
    fn clear_schedule_changes_nothing_when_it_fails() {
        let b = test_bridge();
        // No bucket at all: none is made.
        assert!(b.apply("mqtt", "clear_schedule", "PRESS").is_err());
        assert!(b.store.get("device.S").is_none() && b.store.get("schedule.S").is_none());
        // No schedule: learning stays on.
        b.store.merge("device.S", &fields! {"learning_mode" => true}, 0, 0, true);
        assert!(b.apply("mqtt", "clear_schedule", "PRESS").is_err());
        let d = b.store.get("device.S").unwrap();
        assert_eq!((d.revision, &d.value["learning_mode"]), (1, &json!(true)));
        // No device bucket: the schedule stays.
        let b = test_bridge();
        b.store.merge("schedule.S", &fields! {"days" => json!({"0": {"0": {"temp": 20}}})}, 0, 0, true);
        assert!(b.apply("mqtt", "clear_schedule", "PRESS").is_err());
        assert_eq!(b.store.get("schedule.S").unwrap().revision, 1);
        assert!(b.store.get("device.S").is_none());
    }
}
