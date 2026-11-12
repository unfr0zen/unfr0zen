//! A temporary heating override, with a durable undo record before any change.
use crate::{
    bridge::Bridge,
    fields,
    json::{number, truthy, Fields},
    logf,
};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock, Weak};

pub const MINUTES: [u16; 5] = [0, 30, 60, 120, 180];
static BRIDGE: OnceLock<Weak<Bridge>> = OnceLock::new();

// Keep the unit awake: a userspace timer cannot wake its suspended CPU.
// shortcut: boosts consume extra power, replace with a verified RTC wake alarm before allowing sleep during a boost.
pub fn active() -> bool {
    BRIDGE
        .get()
        .and_then(Weak::upgrade)
        .is_some_and(|b| record(&b).is_some())
}
fn record(b: &Bridge) -> Option<Fields> {
    b.store
        .get(&format!("boost.{}", b.serial))?
        .value
        .get("undo")?
        .as_object()
        .cloned()
}
fn flush(b: &Bridge) -> Result<(), String> {
    b.store.flush().map_err(|e| format!("boost recovery state: {e}"))
}
fn merge(b: &Bridge, bucket: &str, fields: &Fields) {
    b.store.merge(&format!("{bucket}.{}", b.serial), fields, 0, 0, false);
}

pub fn duration(arg: &str) -> Result<u16, String> {
    let minutes = if arg.eq_ignore_ascii_case("off") {
        0
    } else {
        arg.parse::<u16>()
            .map_err(|_| "expected 0, 30, 60, 120 or 180 minutes")?
    };
    if !MINUTES.contains(&minutes) {
        return Err("expected 0, 30, 60, 120 or 180 minutes".into());
    }
    Ok(minutes)
}

/// Call under boost_lock; selecting a duration again restarts that duration without adding more degrees.
pub fn command(b: &Bridge, arg: &str) -> Result<(), String> {
    let minutes = duration(arg)?;
    if minutes == 0 {
        return cancel(b);
    }
    let buckets = b.buckets();
    let dev = buckets.get("device").ok_or("no device settings yet")?;
    let sh = buckets.get("shared").ok_or("no thermostat state yet")?;
    if !truthy(sh.get("can_heat")) {
        return Err("heating is unavailable".into());
    }
    if !matches!(
        sh.get("target_temperature_type").and_then(Value::as_str),
        Some("heat" | "off")
    ) {
        return Err("heating boost requires heat or off mode".into());
    }
    let now = chrono::Utc::now().timestamp();
    if now < 1_700_000_000 {
        return Err("set the thermostat clock before boosting".into());
    }
    if record(b).is_none() {
        let schedule = buckets.get("schedule").ok_or("no schedule received yet")?;
        let days = schedule
            .get("days")
            .filter(|v| v.is_object())
            .ok_or("invalid schedule")?;
        let current = number(sh.get("current_temperature"))
            .filter(|t| t.is_finite() && (-20.0..=50.0).contains(t))
            .ok_or("no valid room temperature")?;
        let previous = number(sh.get("target_temperature"))
            .filter(|t| t.is_finite() && (4.5..=32.0).contains(t))
            .ok_or("no valid target temperature")?;
        let target = ((current + 2.0).max(previous).clamp(9.0, 32.0) * 2.0).round() / 2.0;
        let mut saved_device = Fields::new();
        for key in ["learning_mode", "auto_away_enable", "preconditioning_enabled", "eco"] {
            saved_device.insert(
                key.into(),
                dev.get(key).ok_or_else(|| format!("missing {key}"))?.clone(),
            );
        }
        let applied_device = fields! {"learning_mode" => false, "auto_away_enable" => false,
        "preconditioning_enabled" => false,
        "eco" => json!({"mode":"schedule", "mode_update_timestamp":now, "touched_by":4})};
        let applied_shared = fields! {"target_temperature" => target, "target_temperature_type" => "heat"};
        let empty = json!({"0":{},"1":{},"2":{},"3":{},"4":{},"5":{},"6":{}});
        let undo = json!({
            "device": saved_device, "shared": {"target_temperature":previous,"target_temperature_type":sh["target_temperature_type"]},
            "schedule":{"days":days}, "applied_device":applied_device,
            "applied_shared":applied_shared, "applied_schedule":{"days":empty}
        });
        merge(b, "boost", &fields! {"undo" => undo});
        if let Err(error) = flush(b) {
            merge(b, "boost", &fields! {"undo" => Value::Null});
            return Err(error);
        }
        merge(b, "device", &applied_device);
        merge(b, "schedule", &fields! {"days" => empty});
        let mut shared = applied_shared;
        shared.insert("target_change_pending".into(), true.into());
        merge(b, "shared", &shared);
    }
    merge(
        b,
        "boost",
        &fields! {"ends" => now + i64::from(minutes) * 60, "boot_end" => embassy_time::Instant::now().as_secs() + u64::from(minutes) * 60},
    );
    merge(b, "device", &fields! {"heating_boost" => minutes});
    if let Err(error) = flush(b) {
        let _ = cancel(b);
        return Err(error);
    }
    Ok(())
}

/// Restore only fields still owned by the boost; preserve newer manual edits.
pub fn cancel(b: &Bridge) -> Result<(), String> {
    let Some(undo) = record(b) else { return Ok(()) };
    for bucket in ["device", "shared", "schedule"] {
        let saved = undo
            .get(bucket)
            .and_then(Value::as_object)
            .ok_or("invalid boost recovery record")?;
        let applied = undo
            .get(&format!("applied_{bucket}"))
            .and_then(Value::as_object)
            .ok_or("invalid boost recovery record")?;
        let current = b
            .store
            .get(&format!("{bucket}.{}", b.serial))
            .ok_or("missing boost recovery bucket")?;
        let mut restore = Fields::new();
        for (key, value) in saved {
            let owned = if key == "eco" {
                current.value.get(key).and_then(|v| v.get("mode")) == applied.get(key).and_then(|v| v.get("mode"))
            } else {
                current.value.get(key) == applied.get(key)
            };
            if owned {
                let mut value = value.clone();
                if key == "eco" {
                    if let Some(eco) = value.as_object_mut() {
                        eco.insert("mode_update_timestamp".into(), chrono::Utc::now().timestamp().into());
                        eco.insert("touched_by".into(), 4.into());
                    }
                }
                restore.insert(key.clone(), value);
            }
        }
        if bucket == "shared" && !restore.is_empty() {
            restore.insert("target_change_pending".into(), true.into());
        }
        merge(b, bucket, &restore);
    }
    merge(b, "device", &fields! {"heating_boost" => 0});
    flush(b)?;
    merge(b, "boost", &fields! {"undo" => Value::Null, "ends" => 0});
    flush(b)
}

fn tick(b: &Bridge, now: i64) -> Result<(), String> {
    let Some(undo) = record(b) else { return Ok(()) };
    let ends = b
        .store
        .get(&format!("boost.{}", b.serial))
        .and_then(|o| number(o.value.get("ends")))
        .unwrap_or(0.0);
    let sh = b
        .store
        .get(&format!("shared.{}", b.serial))
        .ok_or("missing shared state")?;
    let applied = undo["applied_shared"].as_object().ok_or("invalid boost state")?;
    let changed = applied
        .iter()
        .any(|(k, v)| sh.value.get(k) != Some(&crate::json::canonical(v.clone())));
    let dev = b
        .store
        .get(&format!("device.{}", b.serial))
        .ok_or("missing device state")?;
    let schedule_edited = b
        .store
        .get(&format!("schedule.{}", b.serial))
        .is_some_and(|o| o.value.get("days") != undo["applied_schedule"].get("days"));
    let flags_edited = ["learning_mode", "auto_away_enable", "preconditioning_enabled"]
        .iter()
        .any(|key| dev.value.get(*key) != Some(&Value::Bool(false)));
    let eco = dev.value.get("eco").and_then(|v| v.get("mode")).and_then(Value::as_str);
    let boot_end = b
        .store
        .get(&format!("boost.{}", b.serial))
        .and_then(|o| number(o.value.get("boot_end")))
        .unwrap_or(0.0);
    if now as f64 >= ends
        || embassy_time::Instant::now().as_secs() as f64 >= boot_end
        || changed
        || schedule_edited
        || flags_edited
        || eco != Some("schedule")
    {
        cancel(b)?;
    }
    Ok(())
}

pub fn start(b: Arc<Bridge>) {
    let _ = BRIDGE.set(Arc::downgrade(&b));
    tokio::spawn(async move {
        {
            let _guard = b.boost_lock.lock().unwrap();
            if let Err(e) = cancel(&b) {
                logf!("boost recovery: {e}");
            }
        }
        loop {
            {
                let _guard = b.boost_lock.lock().unwrap();
                if let Err(e) = tick(&b, chrono::Utc::now().timestamp()) {
                    logf!("boost: {e}");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn fixture() -> Arc<Bridge> {
        let b = crate::bridge::tests::test_bridge();
        merge(
            &b,
            "device",
            &fields! {"learning_mode"=>true,"auto_away_enable"=>true,"preconditioning_enabled"=>true,"eco"=>json!({"mode":"schedule"})},
        );
        merge(
            &b,
            "shared",
            &fields! {"can_heat"=>true,"current_temperature"=>19,"target_temperature"=>18,"target_temperature_type"=>"heat"},
        );
        merge(
            &b,
            "schedule",
            &fields! {"days"=>json!({"0":{"0":{"time":0,"temp":18,"type":"HEAT"}}})},
        );
        b
    }
    #[test]
    fn durations_expire_and_restore_without_stacking() {
        for minutes in &MINUTES[1..] {
            let b = fixture();
            let before = b.buckets();
            b.apply("mqtt", "heating_boost", &minutes.to_string()).unwrap();
            assert_eq!(number(b.buckets()["shared"].get("target_temperature")), Some(21.0));
            b.apply("matter", "heating_boost", &minutes.to_string()).unwrap();
            assert_eq!(number(b.buckets()["shared"].get("target_temperature")), Some(21.0));
            tick(&b, chrono::Utc::now().timestamp() + i64::from(*minutes) * 60 + 1).unwrap();
            assert!(record(&b).is_none());
            assert_eq!(b.buckets()["schedule"], before["schedule"]);
            assert_eq!(
                b.buckets()["shared"]["target_temperature"],
                before["shared"]["target_temperature"]
            );
        }
    }
    #[test]
    fn manual_change_wins_and_invalid_commands_leave_boost_alone() {
        let b = fixture();
        b.apply("zigbee", "heating_boost", "60").unwrap();
        assert!(b.apply("mqtt", "target", "999").is_err());
        assert!(record(&b).is_some());
        b.apply("matter", "target", "22").unwrap();
        assert!(record(&b).is_none());
        assert_eq!(number(b.buckets()["shared"].get("target_temperature")), Some(22.0));
        for arg in ["-1", "45", "NaN", "181"] {
            assert!(b.apply("mqtt", "heating_boost", arg).is_err());
        }
    }
    #[test]
    fn durable_journal_recovers_in_a_fresh_store() {
        let b = fixture();
        command(&b, "120").unwrap();
        let path = std::env::temp_dir().join(format!("boost-reopen-{}.json", std::process::id()));
        let disk = crate::store::Store::new(&path);
        for o in b.store.all() {
            disk.merge(&o.key, &o.value, o.revision, o.timestamp, true);
        }
        disk.flush().unwrap();
        drop(disk);
        let recovered = Bridge::new(
            "nestlocal",
            "homeassistant",
            "S",
            crate::store::Store::new(&path),
            Arc::new(crate::weather::Outdoor::default()),
            None,
            false,
            crate::naming::Naming::load("/nonexistent/nestlocal-ha.json", "S", None, None),
        );
        cancel(&recovered).unwrap();
        assert!(record(&recovered).is_none());
        assert_eq!(
            number(recovered.buckets()["shared"].get("target_temperature")),
            Some(18.0)
        );
        assert_eq!(recovered.buckets()["device"]["learning_mode"], json!(true));
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn off_is_restored_and_cooling_or_range_are_unchanged() {
        let b = fixture();
        merge(&b, "shared", &fields! {"target_temperature_type" => "off"});
        command(&b, "30").unwrap();
        assert_eq!(b.buckets()["shared"]["target_temperature_type"], json!("heat"));
        cancel(&b).unwrap();
        assert_eq!(b.buckets()["shared"]["target_temperature_type"], json!("off"));
        for mode in ["cool", "range"] {
            merge(&b, "shared", &fields! {"target_temperature_type" => mode});
            let before = b.buckets();
            assert!(command(&b, "30").is_err());
            assert_eq!(b.buckets(), before);
        }
    }

    #[test]
    fn boot_deadline_bounds_boost_when_wall_clock_moves_back() {
        let b = fixture();
        command(&b, "180").unwrap();
        merge(&b, "boost", &fields! {"boot_end" => 0});
        tick(&b, chrono::Utc::now().timestamp() - 3600).unwrap();
        assert!(record(&b).is_none());
    }

    #[test]
    fn storage_failure_cannot_start_heating_or_erase_schedule() {
        let original = fixture();
        let path = std::env::temp_dir()
            .join(format!("boost-missing-parent-{}", std::process::id()))
            .join("state.json");
        let store = crate::store::Store::new(path);
        for o in original.store.all() {
            store.merge(&o.key, &o.value, 0, 0, true);
        }
        let b = Bridge::new(
            "nestlocal",
            "homeassistant",
            "S",
            store,
            Arc::new(crate::weather::Outdoor::default()),
            None,
            false,
            crate::naming::Naming::load("/nonexistent/nestlocal-ha.json", "S", None, None),
        );
        let before = b.buckets();
        assert!(command(&b, "30").is_err());
        for bucket in ["shared", "device", "schedule"] {
            assert_eq!(b.buckets()[bucket], before[bucket]);
        }
    }

    #[test]
    fn heating_can_be_turned_off_even_when_restoration_cannot_be_saved() {
        let original = fixture();
        let dir = std::env::temp_dir().join(format!("boost-off-failure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let store = crate::store::Store::new(&path);
        for o in original.store.all() {
            store.merge(&o.key, &o.value, 0, 0, true);
        }
        let b = Bridge::new(
            "nestlocal",
            "homeassistant",
            "S",
            store,
            Arc::new(crate::weather::Outdoor::default()),
            None,
            false,
            crate::naming::Naming::load("/nonexistent/nestlocal-ha.json", "S", None, None),
        );
        command(&b, "60").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
        b.apply("matter", "mode", "off").unwrap();
        assert_eq!(b.buckets()["shared"]["target_temperature_type"], json!("off"));
    }

    #[test]
    fn native_change_and_recovery_preserve_newer_settings() {
        let b = fixture();
        command(&b, "30").unwrap();
        merge(&b, "shared", &fields! {"target_temperature"=>23});
        tick(&b, chrono::Utc::now().timestamp()).unwrap();
        assert_eq!(number(b.buckets()["shared"].get("target_temperature")), Some(23.0));
        command(&b, "60").unwrap();
        cancel(&b).unwrap();
        assert!(record(&b).is_none());
        assert_eq!(number(b.buckets()["shared"].get("target_temperature")), Some(23.0));
    }
}
