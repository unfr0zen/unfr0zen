//! unfr0zen on the host: the unit found on USB and told apart by device, the
//! firmware and payloads embedded at build time, and the recovery flow, each
//! device's transports and steps in its own module. The flow reports through
//! a [`Progress`] so a terminal and a window can drive the same run.

pub mod assets;
pub mod device;
pub mod diamond3;
pub mod flow;
pub mod j49;
pub mod nand;
pub mod progress;
pub mod usb;

pub use device::{Device, WAIT_USB_BOOT_STAGE};
pub use flow::{run, Config, PatchInputs};
pub use unfr0zen_core::nlclient;
pub use progress::{init_logging, log_path, Progress, Stage};
/// The unfr0zen logo, block-character art with "by @will-tm".
pub const LOGO: &str = include_str!("logo.txt");

/// The colours down the diamond3 boot logo's UNFROZEN text, top to bottom,
/// so the terminal and the window colour the logo the same.
pub use unfr0zen_core::boot_branding::GRADIENT as LOGO_GRADIENT;

/// The boot logo's colour at height `t` (0 at the top, 1 at the bottom).
pub fn logo_color(t: f32) -> [u8; 3] {
    let pos = t.clamp(0.0, 1.0) * (LOGO_GRADIENT.len() - 1) as f32;
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(LOGO_GRADIENT.len() - 1);
    let f = pos - lo as f32;
    std::array::from_fn(|c| {
        (LOGO_GRADIENT[lo][c] as f32 + (LOGO_GRADIENT[hi][c] as f32 - LOGO_GRADIENT[lo][c] as f32) * f).round() as u8
    })
}

pub use unfr0zen_core::nestlocal::DEFAULT_MATTER_CODE;
pub use unfr0zen_core::patch::DEFAULT_NTP_SERVER;
pub use unfr0zen_core::{Error, Result, Slot};
