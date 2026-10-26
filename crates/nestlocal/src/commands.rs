//! <base>/set/<name>: Home Assistant's commands, as bucket fields that the
//! store merges and the held subscribe pushes to nlclient.

use serde_json::Value;

use crate::bridge::Bridge;
use crate::fields;
use crate::json::{num, unmarshal_object, Fields};
use crate::weather::HAWeather;
use crate::logf;

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

fn on_off(arg: &str) -> Result<bool, String> {
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
fn temperature(arg: &str, (low, high): (f64, f64)) -> Result<f64, String> {
    match float(arg) {
        Some(t) if (low..=high).contains(&t) => Ok((t * 2.0).round() / 2.0),
        _ => Err(format!("expected a temperature, {low}-{high} °C")),
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
        let (bucket, fields) = match self.fields(name, arg) {
            Err(e) => {
                logf!("{from}: {name} {arg:?}: {e}");
                return Err(e);
            }
            Ok(None) => return Ok(()),
            Ok(Some(change)) => change,
        };
        let mut changes = vec![(format!("{bucket}.{}", self.serial), fields)];
        if name == "clear_schedule" {
            // Learning off too, or the unit would build a new schedule.
            changes.push((format!("device.{}", self.serial), fields!{"learning_mode" => false}));
        }
        if let Some((key, _)) = changes.iter().find(|(key, _)| self.store.get(key).is_none()) {
            logf!("{from}: {name}: no {key} bucket yet");
            return Err(format!("no {key} bucket yet"));
        }
        if changes.iter().any(|(key, fields)| self.display_change(key, fields)) {
            // Publish before notifying held subscribes: nlclient consumes the
            // request when its bucket parser has applied the new settings.
            let expires = (chrono::Utc::now().timestamp() as u32 + 10).to_le_bytes();
            if let Err(e) = crate::settings_ui::publish("display.request", &expires) {
                logf!("display wake: {e}");
            }
        }
        for (key, fields) in &changes {
            logf!("{from}: {name} {arg:?} -> {key} {}", Value::Object(fields.clone()));
            self.store.merge(key, fields, 0, 0, false);
        }
        Ok(())
    }

    fn display_change(&self, key: &str, fields: &Fields) -> bool {
        if !["device.", "shared.", "schedule."].iter().any(|prefix| key.starts_with(prefix)) {
            return false;
        }
        let Some(current) = self.store.get(key) else { return false };
        fields.iter().any(|(name, value)| {
            if name == "eco" {
                current.value.get(name).and_then(|v| v.get("mode")) != value.get("mode")
            } else {
                name != "target_change_pending" && current.value.get(name) != Some(&crate::json::canonical(value.clone()))
            }
        })
    }

    pub fn fields(&self, name: &str, arg: &str) -> Result<Change, String> {
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
                shared(fields!{"target_temperature_type" => t, "target_change_pending" => true})
            }

            "target" | "target_low" | "target_high" => {
                let t = temperature(arg, SETPOINT)?;
                let field = match name {
                    "target" => "target_temperature",
                    "target_low" => "target_temperature_low",
                    _ => "target_temperature_high",
                };
                shared(fields!{field => num(t), "target_change_pending" => true})
            }

            "fan_mode" => match arg {
                "on" => {
                    let duration = self.store.get(&format!("device.{}", self.serial))
                        .and_then(|d| d.value.get("fan_timer_duration").and_then(Value::as_f64))
                        .filter(|d| *d > 0.0)
                        .unwrap_or(900.0);
                    device(fields!{"fan_timer_timeout" => num(now as f64 + duration)})
                }
                "auto" => device(fields!{"fan_timer_timeout" => num(0.0)}),
                _ => Err("expected on or auto".into()),
            },

            "fan_timer_duration" => match arg.parse::<i64>() {
                Ok(m) if (15..=1440).contains(&m) => device(fields!{"fan_timer_duration" => num((m * 60) as f64)}),
                _ => Err("expected minutes, 15-1440".into()),
            },

            "preset" => {
                let mode = match arg {
                    "eco" => "manual-eco",
                    "none" | "" => "schedule",
                    _ => return Err("expected eco or none".into()),
                };
                let eco = fields!{
                    "mode" => mode,
                    "mode_update_timestamp" => num(now as f64),
                    "touched_by" => num(TOUCHED_BY_LOCAL as f64),
                };
                device(fields!{"eco" => Value::Object(eco)})
            }

            "learning_mode" | "auto_away_enable" | "sunlight_correction_enabled" | "preconditioning_enabled"
            | "away_temperature_low_enabled" | "away_temperature_high_enabled" | "target_humidity_enabled" => {
                device(fields!{name => on_off(arg)?})
            }

            "away_temperature_low" => device(fields!{name => num(temperature(arg, ECO_LOW)?)}),
            "away_temperature_high" => device(fields!{name => num(temperature(arg, ECO_HIGH)?)}),

            "target_humidity" => match float(arg) {
                Some(h) if (10.0..=60.0).contains(&h) => device(fields!{"target_humidity" => num(h)}),
                _ => Err("expected 10-60 %".into()),
            },

            "temperature_scale" => match arg {
                "C" | "F" => device(fields!{"temperature_scale" => arg}),
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
                    logf!("mqtt: weather {:?}, {} hourly, {} daily", w.condition,
                        w.hourly.as_ref().map_or(0, Vec::len), w.daily.as_ref().map_or(0, Vec::len));
                    Ok(None)
                }
                Err(e) => Err(format!("expected Home Assistant's weather as JSON: {e}")),
            },

            // Not a bucket: the unit's clock (clock.rs).
            "time" => self.time_answer(arg).map(|()| None),

            "clear_schedule" => {
                // Every day empty: the unit keeps whatever setpoint it has
                // until told otherwise, so Home Assistant does the scheduling.
                // apply turns learning off too.
                let mut days = Fields::new();
                for d in 0..7 {
                    days.insert(d.to_string(), Value::Object(Fields::new()));
                }
                Ok(Some(("schedule".into(), fields!{"days" => Value::Object(days)})))
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
    fn display_wake_requires_a_changed_setting() {
        let b = test_bridge();
        b.store.merge("shared.S", &fields!{"target_temperature" => 21.0,
            "target_temperature_type" => "heat", "target_change_pending" => false}, 0, 0, true);
        b.store.merge("device.S", &fields!{"fan_timer_timeout" => 0.0,
            "eco" => json!({"mode":"schedule","mode_update_timestamp":0,"touched_by":4})}, 0, 0, true);
        for (name, arg, expected) in [
            ("target", "22", true), ("target", "21", false),
            ("mode", "cool", true), ("mode", "heat", false),
            ("fan_mode", "on", true), ("fan_mode", "auto", false),
            ("preset", "eco", true), ("preset", "none", false),
        ] {
            let (bucket, fields) = b.fields(name, arg).unwrap().unwrap();
            assert_eq!(b.display_change(&format!("{bucket}.S"), &fields), expected, "{name} {arg}");
        }
        assert!(!b.display_change("shared.missing", &fields!{"target_temperature" => 22.0}));
        b.store.merge("structure.S", &fields!{"away" => false}, 0, 0, true);
        assert!(!b.display_change("structure.S", &fields!{"away" => true}));
        assert!(b.fields("target", "99").is_err());
        assert!(b.fields("outdoor_temperature", "10").unwrap().is_none());
    }

    #[test]
    fn remote_protocols_share_the_display_change_path() {
        for source in ["mqtt", "matter", "zigbee"] {
            for (name, arg) in [("target", "22"), ("mode", "cool"),
                                ("preset", "eco"), ("fan_mode", "on")] {
                let b = test_bridge();
                b.store.merge("device.S", &fields!{"fan_timer_timeout" => 0.0,
                    "eco" => json!({"mode":"schedule"})}, 0, 0, true);
                b.store.merge("shared.S", &fields!{"target_temperature" => 21.0,
                    "target_temperature_type" => "heat"}, 0, 0, true);
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
        b.store.merge("device.S", &fields!{"fan_timer_duration" => 1800.0}, 0, 0, true);
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
        for (name, arg) in [("mode", "auto"), ("target", "99"), ("preset", "away"), ("nope", ""),
                            ("outdoor_temperature", "unavailable"), ("outdoor_humidity", "101")] {
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
        b.fields("weather", r#"{"condition": "windy", "next_rising": "2026-09-30T11:02:03+00:00",
            "next_setting": "2026-09-30T22:53:00+00:00"}"#).unwrap();
        let current = b.outdoor.answer().unwrap()["current"].clone();
        assert!(current.get("icon").is_none() && current["sunrise"] == num(1790766123.0)
            && current["sunset"] == num(1790808780.0), "night: {current}");
        assert!(b.fields("weather", "unavailable").is_err(), "non-JSON weather accepted");
    }

    #[test]
    fn arguments_at_and_past_their_limits() {
        let b = test_bridge();
        let field = |name: &str, arg: &str, key: &str| b.fields(name, arg).map(|c| c.unwrap().1[key].clone());
        for (arg, want) in [("4.5", Some(4.5)), ("35", Some(35.0)), ("4.74", Some(4.5)), ("4.75", Some(5.0)), ("21.25", Some(21.5)),
                            ("4.4", None), ("35.1", None), ("NaN", None), ("inf", None), ("20,5", None), ("", None), ("20°C", None)] {
            assert_eq!(field("target_high", arg, "target_temperature_high").ok(), want.map(num), "target_high {arg:?}");
        }
        for (name, arg, want) in [("away_temperature_low", "4.5", Some(4.5)), ("away_temperature_low", "21", Some(21.0)),
                                  ("away_temperature_low", "4.4", None), ("away_temperature_low", "21.1", None),
                                  ("away_temperature_high", "24", Some(24.0)), ("away_temperature_high", "32", Some(32.0)),
                                  ("away_temperature_high", "23.9", None), ("away_temperature_high", "32.1", None)] {
            assert_eq!(field(name, arg, name).ok(), want.map(num), "{name} {arg:?}");
        }
        for (arg, want) in [("15", Some(900)), ("1440", Some(86_400)), ("14", None), ("1441", None), ("30.0", None), ("-15", None)] {
            assert_eq!(field("fan_timer_duration", arg, "fan_timer_duration").ok(), want.map(Value::from), "fan_timer_duration {arg:?}");
        }
        for (arg, want) in [("10", true), ("60", true), ("42.5", true), ("9.9", false), ("60.5", false)] {
            assert_eq!(b.fields("target_humidity", arg).is_ok(), want, "target_humidity {arg:?}");
        }
        for (arg, want) in [("-60", true), ("60", true), ("-60.1", false), ("60.1", false)] {
            assert_eq!(b.fields("outdoor_temperature", arg).is_ok(), want, "outdoor_temperature {arg:?}");
        }
        for (arg, want) in [("on", Some(true)), ("True", Some(true)), ("1", Some(true)), ("off", Some(false)), ("0", Some(false)),
                            ("yes", None), ("", None)] {
            assert_eq!(field("sunlight_correction_enabled", arg, "sunlight_correction_enabled").ok(), want.map(Value::from), "{arg:?}");
        }
        assert!(b.fields("temperature_scale", "f").is_err() && b.fields("mode", "HEAT").is_err());
        // raw/: a JSON object; null is no change; anything else is refused.
        assert_eq!(b.fields("raw/schedule", "null"), Ok(None));
        assert_eq!(b.fields("raw/schedule", "{}"), Ok(Some(("schedule".into(), fields!{}))));
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
    fn apply_merges_into_buckets_the_unit_has() {
        let b = test_bridge();
        assert_eq!(b.apply("mqtt", "target", "21"), Err("no shared.S bucket yet".into()));
        assert!(b.store.get("shared.S").is_none(), "a command made the bucket");
        b.store.merge("shared.S", &fields!{"target_temperature" => 19}, 3, 100, true);
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
        b.store.merge("device.S", &fields!{"learning_mode" => true}, 0, 0, true);
        b.store.merge("schedule.S", &fields!{"days" => json!({"0": {"0": {"temp": 20}}}), "ver" => 2}, 0, 0, true);
        b.apply("mqtt", "clear_schedule", "PRESS").unwrap();
        let schedule = b.store.get("schedule.S").unwrap().value;
        assert_eq!(schedule["days"], json!({"0": {}, "1": {}, "2": {}, "3": {}, "4": {}, "5": {}, "6": {}}));
        assert_eq!(schedule["ver"], json!(2));
        assert_eq!(b.store.get("device.S").unwrap().value["learning_mode"], json!(false));
    }

    #[test]
    fn clear_schedule_changes_nothing_when_it_fails() {
        let b = test_bridge();
        // No bucket at all: none is made.
        assert!(b.apply("mqtt", "clear_schedule", "PRESS").is_err());
        assert!(b.store.get("device.S").is_none() && b.store.get("schedule.S").is_none());
        // No schedule: learning stays on.
        b.store.merge("device.S", &fields!{"learning_mode" => true}, 0, 0, true);
        assert!(b.apply("mqtt", "clear_schedule", "PRESS").is_err());
        let d = b.store.get("device.S").unwrap();
        assert_eq!((d.revision, &d.value["learning_mode"]), (1, &json!(true)));
        // No device bucket: the schedule stays.
        let b = test_bridge();
        b.store.merge("schedule.S", &fields!{"days" => json!({"0": {"0": {"temp": 20}}})}, 0, 0, true);
        assert!(b.apply("mqtt", "clear_schedule", "PRESS").is_err());
        assert_eq!(b.store.get("schedule.S").unwrap().revision, 1);
        assert!(b.store.get("device.S").is_none());
    }
}
