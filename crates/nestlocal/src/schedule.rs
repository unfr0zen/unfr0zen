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

/// The setpoints a schedule may hold, 0.01 °C, as the Matter and Zigbee
/// thermostats advertise them, and the least gap a range keeps.
pub const LOWEST: i16 = 900;
pub const HIGHEST: i16 = 3200;
pub const DEAD_BAND: i16 = 150;

/// The unit's days, from its day 0, as MQTT names them.
pub const DAYS: [&str; 7] = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];

/// One transition on one day: minutes after midnight and its setpoints
/// (the heat one, the cool one or both, as the schedule's mode has them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Point {
    pub minutes: u16,
    pub heat: Option<i16>,
    pub cool: Option<i16>,
}

/// The schedule's mode and each of the unit's days' points, in time order.
pub fn week(bucket: &Fields) -> Option<(Mode, [Vec<Point>; 7])> {
    let (mode, transitions) = from_bucket(bucket)?;
    let days = std::array::from_fn(|day| {
        let mut points: Vec<Point> = transitions
            .iter()
            .filter(|t| t.days & day_bit(day) != 0)
            .map(|t| Point { minutes: t.minutes, heat: t.heat, cool: t.cool })
            .collect();
        points.sort_by_key(|p| p.minutes);
        points
    });
    Some((mode, days))
}

/// The schedule bucket's fields with each of `days` (the unit's days, 0 to
/// 6) given its points and every other day as it was. `mode`, when it is
/// not the schedule's, needs every day, since a schedule has one mode.
/// Setpoints go to the unit's half degrees.
pub fn with_days(
    bucket: Option<&Fields>,
    mode: Option<Mode>,
    days: &[(usize, Vec<Point>)],
    now: i64,
) -> Result<Fields, String> {
    let (current, mut transitions) = bucket.and_then(from_bucket).unwrap_or((Mode::Heat, Vec::new()));
    let mode = mode.unwrap_or(current);
    if mode != current {
        if !(0..7).all(|d| days.iter().any(|(day, _)| *day == d)) {
            return Err(format!("the schedule is {}: another mode needs every day", mode_name(current)));
        }
        transitions.clear();
    }
    let given = days.iter().fold(0u8, |mask, (day, _)| mask | day_bit(*day));
    for t in &mut transitions {
        t.days &= !given;
    }
    transitions.retain(|t| t.days != 0);
    let setpoint = |t: Option<i16>, which: &str| match t {
        Some(t) if (LOWEST..=HIGHEST).contains(&t) => Ok(half_degree(t)),
        Some(_) => Err(format!("a {which} setpoint is {}-{} °C", LOWEST / 100, HIGHEST / 100)),
        None => Err(format!("each transition needs a {which} setpoint")),
    };
    for (day, points) in days {
        let name = DAYS.get(*day).ok_or("no such day")?;
        if points.len() > usize::from(PER_DAY) {
            return Err(format!("{name}: at most {PER_DAY} transitions"));
        }
        let mut times = std::collections::BTreeSet::new();
        for p in points {
            if p.minutes >= MINUTES_PER_DAY || !times.insert(p.minutes) {
                return Err(format!("{name}: times are before 24:00 and each once"));
            }
            let (heat, cool) = match mode {
                Mode::Heat => (Some(setpoint(p.heat, "heat")?), None),
                Mode::Cool => (None, Some(setpoint(p.cool, "cool")?)),
                Mode::Auto => {
                    let (low, high) = (setpoint(p.heat, "low")?, setpoint(p.cool, "high")?);
                    if high - low < DEAD_BAND {
                        return Err(format!("{name}: the high is at least {} °C above the low", DEAD_BAND / 100));
                    }
                    (Some(low), Some(high))
                }
            };
            transitions.push(Transition { days: day_bit(*day), minutes: p.minutes, heat, cool });
        }
    }
    Ok(to_bucket(mode, &transitions, bucket, now))
}

/// A mode as the MQTT state topic names it.
pub fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Heat => "heat",
        Mode::Cool => "cool",
        Mode::Auto => "heat_cool",
    }
}

fn hhmm(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// The schedule as MQTT's schedule topic has it: its mode, and each day's
/// transitions with a time and a temperature (a low and a high in
/// heat_cool), °C.
pub fn to_json(bucket: &Fields) -> Option<Value> {
    let (mode, days) = week(bucket)?;
    let mut out = Fields::new();
    for (name, points) in DAYS.iter().zip(&days) {
        let list: Vec<Value> = points
            .iter()
            .map(|p| {
                let mut t = fields! {"time" => hhmm(p.minutes)};
                match mode {
                    Mode::Heat => {
                        t.insert("temperature".into(), celsius(p.heat.unwrap_or_default()));
                    }
                    Mode::Cool => {
                        t.insert("temperature".into(), celsius(p.cool.unwrap_or_default()));
                    }
                    Mode::Auto => {
                        t.insert("low".into(), celsius(p.heat.unwrap_or_default()));
                        t.insert("high".into(), celsius(p.cool.unwrap_or_default()));
                    }
                }
                Value::Object(t)
            })
            .collect();
        out.insert((*name).into(), Value::Array(list));
    }
    Some(Value::Object(fields! {"mode" => mode_name(mode), "days" => Value::Object(out)}))
}

/// MQTT's set/schedule: the days given (in to_json's shape) replace those
/// days, the rest stay as they are; a mode is optional.
pub fn from_json(arg: &str, bucket: Option<&Fields>, now: i64) -> Result<Fields, String> {
    let shape = "expected {\"days\": {\"monday\": [{\"time\": \"07:00\", \"temperature\": 20}, ...], ...}}";
    let v: Value = serde_json::from_str(arg).map_err(|_| shape.to_string())?;
    let mode = match v.get("mode").and_then(Value::as_str) {
        None => None,
        Some("heat") => Some(Mode::Heat),
        Some("cool") => Some(Mode::Cool),
        Some("heat_cool") => Some(Mode::Auto),
        Some(m) => return Err(format!("mode {m:?}: heat, cool or heat_cool")),
    };
    let given = v.get("days").and_then(Value::as_object).ok_or(shape)?;
    // Without a mode, a temperature is the schedule's own: heat or cool.
    let mode = mode.or_else(|| bucket.and_then(from_bucket).map(|(m, _)| m));
    let degrees = |t: &Value, key: &str| t.get(key).and_then(Value::as_f64).map(centi);
    let mut days = Vec::new();
    for (name, list) in given {
        let day = DAYS.iter().position(|d| d == name).ok_or_else(|| format!("{name:?}: a day is monday to sunday"))?;
        let mut points = Vec::new();
        for t in list.as_array().ok_or(shape)? {
            let time = t.get("time").and_then(Value::as_str).ok_or("each transition has a time, \"HH:MM\"")?;
            let minutes = time
                .split_once(':')
                .and_then(|(h, m)| Some((h.parse::<u16>().ok()?, m.parse::<u16>().ok()?)))
                .filter(|&(h, m)| h < 24 && m < 60 && time.len() <= 5)
                .map(|(h, m)| h * 60 + m)
                .ok_or_else(|| format!("{name}: time {time:?} is HH:MM"))?;
            let (heat, cool) = match mode {
                Some(Mode::Cool) => (None, degrees(t, "temperature")),
                Some(Mode::Auto) => (degrees(t, "low"), degrees(t, "high")),
                _ => (degrees(t, "temperature"), None),
            };
            points.push(Point { minutes, heat, cool });
        }
        days.push((day, points));
    }
    with_days(bucket, mode, &days, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(time: i64, temp: f64) -> Value {
        json!({"entry_type": "setpoint", "temp": temp, "time": time, "touched_at": 1, "touched_by": 5,
               "touched_tzo": 0, "type": "HEAT"})
    }

    /// 18 °C at midnight and 21 °C at 17:00 every day, 19 °C at 09:00 on Mondays.
    fn bucket() -> Fields {
        let mut days = serde_json::Map::new();
        for day in 0..7 {
            let mut e = vec![entry(0, 18.0), entry(61200, 21.0)];
            if day == 0 {
                e.push(entry(32400, 19.0));
            }
            days.insert(day.to_string(), e.into_iter().enumerate().map(|(i, e)| (i.to_string(), e)).collect());
        }
        fields! {"days" => Value::Object(days), "schedule_mode" => "HEAT"}
    }

    fn heat(minutes: u16, centi: i16) -> Point {
        Point { minutes, heat: Some(centi), cool: None }
    }

    #[test]
    fn the_week_is_each_day_s_points_in_time_order() {
        let (mode, days) = week(&bucket()).unwrap();
        assert_eq!(mode, Mode::Heat);
        assert_eq!(days[0], vec![heat(0, 1800), heat(540, 1900), heat(1020, 2100)]);
        assert_eq!(days[6], vec![heat(0, 1800), heat(1020, 2100)]);
    }

    #[test]
    fn days_given_are_replaced_and_the_rest_kept_with_their_marks() {
        let b = bucket();
        let f = with_days(Some(&b), None, &[(1, vec![heat(390, 2010)])], 99).unwrap();
        let (_, days) = week(&f).unwrap();
        assert_eq!(days[1], vec![heat(390, 2000)], "to the unit's half degree");
        assert_eq!(days[0], week(&b).unwrap().1[0]);
        assert_eq!(f["days"]["0"]["1"]["touched_by"], json!(5), "an untouched day keeps its entries");
        assert_eq!(f["days"]["1"]["0"]["touched_at"], json!(99));
        // An empty day has no transitions; a later day carries the one before on.
        let f = with_days(Some(&b), None, &[(2, vec![])], 0).unwrap();
        assert!(week(&f).unwrap().1[2].is_empty());
    }

    #[test]
    fn a_day_starting_after_midnight_opens_with_the_last_setpoint_before_it() {
        let b = bucket();
        let f = with_days(Some(&b), None, &[(2, vec![heat(390, 2000)]), (4, vec![])], 99).unwrap();
        let first = |f: &Fields, day: &str| {
            let e = &f["days"][day]["0"];
            (e["entry_type"].as_str().unwrap().to_owned(), e["time"].as_f64().unwrap(), e["temp"].as_f64().unwrap())
        };
        assert_eq!(first(&f, "2"), ("continuation".to_owned(), 0.0, 21.0), "Tuesday's 21 °C carries on");
        assert_eq!(f["days"]["2"]["1"]["time"].as_f64(), Some(23400.0));
        assert_eq!(week(&f).unwrap().1[2], vec![heat(390, 2000)], "a continuation is no transition");
        assert_eq!(first(&f, "4"), ("continuation".to_owned(), 0.0, 21.0), "an empty day has it alone");
        assert_eq!(f["days"]["4"].as_object().unwrap().len(), 1);
        assert_eq!(first(&f, "5").0, "setpoint", "a day starting at midnight has none");
        // Another write keeps it as it was; one that changes the day before renews it.
        let g = with_days(Some(&f), None, &[(0, vec![heat(0, 1800)])], 100).unwrap();
        assert_eq!(g["days"]["2"]["0"]["touched_at"].as_f64(), Some(99.0));
        let g = with_days(Some(&f), None, &[(1, vec![heat(0, 1800), heat(1020, 2000)])], 100).unwrap();
        assert_eq!(first(&g, "2"), ("continuation".to_owned(), 0.0, 20.0));
        assert_eq!(g["days"]["2"]["0"]["touched_at"].as_f64(), Some(100.0));
        // What the unit wrote itself is read as before.
        let mut own = b.clone();
        own["days"]["3"]["0"]["entry_type"] = json!("continuation");
        assert_eq!(week(&own).unwrap().1[3], vec![heat(1020, 2100)]);
    }

    #[test]
    fn what_the_unit_cannot_run_is_refused() {
        let b = bucket();
        let refused = |days: &[(usize, Vec<Point>)], mode| with_days(Some(&b), mode, days, 0).is_err();
        assert!(refused(&[(0, (0..9).map(|i| heat(i * 60, 2000)).collect())], None), "nine on a day");
        assert!(refused(&[(0, vec![heat(60, 2000), heat(60, 2100)])], None), "the same time twice");
        assert!(refused(&[(0, vec![heat(1440, 2000)])], None), "24:00");
        assert!(refused(&[(0, vec![heat(60, 850)])], None), "below 9 °C");
        assert!(refused(&[(0, vec![Point { minutes: 60, heat: None, cool: Some(2400) }])], None), "no heat setpoint");
        assert!(refused(&[(0, vec![heat(60, 2000)])], Some(Mode::Cool)), "another mode for one day");
        let range = |low, high| Point { minutes: 60, heat: Some(low), cool: Some(high) };
        let every = |p: Point| (0..7).map(|d| (d, vec![p.clone()])).collect::<Vec<_>>();
        assert!(refused(&every(range(2000, 2100)), Some(Mode::Auto)), "inside the dead band");
        let f = with_days(Some(&b), Some(Mode::Auto), &every(range(2000, 2400)), 0).unwrap();
        assert_eq!(week(&f).unwrap().0, Mode::Auto, "every day: the mode changes");
    }

    #[test]
    fn mqtt_s_schedule_round_trips() {
        let v = to_json(&bucket()).unwrap();
        assert_eq!(v["mode"], json!("heat"));
        assert_eq!(
            v["days"]["monday"],
            json!([{"time": "00:00", "temperature": 18}, {"time": "09:00", "temperature": 19},
                                               {"time": "17:00", "temperature": 21}])
        );
        let back = from_json(&v.to_string(), Some(&bucket()), 0).unwrap();
        assert_eq!(week(&back), week(&bucket()));
        // One day, no mode: that day alone changes.
        let f =
            from_json(r#"{"days": {"tuesday": [{"time": "6:30", "temperature": 20.5}]}}"#, Some(&bucket()), 0).unwrap();
        assert_eq!(week(&f).unwrap().1[1], vec![heat(390, 2050)]);
        assert_eq!(week(&f).unwrap().1[0], week(&bucket()).unwrap().1[0]);
        for bad in [
            r#"{}"#,
            r#"{"days": {"funday": []}}"#,
            r#"{"days": {"monday": [{"time": "25:00", "temperature": 20}]}}"#,
            r#"{"days": {"monday": [{"temperature": 20}]}}"#,
            r#"{"mode": "dry", "days": {}}"#,
            "not json",
        ] {
            assert!(from_json(bad, Some(&bucket()), 0).is_err(), "{bad}");
        }
    }
}
