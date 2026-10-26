//! The weather nlclient's /weather gets: what Home Assistant publishes,
//! in the shape nlclient's nlWeatherParser reads (6.4-8; the same keys in
//! 5.9.4-5):
//!
//!   current.temp_c, current.humidity   required: without both the whole
//!                                      answer is dropped
//!   current.icon                       a condition name, see nest_icon
//!   current.sunrise, current.sunset    epoch s. The screen shows the day
//!                                      image when sunrise < now < sunset
//!   forecast.hourly[]                  hour (1-48), time (epoch s),
//!                                      temp_c, humidity
//!   forecast.daily[]                   up to 6: date (epoch s),
//!                                      temp_low_c, temp_high_c, humidity
//!
//! Everything but the first two is optional. The icon picks one of the
//! unit's Weather_*.png: ClearDay/Night, PartlyCloudyDay/Night, Snow,
//! Rain, Cloudy, or Default when there is none.

use std::sync::Mutex;

use chrono::{DateTime, Duration, FixedOffset};
use serde::Deserialize;
use serde_json::Value;

use crate::fields;
use crate::json::{num, Fields};

/// Home Assistant's latest readings (set/outdoor_temperature,
/// set/outdoor_humidity) and conditions and forecast (set/weather).
#[derive(Default)]
pub struct Outdoor {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    temp: Option<f64>,
    humidity: Option<f64>,
    current: Fields,  // icon, sunrise, sunset
    forecast: Fields, // hourly, daily
}

/// set/weather's payload: Home Assistant's own names, a weather entity's
/// condition, sun.sun's next_rising and next_setting, and the lists
/// weather.get_forecasts returns. daily may be a twice_daily list (NWS has
/// no daily): day and night halves, with the night's temperature as the
/// low.
#[derive(Deserialize, Default)]
pub struct HAWeather {
    #[serde(default)]
    pub condition: String,
    #[serde(default)]
    pub next_rising: Option<DateTime<FixedOffset>>,
    #[serde(default)]
    pub next_setting: Option<DateTime<FixedOffset>>,
    #[serde(default)]
    pub hourly: Option<Vec<HAForecast>>,
    #[serde(default)]
    pub daily: Option<Vec<HAForecast>>,
}

#[derive(Deserialize, Clone)]
pub struct HAForecast {
    #[serde(default)]
    pub datetime: Option<DateTime<FixedOffset>>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub templow: Option<f64>,
    #[serde(default)]
    pub humidity: Option<f64>,
    #[serde(default)]
    pub is_daytime: Option<bool>, // twice_daily only
}

/// Folds a twice_daily list into days: each daytime half, with the next
/// half's temperature as its low. A daily list passes as it is.
fn days(forecasts: &[HAForecast]) -> Vec<HAForecast> {
    let mut out = Vec::new();
    for (i, f) in forecasts.iter().enumerate() {
        match f.is_daytime {
            None => out.push(f.clone()),
            Some(true) => {
                let mut day = f.clone();
                if let Some(next) = forecasts.get(i + 1).filter(|n| n.is_daytime == Some(false)) {
                    day.templow = next.temperature;
                }
                out.push(day);
            }
            Some(false) => {}
        }
    }
    out
}

/// Home Assistant's conditions as the names the unit knows. windy and
/// exceptional have no match: the unit shows its default image.
fn nest_icon(condition: &str) -> Option<&'static str> {
    Some(match condition {
        "clear-night" => "clear",
        "sunny" => "sunny",
        "partlycloudy" => "partlycloudy",
        "cloudy" | "windy-variant" => "cloudy",
        "fog" => "fog",
        "rainy" | "pouring" => "rain",
        "lightning" | "lightning-rainy" => "tstorms",
        "snowy" => "snow",
        "snowy-rainy" | "hail" => "sleet",
        _ => return None,
    })
}

fn unix(t: Option<DateTime<FixedOffset>>) -> i64 {
    t.map_or(0, |t| t.timestamp())
}

fn put(m: &mut Fields, k: &str, v: Option<f64>) {
    if let Some(v) = v {
        m.insert(k.into(), num(v));
    }
}

impl Outdoor {
    pub fn set(&self, temp: Option<f64>, humidity: Option<f64>) {
        let mut s = self.state.lock().unwrap();
        if temp.is_some() {
            s.temp = temp;
        }
        if humidity.is_some() {
            s.humidity = humidity;
        }
    }

    /// Home Assistant's latest outdoor temperature, °C.
    pub fn temperature(&self) -> Option<f64> {
        self.state.lock().unwrap().temp
    }

    /// The /weather answer under the query key, None until both the
    /// temperature and the humidity are known.
    pub fn answer(&self) -> Option<Fields> {
        let s = self.state.lock().unwrap();
        let (temp, humidity) = (s.temp?, s.humidity?);
        let mut current = fields!{"temp_c" => num(temp), "humidity" => num(humidity.round())};
        current.extend(s.current.clone());
        let mut answer = fields!{"current" => Value::Object(current)};
        if !s.forecast.is_empty() {
            answer.insert("forecast".into(), Value::Object(s.forecast.clone()));
        }
        Some(answer)
    }

    pub fn set_weather(&self, w: &HAWeather) {
        let mut current = Fields::new();
        if let Some(icon) = nest_icon(&w.condition) {
            current.insert("icon".into(), icon.into());
        }
        if let (Some(rising), Some(setting)) = (w.next_rising, w.next_setting) {
            // By day the next sunrise is tomorrow's; the unit wants today's.
            let rise = if setting < rising { rising - Duration::hours(24) } else { rising };
            current.insert("sunrise".into(), rise.timestamp().into());
            current.insert("sunset".into(), setting.timestamp().into());
        }

        let hourly: Vec<Value> = w.hourly.iter().flatten().take(48).enumerate().map(|(i, f)| {
            let mut h = fields!{"hour" => i + 1, "time" => unix(f.datetime)};
            put(&mut h, "temp_c", f.temperature);
            put(&mut h, "humidity", f.humidity);
            Value::Object(h)
        }).collect();
        let daily: Vec<Value> = days(w.daily.as_deref().unwrap_or_default()).into_iter().take(6).map(|f| {
            let mut d = fields!{"date" => unix(f.datetime)};
            put(&mut d, "temp_high_c", f.temperature);
            put(&mut d, "temp_low_c", f.templow);
            put(&mut d, "humidity", f.humidity);
            Value::Object(d)
        }).collect();
        let mut forecast = Fields::new();
        if !hourly.is_empty() {
            forecast.insert("hourly".into(), Value::Array(hourly));
        }
        if !daily.is_empty() {
            forecast.insert("daily".into(), Value::Array(daily));
        }

        let mut s = self.state.lock().unwrap();
        s.current = current;
        s.forecast = forecast;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn forecast(datetime: &str, temperature: f64, is_daytime: Option<bool>) -> HAForecast {
        HAForecast { datetime: DateTime::parse_from_rfc3339(datetime).ok(), temperature: Some(temperature),
                     templow: None, humidity: None, is_daytime }
    }

    #[test]
    fn twice_daily_folds_into_days() {
        let lows = |list: &[HAForecast]| days(list).iter().map(|d| (d.temperature, d.templow)).collect::<Vec<_>>();
        // Starting at night: that half is no day, and is dropped.
        assert_eq!(lows(&[
            forecast("2026-09-29T18:00:00-04:00", 15.0, Some(false)),
            forecast("2026-09-30T06:00:00-04:00", 27.8, Some(true)),
            forecast("2026-09-30T18:00:00-04:00", 16.5, Some(false)),
            forecast("2026-10-01T06:00:00-04:00", 25.0, Some(true)),
        ]), vec![(Some(27.8), Some(16.5)), (Some(25.0), None)]);
        // Two days running: the first takes no low from the second.
        assert_eq!(lows(&[forecast("2026-09-30T06:00:00Z", 20.0, Some(true)), forecast("2026-10-01T06:00:00Z", 21.0, Some(true))]),
                   vec![(Some(20.0), None), (Some(21.0), None)]);
        // A daily list, templow and all, passes as it is.
        let mut daily = forecast("2026-09-30T00:00:00Z", 20.0, None);
        daily.templow = Some(9.0);
        assert_eq!(lows(&[daily]), vec![(Some(20.0), Some(9.0))]);
    }

    #[test]
    fn forecasts_are_cut_to_what_the_unit_reads() {
        let o = Outdoor::default();
        let hours: Vec<_> = (0..60).map(|h| forecast(&format!("2026-09-29T{:02}:00:00Z", h % 24), f64::from(h), None)).collect();
        let days_list: Vec<_> = (1..=9).map(|d| forecast(&format!("2026-10-0{d}T12:00:00Z"), f64::from(d), None)).collect();
        o.set_weather(&HAWeather { hourly: Some(hours), daily: Some(days_list), ..Default::default() });
        o.set(Some(10.0), Some(50.0));
        let a = Value::Object(o.answer().unwrap());
        let hourly = a["forecast"]["hourly"].as_array().unwrap();
        assert_eq!((hourly.len(), &hourly[0]["hour"], &hourly[47]["hour"]), (48, &json!(1), &json!(48)));
        let daily = a["forecast"]["daily"].as_array().unwrap();
        assert_eq!((daily.len(), &daily[5]["temp_high_c"]), (6, &json!(6)));
        // A sunrise without a sunset is left out; so is a condition the unit
        // has no image for.
        assert!(a["current"].get("sunrise").is_none() && a["current"].get("icon").is_none());

        // The next set/weather replaces the last: no forecast, no forecast key.
        o.set_weather(&HAWeather { condition: "pouring".into(),
            next_rising: DateTime::parse_from_rfc3339("2026-09-30T11:00:00Z").ok(), ..Default::default() });
        let a = o.answer().unwrap();
        assert!(a.get("forecast").is_none());
        assert_eq!(Value::Object(a), json!({"current": {"temp_c": 10, "humidity": 50, "icon": "rain"}}));
    }

    #[test]
    fn readings_are_kept_until_replaced() {
        let o = Outdoor::default();
        o.set(None, Some(49.5));
        assert!(o.answer().is_none() && o.temperature().is_none(), "an answer without a temperature");
        o.set(Some(-0.5), None);
        o.set(None, None); // nothing new: both stay
        assert_eq!(o.temperature(), Some(-0.5));
        assert_eq!(Value::Object(o.answer().unwrap()), json!({"current": {"temp_c": -0.5, "humidity": 50}}));
    }
}
