//! Every bucket, persisted to one JSON file, with wakeups for whoever waits
//! on a change (held subscribes, the MQTT bridge).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
}

fn deserialize_fields<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Fields, D::Error> {
    Ok(match canonical(serde_json::Value::deserialize(d)?) {
        serde_json::Value::Object(m) => m,
        _ => Fields::new(),
    })
}

type Watcher = Arc<dyn Fn(&Object) + Send + Sync>;

#[derive(Default)]
struct Inner {
    objects: BTreeMap<String, Object>,
    subs: BTreeMap<u64, mpsc::Sender<Vec<Object>>>, // held subscribes: server-side changes
    next_sub: u64,
    watch: Vec<Watcher>, // every change, device or server side
    device_changed: BTreeMap<String, Instant>, // when the device last changed each bucket
}

pub struct Store {
    path: PathBuf,
    inner: Mutex<Inner>,
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
        Arc::new(Store { path, inner: Mutex::new(inner), dirty: Notify::new() })
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
        let (object, subs, watch, changed) = {
            let mut inner = self.inner.lock().unwrap();
            let existed = inner.objects.contains_key(key);
            let o = inner.objects.entry(key.to_string())
                .or_insert_with(|| Object { key: key.to_string(), ..Default::default() });
            let mut changed = !existed;
            for (k, v) in fields {
                let v = canonical(v.clone());
                if o.value.get(k) != Some(&v) {
                    o.value.insert(k.clone(), v);
                    changed = true;
                }
            }
            if changed {
                o.revision = if rev > o.revision { rev } else { o.revision + 1 };
                o.timestamp = if ts > 0 { ts } else { now_ms() };
            }
            let object = o.clone();
            if changed && from_device {
                inner.device_changed.insert(key.to_string(), Instant::now());
            }
            let subs: Vec<_> = if changed && !from_device { inner.subs.values().cloned().collect() } else { vec![] };
            (object, subs, inner.watch.clone(), changed)
        };
        if changed {
            self.dirty.notify_one();
            for sub in subs {
                let _ = sub.try_send(vec![object.clone()]);
            }
            for w in watch {
                w(&object);
            }
        }
        (object, changed)
    }

    /// When the device last changed key's bucket, since this process started.
    pub fn device_changed_at(&self, key: &str) -> Option<Instant> {
        self.inner.lock().unwrap().device_changed.get(key).copied()
    }

    /// Sends key's bucket to the held subscribes again, its value as it
    /// stands, at a newer revision and timestamp: nlclient takes it as it
    /// takes a change of Home Assistant's, and commits it. Watchers hear
    /// nothing, as no value changed, and the file is not rewritten for it.
    /// The commit also runs nlclient's downlink_activity, which takes any
    /// pending display.request. Being newer, the resend can undo a change
    /// nlclient made to the bucket and has not uploaded yet (in
    /// device.<serial>: Eco, the fan timer, a hot water boost): Resender
    /// paces it. Whether there is such a bucket.
    pub fn resend(&self, key: &str) -> bool {
        let (object, subs) = {
            let mut inner = self.inner.lock().unwrap();
            let Some(o) = inner.objects.get_mut(key) else {
                return false;
            };
            o.revision += 1;
            o.timestamp = now_ms().max(o.timestamp + 1);
            let object = o.clone();
            (object, inner.subs.values().cloned().collect::<Vec<_>>())
        };
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

    /// Writes the file at most every 5 s: NAND wears, and the device
    /// resends anything lost on its next subscribe anyway.
    pub async fn save_changes(self: Arc<Self>) {
        loop {
            self.dirty.notified().await;
            tokio::time::sleep(Duration::from_secs(5)).await;
            let objects = self.all();
            let store = self.clone();
            let saved = tokio::task::spawn_blocking(move || store.write(&objects)).await;
            if let Ok(Err(e)) = saved {
                logf!("store: {e}");
            }
        }
    }

    fn write(&self, objects: &[Object]) -> std::io::Result<()> {
        let mut bytes = Vec::new();
        let mut ser = serde_json::Serializer::with_formatter(&mut bytes, serde_json::ser::PrettyFormatter::with_indent(b" "));
        objects.serialize(&mut ser).map_err(std::io::Error::other)?;
        let tmp = PathBuf::from(format!("{}.tmp", self.path.display()));
        write_private(&tmp, &bytes)?;
        std::fs::rename(&tmp, &self.path)
    }
}

/// How often Resender sends a bucket again at most.
pub const RESEND_EVERY: Duration = Duration::from_secs(60);
/// How long after the device changed the bucket Resender waits.
pub const DEVICE_QUIET: Duration = Duration::from_secs(10);

/// What Resender::resend did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resent {
    Sent,
    /// Sent within RESEND_EVERY: not again.
    Recently,
    /// The device changed the bucket within DEVICE_QUIET: ask again later.
    DeviceBusy,
    /// There is no such bucket.
    Missing,
}

/// One bucket sent to nlclient again (Store::resend), at most once every
/// RESEND_EVERY, and not within DEVICE_QUIET of the device changing it:
/// a resend can undo a change of nlclient's not yet uploaded.
pub struct Resender {
    store: Arc<Store>,
    key: String,
    last: Mutex<Option<Instant>>,
}

impl Resender {
    pub fn new(store: Arc<Store>, key: String) -> Resender {
        Resender { store, key, last: Mutex::new(None) }
    }

    pub fn resend(&self, now: Instant) -> Resent {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|at| now.saturating_duration_since(at) < RESEND_EVERY) {
            return Resent::Recently;
        }
        if self.store.device_changed_at(&self.key).is_some_and(|at| now.saturating_duration_since(at) < DEVICE_QUIET) {
            return Resent::DeviceBusy;
        }
        if !self.store.resend(&self.key) {
            return Resent::Missing;
        }
        *last = Some(now);
        Resent::Sent
    }
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)
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
        let (o, changed) = s.merge("device.S", &fields!{"t" => 20.0}, 0, 0, true);
        assert!(changed && o.revision == 1 && o.timestamp > 0);
        // 20 and 20.0 are the same value: nothing changes.
        let (o, changed) = s.merge("device.S", &fields!{"t" => 20}, 0, 0, true);
        assert!(!changed && o.revision == 1);
        let (o, changed) = s.merge("device.S", &fields!{"t" => 21}, 7, 1234, true);
        assert!(changed && o.revision == 7 && o.timestamp == 1234 && o.value["t"] == json!(21));
    }

    #[test]
    fn only_server_side_changes_wake_subscribes() {
        let s = temp_store();
        let (mut rx, _sub) = s.subscribe();
        s.merge("device.S", &fields!{"a" => 1}, 0, 0, true);
        assert!(rx.try_recv().is_err());
        s.merge("device.S", &fields!{"a" => 2}, 0, 0, false);
        assert_eq!(rx.try_recv().unwrap()[0].value["a"], json!(2));
    }

    #[test]
    fn file_round_trip() {
        let s = temp_store();
        s.merge("device.S", &fields!{"t" => 19.5, "on" => true}, 3, 99, true);
        s.write(&s.all()).unwrap();
        let text = std::fs::read_to_string(&s.path).unwrap();
        assert!(text.starts_with("[\n {\n  \"object_key\": \"device.S\",\n  \"object_revision\": 3,"), "{text}");
        let again = Store::new(s.path.clone());
        let o = again.get("device.S").unwrap();
        assert_eq!((o.revision, o.timestamp, o.value["t"].clone()), (3, 99, json!(19.5)));
        assert!(!PathBuf::from(format!("{}.tmp", s.path.display())).exists(), "the temporary file is left behind");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&s.path).unwrap().permissions().mode() & 0o777, 0o600);
        }
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
            (r#"[{"object_key":"device.S","object_revision":2,"object_timestamp":5,"value":{"t":20.0,"a":{"b":1.0}}}]"#,
             Some((2, 5, 2))),
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
        assert!(!s.merge("device.S", &fields!{"t" => 20}, 0, 0, true).1);
        assert_eq!(s.get("device.S").unwrap().value["a"].to_string(), r#"{"b":1}"#);
    }

    #[test]
    fn merge_keeps_fields_it_is_not_given() {
        let s = temp_store();
        s.merge("device.S", &fields!{"a" => 1, "b" => 2}, 0, 0, true);
        let (o, changed) = s.merge("device.S", &fields!{"b" => 3, "c" => Value::Null}, 0, 0, true);
        assert!(changed && o.revision == 2);
        assert_eq!(Value::Object(o.value), json!({"a": 1, "b": 3, "c": null}));
        // An empty merge creates a bucket, but changes an existing one not at all.
        let (o, changed) = s.merge("shared.S", &Fields::new(), 0, 0, true);
        assert!(changed && o.revision == 1 && o.value.is_empty());
        assert!(!s.merge("shared.S", &Fields::new(), 0, 0, false).1);
    }

    #[test]
    fn resend_reaches_held_subscribes_with_the_value_unchanged() {
        let s = temp_store();
        let seen = Arc::new(Mutex::new(0));
        let count = seen.clone();
        s.merge("device.S", &fields!{"t" => 20, "on" => true}, 4, 1000, true);
        s.watch(move |_| *count.lock().unwrap() += 1);
        let (mut rx, _sub) = s.subscribe();
        let marked = || futures_lite::future::block_on(futures_lite::future::poll_once(s.dirty.notified())).is_some();
        assert!(marked(), "the merge marks the file to be written");
        assert!(s.resend("device.S"));
        let sent = rx.try_recv().unwrap();
        assert_eq!(sent.len(), 1);
        let (o, held) = (&sent[0], s.get("device.S").unwrap());
        assert_eq!((o.key.as_str(), o.revision), ("device.S", 5));
        assert!(o.timestamp > 1000 && o.timestamp == held.timestamp && held.revision == 5);
        assert_eq!(Value::Object(o.value.clone()), json!({"t": 20, "on": true}));
        assert_eq!(held.value, o.value);
        assert_eq!(*seen.lock().unwrap(), 0, "no watcher hears of it");
        assert!(!marked(), "nor is the file rewritten for it");
        // A timestamp ahead of the clock still moves on.
        s.merge("device.S", &fields!{"t" => 21}, 0, i64::MAX / 2, true);
        assert!(s.resend("device.S"));
        assert_eq!(s.get("device.S").unwrap().timestamp, i64::MAX / 2 + 1);
        assert!(!s.resend("shared.S"), "no such bucket");
        assert!(s.get("shared.S").is_none());
    }

    #[test]
    fn a_resender_sends_once_a_minute_and_not_just_after_the_device_changed_the_bucket() {
        let s = temp_store();
        let (mut rx, _sub) = s.subscribe();
        let resender = Resender::new(s.clone(), "device.S".into());
        let start = Instant::now();
        assert_eq!(resender.resend(start), Resent::Missing);
        s.merge("device.S", &fields!{"t" => 20}, 0, 0, true);
        let changed = s.device_changed_at("device.S").unwrap();
        assert_eq!(resender.resend(changed + Duration::from_secs(9)), Resent::DeviceBusy);
        assert!(rx.try_recv().is_err());
        let now = changed + DEVICE_QUIET;
        assert_eq!(resender.resend(now), Resent::Sent);
        assert_eq!(rx.try_recv().unwrap()[0].revision, 2);
        assert_eq!(resender.resend(now + Duration::from_secs(1)), Resent::Recently);
        assert_eq!(resender.resend(now + Duration::from_secs(59)), Resent::Recently);
        assert!(rx.try_recv().is_err(), "two nudges within the minute, one resend");
        assert_eq!(resender.resend(now + RESEND_EVERY), Resent::Sent);
        // Changes from the server side, and the device's that change nothing, do not count.
        s.merge("device.S", &fields!{"t" => 22}, 0, 0, false);
        s.merge("device.S", &fields!{"t" => 22}, 0, 0, true);
        assert_eq!(s.device_changed_at("device.S"), Some(changed));
    }

    #[test]
    fn watchers_see_every_change_and_dropped_subscriptions_leave() {
        let s = temp_store();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        s.watch(move |o| record.lock().unwrap().push((o.key.clone(), o.revision)));
        let (_rx, sub) = s.subscribe();
        let (mut rx2, _sub2) = s.subscribe();
        s.merge("device.S", &fields!{"a" => 1}, 0, 0, true);
        s.merge("device.S", &fields!{"a" => 1}, 0, 0, false); // no change: nobody hears
        s.merge("shared.S", &fields!{"b" => 1}, 0, 0, false);
        assert_eq!(*seen.lock().unwrap(), vec![("device.S".to_string(), 1), ("shared.S".to_string(), 1)]);
        assert_eq!(rx2.try_recv().unwrap()[0].key, "shared.S");
        assert!(rx2.try_recv().is_err());
        assert_eq!(s.inner.lock().unwrap().subs.len(), 2);
        drop(sub);
        assert_eq!(s.inner.lock().unwrap().subs.len(), 1);
    }
}
