//! When nlclient last heard each sensor, and what: it logs every report of one
//! to its event log (UTC), newest reading first:
//!
//!   2026-11-24T20:02:10 KR1Data "kryptonite.18B43000AAAA1111" 22.000 22.000 21.900 21.900 21.900 100 3699416 -60

use std::collections::BTreeMap;
use std::sync::Mutex;

use super::SILENCE;

#[derive(Default)]
struct State {
    /// When the log was first read: a sensor not heard yet may be nlclient's
    /// choice from before.
    since: Option<i64>,
    /// When each bucket was heard, seconds since 1970, and its reading, °C.
    last: BTreeMap<String, (i64, f64)>,
    /// Those in time, as last told.
    shown: Vec<String>,
}

#[derive(Default)]
pub struct Heard(Mutex<State>);

fn report(line: &str) -> Option<(String, i64, f64)> {
    let mut words = line.split_whitespace();
    let at = chrono::NaiveDateTime::parse_from_str(words.next()?, "%Y-%m-%dT%H:%M:%S").ok()?;
    if words.next()? != "KR1Data" {
        return None;
    }
    let bucket = words.next()?.trim_matches('"').to_string();
    let celsius = words.next()?.parse::<f64>().ok().filter(|t| t.is_finite())?;
    bucket.starts_with("kryptonite.").then(|| (bucket, at.and_utc().timestamp(), celsius))
}

impl Heard {
    /// Takes an event log line.
    pub fn line(&self, line: &str) {
        if let Some((bucket, at, celsius)) = report(line) {
            let mut state = self.0.lock().unwrap();
            let last = state.last.entry(bucket).or_insert((at, celsius));
            if at >= last.0 {
                *last = (at, celsius);
            }
        }
    }

    /// Whether which sensors are heard, or what from, has changed, as of now.
    pub fn update(&self, now: i64) -> bool {
        let mut state = self.0.lock().unwrap();
        state.since.get_or_insert(now);
        let shown: Vec<String> = state
            .last
            .iter()
            .filter(|(_, (at, _))| now - at < SILENCE.as_secs() as i64)
            .map(|(bucket, (_, celsius))| format!("{bucket} {celsius}"))
            .collect();
        std::mem::replace(&mut state.shown, shown.clone()) != shown
    }

    /// Whether nlclient regulates from this sensor once it is chosen: it goes
    /// back to the backplate by itself when the sensor falls silent.
    pub fn hears(&self, bucket: &str) -> bool {
        self.hears_at(bucket, chrono::Utc::now().timestamp())
    }

    fn hears_at(&self, bucket: &str, now: i64) -> bool {
        let state = self.0.lock().unwrap();
        let at = state.last.get(bucket).map(|(at, _)| *at).max(state.since);
        at.is_none_or(|at| now - at < SILENCE.as_secs() as i64)
    }

    /// Its reading, °C, while it is heard.
    pub fn reading(&self, bucket: &str) -> Option<f64> {
        let now = chrono::Utc::now().timestamp();
        let state = self.0.lock().unwrap();
        let (at, celsius) = state.last.get(bucket)?;
        (now - at < SILENCE.as_secs() as i64).then_some(*celsius)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "kryptonite.18B43000AAAA1111";
    const AT: i64 = 1_795_550_530;

    fn line(celsius: &str) -> String {
        format!("2026-11-24T20:02:10 KR1Data \"{A}\" {celsius} 22.000 21.900 21.900 21.900 100 3699416 -60\n")
    }

    #[test]
    fn reports_are_read_from_kr1data_lines_only() {
        assert_eq!(report(&line("22.125")), Some((A.into(), AT, 22.125)));
        for other in [
            "2026-11-24T20:02:10 %KR1Data { \"fields\": [] }",
            "2026-11-24T20:02:10 RCSSample \"kryptonite.18B43000AAAA1111\" 2026-11-24T20:02:10 22.0 false",
            "2026-11-24T20:02:10 KR1Data \"kryptonite.18B43000AAAA1111\" warm",
            "2026-11-24T20:02:10 KR1Data \"other\" 22.0",
            "KR1Data",
            "",
        ] {
            assert_eq!(report(other), None, "{other}");
        }
    }

    #[test]
    fn a_sensor_is_heard_until_it_has_been_silent_as_long_as_nlclient_allows() {
        let heard = Heard::default();
        let silence = SILENCE.as_secs() as i64;
        // One not heard since nestlocal started may be, until then.
        assert!(heard.hears_at(A, AT));
        heard.update(AT - 100);
        assert!(heard.hears_at(A, AT) && !heard.hears_at(A, AT - 100 + silence));
        heard.line(&line("22.1"));
        assert!(heard.update(AT + 1) && !heard.update(AT + 2));
        assert!(heard.hears_at(A, AT + silence - 1) && !heard.hears_at(A, AT + silence));
        heard.line(&line("22.2"));
        assert!(heard.update(AT + 3), "another reading");
        assert!(heard.update(AT + silence), "silent");
        // An earlier report read late is not news.
        heard.line(&line("25.0").replace("20:02:10", "20:01:10"));
        assert_eq!(heard.0.lock().unwrap().last[A], (AT, 22.2));
    }
}
