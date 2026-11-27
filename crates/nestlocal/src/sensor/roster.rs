//! The paired Nest Temperature Sensors (KR1), kept in a file of nestlocal's own:
//! nlclient drops a sensor its service does not list, and its key with it, and
//! only Nest's service can give the key again.
//!
//!   {"sensors": [{"id": "18B430...", "key": "<16 bytes, base64 or hex>",
//!                 "room": "Master Bedroom", "label": "", "enabled": true}]}
//!
//! Before nlclient starts, the sensors its settings hold are added to the file,
//! which never loses one. While nestlocal runs, the file's sensors are the ones
//! nlclient is told of, and it asks for a new one's key as it does of Nest's
//! service when it hears the sensor. A sensor with "enabled": false is taken
//! from nlclient and stays in the file. The file is private, and no key is
//! logged or published.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{base64, listed, structure_key, DEFAULT_WHERE, MOST_SENSORS};
use crate::fields;
use crate::logf;
use crate::nlsettings;
use crate::store::Store;

/// How often the file is looked at, and how often what nlclient is told is
/// made sure of.
const POLL: Duration = Duration::from_secs(2);
const SETTLE: u32 = 30;
const WHERE: &str = "00000000-0000-0000-0000-0001000000";
/// nlclient's rooms: the end of their ids, as its name layer has them.
const ROOMS: [(u8, &str); 26] = [
    (0x00, "Entryway"),
    (0x01, "Basement"),
    (0x02, "Hallway"),
    (0x03, "Den"),
    (0x04, "Attic"),
    (0x05, "Master Bedroom"),
    (0x06, "Downstairs"),
    (0x07, "Garage"),
    (0x08, "Kids Room"),
    (0x09, "Bathroom"),
    (0x0a, "Kitchen"),
    (0x0b, "Family Room"),
    (0x0c, "Living Room"),
    (0x0d, "Bedroom"),
    (0x0e, "Office"),
    (0x0f, "Upstairs"),
    (0x10, "Dining Room"),
    (0x11, "Backyard"),
    (0x12, "Driveway"),
    (0x13, "Front Yard"),
    (0x14, "Outside"),
    (0x15, "Guest House"),
    (0x16, "Shed"),
    (0x17, "Deck"),
    (0x18, "Patio"),
    (0x1a, "Guest Room"),
];

fn yes() -> bool {
    true
}

/// A sensor as the file has it.
#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    room: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    label: String,
    #[serde(default = "yes")]
    enabled: bool,
    /// Whatever else someone noted there.
    #[serde(flatten)]
    other: Map<String, Value>,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    sensors: Vec<Entry>,
    #[serde(flatten)]
    other: Map<String, Value>,
}

/// A sensor nlclient is to have.
#[derive(Clone, PartialEq)]
pub struct Paired {
    /// As nlclient names its bucket: kryptonite.<id>.
    bucket: String,
    key: [u8; 16],
    /// None is the thermostat's own room.
    where_id: Option<String>,
    label: String,
}

/// A room's id, from its name, its id, or the id Nest's newer service has.
fn where_id(room: &str) -> Option<String> {
    let room = room.trim();
    if let Some((last, _)) = ROOMS.iter().find(|(_, name)| name.eq_ignore_ascii_case(room)) {
        return Some(format!("{WHERE}{last:02x}"));
    }
    if let Some(n) = room.strip_prefix("ANNOTATION_").and_then(|n| u64::from_str_radix(n, 16).ok()) {
        return u8::try_from(n).ok().map(|n| format!("{WHERE}{n:02x}"));
    }
    let uuid = room.len() == 36
        && room.bytes().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        });
    uuid.then(|| room.to_ascii_lowercase())
}

/// A room as the file names it.
fn room_name(where_id_of: &str) -> String {
    let id = where_id(where_id_of).unwrap_or_default();
    let last = id.strip_prefix(WHERE).and_then(|last| u8::from_str_radix(last, 16).ok());
    match ROOMS.iter().find(|(room, _)| Some(*room) == last) {
        Some((_, name)) => (*name).to_string(),
        None => id,
    }
}

fn parse_key(text: &str) -> Option<[u8; 16]> {
    let text = text.trim();
    super::parse_key(text).or_else(|| crate::cloud::base64_decode(text)?.try_into().ok())
}

fn parse_id(id: &str) -> Option<String> {
    let id = id.trim();
    let id = id.strip_prefix("kryptonite.").unwrap_or(id).to_ascii_uppercase();
    (id.len() == 16 && id.starts_with("18B430") && id.bytes().all(|c| c.is_ascii_hexdigit())).then_some(id)
}

/// nlclient's settings take a value as it stands.
fn plain(label: &str) -> bool {
    label.len() <= 32 && label.chars().all(|c| !c.is_control() && !matches!(c, '"' | '&' | '<' | '>'))
}

/// The entries nlclient is to have, `most` of them, and what is wrong with others.
fn check(entries: &[Entry], most: usize) -> (Vec<Paired>, Vec<String>) {
    let (mut good, mut wrong): (Vec<Paired>, Vec<String>) = Default::default();
    for (i, entry) in entries.iter().enumerate() {
        let name = || format!("sensor {} ({})", i + 1, parse_id(&entry.id).as_deref().unwrap_or("no id"));
        let Some(id) = parse_id(&entry.id) else {
            wrong.push(format!("{}: its id is not 16 hexadecimal digits from 18B430", name()));
            continue;
        };
        let bucket = format!("kryptonite.{id}");
        let Some(key) = parse_key(&entry.key) else {
            wrong.push(format!("{}: its key is not 16 bytes, base64 or hexadecimal", name()));
            continue;
        };
        if !plain(&entry.label) {
            wrong.push(format!("{}: its label is too long or has characters nlclient cannot take", name()));
            continue;
        }
        let room = where_id(&entry.room);
        if room.is_none() && !entry.room.trim().is_empty() {
            wrong.push(format!("{}: no room {:?}", name(), entry.room));
            continue;
        }
        if !entry.enabled {
            continue;
        }
        if good.iter().any(|other| other.bucket == bucket) {
            wrong.push(format!("{}: it is there twice", name()));
        } else if good.len() == most {
            wrong.push(format!("{}: nlclient has room for {most}", name()));
        } else {
            good.push(Paired { bucket, key, where_id: room, label: entry.label.trim().to_string() });
        }
    }
    (good, wrong)
}

/// The file, or none.
fn read(path: &Path) -> io::Result<Option<File>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
            // Where it is, and not what is there.
            io::Error::other(format!("not as expected at line {}, column {}", e.line(), e.column()))
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn write(path: &Path, file: &File) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(file).map_err(io::Error::other)?;
    bytes.push(b'\n');
    crate::durable::replace(path, &bytes, 0o600)
}

/// The sensors in nlclient's settings, but for the stand-in.
fn in_settings(settings: &[u8], stand_in: Option<&str>) -> Vec<Entry> {
    let value = |name: String| {
        nlsettings::value(settings, &name).map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default()
    };
    let ids: Vec<&str> = nlsettings::keys(settings)
        .filter_map(|name| name.strip_prefix("kryptonite.")?.strip_suffix(".key"))
        .filter(|id| stand_in != Some(id))
        .collect();
    ids.into_iter()
        .filter(|id| parse_id(id).is_some() && parse_key(&value(format!("kryptonite.{id}.key"))).is_some())
        .map(|id| Entry {
            id: id.to_string(),
            key: value(format!("kryptonite.{id}.key")),
            room: room_name(&value(format!("kryptonite.{id}.whereid"))),
            label: value(format!("kryptonite.{id}.desc")),
            enabled: true,
            other: Map::new(),
        })
        .collect()
}

/// The stand-in always has its place: nlclient holds six sensors at most, so a sixth
/// paired one is set aside, gone from nlclient's settings. Its key stays in the file, for
/// when there is room. One whose key is not in the file is not set aside.
pub fn make_room(path: &Path, settings: &Path, stand_in: Option<&str>) -> io::Result<()> {
    let original = std::fs::read(settings)?;
    let held = in_settings(&original, stand_in);
    let Some(aside) = held.get(MOST_SENSORS - 1..) else { return Ok(()) };
    let kept = read(path).ok().flatten().unwrap_or_default();
    let aside: Vec<&Entry> = aside
        .iter()
        .filter(|entry| {
            kept.sensors.iter().any(|known| parse_id(&known.id) == parse_id(&entry.id) && known.key == entry.key)
        })
        .collect();
    if aside.is_empty() {
        return Ok(());
    }
    let mut b = original.clone();
    for entry in &aside {
        for name in ["key", "whereid", "desc", "seqno"] {
            b = nlsettings::remove(&b, &format!("kryptonite.{}.{name}", entry.id));
        }
    }
    let associated =
        String::from_utf8_lossy(nlsettings::value(&b, "associated_rcs_sensors").unwrap_or_default()).into_owned();
    let list: String = associated
        .split(',')
        .filter(|bucket| {
            !bucket.is_empty()
                && !aside.iter().any(|entry| bucket.strip_prefix("kryptonite.") == Some(entry.id.as_str()))
        })
        .map(|bucket| format!("{bucket},"))
        .collect();
    if list != associated {
        b = nlsettings::set(&b, "associated_rcs_sensors", &list)?;
    }
    nlsettings::write(settings, &b)?;
    let ids: Vec<&str> = aside.iter().map(|entry| entry.id.as_str()).collect();
    logf!(
        "sensors: nlclient has room for {MOST_SENSORS} with the external temperature; {} set aside, its key stays in {}",
        ids.join(", "),
        path.display()
    );
    Ok(())
}

/// Adds the sensors nlclient's settings hold to the file. One already there
/// stays as the file has it. A file that cannot be read stays too, and they go
/// beside it.
fn capture(path: &Path, settings: &[u8], stand_in: Option<&str>) -> io::Result<usize> {
    let found = in_settings(settings, stand_in);
    if found.is_empty() {
        return Ok(0);
    }
    let (path, mut file) = match read(path) {
        Ok(file) => (path.to_path_buf(), file.unwrap_or_default()),
        Err(e) => {
            let mut beside = path.as_os_str().to_owned();
            beside.push(".saved");
            let beside = PathBuf::from(beside);
            logf!("sensors: {}: {e}; keeping the paired ones in {}", path.display(), beside.display());
            let file = read(&beside).ok().flatten().unwrap_or_default();
            (beside, file)
        }
    };
    let before = file.sensors.len();
    for entry in found {
        if !file.sensors.iter().any(|known| parse_id(&known.id) == parse_id(&entry.id)) {
            file.sensors.push(entry);
        }
    }
    let added = file.sensors.len() - before;
    if added > 0 {
        write(&path, &file)?;
    }
    Ok(added)
}

/// Before nlclient starts: what its settings hold of paired sensors is kept, and
/// what the file has is put in them. `most` is the sensors nlclient has room for
/// beside the stand-in, or none while it cannot hear them.
pub fn prepare(path: &Path, settings: &Path, stand_in: Option<&str>, most: Option<usize>) -> io::Result<()> {
    let original = std::fs::read(settings)?;
    let added = capture(path, &original, stand_in)?;
    if added > 0 {
        logf!("sensors: kept {added} paired with nlclient, in {}", path.display());
    }
    let Ok(Some(file)) = read(path) else { return Ok(()) };
    let Some(most) = most else {
        if let Some(note) = unheard(path, &file) {
            logf!("{note}");
        }
        return Ok(());
    };
    let (paired, _) = check(&file.sensors, most);
    let mut b = original.clone();
    for sensor in &paired {
        let put = |b: &[u8], name: &str, value: &str| nlsettings::set(b, &format!("{}.{name}", sensor.bucket), value);
        b = put(&b, "key", &base64(&sensor.key))?;
        // A room is the thermostat's until the bucket says it.
        if let Some(room) = &sensor.where_id {
            b = put(&b, "whereid", room)?;
        }
        b = put(&b, "desc", &sensor.label)?;
    }
    // One switched off is gone from them, or nlclient brings it back.
    for entry in &file.sensors {
        let Some(id) = parse_id(&entry.id) else { continue };
        let bucket = format!("kryptonite.{id}");
        if paired.iter().any(|sensor| sensor.bucket == bucket) || parse_key(&entry.key).is_none() {
            continue;
        }
        for name in ["key", "whereid", "desc", "seqno"] {
            b = nlsettings::remove(&b, &format!("{bucket}.{name}"));
        }
    }
    let associated = nlsettings::value(&b, "associated_rcs_sensors").unwrap_or_default();
    let associated = String::from_utf8_lossy(associated).into_owned();
    let mut list: Vec<&str> = associated
        .split(',')
        .filter(|bucket| {
            let known =
                file.sensors.iter().any(|entry| parse_id(&entry.id).as_deref() == bucket.strip_prefix("kryptonite."));
            !bucket.is_empty() && (!known || paired.iter().any(|sensor| sensor.bucket == *bucket))
        })
        .collect();
    for sensor in &paired {
        if !list.contains(&sensor.bucket.as_str()) {
            list.push(&sensor.bucket);
        }
    }
    let list: String = list.iter().map(|bucket| format!("{bucket},")).collect();
    if list != associated {
        b = nlsettings::set(&b, "associated_rcs_sensors", &list)?;
    }
    if b != original {
        nlsettings::write(settings, &b)?;
    }
    Ok(())
}

/// What nlclient is told: the structure's swarm, each sensor's bucket, and the
/// sensors it may regulate from. `known` are all the file's sensors, switched on
/// or not: any other is not nestlocal's to take away.
fn tell(store: &Store, serial: &str, paired: &[Paired], known: &[String]) -> bool {
    let Some(structure) = structure_key(store, serial) else { return false };
    let wanted = |list: &[Value]| {
        let mut out: Vec<Value> = list
            .iter()
            .filter(|v| {
                let bucket = v.as_str().unwrap_or_default();
                !known.iter().any(|k| k == bucket) || paired.iter().any(|sensor| sensor.bucket == bucket)
            })
            .cloned()
            .collect();
        for sensor in paired {
            if !out.iter().any(|v| v == sensor.bucket.as_str()) {
                out.push(sensor.bucket.as_str().into());
            }
        }
        out
    };
    let mut changed = false;
    let own_room = super::where_id(store, serial, None).unwrap_or_else(|| DEFAULT_WHERE.into());
    for sensor in paired {
        let id = sensor.bucket.strip_prefix("kryptonite.").unwrap_or_default();
        changed |= store
            .merge(
                &sensor.bucket,
                &fields! {
                    "model" => "KR1",
                    "serial_number" => id,
                    "where_id" => sensor.where_id.as_ref().unwrap_or(&own_room).clone(),
                    "description" => sensor.label.clone(),
                    "structure_id" => structure.split_once('.').map_or("", |(_, id)| id),
                    "report_period_in_seconds" => 60,
                },
                0,
                0,
                false,
            )
            .1;
    }
    let swarm = listed(store.get(&structure).as_ref().and_then(|o| o.value.get("swarm")));
    let want = wanted(&swarm);
    if want != swarm {
        changed |= store.merge(&structure, &fields! {"swarm" => want}, 0, 0, false).1;
    }
    // Until nlclient has sent its own, the choice it made and its schedule are there only.
    let key = format!("rcs_settings.{serial}");
    if let Some(settings) = store.get(&key) {
        let associated = listed(settings.value.get("associated_rcs_sensors"));
        let active = listed(settings.value.get("active_rcs_sensors"));
        let want = wanted(&associated);
        let still: Vec<Value> = active.iter().filter(|v| want.contains(v)).cloned().collect();
        if want != associated || (still != active && active.iter().any(|v| v != "")) {
            changed |= store
                .merge(&key, &fields! {"associated_rcs_sensors" => want, "active_rcs_sensors" => still}, 0, 0, false)
                .1;
        }
    }
    changed
}

/// The file's sensors, as nestlocal runs.
pub struct Roster {
    path: PathBuf,
    /// As `prepare`.
    most: Option<usize>,
    keys: Mutex<BTreeMap<String, [u8; 16]>>,
}

impl Roster {
    pub fn new(path: &Path, most: Option<usize>) -> Arc<Roster> {
        Arc::new(Roster { path: path.to_path_buf(), most, keys: Mutex::default() })
    }

    /// The key of a sensor nlclient is to have, as Nest's service answers it:
    /// in base64.
    pub fn key(&self, id: &str) -> Option<String> {
        self.keys.lock().unwrap().get(&parse_id(id)?).map(|key| base64(key))
    }

    /// Takes the file as it is now and tells nlclient. A file that cannot be
    /// read, or is not there, changes nothing.
    pub fn apply(&self, store: &Store, serial: &str, quiet: bool) {
        let file = match read(&self.path) {
            Ok(Some(file)) => file,
            Ok(None) => return,
            Err(e) => {
                if !quiet {
                    logf!("sensors: {}: {e}; as they were", self.path.display());
                }
                return;
            }
        };
        let (paired, wrong) = check(&file.sensors, self.most.unwrap_or(0));
        // Where nlclient hears none, that there is room for none is no news.
        if !quiet && self.most.is_some() {
            for problem in wrong {
                logf!("sensors: {problem}");
            }
        }
        *self.keys.lock().unwrap() = paired
            .iter()
            .map(|sensor| (sensor.bucket.trim_start_matches("kryptonite.").to_string(), sensor.key))
            .collect();
        let known: Vec<String> =
            file.sensors.iter().filter_map(|entry| Some(format!("kryptonite.{}", parse_id(&entry.id)?))).collect();
        if tell(store, serial, &paired, &known) {
            logf!("sensors: nlclient is to have {} of the {} in {}", paired.len(), known.len(), self.path.display());
        }
    }

    /// Applies the file now, whenever it changes, and once in a while.
    pub fn watch(self: &Arc<Self>, store: &Arc<Store>, serial: &str) {
        let changed = |path: &Path| -> Option<(SystemTime, u64)> {
            let m = std::fs::metadata(path).ok()?;
            Some((m.modified().ok()?, m.len()))
        };
        // Before it is read: what changes meanwhile is read again.
        let mut seen = changed(&self.path);
        self.apply(store, serial, false);
        let (roster, store, serial) = (self.clone(), Arc::downgrade(store), serial.to_string());
        std::thread::spawn(move || {
            let mut tick = 0;
            loop {
                std::thread::sleep(POLL);
                let Some(store) = store.upgrade() else { return };
                tick += 1;
                let now = changed(&roster.path);
                if now != seen {
                    seen = now;
                    roster.apply(&store, &serial, false);
                } else if tick % SETTLE == 0 {
                    roster.apply(&store, &serial, true);
                }
            }
        });
    }
}

/// Said at start where nlclient is to hear none: the file has sensors, but the external
/// temperature or Matter has the Bluetooth chip, so nobody would know why they are silent.
fn unheard(path: &Path, file: &File) -> Option<String> {
    let (paired, _) = check(&file.sensors, MOST_SENSORS);
    (!paired.is_empty()).then(|| {
        format!(
            "sensors: {} in {} are not heard while the external temperature or Matter has the Bluetooth chip; their keys stay in the file",
            paired.len(),
            path.display()
        )
    })
}

/// How many paired sensors nlclient has room for, or none where it hears none:
/// the stand-in takes a place, and its Bluetooth too.
pub fn room(stand_in: bool, matter: bool) -> Option<usize> {
    if matter || stand_in {
        return None;
    }
    Some(MOST_SENSORS - usize::from(stand_in))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::temp_store;
    use serde_json::json;

    const A: &str = "18B43000AAAA1111";
    const B: &str = "18B43000BBBB2222";
    const OURS: &str = "18B43000CCCC3333";

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nestlocal-roster-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn key(n: u8) -> String {
        base64(&[n; 16])
    }

    fn stock(sensors: &[(&str, u8, &str)]) -> String {
        let mut s = String::from("<settings>\n  <s key=\"a\" value=\"1\"/>\n");
        for (id, n, room) in sensors {
            for (name, value) in [("key", key(*n).as_str()), ("whereid", *room), ("desc", ""), ("seqno", "500")] {
                s += &format!("  <s key=\"kryptonite.{id}.{name}\" value=\"{value}\"/>\n");
            }
        }
        let list: String = sensors.iter().map(|(id, ..)| format!("kryptonite.{id},")).collect();
        s + &format!("  <s key=\"associated_rcs_sensors\" value=\"{list}\"/>\n</settings>\n")
    }

    fn file(path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    fn home() -> Arc<Store> {
        let store = temp_store();
        crate::structure::ensure_structure(&store, "S");
        store.merge("device.S", &fields! {"where_id" => "00000000-0000-0000-0000-000100000000"}, 0, 0, true);
        store
    }

    fn told(store: &Store) -> (Value, Value, Value) {
        let structure = store.get(&structure_key(store, "S").unwrap()).unwrap().value;
        let rcs = store.get("rcs_settings.S").map(|o| o.value).unwrap_or_default();
        let of = |f: &crate::json::Fields, k: &str| f.get(k).cloned().unwrap_or(Value::Null);
        (of(&structure, "swarm"), of(&rcs, "associated_rcs_sensors"), of(&rcs, "active_rcs_sensors"))
    }

    #[test]
    fn rooms_by_name_id_and_the_newer_services_id() {
        assert_eq!(where_id("master bedroom").unwrap(), "00000000-0000-0000-0000-000100000005");
        assert_eq!(where_id("ANNOTATION_0000000000000008").unwrap(), "00000000-0000-0000-0000-000100000008");
        assert_eq!(where_id("ANNOTATION_000000000000001a").unwrap(), "00000000-0000-0000-0000-00010000001a");
        assert_eq!(where_id("00000000-0000-0000-0000-00010000000C").unwrap(), DEFAULT_WHERE);
        for bad in ["", "Ballroom", "ANNOTATION_", "ANNOTATION_100", "00000000-0000-0000-0000-00010000000"] {
            assert_eq!(where_id(bad), None, "{bad}");
        }
        for (last, name) in ROOMS {
            assert_eq!(room_name(&format!("{WHERE}{last:02x}")), name);
            assert_eq!(where_id(name).unwrap(), format!("{WHERE}{last:02x}"));
        }
    }

    #[test]
    fn a_stock_units_sensors_are_kept_before_nlclient_starts() {
        let dir = dir("stock");
        let (path, settings) = (dir.join("sensors.json"), dir.join("settings"));
        let before = stock(&[(A, 1, "ANNOTATION_0000000000000005"), (B, 2, "ANNOTATION_0000000000000008")]);
        std::fs::write(&settings, &before).unwrap();
        prepare(&path, &settings, None, Some(6)).unwrap();
        assert_eq!(
            file(&path),
            json!({"sensors": [
                {"id": A, "key": key(1), "room": "Master Bedroom", "enabled": true},
                {"id": B, "key": key(2), "room": "Kids Room", "enabled": true},
            ]})
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // nlclient's own stay, with the rooms by the ids nestlocal's service has.
        let after = std::fs::read_to_string(&settings).unwrap();
        assert_eq!(
            after,
            before
                .replace("ANNOTATION_0000000000000005", "00000000-0000-0000-0000-000100000005")
                .replace("ANNOTATION_0000000000000008", "00000000-0000-0000-0000-000100000008")
        );
        prepare(&path, &settings, None, Some(6)).unwrap();
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), after, "nothing more to do");
    }

    #[test]
    fn what_nlclient_dropped_is_put_back() {
        let dir = dir("back");
        let (path, settings) = (dir.join("sensors.json"), dir.join("settings"));
        std::fs::write(&settings, stock(&[(A, 1, "ANNOTATION_0000000000000005")])).unwrap();
        prepare(&path, &settings, None, Some(6)).unwrap();
        std::fs::write(&settings, "<settings>\n</settings>\n").unwrap();
        prepare(&path, &settings, None, Some(6)).unwrap();
        let b = std::fs::read(&settings).unwrap();
        assert_eq!(nlsettings::value(&b, &format!("kryptonite.{A}.key")).unwrap(), key(1).as_bytes());
        assert_eq!(nlsettings::value(&b, "associated_rcs_sensors").unwrap(), format!("kryptonite.{A},").as_bytes());
        assert!(nlsettings::value(&b, &format!("kryptonite.{A}.seqno")).is_none(), "nlclient's own to start");
    }

    #[test]
    fn sensors_nobody_hears_are_said_at_start_and_their_keys_stay() {
        let dir = dir("unheard");
        let (path, settings) = (dir.join("sensors.json"), dir.join("settings"));
        std::fs::write(&settings, stock(&[(A, 1, ""), (B, 2, "")])).unwrap();
        // Matter, or the stand-in, has the chip: nlclient is to hear none.
        prepare(&path, &settings, None, room(false, true)).unwrap();
        let file = read(&path).unwrap().unwrap();
        let note = unheard(&path, &file).expect("two sensors are in the file");
        assert!(note.contains("2 in") && note.contains("Bluetooth chip"), "{note}");
        assert!(!note.contains(&key(1)), "no key in the log");
        assert_eq!(file.sensors.len(), 2, "the keys are kept");
        // One switched off is not one that is silent.
        let mut file = file;
        file.sensors[1].enabled = false;
        assert!(unheard(&path, &file).unwrap().contains("1 in"));
        file.sensors[0].enabled = false;
        assert!(unheard(&path, &file).is_none(), "nothing to say with none to hear");
    }

    #[test]
    fn the_stand_in_is_not_a_paired_sensor() {
        let dir = dir("ours");
        let (path, settings) = (dir.join("sensors.json"), dir.join("settings"));
        std::fs::write(&settings, stock(&[(OURS, 9, "room"), (A, 1, "")])).unwrap();
        prepare(&path, &settings, Some(OURS), Some(5)).unwrap();
        assert_eq!(file(&path)["sensors"], json!([{"id": A, "key": key(1), "enabled": true}]));
        let b = std::fs::read(&settings).unwrap();
        assert_eq!(
            nlsettings::value(&b, "associated_rcs_sensors").unwrap(),
            format!("kryptonite.{OURS},kryptonite.{A},").as_bytes(),
            "and stays where it is"
        );
    }

    #[test]
    fn one_switched_off_leaves_nlclient_and_not_the_file() {
        let dir = dir("off");
        let (path, settings) = (dir.join("sensors.json"), dir.join("settings"));
        std::fs::write(&settings, stock(&[(A, 1, ""), (B, 2, "")])).unwrap();
        std::fs::write(
            &path,
            json!({"note": "mine", "sensors": [{"id": A, "key": key(1), "enabled": false, "bought": 2019}]})
                .to_string(),
        )
        .unwrap();
        prepare(&path, &settings, None, Some(6)).unwrap();
        let b = std::fs::read(&settings).unwrap();
        assert!(!String::from_utf8_lossy(&b).contains(A));
        assert!(nlsettings::value(&b, &format!("kryptonite.{B}.key")).is_some());
        let file = file(&path);
        assert_eq!(file["sensors"][0], json!({"id": A, "key": key(1), "enabled": false, "bought": 2019}));
        assert_eq!((&file["note"], &file["sensors"][1]["id"]), (&json!("mine"), &json!(B)));
    }

    #[test]
    fn a_file_that_cannot_be_read_is_left_and_nothing_is_lost() {
        let dir = dir("bad");
        let (path, settings) = (dir.join("sensors.json"), dir.join("settings"));
        std::fs::write(&settings, stock(&[(A, 1, "")])).unwrap();
        std::fs::write(&path, "{\"sensors\": [").unwrap();
        prepare(&path, &settings, None, Some(6)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"sensors\": [");
        assert_eq!(file(&dir.join("sensors.json.saved"))["sensors"][0]["id"], A);
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), stock(&[(A, 1, "")]));
        // Nor does it take away what nlclient was told.
        let store = home();
        let roster = Roster::new(&path, Some(6));
        store.merge(
            &structure_key(&store, "S").unwrap(),
            &fields! {"swarm" => vec![format!("kryptonite.{A}")]},
            0,
            0,
            false,
        );
        roster.apply(&store, "S", false);
        assert_eq!(told(&store).0, json!([format!("kryptonite.{A}")]));
    }

    #[test]
    fn what_is_wrong_is_said_without_the_key() {
        let entry = |id: &str, key: &str, room: &str, label: &str| Entry {
            id: id.into(),
            key: key.into(),
            room: room.into(),
            label: label.into(),
            enabled: true,
            other: Map::new(),
        };
        let hex = "00112233445566778899aabbccddeeff";
        let (good, wrong) = check(
            &[
                entry(A, hex, "Den", "Desk"),
                entry(&format!("kryptonite.{}", B.to_lowercase()), &key(2), "", ""),
                entry(A, hex, "", ""),
                entry("18B43", hex, "", ""),
                entry("28B43000AAAA1111", hex, "", ""),
                entry("18B43000DDDD4444", "c2VjcmV0c2VjcmV0", "", ""),
                entry("18B43000DDDD4444", hex, "Ballroom", ""),
                entry("18B43000DDDD4444", hex, "", "a \"b\""),
                entry("18B43000DDDD4444", hex, "", &"x".repeat(33)),
            ],
            6,
        );
        assert_eq!(
            good.iter().map(|s| s.bucket.as_str()).collect::<Vec<_>>(),
            [format!("kryptonite.{A}"), format!("kryptonite.{B}")]
        );
        assert_eq!((good[0].label.as_str(), good[0].key[1]), ("Desk", 0x11));
        assert_eq!(wrong.len(), 7, "{wrong:?}");
        assert!(wrong.iter().all(|w| !w.contains(hex) && !w.contains("c2Vj")), "{wrong:?}");
        let many: Vec<Entry> = (0..8).map(|i| entry(&format!("18B43000DDDD444{i}"), hex, "", "")).collect();
        let (good, wrong) = check(&many, 5);
        assert_eq!((good.len(), wrong.len()), (5, 3), "never more than nlclient has room for");
    }

    #[test]
    fn nlclient_is_told_and_gets_the_key_it_asks_for() {
        let dir = dir("tell");
        let path = dir.join("sensors.json");
        let store = home();
        store.merge(
            "rcs_settings.S",
            &fields! {"active_rcs_sensors" => vec![format!("kryptonite.{OURS}")],
            "associated_rcs_sensors" => vec![format!("kryptonite.{OURS}")], "rcs_control_setting" => "OVERRIDE"},
            0,
            0,
            true,
        );
        store.merge(
            &structure_key(&store, "S").unwrap(),
            &fields! {"swarm" => vec![format!("kryptonite.{OURS}")]},
            0,
            0,
            false,
        );
        let roster = Roster::new(&path, Some(5));
        roster.apply(&store, "S", false);
        assert_eq!(told(&store).0, json!([format!("kryptonite.{OURS}")]), "no file: nothing to say");
        let put = |enabled: bool| {
            let sensors = json!([{"id": A, "key": key(1), "room": "Kids Room", "label": "Desk"},
                                 {"id": B, "key": key(2), "enabled": enabled}]);
            std::fs::write(&path, json!({ "sensors": sensors }).to_string()).unwrap();
            roster.apply(&store, "S", false);
        };
        put(true);
        let all = json!([format!("kryptonite.{OURS}"), format!("kryptonite.{A}"), format!("kryptonite.{B}")]);
        assert_eq!(told(&store), (all.clone(), all, json!([format!("kryptonite.{OURS}")])));
        let bucket = store.get(&format!("kryptonite.{A}")).unwrap().value;
        assert_eq!(bucket["where_id"], "00000000-0000-0000-0000-000100000008");
        assert_eq!(
            (&bucket["description"], &bucket["model"], &bucket["serial_number"]),
            (&json!("Desk"), &json!("KR1"), &json!(A))
        );
        assert_eq!(
            store.get(&format!("kryptonite.{B}")).unwrap().value["where_id"],
            "00000000-0000-0000-0000-000100000000"
        );
        assert_eq!(roster.key(A).unwrap(), key(1));
        assert_eq!(roster.key(&format!("kryptonite.{B}")).unwrap(), key(2));
        assert_eq!(roster.key(OURS), None);
        // Chosen, and then switched off.
        store.merge("rcs_settings.S", &fields! {"active_rcs_sensors" => vec![format!("kryptonite.{B}")]}, 0, 0, true);
        put(false);
        let two = json!([format!("kryptonite.{OURS}"), format!("kryptonite.{A}")]);
        assert_eq!(told(&store), (two.clone(), two, json!([])));
        assert_eq!(roster.key(B), None, "its key is not given out either");
        put(true);
        assert_eq!(roster.key(B).unwrap(), key(2), "and back");
        // The same again is no news to nlclient.
        let revision = |key: &str| store.get(key).unwrap().revision;
        let before = (revision("rcs_settings.S"), revision(&structure_key(&store, "S").unwrap()));
        put(true);
        assert_eq!((revision("rcs_settings.S"), revision(&structure_key(&store, "S").unwrap())), before);
    }

    #[test]
    fn what_nlclient_has_not_sent_yet_is_not_made_up() {
        let dir = dir("fresh");
        let path = dir.join("sensors.json");
        std::fs::write(&path, json!({"sensors": [{"id": A, "key": key(1)}]}).to_string()).unwrap();
        let store = home();
        Roster::new(&path, Some(6)).apply(&store, "S", false);
        assert_eq!(told(&store).0, json!([format!("kryptonite.{A}")]));
        assert!(store.get("rcs_settings.S").is_none(), "its choice and schedule stay its own");
    }

    #[test]
    fn where_nlclient_hears_none_they_wait_in_the_file() {
        assert_eq!(room(false, false), Some(6));
        assert_eq!((room(true, false), room(false, true), room(true, true)), (None, None, None));
        let dir = dir("deaf");
        let (path, settings) = (dir.join("sensors.json"), dir.join("settings"));
        std::fs::write(&settings, stock(&[(A, 1, "")])).unwrap();
        prepare(&path, &settings, None, None).unwrap();
        assert_eq!(file(&path)["sensors"][0]["id"], A, "kept");
        let store = home();
        store.merge(
            &structure_key(&store, "S").unwrap(),
            &fields! {"swarm" => vec![format!("kryptonite.{A}")]},
            0,
            0,
            false,
        );
        let roster = Roster::new(&path, None);
        roster.apply(&store, "S", false);
        assert_eq!((told(&store).0, roster.key(A)), (json!([]), None));
    }

    #[test]
    fn a_changed_file_is_taken_as_it_changes() {
        let dir = dir("watch");
        let path = dir.join("sensors.json");
        let store = home();
        let roster = Roster::new(&path, Some(6));
        roster.watch(&store, "S");
        std::fs::write(&path, json!({"sensors": [{"id": A, "key": key(1)}]}).to_string()).unwrap();
        let end = std::time::Instant::now() + POLL * 4;
        while roster.key(A).is_none() {
            assert!(std::time::Instant::now() < end, "not taken");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// What nlclient holds after the prepare steps main.rs runs with -external-temperature:
    /// the paired sensors first, then the stand-in.
    fn boot(dir: &Path) -> (io::Result<()>, usize) {
        let (sensors, settings, identity) = (dir.join("sensors.json"), dir.join("settings"), dir.join("stand-in.json"));
        let stand_in = crate::sensor::stand_in_id(&identity);
        let first = prepare(&sensors, &settings, stand_in.as_deref(), room(true, false))
            .and_then(|()| make_room(&sensors, &settings, stand_in.as_deref()));
        let second = first.and_then(|()| crate::sensor::prepare_settings(&identity, &settings, "S", None));
        let b = std::fs::read(&settings).unwrap();
        let held = crate::nlsettings::keys(&b).filter(|k| k.starts_with("kryptonite.") && k.ends_with(".key")).count();
        (second, held)
    }

    #[test]
    fn a_sensor_whose_key_is_not_in_the_file_is_not_set_aside() {
        let ids: Vec<String> = (1..=6).map(|i| format!("18B43000AAAA000{i}")).collect();
        let dir = dir("unkept");
        let (path, settings) = (dir.join("sensors.json"), dir.join("settings"));
        let sensors: Vec<(&str, u8, &str)> =
            ids.iter().enumerate().map(|(i, id)| (id.as_str(), i as u8 + 1, "")).collect();
        let before = stock(&sensors);
        std::fs::write(&settings, &before).unwrap();
        // A file nlclient's keys cannot be added to: they wait beside it, and nothing leaves nlclient.
        std::fs::write(&path, "{").unwrap();
        make_room(&path, &settings, None).unwrap();
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), before);
    }

    #[test]
    fn six_paired_sensors_and_the_stand_in_never_pass_six() {
        let ids: Vec<String> = (1..=6).map(|i| format!("18B43000AAAA000{i}")).collect();
        for paired in [5, 6] {
            let dir = dir(&format!("six-{paired}"));
            let sensors: Vec<(&str, u8, &str)> =
                ids.iter().take(paired).enumerate().map(|(i, id)| (id.as_str(), i as u8 + 1, "room")).collect();
            std::fs::write(dir.join("settings"), stock(&sensors)).unwrap();
            for start in 1..=3 {
                let (result, held) = boot(&dir);
                // The stand-in always has its place: the sixth is set aside, and its key stays in the file.
                assert_eq!(held, 6, "{paired} paired, start {start}");
                assert!(result.is_ok(), "{paired} paired, start {start}: {result:?}");
            }
            let b = std::fs::read(dir.join("settings")).unwrap();
            let has = |id: &str| nlsettings::value(&b, &format!("kryptonite.{id}.key")).is_some();
            assert!(!has(&ids[5]), "{paired} paired: the sixth is not in nlclient's settings");
            assert!(ids.iter().take(5).all(|id| has(id)), "{paired} paired: the others stay");
            let json = file(&dir.join("sensors.json"));
            let in_file = json["sensors"].as_array().unwrap().iter().any(|e| e["id"] == ids[5] && e["key"] == key(6));
            assert_eq!(in_file, paired == 6, "{paired} paired: the sixth's key is in the file");
            let list = String::from_utf8_lossy(nlsettings::value(&b, "associated_rcs_sensors").unwrap()).into_owned();
            assert!(!list.contains(ids[5].as_str()), "{paired} paired: nlclient is not told of it: {list}");
        }
    }
}
