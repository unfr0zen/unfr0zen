//! The unit's weekly schedule (its schedule bucket), as MQTT, Zigbee and
//! Matter read and write it: by day, the times the target changes and the
//! temperature it changes to, or for a range schedule (heat-cool) the low
//! and the high it keeps between.
//!
//! The bucket keeps each day under "days" as "0" (Monday) to "6" (Sunday),
//! each an object of entries keyed "0", "1", ...: setpoints, and a
//! continuation at midnight carrying the previous day's last setpoint on.
//! A write replaces "days" whole, so it rebuilds the week: the days it
//! names get their new setpoints, the others keep theirs, and every
//! continuation follows the day before it.

use chrono::TimeZone;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::fields;
use crate::json::{num, number, sprint, Fields};

/// Matter schedule handles and the raw weeks kept alongside the device bucket.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub named: BTreeMap<String, SavedWeek>,
    // Each native mode resumes its own active schedule.
    pub active: BTreeMap<String, String>,
    pub native: BTreeMap<String, Fields>,
    #[serde(default)]
    pub control: BTreeMap<String, crate::schedule_control::Mode>,
    /// Learning as it was before suspension; Some(null) when the device bucket had none.
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub learning: Option<Value>,
    /// Minutes a hold lasts, the same in every mode; 120 when never set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_minutes: Option<u16>,
}

fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWeek {
    pub name: Option<String>,
    pub bucket: Fields,
}

pub const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// The most setpoints a day takes: the transitions one Zigbee or Matter
/// Set Weekly Schedule carries.
pub const DAILY: usize = 10;

/// Marks the setpoints a command writes as the owner's, as nlclient marks
/// the Nest app's.
const TOUCHED_BY_LOCAL: i64 = 4;

/// Setpoint temperatures, °C.
const SETPOINT: (f64, f64) = (4.5, 35.0);

/// The least a range's high is above its low, °C, as the unit keeps them.
pub const RANGE_GAP: f64 = 1.5;

/// A setpoint: minutes after midnight, and °C.
pub type Setpoint = (u16, f64);

/// A range schedule's setpoint: minutes after midnight, and the low and the
/// high, °C.
pub type Range = (u16, f64, f64);

/// What the schedule's temperatures set: heating, cooling, or both, a low
/// and a high (the unit's heat-cool mode).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Heat,
    Cool,
    Range,
}

/// The schedule's kind; a unit that has none yet heats.
pub fn kind(bucket: &Fields) -> Option<Kind> {
    match sprint(bucket.get("schedule_mode")).as_str() {
        "HEAT" | "<nil>" => Some(Kind::Heat),
        "COOL" => Some(Kind::Cool),
        "RANGE" => Some(Kind::Range),
        _ => None,
    }
}

/// A setpoint a command writes: minutes after midnight, and its temperatures.
type Change = (u16, Temps);

/// A setpoint's temperatures: one, or a range's low and high.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Temps {
    One(f64),
    Range(f64, f64),
}

impl Temps {
    /// An entry's, as the bucket keeps them.
    fn of(entry: &Fields) -> Option<Self> {
        match (number(entry.get("temp-min")), number(entry.get("temp-max"))) {
            (Some(low), Some(high)) => Some(Self::Range(low, high)),
            _ => number(entry.get("temp")).map(Self::One),
        }
    }

    fn map(self, f: impl Fn(f64) -> f64) -> Self {
        match self {
            Self::One(c) => Self::One(f(c)),
            Self::Range(low, high) => Self::Range(f(low), f(high)),
        }
    }

    fn fields(self) -> Fields {
        match self {
            Self::One(c) => fields! {"temp" => num(c)},
            Self::Range(low, high) => fields! {"temp-min" => num(low), "temp-max" => num(high)},
        }
    }
}

fn entries(bucket: &Fields, day: usize) -> Vec<Fields> {
    let entries = bucket.get("days").and_then(|d| d.get(day.to_string())).and_then(Value::as_object);
    entries.map_or(Vec::new(), |d| d.values().filter_map(|e| e.as_object().cloned()).collect())
}

fn is_setpoint(e: &Fields) -> bool {
    e.get("entry_type").is_none_or(|kind| kind == "setpoint")
}

fn midnight_carry(e: &Fields) -> bool {
    e.get("entry_type").is_some_and(|kind| kind == "continuation") && number(e.get("time")) == Some(0.0)
}

/// A day with no programmed change can still hold a stock midnight target.
fn shown_entries(bucket: &Fields, day: usize) -> Vec<Fields> {
    let entries = entries(bucket, day);
    let has_points = entries.iter().any(is_setpoint);
    entries.into_iter().filter(|e| if has_points { is_setpoint(e) } else { midnight_carry(e) }).collect()
}

/// A range schedule's day (0 Monday), in time order.
pub fn range_day(bucket: &Fields, day: usize) -> Vec<Range> {
    let mut out: Vec<Range> = shown_entries(bucket, day)
        .iter()
        .filter_map(|e| match Temps::of(e)? {
            Temps::Range(low, high) => Some(((number(e.get("time"))? / 60.0).round() as u16, low, high)),
            Temps::One(_) => None,
        })
        .collect();
    out.sort_by_key(|(minutes, ..)| *minutes);
    out
}

/// A day's setpoints (0 Monday), in time order.
pub fn day(bucket: &Fields, day: usize) -> Vec<Setpoint> {
    let mut out: Vec<Setpoint> = shown_entries(bucket, day)
        .iter()
        .filter_map(|e| Some(((number(e.get("time"))? / 60.0).round() as u16, number(e.get("temp"))?)))
        .collect();
    out.sort_by_key(|(minutes, _)| *minutes);
    out
}

fn hhmm(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// °C to the hundredth, as heating_times shows it.
fn hundredth(c: f64) -> f64 {
    (c * 100.0).round() / 100.0
}

fn half(c: f64) -> f64 {
    (c * 2.0).round() / 2.0
}

/// The schedule as <base>/heating_times has it: {"mode": "heat", "days":
/// {"mon": [{"time": "06:00", "temp": 20.5}, ...], ...}}, every day named,
/// °C to the hundredth; a range schedule's setpoints are {"time", "low",
/// "high"}.
pub fn shown(bucket: &Fields) -> Value {
    let kind = kind(bucket);
    let days: Fields = DAYS
        .iter()
        .enumerate()
        .map(|(d, name)| {
            let list: Vec<Value> = match kind {
                Some(Kind::Range) => range_day(bucket, d)
                    .into_iter()
                    .map(|(m, low, high)| {
                        serde_json::json!({"time": hhmm(m), "low": num(hundredth(low)), "high": num(hundredth(high))})
                    })
                    .collect(),
                Some(_) => day(bucket, d)
                    .into_iter()
                    .map(|(m, c)| serde_json::json!({"time": hhmm(m), "temp": num(hundredth(c))}))
                    .collect(),
                None => Vec::new(),
            };
            ((*name).to_string(), Value::Array(list))
        })
        .collect();
    let mode = match kind {
        Some(Kind::Heat) => "heat",
        Some(Kind::Cool) => "cool",
        Some(Kind::Range) => "range",
        None => "unknown",
    };
    serde_json::json!({"mode": mode, "days": days})
}

const FULL_DAYS: [&str; 7] = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];

/// The schedule topic uses full day names and temperature for a single setpoint.
pub fn schedule_json(bucket: &Fields, times: &Value) -> Value {
    let days: Fields = DAYS
        .iter()
        .zip(FULL_DAYS)
        .enumerate()
        .map(|(index, (day, full))| {
            let mut points = times["days"][*day].as_array().cloned().unwrap_or_default();
            if !points.iter().any(|p| p["time"] == "00:00") {
                if let Some(carry) = entries(bucket, index).iter().find(|e| midnight_carry(e)).and_then(Temps::of) {
                    let point = match carry.map(hundredth) {
                        Temps::One(c) => serde_json::json!({"time":"00:00","temp":num(c)}),
                        Temps::Range(low, high) => serde_json::json!({"time":"00:00","low":num(low),"high":num(high)}),
                    };
                    points.insert(0, point);
                }
            }
            for point in points.iter_mut().filter_map(Value::as_object_mut) {
                if let Some(t) = point.remove("temp") {
                    point.insert("temperature".into(), t);
                }
            }
            (full.into(), Value::Array(points))
        })
        .collect();
    let mode = times["mode"].as_str().unwrap_or("unknown");
    serde_json::json!({"mode": if mode == "range" { "heat_cool" } else { mode }, "days": days})
}

pub fn schedule_days(arg: &str, mode: &str, bucket: &Fields) -> Result<String, String> {
    let value: Value = serde_json::from_str(arg).map_err(|e| e.to_string())?;
    let expected = if mode == "range" { "heat_cool" } else { mode };
    if value.get("mode").is_some_and(|m| m != expected) {
        return Err(format!("the schedule is {expected}"));
    }
    let mut days = Fields::new();
    for (name, points) in value["days"].as_object().ok_or("expected a days object")? {
        let index = FULL_DAYS.iter().position(|d| d == name).ok_or_else(|| format!("unknown day {name}"))?;
        let mut points = points.as_array().ok_or("expected an array of setpoints")?.clone();
        for point in &mut points {
            let p = point.as_object_mut().ok_or("expected a setpoint")?;
            if let Some(t) = p.remove("temperature") {
                p.insert("temp".into(), t);
            }
            if let Some(t) = p.get_mut("time") {
                if let Some(time) = t.as_str().filter(|s| s.len() == 4 && s.as_bytes()[1] == b':') {
                    *t = Value::from(format!("0{time}"));
                }
            }
        }
        // The alias includes the inherited target. Echoing it does not program a new midnight change.
        let existing = entries(bucket, index);
        let carry = existing.iter().find(|e| midnight_carry(e)).and_then(Temps::of).map(|t| t.map(hundredth));
        if let Some(carry) = carry.filter(|_| existing.iter().any(is_setpoint)) {
            if points.iter().filter(|p| p["time"] == "00:00").count() == 1
                && !existing.iter().any(|e| is_setpoint(e) && number(e.get("time")) == Some(0.0))
            {
                points.retain(|p| {
                    let temps = if mode == "range" {
                        number(p.get("low")).zip(number(p.get("high"))).map(|(l, h)| Temps::Range(l, h))
                    } else {
                        number(p.get("temp")).map(Temps::One)
                    };
                    p["time"] != "00:00" || temps != Some(carry)
                });
            }
        }
        days.insert(DAYS[index].into(), Value::Array(points));
    }
    Ok(Value::Object(days).to_string())
}

/// The heating_times command for these days' setpoints (0 Monday).
pub fn command(days: &[(usize, Vec<Setpoint>)]) -> String {
    let named: Fields = days
        .iter()
        .map(|(d, points)| {
            let list = points.iter().map(|(m, c)| serde_json::json!({"time": hhmm(*m), "temp": num(*c)}));
            (DAYS[*d].to_string(), Value::Array(list.collect()))
        })
        .collect();
    Value::Object(named).to_string()
}

/// The heating_times command for these days' ranges (0 Monday).
pub fn range_command(days: &[(usize, Vec<Range>)]) -> String {
    let named: Fields = days
        .iter()
        .map(|(d, points)| {
            let list = points
                .iter()
                .map(|(m, low, high)| serde_json::json!({"time": hhmm(*m), "low": num(*low), "high": num(*high)}));
            (DAYS[*d].to_string(), Value::Array(list.collect()))
        })
        .collect();
    Value::Object(named).to_string()
}

/// "HH:MM" as minutes after midnight.
pub fn minutes(s: &str) -> Option<u16> {
    let (h, m) = s.split_once(':')?;
    let two_digits = |p: &str| p.len() == 2 && p.bytes().all(|b| b.is_ascii_digit());
    if !two_digits(h) || !two_digits(m) {
        return None;
    }
    let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then(|| h * 60 + m)
}

/// The days a heating_times command names, with their new setpoints in
/// time order: {"mon": [{"time": "06:00", "temp": 20.5}, ...], ...}, or for
/// a range schedule {"mon": [{"time": "06:00", "low": 19, "high": 24}], ...}.
fn parse(arg: &str, range: bool) -> Result<Vec<(usize, Vec<Change>)>, String> {
    let expected = if range {
        r#"{"mon": [{"time": "06:00", "low": 19, "high": 24}], ...}"#
    } else {
        r#"{"mon": [{"time": "06:00", "temp": 20.5}], ...}"#
    };
    let Ok(Value::Object(named)) = serde_json::from_str::<Value>(arg) else {
        return Err(format!("expected {expected}"));
    };
    let within = |name: &str, c: f64| {
        if (SETPOINT.0..=SETPOINT.1).contains(&c) {
            Ok(c)
        } else {
            Err(format!("{name}: {c} outside {}-{} °C", SETPOINT.0, SETPOINT.1))
        }
    };
    let mut out = Vec::new();
    for (name, list) in &named {
        let d = DAYS.iter().position(|d| d == name).ok_or(format!("unknown day {name}"))?;
        let list = list.as_array().ok_or(format!("{name}: not a list"))?;
        if list.len() > DAILY {
            return Err(format!("{name}: more than {DAILY} setpoints"));
        }
        let mut points: Vec<Change> = Vec::new();
        for p in list {
            let m = p.get("time").and_then(Value::as_str).and_then(minutes);
            let temps = if range {
                let (Some(m), Some(low), Some(high)) = (m, number(p.get("low")), number(p.get("high"))) else {
                    return Err(format!(
                        "{name}: each setpoint of a range schedule needs a time HH:MM, a low and a high"
                    ));
                };
                // The stored values are checked again after rounding to the display scale.
                let gapless = format!("{name}: {low}-{high} °C: the high must be {RANGE_GAP} °C or more above the low");
                if high - low < RANGE_GAP - 1e-9 {
                    return Err(gapless);
                }
                let (low, high) = (within(name, low)?, within(name, high)?);
                (m, Temps::Range(low, high))
            } else {
                let (Some(m), Some(c)) = (m, number(p.get("temp"))) else {
                    return Err(format!("{name}: each setpoint needs a time HH:MM and a temp"));
                };
                (m, Temps::One(within(name, c)?))
            };
            if points.iter().any(|(pm, _)| *pm == temps.0) {
                return Err(format!("{name}: two setpoints at {}", hhmm(temps.0)));
            }
            points.push(temps);
        }
        points.sort_by_key(|(m, _)| *m);
        out.push((d, points));
    }
    Ok(out)
}

/// The schedule bucket's fields a heating_times command writes: the whole
/// week, the days it names replaced.
pub fn write(bucket: &Fields, arg: &str, now: i64, fahrenheit: bool, limits: (f64, f64)) -> Result<Fields, String> {
    let kind = kind(bucket).ok_or("the unit's schedule is of a kind not written here")?;
    let changes = parse(arg, kind == Kind::Range)?;
    if changes.is_empty() {
        return Ok(bucket.get("days").map_or_else(Fields::new, |d| fields! {"days"=>d.clone()}));
    }
    if let Some(raw) = bucket.get("days") {
        let week = raw.as_object().ok_or("unrecognized schedule days")?;
        for (name, day) in week {
            let entries = day.as_object().ok_or("unrecognized schedule day")?;
            let mut carry = false;
            if !matches!(name.as_str(), "0" | "1" | "2" | "3" | "4" | "5" | "6")
                || !entries.values().all(|e| {
                    let Some(e) = e.as_object() else { return false };
                    let valid_temp = matches!(
                        (kind, Temps::of(e)),
                        (Kind::Heat | Kind::Cool, Some(Temps::One(_))) | (Kind::Range, Some(Temps::Range(_, _)))
                    );
                    if !valid_temp || !number(e.get("time")).is_some_and(|t| (0.0..86400.0).contains(&t)) {
                        return false;
                    }
                    if is_setpoint(e) {
                        return true;
                    }
                    if midnight_carry(e) && !carry {
                        carry = true;
                        return true;
                    }
                    false
                })
            {
                return Err(format!("unrecognized schedule entry on day {name}"));
            }
        }
    }
    let stored = |c: f64| {
        if fahrenheit {
            let f = |c: f64| c * 1.8 + 32.0;
            let degrees = f(c).round().clamp(f(limits.0).ceil(), f(limits.1).floor());
            (((degrees - 32.0) / 1.8) * 100_000.0).round() / 100_000.0
        } else {
            half(c)
        }
    };
    let offset = chrono::Local.timestamp_opt(now, 0).single().map_or(0, |t| t.offset().local_minus_utc());
    let typ = match kind {
        Kind::Heat => "HEAT",
        Kind::Cool => "COOL",
        Kind::Range => "RANGE",
    };
    let entry = |m: u16, temps: Temps, entry_type: &str| {
        let mut e = fields! {
            "entry_type" => entry_type,
            "time" => i64::from(m) * 60,
            "touched_at" => now,
            "touched_by" => TOUCHED_BY_LOCAL,
            "touched_tzo" => offset,
            "type" => typ,
        };
        e.extend(temps.fields());
        e
    };
    // Each day's setpoint entries: the command's, or the ones it has.
    let mut week: Vec<Vec<Fields>> = (0..7)
        .map(|d| {
            let mut points: Vec<Fields> = entries(bucket, d).into_iter().filter(is_setpoint).collect();
            points.sort_by_key(|e| number(e.get("time")).map_or(0, |t| t as i64));
            points
        })
        .collect();
    let clear_week = changes.len() == 7 && changes.iter().all(|(_, points)| points.is_empty());
    let mut changed = clear_week;
    for (d, points) in changes {
        let old = entries(bucket, d);
        let mut replacement = Vec::new();
        for (m, temps) in points {
            // An echoed midnight carry stays a carry, including on stock weeks.
            if let Some(e) = old.iter().find(|e| {
                number(e.get("time")).is_some_and(|s| (s / 60.0).round() == f64::from(m))
                    && Temps::of(e).is_some_and(|old| old.map(hundredth) == temps || old == temps.map(stored))
                    && (is_setpoint(e)
                        || (m == 0 && week[d].is_empty() && sprint(e.get("entry_type")) == "continuation"))
            }) {
                if is_setpoint(e) {
                    replacement.push(e.clone());
                }
            } else {
                let rounded = temps.map(stored);
                if matches!(rounded, Temps::Range(low, high) if high - low < RANGE_GAP - 1e-9) {
                    return Err(format!("{}: range is too narrow on the thermostat's temperature scale", DAYS[d]));
                }
                replacement.push(entry(m, rounded, "setpoint"));
            }
        }
        changed |= week[d] != replacement;
        week[d] = replacement;
    }
    if !changed {
        if let Some(days) = bucket.get("days") {
            return Ok(fields! {"days" => days.clone()});
        }
    }
    // Midnight carries on from the last setpoint before it, however many
    // days back; a week without setpoints keeps the continuations it has.
    let before = |d: usize| (1..=7).find_map(|back| Temps::of(week[(d + 7 - back) % 7].last()?));
    let mut days = Fields::new();
    for (d, points) in week.iter().enumerate() {
        let old = entries(bucket, d).into_iter().find(|e| sprint(e.get("entry_type")) == "continuation");
        let continuation = match (before(d), old) {
            (Some(temps), Some(old)) if Temps::of(&old) == Some(temps) => Some(old),
            (Some(temps), _) => Some(entry(0, temps, "continuation")),
            (None, old) => old,
        };
        let has_midnight = points.first().is_some_and(|e| number(e.get("time")) == Some(0.0));
        let all = continuation.filter(|_| !has_midnight && !clear_week).into_iter().chain(points.iter().cloned());
        let day: Fields = all.enumerate().map(|(i, e)| (i.to_string(), Value::Object(e))).collect();
        days.insert(d.to_string(), Value::Object(day));
    }
    Ok(fields! {"days" => Value::Object(days)})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::canonical;
    use serde_json::json;

    fn obj(v: Value) -> Fields {
        match canonical(v) {
            Value::Object(m) => m,
            _ => unreachable!(),
        }
    }

    fn sp(time: i64, temp: f64) -> Value {
        json!({"entry_type": "setpoint", "temp": temp, "time": time, "touched_by": 1, "type": "HEAT"})
    }

    fn cont(temp: f64) -> Value {
        json!({"entry_type": "continuation", "temp": temp, "time": 0, "touched_by": 1, "type": "HEAT"})
    }

    /// Every day: 06:00 20.5, 22:00 18.
    fn bucket() -> Fields {
        let day = json!({"0": cont(18.0), "1": sp(21600, 20.5), "2": sp(79200, 18.0)});
        let days: Fields = (0..7).map(|d| (d.to_string(), day.clone())).collect();
        obj(json!({"days": days, "schedule_mode": "HEAT", "ver": 2}))
    }

    fn write(bucket: &Fields, arg: &str, now: i64) -> Result<Fields, String> {
        super::write(bucket, arg, now, false, SETPOINT)
    }

    #[test]
    fn shows_each_days_setpoints_without_continuations() {
        let mut b = bucket();
        b["days"]["4"]["2"] = canonical(sp(79200, 17.89737));
        let s = shown(&b);
        assert_eq!(s["mode"], json!("heat"));
        assert_eq!(s["days"]["mon"], json!([{"time": "06:00", "temp": 20.5}, {"time": "22:00", "temp": 18}]));
        assert_eq!(s["days"]["fri"][1]["temp"], json!(17.9));
        assert_eq!(shown(&Fields::new())["days"]["sun"], json!([]), "no schedule yet");
        b["schedule_mode"] = json!("OTHER");
        assert_eq!(shown(&b)["mode"], "unknown");
        b["schedule_mode"] = json!("RANGE");
        assert_eq!((shown(&b)["mode"].clone(), shown(&b)["days"]["mon"].clone()), (json!("range"), json!([])));
    }

    #[test]
    fn writes_the_named_days_and_keeps_the_rest() {
        let b = bucket();
        let f = write(&b, r#"{"fri": [{"time": "22:30", "temp": 17}, {"time": "07:00", "temp": 21.2}]}"#, 99).unwrap();
        let days = &f["days"];
        assert_eq!(days["4"].as_object().unwrap().len(), 3);
        assert_eq!(
            days["4"]["1"],
            json!({"entry_type": "setpoint", "temp": 21, "time": 25200, "touched_at": 99, "touched_by": 4,
                   "touched_tzo": chrono::Local.timestamp_opt(99, 0).unwrap().offset().local_minus_utc(), "type": "HEAT"})
        );
        assert_eq!((&days["4"]["2"]["time"], &days["4"]["2"]["temp"]), (&json!(81000), &json!(17)));
        assert_eq!(days["4"]["0"], canonical(cont(18.0)), "an unchanged continuation stays as it was");
        // Saturday carries on from Friday's new last setpoint.
        assert_eq!((&days["5"]["0"]["entry_type"], &days["5"]["0"]["temp"]), (&json!("continuation"), &json!(17)));
        assert_eq!(days["5"]["1"], canonical(sp(21600, 20.5)), "untouched days keep their setpoints");
        assert_eq!(day(&obj(Value::Object(f.clone())), 4), [(420, 21.0), (1350, 17.0)]);
    }

    #[test]
    fn an_empty_day_carries_on_from_the_day_before_it() {
        let f = write(&bucket(), r#"{"sat": []}"#, 1).unwrap();
        assert_eq!(f["days"]["5"].as_object().unwrap().len(), 1, "only its continuation");
        assert_eq!(f["days"]["6"]["0"]["temp"], json!(18));
        let f = write(&Fields::new(), r#"{"mon": [{"time": "06:00", "temp": 20}]}"#, 1).unwrap();
        assert_eq!(f["days"]["3"]["0"]["temp"], json!(20), "a week of one setpoint");
        assert_eq!(f["days"]["0"]["1"]["type"], json!("HEAT"), "a unit with no schedule yet heats");
        // A week of continuations alone, as a unit holding one temperature has.
        let held = obj(json!({"days": {"0": {"0": cont(20.0)}, "1": {"0": cont(20.0)}}, "schedule_mode": "HEAT"}));
        let f = write(&held, r#"{"mon": []}"#, 1).unwrap();
        assert_eq!((&f["days"]["0"]["0"], &f["days"]["1"]["0"]), (&canonical(cont(20.0)), &canonical(cont(20.0))));
    }

    #[test]
    fn command_round_trips_through_write() {
        let cmd = command(&[(1, vec![(390, 19.5)]), (6, vec![])]);
        assert_eq!(cmd, r#"{"sun":[],"tue":[{"temp":19.5,"time":"06:30"}]}"#);
        let f = obj(Value::Object(write(&bucket(), &cmd, 1).unwrap()));
        assert_eq!((day(&f, 1), day(&f, 6)), (vec![(390, 19.5)], vec![(0, 18.0)]));
    }

    #[test]
    fn held_weeks_round_trip_and_midnight_changes_replace_the_carry() {
        let days: Fields = (0..7).map(|d| (d.to_string(), json!({"0":cont(20.)}))).collect();
        let held = obj(json!({"days":days, "schedule_mode":"HEAT"}));
        let shown = shown(&held);
        assert_eq!(shown["days"]["mon"], json!([{"time":"00:00","temp":20}]));
        let alias = schedule_json(&held, &shown);
        assert_eq!(
            write(&held, &schedule_days(&alias.to_string(), "heat", &held).unwrap(), 1).unwrap()["days"],
            held["days"]
        );
        let changed = write(&held, r#"{"mon":[{"time":"00:00","temp":21}]}"#, 1).unwrap();
        assert_eq!(changed["days"]["0"].as_object().unwrap().len(), 1);
        assert_eq!(changed["days"]["0"]["0"]["entry_type"], "setpoint");
    }

    #[test]
    fn legacy_points_survive_partial_edits_and_alias_midnights_round_trip() {
        let mut b = bucket();
        b["days"]["0"]["1"].as_object_mut().unwrap().remove("entry_type");
        let legacy = b["days"]["0"]["1"].clone();
        let shown = shown(&b);
        assert_eq!(shown["days"]["mon"][0]["time"], "06:00");
        let alias = schedule_json(&b, &shown);
        assert_eq!(alias["days"]["monday"][0], json!({"time":"00:00","temperature":18}));
        let arg = schedule_days(&alias.to_string(), "heat", &b).unwrap();
        assert_eq!(write(&b, &arg, 2).unwrap()["days"], b["days"]);
        let input = r#"{"days":{"tuesday":[{"time":"6:30","temperature":22}]}}"#;
        let updated = write(&b, &schedule_days(input, "heat", &b).unwrap(), 2).unwrap();
        assert_eq!(updated["days"]["0"]["1"], legacy);
        assert_eq!(updated["days"]["1"]["1"]["time"], 23400);
        b["days"]["0"]["1"]["entry_type"] = json!("unknown");
        assert!(write(&b, r#"{"tue":[]}"#, 2).is_err());
    }

    #[test]
    fn new_fahrenheit_setpoints_use_whole_degrees_and_the_local_offset() {
        let f = super::write(&bucket(), r#"{"mon":[{"time":"07:00","temp":20.56}]}"#, 1, true, (9., 32.)).unwrap();
        let point = &f["days"]["0"]["1"];
        assert_eq!(point["temp"], json!(20.55556));
        assert_eq!(point["touched_tzo"], chrono::Local.timestamp_opt(1, 0).unwrap().offset().local_minus_utc());
        assert!(super::write(
            &range_bucket(),
            r#"{"mon":[{"time":"07:00","low":19.75,"high":21.25}]}"#,
            1,
            true,
            (9., 32.)
        )
        .is_err());
    }

    #[test]
    fn times_are_two_digits_each() {
        assert_eq!(minutes("06:30"), Some(390));
        for bad in ["1:002", "001:00", "6:30", "24:00", "23:60", "99999:00", "+1:00", "06:3x", "0630"] {
            assert_eq!(minutes(bad), None, "{bad}");
        }
    }

    #[test]
    fn refuses_what_the_unit_cannot_take() {
        let eleven: Vec<Value> = (0..11).map(|h| json!({"time": format!("{h:02}:00"), "temp": 20})).collect();
        let too_many = json!({"mon": eleven}).to_string();
        for arg in [
            "[]",
            "{",
            r#"{"funday": []}"#,
            r#"{"mon": {}}"#,
            r#"{"mon": [{"time": "6:00", "temp": 20}]}"#,
            r#"{"mon": [{"time": "24:00", "temp": 20}]}"#,
            r#"{"mon": [{"time": "06:00", "temp": 40}]}"#,
            r#"{"mon": [{"time": "06:00"}]}"#,
            r#"{"mon": [{"time": "06:00", "temp": 20}, {"time": "06:00", "temp": 21}]}"#,
            &too_many,
        ] {
            assert!(write(&bucket(), arg, 1).is_err(), "{arg} accepted");
        }
        assert!(write(&range_bucket(), r#"{"mon": [{"time": "06:00", "temp": 20}]}"#, 1).is_err(), "a range's");
        assert!(write(&bucket(), r#"{"mon": [{"time": "06:00", "low": 19, "high": 24}]}"#, 1).is_err(), "heat's");
    }

    /// A range schedule, as the unit has one: every day 03:00 18-25 °C.
    fn range_bucket() -> Fields {
        let entry = |entry_type: &str, time: i64| {
            json!({"entry_type": entry_type, "temp-max": 25, "temp-min": 18, "time": time, "touched_by": 4,
                   "type": "RANGE"})
        };
        let day = json!({"0": entry("continuation", 0), "1": entry("setpoint", 10800)});
        let days: Fields = (0..7).map(|d| (d.to_string(), day.clone())).collect();
        obj(json!({"days": days, "schedule_mode": "RANGE", "ver": 2}))
    }

    #[test]
    fn a_range_schedule_shows_its_lows_and_highs() {
        let b = range_bucket();
        assert_eq!(kind(&b), Some(Kind::Range));
        assert_eq!(range_day(&b, 2), [(180, 18.0, 25.0)]);
        assert_eq!(day(&b, 2), [], "no single temperatures");
        let s = shown(&b);
        assert_eq!(s["mode"], json!("range"));
        assert_eq!(s["days"]["wed"], json!([{"time": "03:00", "low": 18, "high": 25}]));
    }

    #[test]
    fn a_range_schedule_takes_lows_and_highs() {
        let cmd = r#"{"tue": [{"time": "06:30", "low": 19.2, "high": 24}, {"time": "22:00", "low": 17, "high": 26}]}"#;
        let f = write(&range_bucket(), cmd, 7).unwrap();
        let days = &f["days"];
        assert_eq!(
            days["1"]["1"],
            json!({"entry_type": "setpoint", "temp-min": 19, "temp-max": 24, "time": 23400, "touched_at": 7,
                   "touched_by": 4, "touched_tzo": chrono::Local.timestamp_opt(99, 0).unwrap().offset().local_minus_utc(), "type": "RANGE"}),
            "in half degrees"
        );
        // Wednesday carries on from Tuesday's last range.
        let wed = &days["2"]["0"];
        assert_eq!(
            (&wed["entry_type"], &wed["temp-min"], &wed["temp-max"]),
            (&json!("continuation"), &json!(17), &json!(26))
        );
        let f = obj(Value::Object(f.clone()));
        assert_eq!(range_day(&f, 1), [(390, 19.0, 24.0), (1320, 17.0, 26.0)]);
        assert_eq!(range_day(&f, 4), [(180, 18.0, 25.0)], "untouched days keep theirs");
        assert_eq!(range_command(&[(1, vec![(390, 19.0, 24.0)])]), r#"{"tue":[{"high":24,"low":19,"time":"06:30"}]}"#);
    }

    #[test]
    fn a_range_keeps_its_high_above_its_low() {
        for arg in [
            r#"{"mon": [{"time": "06:00", "low": 20, "high": 21}]}"#,
            r#"{"mon": [{"time": "06:00", "low": 22, "high": 20}]}"#,
            r#"{"mon": [{"time": "06:00", "low": 20}]}"#,
            r#"{"mon": [{"time": "06:00", "low": 2, "high": 24}]}"#,
            r#"{"mon": [{"time": "06:00", "low": 20.2, "high": 21.3}]}"#,
        ] {
            assert!(write(&range_bucket(), arg, 1).is_err(), "{arg} accepted");
        }
        assert!(write(&range_bucket(), r#"{"mon": [{"time": "06:00", "low": 20, "high": 21.5}]}"#, 1).is_ok());
    }
}
