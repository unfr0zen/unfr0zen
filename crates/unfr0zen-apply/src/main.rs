//! unfr0zen-apply: runs a setup bundle's setup on the unit, against the root
//! volume mounted at ROOT, with a run's options.json.
//!
//!     unfr0zen-apply BUNDLE OPTIONS ROOT OUT [--dry-run] [--live]
//!     unfr0zen-apply update BUNDLE
//!
//! The bundle's pre-setup script runs first, then the setup, then (unless
//! dry run) its post-setup script. OUT gets result.json (the changes, and
//! the nlclient build's manifest), nlclient.stock when the unit's nlclient
//! is one no review covers, and the scripts as they ran. --live sets up the
//! root the unit is running from, which stays writable, and takes the
//! version from the bundle.
//!
//! update is the unit's own engine (/media/scratch/.unfr0zen/bin/
//! unfr0zen-apply on the scratch volume) taking a new bundle: it checks it against its manifest and the release key, and
//! refuses one older than the unit runs, unless the unit was set up to allow
//! unsigned updates, then runs the bundle's engine --live on / with the
//! unit's options.json, its OUT /media/scratch/.unfr0zen/update-run, removed
//! once the run succeeds. A gen 1 or gen 2 stops nlclient before reading the
//! bundle and leaves it stopped for the caller, which restarts the unit; a
//! failed update starts it again.
//!
//! The log goes to stderr. Exits non-zero with the reason on stderr when the
//! run fails.

#[cfg(unix)]
mod apply;

#[cfg(unix)]
fn main() {
    apply::main()
}

#[cfg(not(unix))]
fn main() {
    eprintln!("unfr0zen-apply runs on the unit");
    std::process::exit(2);
}
