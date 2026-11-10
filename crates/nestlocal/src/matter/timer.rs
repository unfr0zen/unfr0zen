//! The time rs-matter's timers run on. A subscription's report is due by a
//! deadline the controller holds the unit to, and the unit sleeps through
//! most of it: the clock has to count the time asleep, which
//! `std::time::Instant` (CLOCK_MONOTONIC) does not. This driver reads
//! CLOCK_BOOTTIME instead.
//!
//! The alarm thread's wait is the system's, which stops with the unit: after
//! a sleep it is as long as it was before. `resumed` (each wake calls it) has
//! it look at the clock again, and so does the wait's cap, for a wake nothing
//! reports.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

use embassy_time_driver::Driver;
use embassy_time_queue_utils::Queue;

/// The longest the alarm thread waits without reading the clock.
const WAIT_CAP: Duration = Duration::from_secs(30);

struct BootTimeDriver {
    inner: Mutex<Inner>,
    changed: Condvar,
}

struct Inner {
    /// The clock at the first use: time is counted from it.
    zero: Option<u64>,
    queue: Queue,
    /// Set for the alarm thread to read the clock and the queue again.
    signaled: bool,
}

embassy_time_driver::time_driver_impl!(static DRIVER: BootTimeDriver = BootTimeDriver {
    inner: Mutex::new(Inner { zero: None, queue: Queue::new(), signaled: false }),
    changed: Condvar::new(),
});

/// Microseconds on a clock that counts the time the unit sleeps.
#[cfg(target_os = "linux")]
fn clock_micros() -> u64 {
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // CLOCK_BOOTTIME cannot fail on the kernels the units run (2.6.39 on).
    unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) };
    time.tv_sec as u64 * 1_000_000 + time.tv_nsec as u64 / 1_000
}

/// Elsewhere (the tests' hosts) nothing sleeps under nestlocal.
#[cfg(not(target_os = "linux"))]
fn clock_micros() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_micros() as u64
}

impl Inner {
    fn now(&mut self) -> u64 {
        let clock = clock_micros();
        let zero = *self.zero.get_or_insert_with(|| {
            std::thread::spawn(alarm_thread);
            clock
        });
        clock - zero
    }
}

impl Driver for BootTimeDriver {
    fn now(&self) -> u64 {
        self.inner.lock().unwrap().now()
    }

    fn schedule_wake(&self, at: u64, waker: &core::task::Waker) {
        let mut inner = self.inner.lock().unwrap();
        inner.now();
        if inner.queue.schedule_wake(at, waker) {
            inner.signaled = true;
            self.changed.notify_one();
        }
    }
}

fn alarm_thread() {
    let mut inner = DRIVER.inner.lock().unwrap();
    loop {
        inner.signaled = false;
        let now = inner.now();
        // Wakes what is due, and says when the next is.
        let next = inner.queue.next_expiration(now);
        let wait = Duration::from_micros(next.saturating_sub(now)).min(WAIT_CAP);
        inner = DRIVER.changed.wait_timeout_while(inner, wait, |inner| !inner.signaled).unwrap().0;
    }
}

/// The unit woke: timers that came due while it slept fire now.
pub fn resumed() {
    DRIVER.inner.lock().unwrap().signaled = true;
    DRIVER.changed.notify_one();
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    #[test]
    fn a_timer_fires_when_it_is_due_and_time_moves_forward() {
        let before = embassy_time::Instant::now();
        let started = Instant::now();
        futures_lite::future::block_on(embassy_time::Timer::after(embassy_time::Duration::from_millis(50)));
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(embassy_time::Instant::now() >= before + embassy_time::Duration::from_millis(50));
    }

    #[test]
    fn resumed_leaves_pending_timers_to_their_time() {
        let started = Instant::now();
        let timer = embassy_time::Timer::after(embassy_time::Duration::from_millis(80));
        super::resumed();
        futures_lite::future::block_on(timer);
        assert!(started.elapsed() >= Duration::from_millis(80));
    }
}
