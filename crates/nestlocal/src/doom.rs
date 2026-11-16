//! Doom, chosen in the DOOM settings entry: nlclient cannot start a program, so
//! it leaves doom.request and this runs the launcher, which stops nlclient,
//! plays and starts nlclient again. The launcher has a session of its own,
//! so a restart of nestlocal leaves the game running.

use std::process::{Command, Stdio};
use std::time::Duration;

use crate::{logf, settings_ui};

/// The launcher: on the gen 3's scratch volume, else (the gen 1 and gen 2, or
/// a unit set up before) the root volume.
const LAUNCHERS: [&str; 2] = ["/media/scratch/.unfr0zen/doom/unfr0zen-doom", "/nestlabs/sbin/unfr0zen-doom"];

pub fn start() {
    let spawned = std::thread::Builder::new().name("doom".into()).spawn(|| loop {
        std::thread::sleep(Duration::from_millis(500));
        if !settings_ui::take_request("doom") {
            continue;
        }
        logf!("doom: launching");
        let launcher = crate::scratch::installed(&LAUNCHERS);
        match Command::new("setsid")
            .arg(launcher)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) => logf!("doom: over ({status})"),
            Err(e) => logf!("doom: {launcher}: {e}"),
        }
        // A press made while the game ran is not a new request.
        settings_ui::take_request("doom");
    });
    if let Err(e) = spawned {
        logf!("doom: {e}");
    }
}
