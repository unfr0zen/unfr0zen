//! The names Home Assistant shows the unit by: its device's, and the root of
//! its entity IDs (climate.<root>_thermostat). Home Assistant sets an entity's
//! ID once, when it first discovers it, so a new root reaches the IDs only by
//! removing the entities and announcing them again (Bridge::announce); it then
//! gives them back their settings and history under the new IDs. A Home
//! Assistant that was not running misses the removal and keeps the old IDs, so
//! the first time it starts after a rename, the rename is done again. The names
//! last announced are kept: an install that names neither keeps them.

use serde::{Deserialize, Serialize};

use crate::logf;

pub const DEFAULT_DEVICE: &str = "Nest Thermostat";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Names {
    pub device: String,
    pub root: String,
}

impl Names {
    /// What nestlocal announced before it took names.
    pub fn default_for(serial: &str) -> Names {
        Names { device: DEFAULT_DEVICE.into(), root: format!("nestlocal_{}", serial.to_lowercase()) }
    }
}

/// A root Home Assistant keeps as it is in an entity ID: lowercase letters
/// and digits, words joined by single underscores.
pub fn valid_root(root: &str) -> bool {
    !root.is_empty() && root.split('_').all(|word| !word.is_empty()
        && word.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
}

/// What the store holds.
#[derive(Serialize, Deserialize)]
struct Saved {
    #[serde(flatten)]
    names: Names,
    /// Renamed while Home Assistant may not have been running.
    #[serde(default)]
    unconfirmed: bool,
    /// The name last given to Matter's NodeLabel.
    #[serde(default)]
    matter_label: Option<String>,
}

/// The names wanted, and those Home Assistant was last given.
pub struct Naming {
    pub wanted: Names,
    pub announced: Names,
    unconfirmed: bool,
    /// Home Assistant started since an unconfirmed rename: do it again.
    recheck: bool,
    /// The install named the device; else the name is the one last given.
    named: bool,
    matter_label: Option<String>,
    store: String,
}

impl Naming {
    /// The names given (each None to keep the one last announced), and those
    /// last announced, from the store, or the defaults.
    pub fn load(store: &str, serial: &str, device: Option<String>, root: Option<String>) -> Naming {
        let Saved { names: announced, unconfirmed, matter_label } = std::fs::read(store).ok()
            .and_then(|data| serde_json::from_slice::<Saved>(&data).ok())
            .unwrap_or_else(|| Saved { names: Names::default_for(serial), unconfirmed: false, matter_label: None });
        let named = device.is_some();
        let wanted = Names {
            device: device.unwrap_or_else(|| announced.device.clone()),
            root: root.unwrap_or_else(|| announced.root.clone()),
        };
        Naming { wanted, announced, unconfirmed, recheck: false, named, matter_label, store: store.into() }
    }

    /// The name for Matter's NodeLabel, when the install named the device
    /// otherwise than it last was: a label a controller set since stays.
    pub fn matter_label_due(&self) -> Option<String> {
        (self.named && self.matter_label.as_ref() != Some(&self.wanted.device)).then(|| self.wanted.device.clone())
    }

    /// Matter's NodeLabel is the name given.
    pub fn matter_labelled(&mut self) {
        self.matter_label = Some(self.wanted.device.clone());
        self.save();
    }

    /// Whether the entities are to be removed and announced again.
    pub fn rename_due(&self) -> bool {
        self.announced.root != self.wanted.root || self.recheck
    }

    /// The entities were removed and announced again under the wanted root.
    /// Done when Home Assistant had just started, it surely saw it.
    pub fn renamed(&mut self) {
        self.unconfirmed = !self.recheck;
        self.recheck = false;
        self.announced = self.wanted.clone();
        self.save();
    }

    /// Home Assistant started: whether a rename is to be done again.
    pub fn home_assistant_started(&mut self) -> bool {
        self.recheck |= self.unconfirmed;
        self.recheck
    }

    /// Home Assistant has the wanted names now.
    pub fn announced_as_wanted(&mut self) {
        if self.announced == self.wanted {
            return;
        }
        self.announced = self.wanted.clone();
        self.save();
    }

    fn save(&self) {
        let saved = Saved { names: self.announced.clone(), unconfirmed: self.unconfirmed,
                            matter_label: self.matter_label.clone() };
        if let Err(e) = std::fs::write(&self.store, serde_json::to_vec(&saved).unwrap()) {
            logf!("mqtt: {}: {e}", self.store);
        }
    }
}

/// The device name in a -device-name-file: its first line, trimmed; None when
/// there is no file or the line is empty.
pub fn read_device_name(file: &str) -> Option<String> {
    if file.is_empty() {
        return None;
    }
    match std::fs::read_to_string(file) {
        Ok(text) => Some(text.lines().next().unwrap_or("").trim().to_string()).filter(|name| !name.is_empty()),
        Err(e) => {
            logf!("mqtt: {file}: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_home_assistant_keeps_as_they_are() {
        for good in ["nest_famrm", "nestlocal_09ac01ac", "a", "nest2"] {
            assert!(valid_root(good), "{good}");
        }
        for bad in ["", "Nest", "nest famrm", "nest__famrm", "_nest", "nest_", "nest-famrm", "nest.famrm", "néo"] {
            assert!(!valid_root(bad), "{bad}");
        }
    }

    #[test]
    fn names_not_given_are_the_ones_last_announced() {
        let dir = std::env::temp_dir().join(format!("nestlocal-naming-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = dir.join("ha.json").to_string_lossy().into_owned();
        let _ = std::fs::remove_file(&store);

        let n = Naming::load(&store, "09AC", None, None);
        assert_eq!(n.wanted, Names { device: "Nest Thermostat".into(), root: "nestlocal_09ac".into() });
        assert_eq!(n.announced, n.wanted);

        let mut n = Naming::load(&store, "09AC", Some("Nest FamRm".into()), Some("nest_famrm".into()));
        assert_eq!(n.announced.root, "nestlocal_09ac");
        assert!(n.rename_due());
        n.renamed();
        let mut n = Naming::load(&store, "09AC", None, None);
        assert_eq!(n.wanted, Names { device: "Nest FamRm".into(), root: "nest_famrm".into() });
        assert_eq!(n.announced, n.wanted);
        assert!(!n.rename_due());

        // Home Assistant may have missed that rename: the next time it
        // starts, it is done again, and then no more.
        assert!(n.home_assistant_started() && n.rename_due());
        n.renamed();
        let mut n = Naming::load(&store, "09AC", None, None);
        assert!(!n.home_assistant_started() && !n.rename_due());

        // Matter's label follows a name the install gives, once.
        assert_eq!(n.matter_label_due(), None, "no name given: a controller's label stays");
        let mut n = Naming::load(&store, "09AC", Some("Nest Lounge".into()), None);
        assert_eq!(n.matter_label_due().as_deref(), Some("Nest Lounge"));
        n.matter_labelled();
        assert_eq!(Naming::load(&store, "09AC", Some("Nest Lounge".into()), None).matter_label_due(), None);
        assert_eq!(Naming::load(&store, "09AC", Some("Nest Den".into()), None).matter_label_due().as_deref(), Some("Nest Den"));

        let name = dir.join("name");
        std::fs::write(&name, "  Nest FamRm \nrest\n").unwrap();
        assert_eq!(read_device_name(&name.to_string_lossy()).as_deref(), Some("Nest FamRm"));
        std::fs::write(&name, "\n").unwrap();
        assert_eq!(read_device_name(&name.to_string_lossy()), None);
        assert_eq!(read_device_name(""), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
