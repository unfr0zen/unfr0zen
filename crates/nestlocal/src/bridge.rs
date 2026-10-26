//! The Home Assistant side: MQTT discovery for the thermostat, one JSON
//! state topic every entity reads from, and command topics that change
//! buckets. A changed bucket goes to nlclient on its held subscribe; what
//! the unit then does comes back through its own PUTs, so Home Assistant
//! shows what the unit did, not what was asked.
//!
//!   <prefix>/<serial>/availability     online while nlclient calls in and the
//!                                      broker has nestlocal, else offline
//!                                      (retained, will)
//!   <prefix>/<serial>/state            the computed state (retained)
//!   <prefix>/<serial>/raw/<bucket>     each bucket, whole (retained)
//!   <prefix>/<serial>/set/<command>    Home Assistant's commands
//!   <prefix>/<serial>/set/raw/<bucket> merge a JSON object into a bucket
//!                                      (anything the entities don't cover)

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tokio::sync::mpsc;

use crate::clock;
use crate::commands::{ECO_HIGH, ECO_LOW};
use crate::fields;
use crate::json::{num, number, sprint, truthy, Fields};
use crate::liveness::Liveness;
use crate::logf;
use crate::mqtt;
use crate::store::{Object, Store};
use crate::weather::Outdoor;

pub struct Bridge {
    pub serial: String,
    pub base: String,
    discovery: String,
    pub store: Arc<Store>,
    pub outdoor: Arc<Outdoor>,
    live: Option<Arc<Liveness>>,
    mqtt: OnceLock<mqtt::Client>,
    announced: Mutex<Option<BTreeMap<String, String>>>, // discovery topic -> config last published
    time_asked: Mutex<Option<SystemTime>>, // when ask_time last asked, wall clock
    time_ok: mpsc::Sender<()>,             // Home Assistant answered in time
    time_ok_rx: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Bridge {
    pub fn new(prefix: &str, discovery: &str, serial: &str, store: Arc<Store>, outdoor: Arc<Outdoor>,
               live: Option<Arc<Liveness>>) -> Arc<Bridge> {
        let (time_ok, time_ok_rx) = mpsc::channel(1);
        Arc::new(Bridge {
            serial: serial.into(),
            base: format!("{prefix}/{serial}"),
            discovery: discovery.into(),
            store,
            outdoor,
            live,
            mqtt: OnceLock::new(),
            announced: Mutex::new(None),
            time_asked: Mutex::new(None),
            time_ok,
            time_ok_rx: Mutex::new(Some(time_ok_rx)),
        })
    }

    /// Connects to the broker, and from then on publishes every change and
    /// takes Home Assistant's commands.
    pub fn run(self: &Arc<Self>, broker: &mqtt::Broker, user: &str, password: &str,
               tls: Option<Arc<rustls::ClientConfig>>) {
        let options = mqtt::Options {
            client_id: format!("nestlocal-{}", self.serial),
            user: (!user.is_empty()).then(|| user.to_string()),
            password: password.into(),
            will: (format!("{}/availability", self.base), b"offline".to_vec()),
            tls,
        };
        let (client, connection) = mqtt::Client::new(broker.clone(), options);
        let _ = self.mqtt.set(client);
        let bridge = self.clone();
        self.store.watch(move |o| bridge.changed(o));
        if let Some(live) = &self.live {
            let bridge = self.clone();
            live.on_change(move |alive| bridge.publish_availability(alive));
        }
        tokio::spawn(self.clone().keep_time());
        tokio::spawn(connection.run(self.clone()));
    }

    /// Asks Home Assistant for the time (its time automation); the answer
    /// comes to <base>/set/time.
    fn ask_time(&self) {
        let Some(c) = self.mqtt.get().filter(|c| c.is_connected()) else {
            return;
        };
        // The wall clock, not the monotonic one: that one stops while the
        // unit sleeps, and an answer that waited out a sleep must count as
        // late.
        *self.time_asked.lock().unwrap() = Some(SystemTime::now());
        c.publish(&format!("{}/time/request", self.base), b"", 0, false);
    }

    /// Takes Home Assistant's answer, seconds since 1970, if it is prompt.
    pub fn time_answer(&self, arg: &str) -> Result<(), String> {
        let seconds = arg.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 1e9)
            .ok_or("expected seconds since 1970")?;
        let Some(asked) = self.time_asked.lock().unwrap().take() else {
            return Ok(()); // not asked
        };
        let Ok(rtt) = SystemTime::now().duration_since(asked) else {
            return Ok(());
        };
        if rtt > clock::ANSWER {
            return Ok(()); // too late to trust
        }
        clock::set_clock(UNIX_EPOCH + Duration::from_secs_f64(seconds) + rtt / 2, "Home Assistant");
        let _ = self.time_ok.try_send(());
        Ok(())
    }

    /// Asks Home Assistant for the time every minute until it answers, then
    /// every 6 hours.
    async fn keep_time(self: Arc<Self>) {
        let Some(mut answered) = self.time_ok_rx.lock().unwrap().take() else {
            return;
        };
        loop {
            self.ask_time();
            tokio::select! {
                Some(()) = answered.recv() => tokio::time::sleep(clock::RECHECK).await,
                _ = tokio::time::sleep(clock::RETRY) => {}
            }
        }
    }

    fn connected(&self) {
        logf!("mqtt: connected");
        *self.announced.lock().unwrap() = None;
        if let Some(c) = self.mqtt.get() {
            c.subscribe(&format!("{}/set/#", self.base));
            c.subscribe("homeassistant/status");
        }
        self.publish_availability(self.live.as_ref().is_some_and(|l| l.alive()));
        self.ask_time();
        for o in self.store.all() {
            self.publish_raw(&o);
        }
        self.publish_state();
    }

    fn message(&self, topic: &str, payload: &[u8]) {
        if topic == "homeassistant/status" {
            if payload == b"online" {
                // Home Assistant restarted: announce again.
                *self.announced.lock().unwrap() = None;
                self.publish_state();
            }
        } else if topic.starts_with(&format!("{}/set/", self.base)) {
            self.command(topic, payload);
        }
    }

    /// The unit woke (wake.rs). If nothing went to the broker for half the
    /// keepalive by the wall clock (the ping timer stopped with the unit),
    /// publish the availability again at QoS 0: a packet the broker counts,
    /// with no answer to wake the unit again.
    pub fn woke(&self) {
        let Some(c) = self.mqtt.get().filter(|c| c.is_connected()) else {
            return;
        };
        if c.idle() > mqtt::KEEPALIVE / 2 {
            let alive = self.live.as_ref().is_some_and(|l| l.alive());
            c.publish(&format!("{}/availability", self.base), if alive { "online" } else { "offline" }.as_bytes(), 0, true);
        }
    }

    fn publish_availability(&self, alive: bool) {
        if let Some(c) = self.mqtt.get() {
            c.publish(&format!("{}/availability", self.base), if alive { "online" } else { "offline" }.as_bytes(), 1, true);
        }
    }

    fn publish(&self, topic: &str, retain: bool, payload: &[u8]) {
        if let Some(c) = self.mqtt.get() {
            c.publish(topic, payload, 0, retain);
        }
    }

    fn changed(&self, o: &Object) {
        self.publish_raw(o);
        self.publish_state();
    }

    fn publish_raw(&self, o: &Object) {
        let typ = o.key.split_once('.').map_or(o.key.as_str(), |(t, _)| t);
        self.publish(&format!("{}/raw/{typ}", self.base), true, Value::Object(o.value.clone()).to_string().as_bytes());
    }

    /// Buckets of this unit, by type: device, shared, ...
    pub fn buckets(&self) -> BTreeMap<String, Fields> {
        self.store.all().into_iter().filter_map(|o| {
            let (typ, id) = o.key.split_once('.').unwrap_or((o.key.as_str(), ""));
            (id == self.serial).then(|| (typ.to_string(), o.value.clone()))
        }).collect()
    }

    fn publish_state(&self) {
        let buckets = self.buckets();
        let (Some(dev), Some(sh)) = (buckets.get("device"), buckets.get("shared")) else {
            return; // the unit has not reported yet
        };
        self.announce(dev, sh);
        self.publish(&format!("{}/state", self.base), true, Value::Object(state(dev, sh)).to_string().as_bytes());
    }

    /// Publishes Home Assistant discovery, for what this unit's equipment
    /// can do (can_heat, has_fan, ...). Again only when that changes.
    fn announce(&self, dev: &Fields, sh: &Fields) {
        let id = format!("nestlocal_{}", self.serial.to_lowercase());
        let device = fields!{
            "identifiers" => vec![id.clone()],
            "name" => "Nest Thermostat",
            "manufacturer" => "Google Nest",
            "model" => dev.get("model_version").cloned().unwrap_or(Value::Null),
            "sw_version" => dev.get("current_version").cloned().unwrap_or(Value::Null),
            "serial_number" => self.serial.clone(),
        };
        let st = format!("{}/state", self.base);
        let set = format!("{}/set/", self.base);
        let mut configs = BTreeMap::new(); // discovery topic -> config
        let mut add = |component: &str, key: &str, name: &str, extra: Fields| {
            let mut c = fields!{
                "name" => if name.is_empty() { Value::Null } else { name.into() }, // null: the device's own name
                "unique_id" => format!("{id}_{key}"),
                "object_id" => format!("{id}_{key}"),
                "device" => Value::Object(device.clone()),
                "availability_topic" => format!("{}/availability", self.base),
                "state_topic" => st.clone(),
            };
            for (k, v) in extra {
                if v.is_null() {
                    c.remove(&k);
                } else {
                    c.insert(k, v);
                }
            }
            configs.insert(format!("{}/{component}/{id}/{key}/config", self.discovery), Value::Object(c).to_string());
        };
        let tmpl = |k: &str| format!("{{{{ value_json.{k} }}}}");
        let on_off = |k: &str| format!("{{{{ 'ON' if value_json.{k} else 'OFF' }}}}");

        let (can_heat, can_cool) = (truthy(sh.get("can_heat")), truthy(sh.get("can_cool")));
        let mut modes = vec!["off"];
        if can_heat {
            modes.push("heat");
        }
        if can_cool {
            modes.push("cool");
        }
        if can_heat && can_cool {
            modes.push("heat_cool");
        }
        // What the unit's screen shows.
        let precision = if sprint(dev.get("temperature_scale")) == "F" { 1.0 } else { 0.5 };
        let mut climate = fields!{
            "state_topic" => Value::Null,
            "modes" => modes,
            "temperature_unit" => "C",
            "precision" => num(precision),
            "temp_step" => 0.5,
            "min_temp" => 9,
            "max_temp" => 32,
            "mode_state_topic" => st.clone(),
            "mode_state_template" => tmpl("mode"),
            "mode_command_topic" => format!("{set}mode"),
            "action_topic" => st.clone(),
            "action_template" => tmpl("action"),
            "current_temperature_topic" => st.clone(),
            "current_temperature_template" => tmpl("current_temperature"),
            "current_humidity_topic" => st.clone(),
            "current_humidity_template" => tmpl("current_humidity"),
            "temperature_state_topic" => st.clone(),
            "temperature_state_template" => tmpl("target"),
            "temperature_command_topic" => format!("{set}target"),
            "preset_modes" => vec!["eco"],
            "preset_mode_state_topic" => st.clone(),
            "preset_mode_value_template" => tmpl("preset"),
            "preset_mode_command_topic" => format!("{set}preset"),
        };
        if can_heat && can_cool {
            // Home Assistant clears the single setpoint when a range is active.
            climate.extend(fields!{
                "temperature_state_template" => "{{ value_json.target if value_json.mode != 'heat_cool' else None }}",
                "temperature_low_state_topic" => st.clone(),
                "temperature_low_state_template" => "{{ value_json.target_low if value_json.mode == 'heat_cool' else None }}",
                "temperature_low_command_topic" => format!("{set}target_low"),
                "temperature_high_state_topic" => st.clone(),
                "temperature_high_state_template" => "{{ value_json.target_high if value_json.mode == 'heat_cool' else None }}",
                "temperature_high_command_topic" => format!("{set}target_high"),
            });
        }
        let has_fan = truthy(dev.get("has_fan"));
        if has_fan {
            climate.extend(fields!{
                "fan_modes" => vec!["auto", "on"],
                "fan_mode_state_topic" => st.clone(),
                "fan_mode_state_template" => tmpl("fan_mode"),
                "fan_mode_command_topic" => format!("{set}fan_mode"),
            });
        }
        add("climate", "thermostat", "", climate);

        let sensor = |add: &mut dyn FnMut(&str, &str, &str, Fields), key: &str, name: &str, unit: &str, class: &str, extra: Fields| {
            let mut c = fields!{"value_template" => tmpl(key), "state_class" => "measurement"};
            if !unit.is_empty() {
                c.insert("unit_of_measurement".into(), unit.into());
            }
            if !class.is_empty() {
                c.insert("device_class".into(), class.into());
            }
            c.extend(extra);
            add("sensor", key, name, c);
        };
        let diag = || fields!{"entity_category" => "diagnostic"};
        sensor(&mut add, "current_temperature", "Temperature", "°C", "temperature", Fields::new());
        sensor(&mut add, "current_humidity", "Humidity", "%", "humidity", Fields::new());
        sensor(&mut add, "backplate_temperature", "Backplate temperature", "°C", "temperature", diag());
        sensor(&mut add, "battery_level", "Battery voltage", "V", "voltage", diag());
        sensor(&mut add, "rssi", "WiFi signal", "", "", fields!{"entity_category" => "diagnostic", "icon" => "mdi:wifi"});
        add("sensor", "time_to_target", "Time to temperature",
            fields!{"value_template" => tmpl("time_to_target"), "device_class" => "timestamp"});
        add("sensor", "hvac_action", "HVAC action", fields!{"value_template" => tmpl("action"), "icon" => "mdi:hvac"});
        add("sensor", "local_ip", "IP address",
            fields!{"value_template" => tmpl("local_ip"), "entity_category" => "diagnostic", "icon" => "mdi:ip-network"});
        if truthy(dev.get("has_air_filter")) {
            sensor(&mut add, "filter_runtime_h", "Filter runtime", "h", "duration",
                fields!{"state_class" => "total_increasing", "entity_category" => "diagnostic"});
        }

        let mut binary = |key: &str, name: &str, class: &str, extra: Fields| {
            let mut c = fields!{"value_template" => on_off(key)};
            if !class.is_empty() {
                c.insert("device_class".into(), class.into());
            }
            c.extend(extra);
            add("binary_sensor", key, name, c);
        };
        if can_heat {
            binary("heating", "Heating", "heat", Fields::new());
        }
        if can_cool {
            binary("cooling", "Cooling", "cold", Fields::new());
        }
        if has_fan {
            binary("fan_running", "Fan", "running", Fields::new());
        }
        binary("leaf", "Leaf", "", fields!{"icon" => "mdi:leaf"});
        binary("occupancy", "Occupancy", "occupancy", Fields::new());
        binary("preconditioning_active", "Early-On active", "", fields!{"icon" => "mdi:clock-fast"});
        binary("sunlight_correction_active", "Sunblock active", "", fields!{"icon" => "mdi:weather-sunny"});
        binary("hvac_safety_shutoff_active", "Safety shutoff", "problem", fields!{"entity_category" => "diagnostic"});
        if truthy(dev.get("has_air_filter")) {
            binary("filter_replacement_needed", "Filter replacement", "problem", Fields::new());
        }
        if truthy(dev.get("has_hot_water_control")) {
            binary("hot_water_active", "Hot water", "heat", Fields::new());
        }
        if truthy(dev.get("has_humidifier")) {
            binary("humidifier_state", "Humidifier", "running", Fields::new());
        }
        if truthy(dev.get("has_dehumidifier")) {
            binary("dehumidifier_state", "Dehumidifier", "running", Fields::new());
        }

        let sw = |add: &mut dyn FnMut(&str, &str, &str, Fields), key: &str, name: &str, icon: &str| {
            add("switch", key, name, fields!{
                "value_template" => on_off(key),
                "command_topic" => format!("{set}{key}"),
                "entity_category" => "config",
                "icon" => icon,
            });
        };
        sw(&mut add, "learning_mode", "Auto-Schedule", "mdi:school");
        sw(&mut add, "auto_away_enable", "Home/Away Assist", "mdi:home-account");
        sw(&mut add, "sunlight_correction_enabled", "Sunblock", "mdi:weather-sunny");
        sw(&mut add, "preconditioning_enabled", "Early-On", "mdi:clock-fast");
        if can_heat {
            sw(&mut add, "away_temperature_low_enabled", "Eco heat limit", "mdi:leaf");
        }
        if can_cool {
            sw(&mut add, "away_temperature_high_enabled", "Eco cool limit", "mdi:leaf");
        }

        let number_entity = |add: &mut dyn FnMut(&str, &str, &str, Fields), key: &str, name: &str,
                             min: f64, max: f64, step: f64, unit: &str| {
            add("number", key, name, fields!{
                "value_template" => tmpl(key),
                "command_topic" => format!("{set}{key}"),
                "min" => num(min),
                "max" => num(max),
                "step" => num(step),
                "unit_of_measurement" => unit,
                "entity_category" => "config",
                "mode" => "box",
            });
        };
        if can_heat {
            number_entity(&mut add, "away_temperature_low", "Eco heat temperature", ECO_LOW.0, ECO_LOW.1, 0.5, "°C");
        }
        if can_cool {
            number_entity(&mut add, "away_temperature_high", "Eco cool temperature", ECO_HIGH.0, ECO_HIGH.1, 0.5, "°C");
        }
        if truthy(dev.get("has_humidifier")) || truthy(dev.get("has_dehumidifier")) {
            number_entity(&mut add, "target_humidity", "Target humidity", 10.0, 60.0, 5.0, "%");
            sw(&mut add, "target_humidity_enabled", "Humidity control", "mdi:water-percent");
        }
        if has_fan {
            add("select", "fan_timer_duration", "Fan timer duration", fields!{
                "value_template" => tmpl("fan_timer_duration"),
                "command_topic" => format!("{set}fan_timer_duration"),
                "options" => vec!["15", "30", "45", "60", "120", "240", "480", "720"],
                "unit_of_measurement" => "min",
                "entity_category" => "config",
                "icon" => "mdi:fan-clock",
            });
        }
        add("select", "temperature_scale", "Temperature scale", fields!{
            "value_template" => tmpl("temperature_scale"),
            "command_topic" => format!("{set}temperature_scale"),
            "options" => vec!["C", "F"],
            "entity_category" => "config",
        });
        add("button", "clear_schedule", "Clear Nest schedule", fields!{
            "command_topic" => format!("{set}clear_schedule"),
            "entity_category" => "config",
            "icon" => "mdi:calendar-remove",
        });

        // Publish what changed; remove what the equipment no longer offers.
        let old = self.announced.lock().unwrap().replace(configs.clone()).unwrap_or_default();
        for topic in old.keys().filter(|t| !configs.contains_key(*t)) {
            self.publish(topic, true, b"");
        }
        let mut n = 0;
        for (topic, config) in &configs {
            if old.get(topic) != Some(config) {
                self.publish(topic, true, config.as_bytes());
                n += 1;
            }
        }
        if n > 0 {
            logf!("mqtt: announced {n} entities");
        }
    }
}

impl mqtt::Handler for Bridge {
    fn connected(&self) {
        Bridge::connected(self);
    }
    fn message(&self, topic: &str, payload: &[u8]) {
        Bridge::message(self, topic, payload);
    }
}

/// A temperature (°C) as the unit's screen shows it: to half degrees in
/// Celsius, to whole degrees in Fahrenheit (given back in °C, the unit Home
/// Assistant is told). The unit stores finer values, such as 19.87 or a
/// dialled 18.59038, so unrounded they read 19.9 where the unit shows 20.
/// The raw/ topics keep them exact.
pub fn shown(v: Option<&Value>, scale: &str) -> Value {
    let Some(c) = number(v) else {
        return v.cloned().unwrap_or(Value::Null);
    };
    if scale == "F" {
        return num(((c * 9.0 / 5.0 + 32.0).round() - 32.0) * 5.0 / 9.0);
    }
    num((c * 2.0).round() / 2.0)
}

fn rfc3339(t: f64) -> Value {
    chrono::DateTime::from_timestamp(t as i64, 0).map_or(Value::Null, |t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string().into())
}

/// What the entities show, from the device and shared buckets.
pub fn state(dev: &Fields, sh: &Fields) -> Fields {
    let now = chrono::Utc::now().timestamp() as f64;
    let mut s = Fields::new();

    let mode = match sprint(sh.get("target_temperature_type")).as_str() {
        "heat" => "heat",
        "cool" => "cool",
        "range" => "heat_cool",
        _ => "off",
    };
    s.insert("mode".into(), mode.into());
    let scale = sprint(dev.get("temperature_scale"));
    s.insert("target".into(), shown(sh.get("target_temperature"), &scale));
    s.insert("target_low".into(), shown(sh.get("target_temperature_low"), &scale));
    s.insert("target_high".into(), shown(sh.get("target_temperature_high"), &scale));
    s.insert("current_temperature".into(), shown(sh.get("current_temperature"), &scale));

    let any = |keys: &[&str]| keys.iter().any(|k| truthy(sh.get(*k)));
    let action = if mode == "off" {
        "off"
    } else if any(&["hvac_heater_state", "hvac_heat_x2_state", "hvac_heat_x3_state", "hvac_aux_heater_state",
                    "hvac_alt_heat_state", "hvac_emer_heat_state"]) {
        "heating"
    } else if any(&["hvac_ac_state", "hvac_cool_x2_state", "hvac_cool_x3_state"]) {
        "cooling"
    } else if truthy(sh.get("hvac_fan_state")) {
        "fan"
    } else {
        "idle"
    };
    s.insert("action".into(), action.into());
    s.insert("heating".into(), (action == "heating").into());
    s.insert("cooling".into(), (action == "cooling").into());
    s.insert("fan_running".into(), truthy(sh.get("hvac_fan_state")).into());

    match number(dev.get("fan_timer_timeout")) {
        Some(t) if t > now => {
            s.insert("fan_mode".into(), "on".into());
            s.insert("fan_timer_end".into(), rfc3339(t));
        }
        _ => {
            s.insert("fan_mode".into(), "auto".into());
            s.insert("fan_timer_end".into(), Value::Null);
        }
    }
    if let Some(d) = number(dev.get("fan_timer_duration")) {
        s.insert("fan_timer_duration".into(), ((d / 60.0) as i64).to_string().into());
    }

    let eco_mode = sprint(dev.get("eco").and_then(|e| e.get("mode")));
    let preset = if eco_mode == "manual-eco" || eco_mode == "auto-eco" { "eco" } else { "none" };
    s.insert("eco_mode".into(), eco_mode.into());
    s.insert("preset".into(), preset.into());

    for k in ["current_humidity", "backplate_temperature", "battery_level", "rssi", "local_ip",
              "current_version", "filter_runtime_sec", "target_humidity", "temperature_scale"] {
        s.insert(k.into(), dev.get(k).cloned().unwrap_or(Value::Null));
    }
    s.insert("away_temperature_low".into(), shown(dev.get("away_temperature_low"), &scale));
    s.insert("away_temperature_high".into(), shown(dev.get("away_temperature_high"), &scale));
    for k in ["leaf", "hot_water_active", "preconditioning_active", "sunlight_correction_active",
              "filter_replacement_needed", "hvac_safety_shutoff_active", "humidifier_state",
              "dehumidifier_state", "learning_mode", "auto_away_enable", "sunlight_correction_enabled",
              "preconditioning_enabled", "away_temperature_low_enabled", "away_temperature_high_enabled",
              "target_humidity_enabled", "hot_water_boiling_state", "heatpump_ready"] {
        s.insert(k.into(), truthy(dev.get(k)).into());
    }
    s.insert("occupancy".into(), (!truthy(sh.get("auto_away"))).into());
    match number(dev.get("time_to_target")) {
        Some(t) if t > now => s.insert("time_to_target".into(), rfc3339(t)),
        _ => s.insert("time_to_target".into(), Value::Null),
    };
    if let Some(v) = number(dev.get("filter_runtime_sec")) {
        s.insert("filter_runtime_h".into(), num((v / 360.0).round() / 10.0));
    }
    s
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::json::canonical;
    use crate::store::tests::temp_store;
    use serde_json::json;

    pub fn test_bridge() -> Arc<Bridge> {
        Bridge::new("nestlocal", "homeassistant", "S", temp_store(), Arc::new(Outdoor::default()), None)
    }

    fn obj(v: Value) -> Fields {
        match canonical(v) {
            Value::Object(m) => m,
            _ => unreachable!(),
        }
    }

    #[test]
    fn state_from_buckets() {
        let future = chrono::Utc::now().timestamp() + 600;
        let dev = obj(json!({"fan_timer_timeout": future, "fan_timer_duration": 900.0, "eco": {"mode": "manual-eco"}}));
        let sh = obj(json!({"target_temperature_type": "range", "hvac_heater_state": true, "auto_away": 1.0}));
        let s = state(&dev, &sh);
        for (k, want) in [("mode", json!("heat_cool")), ("action", json!("heating")), ("heating", json!(true)),
                          ("fan_mode", json!("on")), ("fan_timer_duration", json!("15")), ("preset", json!("eco")),
                          ("occupancy", json!(false))] {
            assert_eq!(s[k], want, "{k}");
        }
        let off = state(&Fields::new(), &obj(json!({"target_temperature_type": "off", "hvac_heater_state": true})));
        assert_eq!(off["action"], json!("off"));
    }

    #[test]
    fn shown_temperatures() {
        for (input, scale, want) in [
            (19.87, "C", 20.0), (19.74, "C", 19.5), (19.76, "C", 20.0), (18.59038, "C", 18.5),
            (22.70377, "C", 22.5), (19.98209, "C", 20.0), (20.0, "C", 20.0),
            (20.0, "F", 20.0),                         // 68 °F
            (19.5, "F", (67.0 - 32.0) * 5.0 / 9.0),     // 67.1 °F shows 67
            (24.44444, "F", (76.0 - 32.0) * 5.0 / 9.0), // an away high set as 76 °F
        ] {
            assert_eq!(shown(Some(&json!(input)), scale), num(want), "shown({input}, {scale})");
        }
        assert_eq!(shown(None, "C"), Value::Null);
        let s = state(&obj(json!({"temperature_scale": "C", "away_temperature_low": 19.98209})),
                      &obj(json!({"current_temperature": 19.87, "target_temperature": 18.59038})));
        for (k, want) in [("current_temperature", 20.0), ("target", 18.5), ("away_temperature_low", 20.0)] {
            assert_eq!(s[k], num(want), "state {k}");
        }
    }

    #[test]
    fn time_answers() {
        let b = test_bridge();
        let mut answered = b.time_ok_rx.lock().unwrap().take().unwrap();
        let now = || format!("{:.3}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64());
        b.time_answer(&now()).unwrap();
        assert!(answered.try_recv().is_err(), "took an answer nobody asked for");
        *b.time_asked.lock().unwrap() = Some(SystemTime::now() - Duration::from_secs(10));
        b.time_answer(&now()).unwrap();
        assert!(answered.try_recv().is_err(), "took a late answer");
        *b.time_asked.lock().unwrap() = Some(SystemTime::now());
        b.time_answer(&now()).unwrap();
        assert!(answered.try_recv().is_ok(), "did not take a prompt answer");
        assert!(b.time_answer("unavailable").is_err(), "took a payload that is not a time");
    }

    #[test]
    fn discovery_follows_the_equipment() {
        let b = test_bridge();
        let dev = obj(json!({"has_fan": true, "temperature_scale": "F", "model_version": "Display-3.4"}));
        let sh = obj(json!({"can_heat": true, "can_cool": false}));
        b.announce(&dev, &sh);
        let configs = b.announced.lock().unwrap().clone().unwrap();
        let climate: Value = serde_json::from_str(&configs["homeassistant/climate/nestlocal_s/thermostat/config"]).unwrap();
        assert_eq!(climate["modes"], json!(["off", "heat"]));
        assert_eq!(climate["precision"], json!(1));
        assert_eq!(climate["name"], Value::Null);
        assert!(climate.get("state_topic").is_none() && climate.get("temperature_low_command_topic").is_none());
        assert!(configs.contains_key("homeassistant/binary_sensor/nestlocal_s/heating/config"));
        assert!(!configs.contains_key("homeassistant/binary_sensor/nestlocal_s/cooling/config"));
        assert!(configs.contains_key("homeassistant/select/nestlocal_s/fan_timer_duration/config"));
    }

    /// Equipment with everything discovery can announce.
    fn everything() -> (Fields, Fields) {
        (obj(json!({"has_fan": true, "has_air_filter": true, "has_hot_water_control": true, "has_humidifier": true,
                    "has_dehumidifier": true, "fan_timer_duration": 900, "filter_runtime_sec": 7200,
                    "temperature_scale": "C"})),
         obj(json!({"can_heat": true, "can_cool": true, "target_temperature_type": "range"})))
    }

    #[test]
    fn every_entity_reads_the_state_and_commands_what_commands_take() {
        let b = test_bridge();
        let (dev, sh) = everything();
        b.store.merge("device.S", &dev, 0, 0, true);
        b.announce(&dev, &sh);
        let configs = b.announced.lock().unwrap().clone().unwrap();
        let s = state(&dev, &sh);
        let mut ids = std::collections::BTreeSet::new();
        for (topic, config) in &configs {
            let c: Value = serde_json::from_str(config).unwrap();
            let id = c["unique_id"].as_str().unwrap();
            assert!(ids.insert(id.to_string()), "{topic}: unique_id {id} used twice");
            assert_eq!(c["object_id"], c["unique_id"], "{topic}");
            let key = id.strip_prefix("nestlocal_s_").unwrap_or_else(|| panic!("{topic}: unique_id {id}"));
            assert!(topic.ends_with(&format!("/nestlocal_s/{key}/config")), "{topic}");
            assert_eq!(c["availability_topic"], json!("nestlocal/S/availability"), "{topic}");
            assert_eq!(c["device"]["identifiers"], json!(["nestlocal_s"]), "{topic}");
            for (k, v) in c.as_object().unwrap() {
                let Some(v) = v.as_str() else { continue };
                // Every key a template reads, the state has.
                for (i, _) in v.match_indices("value_json.") {
                    let field: String = v[i + 11..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                    assert!(s.contains_key(&field), "{topic}: {k} reads {field}, which the state lacks");
                }
                if k.ends_with("state_topic") {
                    assert_eq!(v, "nestlocal/S/state", "{topic}: {k}");
                }
                // Every command topic names a command (for a payload the
                // entity would send).
                if k.ends_with("command_topic") {
                    let name = v.strip_prefix("nestlocal/S/set/").unwrap_or_else(|| panic!("{topic}: {k} {v}"));
                    let arg = match name {
                        "mode" => "heat",
                        "preset" => "eco",
                        "fan_mode" => "auto",
                        "fan_timer_duration" => "30",
                        "temperature_scale" => "F",
                        "target_humidity" => "40",
                        "clear_schedule" => "PRESS",
                        n if n.ends_with("_enabled") => "ON",
                        "away_temperature_high" => "28",
                        n if n.starts_with("target") || n.starts_with("away_temperature_") => "20",
                        _ => "ON",
                    };
                    assert!(b.fields(name, arg).is_ok(), "{topic}: {k} {name} {arg}: {:?}", b.fields(name, arg));
                }
            }
        }
        // Every fan timer option is one the fan_timer_duration command takes,
        // and the state shows the unit's own the same way.
        let select: Value = serde_json::from_str(&configs["homeassistant/select/nestlocal_s/fan_timer_duration/config"]).unwrap();
        for option in select["options"].as_array().unwrap() {
            assert!(b.fields("fan_timer_duration", option.as_str().unwrap()).is_ok(), "{option}");
        }
        assert!(select["options"].as_array().unwrap().contains(&s["fan_timer_duration"]));
        assert_eq!(s["filter_runtime_h"], json!(2));
    }

    #[test]
    fn number_entities_offer_what_their_commands_take() {
        let b = test_bridge();
        let (dev, sh) = everything();
        b.announce(&dev, &sh);
        let configs = b.announced.lock().unwrap().clone().unwrap();
        let numbers: Vec<Value> = configs.iter().filter(|(topic, _)| topic.starts_with("homeassistant/number/"))
            .map(|(_, c)| serde_json::from_str(c).unwrap()).collect();
        assert_eq!(numbers.len(), 3);
        for c in numbers {
            let name = c["command_topic"].as_str().unwrap().strip_prefix("nestlocal/S/set/").unwrap();
            let (min, max) = (c["min"].as_f64().unwrap(), c["max"].as_f64().unwrap());
            for (v, ok) in [(min, true), (max, true), (min - 0.5, false), (max + 0.5, false)] {
                assert_eq!(b.fields(name, &v.to_string()).is_ok(), ok, "{name} {v}");
            }
        }
    }

    #[test]
    fn state_with_nothing_reported() {
        let s = state(&Fields::new(), &Fields::new());
        for (k, want) in [("mode", json!("off")), ("action", json!("off")), ("target", Value::Null), ("fan_mode", json!("auto")),
                          ("fan_timer_end", Value::Null), ("preset", json!("none")), ("eco_mode", json!("<nil>")),
                          ("occupancy", json!(true)), ("time_to_target", Value::Null), ("leaf", json!(false))] {
            assert_eq!(s[k], want, "{k}");
        }
        assert!(s.get("fan_timer_duration").is_none() && s.get("filter_runtime_h").is_none());
        // A fan timer or time to temperature already past reads as none.
        let past = chrono::Utc::now().timestamp() - 1;
        let s = state(&obj(json!({"fan_timer_timeout": past, "time_to_target": past})), &obj(json!({"target_temperature_type": "cool",
            "hvac_fan_state": true, "current_temperature": "n/a"})));
        assert_eq!((&s["fan_mode"], &s["time_to_target"], &s["action"]), (&json!("auto"), &Value::Null, &json!("fan")));
        assert_eq!(s["current_temperature"], json!("n/a"), "a value that is no number passes as it is");
        let future = chrono::Utc::now().timestamp() + 3600;
        let s = state(&obj(json!({"time_to_target": future, "eco": {"mode": "auto-eco"}})), &obj(json!({"target_temperature_type": "cool",
            "hvac_ac_state": true, "hvac_fan_state": true})));
        assert_eq!((&s["action"], &s["cooling"], &s["preset"]), (&json!("cooling"), &json!(true), &json!("eco")));
        assert_eq!(s["time_to_target"], rfc3339(future as f64));
    }

    /// The next PUBLISH the loopback broker gets that matches: its topic,
    /// payload and retain flag.
    async fn published(broker: &mut tokio::net::TcpStream, buf: &mut bytes::BytesMut,
                       want: impl Fn(&str, &[u8]) -> bool) -> (String, Vec<u8>, bool) {
        let read = async {
            loop {
                let (header, body) = crate::mqtt::tests::read_packet(broker, buf).await;
                if header >> 4 != 3 {
                    continue;
                }
                let (topic, payload) = crate::mqtt::tests::publish_of(header, &body);
                if want(&topic, &payload) {
                    return (topic, payload, header & 1 == 1);
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), read).await.expect("no such publish")
    }

    #[tokio::test]
    async fn bridge_over_a_loopback_broker() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let b = test_bridge();
        let (dev, sh) = everything();
        b.store.merge("device.S", &dev, 0, 0, true);
        b.store.merge("shared.S", &sh, 0, 0, true);
        let broker = mqtt::Broker::parse(&format!("tcp://{}", listener.local_addr().unwrap())).unwrap();
        b.run(&broker, "", "", None);
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut buf = bytes::BytesMut::new();
        crate::mqtt::tests::read_packet(&mut conn, &mut buf).await; // CONNECT
        tokio::io::AsyncWriteExt::write_all(&mut conn, &[0x20, 2, 0, 0]).await.unwrap();

        // On connecting: availability (offline: no nlclient to vouch for the
        // unit), discovery and the state, all retained.
        let (_, payload, retain) = published(&mut conn, &mut buf, |t, _| t == "nestlocal/S/availability").await;
        assert_eq!((payload.as_slice(), retain), (&b"offline"[..], true));
        let cooling = "homeassistant/binary_sensor/nestlocal_s/cooling/config";
        let (_, payload, retain) = published(&mut conn, &mut buf, |t, _| t == cooling).await;
        assert!(retain && !payload.is_empty());
        let (_, payload, _) = published(&mut conn, &mut buf, |t, _| t == "nestlocal/S/state").await;
        assert_eq!(serde_json::from_slice::<Value>(&payload).unwrap()["mode"], json!("heat_cool"));

        // A command changes the bucket; the bridge publishes the bucket and
        // the state again.
        let set = |topic: &str, payload: &[u8]| crate::mqtt::tests::publish_packet(topic, payload);
        tokio::io::AsyncWriteExt::write_all(&mut conn, &set("nestlocal/S/set/target_low", b" 18.7 \n")).await.unwrap();
        let (_, payload, _) = published(&mut conn, &mut buf, |t, _| t == "nestlocal/S/raw/shared").await;
        let raw: Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!((&raw["target_temperature_low"], &raw["target_change_pending"]), (&json!(18.5), &json!(true)));

        // Equipment that can no longer cool: its entities are removed.
        tokio::io::AsyncWriteExt::write_all(&mut conn, &set("nestlocal/S/set/raw/shared", br#"{"can_cool": false}"#)).await.unwrap();
        let (_, payload, retain) = published(&mut conn, &mut buf, |t, _| t == cooling).await;
        assert!(payload.is_empty() && retain, "cooling entity not removed");
        // Home Assistant back: everything announced again, the unchanged too.
        tokio::io::AsyncWriteExt::write_all(&mut conn, &set("homeassistant/status", b"online")).await.unwrap();
        let (_, payload, _) = published(&mut conn, &mut buf,
            |t, _| t == "homeassistant/binary_sensor/nestlocal_s/leaf/config").await;
        assert_eq!(serde_json::from_slice::<Value>(&payload).unwrap()["unique_id"], json!("nestlocal_s_leaf"));
    }
}
