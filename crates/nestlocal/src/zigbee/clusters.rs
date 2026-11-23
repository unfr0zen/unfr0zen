//! The unit as a Zigbee node, in standard clusters, read from its buckets
//! and written through the same commands as MQTT's set/ topics
//! (Bridge::apply), as the Matter side does. Its equipment decides which
//! endpoints it has; each has a fixed number, so a converter can name it:
//!
//!   1   the thermostat: Basic, Identify, Power Configuration (the battery),
//!       Thermostat (eco is the economy bit of its programming operation
//!       mode; the eco temperatures are the unoccupied setpoints), Fan
//!       Control (with a fan), Thermostat User Interface Configuration
//!       (°C or °F), Relative Humidity, and the one manufacturer-specific
//!       cluster, Weather (see WEATHER): what the unit used to get from
//!       Google, written by the coordinator. Its Time client asks the
//!       coordinator for the time (mod.rs).
//!   2   the backplate's temperature: Temperature Measurement
//!   3   the fan timer's duration, minutes (with a fan): Analog Output
//!   4   the target humidity, % (with a humidifier or dehumidifier): Analog
//!       Output
//!   5   the time to temperature, seconds left: Analog Input
//!   6   the filter's runtime, hours (with an air filter): Analog Input;
//!       writing 0 says the filter was changed
//!   7   the fan schedule's run time, minutes per hour (with a fan): Analog
//!       Output
//!   8   the fan schedule's start, hour of the day (with a fan): Analog Output
//!   9   the fan schedule's end, hour of the day (with a fan): Analog Output
//!   10+ the unit's switches: Binary Output, see SWITCHES; a coordinator
//!       names them by its description, which On/Off has none of
//!   20+ what the unit shows or does: Binary Input, see STATES
//!   28  whether the stand-in sensor regulates, with that option: Binary Input
//!   29  whether the fan follows its schedule (with a fan): Binary Output
//!   30+ the HVAC stages that run, those the wiring has: Binary Input, in
//!       bridge::HEAT_STAGES then COOL_STAGES order (30 heat stage 1 to 39
//!       cool stage 3); Thermostat's running state also has the second stages
//!   40  how long the screen stays lit after the last interaction, seconds:
//!       Analog Output (screen.rs)
//!   41  whether the screen stays on: Binary Output, in SWITCHES
//!   42  the screen's brightness, %: Analog Output (brightness.rs)
//!   43  whether the screen's brightness is automatic: Binary Output, in
//!       SWITCHES
//!
//! The Thermostat cluster's weekly schedule is the unit's (schedule.rs): Set
//! Weekly Schedule replaces the days it names, Get Weekly Schedule answers
//! with a response for each group of identical days. A range schedule carries
//! both setpoints. Clear Weekly Schedule empties every day.
//!
//! ZCL temperatures are hundredths of a degree Celsius; 0x8000 is none.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};

use chrono::{DateTime, FixedOffset};
use serde_json::Value as Json;

use crate::bridge::{self, Bridge};
use crate::json::{number, sprint, truthy, Fields};
use crate::weather::{HAForecast, HAWeather};

use super::zcl::{status, Answer, Clusters, Value};
use crate::hotwater;
use crate::schedule::{self, Kind};

pub const BASIC: u16 = 0x0000;
pub const POWER: u16 = 0x0001;
pub const IDENTIFY: u16 = 0x0003;
pub const ANALOG_INPUT: u16 = 0x000C;
pub const ANALOG_OUTPUT: u16 = 0x000D;
pub const BINARY_INPUT: u16 = 0x000F;
pub const BINARY_OUTPUT: u16 = 0x0010;
pub const THERMOSTAT: u16 = 0x0201;
pub const FAN: u16 = 0x0202;
pub const DISPLAY: u16 = 0x0204;
pub const TEMPERATURE: u16 = 0x0402;
pub const HUMIDITY: u16 = 0x0405;
pub const TIME: u16 = 0x000A;
/// Weather, manufacturer-specific (zcl::MANUFACTURER_CODE in its frames):
/// MQTT's set/outdoor_temperature, set/outdoor_humidity and set/weather as
/// attributes, written by the coordinator and kept until written again.
///
///   0x0000  outdoor temperature  int16, 0.01 °C (0x8000: none)
///   0x0001  outdoor humidity     uint8, % (0xFF: none)
///   0x0002  condition            string, Home Assistant's condition name
///   0x0003  next sunrise         UTCTime, as sun.sun's next_rising
///   0x0004  next sunset          UTCTime, as sun.sun's next_setting
///   0x0005  external temperature int16, 0.01 °C: the stand-in sensor's
///           reading (sensor/), as MQTT's set/external_temperature
///   0x1hhf  hourly forecast, hour hh (0-23): f = 0 its time (UTCTime; none
///           leaves the hour out), 1 temperature (int16), 2 humidity (uint8)
///   0x2ddf  daily forecast, day dd (0-5): as hourly, and f = 3 the low
///           (int16); 1 is the high
pub const WEATHER: u16 = 0xFC00;
/// Hot water schedule, manufacturer-specific (zcl::MANUFACTURER_CODE in its
/// frames), on a unit with hot water control: the on periods by day,
/// attribute 0x0000 (Monday) to 0x0006 (Sunday), each a string of them,
/// "06:00-08:00,17:00-20:00" ("" none; one past midnight ends the next
/// day). Writing a day replaces its periods, as MQTT's set/hot_water_times.
pub const HOT_WATER: u16 = 0xFC01;
const HOT_WATER_ATTRIBUTES: &[u16] = &[0x0000, 0x0001, 0x0002, 0x0003, 0x0004, 0x0005, 0x0006];
const OUTDOOR_TEMPERATURE: u16 = 0x0000;
const OUTDOOR_HUMIDITY: u16 = 0x0001;
const CONDITION: u16 = 0x0002;
const NEXT_RISING: u16 = 0x0003;
const NEXT_SETTING: u16 = 0x0004;
const EXTERNAL_TEMPERATURE: u16 = 0x0005;
const HOURLY: u16 = 0x1000;
const DAILY: u16 = 0x2000;
const HOURLY_SLOTS: u16 = 24;
const DAILY_SLOTS: u16 = 6;
/// A forecast slot's attributes, in its low nibble.
const SLOT_TIME: u16 = 0;
const SLOT_TEMPERATURE: u16 = 1;
const SLOT_HUMIDITY: u16 = 2;
const SLOT_LOW: u16 = 3;
/// The weather cluster's attributes, ascending.
static WEATHER_ATTRIBUTES: LazyLock<Vec<u16>> = LazyLock::new(|| {
    let mut ids =
        vec![OUTDOOR_TEMPERATURE, OUTDOOR_HUMIDITY, CONDITION, NEXT_RISING, NEXT_SETTING, EXTERNAL_TEMPERATURE];
    ids.extend((0..HOURLY_SLOTS).flat_map(|h| (SLOT_TIME..=SLOT_HUMIDITY).map(move |f| HOURLY | h << 4 | f)));
    ids.extend((0..DAILY_SLOTS).flat_map(|d| (SLOT_TIME..=SLOT_LOW).map(move |f| DAILY | d << 4 | f)));
    ids
});
/// UTCTime's zero, 2000-01-01 UTC, in seconds since 1970.
const ZIGBEE_EPOCH: i64 = 946_684_800;
const NO_TIME: u32 = 0xFFFF_FFFF;
const NO_HUMIDITY: u8 = 0xFF;

pub const THERMOSTAT_ENDPOINT: u8 = 1;
const BACKPLATE_ENDPOINT: u8 = 2;
const FAN_TIMER_ENDPOINT: u8 = 3;
const HUMIDITY_TARGET_ENDPOINT: u8 = 4;
const TIME_TO_TARGET_ENDPOINT: u8 = 5;
const FILTER_RUNTIME_ENDPOINT: u8 = 6;
const FAN_SCHEDULE_MINUTES_ENDPOINT: u8 = 7;
const FAN_SCHEDULE_START_ENDPOINT: u8 = 8;
const FAN_SCHEDULE_END_ENDPOINT: u8 = 9;
const TEMPERATURE_SOURCE_ENDPOINT: u8 = 28;
/// Past the stages' endpoints (STAGE_ENDPOINTS, ten of them).
const SCREEN_TIMEOUT_ENDPOINT: u8 = 40;
const SCREEN_BRIGHTNESS_ENDPOINT: u8 = 42;
const STAGE_ENDPOINTS: u8 = 30;

/// The switches, by endpoint: their bucket field and description.
const SWITCHES: &[(u8, &str, &str)] = &[
    (10, "learning_mode", "Auto-Schedule"),
    (11, "auto_away_enable", "Home/Away Assist"),
    (12, "sunlight_correction_enabled", "Sunblock"),
    (13, "preconditioning_enabled", "Early-On"),
    (14, "away_temperature_low_enabled", "Eco heat limit"), // to heat
    (15, "away_temperature_high_enabled", "Eco cool limit"), // to cool
    (16, "target_humidity_enabled", "Humidity control"),
    (17, "hot_water_boost", "Hot water boost"),
    (18, "hot_water_schedule", "Hot water schedule"),
    (19, "filter_reminder_enabled", "Filter reminder"), // with an air filter
    (29, "fan_schedule", "Fan schedule"),               // with a fan
    (41, "screen_always_on", "Screen always on"),
    (43, "screen_brightness_auto", "Screen brightness auto"),
];

/// The states, by endpoint: their bucket field, description and, past the
/// first four, the equipment they come with.
const STATES: &[(u8, &str, &str, &str)] = &[
    (20, "leaf", "Leaf", ""),
    (21, "preconditioning_active", "Early-On active", ""),
    (22, "sunlight_correction_active", "Sunblock active", ""),
    (23, "hvac_safety_shutoff_active", "Safety shutoff", ""),
    (24, "filter_replacement_needed", "Filter replacement", "has_air_filter"),
    (25, "hot_water_active", "Hot water", "has_hot_water_control"),
    (26, "humidifier_state", "Humidifier", "has_humidifier"),
    (27, "dehumidifier_state", "Dehumidifier", "has_dehumidifier"),
];

/// The HVAC stages, by endpoint from STAGE_ENDPOINTS.
fn stages() -> impl Iterator<Item = &'static (&'static str, &'static str, &'static str, &'static str)> {
    bridge::HEAT_STAGES.iter().chain(bridge::COOL_STAGES)
}

// Home Automation device types.
const THERMOSTAT_DEVICE: u16 = 0x0301;
const TEMPERATURE_SENSOR: u16 = 0x0302;
const LEVEL_CONTROLLABLE_OUTPUT: u16 = 0x0003;
const ON_OFF_OUTPUT: u16 = 0x0002;
const SIMPLE_SENSOR: u16 = 0x000C;

const BASIC_ATTRIBUTES: &[u16] = &[0x0000, 0x0001, 0x0002, 0x0003, 0x0004, 0x0005, 0x0007, 0x4000];
const THERMOSTAT_ATTRIBUTES: &[u16] = &[
    0x0000, 0x0001, 0x0002, 0x0003, 0x0004, 0x0005, 0x0006, 0x0011, 0x0012, 0x0013, 0x0014, 0x0015, 0x0016, 0x0017,
    0x0018, 0x0019, 0x001B, 0x001C, 0x0020, 0x0021, 0x0022, 0x0025, 0x0029,
];
const ANALOG_OUTPUT_ATTRIBUTES: &[u16] = &[0x001C, 0x0041, 0x0045, 0x0051, 0x0055, 0x006F, 0x0075];
const ANALOG_INPUT_ATTRIBUTES: &[u16] = &[0x001C, 0x0051, 0x0055, 0x006F, 0x0075];
const BINARY_ATTRIBUTES: &[u16] = &[0x001C, 0x0051, 0x0055, 0x006F];
const MEASUREMENT_ATTRIBUTES: &[u16] = &[0x0000, 0x0001, 0x0002];

pub const MANUFACTURER: &str = "Google Nest";
pub const MODEL: &str = "Nest Learning Thermostat";
/// The gen 2's: a model of its own, for the coordinator to tell the two apart.
pub const MODEL_GEN2: &str = "Nest Learning Thermostat (gen2)";

/// The setpoint range, as the MQTT climate entity has it.
const MIN: i16 = 900;
const MAX: i16 = 3200;
/// The least gap the unit keeps between a range's low and high, 0.1 °C.
const DEAD_BAND: i8 = 15;
const NONE: i16 = i16::MIN;
/// ThermostatProgrammingOperationMode: following its schedule, economy.
const SCHEDULE: u8 = 0x01;
const ECONOMY: u8 = 0x04;
/// A weekly schedule's mode: its transitions' heating or cooling setpoints.
const HEAT_SETPOINTS: u8 = 0x01;
const COOL_SETPOINTS: u8 = 0x02;
// BACnet engineering units, Analog Output's.
const SECONDS: u16 = 73;
const MINUTES: u16 = 72;
const HOURS: u16 = 71;
const PERCENT: u16 = 98;

/// An endpoint: its number, device type and server clusters.
#[derive(Debug, PartialEq)]
pub struct Endpoint {
    pub id: u8,
    pub device: u16,
    pub clusters: Vec<u16>,
}

pub struct Nest {
    pub bridge: Arc<Bridge>,
    pub endpoints: Vec<Endpoint>,
    /// The Weather cluster's attributes written so far.
    weather: Mutex<BTreeMap<u16, Value>>,
}

/// The buckets and the computed state, as of now.
struct View {
    dev: Fields,
    sh: Fields,
    state: Fields,
}

impl View {
    fn mode(&self) -> &str {
        self.state.get("mode").and_then(Json::as_str).unwrap_or("")
    }

    /// A temperature as MQTT gives it: the computed state's `key`.
    fn shown(&self, key: &str) -> Option<i16> {
        number(self.state.get(key)).map(centi)
    }
}

/// °C as ZCL's hundredths of a degree.
fn centi(celsius: f64) -> i16 {
    (celsius * 100.0).round() as i16
}

/// A ZCL temperature as the °C argument the commands take.
fn degrees(centi: i16) -> String {
    format!("{}", f64::from(centi) / 100.0)
}

fn view(bridge: &Bridge) -> View {
    let buckets = bridge.buckets();
    let dev = buckets.get("device").cloned().unwrap_or_default();
    let sh = buckets.get("shared").cloned().unwrap_or_default();
    let state = bridge.state(&dev, &sh);
    View { dev, sh, state }
}

impl Nest {
    /// The unit's endpoints, for its equipment as it has reported it.
    pub fn new(bridge: Arc<Bridge>) -> Nest {
        let v = view(&bridge);
        let has = |key: &str| truthy(v.dev.get(key)) || truthy(v.sh.get(key));
        let has_fan = has("has_fan");
        let humidity_control = has("has_humidifier") || has("has_dehumidifier");
        let mut thermostat = vec![BASIC, POWER, IDENTIFY, THERMOSTAT, DISPLAY, HUMIDITY, WEATHER];
        if has_fan {
            thermostat.push(FAN);
        }
        if has("has_hot_water_control") {
            thermostat.push(HOT_WATER);
        }
        let mut endpoints = vec![
            Endpoint { id: THERMOSTAT_ENDPOINT, device: THERMOSTAT_DEVICE, clusters: thermostat },
            Endpoint { id: BACKPLATE_ENDPOINT, device: TEMPERATURE_SENSOR, clusters: vec![TEMPERATURE] },
        ];
        if has_fan {
            endpoints.push(Endpoint {
                id: FAN_TIMER_ENDPOINT,
                device: LEVEL_CONTROLLABLE_OUTPUT,
                clusters: vec![ANALOG_OUTPUT],
            });
        }
        if humidity_control {
            endpoints.push(Endpoint {
                id: HUMIDITY_TARGET_ENDPOINT,
                device: LEVEL_CONTROLLABLE_OUTPUT,
                clusters: vec![ANALOG_OUTPUT],
            });
        }
        endpoints.push(Endpoint { id: TIME_TO_TARGET_ENDPOINT, device: SIMPLE_SENSOR, clusters: vec![ANALOG_INPUT] });
        if has("has_air_filter") {
            endpoints.push(Endpoint {
                id: FILTER_RUNTIME_ENDPOINT,
                device: SIMPLE_SENSOR,
                clusters: vec![ANALOG_INPUT],
            });
        }
        if has_fan {
            for id in [FAN_SCHEDULE_MINUTES_ENDPOINT, FAN_SCHEDULE_START_ENDPOINT, FAN_SCHEDULE_END_ENDPOINT] {
                endpoints.push(Endpoint { id, device: LEVEL_CONTROLLABLE_OUTPUT, clusters: vec![ANALOG_OUTPUT] });
            }
        }
        for id in [SCREEN_TIMEOUT_ENDPOINT, SCREEN_BRIGHTNESS_ENDPOINT] {
            endpoints.push(Endpoint { id, device: LEVEL_CONTROLLABLE_OUTPUT, clusters: vec![ANALOG_OUTPUT] });
        }
        for &(id, key, _) in SWITCHES {
            let present = match key {
                "away_temperature_low_enabled" => has("can_heat"),
                "away_temperature_high_enabled" => has("can_cool"),
                "target_humidity_enabled" => humidity_control,
                "hot_water_boost" | "hot_water_schedule" => has("has_hot_water_control"),
                "filter_reminder_enabled" => has("has_air_filter"),
                "fan_schedule" => has_fan,
                _ => true,
            };
            if present {
                endpoints.push(Endpoint { id, device: ON_OFF_OUTPUT, clusters: vec![BINARY_OUTPUT] });
            }
        }
        for &(id, _, _, equipment) in STATES {
            if equipment.is_empty() || has(equipment) {
                endpoints.push(Endpoint { id, device: SIMPLE_SENSOR, clusters: vec![BINARY_INPUT] });
            }
        }
        let wired = bridge::wired_stages(&v.dev);
        for (id, &(key, ..)) in (STAGE_ENDPOINTS..).zip(stages()) {
            if wired.iter().any(|w| w.0 == key) {
                endpoints.push(Endpoint { id, device: SIMPLE_SENSOR, clusters: vec![BINARY_INPUT] });
            }
        }
        if bridge.sensor.get().is_some() {
            endpoints.push(Endpoint {
                id: TEMPERATURE_SOURCE_ENDPOINT,
                device: SIMPLE_SENSOR,
                clusters: vec![BINARY_INPUT],
            });
        }
        Nest { bridge, endpoints, weather: Mutex::new(BTreeMap::new()) }
    }

    /// The endpoints the unit's equipment calls for now, when they differ
    /// from these and keep all of them: equipment reported after Zigbee
    /// started, such as a Heat Link's hot water. Equipment that goes keeps
    /// its endpoints until nestlocal restarts, as on Matter.
    pub fn grown(&self) -> Option<Vec<Endpoint>> {
        let now = Nest::new(self.bridge.clone()).endpoints;
        let kept = self
            .endpoints
            .iter()
            .all(|e| now.iter().any(|n| n.id == e.id && e.clusters.iter().all(|c| n.clusters.contains(c))));
        (kept && now != self.endpoints).then_some(now)
    }

    /// What changes reports go out for: endpoint, cluster, attributes.
    pub fn reported(&self) -> Vec<(u8, u16, &'static [u16])> {
        let mut out = Vec::new();
        for e in &self.endpoints {
            for &cluster in &e.clusters {
                let attributes: &'static [u16] = match cluster {
                    THERMOSTAT => {
                        &[0x0000, 0x0001, 0x0002, 0x0011, 0x0012, 0x0013, 0x0014, 0x001B, 0x001C, 0x0025, 0x0029]
                    }
                    POWER => &[0x0020],
                    FAN | DISPLAY | HUMIDITY | TEMPERATURE => &[0x0000],
                    ANALOG_OUTPUT | ANALOG_INPUT | BINARY_INPUT | BINARY_OUTPUT => &[0x0055],
                    HOT_WATER => HOT_WATER_ATTRIBUTES,
                    _ => continue,
                };
                out.push((e.id, cluster, attributes));
            }
        }
        out
    }

    fn view(&self) -> View {
        view(&self.bridge)
    }

    fn has(&self, endpoint: u8, cluster: u16) -> bool {
        self.endpoints.iter().any(|e| e.id == endpoint && e.clusters.contains(&cluster))
    }

    fn apply(&self, name: &str, arg: &str) -> u8 {
        match self.bridge.apply("zigbee", name, arg) {
            Ok(()) => status::SUCCESS,
            Err(_) => status::INVALID_VALUE,
        }
    }

    /// The setpoint a heating (or cooling) write changes, for the unit's mode.
    fn setpoint(view: &View, heating: bool) -> &'static str {
        match (view.mode(), heating) {
            ("heat_cool", true) => "target_low",
            ("heat_cool", false) => "target_high",
            _ => "target",
        }
    }

    fn occupied_setpoint(view: &View, heating: bool) -> i16 {
        let eco = bridge::eco_targets(&view.state).and_then(|(heat, cool)| match (view.mode(), heating) {
            ("heat", _) | ("heat_cool", true) => heat,
            ("cool", _) | ("heat_cool", false) => cool,
            _ => None,
        });
        eco.map(centi).or_else(|| view.shown(Self::setpoint(view, heating))).unwrap_or(if heating {
            2000
        } else {
            2400
        })
    }

    /// The unit's schedule bucket, and the mode bits its setpoints are: a
    /// range schedule's are both, its low the heating setpoint and its high
    /// the cooling one.
    fn schedule(&self) -> (Option<Fields>, Option<u8>) {
        let bucket = self.bridge.buckets().remove("schedule");
        let mode = schedule::kind(bucket.as_ref().unwrap_or(&Fields::new())).map(|k| match k {
            Kind::Heat => HEAT_SETPOINTS,
            Kind::Cool => COOL_SETPOINTS,
            Kind::Range => HEAT_SETPOINTS | COOL_SETPOINTS,
        });
        (bucket, mode)
    }

    /// Set Weekly Schedule: transitions, days (bit 0 Sunday to bit 6
    /// Saturday, bit 7 away), mode, then each transition's minutes after
    /// midnight and its heating then cooling setpoint, as the mode has them.
    fn set_weekly_schedule(&self, payload: &[u8]) -> u8 {
        let [count, days, mode, transitions @ ..] = payload else {
            return status::MALFORMED_COMMAND;
        };
        let width =
            2 + 2 * (mode & HEAT_SETPOINTS).count_ones() as usize + 2 * (mode & COOL_SETPOINTS).count_ones() as usize;
        if transitions.len() != usize::from(*count) * width {
            return status::MALFORMED_COMMAND;
        }
        let (bucket, Some(want)) = self.schedule() else {
            return status::INVALID_FIELD;
        };
        let bucket = bucket.unwrap_or_default();
        // A range schedule takes both setpoints; the others the one theirs is.
        if usize::from(*count) > schedule::DAILY {
            return status::INSUFFICIENT_SPACE;
        }
        if mode & want != want || mode & !3 != 0 || *days == 0 || days & 0x80 != 0 {
            return status::INVALID_FIELD;
        }
        // The heating setpoint comes first; each only where the mode has it.
        let has = |bit: u8| mode & bit != 0;
        let cool_at = if has(HEAT_SETPOINTS) { 4 } else { 2 };
        let setpoint = |t: &[u8], at: usize| i16::from_le_bytes([t[at], t[at + 1]]);
        let days: Vec<usize> = (0..7).filter(|b| days & (1 << b) != 0).map(|b| (b + 6) % 7).collect();
        // What each day named holds: minutes, and the setpoints taken.
        let held: Vec<Vec<(u16, Vec<i16>)>> = days
            .iter()
            .map(|&d| match want {
                HEAT_SETPOINTS | COOL_SETPOINTS => {
                    schedule::day(&bucket, d).into_iter().map(|(m, c)| (m, vec![centi(c)])).collect()
                }
                _ => schedule::range_day(&bucket, d)
                    .into_iter()
                    .map(|(m, low, high)| (m, vec![centi(low), centi(high)]))
                    .collect(),
            })
            .collect();
        let mut points = Vec::new();
        for t in transitions.chunks(width) {
            let minutes = u16::from_le_bytes([t[0], t[1]]);
            if minutes >= 24 * 60 {
                return status::INVALID_FIELD;
            }
            let heat = if has(HEAT_SETPOINTS) { setpoint(t, 2) } else { 0 };
            let cool = if has(COOL_SETPOINTS) { setpoint(t, cool_at) } else { 0 };
            // Within the setpoint limits the cluster advertises, a range's
            // high the unit's gap above its low.
            let taken = match want {
                HEAT_SETPOINTS => vec![heat],
                COOL_SETPOINTS => vec![cool],
                _ => vec![heat, cool],
            };
            // A day sent back whole echoes the setpoints it holds, those the
            // thermostat or MQTT set outside these narrower limits too.
            let echoed = held.iter().all(|day| day.iter().any(|(m, s)| (*m, s) == (minutes, &taken)));
            if (!echoed && taken.iter().any(|s| !(MIN..=MAX).contains(s)))
                || (want == HEAT_SETPOINTS | COOL_SETPOINTS && f64::from(cool - heat) / 100.0 < schedule::RANGE_GAP)
            {
                return status::INVALID_VALUE;
            }
            points.push((minutes, f64::from(heat) / 100.0, f64::from(cool) / 100.0));
        }
        let days = days.into_iter();
        let command = if want == HEAT_SETPOINTS | COOL_SETPOINTS {
            schedule::range_command(&days.map(|d| (d, points.clone())).collect::<Vec<_>>())
        } else {
            let one = |&(m, heat, cool): &(u16, f64, f64)| (m, if want == HEAT_SETPOINTS { heat } else { cool });
            schedule::command(&days.map(|d| (d, points.iter().map(one).collect())).collect::<Vec<_>>())
        };
        let expected = match want {
            HEAT_SETPOINTS => Kind::Heat,
            COOL_SETPOINTS => Kind::Cool,
            _ => Kind::Range,
        };
        match self.bridge.apply_in_mode("zigbee", "heating_times", &command, Some(expected)) {
            Ok(()) => status::SUCCESS,
            Err(_) => status::INVALID_VALUE,
        }
    }

    /// The whole week, as Get Weekly Schedule Responses with the schedule's
    /// own setpoints; none without a schedule.
    pub fn week(&self) -> Vec<Vec<u8>> {
        match self.get_weekly_schedule(&[0x7F, HEAT_SETPOINTS | COOL_SETPOINTS]) {
            Answer::Command(_, responses) => responses,
            Answer::Status(_) => Vec::new(),
        }
    }

    /// Get Weekly Schedule (days, mode): the days it asks, as a Get Weekly
    /// Schedule Response, in Set Weekly Schedule's layout, for each set of
    /// those days with the same transitions.
    fn get_weekly_schedule(&self, payload: &[u8]) -> Answer {
        let [days, mode, ..] = *payload else {
            return status::MALFORMED_COMMAND.into();
        };
        let asked: Vec<usize> = (0..7).filter(|b| days & (1 << b) != 0).collect();
        if asked.is_empty() {
            return status::INVALID_FIELD.into();
        }
        let (Some(bucket), Some(want)) = self.schedule() else {
            return status::NOT_FOUND.into();
        };
        if mode & want == 0 {
            return status::NOT_FOUND.into();
        }
        // The setpoints asked for that the schedule has: a range's low as
        // the heating setpoint, its high as the cooling one.
        let want = mode & want;
        // Minutes after midnight, and those setpoints.
        type Transition = (u16, Vec<i16>);
        let range = schedule::kind(&bucket) == Some(Kind::Range);
        let day = |d: usize| -> Vec<Transition> {
            if range {
                let points = schedule::range_day(&bucket, d).into_iter();
                let pick = |low: f64, high: f64| match want {
                    HEAT_SETPOINTS => vec![centi(low)],
                    COOL_SETPOINTS => vec![centi(high)],
                    _ => vec![centi(low), centi(high)],
                };
                points.map(|(m, low, high)| (m, pick(low, high))).collect()
            } else {
                schedule::day(&bucket, d).into_iter().map(|(m, c)| (m, vec![centi(c)])).collect()
            }
        };
        let mut groups: Vec<(u8, Vec<Transition>)> = Vec::new();
        for bit in asked {
            let points: Vec<_> = day((bit + 6) % 7).into_iter().take(schedule::DAILY).collect();
            match groups.iter_mut().find(|(_, p)| *p == points) {
                Some((bits, _)) => *bits |= 1 << bit,
                None => groups.push((1 << bit, points)),
            }
        }
        let responses = groups.into_iter().map(|(bits, points)| {
            let mut out = vec![points.len() as u8, bits, want];
            for (minutes, setpoints) in points {
                out.extend(minutes.to_le_bytes());
                for s in setpoints {
                    out.extend(s.to_le_bytes());
                }
            }
            out
        });
        Answer::Command(0x00, responses.collect())
    }

    fn thermostat(&self, attribute: u16) -> Option<Value> {
        let view = self.view();
        let temperature = |t: Option<i16>| Value::I16(t.unwrap_or(NONE));
        Some(match attribute {
            0x0000 => temperature(view.shown("current_temperature")),
            0x0001 => temperature(self.bridge.outdoor.temperature().map(centi)),
            0x0002 => Value::Bitmap8(u8::from(self.bridge.occupancy.occupied())),
            0x0003 | 0x0005 | 0x0015 | 0x0017 => Value::I16(MIN),
            0x0004 | 0x0006 | 0x0016 | 0x0018 => Value::I16(MAX),
            0x0011 => Value::I16(Self::occupied_setpoint(&view, false)),
            0x0012 => Value::I16(Self::occupied_setpoint(&view, true)),
            0x0013 => Value::I16(view.shown("away_temperature_high").unwrap_or(MAX)),
            0x0014 => Value::I16(view.shown("away_temperature_low").unwrap_or(MIN)),
            0x0019 => Value::I8(DEAD_BAND),
            0x0020 => Value::Enum8(1), // StartOfWeek: Monday
            0x0021 => Value::U8((schedule::DAILY * 7) as u8),
            0x0022 => Value::U8(schedule::DAILY as u8),
            // Cooling only, heating only, or both.
            0x001B => Value::Enum8(match (truthy(view.sh.get("can_heat")), truthy(view.sh.get("can_cool"))) {
                (true, false) => 0x02,
                (false, true) => 0x00,
                _ => 0x04,
            }),
            0x001C => Value::Enum8(match view.mode() {
                "heat" => 0x04,
                "cool" => 0x03,
                "heat_cool" => 0x01,
                _ => 0x00,
            }),
            0x0025 => {
                let eco = view.state.get("preset").and_then(Json::as_str) == Some("eco");
                Value::Bitmap8(if eco { ECONOMY } else { SCHEDULE })
            }
            0x0029 => {
                let mut running = match view.state.get("action").and_then(Json::as_str) {
                    Some("heating") => 0x0001,
                    Some("cooling") => 0x0002,
                    _ => 0,
                };
                if truthy(view.state.get("fan_running")) {
                    running |= 0x0004;
                }
                if truthy(view.state.get("heat_stage_2")) {
                    running |= 0x0008;
                }
                if truthy(view.state.get("cool_stage_2")) {
                    running |= 0x0010;
                }
                Value::Bitmap16(running)
            }
            _ => return None,
        })
    }

    fn write_thermostat(&self, attribute: u16, value: Value) -> u8 {
        let view = self.view();
        match (attribute, value) {
            (0x0011 | 0x0012, Value::I16(t)) if !(MIN..=MAX).contains(&t) => status::INVALID_VALUE,
            (0x0011, Value::I16(t)) => self.apply(Self::setpoint(&view, false), &degrees(t)),
            (0x0012, Value::I16(t)) => self.apply(Self::setpoint(&view, true), &degrees(t)),
            (0x0013, Value::I16(t)) => self.apply("away_temperature_high", &degrees(t)),
            (0x0014, Value::I16(t)) => self.apply("away_temperature_low", &degrees(t)),
            (0x001C, Value::Enum8(mode)) => match mode {
                0x00 => self.apply("mode", "off"),
                0x01 => self.apply("mode", "heat_cool"),
                0x03 => self.apply("mode", "cool"),
                0x04 => self.apply("mode", "heat"),
                _ => status::INVALID_VALUE,
            },
            (0x0025, Value::Bitmap8(mode)) => self.apply("preset", if mode & ECONOMY != 0 { "eco" } else { "none" }),
            _ => status::READ_ONLY,
        }
    }

    /// setpointRaiseLower: mode (heat, cool, both), amount in 0.1 °C, the
    /// result kept within the setpoint limits. Outside heat_cool the one
    /// setpoint moves unless the mode names the other side of the unit's.
    fn raise_lower(&self, payload: &[u8]) -> u8 {
        let [mode, amount, ..] = *payload else {
            return status::INVALID_FIELD;
        };
        if mode > 0x02 {
            return status::INVALID_FIELD;
        }
        let (heat, cool) = (mode != 0x01, mode != 0x00);
        let delta = i16::from(amount as i8) * 10;
        let view = self.view();
        let raise = |heating: bool, field: &str| match view.shown(field) {
            Some(t) => self.apply(Self::setpoint(&view, heating), &degrees((t + delta).clamp(MIN, MAX))),
            None => status::SUCCESS,
        };
        match view.mode() {
            "heat_cool" => {}
            "heat" if !heat => return status::SUCCESS,
            "cool" if !cool => return status::SUCCESS,
            _ => return raise(true, "target"),
        }
        let mut result = status::SUCCESS;
        if heat {
            result = raise(true, "target_low");
        }
        if cool && result == status::SUCCESS {
            result = raise(false, "target_high");
        }
        result
    }

    /// An Analog Output: the fan timer's minutes, the fan schedule's, its
    /// start and end hours, the screen timeout's seconds, the screen's
    /// brightness (with it automatic, the percentage stored for manual, else
    /// full), or the target humidity.
    fn analog_output(&self, endpoint: u8, attribute: u16) -> Option<Value> {
        let dev = self.view().dev;
        let hour = |field: &str| number(dev.get(field)).map_or(0.0, |s| (s / 3600.0).floor());
        let (description, units, min, max, value) = match endpoint {
            FAN_TIMER_ENDPOINT => {
                let minutes = number(dev.get("fan_timer_duration")).map_or(15.0, |s| s / 60.0);
                ("Fan timer duration", MINUTES, 15.0, 1440.0, minutes)
            }
            FAN_SCHEDULE_MINUTES_ENDPOINT => {
                let minutes = number(dev.get("fan_duty_cycle")).map_or(60.0, |s| (s / 60.0).round());
                ("Fan schedule run time", MINUTES, 15.0, 60.0, minutes)
            }
            FAN_SCHEDULE_START_ENDPOINT => ("Fan schedule start", HOURS, 0.0, 23.0, hour("fan_duty_start_time")),
            FAN_SCHEDULE_END_ENDPOINT => ("Fan schedule end", HOURS, 0.0, 23.0, hour("fan_duty_end_time")),
            SCREEN_TIMEOUT_ENDPOINT => (
                "Screen timeout",
                SECONDS,
                *crate::screen::SECONDS.start() as f32,
                *crate::screen::SECONDS.end() as f32,
                crate::screen::seconds(&dev) as f64,
            ),
            SCREEN_BRIGHTNESS_ENDPOINT => {
                let (percent, _) = crate::brightness::reported(&dev, self.bridge.brightness.shown());
                (
                    "Screen brightness",
                    PERCENT,
                    *crate::brightness::PERCENTS.start() as f32,
                    *crate::brightness::PERCENTS.end() as f32,
                    percent.unwrap_or(100) as f64,
                )
            }
            _ => ("Target humidity", PERCENT, 10.0, 60.0, number(dev.get("target_humidity")).unwrap_or(35.0)),
        };
        Some(match attribute {
            0x001C => Value::Str(description.into()),
            0x0041 => Value::F32(max),
            0x0045 => Value::F32(min),
            0x0051 => Value::Bool(false), // in service
            0x0055 => Value::F32(value as f32),
            0x006F => Value::Bitmap8(0), // status flags: all well
            0x0075 => Value::Enum16(units),
            _ => return None,
        })
    }

    fn write_analog_output(&self, endpoint: u8, attribute: u16, value: Value) -> u8 {
        match (attribute, value) {
            (0x0055, Value::F32(v)) if endpoint == FAN_TIMER_ENDPOINT => {
                self.apply("fan_timer_duration", &(v.round() as i64).to_string())
            }
            (0x0055, Value::F32(v)) if endpoint == FAN_SCHEDULE_MINUTES_ENDPOINT => {
                self.apply("fan_schedule_minutes", &(v.round() as i64).to_string())
            }
            (0x0055, Value::F32(v))
                if endpoint == FAN_SCHEDULE_START_ENDPOINT || endpoint == FAN_SCHEDULE_END_ENDPOINT =>
            {
                let name =
                    if endpoint == FAN_SCHEDULE_START_ENDPOINT { "fan_schedule_start" } else { "fan_schedule_end" };
                self.apply(name, &(v.round() as i64).to_string())
            }
            (0x0055, Value::F32(v)) if endpoint == SCREEN_TIMEOUT_ENDPOINT => {
                self.apply("screen_timeout", &(v.round() as i64).to_string())
            }
            (0x0055, Value::F32(v)) if endpoint == SCREEN_BRIGHTNESS_ENDPOINT => {
                self.apply("screen_brightness", &(v.round() as i64).to_string())
            }
            (0x0055, Value::F32(v)) => self.apply("target_humidity", &format!("{}", v.round())),
            _ => status::READ_ONLY,
        }
    }

    /// An Analog Input: the seconds to temperature (0: none), or the
    /// filter's hours.
    fn analog_input(&self, endpoint: u8, attribute: u16) -> Option<Value> {
        let view = self.view();
        let (description, units, value) = match endpoint {
            TIME_TO_TARGET_ENDPOINT => {
                let now =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
                let left = number(view.dev.get("time_to_target")).map_or(0.0, |t| (t - now).max(0.0).round());
                ("Time to temperature", SECONDS, left)
            }
            _ => (
                "Filter runtime",
                HOURS,
                number(view.dev.get("filter_runtime_sec")).map_or(0.0, |s| (s / 360.0).round() / 10.0),
            ),
        };
        Some(match attribute {
            0x001C => Value::Str(description.into()),
            0x0051 => Value::Bool(false), // in service
            0x0055 => Value::F32(value as f32),
            0x006F => Value::Bitmap8(0), // status flags: all well
            0x0075 => Value::Enum16(units),
            _ => return None,
        })
    }

    /// A state endpoint's description and value.
    fn state(&self, endpoint: u8) -> Option<(&'static str, bool)> {
        let v = self.view();
        if let Some(&(key, _, name, _)) = endpoint.checked_sub(STAGE_ENDPOINTS).and_then(|i| stages().nth(i as usize)) {
            return Some((name, truthy(v.state.get(key))));
        }
        STATES
            .iter()
            .find(|(e, _, _, _)| *e == endpoint)
            .map(|(_, key, description, _)| (*description, truthy(v.dev.get(*key))))
    }

    fn switch(&self, endpoint: u8) -> Option<(&'static str, &'static str)> {
        SWITCHES.iter().find(|(e, _, _)| *e == endpoint).map(|(_, key, description)| (*key, *description))
    }

    /// A Weather attribute: as written, or none.
    fn weather(&self, attribute: u16) -> Option<Value> {
        if attribute == EXTERNAL_TEMPERATURE {
            let reading = self.bridge.sensor.get().and_then(|sensor| sensor.reading());
            return Some(Value::I16(reading.map_or(NONE, centi)));
        }
        if let Some(value) = self.weather.lock().unwrap().get(&attribute) {
            return Some(value.clone());
        }
        Some(match (attribute, attribute & 0xF) {
            (OUTDOOR_TEMPERATURE | EXTERNAL_TEMPERATURE, _) => Value::I16(NONE),
            (OUTDOOR_HUMIDITY, _) => Value::U8(NO_HUMIDITY),
            (CONDITION, _) => Value::Str(String::new()),
            (NEXT_RISING | NEXT_SETTING, _) => Value::Utc(NO_TIME),
            (_, SLOT_TIME) => Value::Utc(NO_TIME),
            (_, SLOT_TEMPERATURE | SLOT_LOW) => Value::I16(NONE),
            (_, SLOT_HUMIDITY) => Value::U8(NO_HUMIDITY),
            _ => return None,
        })
    }

    /// Writes a Weather attribute (of its type, the caller has checked):
    /// the readings go to the outdoor state as MQTT's do; the rest make up
    /// the conditions and forecast, as set/weather's payload does.
    fn write_weather(&self, attribute: u16, value: Value) -> u8 {
        match value {
            Value::I16(t) if t != NONE && !(-6000..=6000).contains(&t) => return status::INVALID_VALUE,
            Value::U8(h) if h != NO_HUMIDITY && h > 100 => return status::INVALID_VALUE,
            _ => {}
        }
        if attribute == EXTERNAL_TEMPERATURE {
            let Value::I16(t) = value else {
                return status::INVALID_VALUE;
            };
            return self.apply("external_temperature", &(f64::from(t) / 100.0).to_string());
        }
        let mut weather = self.weather.lock().unwrap();
        weather.insert(attribute, value);
        match attribute {
            OUTDOOR_TEMPERATURE | OUTDOOR_HUMIDITY => {
                self.bridge
                    .outdoor
                    .set(temperature(&weather, OUTDOOR_TEMPERATURE), humidity(&weather, OUTDOOR_HUMIDITY));
            }
            _ => {
                let forecast = |base: u16, slots: u16| -> Vec<HAForecast> {
                    (0..slots)
                        .map(|i| base | i << 4)
                        .filter_map(|slot| {
                            Some(HAForecast {
                                datetime: Some(time(&weather, slot | SLOT_TIME)?),
                                temperature: temperature(&weather, slot | SLOT_TEMPERATURE),
                                templow: temperature(&weather, slot | SLOT_LOW),
                                humidity: humidity(&weather, slot | SLOT_HUMIDITY),
                                is_daytime: None,
                            })
                        })
                        .collect()
                };
                let condition = match weather.get(&CONDITION) {
                    Some(Value::Str(c)) => c.clone(),
                    _ => String::new(),
                };
                self.bridge.outdoor.set_weather(&HAWeather {
                    condition,
                    next_rising: time(&weather, NEXT_RISING),
                    next_setting: time(&weather, NEXT_SETTING),
                    hourly: Some(forecast(HOURLY, HOURLY_SLOTS)),
                    daily: Some(forecast(DAILY, DAILY_SLOTS)),
                });
            }
        }
        status::SUCCESS
    }
}

/// A written Weather temperature, °C.
fn temperature(weather: &BTreeMap<u16, Value>, attribute: u16) -> Option<f64> {
    match weather.get(&attribute) {
        Some(Value::I16(t)) if *t != NONE => Some(f64::from(*t) / 100.0),
        _ => None,
    }
}

/// A written Weather humidity, %.
fn humidity(weather: &BTreeMap<u16, Value>, attribute: u16) -> Option<f64> {
    match weather.get(&attribute) {
        Some(Value::U8(h)) if *h != NO_HUMIDITY => Some(f64::from(*h)),
        _ => None,
    }
}

/// A written Weather time.
fn time(weather: &BTreeMap<u16, Value>, attribute: u16) -> Option<DateTime<FixedOffset>> {
    match weather.get(&attribute) {
        Some(Value::Utc(t)) if *t != NO_TIME => {
            DateTime::from_timestamp(ZIGBEE_EPOCH + i64::from(*t), 0).map(|d| d.fixed_offset())
        }
        _ => None,
    }
}

impl Clusters for Nest {
    fn attributes(&self, endpoint: u8, cluster: u16) -> Option<&'static [u16]> {
        if !self.has(endpoint, cluster) {
            return None;
        }
        Some(match cluster {
            BASIC => BASIC_ATTRIBUTES,
            POWER => &[0x0020],
            IDENTIFY => &[0x0000],
            THERMOSTAT => THERMOSTAT_ATTRIBUTES,
            FAN | DISPLAY => &[0x0000, 0x0001],
            TEMPERATURE | HUMIDITY => MEASUREMENT_ATTRIBUTES,
            ANALOG_OUTPUT => ANALOG_OUTPUT_ATTRIBUTES,
            ANALOG_INPUT => ANALOG_INPUT_ATTRIBUTES,
            BINARY_INPUT | BINARY_OUTPUT => BINARY_ATTRIBUTES,
            WEATHER => WEATHER_ATTRIBUTES.as_slice(),
            HOT_WATER => HOT_WATER_ATTRIBUTES,
            _ => return None,
        })
    }

    fn read(&self, endpoint: u8, cluster: u16, attribute: u16) -> Option<Value> {
        match (cluster, attribute) {
            (BASIC, 0x0000) => Some(Value::U8(3)), // ZCL version
            (BASIC, 0x0001) => Some(Value::U8(1)), // application version
            (BASIC, 0x0002) => Some(Value::U8(6)), // stack version: EmberZNet 6
            (BASIC, 0x0003) => Some(Value::U8(crate::device::current().generation)), // hardware
            (BASIC, 0x0004) => Some(Value::Str(MANUFACTURER.into())),
            (BASIC, 0x0005) => {
                let model = if crate::device::current().generation == 2 { MODEL_GEN2 } else { MODEL };
                Some(Value::Str(model.into()))
            }
            (BASIC, 0x0007) => Some(Value::Enum8(0x81)), // mains (the HVAC system's 24 V), battery backup
            (BASIC, 0x4000) => Some(Value::Str(sprint(self.view().dev.get("current_version")))),
            // Volts, in 100 mV.
            (POWER, 0x0020) => {
                let volts = number(self.view().dev.get("battery_level"));
                Some(Value::U8(volts.map_or(0xFF, |v| (v * 10.0).round() as u8)))
            }
            (IDENTIFY, 0x0000) => Some(Value::U16(0)),
            (THERMOSTAT, a) => self.thermostat(a),
            (FAN, 0x0000) => {
                let on = self.view().state.get("fan_mode").and_then(Json::as_str) == Some("on");
                Some(Value::Enum8(if on { 0x04 } else { 0x05 }))
            }
            (FAN, 0x0001) => Some(Value::Enum8(0x04)), // on/auto
            (DISPLAY, 0x0000) => {
                let fahrenheit = sprint(self.view().dev.get("temperature_scale")) == "F";
                Some(Value::Enum8(u8::from(fahrenheit)))
            }
            (DISPLAY, 0x0001) => Some(Value::Enum8(0)), // no keypad lockout
            (HUMIDITY, 0x0000) => {
                let humidity = number(self.view().dev.get("current_humidity"));
                Some(Value::U16(humidity.map_or(0xFFFF, |h| (h * 100.0).round() as u16)))
            }
            (HUMIDITY, 0x0001) => Some(Value::U16(0)),
            (HUMIDITY, 0x0002) => Some(Value::U16(10000)),
            (TEMPERATURE, 0x0000) => {
                let backplate = number(self.view().dev.get("backplate_temperature")).map(centi);
                Some(Value::I16(backplate.unwrap_or(NONE)))
            }
            (TEMPERATURE, 0x0001) => Some(Value::I16(-2000)),
            (TEMPERATURE, 0x0002) => Some(Value::I16(6000)),
            (ANALOG_OUTPUT, a) => self.analog_output(endpoint, a),
            (ANALOG_INPUT, a) => self.analog_input(endpoint, a),
            (BINARY_INPUT | BINARY_OUTPUT, 0x0051) => Some(Value::Bool(false)),
            (BINARY_INPUT | BINARY_OUTPUT, 0x006F) => Some(Value::Bitmap8(0)),
            (BINARY_INPUT, 0x001C) if endpoint == TEMPERATURE_SOURCE_ENDPOINT => {
                Some(Value::Str("External temperature in use".into()))
            }
            (BINARY_INPUT, 0x0055) if endpoint == TEMPERATURE_SOURCE_ENDPOINT => {
                Some(Value::Bool(self.bridge.temperature_source() == Some("external")))
            }
            (BINARY_INPUT, 0x001C) => self.state(endpoint).map(|(description, _)| Value::Str(description.into())),
            (BINARY_INPUT, 0x0055) => self.state(endpoint).map(|(_, on)| Value::Bool(on)),
            (BINARY_OUTPUT, 0x001C) => self.switch(endpoint).map(|(_, description)| Value::Str(description.into())),
            (BINARY_OUTPUT, 0x0055) => self.switch(endpoint).map(|(key, _)| {
                Value::Bool(match key {
                    "screen_brightness_auto" => {
                        crate::brightness::reported(&self.view().dev, self.bridge.brightness.shown()).1
                    }
                    _ => bridge::switch_on(&self.view().dev, key),
                })
            }),
            (WEATHER, a) => self.weather(a),
            (HOT_WATER, day @ 0x0000..=0x0006) => {
                let hw = self.bridge.hot_water_schedule().map(|o| o.value).unwrap_or_default();
                Some(Value::Str(hotwater::text(&hotwater::day(&hw, usize::from(day)))))
            }
            _ => None,
        }
    }

    fn write(&self, endpoint: u8, cluster: u16, attribute: u16, value: Value) -> u8 {
        match (cluster, attribute, value) {
            (THERMOSTAT, a, v) => self.write_thermostat(a, v),
            (FAN, 0x0000, Value::Enum8(mode)) => match mode {
                0x04 => self.apply("fan_mode", "on"),
                0x00 | 0x05 => self.apply("fan_mode", "auto"),
                _ => status::INVALID_VALUE,
            },
            (DISPLAY, 0x0000, Value::Enum8(scale)) => match scale {
                0 => self.apply("temperature_scale", "C"),
                1 => self.apply("temperature_scale", "F"),
                _ => status::INVALID_VALUE,
            },
            (ANALOG_OUTPUT, a, v) => self.write_analog_output(endpoint, a, v),
            (ANALOG_INPUT, 0x0055, Value::F32(hours)) if endpoint == FILTER_RUNTIME_ENDPOINT => {
                if hours == 0.0 {
                    self.apply("filter_reset", "ON")
                } else {
                    status::INVALID_VALUE
                }
            }
            (BINARY_OUTPUT, 0x0055, Value::Bool(on)) => match self.switch(endpoint) {
                Some((key, _)) => self.apply(key, if on { "ON" } else { "OFF" }),
                None => status::READ_ONLY,
            },
            // Identifying lights the display; stopping leaves it to its timeout.
            (IDENTIFY, 0x0000, Value::U16(0)) => status::SUCCESS,
            (IDENTIFY, 0x0000, Value::U16(_)) => self.apply("wake_screen", ""),
            (WEATHER, a, v) => self.write_weather(a, v),
            (HOT_WATER, day @ 0x0000..=0x0006, Value::Str(periods)) => match hotwater::from_text(&periods) {
                Some(periods) => self.apply("hot_water_times", &hotwater::command(&[(usize::from(day), periods)])),
                None => status::INVALID_VALUE,
            },
            _ => status::READ_ONLY,
        }
    }

    fn command(&self, _endpoint: u8, cluster: u16, command: u8, payload: &[u8]) -> Answer {
        match (cluster, command) {
            (THERMOSTAT, 0x00) => self.raise_lower(payload).into(),
            (THERMOSTAT, 0x01) => self.set_weekly_schedule(payload).into(),
            (THERMOSTAT, 0x02) => self.get_weekly_schedule(payload),
            (THERMOSTAT, 0x03) => self.apply("clear_schedule", "").into(), // Clear Weekly Schedule
            (IDENTIFY, 0x00) => match *payload {
                [0, 0, ..] => status::SUCCESS,
                [_, _, ..] => self.apply("wake_screen", ""),
                _ => status::INVALID_FIELD,
            }
            .into(),
            (IDENTIFY, 0x01) => status::SUCCESS.into(),
            _ => status::UNSUP_CLUSTER_COMMAND.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::zcl::{serve, serve_all};
    use super::*;
    use crate::fields;

    fn nest(dev: Fields, sh: Fields) -> Nest {
        let bridge = crate::bridge::tests::test_bridge();
        bridge.store.merge(&format!("device.{}", bridge.serial), &dev, 0, 0, true);
        bridge.store.merge(&format!("shared.{}", bridge.serial), &sh, 0, 0, true);
        Nest::new(bridge)
    }

    #[test]
    fn endpoints_follow_the_equipment() {
        let ids = |n: &Nest| n.endpoints.iter().map(|e| e.id).collect::<Vec<_>>();
        let n = nest(fields! {}, fields! {"can_heat" => true});
        assert_eq!(ids(&n), [1, 2, 5, 40, 42, 10, 11, 12, 13, 14, 41, 43, 20, 21, 22, 23]);
        assert!(!n.has(1, FAN));
        let n = nest(
            fields! {"has_fan" => true, "has_humidifier" => true, "has_air_filter" => true},
            fields! {"can_heat" => true, "can_cool" => true},
        );
        assert_eq!(
            ids(&n),
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 40, 42, 10, 11, 12, 13, 14, 15, 16, 19, 29, 41, 43, 20, 21, 22, 23, 24, 26]
        );
        assert!(n.has(1, FAN));
    }

    #[test]
    fn generation_models_fit_the_basic_cluster_model_identifier() {
        assert!(MODEL.len() <= 32);
        assert!(MODEL_GEN2.len() <= 32);
        assert_ne!(MODEL, MODEL_GEN2);
    }

    #[test]
    fn the_thermostat_reads_its_temperatures() {
        let n = nest(
            fields! {"temperature_scale" => "C", "current_humidity" => 41, "battery_level" => 3.92,
            "backplate_temperature" => 22.1},
            fields! {"target_temperature_type" => "heat", "target_temperature" => 21.38377,
            "current_temperature" => 18.20557, "can_heat" => true, "hvac_heater_state" => true},
        );
        assert_eq!(n.read(1, THERMOSTAT, 0x0000), Some(Value::I16(1821)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0012), Some(Value::I16(2138)));
        assert_eq!(n.read(1, THERMOSTAT, 0x001C), Some(Value::Enum8(0x04)));
        assert_eq!(n.read(1, THERMOSTAT, 0x001B), Some(Value::Enum8(0x02)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0025), Some(Value::Bitmap8(SCHEDULE)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0029), Some(Value::Bitmap16(0x0001)));
        assert_eq!(n.read(1, HUMIDITY, 0x0000), Some(Value::U16(4100)));
        assert_eq!(n.read(1, DISPLAY, 0x0000), Some(Value::Enum8(0)));
        assert_eq!(n.read(1, POWER, 0x0020), Some(Value::U8(39)));
        assert_eq!(n.read(2, TEMPERATURE, 0x0000), Some(Value::I16(2210)));
        assert_eq!(n.read(10, BINARY_OUTPUT, 0x0055), Some(Value::Bool(false)));
        assert!(n.attributes(3, ANALOG_OUTPUT).is_none()); // no fan
    }

    #[test]
    fn states_and_times() {
        let soon = chrono::Utc::now().timestamp() + 600;
        let n = nest(
            fields! {"leaf" => true, "time_to_target" => soon, "has_air_filter" => true,
            "filter_runtime_sec" => 36000, "filter_replacement_needed" => false},
            fields! {"can_heat" => true},
        );
        assert_eq!(n.read(20, BINARY_INPUT, 0x0055), Some(Value::Bool(true)));
        assert_eq!(n.read(23, BINARY_INPUT, 0x0055), Some(Value::Bool(false)));
        assert_eq!(n.read(24, BINARY_INPUT, 0x0055), Some(Value::Bool(false)));
        assert!(n.attributes(25, BINARY_INPUT).is_none()); // no hot water
        let Some(Value::F32(left)) = n.read(5, ANALOG_INPUT, 0x0055) else { panic!() };
        assert!((598.0..=600.0).contains(&left));
        assert_eq!(n.read(5, ANALOG_INPUT, 0x0075), Some(Value::Enum16(SECONDS)));
        assert_eq!(n.read(6, ANALOG_INPUT, 0x0055), Some(Value::F32(10.0)));
        // Only 0 is written to the filter's runtime: the filter was changed.
        assert_eq!(n.write(6, ANALOG_INPUT, 0x0055, Value::F32(5.0)), status::INVALID_VALUE);
        assert_eq!(n.write(5, ANALOG_INPUT, 0x0055, Value::F32(0.0)), status::READ_ONLY);
        assert_eq!(n.write(6, ANALOG_INPUT, 0x0055, Value::F32(0.0)), status::SUCCESS);
        let changed = number(n.view().dev.get("filter_changed_date")).unwrap() as i64;
        assert!((soon - 602..=soon - 598).contains(&changed));
        assert_eq!(n.write(19, BINARY_OUTPUT, 0x0055, Value::Bool(true)), status::SUCCESS);
        assert_eq!(n.read(19, BINARY_OUTPUT, 0x0055), Some(Value::Bool(true)));
        assert_eq!(n.command(1, THERMOSTAT, 0x03, &[]), status::INVALID_VALUE.into()); // no schedule yet
        n.bridge.store.merge(&format!("schedule.{}", n.bridge.serial), &fields! {"ver" => 2}, 0, 0, true);
        assert_eq!(n.command(1, THERMOSTAT, 0x03, &[]), status::SUCCESS.into());
        assert_eq!(n.read(10, BINARY_OUTPUT, 0x0055), Some(Value::Bool(false)));
        // learning off with the schedule
    }

    #[test]
    fn every_listed_attribute_reads_and_every_reported_one_is_listed() {
        let n = nest(
            fields! {"has_fan" => true, "has_humidifier" => true, "has_dehumidifier" => true,
            "has_air_filter" => true, "has_hot_water_control" => true, "has_x2_heat" => true,
            "has_emer_heat" => true, "has_x3_cool" => true},
            fields! {"can_heat" => true, "can_cool" => true},
        );
        for e in &n.endpoints {
            for &cluster in &e.clusters {
                let ids = n.attributes(e.id, cluster).unwrap_or_else(|| panic!("{} {cluster:#06x}", e.id));
                assert!(ids.windows(2).all(|w| w[0] < w[1]), "{} {cluster:#06x} ascending", e.id);
                for &a in ids {
                    assert!(n.read(e.id, cluster, a).is_some(), "{} {cluster:#06x} {a:#06x}", e.id);
                }
            }
        }
        for (endpoint, cluster, attributes) in n.reported() {
            let ids = n.attributes(endpoint, cluster).unwrap();
            assert!(attributes.iter().all(|a| ids.contains(a)), "{endpoint} {cluster:#06x}");
        }
        // Clusters an endpoint does not have.
        assert!(n.attributes(1, BINARY_OUTPUT).is_none());
        assert!(n.attributes(10, THERMOSTAT).is_none());
        assert!(n.attributes(99, BASIC).is_none());
    }

    #[test]
    fn what_the_unit_has_not_reported_reads_as_zcls_invalid_values() {
        let n = nest(fields! {}, fields! {});
        assert_eq!(n.read(1, THERMOSTAT, 0x0000), Some(Value::I16(i16::MIN))); // 0x8000
        assert_eq!(n.read(1, THERMOSTAT, 0x0001), Some(Value::I16(i16::MIN)));
        assert_eq!(n.read(2, TEMPERATURE, 0x0000), Some(Value::I16(i16::MIN)));
        assert_eq!(n.read(1, HUMIDITY, 0x0000), Some(Value::U16(0xFFFF)));
        assert_eq!(n.read(1, POWER, 0x0020), Some(Value::U8(0xFF)));
        // Off, no setpoints yet, and neither heating nor cooling said: both.
        assert_eq!(n.read(1, THERMOSTAT, 0x001C), Some(Value::Enum8(0x00)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0011), Some(Value::I16(2400)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0012), Some(Value::I16(2000)));
        assert_eq!(n.read(1, THERMOSTAT, 0x001B), Some(Value::Enum8(0x04)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0029), Some(Value::Bitmap16(0)));
    }

    #[test]
    fn cooling_only_and_running_states() {
        let n = nest(
            fields! {},
            fields! {"target_temperature_type" => "cool", "can_cool" => true, "hvac_ac_state" => true,
            "hvac_fan_state" => true},
        );
        assert_eq!(n.read(1, THERMOSTAT, 0x001B), Some(Value::Enum8(0x00)));
        assert_eq!(n.read(1, THERMOSTAT, 0x001C), Some(Value::Enum8(0x03)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0029), Some(Value::Bitmap16(0x0006)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0002), Some(Value::Bitmap8(0))); // no motion: unoccupied
        n.bridge.occupancy.detected(chrono::Utc::now().timestamp());
        assert_eq!(n.read(1, THERMOSTAT, 0x0002), Some(Value::Bitmap8(1)));
    }

    #[test]
    fn stages_have_endpoints_as_wired_and_second_ones_running_state_bits() {
        let n = nest(
            fields! {"has_x2_heat" => true, "has_aux_heat" => true},
            fields! {"target_temperature_type" => "heat", "can_heat" => true, "hvac_heater_state" => true,
            "hvac_heat_x2_state" => true, "hvac_fan_state" => true},
        );
        let stage = |id: u8| (STAGE_ENDPOINTS..SCREEN_TIMEOUT_ENDPOINT).contains(&id);
        let ids: Vec<u8> = n.endpoints.iter().map(|e| e.id).filter(|&id| stage(id)).collect();
        assert_eq!(ids, [30, 31, 33], "heat stages 1 and 2, aux; no cool stage of its own");
        assert_eq!(n.read(31, BINARY_INPUT, 0x001C), Some(Value::Str("Heat stage 2".into())));
        assert_eq!(n.read(31, BINARY_INPUT, 0x0055), Some(Value::Bool(true)));
        assert_eq!(n.read(33, BINARY_INPUT, 0x0055), Some(Value::Bool(false)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0029), Some(Value::Bitmap16(0x000D)));
        assert!(!nest(fields! {}, fields! {"can_heat" => true}).endpoints.iter().any(|e| stage(e.id)));
        let cool = nest(
            fields! {"has_x2_cool" => true},
            fields! {"target_temperature_type" => "cool", "can_cool" => true, "hvac_cool_x2_state" => true},
        );
        assert_eq!(cool.read(1, THERMOSTAT, 0x0029), Some(Value::Bitmap16(0x0012)));
        assert_eq!(cool.read(38, BINARY_INPUT, 0x001C), Some(Value::Str("Cool stage 2".into())));
    }

    #[test]
    fn temperatures_read_the_same_in_either_scale() {
        let n = nest(
            fields! {"temperature_scale" => "F"},
            fields! {"target_temperature_type" => "heat", "target_temperature" => 19.5,
            "current_temperature" => 22.0, "can_heat" => true},
        );
        assert_eq!(n.read(1, DISPLAY, 0x0000), Some(Value::Enum8(1)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0012), Some(Value::I16(1950)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0000), Some(Value::I16(2200)));
        assert_eq!(n.write(1, DISPLAY, 0x0000, Value::Enum8(0)), status::SUCCESS);
        assert_eq!(n.read(1, DISPLAY, 0x0000), Some(Value::Enum8(0)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0012), Some(Value::I16(1950)));
        assert_eq!(n.write(1, DISPLAY, 0x0000, Value::Enum8(2)), status::INVALID_VALUE);
    }

    #[test]
    fn eco_setpoints_after_preset_change() {
        let n = nest(
            fields! {"temperature_scale" => "C", "away_temperature_low" => 16,
            "away_temperature_high" => 29, "away_temperature_low_enabled" => true,
            "away_temperature_high_enabled" => true},
            fields! {"target_temperature_type" => "range", "target_temperature" => 22,
            "target_temperature_low" => 20, "target_temperature_high" => 24,
            "can_heat" => true, "can_cool" => true},
        );
        let targets = || (n.read(1, THERMOSTAT, 0x0012), n.read(1, THERMOSTAT, 0x0011));
        let i16s = |low, high| (Some(Value::I16(low)), Some(Value::I16(high)));
        assert_eq!(targets(), i16s(2000, 2400));
        assert_eq!(n.write(1, THERMOSTAT, 0x0025, Value::Bitmap8(ECONOMY)), status::SUCCESS);
        assert_eq!(targets(), i16s(1600, 2900));
        assert_eq!(n.apply("away_temperature_high_enabled", "off"), status::SUCCESS);
        assert_eq!(targets(), i16s(1600, 2400));
        assert_eq!(n.write(1, THERMOSTAT, 0x0025, Value::Bitmap8(SCHEDULE)), status::SUCCESS);
        assert_eq!(targets(), i16s(2000, 2400));
        assert_eq!(n.write(1, THERMOSTAT, 0x001C, Value::Enum8(0x03)), status::SUCCESS);
        assert_eq!(n.apply("away_temperature_high_enabled", "on"), status::SUCCESS);
        assert_eq!(n.write(1, THERMOSTAT, 0x0025, Value::Bitmap8(ECONOMY)), status::SUCCESS);
        assert_eq!(targets(), i16s(2900, 2900));
    }

    #[test]
    fn in_heat_cool_the_heating_and_cooling_setpoints_are_the_range() {
        let n = nest(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "range", "target_temperature_low" => 19.0,
            "target_temperature_high" => 24.0, "can_heat" => true, "can_cool" => true},
        );
        let range = |n: &Nest| (n.read(1, THERMOSTAT, 0x0012), n.read(1, THERMOSTAT, 0x0011));
        let i16s = |low, high| (Some(Value::I16(low)), Some(Value::I16(high)));
        assert_eq!(n.read(1, THERMOSTAT, 0x001C), Some(Value::Enum8(0x01)));
        assert_eq!(range(&n), i16s(1900, 2400));
        assert_eq!(n.write(1, THERMOSTAT, 0x0012, Value::I16(2000)), status::SUCCESS);
        assert_eq!(n.write(1, THERMOSTAT, 0x0011, Value::I16(2550)), status::SUCCESS);
        assert_eq!(range(&n), i16s(2000, 2550));
        // setpointRaiseLower: heat down 1 °C, both up 0.5 °C, cool up 0.5 °C.
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x00, (-10i8) as u8]), status::SUCCESS.into());
        assert_eq!(range(&n), i16s(1900, 2550));
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x02, 5]), status::SUCCESS.into());
        assert_eq!(range(&n), i16s(1950, 2600));
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x01, 5]), status::SUCCESS.into());
        assert_eq!(range(&n), i16s(1950, 2650));
        // Its amount missing.
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x02]), status::INVALID_FIELD.into());
        assert_eq!(range(&n), i16s(1950, 2650));
    }

    #[test]
    fn the_fan_and_the_analog_outputs_take_what_mqtt_takes() {
        let n = nest(fields! {"has_fan" => true, "has_humidifier" => true}, fields! {"can_heat" => true});
        assert_eq!(n.read(1, FAN, 0x0000), Some(Value::Enum8(0x05)));
        assert_eq!(n.write(1, FAN, 0x0000, Value::Enum8(0x04)), status::SUCCESS);
        assert_eq!(n.read(1, FAN, 0x0000), Some(Value::Enum8(0x04)));
        assert_eq!(n.write(1, FAN, 0x0000, Value::Enum8(0x05)), status::SUCCESS);
        assert_eq!(n.read(1, FAN, 0x0000), Some(Value::Enum8(0x05)));
        assert_eq!(n.write(1, FAN, 0x0000, Value::Enum8(0x02)), status::INVALID_VALUE);
        // The fan timer, whole minutes, 15-1440.
        assert_eq!(n.write(3, ANALOG_OUTPUT, 0x0055, Value::F32(10.0)), status::INVALID_VALUE);
        assert_eq!(n.write(3, ANALOG_OUTPUT, 0x0055, Value::F32(29.6)), status::SUCCESS);
        assert_eq!(n.read(3, ANALOG_OUTPUT, 0x0055), Some(Value::F32(30.0)));
        assert_eq!(n.read(3, ANALOG_OUTPUT, 0x0075), Some(Value::Enum16(MINUTES)));
        // The target humidity, whole percent, 10-60.
        assert_eq!(n.read(4, ANALOG_OUTPUT, 0x0055), Some(Value::F32(35.0)));
        assert_eq!(n.write(4, ANALOG_OUTPUT, 0x0055, Value::F32(40.4)), status::SUCCESS);
        assert_eq!(n.read(4, ANALOG_OUTPUT, 0x0055), Some(Value::F32(40.0)));
        assert_eq!(n.write(4, ANALOG_OUTPUT, 0x0055, Value::F32(70.0)), status::INVALID_VALUE);
        assert_eq!(n.read(4, ANALOG_OUTPUT, 0x0075), Some(Value::Enum16(PERCENT)));
        assert_eq!(n.write(4, ANALOG_OUTPUT, 0x0041, Value::F32(90.0)), status::READ_ONLY);
    }

    #[test]
    fn the_fan_schedule_is_a_switch_and_three_analog_outputs() {
        let n = nest(
            fields! {"has_fan" => true, "fan_mode" => "auto", "fan_duty_cycle" => 3600,
            "fan_duty_start_time" => 0, "fan_duty_end_time" => 0},
            fields! {},
        );
        assert_eq!(n.read(29, BINARY_OUTPUT, 0x001C), Some(Value::Str("Fan schedule".into())));
        assert_eq!(n.read(29, BINARY_OUTPUT, 0x0055), Some(Value::Bool(false)));
        assert_eq!(n.write(29, BINARY_OUTPUT, 0x0055, Value::Bool(true)), status::SUCCESS);
        assert_eq!(n.view().dev["fan_mode"], "duty-cycle");
        assert_eq!(n.read(29, BINARY_OUTPUT, 0x0055), Some(Value::Bool(true)));
        // Run time: 15, 30, 45 or 60 minutes per hour.
        assert_eq!(n.read(7, ANALOG_OUTPUT, 0x0055), Some(Value::F32(60.0)));
        assert_eq!(n.write(7, ANALOG_OUTPUT, 0x0055, Value::F32(30.0)), status::SUCCESS);
        assert_eq!(n.view().dev["fan_duty_cycle"], 1800);
        assert_eq!(n.write(7, ANALOG_OUTPUT, 0x0055, Value::F32(20.0)), status::INVALID_VALUE);
        // Start and end, hours of the day.
        assert_eq!(n.write(8, ANALOG_OUTPUT, 0x0055, Value::F32(7.0)), status::SUCCESS);
        assert_eq!(n.write(9, ANALOG_OUTPUT, 0x0055, Value::F32(23.0)), status::SUCCESS);
        assert_eq!(n.write(9, ANALOG_OUTPUT, 0x0055, Value::F32(24.0)), status::INVALID_VALUE);
        assert_eq!(
            (n.read(8, ANALOG_OUTPUT, 0x0055), n.read(9, ANALOG_OUTPUT, 0x0055)),
            (Some(Value::F32(7.0)), Some(Value::F32(23.0)))
        );
        assert_eq!(
            (&n.view().dev["fan_duty_start_time"], &n.view().dev["fan_duty_end_time"]),
            (&serde_json::json!(25_200), &serde_json::json!(82_800))
        );
        assert_eq!(n.read(8, ANALOG_OUTPUT, 0x0075), Some(Value::Enum16(HOURS)));

        let without = nest(fields! {}, fields! {});
        assert!([7, 8, 9].iter().all(|&id| without.attributes(id, ANALOG_OUTPUT).is_none()));
        assert!(without.attributes(29, BINARY_OUTPUT).is_none());
    }

    #[test]
    fn the_screen_timeout_is_an_analog_output_and_always_on_a_switch() {
        let n = nest(fields! {}, fields! {});
        assert_eq!(n.read(40, ANALOG_OUTPUT, 0x001C), Some(Value::Str("Screen timeout".into())));
        assert_eq!(n.read(40, ANALOG_OUTPUT, 0x0055), Some(Value::F32(30.0)), "nlclient's own until set");
        assert_eq!(
            (n.read(40, ANALOG_OUTPUT, 0x0045), n.read(40, ANALOG_OUTPUT, 0x0041)),
            (Some(Value::F32(10.0)), Some(Value::F32(600.0)))
        );
        assert_eq!(n.read(40, ANALOG_OUTPUT, 0x0075), Some(Value::Enum16(SECONDS)));
        assert_eq!(n.write(40, ANALOG_OUTPUT, 0x0055, Value::F32(119.6)), status::SUCCESS);
        assert_eq!(n.read(40, ANALOG_OUTPUT, 0x0055), Some(Value::F32(120.0)));
        assert_eq!(n.view().dev[crate::screen::TIMEOUT], 120);
        for bad in [5.0, 601.0] {
            assert_eq!(n.write(40, ANALOG_OUTPUT, 0x0055, Value::F32(bad)), status::INVALID_VALUE, "{bad}");
        }
        assert_eq!(n.read(41, BINARY_OUTPUT, 0x001C), Some(Value::Str("Screen always on".into())));
        assert_eq!(n.read(41, BINARY_OUTPUT, 0x0055), Some(Value::Bool(false)));
        assert_eq!(n.write(41, BINARY_OUTPUT, 0x0055, Value::Bool(true)), status::SUCCESS);
        assert_eq!(n.read(41, BINARY_OUTPUT, 0x0055), Some(Value::Bool(true)));
        assert_eq!(crate::screen::milliseconds(&n.view().dev), Some(crate::screen::ALWAYS_ON_MS));
        assert!(n.reported().contains(&(40, ANALOG_OUTPUT, &[0x0055][..])));
        assert!(n.reported().contains(&(41, BINARY_OUTPUT, &[0x0055][..])));
    }

    #[test]
    fn the_screen_brightness_is_an_analog_output_and_auto_a_switch() {
        let n = nest(fields! {}, fields! {});
        assert_eq!(n.read(42, ANALOG_OUTPUT, 0x001C), Some(Value::Str("Screen brightness".into())));
        assert_eq!(
            (n.read(42, ANALOG_OUTPUT, 0x0045), n.read(42, ANALOG_OUTPUT, 0x0041)),
            (Some(Value::F32(0.0)), Some(Value::F32(100.0)))
        );
        assert_eq!(n.read(42, ANALOG_OUTPUT, 0x0075), Some(Value::Enum16(PERCENT)));
        assert_eq!(n.read(43, BINARY_OUTPUT, 0x001C), Some(Value::Str("Screen brightness auto".into())));
        assert_eq!(n.read(43, BINARY_OUTPUT, 0x0055), Some(Value::Bool(false)));
        assert_eq!(n.write(42, ANALOG_OUTPUT, 0x0055, Value::F32(39.6)), status::SUCCESS);
        assert_eq!(n.read(42, ANALOG_OUTPUT, 0x0055), Some(Value::F32(40.0)));
        assert_eq!(crate::brightness::request(&n.view().dev).as_deref(), Some("40"));
        for bad in [-1.0, 101.0] {
            assert_eq!(n.write(42, ANALOG_OUTPUT, 0x0055, Value::F32(bad)), status::INVALID_VALUE, "{bad}");
        }
        assert_eq!(n.write(43, BINARY_OUTPUT, 0x0055, Value::Bool(true)), status::SUCCESS);
        assert_eq!(n.read(43, BINARY_OUTPUT, 0x0055), Some(Value::Bool(true)));
        assert_eq!(n.read(42, ANALOG_OUTPUT, 0x0055), Some(Value::F32(40.0)), "the manual percentage, kept");
        assert_eq!(crate::brightness::request(&n.view().dev).as_deref(), Some("auto"));
        // What the unit shows wins, as its state file reports it.
        n.bridge.brightness.set_shown(Some(crate::brightness::Shown::Percent(70)));
        assert_eq!(n.read(43, BINARY_OUTPUT, 0x0055), Some(Value::Bool(false)));
        assert_eq!(n.read(42, ANALOG_OUTPUT, 0x0055), Some(Value::F32(70.0)));
        assert!(n.reported().contains(&(42, ANALOG_OUTPUT, &[0x0055][..])));
        assert!(n.reported().contains(&(43, BINARY_OUTPUT, &[0x0055][..])));
    }

    #[test]
    fn the_hot_water_schedule_comes_with_hot_water_control() {
        assert!(!nest(fields! {}, fields! {"can_heat" => true}).has(1, HOT_WATER));
        let n = nest(fields! {"has_hot_water_control" => true}, fields! {"can_heat" => true});
        assert!(n.reported().contains(&(1, HOT_WATER, HOT_WATER_ATTRIBUTES)));
        let interval = serde_json::json!({"start_day": 1, "start_time": 21600, "end_day": 1, "end_time": 28800});
        let hw = fields! {"mode" => "hotwater", "intervals" => serde_json::json!([interval])};
        n.bridge.store.merge("custom_schedule.hotwater", &hw, 0, 0, true);
        // Read Tuesday (0x0001), as a manufacturer-specific frame.
        assert_eq!(
            serve(&n, 1, HOT_WATER, &[0x04, 0xD6, 0x10, 1, 0x00, 0x01, 0x00]).unwrap(),
            [&[0x1C, 0xD6, 0x10, 1, 0x01, 0x01, 0x00, 0x00, 0x42, 11][..], b"06:00-08:00"].concat()
        );
        // Write Sunday: 23:00 until 01:00 Monday.
        let mut write = vec![0x04, 0xD6, 0x10, 2, 0x02, 0x06, 0x00, 0x42, 11];
        write.extend(b"23:00-01:00");
        assert_eq!(serve(&n, 1, HOT_WATER, &write).unwrap(), [0x1C, 0xD6, 0x10, 2, 0x04, 0x00]);
        let hw = n.bridge.store.get("custom_schedule.hotwater").unwrap().value;
        assert_eq!((hotwater::day(&hw, 6), hotwater::day(&hw, 1)), (vec![(1380, 60)], vec![(360, 480)]));
        assert_eq!(n.write(1, HOT_WATER, 0x0000, Value::Str("6-8".into())), status::INVALID_VALUE);
    }

    #[test]
    fn a_day_sent_back_keeps_a_setpoint_it_holds_outside_the_limits() {
        let n = nest(fields! {}, fields! {"can_heat" => true});
        let key = format!("schedule.{}", n.bridge.serial);
        let frost = serde_json::json!({"entry_type": "setpoint", "time": 3600, "temp": 6, "type": "HEAT"});
        let days = serde_json::json!({"0": {"1": frost}});
        n.bridge.store.merge(&key, &fields! {"schedule_mode" => "HEAT", "days" => days, "ver" => 2}, 0, 0, true);
        // Monday: 01:00 6 °C as it holds, and 07:00 20 °C beside it.
        let edit = [2, 0x02, 0x01, 0x3C, 0x00, 0x58, 0x02, 0xA4, 0x01, 0xD0, 0x07];
        assert_eq!(n.command(1, THERMOSTAT, 0x01, &edit), status::SUCCESS.into());
        assert_eq!(schedule::day(&n.bridge.store.get(&key).unwrap().value, 0), [(60, 6.0), (420, 20.0)]);
        // 6 °C at another time, or on a day that does not hold it, is refused.
        assert_eq!(
            n.command(1, THERMOSTAT, 0x01, &[1, 0x02, 0x01, 0x78, 0x00, 0x58, 0x02]),
            status::INVALID_VALUE.into()
        );
        assert_eq!(
            n.command(1, THERMOSTAT, 0x01, &[1, 0x04, 0x01, 0x3C, 0x00, 0x58, 0x02]),
            status::INVALID_VALUE.into()
        );
        assert_eq!(
            n.command(1, THERMOSTAT, 0x01, &[1, 0x06, 0x01, 0x3C, 0x00, 0x58, 0x02]),
            status::INVALID_VALUE.into()
        );
    }

    #[test]
    fn the_weekly_schedule_is_the_units() {
        let n = nest(fields! {"learning_mode" => true}, fields! {"can_heat" => true});
        // Get Weekly Schedule, Monday and Tuesday, heat: none yet.
        assert_eq!(serve(&n, 1, THERMOSTAT, &[0x01, 1, 0x02, 0x06, 0x01]).unwrap(), [0x18, 1, 0x0B, 0x02, 0x8B]);
        let key = format!("schedule.{}", n.bridge.serial);
        n.bridge.store.merge(
            &key,
            &fields! {"schedule_mode" => "HEAT", "days" => serde_json::json!({}), "ver" => 2},
            0,
            0,
            true,
        );
        // Set: Monday and Saturday, heat and cool, 06:30 20.5 °C, 22:00 17 °C.
        let set =
            [0x01, 2, 0x01, 2, 0x42, 0x03, 0x86, 0x01, 0x02, 0x08, 0x60, 0x09, 0x28, 0x05, 0xA4, 0x06, 0x60, 0x09];
        assert!(serve(&n, 1, THERMOSTAT, &set).is_some_and(|out| out == [0x18, 2, 0x0B, 0x01, 0x00]));
        let bucket = n.bridge.store.get(&key).unwrap().value;
        assert_eq!(schedule::day(&bucket, 0), [(390, 20.5), (1320, 17.0)]);
        assert_eq!(schedule::day(&bucket, 5), [(390, 20.5), (1320, 17.0)]);
        assert_eq!(schedule::day(&bucket, 1), [(0, 17.0)]);
        assert_eq!(n.view().dev["learning_mode"], serde_json::json!(true));
        // Get, Monday and Tuesday: a response each, heating setpoints.
        assert_eq!(
            serve_all(&n, 1, THERMOSTAT, &[0x01, 3, 0x02, 0x06, 0x03]),
            [
                vec![0x19, 3, 0x00, 2, 0x02, 0x01, 0x86, 0x01, 0x02, 0x08, 0x28, 0x05, 0xA4, 0x06],
                vec![0x19, 3, 0x00, 1, 0x04, 0x01, 0, 0, 0xa4, 0x06],
            ]
        );
        // The whole week: the days with the same transitions together.
        let week = serve_all(&n, 1, THERMOSTAT, &[0x01, 4, 0x02, 0x7F, 0x01]);
        assert_eq!(week.iter().map(|f| f[4]).collect::<Vec<_>>(), [0x01 | 0x1C | 0x20, 0x02 | 0x40]);
        assert_eq!(n.command(1, THERMOSTAT, 0x02, &[0x01, 0x02]), status::NOT_FOUND.into(), "no cooling schedule");
        // Zigbee2MQTT's (the converter's): Monday, both setpoints, no default response.
        let z2m =
            [0x11, 9, 0x01, 2, 0x02, 0x03, 0x86, 0x01, 0x02, 0x08, 0x02, 0x08, 0x28, 0x05, 0xA4, 0x06, 0xA4, 0x06];
        assert_eq!(serve(&n, 1, THERMOSTAT, &z2m), None);
        assert_eq!(schedule::day(&n.bridge.store.get(&key).unwrap().value, 0), [(390, 20.5), (1320, 17.0)]);
        // A wrong length, no day, a time past midnight, a temperature the unit refuses.
        for bad in [
            &[1, 0x02, 0x01, 0x00][..],
            &[0, 0x80, 0x01],
            &[0, 0x82, 0x01],
            &[1, 0x02, 0x01, 0xA0, 0x05, 0x08, 0x07],
            &[1, 0x02, 0x01, 0x00, 0x00, 0x00, 0x10],
            &[1, 0x02, 0x01, 0x00, 0x00, 0x20, 0x03], // 8 °C, under the advertised 9 °C
        ] {
            assert_ne!(n.command(1, THERMOSTAT, 0x01, bad), status::SUCCESS.into(), "{bad:02x?}");
        }
        let mut full = vec![11, 0x02, 0x01];
        full.resize(3 + 11 * 4, 0);
        assert_eq!(n.command(1, THERMOSTAT, 0x01, &full), status::INSUFFICIENT_SPACE.into());
        assert_eq!(n.read(1, THERMOSTAT, 0x0022), Some(Value::U8(10)));
    }

    #[test]
    fn the_week_follows_the_schedule_whoever_changes_it() {
        let n = nest(fields! {}, fields! {"can_cool" => true});
        assert!(n.week().is_empty(), "no schedule yet");
        let key = format!("schedule.{}", n.bridge.serial);
        let day = |time: u32, temp: f64| serde_json::json!({"0": {"entry_type": "setpoint", "time": time, "temp": temp, "type": "COOL"}});
        let week = |monday| {
            serde_json::json!({"0": monday, "1": day(23400, 24.0), "2": day(23400, 24.0),
            "3": day(23400, 24.0), "4": day(23400, 24.0), "5": day(23400, 24.0), "6": day(23400, 24.0)})
        };
        let bucket = |monday| fields! {"schedule_mode" => "COOL", "days" => week(monday), "ver" => 2};
        n.bridge.store.merge(&key, &bucket(day(23400, 24.0)), 0, 0, true);
        // Every day alike: one response, all seven days, cooling setpoints, 06:30 24 °C.
        assert_eq!(n.week(), [vec![1, 0x7F, 0x02, 0x86, 0x01, 0x60, 0x09]]);
        // Monday moved to 09:15, as MQTT or Matter would: Monday apart.
        n.bridge.store.merge(&key, &bucket(day(33300, 24.0)), 0, 0, true);
        assert_eq!(
            n.week(),
            [vec![1, 0x7D, 0x02, 0x86, 0x01, 0x60, 0x09], vec![1, 0x02, 0x02, 0x2B, 0x02, 0x60, 0x09]]
        );
        // Sent unasked, as the cluster's command 0x00 to the client.
        assert_eq!(super::super::zcl::server_command_out(7, 0x00, &n.week()[0])[..4], [0x19, 7, 0x00, 1]);
    }

    #[test]
    fn hot_water_reported_later_grows_the_endpoints_and_going_keeps_them() {
        let n = nest(fields! {}, fields! {"can_heat" => true});
        assert_eq!(n.grown(), None, "nothing new");
        let device = format!("device.{}", n.bridge.serial);
        n.bridge.store.merge(&device, &fields! {"has_hot_water_control" => true}, 0, 0, true);
        let grown = n.grown().expect("hot water adds endpoints");
        assert!(grown.iter().any(|e| e.id == THERMOSTAT_ENDPOINT && e.clusters.contains(&HOT_WATER)));
        assert!(grown.iter().any(|e| e.id == 17) && grown.iter().any(|e| e.id == 18));
        let mut with = nest(fields! {"has_hot_water_control" => true}, fields! {"can_heat" => true});
        with.endpoints = grown;
        let device = format!("device.{}", with.bridge.serial);
        with.bridge.store.merge(&device, &fields! {"has_hot_water_control" => false}, 0, 0, true);
        assert_eq!(with.grown(), None, "hot water gone keeps its endpoints");
    }

    #[test]
    fn commands_the_endpoint_does_not_have_are_refused() {
        let n = nest(fields! {}, fields! {"can_heat" => true});
        assert_eq!(n.command(10, BINARY_OUTPUT, 0x01, &[]), status::UNSUP_CLUSTER_COMMAND.into());
        assert_eq!(n.write(20, BINARY_INPUT, 0x0055, Value::Bool(true)), status::READ_ONLY); // a state, not a switch
        assert_eq!(n.write(20, BINARY_OUTPUT, 0x0055, Value::Bool(true)), status::READ_ONLY);
        assert_eq!(n.command(1, THERMOSTAT, 0x04, &[]), status::UNSUP_CLUSTER_COMMAND.into());
        assert_eq!(n.write(1, BASIC, 0x0005, Value::Str("x".into())), status::READ_ONLY);
    }

    #[test]
    fn a_range_schedule_carries_both_setpoints() {
        let n = nest(fields! {}, fields! {"can_heat" => true, "can_cool" => true});
        let key = format!("schedule.{}", n.bridge.serial);
        let entry = serde_json::json!({"entry_type": "setpoint", "time": 10800, "temp-min": 18, "temp-max": 25, "type": "RANGE"});
        let days = serde_json::json!({"0": {"1": entry}});
        n.bridge.store.merge(&key, &fields! {"schedule_mode" => "RANGE", "days" => days, "ver" => 2}, 0, 0, true);
        // Get, Monday: 03:00, heating 18 °C (the low), cooling 25 °C (the high).
        assert_eq!(
            serve(&n, 1, THERMOSTAT, &[0x01, 1, 0x02, 0x02, 0x03]).unwrap(),
            [0x19, 1, 0x00, 1, 0x02, 0x03, 0xB4, 0x00, 0x08, 0x07, 0xC4, 0x09]
        );
        // Get, Monday, heating alone: the lows.
        assert_eq!(
            serve(&n, 1, THERMOSTAT, &[0x01, 5, 0x02, 0x02, 0x01]).unwrap(),
            [0x19, 5, 0x00, 1, 0x02, 0x01, 0xB4, 0x00, 0x08, 0x07]
        );
        // Set, Tuesday: 06:30, 19-24 °C.
        let set = [0x01, 2, 0x01, 1, 0x04, 0x03, 0x86, 0x01, 0x6C, 0x07, 0x60, 0x09];
        assert_eq!(serve(&n, 1, THERMOSTAT, &set).unwrap(), [0x18, 2, 0x0B, 0x01, 0x00]);
        assert_eq!(schedule::range_day(&n.bridge.store.get(&key).unwrap().value, 1), [(390, 19.0, 24.0)]);
        // One setpoint alone, or a high not the unit's gap above the low, is refused.
        assert_eq!(
            n.command(1, THERMOSTAT, 0x01, &[1, 0x04, 0x01, 0x86, 0x01, 0x6C, 0x07]),
            status::INVALID_FIELD.into()
        );
        let close = [1, 0x04, 0x03, 0x86, 0x01, 0xD0, 0x07, 0x34, 0x08];
        assert_eq!(n.command(1, THERMOSTAT, 0x01, &close), status::INVALID_VALUE.into());
    }

    #[test]
    fn zcl_frames_reach_the_unit() {
        let n = nest(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "heat", "target_temperature" => 20.0, "can_heat" => true},
        );
        // Write the heating setpoint, 21.5 °C, then as an enum8.
        let out = serve(&n, 1, THERMOSTAT, &[0x00, 1, 0x02, 0x12, 0x00, 0x29, 0x66, 0x08]).unwrap();
        assert_eq!(out, [0x18, 1, 0x04, 0x00]);
        assert_eq!(n.read(1, THERMOSTAT, 0x0012), Some(Value::I16(2150)));
        let out = serve(&n, 1, THERMOSTAT, &[0x00, 2, 0x02, 0x12, 0x00, 0x30, 0x01]).unwrap();
        assert_eq!(out, [0x18, 2, 0x04, 0x8D, 0x12, 0x00]);
        // The thermostat's endpoint has no Binary Output.
        let out = serve(&n, 1, BINARY_OUTPUT, &[0x01, 3, 0x01]).unwrap();
        assert_eq!(out, [0x18, 3, 0x0B, 0x01, 0xC3]);
    }

    #[test]
    fn writes_are_commands() {
        let n = nest(
            fields! {"temperature_scale" => "C", "has_fan" => true, "fan_timer_duration" => 900},
            fields! {"target_temperature_type" => "heat", "target_temperature" => 20.0},
        );
        assert_eq!(n.write(1, THERMOSTAT, 0x0012, Value::I16(2150)), status::SUCCESS);
        assert_eq!(n.read(1, THERMOSTAT, 0x0012), Some(Value::I16(2150)));
        assert_eq!(n.write(1, THERMOSTAT, 0x001C, Value::Enum8(0x03)), status::SUCCESS);
        assert_eq!(n.read(1, THERMOSTAT, 0x001C), Some(Value::Enum8(0x03)));
        assert_eq!(n.write(1, THERMOSTAT, 0x001C, Value::Enum8(0x07)), status::INVALID_VALUE);
        assert_eq!(n.write(1, THERMOSTAT, 0x0000, Value::I16(0)), status::READ_ONLY);
        // Half degrees, as the unit has them; out of range is refused.
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x00, 5]), status::SUCCESS.into());
        assert_eq!(n.write(1, THERMOSTAT, 0x0011, Value::I16(9000)), status::INVALID_VALUE);
        // Eco, the fan timer, a switch.
        assert_eq!(n.write(1, THERMOSTAT, 0x0025, Value::Bitmap8(ECONOMY)), status::SUCCESS);
        assert_eq!(n.read(1, THERMOSTAT, 0x0025), Some(Value::Bitmap8(ECONOMY)));
        assert_eq!(n.read(3, ANALOG_OUTPUT, 0x0055), Some(Value::F32(15.0)));
        assert_eq!(n.write(3, ANALOG_OUTPUT, 0x0055, Value::F32(60.0)), status::SUCCESS);
        assert_eq!(n.read(3, ANALOG_OUTPUT, 0x0055), Some(Value::F32(60.0)));
        assert_eq!(n.write(11, BINARY_OUTPUT, 0x0055, Value::Bool(true)), status::SUCCESS);
        assert_eq!(n.read(11, BINARY_OUTPUT, 0x0055), Some(Value::Bool(true)));
        assert_eq!(n.write(11, BINARY_OUTPUT, 0x0055, Value::Bool(false)), status::SUCCESS);
        assert_eq!(n.read(11, BINARY_OUTPUT, 0x0055), Some(Value::Bool(false)));
        assert_eq!(n.write(11, BINARY_OUTPUT, 0x001C, Value::Str("x".into())), status::READ_ONLY);
    }

    #[test]
    fn switches_and_states_tell_their_names() {
        let n = nest(fields! {"has_humidifier" => true}, fields! {"can_heat" => true, "can_cool" => true});
        let names =
            |cluster, ids: &[u8]| -> Vec<Option<Value>> { ids.iter().map(|&id| n.read(id, cluster, 0x001C)).collect() };
        let text = |names: &[&str]| -> Vec<Option<Value>> {
            names.iter().map(|name| Some(Value::Str((*name).into()))).collect()
        };
        assert_eq!(
            names(BINARY_OUTPUT, &[10, 11, 12, 13, 14, 15, 16]),
            text(&[
                "Auto-Schedule",
                "Home/Away Assist",
                "Sunblock",
                "Early-On",
                "Eco heat limit",
                "Eco cool limit",
                "Humidity control"
            ])
        );
        assert_eq!(
            names(BINARY_INPUT, &[20, 21, 22, 23, 26]),
            text(&["Leaf", "Early-On active", "Sunblock active", "Safety shutoff", "Humidifier"])
        );
        assert!(SWITCHES.iter().all(|(id, _, _)| n.attributes(*id, 0x0006).is_none()), "no On/Off beside them");
    }

    #[test]
    fn hot_water_endpoints_come_with_its_control_and_drive_its_fields() {
        let without = nest(fields! {}, fields! {});
        assert!(without.attributes(17, BINARY_OUTPUT).is_none());
        assert!(without.attributes(18, BINARY_OUTPUT).is_none());

        let n = nest(fields! {"has_hot_water_control" => true}, fields! {});
        assert_eq!(n.read(17, BINARY_OUTPUT, 0x001C), Some(Value::Str("Hot water boost".into())));
        assert_eq!(n.read(17, BINARY_OUTPUT, 0x0055), Some(Value::Bool(false)));
        assert_eq!(n.write(17, BINARY_OUTPUT, 0x0055, Value::Bool(true)), status::SUCCESS);
        assert_eq!(n.read(17, BINARY_OUTPUT, 0x0055), Some(Value::Bool(true)));
        assert_eq!(n.write(17, BINARY_OUTPUT, 0x0055, Value::Bool(false)), status::SUCCESS);
        assert_eq!(n.read(17, BINARY_OUTPUT, 0x0055), Some(Value::Bool(false)));

        assert_eq!(n.read(18, BINARY_OUTPUT, 0x0055), Some(Value::Bool(true)));
        assert_eq!(n.write(18, BINARY_OUTPUT, 0x0055, Value::Bool(false)), status::SUCCESS);
        assert_eq!(n.read(18, BINARY_OUTPUT, 0x0055), Some(Value::Bool(false)));
        assert_eq!(n.view().dev["hot_water_mode"], "off");
    }

    #[test]
    fn a_write_attributes_frame_flips_a_switch() {
        let n = nest(fields! {}, fields! {"can_heat" => true});
        let out = serve(&n, 11, BINARY_OUTPUT, &[0x00, 1, 0x02, 0x55, 0x00, 0x10, 0x01]).unwrap();
        assert_eq!(out, [0x18, 1, 0x04, 0x00]);
        assert_eq!(n.read(11, BINARY_OUTPUT, 0x0055), Some(Value::Bool(true)));
    }

    #[test]
    fn setpoints_outside_the_advertised_limits_are_refused() {
        let n = nest(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "heat", "target_temperature" => 20.0, "can_heat" => true},
        );
        assert_eq!(n.read(1, THERMOSTAT, 0x0015), Some(Value::I16(MIN)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0016), Some(Value::I16(MAX)));
        for t in [450, 899, 3201, 3500] {
            assert_eq!(n.write(1, THERMOSTAT, 0x0012, Value::I16(t)), status::INVALID_VALUE, "{t}");
            assert_eq!(n.write(1, THERMOSTAT, 0x0011, Value::I16(t)), status::INVALID_VALUE, "{t}");
        }
        assert_eq!(n.read(1, THERMOSTAT, 0x0012), Some(Value::I16(2000)));
        assert_eq!(n.write(1, THERMOSTAT, 0x0012, Value::I16(MIN)), status::SUCCESS);
        assert_eq!(n.write(1, THERMOSTAT, 0x0012, Value::I16(MAX)), status::SUCCESS);
        // Per record, in a write frame: 4.5 °C fails, the rest is not stopped.
        let out = serve(&n, 1, THERMOSTAT, &[0x00, 1, 0x02, 0x12, 0x00, 0x29, 0xC2, 0x01]).unwrap();
        assert_eq!(out, [0x18, 1, 0x04, 0x87, 0x12, 0x00]);
    }

    #[test]
    fn identifying_lights_the_display_and_stopping_does_nothing() {
        let n = nest(fields! {"temperature_scale" => "C"}, fields! {"can_heat" => true});
        let (mut rx, _sub) = n.bridge.store.subscribe();
        let device = format!("device.{}", n.bridge.serial);
        let resent = |rx: &mut tokio::sync::mpsc::Receiver<Vec<crate::store::Object>>| {
            std::iter::from_fn(|| rx.try_recv().ok()).flatten().filter(|o| o.key == device).count()
        };
        assert_eq!(n.command(1, IDENTIFY, 0x00, &[0, 0]), status::SUCCESS.into());
        assert_eq!(n.write(1, IDENTIFY, 0x0000, Value::U16(0)), status::SUCCESS);
        assert_eq!(resent(&mut rx), 0);
        assert_eq!(n.command(1, IDENTIFY, 0x00, &[5, 0]), status::SUCCESS.into());
        assert_eq!(n.write(1, IDENTIFY, 0x0000, Value::U16(5)), status::SUCCESS);
        assert_eq!(resent(&mut rx), 2);
        assert_eq!(n.command(1, IDENTIFY, 0x00, &[5]), status::INVALID_FIELD.into());
        assert_eq!(n.read(1, IDENTIFY, 0x0000), Some(Value::U16(0)));
    }

    #[test]
    fn raise_lower_stays_within_the_limits_and_follows_its_mode() {
        let n = nest(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "heat", "target_temperature" => 31.0, "can_heat" => true},
        );
        let heating = |n: &Nest| n.read(1, THERMOSTAT, 0x0012);
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x00, 50]), status::SUCCESS.into());
        assert_eq!(heating(&n), Some(Value::I16(MAX)));
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x02, (-128i8) as u8]), status::SUCCESS.into());
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x02, (-128i8) as u8]), status::SUCCESS.into());
        assert_eq!(heating(&n), Some(Value::I16(MIN)));
        // Cool alone leaves a heating unit's setpoint; no such mode as 3.
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x01, 20]), status::SUCCESS.into());
        assert_eq!(heating(&n), Some(Value::I16(MIN)));
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x03, 20]), status::INVALID_FIELD.into());
        assert_eq!(heating(&n), Some(Value::I16(MIN)));
    }

    #[test]
    fn an_external_temperature_written_over_zigbee_is_the_sensors_reading() {
        let n = nest(fields! {}, fields! {});
        assert_eq!(n.read(1, WEATHER, EXTERNAL_TEMPERATURE), Some(Value::I16(NONE)));
        assert_ne!(
            n.write(1, WEATHER, EXTERNAL_TEMPERATURE, Value::I16(2150)),
            status::SUCCESS,
            "no -external-temperature"
        );
        let dir = std::env::temp_dir().join(format!("nestlocal-zb-sensor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sensor = crate::sensor::Sensor::open(
            &dir.join("sensor.json"),
            "S",
            &dir.join("none"),
            std::time::Duration::from_secs(1800),
        )
        .unwrap();
        n.bridge.attach_sensor(sensor.clone());
        assert_eq!(n.write(1, WEATHER, EXTERNAL_TEMPERATURE, Value::I16(2150)), status::SUCCESS);
        assert_eq!(sensor.reading(), Some(21.5));
        assert_eq!(n.read(1, WEATHER, EXTERNAL_TEMPERATURE), Some(Value::I16(2150)));
        assert_ne!(n.write(1, WEATHER, EXTERNAL_TEMPERATURE, Value::I16(9000)), status::SUCCESS, "out of range");
    }

    #[test]
    fn the_external_temperature_has_an_endpoint_saying_whether_it_regulates() {
        let without = nest(fields! {}, fields! {});
        assert!(without.attributes(TEMPERATURE_SOURCE_ENDPOINT, BINARY_INPUT).is_none());

        let bridge = crate::bridge::tests::test_bridge();
        bridge.store.merge(&format!("link.{}", bridge.serial), &fields! {"structure" => "structure.home"}, 0, 0, false);
        let dir = std::env::temp_dir().join(format!("nestlocal-zb-source-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sensor = crate::sensor::Sensor::open(
            &dir.join("sensor.json"),
            &bridge.serial,
            &dir.join("none"),
            std::time::Duration::from_secs(1800),
        )
        .unwrap();
        bridge.attach_sensor(sensor.clone());
        crate::sensor::ensure_buckets(&bridge.store, &bridge.serial, &sensor.bucket, None);
        let n = Nest::new(bridge);
        let source = || n.read(TEMPERATURE_SOURCE_ENDPOINT, BINARY_INPUT, 0x0055);
        assert!(n.reported().contains(&(TEMPERATURE_SOURCE_ENDPOINT, BINARY_INPUT, &[0x0055][..])));
        assert_eq!(source(), Some(Value::Bool(false)), "no reading yet");
        assert_eq!(n.write(1, WEATHER, EXTERNAL_TEMPERATURE, Value::I16(2100)), status::SUCCESS);
        sensor.due().unwrap();
        assert_eq!(source(), Some(Value::Bool(true)));
        n.bridge.store.merge(
            &format!("rcs_settings.{}", n.bridge.serial),
            &fields! {"active_rcs_sensors" => Vec::<Json>::new()},
            0,
            0,
            true,
        );
        assert_eq!(source(), Some(Value::Bool(false)), "the backplate is chosen");
        assert_eq!(n.write(TEMPERATURE_SOURCE_ENDPOINT, BINARY_INPUT, 0x0055, Value::Bool(true)), status::READ_ONLY);
    }

    #[test]
    fn weather_written_over_zigbee_is_what_mqtt_gives() {
        use serde_json::json;
        let n = nest(fields! {}, fields! {});
        let answer = || n.bridge.outdoor.answer().map(Json::Object);
        // The readings: as the thermostat's outdoor temperature too.
        assert_eq!(n.write(1, WEATHER, OUTDOOR_TEMPERATURE, Value::I16(-340)), status::SUCCESS);
        assert_eq!(n.read(1, THERMOSTAT, 0x0001), Some(Value::I16(-340)));
        assert_eq!(answer(), None, "an answer without humidity");
        assert_eq!(n.write(1, WEATHER, OUTDOOR_HUMIDITY, Value::U8(82)), status::SUCCESS);
        assert_eq!(answer(), Some(crate::json::canonical(json!({"current": {"temp_c": -3.4, "humidity": 82.0}}))));
        // Written over the air, with the manufacturer code: the conditions,
        // the first hour and the first day (1790679723 is 2026-09-29 11:02:03 UTC).
        let utc = |unix: u32| (unix - 946_684_800).to_le_bytes();
        let mut frame = vec![0x04, 0xD6, 0x10, 7, 0x02];
        for (id, type_id, value) in [
            (CONDITION, 0x42, b"\x0cpartlycloudy"[..].to_vec()),
            (NEXT_RISING, 0xE2, utc(1790679723 + 86400).to_vec()),
            (NEXT_SETTING, 0xE2, utc(1790722504).to_vec()),
            (HOURLY | SLOT_TIME, 0xE2, utc(1790697600).to_vec()),
            (HOURLY | SLOT_TEMPERATURE, 0x29, 2280i16.to_le_bytes().to_vec()),
            (HOURLY | SLOT_HUMIDITY, 0x20, vec![66]),
            (DAILY | SLOT_TIME, 0xE2, utc(1790694000).to_vec()),
            (DAILY | SLOT_TEMPERATURE, 0x29, 2330i16.to_le_bytes().to_vec()),
            (DAILY | SLOT_LOW, 0x29, 1500i16.to_le_bytes().to_vec()),
        ] {
            frame.extend(id.to_le_bytes());
            frame.push(type_id);
            frame.extend(value);
        }
        assert_eq!(serve(&n, 1, WEATHER, &frame), Some(vec![0x1C, 0xD6, 0x10, 7, 0x04, 0x00]));
        assert_eq!(
            answer(),
            Some(crate::json::canonical(json!({
                "current": {"temp_c": -3.4, "humidity": 82.0, "icon": "partlycloudy", "sunrise": 1790679723_i64, "sunset": 1790722504_i64},
                "forecast": {
                    "hourly": [{"hour": 1, "time": 1790697600_i64, "temp_c": 22.8, "humidity": 66.0}],
                    "daily": [{"date": 1790694000_i64, "temp_high_c": 23.3, "temp_low_c": 15.0}],
                },
            })))
        );
        assert_eq!(n.read(1, WEATHER, CONDITION), Some(Value::Str("partlycloudy".into())));
        assert_eq!(n.read(1, WEATHER, HOURLY | 1 << 4 | SLOT_TIME), Some(Value::Utc(NO_TIME)), "an hour not written");
        // An hour with its time taken away leaves the forecast; a condition
        // the unit has no image for leaves the icon out.
        assert_eq!(n.write(1, WEATHER, HOURLY | SLOT_TIME, Value::Utc(NO_TIME)), status::SUCCESS);
        assert_eq!(n.write(1, WEATHER, CONDITION, Value::Str("windy".into())), status::SUCCESS);
        let a = answer().unwrap();
        assert!(a["forecast"].get("hourly").is_none() && a["current"].get("icon").is_none(), "{a}");
        // Out of range, and the wrong type over the air.
        assert_eq!(n.write(1, WEATHER, OUTDOOR_TEMPERATURE, Value::I16(6100)), status::INVALID_VALUE);
        assert_eq!(n.write(1, WEATHER, DAILY | SLOT_HUMIDITY, Value::U8(101)), status::INVALID_VALUE);
        assert_eq!(
            serve(&n, 1, WEATHER, &[0x04, 0xD6, 0x10, 8, 0x02, 0x01, 0x00, 0x29, 0x00, 0x00]),
            Some(vec![0x1C, 0xD6, 0x10, 8, 0x04, 0x8D, 0x01, 0x00])
        );
        assert_eq!(n.read(1, THERMOSTAT, 0x0001), Some(Value::I16(-340)), "the readings stay");
    }
}
