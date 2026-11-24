//! Every bucket, persisted to one JSON file, with wakeups for whoever waits
//! on a change (held subscribes, the MQTT bridge).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Notify};

use crate::json::{canonical, Fields};
use crate::logf;

/// One Nest bucket: "device.<serial>", "shared.<serial>", "structure.<id>",
/// ... Revision and timestamp (ms) follow the cloud's rules: the larger
/// timestamp wins, zero means "no data". The file keeps this field order.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Object {
    #[serde(rename = "object_key", default)]
    pub key: String,
    #[serde(rename = "object_revision", default)]
    pub revision: i64,
    #[serde(rename = "object_timestamp", default)]
    pub timestamp: i64,
    #[serde(default, deserialize_with = "deserialize_fields")]
    pub value: Fields,
    /// Local Matter schedule handles and inactive weeks. Cloud responses only carry value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_schedules: Option<crate::schedule::Catalog>,
    /// Learning restored after suspension, protected until the device reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_learning: Option<RestoredLearning>,
}

/// Until `until` (Unix seconds), a device report of another learning value is
/// a late echo of the suspension's and is corrected; after it, the unit's own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RestoredLearning {
    pub value: serde_json::Value,
    pub until: i64,
}

fn deserialize_fields<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Fields, D::Error> {
    Ok(match canonical(serde_json::Value::deserialize(d)?) {
        serde_json::Value::Object(m) => m,
        _ => Fields::new(),
    })
}

type Watcher = Arc<dyn Fn(&Object) + Send + Sync>;
type DeviceWatcher = Arc<dyn Fn(&Fields, &Object) + Send + Sync>;

#[derive(Default)]
struct Inner {
    objects: BTreeMap<String, Object>,
    subs: BTreeMap<u64, mpsc::Sender<Vec<Object>>>, // held subscribes: server-side changes
    next_sub: u64,
    watch: Vec<Watcher>,              // every change, device or server side
    device_watch: Vec<DeviceWatcher>, // the device's own changes, with the value before
    catalog_writes: u64,              // the file's writes made within an update
}

pub struct Store {
    path: PathBuf,
    inner: Mutex<Inner>,
    /// Held to write the file, with the count of the writes made within an
    /// update that it holds. Taken after `inner`, or without it: the
    /// periodic save writes with `inner` free, so nothing waits on the NAND
    /// to read a bucket.
    file: Mutex<u64>,
    dirty: Notify,
}

/// A held subscribe's place among the store's listeners; dropping it
/// leaves.
pub struct Subscription {
    store: Arc<Store>,
    id: u64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.store.inner.lock().unwrap().subs.remove(&self.id);
    }
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Store {
    /// The store at path, with the buckets the file there holds.
    pub fn new(path: impl Into<PathBuf>) -> Arc<Store> {
        let path = path.into();
        let mut inner = Inner::default();
        if let Ok(bytes) = std::fs::read(&path) {
            match serde_json::from_slice::<Vec<Object>>(&bytes) {
                Ok(objects) => inner.objects = objects.into_iter().map(|o| (o.key.clone(), o)).collect(),
                Err(e) => logf!("store: {}: {e} (starting empty)", path.display()),
            }
            logf!("store: loaded {} buckets from {}", inner.objects.len(), path.display());
        }
        Arc::new(Store { path, inner: Mutex::new(inner), file: Mutex::new(0), dirty: Notify::new() })
    }

    pub fn get(&self, key: &str) -> Option<Object> {
        self.inner.lock().unwrap().objects.get(key).cloned()
    }

    pub fn all(&self) -> Vec<Object> {
        self.inner.lock().unwrap().objects.values().cloned().collect()
    }

    /// Applies fields to a bucket. A change bumps the revision (or sets rev,
    /// when it is ahead) and the timestamp (ts, or now when 0); an identical
    /// merge changes nothing, so the device is not sent back what it already
    /// has.
    /// from_device marks changes the device made itself: they are not pushed
    /// back to it.
    pub fn merge(&self, key: &str, fields: &Fields, rev: i64, ts: i64, from_device: bool) -> (Object, bool) {
        let mut learning_corrected = false;
        let result = self
            .update(key, rev, ts, from_device, |objects, o| {
                for (k, v) in fields {
                    o.value.insert(k.clone(), canonical(v.clone()));
                }
                if fields.contains_key("learning_mode")
                    && (!from_device || o.value.get("learning_mode") == o.restored_learning.as_ref().map(|r| &r.value))
                {
                    o.restored_learning = None;
                }
                if from_device {
                    if fields.contains_key("days") {
                        if let Some(name) = crate::schedule_control::mode(&o.value) {
                            if let Some(control) = o.local_schedules.as_mut().and_then(|c| c.control.get_mut(name)) {
                                if control.saved.is_none()
                                    && control.restored.as_ref().and_then(|b| b.get("days")) == o.value.get("days")
                                {
                                    control.restored = None;
                                }
                            }
                        }
                    }
                    if let Some(serial) = key.strip_prefix("device.") {
                        let suspended = objects
                            .get(&format!("schedule.{serial}"))
                            .and_then(|o| o.local_schedules.as_ref())
                            .is_some_and(|c| c.control.values().any(|s| s.saved.is_some()));
                        if suspended {
                            learning_corrected = o.value.get("learning_mode") != Some(&false.into());
                            o.value.insert("learning_mode".into(), false.into());
                        }
                    }
                }
                Ok(())
            })
            .unwrap_or_else(|e| {
                logf!("store: rejected {key}: {e}");
                (self.get(key).unwrap_or_else(|| Object { key: key.into(), ..Default::default() }), false)
            });
        if learning_corrected {
            self.resend(key);
        }
        result
    }

    /// Edits and checks a single bucket under the same lock used for device updates.
    pub fn update(
        &self,
        key: &str,
        rev: i64,
        ts: i64,
        from_device: bool,
        edit: impl FnOnce(&BTreeMap<String, Object>, &mut Object) -> Result<(), String>,
    ) -> Result<(Object, bool), String> {
        let (object, subs, watch, devices, before) = {
            let mut inner = self.inner.lock().unwrap();
            let old = inner.objects.get(key);
            let before = if from_device { old.map(|old| old.value.clone()).unwrap_or_default() } else { Fields::new() };
            let mut o = old.cloned().unwrap_or_else(|| Object { key: key.into(), ..Default::default() });
            edit(&inner.objects, &mut o)?;
            if !from_device && old.is_some_and(|old| old.value.get("days") != o.value.get("days")) {
                if let Some(name) = crate::schedule_control::mode(&o.value) {
                    if let Some(control) = o.local_schedules.as_mut().and_then(|c| c.control.get_mut(name)) {
                        if control.saved.is_none() {
                            control.restored = None;
                        }
                    }
                }
            }
            let mut corrected = from_device && old.is_some_and(|old| crate::schedule_control::reconcile(old, &mut o));
            if from_device {
                if let Some(learning) = &o.restored_learning {
                    if learning.until <= chrono::Utc::now().timestamp() {
                        o.restored_learning = None;
                    } else if o.value.get("learning_mode") != Some(&learning.value) {
                        o.value.insert("learning_mode".into(), learning.value.clone());
                        corrected = true;
                    }
                }
            }
            let changed = corrected || old.is_none_or(|old| old.value != o.value);
            if let (Some(previous), Some(weeks)) = (old, &mut o.local_schedules) {
                if previous.value.get("schedule_mode") != o.value.get("schedule_mode") {
                    if let Some(week) = previous
                        .value
                        .get("schedule_mode")
                        .and_then(|m| m.as_str())
                        .and_then(|m| weeks.active.get(m))
                        .and_then(|h| weeks.named.get_mut(h))
                    {
                        week.bucket = previous
                            .local_schedules
                            .as_ref()
                            .and_then(|c| crate::schedule_control::mode(&previous.value).and_then(|m| c.control.get(m)))
                            .and_then(|c| c.saved.clone())
                            .unwrap_or_else(|| previous.value.clone());
                    }
                }
            }
            let catalog = old.and_then(|o| o.local_schedules.as_ref()) != o.local_schedules.as_ref()
                || old.and_then(|o| o.restored_learning.as_ref()) != o.restored_learning.as_ref();
            if !changed && !catalog {
                return Ok((o, false));
            }
            if changed {
                o.revision = if rev > o.revision { rev } else { o.revision + 1 };
                o.timestamp = if corrected {
                    now_ms().max(ts.saturating_add(1))
                } else if ts > 0 {
                    ts
                } else {
                    now_ms()
                };
            }
            if catalog {
                // The active handle and the days it selects must survive a restart together.
                let snapshot: Vec<_> =
                    inner.objects.values().filter(|other| other.key != key).chain(std::iter::once(&o)).collect();
                let mut file = self.file.lock().unwrap();
                self.write(&snapshot).map_err(|e| e.to_string())?;
                *file += 1;
                inner.catalog_writes = *file;
            }
            inner.objects.insert(key.to_owned(), o.clone());
            let subs =
                if changed && (!from_device || corrected) { inner.subs.values().cloned().collect() } else { vec![] };
            let devices = if changed && from_device { inner.device_watch.clone() } else { vec![] };
            (o, subs, inner.watch.clone(), devices, before)
        };
        self.dirty.notify_one();
        for sub in subs {
            let _ = sub.try_send(vec![object.clone()]);
        }
        for w in watch {
            w(&object);
        }
        for w in devices {
            w(&before, &object);
        }
        Ok((object, true))
    }

    /// Commits related buckets and local schedule state together before notifying the unit.
    pub fn transaction(
        &self,
        edit: impl FnOnce(&mut BTreeMap<String, Object>) -> Result<(), String>,
    ) -> Result<(), String> {
        let (changed, subs, watch) = {
            let mut inner = self.inner.lock().unwrap();
            let mut objects = inner.objects.clone();
            edit(&mut objects)?;
            let mut changed = Vec::new();
            let mut local = false;
            for (key, o) in &mut objects {
                let old = inner.objects.get(key);
                let value_changed = old.is_none_or(|old| old.value != o.value);
                let local_changed = old.and_then(|old| old.local_schedules.as_ref()) != o.local_schedules.as_ref()
                    || old.and_then(|old| old.restored_learning.as_ref()) != o.restored_learning.as_ref();
                if value_changed || local_changed {
                    if value_changed {
                        o.revision += 1;
                        o.timestamp = now_ms();
                    }
                    local |= local_changed;
                    changed.push((o.clone(), value_changed));
                }
            }
            if changed.is_empty() {
                return Ok(());
            }
            // Local state exists nowhere else: it is on the NAND before anyone
            // sees it. Buckets alone the device resends, and the batch saves.
            if local {
                let mut file = self.file.lock().unwrap();
                self.write(&objects.values().collect::<Vec<_>>()).map_err(|e| e.to_string())?;
                *file += 1;
                inner.catalog_writes = *file;
            }
            inner.objects = objects;
            (changed, inner.subs.values().cloned().collect::<Vec<_>>(), inner.watch.clone())
        };
        self.dirty.notify_one();
        let native: Vec<_> = changed.iter().filter(|(_, value)| *value).map(|(o, _)| o.clone()).collect();
        for sub in subs {
            if !native.is_empty() {
                let _ = sub.try_send(native.clone());
            }
        }
        for (o, _) in changed {
            for w in &watch {
                w(&o);
            }
        }
        Ok(())
    }

    /// Sends the bucket `key` to the held subscribes again, unchanged but for
    /// a new revision: nlclient commits it, which is when it takes a display
    /// wake request (settings_ui::request_wake). False when there is none.
    pub fn resend(&self, key: &str) -> bool {
        let (object, subs) = {
            let mut inner = self.inner.lock().unwrap();
            let Some(o) = inner.objects.get_mut(key) else { return false };
            o.revision += 1;
            o.timestamp = now_ms();
            let object = o.clone();
            (object, inner.subs.values().cloned().collect::<Vec<_>>())
        };
        self.dirty.notify_one();
        for sub in subs {
            let _ = sub.try_send(vec![object.clone()]);
        }
        true
    }

    /// Server-side changes, for a held subscribe, until the subscription is
    /// dropped.
    pub fn subscribe(self: &Arc<Self>) -> (mpsc::Receiver<Vec<Object>>, Subscription) {
        let (tx, rx) = mpsc::channel(16);
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_sub;
        inner.next_sub += 1;
        inner.subs.insert(id, tx);
        (rx, Subscription { store: self.clone(), id })
    }

    pub fn watch(&self, f: impl Fn(&Object) + Send + Sync + 'static) {
        self.inner.lock().unwrap().watch.push(Arc::new(f));
    }

    /// Calls `f` with the value before and the bucket after each change the device made itself.
    pub fn watch_device(&self, f: impl Fn(&Fields, &Object) + Send + Sync + 'static) {
        self.inner.lock().unwrap().device_watch.push(Arc::new(f));
    }

    /// Writes the file at most every 5 s: NAND wears, and the device
    /// resends anything lost on its next subscribe anyway.
    pub async fn save_changes(self: Arc<Self>) {
        loop {
            self.dirty.notified().await;
            tokio::time::sleep(Duration::from_secs(5)).await;
            let store = self.clone();
            let saved = tokio::task::spawn_blocking(move || store.save()).await;
            if let Ok(Err(e)) = saved {
                logf!("store: {e}");
            }
        }
    }

    /// Writes the buckets as they are, read with the store free for others
    /// while the file is written. An update that wrote the file since they
    /// were read holds newer ones: they are read again, never written over.
    fn save(&self) -> std::io::Result<()> {
        loop {
            let (objects, catalog_writes) = {
                let inner = self.inner.lock().unwrap();
                (inner.objects.values().cloned().collect::<Vec<_>>(), inner.catalog_writes)
            };
            let file = self.file.lock().unwrap();
            if *file == catalog_writes {
                return self.write(&objects);
            }
        }
    }

    fn write(&self, objects: &[impl Serialize]) -> std::io::Result<()> {
        let mut bytes = Vec::new();
        let mut ser =
            serde_json::Serializer::with_formatter(&mut bytes, serde_json::ser::PrettyFormatter::with_indent(b" "));
        objects.serialize(&mut ser).map_err(std::io::Error::other)?;
        crate::durable::replace(&self.path, &bytes, 0o600)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::fields;
    use serde_json::{json, Value};

    pub fn temp_store() -> Arc<Store> {
        let dir = std::env::temp_dir().join(format!("nestlocal-test-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        Store::new(dir.join(format!("s{}.json", rand_suffix())))
    }

    fn rand_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn merge_bumps_only_on_change() {
        let s = temp_store();
        let (o, changed) = s.merge("device.S", &fields! {"t" => 20.0}, 0, 0, true);
        assert!(changed && o.revision == 1 && o.timestamp > 0);
        // 20 and 20.0 are the same value: nothing changes.
        let (o, changed) = s.merge("device.S", &fields! {"t" => 20}, 0, 0, true);
        assert!(!changed && o.revision == 1);
        let (o, changed) = s.merge("device.S", &fields! {"t" => 21}, 7, 1234, true);
        assert!(changed && o.revision == 7 && o.timestamp == 1234 && o.value["t"] == json!(21));
    }

    #[test]
    fn only_server_side_changes_wake_subscribes() {
        let s = temp_store();
        let (mut rx, _sub) = s.subscribe();
        s.merge("device.S", &fields! {"a" => 1}, 0, 0, true);
        assert!(rx.try_recv().is_err());
        s.merge("device.S", &fields! {"a" => 2}, 0, 0, false);
        assert_eq!(rx.try_recv().unwrap()[0].value["a"], json!(2));
    }

    #[test]
    fn a_bucket_resent_reaches_a_held_subscribe_unchanged_with_a_new_revision() {
        let s = temp_store();
        let (mut rx, _sub) = s.subscribe();
        let (before, _) = s.merge("device.S", &fields! {"a" => 1}, 7, 0, true);
        assert!(rx.try_recv().is_err());
        assert!(s.resend("device.S"));
        let resent = rx.try_recv().unwrap().remove(0);
        assert_eq!((resent.value, resent.revision), (before.value, before.revision + 1));
        assert!(!s.resend("device.missing"), "nothing to send");
    }

    #[test]
    fn file_round_trip() {
        let s = temp_store();
        s.merge("device.S", &fields! {"t" => 19.5, "on" => true}, 3, 99, true);
        s.write(&s.all()).unwrap();
        let text = std::fs::read_to_string(&s.path).unwrap();
        assert!(text.starts_with("[\n {\n  \"object_key\": \"device.S\",\n  \"object_revision\": 3,"), "{text}");
        let again = Store::new(s.path.clone());
        let o = again.get("device.S").unwrap();
        assert_eq!((o.revision, o.timestamp, o.value["t"].clone()), (3, 99, json!(19.5)));
        assert!(!PathBuf::from(format!("{}.partial", s.path.display())).exists(), "the temporary file is left behind");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&s.path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn a_save_leaves_the_buckets_free_and_keeps_what_an_update_wrote() {
        let s = temp_store();
        s.merge("schedule.S", &fields! {"days" => json!({})}, 0, 0, true);
        let file = s.file.lock().unwrap();
        let saving = std::thread::spawn({
            let s = s.clone();
            move || s.save()
        });
        std::thread::sleep(Duration::from_millis(50));
        // The save waits to write with the buckets free.
        assert!(s.get("schedule.S").is_some());
        // An update that writes the file itself, after the save read the buckets.
        let updating = std::thread::spawn({
            let s = s.clone();
            move || {
                s.update("schedule.S", 0, 0, false, |_, o| {
                    o.local_schedules = Some(crate::schedule::Catalog {
                        active: [("HEAT".to_owned(), "s1".to_owned())].into(),
                        ..Default::default()
                    });
                    Ok(())
                })
            }
        });
        std::thread::sleep(Duration::from_millis(50));
        drop(file);
        saving.join().unwrap().unwrap();
        updating.join().unwrap().unwrap();
        let again = Store::new(s.path.clone());
        assert_eq!(again.get("schedule.S").unwrap().local_schedules, s.get("schedule.S").unwrap().local_schedules);
        assert!(again.get("schedule.S").unwrap().local_schedules.is_some());
    }

    #[test]
    fn a_file_that_does_not_parse_starts_empty() {
        let path = temp_store().path.clone();
        for (text, want) in [
            ("not json", None),
            (r#"{"object_key":"device.S"}"#, None), // not a list
            (r#"[{"object_key":"device.S","object_revision":2,"object_timestamp":5,"value":{"t":20}}"#, None), // cut short
            // Missing fields default; a value that is no object reads as empty.
            (r#"[{"object_key":"device.S","value":[1]},{"object_key":"shared.S"}]"#, Some((0, 0, 0))),
            (
                r#"[{"object_key":"device.S","object_revision":2,"object_timestamp":5,"value":{"t":20.0,"a":{"b":1.0}}}]"#,
                Some((2, 5, 2)),
            ),
        ] {
            std::fs::write(&path, text).unwrap();
            let s = Store::new(path.clone());
            let got = s.get("device.S").map(|o| (o.revision, o.timestamp, o.value.len()));
            assert_eq!(got, want, "{text}");
            if want.is_none() {
                assert!(s.all().is_empty(), "{text}");
                assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "the file was changed by reading it");
            }
        }
        // Numbers are in canonical form once loaded: 20.0 merges over as 20.
        let s = Store::new(path);
        assert!(!s.merge("device.S", &fields! {"t" => 20}, 0, 0, true).1);
        assert_eq!(s.get("device.S").unwrap().value["a"].to_string(), r#"{"b":1}"#);
    }

    #[test]
    fn merge_keeps_fields_it_is_not_given() {
        let s = temp_store();
        s.merge("device.S", &fields! {"a" => 1, "b" => 2}, 0, 0, true);
        let (o, changed) = s.merge("device.S", &fields! {"b" => 3, "c" => Value::Null}, 0, 0, true);
        assert!(changed && o.revision == 2);
        assert_eq!(Value::Object(o.value), json!({"a": 1, "b": 3, "c": null}));
        // An empty merge creates a bucket, but changes an existing one not at all.
        let (o, changed) = s.merge("shared.S", &Fields::new(), 0, 0, true);
        assert!(changed && o.revision == 1 && o.value.is_empty());
        assert!(!s.merge("shared.S", &Fields::new(), 0, 0, false).1);
    }

    #[test]
    fn watchers_see_every_change_and_dropped_subscriptions_leave() {
        let s = temp_store();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        s.watch(move |o| record.lock().unwrap().push((o.key.clone(), o.revision)));
        let (_rx, sub) = s.subscribe();
        let (mut rx2, _sub2) = s.subscribe();
        s.merge("device.S", &fields! {"a" => 1}, 0, 0, true);
        s.merge("device.S", &fields! {"a" => 1}, 0, 0, false); // no change: nobody hears
        s.merge("shared.S", &fields! {"b" => 1}, 0, 0, false);
        assert_eq!(*seen.lock().unwrap(), vec![("device.S".to_string(), 1), ("shared.S".to_string(), 1)]);
        assert_eq!(rx2.try_recv().unwrap()[0].key, "shared.S");
        assert!(rx2.try_recv().is_err());
        assert_eq!(s.inner.lock().unwrap().subs.len(), 2);
        drop(sub);
        assert_eq!(s.inner.lock().unwrap().subs.len(), 1);
    }
}
