//! A Nest Temperature Sensor that is not there. nlclient regulates from a
//! sensor it has selected and goes back to the backplate by itself when that
//! sensor falls silent, so nestlocal gives it one: a sensor registered as a
//! paired one is, whose beacons come from an external thermometer's readings
//! (set/external_temperature, or a Zigbee or Matter write) through a stand-in
//! for the BLE chip (port.rs, chip.rs, beacon.rs).

pub mod beacon;
pub mod chip;
#[cfg(target_os = "linux")]
pub mod port;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fields;
use crate::logf;
use crate::nlsettings;
use crate::store::Store;

/// How often a reading goes out again, as a KR1 reports (its sample_interval).
const REPEAT: Duration = Duration::from_secs(60);
/// How long nlclient keeps a silent sensor before going back to the backplate.
const SILENCE: Duration = Duration::from_secs(693);
/// Sequence numbers a run may use before the file records more, so that NAND
/// is written about once a day while no number is ever used twice.
const RESERVE: u32 = 1440;
/// The readings the sensor's beacon carries, °C.
const RANGE: (f64, f64) = (-30.0, 60.0);
/// How long after nlclient turns sensors off it is told again, past its
/// update's answer.
#[cfg(not(test))]
const SELECT_AFTER: Duration = Duration::from_secs(2);
#[cfg(test)]
const SELECT_AFTER: Duration = Duration::from_millis(10);
const DESCRIPTION: &str = "External";
const DEFAULT_WHERE: &str = "00000000-0000-0000-0000-00010000000c";

/// Who the sensor is, kept between runs: the key only nlclient and nestlocal
/// hold, and the sequence numbers already spoken for.
#[derive(Serialize, Deserialize)]
struct Identity {
    id: String,
    key: String,
    sequence: u32,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_key(text: &str) -> Option<[u8; 16]> {
    let mut key = [0u8; 16];
    if text.len() != 32 || !text.is_ascii() {
        return None;
    }
    for (byte, pair) in key.iter_mut().zip(text.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(key)
}

/// A sensor's id for this unit, in a real KR1's form: the same every time.
fn sensor_id(serial: &str) -> String {
    let digest = Md5::digest(serial.as_bytes());
    format!("18B430{}", hex(&digest[..5]).to_uppercase())
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let value = u32::from(chunk[0]) << 16 | u32::from(*chunk.get(1).unwrap_or(&0)) << 8 | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(value >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn read_identity(path: &Path) -> Option<Identity> {
    let identity: Identity = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    parse_key(&identity.key)?;
    u64::from_str_radix(&identity.id, 16).ok()?;
    Some(identity)
}

fn write_identity(path: &Path, identity: &Identity) -> io::Result<()> {
    let bytes = serde_json::to_vec(identity).map_err(io::Error::other)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".partial");
    {
        use std::os::unix::fs::OpenOptionsExt;
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// The identity at path, made on first use.
fn load_identity(path: &Path, serial: &str) -> io::Result<Identity> {
    if let Some(identity) = read_identity(path) {
        return Ok(identity);
    }
    let mut key = [0u8; 16];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(&mut key)?;
    let identity = Identity { id: sensor_id(serial), key: hex(&key), sequence: 1 };
    write_identity(path, &identity)?;
    Ok(identity)
}

/// Time since boot, counting sleep: a reading ages while the unit sleeps.
pub fn boot_time() -> Duration {
    #[cfg(target_os = "linux")]
    const CLOCK: libc::clockid_t = libc::CLOCK_BOOTTIME;
    #[cfg(not(target_os = "linux"))]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: now is a valid timespec for the call to fill.
    unsafe { libc::clock_gettime(CLOCK, &mut now) };
    Duration::new(now.tv_sec as u64, now.tv_nsec as u32)
}

struct State {
    /// The reading and when it came.
    reading: Option<(i16, Duration)>,
    /// A reading not yet sent.
    pending: bool,
    last_beacon: Option<Duration>,
    sequence: u32,
    reserved: u32,
    /// What the last publish said, to tell Home Assistant only of changes.
    last_shown: (&'static str, bool),
    /// The one beacon before any reading has gone out.
    introduced: bool,
}

/// The sensor nlclient hears.
pub struct Sensor {
    pub bucket: String,
    id: u64,
    key: [u8; 16],
    path: PathBuf,
    timeout: Duration,
    state: Mutex<State>,
    /// Interrupts the port's wait for a new reading (port.rs).
    notify: OnceLock<Box<dyn Fn() + Send + Sync>>,
    on_change: OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// The temperature of the beacon before any reading, °C.
    introduction: OnceLock<Box<dyn Fn() -> Option<f64> + Send + Sync>>,
}

impl Sensor {
    /// The sensor kept at path, past every sequence number the settings say
    /// nlclient has taken.
    pub fn open(path: &Path, serial: &str, settings: &Path, timeout: Duration) -> io::Result<Arc<Sensor>> {
        let identity = load_identity(path, serial)?;
        let bucket = format!("kryptonite.{}", identity.id);
        let taken = std::fs::read(settings).ok().and_then(|b| {
            let value = nlsettings::value(&b, &format!("{bucket}.seqno"))?.to_vec();
            String::from_utf8(value).ok()?.parse::<u32>().ok()
        });
        let sensor = Sensor {
            bucket,
            id: u64::from_str_radix(&identity.id, 16).map_err(io::Error::other)?,
            key: parse_key(&identity.key).ok_or_else(|| io::Error::other("bad key"))?,
            path: path.to_path_buf(),
            timeout,
            state: Mutex::new(State {
                reading: None,
                pending: false,
                last_beacon: None,
                sequence: identity.sequence.max(taken.map_or(0, |t| t.saturating_add(1))),
                reserved: 0,
                last_shown: ("backplate", false),
                introduced: false,
            }),
            notify: OnceLock::new(),
            on_change: OnceLock::new(),
            introduction: OnceLock::new(),
        };
        Ok(Arc::new(sensor))
    }

    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.on_change.set(Box::new(f));
    }

    pub fn introduce_with(&self, temperature: impl Fn() -> Option<f64> + Send + Sync + 'static) {
        let _ = self.introduction.set(Box::new(temperature));
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn set_notify(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.notify.set(Box::new(f));
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// A reading from the thermometer, °C.
    pub fn set(&self, celsius: f64) -> Result<(), String> {
        if !celsius.is_finite() || !(RANGE.0..=RANGE.1).contains(&celsius) {
            return Err(format!("expected a temperature, {}-{} °C", RANGE.0, RANGE.1));
        }
        let tenths = beacon::tenths(celsius).ok_or("out of the sensor's range")?;
        {
            let mut state = self.state.lock().unwrap();
            state.reading = Some((tenths, boot_time()));
            state.pending = true;
        }
        if let Some(notify) = self.notify.get() {
            notify();
        }
        self.publish_changes(true);
        Ok(())
    }

    /// Whether a reading has come since nestlocal started.
    pub fn has_reading(&self) -> bool {
        self.state.lock().unwrap().reading.is_some()
    }

    /// The reading while it is fresh, °C.
    pub fn reading(&self) -> Option<f64> {
        let state = self.state.lock().unwrap();
        self.fresh(&state, boot_time()).map(|tenths| f64::from(tenths) / 10.0)
    }

    fn fresh(&self, state: &State, now: Duration) -> Option<i16> {
        state.reading.filter(|(_, at)| now.saturating_sub(*at) < self.timeout).map(|(tenths, _)| tenths)
    }

    /// What regulates: the sensor while its beacons keep nlclient on it.
    pub fn source(&self) -> &'static str {
        let state = self.state.lock().unwrap();
        Self::source_of(&state, boot_time())
    }

    fn source_of(state: &State, now: Duration) -> &'static str {
        match state.last_beacon {
            Some(at) if now.saturating_sub(at) < SILENCE => "external",
            _ => "backplate",
        }
    }

    /// Tells Home Assistant when what it shows has changed.
    fn publish_changes(&self, always: bool) {
        let changed = {
            let mut state = self.state.lock().unwrap();
            let now = boot_time();
            let shown = (Self::source_of(&state, now), self.fresh(&state, now).is_some());
            let changed = shown != state.last_shown;
            state.last_shown = shown;
            changed
        };
        if changed || always {
            if let Some(f) = self.on_change.get() {
                f();
            }
        }
    }

    /// Called now and then by the port while nlclient is not listening: time
    /// passes for what Home Assistant shows.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn quiet_tick(&self) {
        self.publish_changes(false);
    }

    /// Called now and then by the port: the beacon due, if one is.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn due(&self) -> Option<[u8; beacon::REPORT_LEN]> {
        let report = self.next_report(boot_time());
        self.publish_changes(false);
        report
    }

    fn next_report(&self, now: Duration) -> Option<[u8; beacon::REPORT_LEN]> {
        let mut state = self.state.lock().unwrap();
        let Some(tenths) = self.fresh(&state, now) else {
            return self.introduction_report(&mut state);
        };
        let quiet = state.last_beacon.is_none_or(|at| now.saturating_sub(at) >= REPEAT);
        if !state.pending && !quiet {
            return None;
        }
        let sequence = self.next_sequence(&mut state)?;
        state.pending = false;
        state.last_beacon = Some(now);
        Some(beacon::report(self.id, &self.key, sequence, tenths))
    }

    /// nlclient lists a sensor on its Sensors menu once it has heard it, so it
    /// hears this one once before any reading. Only once: a sensor chosen on
    /// that menu falls silent and nlclient goes back to the backplate, where
    /// repeating the unit's own temperature to it would regulate from an echo.
    fn introduction_report(&self, state: &mut State) -> Option<[u8; beacon::REPORT_LEN]> {
        if state.reading.is_some() || state.introduced {
            return None;
        }
        let tenths = self.introduction.get().and_then(|temperature| temperature()).and_then(beacon::tenths)?;
        let sequence = self.next_sequence(state)?;
        state.introduced = true;
        logf!("external temperature: introducing the sensor to nlclient at {} °C", f64::from(tenths) / 10.0);
        Some(beacon::report(self.id, &self.key, sequence, tenths))
    }

    fn next_sequence(&self, state: &mut State) -> Option<u32> {
        if state.sequence > beacon::MAX_SEQUENCE {
            return None;
        }
        if state.sequence >= state.reserved {
            let reserved = state.sequence + RESERVE;
            let identity = Identity { id: format!("{:016X}", self.id), key: hex(&self.key), sequence: reserved };
            if let Err(e) = write_identity(&self.path, &identity) {
                logf!("external temperature: {}: {e}", self.path.display());
                return None;
            }
            state.reserved = reserved;
        }
        let sequence = state.sequence;
        state.sequence += 1;
        Some(sequence)
    }
}

/// nlclient's settings as they must be when it starts: the sensor's key and
/// place, which it reads for a paired sensor.
pub fn prepare_settings(path: &Path, settings: &Path, serial: &str, where_id: Option<&str>) -> io::Result<()> {
    let identity = load_identity(path, serial)?;
    let bucket = format!("kryptonite.{}", identity.id);
    let key = parse_key(&identity.key).ok_or_else(|| io::Error::other("bad key"))?;
    let original = std::fs::read(settings)?;
    let mut b = original.clone();
    let where_id = where_id.unwrap_or(DEFAULT_WHERE);
    for (name, value) in [("key", base64(&key)), ("whereid", where_id.to_string()), ("desc", DESCRIPTION.to_string())] {
        b = nlsettings::set(&b, &format!("{bucket}.{name}"), &value)?;
    }
    if b != original {
        nlsettings::write(settings, &b)?;
    }
    Ok(())
}

/// What nlclient keeps of the sensor gone, before it starts on a unit that no
/// longer has -external-temperature, or it brings the sensor and its menu back:
/// its settings, and its files beside the sensor's identity. Other sensors
/// stay. The settings go last: a run they survive is done again.
pub fn forget_settings(path: &Path, settings: &Path) -> io::Result<()> {
    let Some(identity) = read_identity(path) else { return Ok(()) };
    let bucket = format!("kryptonite.{}", identity.id);
    let original = std::fs::read(settings)?;
    if nlsettings::value(&original, &format!("{bucket}.key")).is_none() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        forget_files(dir, &bucket)?;
    }
    let mut b = original;
    for name in ["key", "whereid", "desc", "seqno"] {
        b = nlsettings::remove(&b, &format!("{bucket}.{name}"));
    }
    let associated = nlsettings::value(&b, "associated_rcs_sensors").unwrap_or_default();
    let others: String = String::from_utf8_lossy(associated).split(',')
        .filter(|sensor| !sensor.is_empty() && *sensor != bucket)
        .map(|sensor| format!("{sensor},")).collect();
    if others.is_empty() {
        b = nlsettings::remove(&b, "associated_rcs_sensors");
        b = nlsettings::remove(&b, "is_onboarded_to_rcs");
    } else {
        b = nlsettings::set(&b, "associated_rcs_sensors", &others)?;
    }
    nlsettings::write(settings, &b)
}

fn forget_files(dir: &Path, bucket: &str) -> io::Result<()> {
    match std::fs::remove_file(dir.join(format!("rcs.{bucket}.recovery"))) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let manager = dir.join("rcsmanager.json");
    if let Some(mut state) = read_json(&manager) {
        if let Some(sensors) = state.get_mut("associated_sensor_info").and_then(Value::as_array_mut) {
            sensors.retain(|sensor| sensor.get("ID").and_then(Value::as_str) != Some(bucket));
        }
        if listed(state.get("associated_sensor_info")).is_empty() {
            std::fs::remove_file(&manager)?;
        } else {
            if state.get("active_sensors").and_then(Value::as_str) == Some(bucket) {
                state["active_sensors"] = "".into();
                state["selected_sensor_type"] = "BACKPLATE".into();
            }
            std::fs::write(&manager, state.to_string())?;
        }
    }
    let prediction = dir.join("predictionmanager.json");
    if let Some(mut state) = read_json(&prediction) {
        let models = state.get_mut("RemoteModels").and_then(Value::as_object_mut);
        if models.is_some_and(|models| models.remove(bucket).is_some()) {
            std::fs::write(&prediction, state.to_string())?;
        }
    }
    Ok(())
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn structure_key(store: &Store, serial: &str) -> Option<String> {
    let link = store.get(&format!("link.{serial}"))?;
    link.value.get("structure").and_then(Value::as_str).map(str::to_string)
}

fn listed(value: Option<&Value>) -> Vec<Value> {
    value.and_then(Value::as_array).cloned().unwrap_or_default()
}

/// The buckets of a paired sensor, with the backplate chosen until a reading
/// comes (`select`). nlclient reads the sensor's existence from the structure's
/// swarm and the rcs_settings bucket.
pub fn ensure_buckets(store: &Arc<Store>, serial: &str, bucket: &str, where_id: Option<&str>) {
    let Some(structure) = structure_key(store, serial) else {
        logf!("external temperature: unit {serial} has no structure");
        return;
    };
    let id = bucket.split_once('.').map_or(bucket, |(_, id)| id);
    let mut swarm = listed(store.get(&structure).as_ref().and_then(|o| o.value.get("swarm")));
    if !swarm.iter().any(|v| v == bucket) {
        swarm.push(bucket.into());
        store.merge(&structure, &fields!{"swarm" => swarm}, 0, 0, false);
    }
    store.merge(bucket, &fields!{
        "model" => "KR1",
        "serial_number" => id,
        "where_id" => where_id.unwrap_or(DEFAULT_WHERE),
        "structure_id" => structure.split_once('.').map_or("", |(_, id)| id),
        "report_period_in_seconds" => 60,
    }, 0, 0, false);
    choose(store, serial, bucket, false);
}

/// The sensor chosen, for its first reading to regulate.
pub fn select(store: &Store, serial: &str, bucket: &str) {
    choose(store, serial, bucket, true);
}

fn choose(store: &Store, serial: &str, bucket: &str, active: bool) {
    let key = format!("rcs_settings.{serial}");
    let mut associated = listed(store.get(&key).as_ref().and_then(|o| o.value.get("associated_rcs_sensors")));
    if !associated.iter().any(|v| v == bucket) {
        associated.push(bucket.into());
    }
    store.merge(&key, &fields!{
        "active_rcs_sensors" => if active { vec![Value::from(bucket)] } else { Vec::new() },
        "associated_rcs_sensors" => associated,
        "rcs_control_setting" => "OVERRIDE",
        "multiroom_active" => false,
        "thermostat_alert" => "OFF",
    }, 0, 0, false);
}

/// nlclient turns sensors OFF while it starts, before it has heard of them:
/// it is told again whenever it says so, the sensor chosen once a reading has
/// come. A selection of the backplate on its screen stays: that one comes with
/// OVERRIDE.
///
/// Not at once: this watch runs inside nlclient's own update, whose answer
/// would carry the new revision and timestamp without the values, and nlclient
/// would take itself to be up to date. Later, the change reaches it as a push.
pub fn keep_selected(store: &Arc<Store>, serial: &str, sensor: &Arc<Sensor>) {
    let key = format!("rcs_settings.{serial}");
    let (again, serial, sensor) = (store.clone(), serial.to_string(), sensor.clone());
    let waiting = Arc::new(AtomicBool::new(false));
    store.watch(move |o| {
        if o.key != key || o.value.get("rcs_control_setting").and_then(Value::as_str) != Some("OFF")
            || waiting.swap(true, Ordering::SeqCst) {
            return;
        }
        let (store, serial, sensor, waiting) = (again.clone(), serial.clone(), sensor.clone(), waiting.clone());
        std::thread::spawn(move || {
            std::thread::sleep(SELECT_AFTER);
            waiting.store(false, Ordering::SeqCst);
            choose(&store, &serial, &sensor.bucket, sensor.has_reading());
        });
    });
}

/// The sensor gone from a unit that no longer has -external-temperature.
pub fn withdraw(path: &Path, store: &Store, serial: &str) {
    let Some(identity) = read_identity(path) else { return };
    let bucket = format!("kryptonite.{}", identity.id);
    let without = |list: Vec<Value>| list.into_iter().filter(|v| v != bucket.as_str()).collect::<Vec<_>>();
    if let Some(structure) = structure_key(store, serial).and_then(|key| store.get(&key)) {
        let swarm = listed(structure.value.get("swarm"));
        if swarm.iter().any(|v| v == bucket.as_str()) {
            store.merge(&structure.key, &fields!{"swarm" => without(swarm)}, 0, 0, false);
        }
    }
    if let Some(settings) = store.get(&format!("rcs_settings.{serial}")) {
        let active = listed(settings.value.get("active_rcs_sensors"));
        let associated = listed(settings.value.get("associated_rcs_sensors"));
        if active.iter().chain(&associated).any(|v| v == bucket.as_str()) {
            let associated = without(associated);
            let control = if associated.is_empty() { "OFF" } else { "OVERRIDE" };
            store.merge(&settings.key, &fields!{
                "active_rcs_sensors" => without(active), "associated_rcs_sensors" => associated,
                "rcs_control_setting" => control,
            }, 0, 0, false);
        }
    }
}

/// Whether nlclient has this sensor selected, as opposed to the backplate.
pub fn selected(store: &Store, serial: &str, bucket: &str) -> bool {
    store.get(&format!("rcs_settings.{serial}"))
        .is_some_and(|o| listed(o.value.get("active_rcs_sensors")).iter().any(|v| v == bucket))
}

/// The unit's room, as its device bucket names it.
pub fn where_id(store: &Store, serial: &str) -> Option<String> {
    store.get(&format!("device.{serial}"))?.value.get("where_id")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::temp_store;
    use serde_json::json;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nestlocal-sensor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sensor(name: &str) -> (Arc<Sensor>, PathBuf) {
        let dir = dir(name);
        let path = dir.join("sensor.json");
        let sensor = Sensor::open(&path, "S", &dir.join("none"), Duration::from_secs(1800)).unwrap();
        (sensor, path)
    }

    #[test]
    fn an_identity_is_made_once_and_kept() {
        let dir = dir("identity");
        let path = dir.join("sensor.json");
        let first = load_identity(&path, "09AA01AC25160NMZ").unwrap();
        assert_eq!(first.id.len(), 16);
        assert!(first.id.starts_with("18B430"));
        let again = load_identity(&path, "09AA01AC25160NMZ").unwrap();
        assert_eq!((first.id, first.key), (again.id, again.key));
    }

    #[test]
    fn readings_outside_the_range_are_refused() {
        let (sensor, _) = sensor("range");
        assert!(sensor.set(60.5).is_err());
        assert!(sensor.set(f64::NAN).is_err());
        assert_eq!(sensor.reading(), None);
        sensor.set(20.54).unwrap();
        assert_eq!(sensor.reading(), Some(20.5));
    }

    #[test]
    fn a_reading_goes_out_at_once_then_every_minute_until_it_is_stale() {
        let (sensor, _) = sensor("repeat");
        let start = boot_time();
        assert!(sensor.next_report(start).is_none(), "no reading yet");
        sensor.set(20.0).unwrap();
        let start = boot_time();
        let first = sensor.next_report(start).expect("a new reading goes out at once");
        assert!(sensor.next_report(start + Duration::from_secs(59)).is_none());
        let second = sensor.next_report(start + Duration::from_secs(60)).expect("every minute");
        assert_ne!(first, second, "every beacon has a sequence number of its own");
        assert!(sensor.next_report(start + Duration::from_secs(1801)).is_none(), "stale: nlclient falls back");
    }

    #[test]
    fn the_source_follows_the_beacons_not_the_reading() {
        let (sensor, _) = sensor("source");
        assert_eq!(sensor.source(), "backplate");
        sensor.set(20.0).unwrap();
        assert_eq!(sensor.source(), "backplate", "nothing sent yet");
        let now = boot_time();
        sensor.next_report(now).unwrap();
        let state = sensor.state.lock().unwrap();
        assert_eq!(Sensor::source_of(&state, now + Duration::from_secs(692)), "external");
        assert_eq!(Sensor::source_of(&state, now + Duration::from_secs(693)), "backplate");
    }

    #[test]
    fn sequence_numbers_never_repeat_across_runs() {
        let dir = dir("sequence");
        let (path, settings) = (dir.join("sensor.json"), dir.join("settings"));
        let mut seen = Vec::new();
        for run in 0..3 {
            let sensor = Sensor::open(&path, "S", &settings, Duration::from_secs(1800)).unwrap();
            sensor.set(20.0).unwrap();
            let now = boot_time();
            sensor.next_report(now).unwrap();
            sensor.next_report(now + Duration::from_secs(60)).unwrap();
            let state = sensor.state.lock().unwrap();
            seen.push(state.sequence);
            assert!(state.reserved > state.sequence, "run {run} reserved ahead");
        }
        assert!(seen.windows(2).all(|w| w[1] >= w[0] + RESERVE - 2), "{seen:?}");
    }

    #[test]
    fn the_sequence_starts_past_what_nlclient_has_taken() {
        let dir = dir("taken");
        let (path, settings) = (dir.join("sensor.json"), dir.join("settings"));
        load_identity(&path, "S").unwrap();
        let bucket = format!("kryptonite.{}", sensor_id("S"));
        std::fs::write(&settings, format!("<settings>\n  <s key=\"{bucket}.seqno\" value=\"5000\"/>\n</settings>\n")).unwrap();
        let sensor = Sensor::open(&path, "S", &settings, Duration::from_secs(1800)).unwrap();
        assert_eq!(sensor.state.lock().unwrap().sequence, 5001);
    }

    #[test]
    fn the_settings_get_the_key_nlclient_reads() {
        let dir = dir("settings");
        let (path, settings) = (dir.join("sensor.json"), dir.join("settings"));
        std::fs::write(&settings, "<settings>\n  <s key=\"a\" value=\"1\"/>\n</settings>\n").unwrap();
        prepare_settings(&path, &settings, "S", Some("room")).unwrap();
        let identity = read_identity(&path).unwrap();
        let b = std::fs::read(&settings).unwrap();
        let bucket = format!("kryptonite.{}", identity.id);
        assert_eq!(nlsettings::value(&b, &format!("{bucket}.key")).unwrap(), base64(&parse_key(&identity.key).unwrap()).as_bytes());
        assert_eq!(nlsettings::value(&b, &format!("{bucket}.whereid")).unwrap(), b"room");
        assert_eq!(nlsettings::value(&b, "a").unwrap(), b"1", "the rest stays");
    }

    #[test]
    fn base64_as_nlclient_decodes_it() {
        assert_eq!(base64(&parse_key("00112233445566778899aabbccddeeff").unwrap()), "ABEiM0RVZneImaq7zN3u/w==");
    }

    #[test]
    fn the_buckets_register_the_sensor_and_a_reading_selects_it() {
        let store = temp_store();
        store.merge("link.S", &fields!{"structure" => "structure.home"}, 0, 0, false);
        store.merge("structure.home", &fields!{"swarm" => vec![Value::from("kryptonite.REAL")]}, 0, 0, false);
        ensure_buckets(&store, "S", "kryptonite.AB", Some("room"));
        let value = |key: &str, field: &str| store.get(key).unwrap().value.get(field).cloned().unwrap();
        assert_eq!(value("structure.home", "swarm"), json!(["kryptonite.REAL", "kryptonite.AB"]));
        assert_eq!(value("kryptonite.AB", "where_id"), json!("room"));
        assert_eq!(value("rcs_settings.S", "associated_rcs_sensors"), json!(["kryptonite.AB"]));
        assert_eq!(value("rcs_settings.S", "active_rcs_sensors"), json!([]), "the backplate until a reading");
        assert_eq!(value("rcs_settings.S", "rcs_control_setting"), json!("OVERRIDE"));
        select(&store, "S", "kryptonite.AB");
        assert_eq!(value("rcs_settings.S", "active_rcs_sensors"), json!(["kryptonite.AB"]));
    }

    #[test]
    fn nlclient_turning_sensors_off_is_answered_by_what_the_readings_call_for() {
        let dir = dir("keep");
        let sensor = Sensor::open(&dir.join("sensor.json"), "S", &dir.join("none"), Duration::from_secs(1800)).unwrap();
        let store = temp_store();
        store.merge("link.S", &fields!{"structure" => "structure.home"}, 0, 0, false);
        ensure_buckets(&store, "S", &sensor.bucket, None);
        keep_selected(&store, "S", &sensor);
        let setting = || store.get("rcs_settings.S").unwrap().value;
        let off = || store.merge("rcs_settings.S", &fields!{"rcs_control_setting" => "OFF", "active_rcs_sensors" => Vec::<Value>::new()}, 0, 0, true);
        off();
        assert_eq!(setting().get("rcs_control_setting").unwrap(), "OFF", "not within nlclient's update");
        std::thread::sleep(SELECT_AFTER * 20);
        assert_eq!(setting().get("rcs_control_setting").unwrap(), "OVERRIDE");
        assert_eq!(setting().get("active_rcs_sensors").unwrap(), &json!([]), "no reading yet");
        sensor.set(20.0).unwrap();
        off();
        std::thread::sleep(SELECT_AFTER * 20);
        assert_eq!(setting().get("active_rcs_sensors").unwrap(), &json!([sensor.bucket]));
        store.merge("rcs_settings.S", &fields!{"active_rcs_sensors" => Vec::<Value>::new()}, 0, 0, true);
        assert_eq!(setting().get("active_rcs_sensors").unwrap(), &json!([]), "the backplate chosen on the screen stays");
    }

    #[test]
    fn nlclient_hears_the_sensor_once_before_any_reading() {
        let dir = dir("introduce");
        let sensor = Sensor::open(&dir.join("sensor.json"), "S", &dir.join("none"), Duration::from_secs(1800)).unwrap();
        let start = boot_time();
        assert!(sensor.next_report(start).is_none(), "nothing to tell it yet");
        sensor.introduce_with(|| Some(20.5));
        assert!(sensor.next_report(start).is_some());
        assert!(sensor.next_report(start + REPEAT * 10).is_none(), "only once");
        assert_eq!(sensor.source(), "backplate", "it regulates from nothing");
        assert_eq!(sensor.reading(), None);
        sensor.set(21.0).unwrap();
        assert!(sensor.next_report(start).is_some(), "a reading goes out at once");
        assert_eq!(sensor.source(), "external");
    }

    #[test]
    fn an_unusable_temperature_introduces_nothing_until_a_usable_one() {
        let dir = dir("introduce-later");
        let sensor = Sensor::open(&dir.join("sensor.json"), "S", &dir.join("none"), Duration::from_secs(1800)).unwrap();
        let known = Arc::new(AtomicBool::new(false));
        let reported = known.clone();
        sensor.introduce_with(move || reported.load(Ordering::SeqCst).then_some(20.0));
        assert!(sensor.next_report(boot_time()).is_none());
        known.store(true, Ordering::SeqCst);
        assert!(sensor.next_report(boot_time()).is_some());
    }

    #[test]
    fn the_sensor_counts_as_selected_only_while_nlclient_has_it_active() {
        let store = temp_store();
        assert!(!selected(&store, "S", "kryptonite.AB"));
        store.merge("link.S", &fields!{"structure" => "structure.home"}, 0, 0, false);
        ensure_buckets(&store, "S", "kryptonite.AB", None);
        assert!(!selected(&store, "S", "kryptonite.AB"));
        select(&store, "S", "kryptonite.AB");
        assert!(selected(&store, "S", "kryptonite.AB"));
        store.merge("rcs_settings.S", &fields!{"active_rcs_sensors" => Vec::<Value>::new()}, 0, 0, true);
        assert!(!selected(&store, "S", "kryptonite.AB"));
    }

    #[test]
    fn withdrawing_takes_the_sensor_out_and_leaves_real_ones() {
        let dir = dir("withdraw");
        let path = dir.join("sensor.json");
        let identity = load_identity(&path, "S").unwrap();
        let bucket = format!("kryptonite.{}", identity.id);
        let store = temp_store();
        store.merge("link.S", &fields!{"structure" => "structure.home"}, 0, 0, false);
        store.merge("structure.home", &fields!{"swarm" => vec![Value::from("kryptonite.REAL")]}, 0, 0, false);
        ensure_buckets(&store, "S", &bucket, None);
        withdraw(&path, &store, "S");
        let value = |key: &str, field: &str| store.get(key).unwrap().value.get(field).cloned().unwrap();
        assert_eq!(value("structure.home", "swarm"), json!(["kryptonite.REAL"]));
        assert_eq!(value("rcs_settings.S", "active_rcs_sensors"), json!([]));
    }

    fn nlclient_after_the_sensor(name: &str, others: &str) -> (PathBuf, PathBuf, String) {
        let dir = dir(name);
        let path = dir.join("sensor.json");
        let settings = dir.join("settings");
        let identity = load_identity(&path, "S").unwrap();
        let bucket = format!("kryptonite.{}", identity.id);
        std::fs::write(&settings, format!("<settings>\n  <s key=\"a\" value=\"1\"/>\n  <s key=\"is_onboarded_to_rcs\" value=\"1\"/>\n  \
            <s key=\"associated_rcs_sensors\" value=\"{bucket},{others}\"/>\n  <s key=\"{bucket}.seqno\" value=\"7\"/>\n</settings>\n")).unwrap();
        prepare_settings(&path, &settings, "S", None).unwrap();
        std::fs::write(dir.join(format!("rcs.{bucket}.recovery")), b"history").unwrap();
        let real = if others.is_empty() { String::new() } else { r#",{"ID":"kryptonite.REAL"}"#.to_string() };
        std::fs::write(dir.join("rcsmanager.json"), format!(
            r#"{{"active_sensors":"{bucket}","selected_sensor_type":"SINGLE_KRYPTONITE","associated_sensor_info":[{{"ID":"{bucket}"}}{real}]}}"#)).unwrap();
        std::fs::write(dir.join("predictionmanager.json"), format!(
            r#"{{"RemoteModels":{{"{bucket}":{{}},"kryptonite.REAL":{{}}}}}}"#)).unwrap();
        (path, settings, bucket)
    }

    #[test]
    fn unticked_nlclient_forgets_the_sensor_and_its_menu() {
        let (path, settings, bucket) = nlclient_after_the_sensor("forget", "");
        let dir = path.parent().unwrap().to_path_buf();
        forget_settings(&path, &settings).unwrap();
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "<settings>\n  <s key=\"a\" value=\"1\"/>\n</settings>\n");
        assert!(!dir.join(format!("rcs.{bucket}.recovery")).exists());
        assert!(!dir.join("rcsmanager.json").exists());
        let prediction = std::fs::read_to_string(dir.join("predictionmanager.json")).unwrap();
        assert!(!prediction.contains(&bucket) && prediction.contains("kryptonite.REAL"));
        assert!(read_identity(&path).is_some(), "its sequence numbers are never used twice");
    }

    #[test]
    fn forgetting_the_sensor_leaves_a_paired_one() {
        let (path, settings, bucket) = nlclient_after_the_sensor("forget-real", "kryptonite.REAL,");
        forget_settings(&path, &settings).unwrap();
        let b = std::fs::read(&settings).unwrap();
        assert_eq!(nlsettings::value(&b, "associated_rcs_sensors").unwrap(), b"kryptonite.REAL,");
        assert_eq!(nlsettings::value(&b, "is_onboarded_to_rcs").unwrap(), b"1");
        let manager = read_json(&path.parent().unwrap().join("rcsmanager.json")).unwrap();
        assert_eq!(manager["active_sensors"], "");
        assert_eq!(manager["selected_sensor_type"], "BACKPLATE");
        assert_eq!(manager["associated_sensor_info"], json!([{"ID": "kryptonite.REAL"}]));
        assert!(nlsettings::value(&b, &format!("{bucket}.key")).is_none());
    }

    #[test]
    fn forgetting_leaves_alone_a_unit_that_never_had_the_sensor() {
        let dir = dir("forget-never");
        let settings = dir.join("settings");
        let config = "<settings>\n  <s key=\"is_onboarded_to_rcs\" value=\"1\"/>\n</settings>\n";
        std::fs::write(&settings, config).unwrap();
        forget_settings(&dir.join("sensor.json"), &settings).unwrap();
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), config);
        load_identity(&dir.join("sensor.json"), "S").unwrap();
        forget_settings(&dir.join("sensor.json"), &settings).unwrap();
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), config, "a real sensor's onboarding stays once the sensor is gone");
    }
}
