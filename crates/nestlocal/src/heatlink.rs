//! A Heat Link unit whose radio nestlocal holds. Firmware 6.4-8 on a gen3
//! Display-3.x or Display-5.x expects a Heat Link: with no W1 wired it locks
//! the screen on H71 ("can't connect to the Heat Link", 0x2e4678), and the
//! Heat Link only connects through the radio. nlclient skips that check for
//! a Works with Nest boiler, which it reads at start from using_wwn_boiler in
//! its settings file. With it the unit still heats through W1 and keeps its
//! safety temperature, but reports can_heat false (bridge.rs reports it
//! true). nestlocal sets it before nlclient starts, and takes back only what
//! it set: a marker file next to its store records that. When the wiring
//! nlclient reports later changes what is wanted, nestlocal restarts
//! nlclient around the change (keep).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use crate::logf;
use crate::nlsettings;
use crate::store::Store;

pub const KEY: &str = "using_wwn_boiler";

/// Whether the firmware expects a Heat Link on this gen3 model.
pub fn heat_link_model(model: &str) -> bool {
    model.starts_with("Display-3.") || model.starts_with("Display-5.")
}

/// The unit's model: nlmodel as U-Boot passes it to the kernel, else from
/// U-Boot's environment.
pub fn model() -> Option<String> {
    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    if let Some(model) = cmdline.split_whitespace().find_map(|w| w.strip_prefix("nlmodel=")) {
        return Some(model.to_string());
    }
    let out = Command::new("fw_printenv").args(["-n", "nlmodel"]).output().ok()?;
    let model = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !model.is_empty()).then_some(model)
}

/// Whether W1 is among the pins nlclient last reported (its device bucket's
/// hvac_pins, "W1,Y1,C,Rh,G"); None before it has reported them.
pub fn w1_wired(store: &Store, serial: &str) -> Option<bool> {
    let pins = store.get(&format!("device.{serial}"))?.value.get("hvac_pins")?.as_str()?.to_string();
    Some(pins.split(',').any(|pin| pin.trim() == "W1"))
}

/// Whether nestlocal holds the radio of a gen3 Heat Link unit.
pub fn heat_link_unit(holds_radio: bool, generation: u8, model: Option<&str>) -> bool {
    holds_radio && generation == 3 && model.is_some_and(heat_link_model)
}

/// Whether nlclient should run with using_wwn_boiler: on such a unit with no
/// W1 wired, or none reported yet. With W1 the firmware sees a wired Heat
/// Link and shows no H71.
pub fn wanted(holds_radio: bool, generation: u8, model: Option<&str>, w1: Option<bool>) -> bool {
    heat_link_unit(holds_radio, generation, model) && w1 != Some(true)
}

/// Where nestlocal records that it set using_wwn_boiler: beside its store.
pub fn marker(state: &str) -> PathBuf {
    Path::new(state).with_file_name("nestlocal-wwn-boiler")
}

/// Whether the settings file at path has using_wwn_boiler on, as nlclient
/// reads it: a number other than 0.
pub fn using(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|b| on(&b))
}

fn on(b: &[u8]) -> bool {
    nlsettings::value(b, KEY).and_then(|v| std::str::from_utf8(v).ok()?.trim().parse::<i64>().ok()).is_some_and(|v| v != 0)
}

/// Sets using_wwn_boiler in the settings file when wanted, or removes it when
/// not and nestlocal set it. Some(true) when it set it, Some(false) when it
/// removed it.
pub fn prepare(settings: &Path, marker: &Path, wanted: bool) -> io::Result<Option<bool>> {
    let b = std::fs::read(settings)?;
    let set = on(&b);
    let context = |e: io::Error| io::Error::other(format!("{}: {e}", settings.display()));
    if wanted && !set {
        // The marker first: a setting nestlocal made is never left unmarked.
        std::fs::write(marker, b"")?;
        nlsettings::write(settings, &nlsettings::set(&b, KEY, "1").map_err(context)?)?;
        return Ok(Some(true));
    }
    if !wanted && marker.exists() {
        if set {
            nlsettings::write(settings, &nlsettings::remove(&b, KEY))?;
        }
        std::fs::remove_file(marker)?;
        return Ok(set.then_some(false));
    }
    Ok(None)
}

/// Whether nlclient must restart for using_wwn_boiler to become wanted:
/// set when wanted, removed when not wanted and nestlocal set it.
fn restart_needed(wanted: bool, set: bool, ours: bool) -> bool {
    (wanted && !set) || (!wanted && set && ours)
}

/// The settings file and marker of a gen3 Heat Link unit whose radio
/// nestlocal holds, and its serial.
pub struct Unit {
    pub serial: String,
    pub settings: PathBuf,
    pub marker: PathBuf,
}

/// Keeps using_wwn_boiler as the wiring nlclient reports wants it, on a
/// thread of its own. nlclient reads the setting only at start and writes
/// its settings back whole, so it is stopped around the change and started
/// again. `changed` hears what nlclient now runs with.
pub fn keep(store: Arc<Store>, unit: Unit, changed: impl Fn(bool) + Send + 'static) {
    let (wiring, reports) = mpsc::channel();
    let key = format!("device.{}", unit.serial);
    store.watch(move |o| {
        if o.key == key {
            let _ = wiring.send(());
        }
    });
    let spawned = std::thread::Builder::new().name("heatlink".into()).spawn(move || {
        let mut last = w1_wired(&store, &unit.serial);
        while reports.recv().is_ok() {
            let w1 = w1_wired(&store, &unit.serial);
            if w1 == last {
                continue;
            }
            last = w1;
            let wanted = w1 != Some(true);
            if !restart_needed(wanted, using(&unit.settings), unit.marker.exists()) {
                continue;
            }
            logf!("nestlocal: W1 {}: restarting nlclient to {} {KEY}",
                  if wanted { "gone" } else { "wired" }, if wanted { "set" } else { "remove" });
            if !stop_nlclient() {
                logf!("nestlocal: nlclient did not stop; {KEY} left as it is");
            } else if let Err(e) = prepare(&unit.settings, &unit.marker, wanted) {
                logf!("nestlocal: {e}");
            }
            changed(using(&unit.settings));
            start_nlclient();
        }
    });
    if let Err(e) = spawned {
        logf!("nestlocal: {e}");
    }
}

/// Stops nlclient as the Doom launcher does: monit lets it go, then TERM,
/// then KILL. True once it has gone.
fn stop_nlclient() -> bool {
    let _ = Command::new("monit").args(["unmonitor", "nlclient"]).output();
    for attempt in 0..10 {
        let pids = crate::pids("nlclient");
        if pids.is_empty() {
            return true;
        }
        let signal = if attempt < 5 { libc::SIGTERM } else { libc::SIGKILL };
        for pid in pids {
            // SAFETY: kill takes any pid and signal number.
            unsafe { libc::kill(pid, signal) };
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    crate::pids("nlclient").is_empty()
}

fn start_nlclient() {
    let _ = Command::new("monit").args(["monitor", "nlclient"]).output();
    let _ = Command::new("monit").args(["start", "nlclient"]).output();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields;
    use crate::store::tests::temp_store;

    #[test]
    fn only_heat_link_models() {
        assert!(heat_link_model("Display-3.4") && heat_link_model("Display-5.1"));
        assert!(!heat_link_model("Display-2.7") && !heat_link_model("Display-13.1"));
    }

    #[test]
    fn w1_from_the_reported_pins() {
        let s = temp_store();
        assert_eq!(w1_wired(&s, "S"), None);
        s.merge("device.S", &fields!{"hvac_pins" => "C,Rh"}, 0, 0, true);
        assert_eq!(w1_wired(&s, "S"), Some(false));
        s.merge("device.S", &fields!{"hvac_pins" => "W1,Y1,C,Rh,G"}, 0, 0, true);
        assert_eq!(w1_wired(&s, "S"), Some(true));
    }

    #[test]
    fn wanted_only_while_nestlocal_holds_a_heat_link_units_radio() {
        let eu = Some("Display-3.4");
        assert!(wanted(true, 3, eu, Some(false)));
        assert!(wanted(true, 3, eu, None), "wiring not reported yet");
        assert!(!wanted(true, 3, eu, Some(true)), "W1: a wired Heat Link");
        assert!(!wanted(false, 3, eu, Some(false)), "the radio is Nest's");
        assert!(!wanted(true, 3, Some("Display-2.7"), Some(false)));
        assert!(!wanted(true, 3, None, Some(false)));
        assert!(!wanted(true, 2, eu, Some(false)));
    }

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nestlocal-heatlink-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const STOCK: &str = "<settings>\n  <s key=\"a\" value=\"1\"/>\n</settings>\n";

    #[test]
    fn sets_then_takes_back_its_own() {
        let dir = dir("own");
        let (settings, marker) = (dir.join("settings.config"), dir.join("marker"));
        std::fs::write(&settings, STOCK).unwrap();
        assert_eq!(prepare(&settings, &marker, true).unwrap(), Some(true));
        assert!(using(&settings) && marker.exists());
        assert_eq!(prepare(&settings, &marker, true).unwrap(), None);
        assert_eq!(prepare(&settings, &marker, false).unwrap(), Some(false));
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), STOCK);
        assert!(!marker.exists());
    }

    #[test]
    fn leaves_a_setting_it_did_not_make() {
        let dir = dir("theirs");
        let (settings, marker) = (dir.join("settings.config"), dir.join("marker"));
        let wwn = STOCK.replace("</settings>", "  <s key=\"using_wwn_boiler\" value=\"1\"/>\n</settings>");
        std::fs::write(&settings, &wwn).unwrap();
        assert_eq!(prepare(&settings, &marker, true).unwrap(), None);
        assert!(!marker.exists());
        assert_eq!(prepare(&settings, &marker, false).unwrap(), None);
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), wwn);
    }

    #[test]
    fn a_marker_without_its_setting_goes() {
        let dir = dir("stale");
        let (settings, marker) = (dir.join("settings.config"), dir.join("marker"));
        std::fs::write(&settings, STOCK).unwrap();
        std::fs::write(&marker, b"").unwrap();
        assert_eq!(prepare(&settings, &marker, false).unwrap(), None);
        assert!(!marker.exists());
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), STOCK);
    }

    #[test]
    fn zero_is_off() {
        assert!(!on(br#"<s key="using_wwn_boiler" value="0"/>"#) && on(br#"<s key="using_wwn_boiler" value="1"/>"#));
        assert!(!on(b"<settings/>") && !using(Path::new("/nonexistent/settings.config")));
    }

    #[test]
    fn restarts_only_for_a_change_it_may_make() {
        assert!(restart_needed(true, false, false), "set it");
        assert!(restart_needed(false, true, true), "take back its own");
        assert!(!restart_needed(false, true, false), "a setting it did not make stays");
        assert!(!restart_needed(true, true, false) && !restart_needed(false, false, true));
    }

    #[test]
    fn the_marker_sits_beside_the_store() {
        assert_eq!(marker("/media/data/nestlocal.json"), Path::new("/media/data/nestlocal-wwn-boiler"));
    }
}
