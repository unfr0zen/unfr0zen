//! The unit's schedule bucket as transitions, for every protocol: the
//! Matter schedules (matter/schedule.rs), Zigbee's weekly schedule
//! commands and MQTT's schedule topic all read and write it through these.
//!
//! nlclient keeps a schedule for each system mode (heat, cool, range) and
//! runs the one for the mode the unit is in; the bucket holds that one. So
//! a write is for the schedule that runs: one in another mode, or one the
//! equipment can't run, is refused (Unit::check).

use chrono::TimeZone;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fields;
use crate::json::{num, sprint, truthy, Fields};

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

/// What a schedule write has to fit: the modes the equipment runs, the
/// mode the unit is in (None when off), and its scale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub modes: Vec<Mode>,
    pub running: Option<Mode>,
    pub fahrenheit: bool,
}

impl Unit {
    /// From the device and shared buckets.
    pub fn of(device: Option<&Fields>, shared: Option<&Fields>) -> Self {
        let flag = |key: &str| truthy(shared.and_then(|s| s.get(key)));
        let running = match shared.and_then(|s| s.get("target_temperature_type")).and_then(Value::as_str) {
            Some("heat") => Some(Mode::Heat),
            Some("cool") => Some(Mode::Cool),
            Some("range") => Some(Mode::Auto),
            _ => None,
        };
        Unit {
            modes: equipment(flag("can_heat"), flag("can_cool")),
            running,
            fahrenheit: sprint(device.and_then(|d| d.get("temperature_scale"))) == "F",
        }
    }

    /// The mode a schedule written now has: the unit's, or with the unit
    /// off, that of the schedule it holds (`current`).
    pub fn mode(&self, current: Mode) -> Mode {
        self.running.unwrap_or(current)
    }

    /// Whether a schedule in `mode` is the one that runs, `current` being
    /// the mode of the schedule the bucket holds.
    pub fn check(&self, mode: Mode, current: Mode) -> Result<(), String> {
        if !self.modes.contains(&mode) {
            return Err(format!("the equipment runs no {} schedule", mode_name(mode)));
        }
        let want = self.mode(current);
        if mode != want {
            return Err(match self.running {
                Some(_) => format!("the unit is in {0}: only its {0} schedule runs", mode_name(want)),
                None => format!("the unit is off: its schedule stays {}", mode_name(want)),
            });
        }
        Ok(())
    }
}

/// The schedule modes equipment runs, as Matter's control sequence has it:
/// all three until the unit has said.
pub fn equipment(can_heat: bool, can_cool: bool) -> Vec<Mode> {
    match (can_heat, can_cool) {
        (true, false) => vec![Mode::Heat],
        (false, true) => vec![Mode::Cool],
        _ => vec![Mode::Heat, Mode::Cool, Mode::Auto],
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

/// °C a whole °F is, exactly.
fn whole_fahrenheit(centi: i16) -> f64 {
    ((f64::from(centi) / 100.0 * 9.0 / 5.0 + 32.0).round() - 32.0) * 5.0 / 9.0
}

/// A setpoint as the unit stores it: to half degrees in Celsius, to whole
/// degrees in Fahrenheit, within 9–32 °C.
pub fn to_unit(centi_c: i16, fahrenheit: bool) -> i16 {
    if !fahrenheit {
        return ((f64::from(centi_c) / 50.0).round() * 50.0) as i16;
    }
    let mut c = centi(whole_fahrenheit(centi_c));
    // 9 and 32 °C are 48.2 and 89.6 °F: the whole degree inside.
    if c < LOWEST {
        c = centi(whole_fahrenheit(c + 56));
    } else if c > HIGHEST {
        c = centi(whole_fahrenheit(c - 56));
    }
    c
}

/// A setpoint as the bucket keeps it: a whole °F exactly, as the unit
/// writes it (69 °F is 20.5556 °C), on a unit in Fahrenheit.
fn stored(centi_c: i16, fahrenheit: bool) -> Value {
    let c = whole_fahrenheit(centi_c);
    if fahrenheit && centi(c) == centi_c {
        return num((c * 100_000.0).round() / 100_000.0);
    }
    celsius(centi_c)
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

/// A day's transitions as the unit runs them: its setpoints, and its
/// continuation at 00:00 when no setpoint starts the day.
fn day_transitions(entries: &serde_json::Map<String, Value>) -> Vec<Transition> {
    let mut out: Vec<Transition> = entries.values().filter_map(entry_transition).collect();
    if !out.iter().any(|t| t.minutes == 0) {
        let carried = entries.values().filter(|e| !is_setpoint(e)).filter_map(entry_values);
        out.extend(carried.filter(|t| t.minutes == 0).take(1));
    }
    out
}

/// The days (Matter's bits) whose 00:00 is a continuation alone.
fn carried_days(bucket: &Fields) -> u8 {
    let Some(days) = bucket.get("days").and_then(Value::as_object) else { return 0 };
    (0..7)
        .filter(|day| {
            let Some(entries) = days.get(&day.to_string()).and_then(Value::as_object) else { return false };
            !entries.values().filter_map(entry_transition).any(|t| t.minutes == 0)
                && entries.values().any(|e| !is_setpoint(e) && entry_values(e).is_some_and(|t| t.minutes == 0))
        })
        .fold(0, |mask, day| mask | day_bit(day))
}

/// The schedule bucket as one schedule's mode and transitions, a transition
/// for each time and setpoint with every day that has it. A day the unit
/// runs from a continuation alone, as its stock schedule does, has it as a
/// transition at 00:00.
pub fn from_bucket(bucket: &Fields) -> Option<(Mode, Vec<Transition>)> {
    let mode = bucket.get("schedule_mode").and_then(Value::as_str).and_then(Mode::from_nest).unwrap_or(Mode::Heat);
    let days = bucket.get("days")?.as_object()?;
    let mut transitions: Vec<Transition> = Vec::new();
    for day in 0..7 {
        let Some(entries) = days.get(&day.to_string()).and_then(Value::as_object) else { continue };
        for t in day_transitions(entries) {
            match transitions.iter_mut().find(|o| (o.minutes, o.heat, o.cool) == (t.minutes, t.heat, t.cool)) {
                Some(o) => o.days |= day_bit(day),
                None => transitions.push(Transition { days: day_bit(day), ..t }),
            }
        }
    }
    transitions.sort_by_key(|t| (t.minutes, t.heat, t.cool));
    Some((mode, transitions))
}

/// The unit's offset from UTC at `now`, seconds, as it stamps touched_tzo.
fn utc_offset(now: i64) -> i32 {
    chrono::Local.timestamp_opt(now, 0).single().map_or(0, |t| t.offset().local_minus_utc())
}

pub fn new_entry(mode: Mode, t: &Transition, now: i64, fahrenheit: bool) -> Value {
    let mut e = fields! {
        "entry_type" => "setpoint",
        "time" => num(f64::from(t.minutes) * 60.0),
        "touched_at" => num(now as f64),
        "touched_by" => num(TOUCHED_BY_LOCAL as f64),
        "touched_tzo" => num(f64::from(utc_offset(now))),
        "type" => mode.nest(),
    };
    let temp = |c: Option<i16>| stored(c.unwrap_or_default(), fahrenheit);
    match mode {
        Mode::Heat => {
            e.insert("temp".into(), temp(t.heat));
        }
        Mode::Cool => {
            e.insert("temp".into(), temp(t.cool));
        }
        Mode::Auto => {
            e.insert("temp-min".into(), temp(t.heat));
            e.insert("temp-max".into(), temp(t.cool));
        }
    }
    Value::Object(e)
}

/// The schedule bucket's fields for a schedule: every day's entries, the
/// ones it already had kept as they were (their touched_ marks with them).
/// A day that starts after midnight opens with a continuation, the last
/// setpoint before it, as the unit's own schedules do; so does one whose
/// 00:00 transition is that setpoint again.
pub fn to_bucket(
    mode: Mode,
    transitions: &[Transition],
    current: Option<&Fields>,
    now: i64,
    fahrenheit: bool,
) -> Fields {
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
        let continuation = |want: &Transition| {
            kept(false, want).unwrap_or_else(|| {
                let mut e = new_entry(mode, want, now, fahrenheit);
                e["entry_type"] = "continuation".into();
                e
            })
        };
        // The last setpoint before this day, a week back at most.
        let carried = (1..=7).find_map(|back| per_day[(day + 7 - back) % 7].last()).map(|last| Transition {
            days: 0,
            minutes: 0,
            ..(*last).clone()
        });
        let mut mine = per_day[day].iter().map(|t| Transition { days: 0, ..(*t).clone() }).peekable();
        let mut entries: Vec<Value> = Vec::new();
        match (mine.peek(), &carried) {
            // A setpoint the day already had at 00:00 stays one.
            (Some(first), Some(c)) if first == c && kept(true, first).is_none() => {
                entries.push(continuation(c));
                mine.next();
            }
            (Some(first), _) if first.minutes == 0 => {}
            (_, Some(c)) => entries.push(continuation(c)),
            (_, None) => {}
        }
        for want in mine {
            entries.push(kept(true, &want).unwrap_or_else(|| new_entry(mode, &want, now, fahrenheit)));
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
/// 6) given its points and every other day's setpoints as they were; a
/// continuation follows the setpoint before it. `mode`, when given, has to
/// be the one that runs (Unit::check). Setpoints go to what the unit
/// stores.
pub fn with_days(
    bucket: Option<&Fields>,
    unit: &Unit,
    mode: Option<Mode>,
    days: &[(usize, Vec<Point>)],
    now: i64,
) -> Result<Fields, String> {
    let (current, base) = bucket.and_then(from_bucket).unwrap_or((Mode::Heat, Vec::new()));
    let mode = mode.unwrap_or(unit.mode(current));
    unit.check(mode, current)?;
    let every_day = (0..7).all(|d| days.iter().any(|(day, _)| *day == d));
    if mode != current && !every_day {
        // The unit is in a mode whose schedule the bucket doesn't hold yet:
        // the other days would be another mode's.
        return Err(format!("the unit's schedule is still {}: give every day", mode_name(current)));
    }
    let mut given_transitions = Vec::new();
    for (day, points) in days {
        let name = DAYS.get(*day).ok_or("no such day")?;
        // A point the day already has keeps its exact setpoints: only new
        // ones go to what the unit stores.
        let had = |p: &Point| {
            mode == current
                && base
                    .iter()
                    .any(|t| t.days & day_bit(*day) != 0 && (t.minutes, t.heat, t.cool) == (p.minutes, p.heat, p.cool))
        };
        let setpoint = |p: &Point, t: Option<i16>, which: &str| match t {
            Some(t) if (LOWEST..=HIGHEST).contains(&t) => Ok(if had(p) { t } else { to_unit(t, unit.fahrenheit) }),
            Some(_) => Err(format!("a {which} setpoint is {}-{} °C", LOWEST / 100, HIGHEST / 100)),
            None => Err(format!("each transition needs a {which} setpoint")),
        };
        if points.len() > usize::from(PER_DAY) {
            return Err(format!("{name}: at most {PER_DAY} transitions"));
        }
        let mut times = std::collections::BTreeSet::new();
        for p in points {
            if p.minutes >= MINUTES_PER_DAY || !times.insert(p.minutes) {
                return Err(format!("{name}: times are before 24:00 and each once"));
            }
            let (heat, cool) = match mode {
                Mode::Heat => (Some(setpoint(p, p.heat, "heat")?), None),
                Mode::Cool => (None, Some(setpoint(p, p.cool, "cool")?)),
                Mode::Auto => {
                    let (low, high) = (setpoint(p, p.heat, "low")?, setpoint(p, p.cool, "high")?);
                    if high - low < DEAD_BAND {
                        return Err(format!("{name}: the high is at least {} °C above the low", DEAD_BAND / 100));
                    }
                    (Some(low), Some(high))
                }
            };
            given_transitions.push(Transition { days: day_bit(*day), minutes: p.minutes, heat, cool });
        }
    }
    let given = days.iter().fold(0u8, |mask, (day, _)| mask | day_bit(*day));
    let carried = bucket.map(carried_days).unwrap_or(0);
    // The other days as they were; their continuations follow the new days
    // (to_bucket), unless no setpoint is left to follow.
    let others = |strip: u8| -> Vec<Transition> {
        if mode != current {
            return Vec::new();
        }
        let mut ts = base.clone();
        for t in &mut ts {
            t.days &= !given;
            if t.minutes == 0 {
                t.days &= !strip;
            }
        }
        ts.retain(|t| t.days != 0);
        ts
    };
    let mut transitions = [others(carried), given_transitions.clone()].concat();
    if transitions.is_empty() {
        transitions = [others(0), given_transitions].concat();
    }
    Ok(to_bucket(mode, &transitions, bucket, now, unit.fahrenheit))
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
    Some(days_json(mode, &days.into_iter().enumerate().collect::<Vec<_>>()))
}

/// Days (the unit's, 0 to 6) and their points in to_json's shape, which
/// from_json reads.
pub fn days_json(mode: Mode, days: &[(usize, Vec<Point>)]) -> Value {
    let mut out = Fields::new();
    for (day, points) in days {
        let Some(name) = DAYS.get(*day) else { continue };
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
    Value::Object(fields! {"mode" => mode_name(mode), "days" => Value::Object(out)})
}

/// MQTT's set/schedule: the days given (in to_json's shape) replace those
/// days, the rest stay as they are; a mode is optional.
pub fn from_json(arg: &str, bucket: Option<&Fields>, unit: &Unit, now: i64) -> Result<Fields, String> {
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
    // Without a mode, a temperature is the running schedule's: heat or cool.
    let current = bucket.and_then(from_bucket).map_or(Mode::Heat, |(m, _)| m);
    let read_as = mode.unwrap_or(unit.mode(current));
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
            let (heat, cool) = match read_as {
                Mode::Cool => (None, degrees(t, "temperature")),
                Mode::Auto => (degrees(t, "low"), degrees(t, "high")),
                Mode::Heat => (degrees(t, "temperature"), None),
            };
            points.push(Point { minutes, heat, cool });
        }
        days.push((day, points));
    }
    with_days(bucket, unit, mode, &days, now)
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

    /// Heat and cool equipment, in `running`, in Celsius.
    fn unit(running: Option<Mode>) -> Unit {
        Unit { modes: vec![Mode::Heat, Mode::Cool, Mode::Auto], running, fahrenheit: false }
    }

    fn heating() -> Unit {
        unit(Some(Mode::Heat))
    }

    /// The unit's stock schedule: 20 °C all week from continuations alone.
    fn stock() -> Fields {
        let days: serde_json::Map<_, _> = (0..7)
            .map(|d| {
                let mut e = entry(0, 20.0);
                e["entry_type"] = json!("continuation");
                (d.to_string(), json!({"0": e}))
            })
            .collect();
        fields! {"days" => Value::Object(days), "schedule_mode" => "HEAT"}
    }

    #[test]
    fn the_week_is_each_day_s_points_in_time_order() {
        let (mode, days) = week(&bucket()).unwrap();
        assert_eq!(mode, Mode::Heat);
        assert_eq!(days[0], vec![heat(0, 1800), heat(540, 1900), heat(1020, 2100)]);
        assert_eq!(days[6], vec![heat(0, 1800), heat(1020, 2100)]);
    }

    #[test]
    fn a_continuation_alone_reads_as_the_day_s_00_00() {
        let (_, days) = week(&stock()).unwrap();
        assert!(days.iter().all(|d| *d == vec![heat(0, 2000)]), "{days:?}");
        // Beside a setpoint at 00:00 it is no transition of its own.
        let mut b = bucket();
        b["days"]["3"]["3"] = json!({"entry_type": "continuation", "temp": 21.0, "time": 0, "type": "HEAT"});
        assert_eq!(week(&b).unwrap().1[3], vec![heat(0, 1800), heat(1020, 2100)]);
    }

    #[test]
    fn what_was_read_writes_back_unchanged() {
        let made = with_days(Some(&bucket()), &heating(), None, &[(1, vec![heat(390, 2000)])], 9).unwrap();
        for b in [stock(), made] {
            let json = to_json(&b).unwrap();
            assert_eq!(from_json(&json.to_string(), Some(&b), &heating(), 99).unwrap(), b);
        }
    }

    #[test]
    fn days_given_are_replaced_and_the_rest_kept_with_their_marks() {
        let b = bucket();
        let f = with_days(Some(&b), &heating(), None, &[(1, vec![heat(390, 2010)])], 99).unwrap();
        let (_, days) = week(&f).unwrap();
        assert_eq!(days[1], vec![heat(0, 2100), heat(390, 2000)], "to the unit's half degree, after Monday's 21 °C");
        assert_eq!(days[0], week(&b).unwrap().1[0]);
        assert_eq!(f["days"]["0"]["1"]["touched_by"], json!(5), "an untouched day keeps its entries");
        assert_eq!(f["days"]["1"]["1"]["touched_at"], json!(99));
        assert_eq!(f["days"]["1"]["1"]["touched_tzo"], json!(utc_offset(99)));
        // An empty day runs on from the day before.
        let f = with_days(Some(&b), &heating(), None, &[(2, vec![])], 0).unwrap();
        assert_eq!(week(&f).unwrap().1[2], vec![heat(0, 2100)]);
        assert_eq!(f["days"]["2"]["0"]["entry_type"], json!("continuation"));
    }

    #[test]
    fn a_day_starting_after_midnight_opens_with_the_last_setpoint_before_it() {
        let b = bucket();
        let f = with_days(Some(&b), &heating(), None, &[(2, vec![heat(390, 2000)]), (4, vec![])], 99).unwrap();
        let first = |f: &Fields, day: &str| {
            let e = &f["days"][day]["0"];
            (e["entry_type"].as_str().unwrap().to_owned(), e["time"].as_f64().unwrap(), e["temp"].as_f64().unwrap())
        };
        assert_eq!(first(&f, "2"), ("continuation".to_owned(), 0.0, 21.0), "Tuesday's 21 °C carries on");
        assert_eq!(f["days"]["2"]["1"]["time"].as_f64(), Some(23400.0));
        assert_eq!(first(&f, "4"), ("continuation".to_owned(), 0.0, 21.0), "an empty day has it alone");
        assert_eq!(f["days"]["4"].as_object().unwrap().len(), 1);
        assert_eq!(first(&f, "5").0, "setpoint", "a day starting at midnight has none");
        // Another write keeps it as it was; one that changes the day before renews it.
        let g = with_days(Some(&f), &heating(), None, &[(0, vec![heat(0, 1800)])], 100).unwrap();
        assert_eq!(g["days"]["2"]["0"]["touched_at"].as_f64(), Some(99.0));
        let g = with_days(Some(&f), &heating(), None, &[(1, vec![heat(0, 1800), heat(1020, 2000)])], 100).unwrap();
        assert_eq!(first(&g, "2"), ("continuation".to_owned(), 0.0, 20.0));
        assert_eq!(g["days"]["2"]["0"]["touched_at"].as_f64(), Some(100.0));
        // A setpoint at 00:00 stays one, even when it is the day before's last.
        let mut own = b.clone();
        own["days"]["3"]["0"]["temp"] = json!(21.0);
        let g = with_days(Some(&own), &heating(), None, &[(0, vec![heat(0, 1800)])], 100).unwrap();
        assert_eq!(g["days"]["3"]["0"], own["days"]["3"]["0"]);
    }

    #[test]
    fn the_stock_schedule_s_continuations_follow_a_written_day() {
        let f = with_days(Some(&stock()), &heating(), None, &[(0, vec![heat(420, 2300)])], 99).unwrap();
        let (_, days) = week(&f).unwrap();
        assert_eq!(days[0], vec![heat(0, 2300), heat(420, 2300)], "the week's one setpoint carries round to Monday");
        assert!(days[1..].iter().all(|d| *d == vec![heat(0, 2300)]), "{days:?}");
        // With no setpoint left to follow, the others stay as they were.
        let f = with_days(Some(&stock()), &heating(), None, &[(0, vec![])], 99).unwrap();
        assert_eq!(f, stock());
    }

    #[test]
    fn only_the_schedule_that_runs_is_written() {
        let b = bucket();
        let every = |p: Point| (0..7).map(|d| (d, vec![p.clone()])).collect::<Vec<_>>();
        let range = |low, high| Point { minutes: 60, heat: Some(low), cool: Some(high) };
        let write = |unit: &Unit, mode, days: &[(usize, Vec<Point>)]| with_days(Some(&b), unit, mode, days, 0);
        // The unit heats: a cool or range schedule would never run.
        assert!(write(&heating(), Some(Mode::Auto), &every(range(2000, 2400))).is_err());
        assert!(write(&heating(), Some(Mode::Cool), &[(0, vec![heat(60, 2000)])]).is_err());
        // Off: the schedule it holds, in its own mode.
        assert!(write(&unit(None), None, &[(0, vec![heat(60, 2000)])]).is_ok());
        assert!(
            write(&unit(None), Some(Mode::Cool), &every(Point { minutes: 60, heat: None, cool: Some(2400) })).is_err()
        );
        // Equipment that can't cool.
        let heat_only = Unit { modes: vec![Mode::Heat], running: Some(Mode::Cool), fahrenheit: false };
        assert!(write(&heat_only, None, &every(Point { minutes: 60, heat: None, cool: Some(2400) })).is_err());
        // In range with a heat schedule still in the bucket: every day, or nothing.
        let ranging = unit(Some(Mode::Auto));
        assert!(write(&ranging, None, &[(0, vec![range(2000, 2400)])]).is_err());
        let f = write(&ranging, None, &every(range(2000, 2400))).unwrap();
        assert_eq!(week(&f).unwrap().0, Mode::Auto);
        let carried = Point { minutes: 0, ..range(2000, 2400) };
        assert!(week(&f).unwrap().1.iter().all(|d| *d == vec![carried.clone(), range(2000, 2400)]));
    }

    #[test]
    fn what_the_unit_cannot_run_is_refused() {
        let b = bucket();
        let refused = |days: &[(usize, Vec<Point>)]| with_days(Some(&b), &heating(), None, days, 0).is_err();
        assert!(refused(&[(0, (0..9).map(|i| heat(i * 60, 2000)).collect())]), "nine on a day");
        assert!(refused(&[(0, vec![heat(60, 2000), heat(60, 2100)])]), "the same time twice");
        assert!(refused(&[(0, vec![heat(1440, 2000)])]), "24:00");
        assert!(refused(&[(0, vec![heat(60, 890)])]), "below 9 °C, before rounding");
        assert!(refused(&[(0, vec![heat(60, 3224)])]), "above 32 °C, before rounding");
        assert!(refused(&[(0, vec![Point { minutes: 60, heat: None, cool: Some(2400) }])]), "no heat setpoint");
        let range = |low, high| Point { minutes: 60, heat: Some(low), cool: Some(high) };
        let every = |p: Point| (0..7).map(|d| (d, vec![p.clone()])).collect::<Vec<_>>();
        let ranging = unit(Some(Mode::Auto));
        assert!(with_days(Some(&b), &ranging, None, &every(range(2000, 2100)), 0).is_err(), "inside the dead band");
    }

    #[test]
    fn fahrenheit_units_get_whole_degrees() {
        assert_eq!(to_unit(2050, true), 2056, "69 °F");
        assert_eq!(to_unit(900, true), 944, "48 °F is below 9 °C: 49 °F");
        assert_eq!(to_unit(3200, true), 3167, "90 °F is above 32 °C: 89 °F");
        assert_eq!(to_unit(2010, false), 2000);
        let f_unit = Unit { fahrenheit: true, ..heating() };
        let f = with_days(Some(&bucket()), &f_unit, None, &[(1, vec![heat(390, 2050)])], 0).unwrap();
        assert_eq!(f["days"]["1"]["1"]["temp"], json!(20.55556), "as the unit stores 69 °F");
        assert_eq!(week(&f).unwrap().1[1][1], heat(390, 2056));
        // Written back as read, it is kept as it was.
        let g = with_days(Some(&f), &f_unit, None, &[(1, week(&f).unwrap().1[1].clone())], 5).unwrap();
        assert_eq!(g, f);
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
        let back = from_json(&v.to_string(), Some(&bucket()), &heating(), 0).unwrap();
        assert_eq!(week(&back), week(&bucket()));
        // One day, no mode: that day alone changes.
        let f = from_json(
            r#"{"days": {"tuesday": [{"time": "6:30", "temperature": 20.5}]}}"#,
            Some(&bucket()),
            &heating(),
            0,
        )
        .unwrap();
        assert_eq!(week(&f).unwrap().1[1], vec![heat(0, 2100), heat(390, 2050)]);
        assert_eq!(week(&f).unwrap().1[0], week(&bucket()).unwrap().1[0]);
        for bad in [
            r#"{}"#,
            r#"{"days": {"funday": []}}"#,
            r#"{"days": {"monday": [{"time": "25:00", "temperature": 20}]}}"#,
            r#"{"days": {"monday": [{"temperature": 20}]}}"#,
            r#"{"mode": "dry", "days": {}}"#,
            r#"{"mode": "cool", "days": {"monday": [{"time": "07:00", "temperature": 24}]}}"#,
            "not json",
        ] {
            assert!(from_json(bad, Some(&bucket()), &heating(), 0).is_err(), "{bad}");
        }
    }
}
