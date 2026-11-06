//! What is on USB right now, polled on a background thread, so the window
//! names the unit and its mode before a run and between runs.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use unfr0zen_host::device;
use unfr0zen_host::usb::{self, Id};
use unfr0zen_host::Device;

/// A supported unit seen on the bus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub device: Device,
    pub mode: &'static str,
    pub port: String,
    pub id: Id,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Bus {
    #[default]
    Nothing,
    One(Seen),
    Several(Vec<Seen>),
}

impl Bus {
    fn read() -> Bus {
        let mut seen: Vec<Seen> = usb::list().unwrap_or_default().into_iter().filter_map(|(port, id)| {
            let device = device::identify(id)?;
            Some(Seen { device, mode: device::mode_name(device, id).unwrap_or(crate::tr!("bus.unknown_mode")), port, id })
        }).collect();
        match seen.len() {
            0 => Bus::Nothing,
            1 => Bus::One(seen.remove(0)),
            _ => Bus::Several(seen),
        }
    }

    /// The unit and its mode, for the status bar; the application is the
    /// usual mode and goes unsaid.
    pub fn summary(&self) -> String {
        match self {
            Bus::Nothing => crate::tr!("bus.disconnected").into(),
            Bus::One(seen) if seen.mode == "app" => seen.device.description().to_string(),
            Bus::One(seen) => crate::tr!("bus.unit_mode", device = seen.device.description(), mode = seen.mode),
            Bus::Several(seen) => crate::tr!("bus.several_units", count = seen.len()),
        }
    }
}

/// The poller. Default has no thread: the bus stays empty, for tests.
#[derive(Default)]
pub struct Watcher {
    state: Arc<Mutex<Bus>>,
}

const POLL: Duration = Duration::from_secs(1);

impl Watcher {
    /// Polls the bus every second for the window's lifetime, repainting
    /// when something changed.
    pub fn start(ctx: egui::Context) -> Watcher {
        let watcher = Watcher::default();
        let state = watcher.state.clone();
        thread::spawn(move || loop {
            let bus = Bus::read();
            let changed = {
                let mut current = state.lock().unwrap();
                let changed = *current != bus;
                *current = bus;
                changed
            };
            if changed {
                ctx.request_repaint();
            }
            thread::sleep(POLL);
        });
        watcher
    }

    /// A watcher fixed at one bus state, with no polling thread.
    #[cfg(test)]
    pub fn fixed(bus: Bus) -> Watcher {
        Watcher { state: Arc::new(Mutex::new(bus)) }
    }

    pub fn current(&self) -> Bus {
        self.state.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_names_the_unit_and_its_mode() {
        let seen = Seen { device: Device::Diamond3, mode: "USB download (SDP)", port: "20-1".into(), id: (0x15a2, 0x0071) };
        assert_eq!(Bus::One(seen.clone()).summary(), "Nest gen 3 (diamond3) · USB download (SDP)");
        let app = Seen { mode: "app", id: (0x2464, 0x0030), ..seen.clone() };
        assert_eq!(Bus::One(app).summary(), "Nest gen 3 (diamond3)", "the usual mode goes unsaid");
        assert_eq!(Bus::Several(vec![seen.clone(), seen]).summary(), "2 units on USB; connect one");
        assert_eq!(Bus::Nothing.summary(), "Disconnected");
        assert_eq!(Watcher::default().current(), Bus::Nothing);
    }
}
