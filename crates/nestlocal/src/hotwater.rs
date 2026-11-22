//! The unit's hot water schedule, as MQTT and Zigbee read and write it: by
//! day, the periods hot water is on.
//!
//! nlclient keeps it as a custom schedule, in a bucket of its own
//! (custom_schedule.<name>, cloud.rs) with "mode": "hotwater", whose
//! "intervals" are the on periods: each starts and ends on a day (0 Monday
//! to 6 Sunday) at seconds after midnight, a period that runs past midnight
//! ending on the next day, and repeats on the days its "repeat" lists
//! (start_day the first of them): nlclient folds the same period on several
//! days into one interval. A write replaces "intervals" whole: the days it
//! names get their new periods, by the day they start on, and the others
//! keep theirs, an interval shared with them keeping its other days.

use serde_json::Value;

use crate::fields;
use crate::json::{num, number, sprint, Fields};
use crate::schedule::{minutes, DAYS};

/// The most periods a day takes: what a day's Zigbee attribute holds in
/// one frame (zcl::MAX_FRAME), 11 characters each and a comma between.
pub const DAILY: usize = 6;

/// Marks the periods a command writes as the owner's, as nlclient marks
/// the Nest app's.
const TOUCHED_BY_LOCAL: i64 = 4;

const DAY: u32 = 24 * 3600;

/// A period: the minutes after midnight hot water goes on, then off; off
/// before on runs past midnight.
pub type Period = (u16, u16);

/// Whether a custom schedule bucket is the hot water one.
pub fn is_hot_water(bucket: &Fields) -> bool {
    sprint(bucket.get("mode")).eq_ignore_ascii_case("hotwater")
}

/// A day's periods (0 Monday), in time order.
pub fn day(bucket: &Fields, day: usize) -> Vec<Period> {
    let intervals = bucket.get("intervals").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let mut out: Vec<Period> = intervals
        .iter()
        .filter(|i| days(i).contains(&day))
        .filter_map(|i| {
            let minutes = |k| number(i.get(k)).map(|s| ((s as u32 % DAY) / 60) as u16);
            Some((minutes("start_time")?, minutes("end_time")?))
        })
        .collect();
    out.sort();
    out
}

/// The days an interval repeats on (0 Monday): its "repeat", or its
/// start_day without one.
fn days(interval: &Value) -> Vec<usize> {
    let listed: Vec<usize> = interval
        .get("repeat")
        .and_then(Value::as_array)
        .map_or(Vec::new(), |r| r.iter().filter_map(|d| number(Some(d))).map(|d| d as usize % 7).collect());
    if listed.is_empty() {
        number(interval.get("start_day")).map_or(Vec::new(), |d| vec![d as usize % 7])
    } else {
        listed
    }
}

fn hhmm(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// The schedule as <base>/hot_water_times has it: {"days": {"mon": [{"on":
/// "06:00", "off": "08:00"}, ...], ...}}, every day named.
pub fn shown(bucket: &Fields) -> Value {
    let days: Fields = DAYS
        .iter()
        .enumerate()
        .map(|(d, name)| {
            let list =
                day(bucket, d).into_iter().map(|(on, off)| serde_json::json!({"on": hhmm(on), "off": hhmm(off)}));
            ((*name).to_string(), Value::Array(list.collect()))
        })
        .collect();
    serde_json::json!({"days": days})
}

/// A day's periods as its Zigbee attribute has them: "06:00-08:00,17:00-20:00".
pub fn text(periods: &[Period]) -> String {
    periods.iter().map(|&(on, off)| format!("{}-{}", hhmm(on), hhmm(off))).collect::<Vec<_>>().join(",")
}

/// A Zigbee attribute's periods, as `text` writes them; None if malformed.
pub fn from_text(s: &str) -> Option<Vec<Period>> {
    if s.is_empty() {
        return Some(Vec::new());
    }
    s.split(',').map(|p| p.split_once('-').and_then(|(on, off)| Some((minutes(on)?, minutes(off)?)))).collect()
}

/// The hot_water_times command for these days' periods (0 Monday).
pub fn command(days: &[(usize, Vec<Period>)]) -> String {
    let named: Fields = days
        .iter()
        .map(|(d, periods)| {
            let list = periods.iter().map(|&(on, off)| serde_json::json!({"on": hhmm(on), "off": hhmm(off)}));
            (DAYS[*d].to_string(), Value::Array(list.collect()))
        })
        .collect();
    Value::Object(named).to_string()
}

/// The days a hot_water_times command names, with their periods in time
/// order: {"mon": [{"on": "06:00", "off": "08:00"}, ...], ...}.
fn parse(arg: &str) -> Result<Vec<(usize, Vec<Period>)>, String> {
    let Ok(Value::Object(named)) = serde_json::from_str::<Value>(arg) else {
        return Err(r#"expected {"mon": [{"on": "06:00", "off": "08:00"}], ...}"#.into());
    };
    let mut out = Vec::new();
    for (name, list) in &named {
        let d = DAYS.iter().position(|d| d == name).ok_or(format!("unknown day {name}"))?;
        let list = list.as_array().ok_or(format!("{name}: not a list"))?;
        if list.len() > DAILY {
            return Err(format!("{name}: more than {DAILY} periods"));
        }
        let mut periods = Vec::new();
        for p in list {
            let time = |k| p.get(k).and_then(Value::as_str).and_then(minutes);
            let (Some(on), Some(off)) = (time("on"), time("off")) else {
                return Err(format!("{name}: each period needs an on and an off, HH:MM"));
            };
            if on == off {
                return Err(format!("{name}: a period from {} to itself", hhmm(on)));
            }
            periods.push((on, off));
        }
        periods.sort();
        // In time order, each must end before the next starts; one running
        // past midnight can only be the last.
        let overlaps = periods.windows(2).any(|w| w[0].1 < w[0].0 || w[0].1 > w[1].0);
        if overlaps {
            return Err(format!("{name}: periods overlap"));
        }
        out.push((d, periods));
    }
    Ok(out)
}

/// The hot water bucket's fields a hot_water_times command writes: every
/// interval, the days it names replaced.
pub fn write(bucket: &Fields, arg: &str, now: i64) -> Result<Fields, String> {
    let changes = parse(arg)?;
    let intervals = bucket.get("intervals").and_then(Value::as_array).cloned().unwrap_or_default();
    let named: Vec<usize> = changes.iter().map(|(d, _)| *d).collect();
    // Each interval without the days named: as it was if it has none of
    // them, gone if it has only those, else on its other days.
    let mut out: Vec<Value> = intervals
        .into_iter()
        .filter_map(|i| {
            let all = days(&i);
            let rest: Vec<usize> = all.iter().copied().filter(|d| !named.contains(d)).collect();
            match rest.first() {
                None => None,
                Some(_) if rest.len() == all.len() => Some(i),
                Some(&first) => {
                    let n = |k| number(i.get(k)).unwrap_or(0.0) as usize;
                    let span = (n("end_day") + 7 - n("start_day")) % 7;
                    let mut kept = i.as_object().cloned().unwrap_or_default();
                    kept.insert("start_day".into(), num(first as f64));
                    kept.insert("end_day".into(), num(((first + span) % 7) as f64));
                    kept.insert("repeat".into(), Value::Array(rest.iter().map(|&d| num(d as f64)).collect()));
                    Some(Value::Object(kept))
                }
            }
        })
        .collect();
    for (d, periods) in &changes {
        for &(on, off) in periods {
            let end = if off < on { (d + 1) % 7 } else { *d };
            out.push(Value::Object(fields! {
                "start_day" => num(*d as f64),
                "start_time" => u32::from(on) * 60,
                "end_day" => num(end as f64),
                "end_time" => u32::from(off) * 60,
                "repeat" => vec![num(*d as f64)],
                "touched_at" => now,
                "touched_by" => TOUCHED_BY_LOCAL,
                "touched_tzo" => 0,
            }));
        }
    }
    let at = |i: &Value| {
        (number(i.get("start_day")).unwrap_or(0.0) as i64, number(i.get("start_time")).unwrap_or(0.0) as i64)
    };
    out.sort_by_key(at);
    // No two periods may overlap, across midnight and the week's end too,
    // on every day each repeats on.
    let week = 7 * i64::from(DAY);
    let mut spans: Vec<(i64, i64)> = out
        .iter()
        .flat_map(|i| {
            let n = |k| number(i.get(k)).unwrap_or(0.0) as i64;
            let (on, off) = (n("start_time"), n("end_time"));
            let span = (n("end_day") + 7 - n("start_day")) % 7;
            days(i).into_iter().map(move |d| {
                let start = d as i64 * i64::from(DAY) + on;
                let end = (d as i64 + span) * i64::from(DAY) + off;
                (start, if end <= start { end + i64::from(DAY) } else { end })
            })
        })
        .collect();
    spans.sort();
    let wraps = spans.len() > 1 && spans.last().zip(spans.first()).is_some_and(|(last, first)| last.1 > first.0 + week);
    if spans.windows(2).any(|w| w[0].1 > w[1].0) || wraps {
        return Err("periods overlap".into());
    }
    Ok(fields! {"intervals" => Value::Array(out)})
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

    fn interval(day: i64, on: i64, off: i64) -> Value {
        json!({"start_day": day, "start_time": on, "end_day": day, "end_time": off, "repeat": [day],
               "touched_at": 1694618279, "touched_by": 1, "touched_tzo": -25200})
    }

    /// Monday 06:00-08:00 and 17:00-20:00, Friday 15:30-16:30.
    fn bucket() -> Fields {
        obj(json!({"mode": "hotwater", "name": "HOTWATER", "is_built_in": true, "version": "1",
                   "intervals": [interval(0, 61200, 72000), interval(0, 21600, 28800), interval(4, 55800, 59400)]}))
    }

    #[test]
    fn shows_each_days_periods_in_time_order() {
        let s = shown(&bucket());
        assert_eq!(s["days"]["mon"], json!([{"on": "06:00", "off": "08:00"}, {"on": "17:00", "off": "20:00"}]));
        assert_eq!(s["days"]["fri"], json!([{"on": "15:30", "off": "16:30"}]));
        assert_eq!(s["days"]["sun"], json!([]));
        assert!(is_hot_water(&bucket()) && !is_hot_water(&Fields::new()));
    }

    #[test]
    fn writes_the_named_days_and_keeps_the_rest() {
        let f = write(
            &bucket(),
            r#"{"fri": [{"on": "18:00", "off": "19:00"}], "sat": [{"on": "23:00", "off": "01:00"}]}"#,
            9,
        )
        .unwrap();
        let b = obj(Value::Object(f.clone()));
        assert_eq!(day(&b, 0), [(360, 480), (1020, 1200)], "Monday as it was");
        assert_eq!(day(&b, 4), [(1080, 1140)]);
        assert_eq!(day(&b, 5), [(1380, 60)], "past midnight");
        let sat = f["intervals"].as_array().unwrap().iter().find(|i| i["start_day"] == json!(5)).unwrap();
        assert_eq!(
            sat,
            &json!({"start_day": 5, "start_time": 82800, "end_day": 6, "end_time": 3600, "repeat": [5],
                    "touched_at": 9, "touched_by": 4, "touched_tzo": 0})
        );
        assert_eq!(f["intervals"][0], canonical(interval(0, 21600, 28800)), "untouched intervals stay as they were");
        let empty = write(&bucket(), r#"{"mon": []}"#, 9).unwrap();
        assert_eq!(day(&obj(Value::Object(empty)), 0), []);
    }

    #[test]
    fn an_interval_repeats_on_the_days_nlclient_folds_into_it() {
        // As nlclient keeps the same periods every day: two intervals.
        let folded = |on: i64, off: i64| {
            json!({"start_day": 0, "start_time": on, "end_day": 0, "end_time": off, "repeat": [0, 1, 2, 3, 4, 5, 6],
                   "touched_at": 1, "touched_by": 1, "touched_tzo": 0})
        };
        let b = obj(json!({"mode": "hotwater", "intervals": [folded(14400, 21600), folded(57600, 64800)]}));
        assert!((0..7).all(|d| day(&b, d) == [(240, 360), (960, 1080)]), "every day");
        // Writing Tuesday leaves the folded intervals on the other days.
        let f = obj(Value::Object(write(&b, r#"{"tue": [{"on": "07:00", "off": "08:00"}]}"#, 5).unwrap()));
        assert_eq!(day(&f, 1), [(420, 480)]);
        assert!([0, 2, 3, 4, 5, 6].iter().all(|&d| day(&f, d) == [(240, 360), (960, 1080)]));
        let kept = &f["intervals"].as_array().unwrap()[0];
        assert_eq!(
            (&kept["repeat"], &kept["touched_by"]),
            (&json!([0, 2, 3, 4, 5, 6]), &json!(1)),
            "its metadata kept"
        );
        // Past midnight on a folded interval, into each next day's 04:00.
        let late = obj(json!({"mode": "hotwater", "intervals": [
            {"start_day": 0, "start_time": 82800, "end_day": 1, "end_time": 3600, "repeat": [0, 2]}]}));
        assert_eq!((day(&late, 0), day(&late, 2), day(&late, 1)), (vec![(1380, 60)], vec![(1380, 60)], vec![]));
        assert!(write(&late, r#"{"tue": [{"on": "00:30", "off": "02:00"}]}"#, 1).is_err(), "Monday's runs into it");
        assert!(write(&late, r#"{"wed": [{"on": "00:30", "off": "02:00"}]}"#, 1).is_ok(), "Wednesday's own goes");
    }

    #[test]
    fn zigbee_text_round_trips() {
        assert_eq!(text(&[(360, 480), (1380, 60)]), "06:00-08:00,23:00-01:00");
        assert_eq!(from_text("06:00-08:00,23:00-01:00"), Some(vec![(360, 480), (1380, 60)]));
        assert_eq!(from_text(""), Some(vec![]));
        assert_eq!(from_text("6:00-8:00"), None);
        let f = write(&bucket(), &command(&[(2, vec![(420, 450)])]), 1).unwrap();
        assert_eq!(day(&obj(Value::Object(f)), 2), [(420, 450)]);
    }

    #[test]
    fn refuses_what_the_unit_cannot_take() {
        let seven: Vec<Value> =
            (0..7).map(|h| json!({"on": format!("{h:02}:00"), "off": format!("{h:02}:30")})).collect();
        let too_many = json!({"mon": seven}).to_string();
        for arg in [
            "[]",
            r#"{"funday": []}"#,
            r#"{"mon": [{"on": "06:00"}]}"#,
            r#"{"mon": [{"on": "06:00", "off": "06:00"}]}"#,
            r#"{"mon": [{"on": "06:00", "off": "09:00"}, {"on": "08:00", "off": "10:00"}]}"#,
            r#"{"mon": [{"on": "23:00", "off": "01:00"}, {"on": "23:30", "off": "23:45"}]}"#,
            &too_many,
            // Into the next day's period: Monday 06:00-08:00 is there.
            r#"{"sun": [{"on": "23:00", "off": "06:30"}]}"#,
            r#"{"mon": [{"on": "23:00", "off": "02:00"}], "tue": [{"on": "01:00", "off": "03:00"}]}"#,
        ] {
            assert!(write(&bucket(), arg, 1).is_err(), "{arg} accepted");
        }
        assert!(write(&bucket(), r#"{"sun": [{"on": "23:00", "off": "06:00"}]}"#, 1).is_ok(), "up to Monday's 06:00");
    }
}
