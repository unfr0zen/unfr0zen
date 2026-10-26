//! Whether nlclient is still talking to nestlocal, for the availability
//! topic. The broker's will only covers nestlocal itself: if nlclient hangs
//! or stops calling in, Home Assistant would go on showing the last values
//! as live. nlclient holds a subscribe open almost all the time (hold, 290
//! s), and calls again at once when it ends, so any request in flight
//! counts, and after the last one ends the unit stays alive for stale.
//! A unit that hangs with a subscribe held shows as offline stale after
//! the hold runs out.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::logf;

type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;
type OnChange = Arc<dyn Fn(bool) + Send + Sync>;

struct State {
    in_flight: i64,
    last: Option<Instant>,
    alive: bool,
    on_change: Option<OnChange>,
}

pub struct Liveness {
    stale: Duration,
    now: Clock,
    state: Mutex<State>,
}

/// One request from nlclient, counted while it lasts.
pub struct InFlight(Arc<Liveness>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.update(-1);
    }
}

impl Liveness {
    pub fn new(stale: Duration) -> Arc<Liveness> {
        Self::with_clock(stale, Arc::new(Instant::now))
    }

    fn with_clock(stale: Duration, now: Clock) -> Arc<Liveness> {
        Arc::new(Liveness { stale, now, state: Mutex::new(State { in_flight: 0, last: None, alive: false, on_change: None }) })
    }

    /// What to call when the unit comes or goes.
    pub fn on_change(&self, f: impl Fn(bool) + Send + Sync + 'static) {
        self.state.lock().unwrap().on_change = Some(Arc::new(f));
    }

    pub fn alive(&self) -> bool {
        self.state.lock().unwrap().alive
    }

    /// Counts a request from nlclient until the returned guard is dropped.
    pub fn request(self: &Arc<Self>) -> InFlight {
        self.update(1);
        InFlight(self.clone())
    }

    /// Notices silence.
    pub async fn run(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        loop {
            tick.tick().await;
            self.update(0);
        }
    }

    fn update(&self, delta: i64) {
        let now = (self.now)();
        let (changed, alive, f) = {
            let mut s = self.state.lock().unwrap();
            s.in_flight += delta;
            if delta != 0 {
                s.last = Some(now);
            }
            let alive = s.in_flight > 0 || s.last.is_some_and(|last| now.duration_since(last) < self.stale);
            let changed = alive != s.alive;
            s.alive = alive;
            (changed, alive, s.on_change.clone())
        };
        if changed {
            if alive {
                logf!("nestlocal: nlclient is calling in");
            } else {
                logf!("nestlocal: nothing from nlclient for {}, availability offline", crate::logger::duration(self.stale));
            }
            if let Some(f) = f {
                f(alive);
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness() {
        let now = Arc::new(Mutex::new(Instant::now()));
        let clock = now.clone();
        let l = Liveness::with_clock(Duration::from_secs(300), Arc::new(move || *clock.lock().unwrap()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        l.on_change(move |alive| record.lock().unwrap().push(alive));
        let advance = |secs| *now.lock().unwrap() += Duration::from_secs(secs);

        l.update(0);
        assert!(!l.alive(), "alive before any request");
        // A held subscribe: alive however long it lasts.
        let held = l.request();
        advance(290);
        l.update(0);
        assert!(l.alive(), "not alive with a request in flight");
        drop(held);
        advance(240);
        l.update(0);
        assert!(l.alive(), "offline before stale");
        advance(60);
        l.update(0);
        assert!(!l.alive(), "still alive after stale");
        // Back as soon as it calls again.
        drop(l.request());
        assert!(l.alive());
        assert_eq!(*seen.lock().unwrap(), vec![true, false, true]);
    }

    #[test]
    fn overlapping_requests_and_the_stale_boundary() {
        let now = Arc::new(Mutex::new(Instant::now()));
        let clock = now.clone();
        let l = Liveness::with_clock(Duration::from_secs(300), Arc::new(move || *clock.lock().unwrap()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        l.on_change(move |alive| record.lock().unwrap().push(alive));
        let advance = |secs| *now.lock().unwrap() += Duration::from_secs(secs);

        // A held subscribe and a put beside it: the put's end leaves it alive.
        let held = l.request();
        let put = l.request();
        drop(put);
        advance(1000);
        l.update(0);
        assert!(l.alive(), "the held subscribe no longer counted");
        // Stale counts from the end of the last request, not from its start,
        // and the 15 s ticks do not move it.
        drop(held);
        advance(299);
        l.update(0);
        l.update(0);
        assert!(l.alive(), "offline before stale");
        advance(1);
        l.update(0);
        assert!(!l.alive(), "alive at stale");
        l.update(0);
        assert_eq!(*seen.lock().unwrap(), vec![true, false], "told of something that did not change");
    }

}
