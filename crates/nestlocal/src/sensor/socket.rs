//! Gen 2 readings enter the patched nlclient over its loopback RCS input.
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::{mpsc, Arc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{beacon, boot_time, Sensor, REPEAT};
use crate::logf;

const IO_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
struct Reading {
    tenths: i16,
    at: Option<Duration>,
}

fn due(sensor: &Sensor, now: Duration) -> Option<Reading> {
    let state = sensor.state.lock().unwrap();
    if let Some(tenths) = sensor.fresh(&state, now) {
        if state.pending || state.last_beacon.is_none_or(|at| now.saturating_sub(at) >= REPEAT) {
            return Some(Reading { tenths, at: state.reading.map(|(_, at)| at) });
        }
    } else if state.reading.is_none() && !state.introduced {
        let tenths = sensor.introduction.get().and_then(|temperature| temperature()).and_then(beacon::tenths)?;
        return Some(Reading { tenths, at: None });
    }
    None
}

fn transmit(address: SocketAddr, bucket: &str, tenths: i16) -> io::Result<()> {
    let stream = TcpStream::connect_timeout(&address, IO_TIMEOUT)
        .map_err(|e| io::Error::new(e.kind(), format!("connect RCS input: {e}")))?;
    exchange(stream, bucket, tenths)
}

fn exchange(mut stream: TcpStream, bucket: &str, tenths: i16) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let command = format!("kryptonitedata {bucket} -30 {:.1} 100\n", f64::from(tenths) / 10.0);
    stream.write_all(command.as_bytes())?;
    stream.shutdown(Shutdown::Write)?;
    let mut reply = Vec::new();
    stream.take(256).read_to_end(&mut reply)?;
    if reply != b"Done" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "nlclient rejected the external temperature"));
    }
    Ok(())
}

fn delivered(sensor: &Sensor, reading: Reading, now: Duration) {
    let mut state = sensor.state.lock().unwrap();
    if let Some(at) = reading.at {
        state.last_beacon = Some(now);
        if state.reading == Some((reading.tenths, at)) {
            state.pending = false;
        }
    } else {
        state.introduced = true;
    }
}

fn client_process() -> Option<(u32, u64)> {
    let pid = std::fs::read_to_string("/var/run/nlclient.pid").ok()?.trim().parse::<u32>().ok()?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat.rsplit_once(") ")?.1;
    let started = fields.split_whitespace().nth(19)?.parse().ok()?;
    Some((pid, started))
}

fn watch_restart(store: &Arc<crate::store::Store>, serial: &str, sensor: &Arc<Sensor>, booting: Arc<AtomicBool>) {
    let key = format!("rcs_settings.{serial}");
    let (weak, sensor, serial) = (Arc::downgrade(store), Arc::downgrade(sensor), serial.to_string());
    store.watch(move |object| {
        if object.key != key || object.value.get("rcs_control_setting").and_then(serde_json::Value::as_str) != Some("OFF")
            || !booting.swap(false, Ordering::SeqCst) { return; }
        let (store, sensor, serial) = (weak.clone(), sensor.clone(), serial.clone());
        std::thread::spawn(move || {
            // The OFF upload must finish before its acknowledgement can
            // swallow a new revision of this same bucket.
            std::thread::sleep(super::SELECT_AFTER);
            if let (Some(store), Some(sensor)) = (store.upgrade(), sensor.upgrade()) {
                // Native startup must load the association before it can
                // accept a sample, including before the first real reading.
                super::choose(&store, &serial, &sensor.bucket, false);
                if sensor.has_reading() { sensor.select_when_ready(&store, &serial); }
            }
        });
    });
}

/// Retry across nlclient starts, without extending a stale reading or claiming
/// a delivery until the input acknowledges it. No Bluetooth is used on j49.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn start(sensor: Arc<Sensor>, store: Arc<crate::store::Store>, serial: String) -> io::Result<()> {
    const RETRY: Duration = Duration::from_secs(1);
    let address = "127.0.0.1:1234".parse().expect("constant loopback address");
    let (send, receive) = mpsc::sync_channel(1);
    sensor.set_notify(move || { let _ = send.try_send(()); });
    let select = Arc::new(AtomicBool::new(false));
    let requested = select.clone();
    let _ = sensor.select_after_delivery.set(Box::new(move || requested.store(true, Ordering::SeqCst)));
    let booting = Arc::new(AtomicBool::new(false));
    watch_restart(&store, &serial, &sensor, booting.clone());
    std::thread::Builder::new().name("rcs-input".into()).spawn(move || {
        let mut error = None;
        let mut process = client_process();
        loop {
            let current = client_process();
            if current != process {
                process = current;
                {
                    let mut state = sensor.state.lock().unwrap();
                    state.last_beacon = None;
                    state.pending = true;
                    if state.reading.is_none() { state.introduced = false; }
                }
                if current.is_some() {
                    // Unlike BLE startup, j49's UI also emits OFF when the
                    // owner chooses its backplate. Only a new process should
                    // restore the external default, after accepting a sample.
                    booting.store(true, Ordering::SeqCst);
                    super::choose(&store, &serial, &sensor.bucket, false);
                }
            }
            if let Some(reading) = due(&sensor, boot_time()) {
                match transmit(address, &sensor.bucket, reading.tenths) {
                    Ok(()) => {
                        delivered(&sensor, reading, boot_time());
                        if reading.at.is_some() && select.swap(false, Ordering::SeqCst) {
                            super::select(&store, &serial, &sensor.bucket);
                        }
                        sensor.publish_changes(false);
                        error = None;
                    }
                    Err(e) => {
                        if error != Some(e.kind()) {
                            logf!("external temperature: RCS input: {e}");
                            error = Some(e.kind());
                        }
                    }
                }
            }
            sensor.quiet_tick();
            let _ = receive.recv_timeout(RETRY);
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn sensor() -> Arc<Sensor> {
        let path = std::env::temp_dir().join(format!("nestlocal-socket-{}-{:?}.json", std::process::id(), std::thread::current().id()));
        Sensor::open(&path, "S", std::path::Path::new("/no-settings"), Duration::from_secs(120)).unwrap()
    }

    #[test]
    fn input_uses_the_canonical_bucket_and_requires_an_acknowledgement() {
        for (reply, ok) in [(b"Done".as_slice(), true), (b"Retry".as_slice(), false),
                            (b"Unknown command".as_slice(), false), (b"".as_slice(), false)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let worker = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                stream.read_to_string(&mut request).unwrap();
                assert_eq!(request, "kryptonitedata kryptonite.18B4301234567890 -30 20.0 100\n");
                stream.write_all(reply).unwrap();
            });
            let stream = TcpStream::connect(address).unwrap();
            let result = exchange(stream, "kryptonite.18B4301234567890", 200);
            assert_eq!(result.is_ok(), ok, "{result:?}");
            worker.join().unwrap();
        }
    }

    #[test]
    fn startup_off_is_retried_but_manual_backplate_off_is_respected() {
        let sensor = sensor();
        let store = crate::store::tests::temp_store();
        sensor.set(20.0).unwrap();
        let requested = Arc::new(AtomicBool::new(false));
        let flag = requested.clone();
        sensor.select_after_delivery.set(Box::new(move || flag.store(true, Ordering::SeqCst))).ok().unwrap();
        let booting = Arc::new(AtomicBool::new(true));
        watch_restart(&store, "S", &sensor, booting.clone());
        let off = crate::fields!{"rcs_control_setting" => "OFF"};
        let override_mode = crate::fields!{"rcs_control_setting" => "OVERRIDE"};
        store.merge("rcs_settings.S", &off, 0, 0, true);
        for _ in 0..100 {
            if requested.load(Ordering::SeqCst) { break; }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(requested.load(Ordering::SeqCst));
        assert!(!booting.load(Ordering::SeqCst));
        let settings = store.get("rcs_settings.S").unwrap();
        assert_eq!(settings.value["rcs_control_setting"], "OVERRIDE");
        assert_eq!(settings.value["associated_rcs_sensors"], serde_json::json!([sensor.bucket]));
        assert_eq!(settings.value["active_rcs_sensors"], serde_json::json!([]));
        requested.store(false, Ordering::SeqCst);
        store.merge("rcs_settings.S", &override_mode, 0, 0, false);
        store.merge("rcs_settings.S", &off, 0, 0, true);
        std::thread::sleep(super::super::SELECT_AFTER + Duration::from_millis(20));
        assert!(!requested.load(Ordering::SeqCst));
    }

    #[test]
    fn startup_restores_association_before_the_first_reading() {
        let sensor = sensor();
        let store = crate::store::tests::temp_store();
        let booting = Arc::new(AtomicBool::new(true));
        watch_restart(&store, "S", &sensor, booting);
        store.merge("rcs_settings.S", &crate::fields!{"rcs_control_setting" => "OFF"}, 0, 0, true);
        for _ in 0..100 {
            if store.get("rcs_settings.S").unwrap().value["rcs_control_setting"] == "OVERRIDE" { break; }
            std::thread::sleep(Duration::from_millis(2));
        }
        let settings = store.get("rcs_settings.S").unwrap();
        assert_eq!(settings.value["rcs_control_setting"], "OVERRIDE");
        assert_eq!(settings.value["associated_rcs_sensors"], serde_json::json!([sensor.bucket]));
        assert_eq!(settings.value["active_rcs_sensors"], serde_json::json!([]));
        assert!(!sensor.has_reading());
    }

    #[test]
    fn introduction_is_once_and_does_not_claim_external_control() {
        let sensor = sensor();
        sensor.introduce_with(|| Some(21.5));
        let now = boot_time();
        let first = due(&sensor, now).unwrap();
        assert_eq!(first.tenths, 215);
        assert!(first.at.is_none());
        assert!(due(&sensor, now).is_some(), "an unacknowledged introduction is retried");
        delivered(&sensor, first, now);
        assert!(due(&sensor, now).is_none());
        assert_eq!(sensor.source(), "backplate");
    }

    #[test]
    fn fresh_readings_repeat_but_stale_readings_do_not() {
        let sensor = sensor();
        sensor.set(20.0).unwrap();
        let now = boot_time();
        let reading = due(&sensor, now).unwrap();
        assert_eq!(reading.tenths, 200);
        assert!(sensor.state.lock().unwrap().pending);
        delivered(&sensor, reading, now);
        assert!(!sensor.state.lock().unwrap().pending);
        assert!(due(&sensor, now + Duration::from_secs(1)).is_none());
        assert!(due(&sensor, now + REPEAT).is_some());
        assert!(due(&sensor, now + Duration::from_secs(120)).is_none());
    }

    #[test]
    fn acknowledging_an_old_reading_does_not_clear_a_new_one() {
        let sensor = sensor();
        sensor.set(20.0).unwrap();
        let now = boot_time();
        let first = due(&sensor, now).unwrap();
        sensor.set(22.0).unwrap();
        delivered(&sensor, first, now);
        assert!(sensor.state.lock().unwrap().pending);
        assert_eq!(due(&sensor, boot_time()).unwrap().tenths, 220);
    }
}
