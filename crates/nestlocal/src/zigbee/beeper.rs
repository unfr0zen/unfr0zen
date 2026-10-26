//! Audible feedback for pairing requested through the settings menu.

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

const BEEPER: &str = "pwm-beeper";
/// A struct input_event: a timeval of two longs, type, code, value.
const EVENT: usize = 2 * std::mem::size_of::<libc::c_long>() + 8;
const EV_SND: u16 = 0x12;
const SND_TONE: u16 = 0x02;

/// The input device the kernel names `name`.
fn device(name: &str) -> Option<PathBuf> {
    std::fs::read_dir("/sys/class/input").ok()?.flatten().find_map(|entry| {
        let event = entry.file_name().into_string().ok().filter(|n| n.starts_with("event"))?;
        let found = std::fs::read_to_string(entry.path().join("device/name")).ok()?;
        (found.trim_end() == name).then(|| PathBuf::from("/dev/input").join(event))
    })
}

/// An event of type `kind`, code and value, untimed.
fn event(kind: u16, code: u16, value: i32) -> [u8; EVENT] {
    let mut event = [0; EVENT];
    let at = EVENT - 8;
    event[at..at + 2].copy_from_slice(&kind.to_ne_bytes());
    event[at + 2..at + 4].copy_from_slice(&code.to_ne_bytes());
    event[at + 4..].copy_from_slice(&value.to_ne_bytes());
    event
}

/// A short beep, if the unit has a beeper.
pub fn beep() {
    let Some(mut beeper) = device(BEEPER).and_then(|path| File::options().write(true).open(path).ok()) else {
        return;
    };
    let _ = beeper.write_all(&event(EV_SND, SND_TONE, 2000));
    std::thread::sleep(Duration::from_millis(150));
    let _ = beeper.write_all(&event(EV_SND, SND_TONE, 0));
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn an_event_is_laid_out_as_the_kernels_input_event() {
        use std::mem::{offset_of, size_of};
        assert_eq!(EVENT, size_of::<libc::input_event>());
        let e = event(EV_SND, SND_TONE, 2000);
        let at = |offset: usize, n: usize| &e[offset..offset + n];
        assert!(at(0, offset_of!(libc::input_event, type_)).iter().all(|&b| b == 0)); // untimed
        assert_eq!(at(offset_of!(libc::input_event, type_), 2), EV_SND.to_ne_bytes());
        assert_eq!(at(offset_of!(libc::input_event, code), 2), SND_TONE.to_ne_bytes());
        assert_eq!(at(offset_of!(libc::input_event, value), 4), 2000i32.to_ne_bytes());
    }
}
