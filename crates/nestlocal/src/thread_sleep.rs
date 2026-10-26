//! The Linux Thread stack may suspend only while its next alarm is armed
//! and the radio interrupt is high. The daemon clears its deadline while busy.

#[cfg(any(target_os = "linux", test))]
fn deadline_ready(state: &[u8], now_us: u64) -> bool {
    state.len() == 16 && u64::from_le_bytes(state[8..16].try_into().unwrap()).saturating_sub(now_us) > 200_000
}

#[cfg(target_os = "linux")]
pub fn ready() -> bool {
    let Ok(state) = std::fs::read("/run/nestlocal-thread-sleep") else { return false; };
    if state.len() != 16 { return false; }
    let pid = u32::from_le_bytes(state[..4].try_into().unwrap());
    let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // The record and OpenThread use CLOCK_BOOTTIME, which includes suspend.
    if pid == 0 || pid > i32::MAX as u32 || unsafe { libc::kill(pid as i32, 0) } != 0
        || unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut now) } != 0 { return false; }
    deadline_ready(&state, now.tv_sec as u64 * 1_000_000 + now.tv_nsec as u64 / 1000)
        && !crate::zigbee::wakeup::pending()
}

#[cfg(not(target_os = "linux"))]
pub fn ready() -> bool { false }

pub fn blocked() -> bool {
    std::path::Path::new("/sys/class/net/thread0").exists() && !ready()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_an_idle_daemon_with_time_to_suspend_and_resume() {
        let mut state = [0; 16];
        assert!(!deadline_ready(&[], 0));
        assert!(!deadline_ready(&state, 0));
        state[8..].copy_from_slice(&5_000_000u64.to_le_bytes());
        assert!(deadline_ready(&state, 4_799_999));
        assert!(!deadline_ready(&state, 4_800_000));
        assert!(!deadline_ready(&state, 6_000_000));
    }
}
