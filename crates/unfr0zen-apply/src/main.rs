//! unfr0zen-apply: runs a patch bundle's patch on the unit, against the root
//! volume mounted at ROOT, with a run's options.json.
//!
//!     unfr0zen-apply BUNDLE OPTIONS ROOT OUT [--dry-run]
//!
//! The bundle's pre-patch script runs first, then the patch, then (unless
//! dry run) its post-patch script. OUT gets result.json (the changes, and
//! the nlclient build's manifest), nlclient.stock when the unit's nlclient
//! is one no review covers, and the scripts as they ran. The log goes to
//! stderr. Exits non-zero with the reason on stderr when the run fails.

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
