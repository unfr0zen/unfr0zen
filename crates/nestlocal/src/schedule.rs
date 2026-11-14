//! The unit's schedule bucket as transitions, for every protocol: the
//! Matter schedules (matter/schedule.rs), Zigbee's weekly schedule
//! commands and MQTT's schedule topic all read and write it through these.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fields;
use crate::json::{num, Fields};

/// Transitions on one day, as the unit's schedule screen fits them.
pub const PER_DAY: u8 = 8;

pub const MINUTES_PER_DAY: u16 = 1440;

/// As commands.rs marks the owner's own changes.
pub const TOUCHED_BY_LOCAL: i64 = 4;

/// A schedule's system mode: the unit's schedule_mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Heat,
    Cool,
    /// The unit's RANGE: a low and a high.
    Auto,
}

impl Mode {
    pub fn nest(self) -> &'static str {
        match self {
            Mode::Heat => "HEAT",
            Mode::Cool => "COOL",
            Mode::Auto => "RANGE",
        }
    }

    pub fn from_nest(s: &str) -> Option<Self> {
        match s {
            "HEAT" => Some(Mode::Heat),
            "COOL" => Some(Mode::Cool),
            "RANGE" => Some(Mode::Auto),
            _ => None,
        }
    }
}

/// One transition: from `minutes` after local midnight on `days` (Matter's
/// day bits, Sunday 0x1), the setpoints. Temperatures in 0.01 °C.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    pub days: u8,
    pub minutes: u16,
    pub heat: Option<i16>,
    pub cool: Option<i16>,
}

/// The unit's Nest day (0, Monday, to 6) as Matter's day bit.
pub fn day_bit(day: usize) -> u8 {
    1 << ((day + 1) % 7)
}

pub fn centi(celsius: f64) -> i16 {
    (celsius * 100.0).round() as i16
}

/// To the unit's half degrees.
pub fn half_degree(centi: i16) -> i16 {
    ((f64::from(centi) / 50.0).round() * 50.0) as i16
}

pub fn celsius(centi: i16) -> Value {
    num(f64::from(centi) / 100.0)
}

/// Whether a bucket entry is a setpoint; the other kind is a continuation,
/// the day before's last setpoint again at 00:00.
fn is_setpoint(e: &Value) -> bool {
    e.get("entry_type").and_then(Value::as_str).is_none_or(|t| t == "setpoint")
}

/// A bucket entry's setpoint as a transition on no day yet.
pub fn entry_transition(e: &Value) -> Option<Transition> {
    is_setpoint(e).then(|| entry_values(e)).flatten()
}

/// Any bucket entry's time and temperatures.
fn entry_values(e: &Value) -> Option<Transition> {
    let minutes = (e.get("time")?.as_f64()? / 60.0).floor();
    if !(0.0..f64::from(MINUTES_PER_DAY)).contains(&minutes) {
        return None;
    }
    let temp = |key: &str| e.get(key).and_then(Value::as_f64).map(centi);
    let (heat, cool) = match Mode::from_nest(e.get("type")?.as_str()?)? {
        Mode::Heat => (Some(temp("temp")?), None),
        Mode::Cool => (None, Some(temp("temp")?)),
        Mode::Auto => (Some(temp("temp-min")?), Some(temp("temp-max")?)),
    };
    Some(Transition { days: 0, minutes: minutes as u16, heat, cool })
}

/// The schedule bucket as one schedule's mode and transitions, a transition
/// for each time and setpoint with every day that has it.
pub fn from_bucket(bucket: &Fields) -> Option<(Mode, Vec<Transition>)> {
    let mode = bucket.get("schedule_mode").and_then(Value::as_str).and_then(Mode::from_nest).unwrap_or(Mode::Heat);
    let days = bucket.get("days")?.as_object()?;
    let mut transitions: Vec<Transition> = Vec::new();
    for day in 0..7 {
        let Some(entries) = days.get(&day.to_string()).and_then(Value::as_object) else { continue };
        for t in entries.values().filter_map(entry_transition) {
            match transitions.iter_mut().find(|o| (o.minutes, o.heat, o.cool) == (t.minutes, t.heat, t.cool)) {
                Some(o) => o.days |= day_bit(day),
                None => transitions.push(Transition { days: day_bit(day), ..t }),
            }
        }
    }
    transitions.sort_by_key(|t| (t.minutes, t.heat, t.cool));
    Some((mode, transitions))
}

pub fn new_entry(mode: Mode, t: &Transition, now: i64) -> Value {
    let mut e = fields! {
        "entry_type" => "setpoint",
        "time" => num(f64::from(t.minutes) * 60.0),
        "touched_at" => num(now as f64),
        "touched_by" => num(TOUCHED_BY_LOCAL as f64),
        "touched_tzo" => num(0.0),
        "type" => mode.nest(),
    };
    match mode {
        Mode::Heat => {
            e.insert("temp".into(), celsius(t.heat.unwrap_or_default()));
        }
        Mode::Cool => {
            e.insert("temp".into(), celsius(t.cool.unwrap_or_default()));
        }
        Mode::Auto => {
            e.insert("temp-min".into(), celsius(t.heat.unwrap_or_default()));
            e.insert("temp-max".into(), celsius(t.cool.unwrap_or_default()));
        }
    }
    Value::Object(e)
}

/// The schedule bucket's fields for a schedule: every day's entries, the
/// ones it already had kept as they were (their touched_ marks with them).
/// A day that starts after midnight opens with a continuation, as the
/// unit's own schedules do.
pub fn to_bucket(mode: Mode, transitions: &[Transition], current: Option<&Fields>, now: i64) -> Fields {
    let old_days = current.and_then(|b| b.get("days")).and_then(Value::as_object);
    let per_day: [Vec<&Transition>; 7] = std::array::from_fn(|day| {
        let mut mine: Vec<&Transition> = transitions.iter().filter(|t| t.days & day_bit(day) != 0).collect();
        mine.sort_by_key(|t| t.minutes);
        mine
    });
    let mut days = Fields::new();
    for day in 0..7 {
        let old: Vec<&Value> = old_days
            .and_then(|d| d.get(&day.to_string()))
            .and_then(Value::as_object)
            .map(|o| o.values().collect())
            .unwrap_or_default();
        let kept = |setpoint: bool, want: &Transition| {
            old.iter()
                .find(|e| {
                    e.get("type").and_then(Value::as_str) == Some(mode.nest())
                        && is_setpoint(e) == setpoint
                        && entry_values(e).as_ref() == Some(want)
                })
                .map(|e| (*e).clone())
        };
        let mut entries: Vec<Value> = Vec::new();
        if per_day[day].first().is_none_or(|t| t.minutes != 0) {
            // The last setpoint before this day, a week back at most.
            if let Some(last) = (1..=7).find_map(|back| per_day[(day + 7 - back) % 7].last()) {
                let want = Transition { days: 0, minutes: 0, ..(*last).clone() };
                entries.push(kept(false, &want).unwrap_or_else(|| {
                    let mut e = new_entry(mode, &want, now);
                    e["entry_type"] = "continuation".into();
                    e
                }));
            }
        }
        for t in &per_day[day] {
            let want = Transition { days: 0, ..(*t).clone() };
            entries.push(kept(true, &want).unwrap_or_else(|| new_entry(mode, t, now)));
        }
        days.insert(
            day.to_string(),
            Value::Object(entries.into_iter().enumerate().map(|(i, e)| (i.to_string(), e)).collect()),
        );
    }
    fields! {"days" => Value::Object(days), "schedule_mode" => mode.nest()}
}
