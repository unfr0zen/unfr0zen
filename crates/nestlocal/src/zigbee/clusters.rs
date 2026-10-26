//! The unit as a Zigbee node, in standard clusters only, read from its
//! buckets and written through the same commands as MQTT's set/ topics
//! (Bridge::apply), as the Matter side does. Its equipment decides which
//! endpoints it has; each has a fixed number, so a converter can name it:
//!
//!   1   the thermostat: Basic, Identify, Power Configuration (the battery),
//!       Thermostat (eco is the economy bit of its programming operation
//!       mode; the eco temperatures are the unoccupied setpoints), Fan
//!       Control (with a fan), Thermostat User Interface Configuration
//!       (°C or °F) and Relative Humidity
//!   2   the backplate's temperature: Temperature Measurement
//!   3   the fan timer's duration, minutes (with a fan): Analog Output
//!   4   the target humidity, % (with a humidifier or dehumidifier): Analog
//!       Output
//!   5   the time to temperature, seconds left: Analog Input
//!   6   the filter's runtime, hours (with an air filter): Analog Input
//!   10+ the unit's switches: On/Off, see SWITCHES
//!   20+ what the unit shows or does: Binary Input, see STATES
//!
//! The Thermostat cluster's Clear Weekly Schedule clears the unit's schedule.
//!
//! ZCL temperatures are hundredths of a degree Celsius; 0x8000 is none.

use std::sync::Arc;

use serde_json::Value as Json;

use crate::bridge::{self, Bridge};
use crate::json::{number, sprint, truthy, Fields};

use super::zcl::{status, Clusters, Value};

pub const BASIC: u16 = 0x0000;
pub const POWER: u16 = 0x0001;
pub const IDENTIFY: u16 = 0x0003;
pub const ON_OFF: u16 = 0x0006;
pub const ANALOG_INPUT: u16 = 0x000C;
pub const ANALOG_OUTPUT: u16 = 0x000D;
pub const BINARY_INPUT: u16 = 0x000F;
pub const THERMOSTAT: u16 = 0x0201;
pub const FAN: u16 = 0x0202;
pub const DISPLAY: u16 = 0x0204;
pub const TEMPERATURE: u16 = 0x0402;
pub const HUMIDITY: u16 = 0x0405;

pub const THERMOSTAT_ENDPOINT: u8 = 1;
const BACKPLATE_ENDPOINT: u8 = 2;
const FAN_TIMER_ENDPOINT: u8 = 3;
const HUMIDITY_TARGET_ENDPOINT: u8 = 4;
const TIME_TO_TARGET_ENDPOINT: u8 = 5;
const FILTER_RUNTIME_ENDPOINT: u8 = 6;

/// The switches, by endpoint: their bucket field.
const SWITCHES: &[(u8, &str)] = &[
    (10, "learning_mode"),                 // Auto-Schedule
    (11, "auto_away_enable"),              // Home/Away Assist
    (12, "sunlight_correction_enabled"),   // Sunblock
    (13, "preconditioning_enabled"),       // Early-On
    (14, "away_temperature_low_enabled"),  // the eco heat limit, to heat
    (15, "away_temperature_high_enabled"), // the eco cool limit, to cool
    (16, "target_humidity_enabled"),       // humidity control
];

/// The states, by endpoint: their bucket field and, past the first four,
/// the equipment they come with.
const STATES: &[(u8, &str, &str)] = &[
    (20, "leaf", ""),
    (21, "preconditioning_active", ""),       // Early-On
    (22, "sunlight_correction_active", ""),   // Sunblock
    (23, "hvac_safety_shutoff_active", ""),
    (24, "filter_replacement_needed", "has_air_filter"),
    (25, "hot_water_active", "has_hot_water_control"),
    (26, "humidifier_state", "has_humidifier"),
    (27, "dehumidifier_state", "has_dehumidifier"),
];

// Home Automation device types.
const THERMOSTAT_DEVICE: u16 = 0x0301;
const TEMPERATURE_SENSOR: u16 = 0x0302;
const LEVEL_CONTROLLABLE_OUTPUT: u16 = 0x0003;
const ON_OFF_OUTPUT: u16 = 0x0002;
const SIMPLE_SENSOR: u16 = 0x000C;

const BASIC_ATTRIBUTES: &[u16] = &[0x0000, 0x0001, 0x0002, 0x0003, 0x0004, 0x0005, 0x0007, 0x4000];
const THERMOSTAT_ATTRIBUTES: &[u16] = &[
    0x0000, 0x0001, 0x0002, 0x0003, 0x0004, 0x0005, 0x0006, 0x0011, 0x0012, 0x0013, 0x0014, 0x0015, 0x0016,
    0x0017, 0x0018, 0x0019, 0x001B, 0x001C, 0x0025, 0x0029,
];
const ANALOG_OUTPUT_ATTRIBUTES: &[u16] = &[0x001C, 0x0041, 0x0045, 0x0051, 0x0055, 0x006F, 0x0075];
const ANALOG_INPUT_ATTRIBUTES: &[u16] = &[0x001C, 0x0051, 0x0055, 0x006F, 0x0075];
const BINARY_INPUT_ATTRIBUTES: &[u16] = &[0x001C, 0x0051, 0x0055, 0x006F];
const MEASUREMENT_ATTRIBUTES: &[u16] = &[0x0000, 0x0001, 0x0002];

pub const MANUFACTURER: &str = "Google Nest";
pub const MODEL: &str = "Nest Learning Thermostat";

/// The setpoint range, as the MQTT climate entity has it.
const MIN: i16 = 900;
const MAX: i16 = 3200;
/// The least gap the unit keeps between a range's low and high, 0.1 °C.
const DEAD_BAND: i8 = 15;
const NONE: i16 = i16::MIN;
/// ThermostatProgrammingOperationMode: following its schedule, economy.
const SCHEDULE: u8 = 0x01;
const ECONOMY: u8 = 0x04;
// BACnet engineering units, Analog Output's.
const SECONDS: u16 = 73;
const MINUTES: u16 = 72;
const HOURS: u16 = 71;
const PERCENT: u16 = 98;

/// An endpoint: its number, device type and server clusters.
pub struct Endpoint {
    pub id: u8,
    pub device: u16,
    pub clusters: Vec<u16>,
}

pub struct Nest {
    pub bridge: Arc<Bridge>,
    pub endpoints: Vec<Endpoint>,
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

    /// A temperature as the unit's screen shows it, as MQTT gives it.
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
    let state = bridge::state(&dev, &sh);
    View { dev, sh, state }
}

impl Nest {
    /// The unit's endpoints, for its equipment as it has reported it.
    pub fn new(bridge: Arc<Bridge>) -> Nest {
        let v = view(&bridge);
        let has = |key: &str| truthy(v.dev.get(key)) || truthy(v.sh.get(key));
        let has_fan = has("has_fan");
        let humidity_control = has("has_humidifier") || has("has_dehumidifier");
        let mut thermostat = vec![BASIC, POWER, IDENTIFY, THERMOSTAT, DISPLAY, HUMIDITY];
        if has_fan {
            thermostat.push(FAN);
        }
        let mut endpoints = vec![
            Endpoint { id: THERMOSTAT_ENDPOINT, device: THERMOSTAT_DEVICE, clusters: thermostat },
            Endpoint { id: BACKPLATE_ENDPOINT, device: TEMPERATURE_SENSOR, clusters: vec![TEMPERATURE] },
        ];
        if has_fan {
            endpoints.push(Endpoint { id: FAN_TIMER_ENDPOINT, device: LEVEL_CONTROLLABLE_OUTPUT, clusters: vec![ANALOG_OUTPUT] });
        }
        if humidity_control {
            endpoints.push(Endpoint {
                id: HUMIDITY_TARGET_ENDPOINT, device: LEVEL_CONTROLLABLE_OUTPUT, clusters: vec![ANALOG_OUTPUT],
            });
        }
        endpoints.push(Endpoint { id: TIME_TO_TARGET_ENDPOINT, device: SIMPLE_SENSOR, clusters: vec![ANALOG_INPUT] });
        if has("has_air_filter") {
            endpoints.push(Endpoint { id: FILTER_RUNTIME_ENDPOINT, device: SIMPLE_SENSOR, clusters: vec![ANALOG_INPUT] });
        }
        for &(id, key) in SWITCHES {
            let present = match key {
                "away_temperature_low_enabled" => has("can_heat"),
                "away_temperature_high_enabled" => has("can_cool"),
                "target_humidity_enabled" => humidity_control,
                _ => true,
            };
            if present {
                endpoints.push(Endpoint { id, device: ON_OFF_OUTPUT, clusters: vec![ON_OFF] });
            }
        }
        for &(id, _, equipment) in STATES {
            if equipment.is_empty() || has(equipment) {
                endpoints.push(Endpoint { id, device: SIMPLE_SENSOR, clusters: vec![BINARY_INPUT] });
            }
        }
        Nest { bridge, endpoints }
    }

    /// What changes reports go out for: endpoint, cluster, attributes.
    pub fn reported(&self) -> Vec<(u8, u16, &'static [u16])> {
        let mut out = Vec::new();
        for e in &self.endpoints {
            for &cluster in &e.clusters {
                let attributes: &'static [u16] = match cluster {
                    THERMOSTAT => &[0x0000, 0x0001, 0x0002, 0x0011, 0x0012, 0x0013, 0x0014, 0x001B, 0x001C, 0x0025, 0x0029],
                    POWER => &[0x0020],
                    FAN | DISPLAY | HUMIDITY | TEMPERATURE | ON_OFF => &[0x0000],
                    ANALOG_OUTPUT | ANALOG_INPUT | BINARY_INPUT => &[0x0055],
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

    fn thermostat(&self, attribute: u16) -> Option<Value> {
        let view = self.view();
        let temperature = |t: Option<i16>| Value::I16(t.unwrap_or(NONE));
        Some(match attribute {
            0x0000 => temperature(view.shown("current_temperature")),
            0x0001 => temperature(self.bridge.outdoor.temperature().map(centi)),
            0x0002 => Value::Bitmap8(u8::from(!truthy(view.sh.get("auto_away")))),
            0x0003 | 0x0005 | 0x0015 | 0x0017 => Value::I16(MIN),
            0x0004 | 0x0006 | 0x0016 | 0x0018 => Value::I16(MAX),
            0x0011 => {
                let field = if view.mode() == "heat_cool" { "target_high" } else { "target" };
                Value::I16(view.shown(field).unwrap_or(2400))
            }
            0x0012 => {
                let field = if view.mode() == "heat_cool" { "target_low" } else { "target" };
                Value::I16(view.shown(field).unwrap_or(2000))
            }
            0x0013 => Value::I16(view.shown("away_temperature_high").unwrap_or(MAX)),
            0x0014 => Value::I16(view.shown("away_temperature_low").unwrap_or(MIN)),
            0x0019 => Value::I8(DEAD_BAND),
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

    /// An Analog Output: the fan timer's minutes, or the target humidity.
    fn analog_output(&self, endpoint: u8, attribute: u16) -> Option<Value> {
        let (description, units, min, max, value) = match endpoint {
            FAN_TIMER_ENDPOINT => {
                let minutes = number(self.view().dev.get("fan_timer_duration")).map_or(15.0, |s| s / 60.0);
                ("Fan timer duration", MINUTES, 15.0, 1440.0, minutes)
            }
            _ => ("Target humidity", PERCENT, 10.0, 60.0, number(self.view().dev.get("target_humidity")).unwrap_or(35.0)),
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
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
                let left = number(view.dev.get("time_to_target")).map_or(0.0, |t| (t - now).max(0.0).round());
                ("Time to temperature", SECONDS, left)
            }
            _ => ("Filter runtime", HOURS, number(view.dev.get("filter_runtime_sec")).map_or(0.0, |s| (s / 360.0).round() / 10.0)),
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

    fn state(&self, endpoint: u8) -> Option<&'static str> {
        STATES.iter().find(|(e, _, _)| *e == endpoint).map(|(_, key, _)| *key)
    }

    fn switch(&self, endpoint: u8) -> Option<&'static str> {
        SWITCHES.iter().find(|(e, _)| *e == endpoint).map(|(_, key)| *key)
    }

    /// On/Off's commands: off, on, toggle.
    fn switch_command(&self, endpoint: u8, command: u8) -> u8 {
        let Some(key) = self.switch(endpoint) else {
            return status::UNSUP_CLUSTER_COMMAND;
        };
        let on = match command {
            0x00 => false,
            0x01 => true,
            0x02 => !truthy(self.view().dev.get(key)),
            _ => return status::UNSUP_CLUSTER_COMMAND,
        };
        self.apply(key, if on { "ON" } else { "OFF" })
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
            IDENTIFY | ON_OFF => &[0x0000],
            THERMOSTAT => THERMOSTAT_ATTRIBUTES,
            FAN | DISPLAY => &[0x0000, 0x0001],
            TEMPERATURE | HUMIDITY => MEASUREMENT_ATTRIBUTES,
            ANALOG_OUTPUT => ANALOG_OUTPUT_ATTRIBUTES,
            ANALOG_INPUT => ANALOG_INPUT_ATTRIBUTES,
            BINARY_INPUT => BINARY_INPUT_ATTRIBUTES,
            _ => return None,
        })
    }

    fn read(&self, endpoint: u8, cluster: u16, attribute: u16) -> Option<Value> {
        match (cluster, attribute) {
            (BASIC, 0x0000) => Some(Value::U8(3)), // ZCL version
            (BASIC, 0x0001) => Some(Value::U8(1)), // application version
            (BASIC, 0x0002) => Some(Value::U8(6)), // stack version: EmberZNet 6
            (BASIC, 0x0003) => Some(Value::U8(3)), // hardware: the 3rd generation
            (BASIC, 0x0004) => Some(Value::Str(MANUFACTURER.into())),
            (BASIC, 0x0005) => Some(Value::Str(MODEL.into())),
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
            (BINARY_INPUT, 0x001C) => self.state(endpoint).map(|key| Value::Str(key.into())),
            (BINARY_INPUT, 0x0051) => Some(Value::Bool(false)),
            (BINARY_INPUT, 0x0055) => self.state(endpoint).map(|key| Value::Bool(truthy(self.view().dev.get(key)))),
            (BINARY_INPUT, 0x006F) => Some(Value::Bitmap8(0)),
            (ON_OFF, 0x0000) => self.switch(endpoint).map(|key| Value::Bool(truthy(self.view().dev.get(key)))),
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
            (IDENTIFY, 0x0000, _) => status::SUCCESS, // nothing to blink
            _ => status::READ_ONLY,
        }
    }

    fn command(&self, endpoint: u8, cluster: u16, command: u8, payload: &[u8]) -> u8 {
        match (cluster, command) {
            (THERMOSTAT, 0x00) => self.raise_lower(payload),
            (THERMOSTAT, 0x03) => self.apply("clear_schedule", ""), // Clear Weekly Schedule
            (ON_OFF, c) => self.switch_command(endpoint, c),
            (IDENTIFY, 0x00 | 0x01) => status::SUCCESS,
            _ => status::UNSUP_CLUSTER_COMMAND,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields;
    use super::super::zcl::serve;

    fn nest(dev: Fields, sh: Fields) -> Nest {
        let bridge = crate::bridge::tests::test_bridge();
        bridge.store.merge(&format!("device.{}", bridge.serial), &dev, 0, 0, true);
        bridge.store.merge(&format!("shared.{}", bridge.serial), &sh, 0, 0, true);
        Nest::new(bridge)
    }

    #[test]
    fn endpoints_follow_the_equipment() {
        let ids = |n: &Nest| n.endpoints.iter().map(|e| e.id).collect::<Vec<_>>();
        let n = nest(fields!{}, fields!{"can_heat" => true});
        assert_eq!(ids(&n), [1, 2, 5, 10, 11, 12, 13, 14, 20, 21, 22, 23]);
        assert!(!n.has(1, FAN));
        let n = nest(fields!{"has_fan" => true, "has_humidifier" => true, "has_air_filter" => true},
                     fields!{"can_heat" => true, "can_cool" => true});
        assert_eq!(ids(&n), [1, 2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 15, 16, 20, 21, 22, 23, 24, 26]);
        assert!(n.has(1, FAN));
    }

    #[test]
    fn the_thermostat_as_the_screen_shows_it() {
        let n = nest(fields!{"temperature_scale" => "C", "current_humidity" => 41, "battery_level" => 3.92,
                             "backplate_temperature" => 22.1},
                     fields!{"target_temperature_type" => "heat", "target_temperature" => 21.38377,
                             "current_temperature" => 18.20557, "can_heat" => true, "hvac_heater_state" => true});
        assert_eq!(n.read(1, THERMOSTAT, 0x0000), Some(Value::I16(1800)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0012), Some(Value::I16(2150)));
        assert_eq!(n.read(1, THERMOSTAT, 0x001C), Some(Value::Enum8(0x04)));
        assert_eq!(n.read(1, THERMOSTAT, 0x001B), Some(Value::Enum8(0x02)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0025), Some(Value::Bitmap8(SCHEDULE)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0029), Some(Value::Bitmap16(0x0001)));
        assert_eq!(n.read(1, HUMIDITY, 0x0000), Some(Value::U16(4100)));
        assert_eq!(n.read(1, DISPLAY, 0x0000), Some(Value::Enum8(0)));
        assert_eq!(n.read(1, POWER, 0x0020), Some(Value::U8(39)));
        assert_eq!(n.read(2, TEMPERATURE, 0x0000), Some(Value::I16(2210)));
        assert_eq!(n.read(10, ON_OFF, 0x0000), Some(Value::Bool(false)));
        assert!(n.attributes(3, ANALOG_OUTPUT).is_none()); // no fan
    }

    #[test]
    fn states_and_times() {
        let soon = chrono::Utc::now().timestamp() + 600;
        let n = nest(fields!{"leaf" => true, "time_to_target" => soon, "has_air_filter" => true,
                             "filter_runtime_sec" => 36000, "filter_replacement_needed" => false},
                     fields!{"can_heat" => true});
        assert_eq!(n.read(20, BINARY_INPUT, 0x0055), Some(Value::Bool(true)));
        assert_eq!(n.read(23, BINARY_INPUT, 0x0055), Some(Value::Bool(false)));
        assert_eq!(n.read(24, BINARY_INPUT, 0x0055), Some(Value::Bool(false)));
        assert!(n.attributes(25, BINARY_INPUT).is_none()); // no hot water
        let Some(Value::F32(left)) = n.read(5, ANALOG_INPUT, 0x0055) else { panic!() };
        assert!((598.0..=600.0).contains(&left));
        assert_eq!(n.read(5, ANALOG_INPUT, 0x0075), Some(Value::Enum16(SECONDS)));
        assert_eq!(n.read(6, ANALOG_INPUT, 0x0055), Some(Value::F32(10.0)));
        assert_eq!(n.command(1, THERMOSTAT, 0x03, &[]), status::INVALID_VALUE); // no schedule yet
        n.bridge.store.merge(&format!("schedule.{}", n.bridge.serial), &fields!{"ver" => 2}, 0, 0, true);
        assert_eq!(n.command(1, THERMOSTAT, 0x03, &[]), status::SUCCESS);
        assert_eq!(n.read(10, ON_OFF, 0x0000), Some(Value::Bool(false))); // learning off with the schedule
    }

    #[test]
    fn every_listed_attribute_reads_and_every_reported_one_is_listed() {
        let n = nest(fields!{"has_fan" => true, "has_humidifier" => true, "has_dehumidifier" => true,
                             "has_air_filter" => true, "has_hot_water_control" => true},
                     fields!{"can_heat" => true, "can_cool" => true});
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
        assert!(n.attributes(1, ON_OFF).is_none());
        assert!(n.attributes(10, THERMOSTAT).is_none());
        assert!(n.attributes(99, BASIC).is_none());
    }

    #[test]
    fn what_the_unit_has_not_reported_reads_as_zcls_invalid_values() {
        let n = nest(fields!{}, fields!{});
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
        let n = nest(fields!{},
                     fields!{"target_temperature_type" => "cool", "can_cool" => true, "hvac_ac_state" => true,
                             "hvac_fan_state" => true, "auto_away" => 1});
        assert_eq!(n.read(1, THERMOSTAT, 0x001B), Some(Value::Enum8(0x00)));
        assert_eq!(n.read(1, THERMOSTAT, 0x001C), Some(Value::Enum8(0x03)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0029), Some(Value::Bitmap16(0x0006)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0002), Some(Value::Bitmap8(0))); // away: unoccupied
    }

    #[test]
    fn fahrenheit_temperatures_are_the_whole_degrees_the_screen_shows() {
        // 19.5 °C shows as 67 °F, 22 °C as 72 °F.
        let n = nest(fields!{"temperature_scale" => "F"},
                     fields!{"target_temperature_type" => "heat", "target_temperature" => 19.5,
                             "current_temperature" => 22.0, "can_heat" => true});
        assert_eq!(n.read(1, DISPLAY, 0x0000), Some(Value::Enum8(1)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0012), Some(Value::I16(1944)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0000), Some(Value::I16(2222)));
        assert_eq!(n.write(1, DISPLAY, 0x0000, Value::Enum8(0)), status::SUCCESS);
        assert_eq!(n.read(1, DISPLAY, 0x0000), Some(Value::Enum8(0)));
        assert_eq!(n.read(1, THERMOSTAT, 0x0012), Some(Value::I16(1950)));
        assert_eq!(n.write(1, DISPLAY, 0x0000, Value::Enum8(2)), status::INVALID_VALUE);
    }

    #[test]
    fn in_heat_cool_the_heating_and_cooling_setpoints_are_the_range() {
        let n = nest(fields!{"temperature_scale" => "C"},
                     fields!{"target_temperature_type" => "range", "target_temperature_low" => 19.0,
                             "target_temperature_high" => 24.0, "can_heat" => true, "can_cool" => true});
        let range = |n: &Nest| (n.read(1, THERMOSTAT, 0x0012), n.read(1, THERMOSTAT, 0x0011));
        let i16s = |low, high| (Some(Value::I16(low)), Some(Value::I16(high)));
        assert_eq!(n.read(1, THERMOSTAT, 0x001C), Some(Value::Enum8(0x01)));
        assert_eq!(range(&n), i16s(1900, 2400));
        assert_eq!(n.write(1, THERMOSTAT, 0x0012, Value::I16(2000)), status::SUCCESS);
        assert_eq!(n.write(1, THERMOSTAT, 0x0011, Value::I16(2550)), status::SUCCESS);
        assert_eq!(range(&n), i16s(2000, 2550));
        // setpointRaiseLower: heat down 1 °C, both up 0.5 °C, cool up 0.5 °C.
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x00, (-10i8) as u8]), status::SUCCESS);
        assert_eq!(range(&n), i16s(1900, 2550));
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x02, 5]), status::SUCCESS);
        assert_eq!(range(&n), i16s(1950, 2600));
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x01, 5]), status::SUCCESS);
        assert_eq!(range(&n), i16s(1950, 2650));
        // Its amount missing.
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x02]), status::INVALID_FIELD);
        assert_eq!(range(&n), i16s(1950, 2650));
    }

    #[test]
    fn the_fan_and_the_analog_outputs_take_what_mqtt_takes() {
        let n = nest(fields!{"has_fan" => true, "has_humidifier" => true}, fields!{"can_heat" => true});
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
    fn commands_the_endpoint_does_not_have_are_refused() {
        let n = nest(fields!{}, fields!{"can_heat" => true});
        assert_eq!(n.command(10, ON_OFF, 0x40, &[]), status::UNSUP_CLUSTER_COMMAND);
        assert_eq!(n.command(20, ON_OFF, 0x01, &[]), status::UNSUP_CLUSTER_COMMAND); // a state, not a switch
        assert_eq!(n.command(1, THERMOSTAT, 0x01, &[]), status::UNSUP_CLUSTER_COMMAND);
        assert_eq!(n.write(1, BASIC, 0x0005, Value::Str("x".into())), status::READ_ONLY);
    }

    #[test]
    fn zcl_frames_reach_the_unit() {
        let n = nest(fields!{"temperature_scale" => "C"},
                     fields!{"target_temperature_type" => "heat", "target_temperature" => 20.0, "can_heat" => true});
        // Write the heating setpoint, 21.5 °C, then as an enum8.
        let out = serve(&n, 1, THERMOSTAT, &[0x00, 1, 0x02, 0x12, 0x00, 0x29, 0x66, 0x08]).unwrap();
        assert_eq!(out, [0x18, 1, 0x04, 0x00]);
        assert_eq!(n.read(1, THERMOSTAT, 0x0012), Some(Value::I16(2150)));
        let out = serve(&n, 1, THERMOSTAT, &[0x00, 2, 0x02, 0x12, 0x00, 0x30, 0x01]).unwrap();
        assert_eq!(out, [0x18, 2, 0x04, 0x8D, 0x12, 0x00]);
        // The thermostat's endpoint has no On/Off.
        let out = serve(&n, 1, ON_OFF, &[0x01, 3, 0x01]).unwrap();
        assert_eq!(out, [0x18, 3, 0x0B, 0x01, 0xC3]);
    }

    #[test]
    fn writes_are_commands() {
        let n = nest(fields!{"temperature_scale" => "C", "has_fan" => true, "fan_timer_duration" => 900},
                     fields!{"target_temperature_type" => "heat", "target_temperature" => 20.0});
        assert_eq!(n.write(1, THERMOSTAT, 0x0012, Value::I16(2150)), status::SUCCESS);
        assert_eq!(n.read(1, THERMOSTAT, 0x0012), Some(Value::I16(2150)));
        assert_eq!(n.write(1, THERMOSTAT, 0x001C, Value::Enum8(0x03)), status::SUCCESS);
        assert_eq!(n.read(1, THERMOSTAT, 0x001C), Some(Value::Enum8(0x03)));
        assert_eq!(n.write(1, THERMOSTAT, 0x001C, Value::Enum8(0x07)), status::INVALID_VALUE);
        assert_eq!(n.write(1, THERMOSTAT, 0x0000, Value::I16(0)), status::READ_ONLY);
        // Half degrees, as the unit has them; out of range is refused.
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x00, 5]), status::SUCCESS);
        assert_eq!(n.write(1, THERMOSTAT, 0x0011, Value::I16(9000)), status::INVALID_VALUE);
        // Eco, the fan timer, a switch.
        assert_eq!(n.write(1, THERMOSTAT, 0x0025, Value::Bitmap8(ECONOMY)), status::SUCCESS);
        assert_eq!(n.read(1, THERMOSTAT, 0x0025), Some(Value::Bitmap8(ECONOMY)));
        assert_eq!(n.read(3, ANALOG_OUTPUT, 0x0055), Some(Value::F32(15.0)));
        assert_eq!(n.write(3, ANALOG_OUTPUT, 0x0055, Value::F32(60.0)), status::SUCCESS);
        assert_eq!(n.read(3, ANALOG_OUTPUT, 0x0055), Some(Value::F32(60.0)));
        assert_eq!(n.command(11, ON_OFF, 0x01, &[]), status::SUCCESS);
        assert_eq!(n.read(11, ON_OFF, 0x0000), Some(Value::Bool(true)));
        assert_eq!(n.command(11, ON_OFF, 0x02, &[]), status::SUCCESS);
        assert_eq!(n.read(11, ON_OFF, 0x0000), Some(Value::Bool(false)));
    }

    #[test]
    fn setpoints_outside_the_advertised_limits_are_refused() {
        let n = nest(fields!{"temperature_scale" => "C"},
                     fields!{"target_temperature_type" => "heat", "target_temperature" => 20.0, "can_heat" => true});
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
    fn raise_lower_stays_within_the_limits_and_follows_its_mode() {
        let n = nest(fields!{"temperature_scale" => "C"},
                     fields!{"target_temperature_type" => "heat", "target_temperature" => 31.0, "can_heat" => true});
        let heating = |n: &Nest| n.read(1, THERMOSTAT, 0x0012);
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x00, 50]), status::SUCCESS);
        assert_eq!(heating(&n), Some(Value::I16(MAX)));
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x02, (-128i8) as u8]), status::SUCCESS);
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x02, (-128i8) as u8]), status::SUCCESS);
        assert_eq!(heating(&n), Some(Value::I16(MIN)));
        // Cool alone leaves a heating unit's setpoint; no such mode as 3.
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x01, 20]), status::SUCCESS);
        assert_eq!(heating(&n), Some(Value::I16(MIN)));
        assert_eq!(n.command(1, THERMOSTAT, 0x00, &[0x03, 20]), status::INVALID_FIELD);
        assert_eq!(heating(&n), Some(Value::I16(MIN)));
    }
}
