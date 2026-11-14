//! Matter schedules (the Thermostat cluster's MSCH feature), kept as the
//! unit keeps its own: the active schedule is the schedule bucket, which the
//! screen, learning and the Nest app edit too, so it is read from there each
//! time; the others wait in schedules.json beside the commissioning.
//!
//! Schedules is written atomically (AtomicRequest): a client begins, writes
//! the list, then commits or rolls back. Until it commits, reads show the
//! committed list and the unit runs it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fields;
use crate::json::Fields;
use crate::logf;
use crate::schedule::{day_bit, from_bucket, half_degree, to_bucket, MINUTES_PER_DAY};
pub use crate::schedule::{Mode, Transition, PER_DAY};

/// The Schedules attribute, the one AtomicRequest takes.
pub const SCHEDULES: u32 = 0x0051;
pub const NUMBER_OF_SCHEDULES: u8 = 4;
/// Transitions in one schedule; each may cover several days.
pub const TRANSITIONS: u8 = 56;
/// The longest atomic write a client is given, ms.
pub const MAX_TIMEOUT_MS: u16 = 30_000;

/// ScheduleDayOfWeekBitmap's Away: no away days here.
const AWAY: u8 = 0x80;

/// Interaction model status codes, as AtomicResponse carries them.
pub mod status {
    pub const SUCCESS: u8 = 0x00;
    pub const FAILURE: u8 = 0x01;
    pub const CONSTRAINT_ERROR: u8 = 0x87;
    pub const RESOURCE_EXHAUSTED: u8 = 0x89;
    pub const NOT_FOUND: u8 = 0x8b;
    pub const BUSY: u8 = 0x9c;
    pub const INVALID_IN_STATE: u8 = 0xcb;
}

pub const INVALID_COMMAND: u8 = 0x85;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// None until a commit gives it one.
    pub handle: Option<Vec<u8>>,
    pub mode: Mode,
    pub name: Option<String>,
    pub transitions: Vec<Transition>,
}

/// What the unit takes, for a commit to check against.
pub struct Limits {
    pub modes: Vec<Mode>,
    pub min: i16,
    pub max: i16,
    /// The least gap between a range's low and high, 0.01 °C.
    pub dead_band: i16,
}

/// What an AtomicRequest answers.
#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub status: u8,
    pub attributes: Vec<(u32, u8)>,
    pub timeout: Option<u16>,
}

impl Outcome {
    fn schedules(status: u8) -> Self {
        let overall = if status == status::SUCCESS { status::SUCCESS } else { status::FAILURE };
        Outcome { status: overall, attributes: vec![(SCHEDULES, status)], timeout: None }
    }
}

/// A write to Schedules, as the list arrives: whole, or item by item after
/// an empty one when it is too long for one message.
pub enum Write {
    Replace(Vec<Schedule>),
    Add(Schedule),
}

/// Who holds an atomic write: fabric index and node id.
pub type Owner = (u8, u64);

/// A pending list as the unit would keep it: setpoints to its half degrees.
fn normalized(mut schedules: Vec<Schedule>) -> Vec<Schedule> {
    for t in schedules.iter_mut().flat_map(|s| s.transitions.iter_mut()) {
        t.heat = t.heat.map(half_degree);
        t.cool = t.cool.map(half_degree);
    }
    schedules
}

/// Whether a committed list can replace `committed`: a status if not.
pub fn validate(new: &[Schedule], committed: &[Schedule], active: Option<&[u8]>, limits: &Limits) -> Result<(), u8> {
    use status::*;
    if new.len() > usize::from(NUMBER_OF_SCHEDULES) {
        return Err(RESOURCE_EXHAUSTED);
    }
    for (i, s) in new.iter().enumerate() {
        if let Some(h) = &s.handle {
            if !committed.iter().any(|c| c.handle.as_ref() == Some(h)) {
                return Err(NOT_FOUND);
            }
            if new[..i].iter().any(|o| o.handle.as_ref() == Some(h)) {
                return Err(CONSTRAINT_ERROR);
            }
        }
        if !limits.modes.contains(&s.mode) || s.name.as_ref().is_some_and(|n| n.len() > 64) {
            return Err(CONSTRAINT_ERROR);
        }
        if s.transitions.len() > usize::from(TRANSITIONS) {
            return Err(RESOURCE_EXHAUSTED);
        }
        let ok = |c: Option<i16>| c.is_some_and(|c| (limits.min..=limits.max).contains(&c));
        for t in &s.transitions {
            if t.days == 0 || t.days & AWAY != 0 || t.minutes >= MINUTES_PER_DAY {
                return Err(CONSTRAINT_ERROR);
            }
            let setpoints = match s.mode {
                Mode::Heat => ok(t.heat) && t.cool.is_none(),
                Mode::Cool => ok(t.cool) && t.heat.is_none(),
                Mode::Auto => ok(t.heat) && ok(t.cool) && t.cool.unwrap() - t.heat.unwrap() >= limits.dead_band,
            };
            if !setpoints {
                return Err(CONSTRAINT_ERROR);
            }
        }
        for day in 0..7 {
            let on: Vec<u16> = s.transitions.iter().filter(|t| t.days & day_bit(day) != 0).map(|t| t.minutes).collect();
            if on.len() > usize::from(PER_DAY) {
                return Err(RESOURCE_EXHAUSTED);
            }
            if on.iter().enumerate().any(|(i, m)| on[..i].contains(m)) {
                return Err(CONSTRAINT_ERROR);
            }
        }
    }
    if let Some(active) = active {
        if !new.iter().any(|s| s.handle.as_deref() == Some(active)) {
            return Err(INVALID_IN_STATE);
        }
    }
    Ok(())
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    active: Option<Vec<u8>>,
    /// The active one's transitions as of the last change; the bucket's are
    /// the ones that count.
    schedules: Vec<Schedule>,
    /// The bucket's days as each schedule left it when another became
    /// active, so its entries come back with their touched_ marks.
    #[serde(default)]
    days: Vec<(Vec<u8>, Value)>,
}

impl Saved {
    fn days(&self, handle: &[u8]) -> Option<Fields> {
        self.days.iter().find(|(h, _)| h == handle).map(|(_, d)| fields! {"days" => d.clone()})
    }
}

struct Atomic {
    owner: Owner,
    until: Instant,
    pending: Vec<Schedule>,
}

struct Inner {
    saved: Saved,
    atomic: Option<Atomic>,
}

impl Inner {
    /// The atomic write still open, if its time has not run out.
    fn atomic(&mut self) -> Option<&mut Atomic> {
        if self.atomic.as_ref().is_some_and(|a| a.until <= Instant::now()) {
            logf!("matter: schedules: atomic write timed out, rolled back");
            self.atomic = None;
        }
        self.atomic.as_mut()
    }

    fn owned(&mut self, owner: Owner) -> Result<&mut Atomic, u8> {
        match self.atomic() {
            Some(a) if a.owner == owner => Ok(a),
            Some(_) => Err(status::BUSY),
            None => Err(status::INVALID_IN_STATE),
        }
    }

    /// The schedules and the active handle, the active one as the bucket has it.
    fn list(&self, bucket: Option<&Fields>) -> (Vec<Schedule>, Option<Vec<u8>>) {
        let Some((mode, transitions)) = bucket.and_then(from_bucket) else {
            return (self.saved.schedules.clone(), self.saved.active.clone());
        };
        if self.saved.schedules.is_empty() || self.saved.active.is_none() {
            let name = bucket.and_then(|b| b.get("name")).and_then(Value::as_str).unwrap_or("Nest").to_string();
            let handle = b"s1".to_vec();
            let first = Schedule { handle: Some(handle.clone()), mode, name: Some(name), transitions };
            return (vec![first], Some(handle));
        }
        let mut schedules = self.saved.schedules.clone();
        if let Some(s) = schedules.iter_mut().find(|s| s.handle == self.saved.active) {
            s.mode = mode;
            s.transitions = transitions;
        }
        (schedules, self.saved.active.clone())
    }
}

/// The unit's schedules and the atomic write in progress.
pub struct Schedules {
    path: Option<PathBuf>,
    inner: Mutex<Inner>,
}

impl Schedules {
    /// From `path` when it is there; written back on each change.
    pub fn open(path: Option<PathBuf>) -> Self {
        let saved = path.as_deref().and_then(load).unwrap_or_default();
        Schedules { path, inner: Mutex::new(Inner { saved, atomic: None }) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn list(&self, bucket: Option<&Fields>) -> (Vec<Schedule>, Option<Vec<u8>>) {
        self.lock().list(bucket)
    }

    /// AtomicRequest BeginWrite: Schedules is the one attribute it takes.
    pub fn begin(
        &self,
        owner: Owner,
        attributes: &[u32],
        timeout: Option<u16>,
        bucket: Option<&Fields>,
    ) -> Result<Outcome, u8> {
        let Some(timeout) = timeout else { return Err(INVALID_COMMAND) };
        if attributes.is_empty() || attributes.iter().any(|a| *a != SCHEDULES) {
            return Err(INVALID_COMMAND);
        }
        let mut inner = self.lock();
        match inner.atomic() {
            // As CHIP: the same client again is a command error, another a busy attribute.
            Some(a) if a.owner == owner => return Err(status::INVALID_IN_STATE),
            Some(_) => return Ok(Outcome::schedules(status::BUSY)),
            None => {}
        }
        let timeout = timeout.min(MAX_TIMEOUT_MS);
        let (pending, _) = inner.list(bucket);
        inner.atomic =
            Some(Atomic { owner, until: Instant::now() + Duration::from_millis(u64::from(timeout)), pending });
        Ok(Outcome { timeout: Some(timeout), ..Outcome::schedules(status::SUCCESS) })
    }

    /// A write to Schedules, only inside the writer's own atomic write.
    pub fn write(&self, owner: Owner, write: Write) -> Result<(), u8> {
        let mut inner = self.lock();
        let atomic = inner.owned(owner)?;
        match write {
            Write::Replace(list) if list.len() > usize::from(NUMBER_OF_SCHEDULES) => Err(status::RESOURCE_EXHAUSTED),
            Write::Replace(list) => {
                atomic.pending = list;
                Ok(())
            }
            Write::Add(_) if atomic.pending.len() >= usize::from(NUMBER_OF_SCHEDULES) => {
                Err(status::RESOURCE_EXHAUSTED)
            }
            Write::Add(s) => {
                atomic.pending.push(s);
                Ok(())
            }
        }
    }

    /// AtomicRequest CommitWrite: the pending list checked and made the
    /// committed one, the active schedule written to the bucket if it changed.
    /// The atomic write ends either way.
    pub fn commit(
        &self,
        owner: Owner,
        bucket: Option<&Fields>,
        limits: &Limits,
        apply: impl FnOnce(Fields) -> Result<(), String>,
    ) -> Result<Outcome, u8> {
        let mut inner = self.lock();
        inner.owned(owner).map_err(|_| status::INVALID_IN_STATE)?;
        let pending = normalized(inner.atomic.take().map(|a| a.pending).unwrap_or_default());
        let (committed, active) = inner.list(bucket);
        let Some(active) = active else { return Ok(Outcome::schedules(status::INVALID_IN_STATE)) };
        if let Err(code) = validate(&pending, &committed, Some(&active), limits) {
            logf!("matter: schedules: commit refused, status {code:#04x}");
            return Ok(Outcome::schedules(code));
        }
        let mut schedules = pending;
        for i in 0..schedules.len() {
            if schedules[i].handle.is_none() {
                let free = (1..)
                    .map(|n| format!("s{n}").into_bytes())
                    .find(|h| !schedules.iter().any(|s| s.handle.as_ref() == Some(h)))
                    .unwrap();
                schedules[i].handle = Some(free);
            }
        }
        let running = schedules.iter().find(|s| s.handle.as_deref() == Some(&active)).unwrap();
        if bucket.and_then(from_bucket) != Some((running.mode, running.transitions.clone())) {
            let fields = to_bucket(running.mode, &running.transitions, bucket, chrono::Utc::now().timestamp());
            if let Err(e) = apply(fields) {
                logf!("matter: schedules: commit not applied: {e}");
                return Ok(Outcome::schedules(status::FAILURE));
            }
        }
        let mut days = std::mem::take(&mut inner.saved.days);
        days.retain(|(h, _)| schedules.iter().any(|s| s.handle.as_ref() == Some(h)));
        inner.saved = Saved { active: Some(active), schedules, days };
        self.save(&inner.saved);
        Ok(Outcome::schedules(status::SUCCESS))
    }

    /// AtomicRequest RollbackWrite.
    pub fn rollback(&self, owner: Owner) -> Result<Outcome, u8> {
        let mut inner = self.lock();
        inner.owned(owner).map_err(|_| status::INVALID_IN_STATE)?;
        inner.atomic = None;
        Ok(Outcome::schedules(status::SUCCESS))
    }

    /// SetActiveScheduleRequest: the schedule written to the bucket, the one
    /// it replaces kept as the bucket had it.
    pub fn activate(
        &self,
        handle: &[u8],
        bucket: Option<&Fields>,
        apply: impl FnOnce(Fields) -> Result<(), String>,
    ) -> Result<(), u8> {
        let mut inner = self.lock();
        let (schedules, active) = inner.list(bucket);
        let Some(target) = schedules.iter().find(|s| s.handle.as_deref() == Some(handle)) else {
            // As CHIP answers an unknown handle.
            return Err(INVALID_COMMAND);
        };
        if active.as_deref() == Some(handle) {
            return Ok(());
        }
        let kept = inner.saved.days(handle);
        let fields = to_bucket(target.mode, &target.transitions, kept.as_ref(), chrono::Utc::now().timestamp());
        apply(fields).map_err(|e| {
            logf!("matter: schedules: {} not activated: {e}", String::from_utf8_lossy(handle));
            status::FAILURE
        })?;
        let mut days = std::mem::take(&mut inner.saved.days);
        days.retain(|(h, _)| h != handle && Some(h) != active.as_ref());
        if let (Some(old), Some(d)) = (active, bucket.and_then(|b| b.get("days"))) {
            days.push((old, d.clone()));
        }
        inner.saved = Saved { active: Some(handle.to_vec()), schedules, days };
        self.save(&inner.saved);
        Ok(())
    }

    fn save(&self, saved: &Saved) {
        let Some(path) = &self.path else { return };
        let tmp = path.with_extension("json.tmp");
        let result = serde_json::to_vec(saved)
            .map_err(|e| e.to_string())
            .and_then(|b| std::fs::write(&tmp, b).map_err(|e| e.to_string()))
            .and_then(|()| std::fs::rename(&tmp, path).map_err(|e| e.to_string()));
        if let Err(e) = result {
            logf!("matter: schedules: {}: {e}", path.display());
        }
    }
}

fn load(path: &Path) -> Option<Saved> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(saved) => Some(saved),
        Err(e) => {
            logf!("matter: schedules: {}: {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MON: u8 = 0x02;
    const TUE: u8 = 0x04;
    const WED: u8 = 0x08;
    const SUN: u8 = 0x01;
    const EVERY_DAY: u8 = 0x7f;
    const A: Owner = (1, 100);
    const B: Owner = (2, 200);

    fn limits() -> Limits {
        Limits { modes: vec![Mode::Heat], min: 900, max: 3200, dead_band: 150 }
    }

    fn heat(days: u8, minutes: u16, centi: i16) -> Transition {
        Transition { days, minutes, heat: Some(centi), cool: None }
    }

    fn entry(time: i64, temp: f64) -> Value {
        json!({"entry_type": "setpoint", "temp": temp, "time": time, "touched_at": 1768340600,
               "touched_by": 5, "touched_tzo": 0, "type": "HEAT"})
    }

    /// The unit's: 18 °C from midnight, 19 from 9:00, 21 from 17:00 every
    /// day, and 20 from 10:30 on Wednesdays (Nest day 2).
    fn bucket() -> Fields {
        let day = json!({"0": entry(0, 18.0), "1": entry(32400, 19.0), "2": entry(61200, 21.0)});
        let mut days = serde_json::Map::new();
        for d in 0..7 {
            days.insert(d.to_string(), day.clone());
        }
        days.insert(
            "2".into(),
            json!({"0": entry(0, 18.0), "1": entry(32400, 19.0), "2": entry(37800, 20.0),
                                        "3": entry(61200, 21.0)}),
        );
        fields! {"days" => Value::Object(days), "name" => "Current Schedule", "schedule_mode" => "HEAT", "ver" => 2}
    }

    fn schedules() -> Schedules {
        Schedules::open(None)
    }

    fn sched(handle: Option<&[u8]>, transitions: Vec<Transition>) -> Schedule {
        Schedule { handle: handle.map(<[u8]>::to_vec), mode: Mode::Heat, name: None, transitions }
    }

    /// Applies a commit's bucket write to `bucket`.
    fn commit(s: &Schedules, owner: Owner, bucket: &mut Fields) -> Result<Outcome, u8> {
        let current = bucket.clone();
        s.commit(owner, Some(&current), &limits(), |f| {
            bucket.extend(f);
            Ok(())
        })
    }

    #[test]
    fn nest_days_start_on_monday() {
        assert_eq!((day_bit(0), day_bit(5), day_bit(6)), (MON, 0x40, SUN));
    }

    #[test]
    fn the_bucket_reads_as_one_schedule_a_transition_per_time_and_setpoint() {
        let (mode, transitions) = from_bucket(&bucket()).unwrap();
        assert_eq!(mode, Mode::Heat);
        assert_eq!(
            transitions,
            vec![
                heat(EVERY_DAY, 0, 1800),
                heat(EVERY_DAY, 540, 1900),
                heat(WED, 630, 2000),
                heat(EVERY_DAY, 1020, 2100)
            ]
        );
        let (list, active) = schedules().list(Some(&bucket()));
        assert_eq!(active.as_deref(), Some(&b"s1"[..]));
        assert_eq!((list.len(), list[0].name.as_deref()), (1, Some("Current Schedule")));
    }

    #[test]
    fn writing_back_what_was_read_keeps_every_entry_as_it_was() {
        let b = bucket();
        let (mode, transitions) = from_bucket(&b).unwrap();
        let back = to_bucket(mode, &transitions, Some(&b), 42);
        assert_eq!(back["days"], b["days"]);
        assert_eq!(back["schedule_mode"], json!("HEAT"));
    }

    #[test]
    fn new_transitions_become_entries_by_day_in_time_order() {
        let b = bucket();
        let back = to_bucket(Mode::Heat, &[heat(TUE | SUN, 1200, 2050), heat(TUE, 360, 1900)], Some(&b), 42);
        // Each day opens with a continuation: no day here starts at midnight.
        let tue = &back["days"]["1"];
        assert_eq!(
            (tue["0"]["entry_type"].clone(), tue["0"]["time"].clone(), tue["0"]["temp"].clone()),
            (json!("continuation"), json!(0), json!(20.5))
        );
        assert_eq!((tue["1"]["time"].clone(), tue["1"]["temp"].clone()), (json!(21600), json!(19)));
        assert_eq!(
            (tue["2"]["time"].clone(), tue["2"]["temp"].clone(), tue["2"]["touched_by"].clone()),
            (json!(72000), json!(20.5), json!(4))
        );
        assert_eq!(back["days"]["6"]["1"]["time"], json!(72000));
        let mon = back["days"]["0"].as_object().unwrap();
        assert_eq!((mon.len(), mon["0"]["entry_type"].clone()), (1, json!("continuation")), "an empty day");
    }

    #[test]
    fn a_range_schedule_keeps_its_low_and_high() {
        let t = Transition { days: MON, minutes: 60, heat: Some(1900), cool: Some(2400) };
        let back = to_bucket(Mode::Auto, std::slice::from_ref(&t), None, 0);
        assert_eq!(back["schedule_mode"], json!("RANGE"));
        assert_eq!(
            (back["days"]["0"]["0"]["temp-min"].clone(), back["days"]["0"]["0"]["temp-max"].clone()),
            (json!(19), json!(24))
        );
        assert_eq!(from_bucket(&back).unwrap(), (Mode::Auto, vec![t]));
    }

    #[test]
    fn an_atomic_write_commits_to_the_bucket() {
        let s = schedules();
        let mut b = bucket();
        let begun = s.begin(A, &[SCHEDULES], Some(60_000), Some(&b)).unwrap();
        assert_eq!(begun, Outcome { status: 0, attributes: vec![(SCHEDULES, 0)], timeout: Some(MAX_TIMEOUT_MS) });
        let new = vec![sched(Some(b"s1"), vec![heat(EVERY_DAY, 420, 2026)]), sched(None, vec![heat(MON, 0, 1500)])];
        s.write(A, Write::Replace(new)).unwrap();
        // Until the commit, the committed list is what reads see.
        assert_eq!(s.list(Some(&b)).0.len(), 1);
        assert_eq!(commit(&s, A, &mut b).unwrap(), Outcome::schedules(status::SUCCESS));
        assert_eq!(from_bucket(&b).unwrap().1, vec![heat(EVERY_DAY, 420, 2050)], "to half degrees");
        let (list, active) = s.list(Some(&b));
        assert_eq!(
            list.iter().map(|s| s.handle.clone().unwrap()).collect::<Vec<_>>(),
            [b"s1".to_vec(), b"s2".to_vec()]
        );
        assert_eq!(active.as_deref(), Some(&b"s1"[..]));
        // Ended: another commit has nothing to commit.
        assert_eq!(commit(&s, A, &mut b), Err(status::INVALID_IN_STATE));
    }

    #[test]
    fn a_chunked_write_is_an_empty_list_then_each_item() {
        let s = schedules();
        let mut b = bucket();
        s.begin(A, &[SCHEDULES], Some(5000), Some(&b)).unwrap();
        s.write(A, Write::Replace(vec![])).unwrap();
        s.write(A, Write::Add(sched(Some(b"s1"), vec![heat(EVERY_DAY, 0, 1700)]))).unwrap();
        s.write(A, Write::Add(sched(None, vec![]))).unwrap();
        assert_eq!(commit(&s, A, &mut b).unwrap().status, status::SUCCESS);
        assert_eq!(s.list(Some(&b)).0.len(), 2);
        assert_eq!(from_bucket(&b).unwrap().1, vec![heat(EVERY_DAY, 0, 1700)]);
    }

    #[test]
    fn rollback_and_timeout_leave_everything_as_it_was() {
        let s = schedules();
        let mut b = bucket();
        let before = b.clone();
        s.begin(A, &[SCHEDULES], Some(5000), Some(&b)).unwrap();
        s.write(A, Write::Replace(vec![sched(Some(b"s1"), vec![])])).unwrap();
        assert_eq!(s.rollback(A).unwrap(), Outcome::schedules(status::SUCCESS));
        assert_eq!(s.rollback(A), Err(status::INVALID_IN_STATE));
        assert_eq!(commit(&s, A, &mut b), Err(status::INVALID_IN_STATE));
        assert_eq!(b, before);

        s.begin(A, &[SCHEDULES], Some(0), Some(&b)).unwrap();
        assert_eq!(s.write(A, Write::Replace(vec![])), Err(status::INVALID_IN_STATE), "timed out");
        assert_eq!(b, before);
    }

    #[test]
    fn only_the_writer_writes_and_one_write_is_open_at_a_time() {
        let s = schedules();
        let b = bucket();
        assert_eq!(s.write(A, Write::Replace(vec![])), Err(status::INVALID_IN_STATE), "no atomic write");
        s.begin(A, &[SCHEDULES], Some(5000), Some(&b)).unwrap();
        assert_eq!(s.begin(B, &[SCHEDULES], Some(5000), Some(&b)).unwrap(), Outcome::schedules(status::BUSY));
        assert_eq!(s.write(B, Write::Replace(vec![])), Err(status::BUSY));
        assert_eq!(s.rollback(B), Err(status::INVALID_IN_STATE));
        assert_eq!(s.begin(A, &[SCHEDULES], Some(5000), Some(&b)), Err(status::INVALID_IN_STATE), "already open");
        // Only Schedules, and a timeout to begin with.
        assert_eq!(s.begin(B, &[0x0050], Some(5000), Some(&b)), Err(INVALID_COMMAND));
        assert_eq!(s.begin(B, &[SCHEDULES], None, Some(&b)), Err(INVALID_COMMAND));
    }

    #[test]
    fn commits_the_unit_cannot_run_are_refused_and_end_the_write() {
        let cases: Vec<(Vec<Schedule>, u8)> = vec![
            (vec![], status::INVALID_IN_STATE), // the active one removed
            (vec![sched(Some(b"s9"), vec![])], status::NOT_FOUND),
            (vec![Schedule { mode: Mode::Cool, ..sched(Some(b"s1"), vec![]) }], status::CONSTRAINT_ERROR),
            (vec![sched(Some(b"s1"), vec![heat(MON, 1440, 2000)])], status::CONSTRAINT_ERROR),
            (vec![sched(Some(b"s1"), vec![heat(0, 60, 2000)])], status::CONSTRAINT_ERROR),
            (vec![sched(Some(b"s1"), vec![heat(AWAY, 60, 2000)])], status::CONSTRAINT_ERROR),
            (vec![sched(Some(b"s1"), vec![heat(MON, 60, 3300)])], status::CONSTRAINT_ERROR),
            (
                vec![sched(Some(b"s1"), vec![Transition { days: MON, minutes: 60, heat: None, cool: Some(2400) }])],
                status::CONSTRAINT_ERROR,
            ),
            (vec![sched(Some(b"s1"), vec![heat(MON, 60, 2000), heat(MON | TUE, 60, 2100)])], status::CONSTRAINT_ERROR),
            (vec![sched(Some(b"s1"), (0..9).map(|h| heat(MON, h * 60, 2000)).collect())], status::RESOURCE_EXHAUSTED),
            (vec![sched(Some(b"s1"), vec![]), sched(Some(b"s1"), vec![])], status::CONSTRAINT_ERROR),
        ];
        for (new, want) in cases {
            let s = schedules();
            let mut b = bucket();
            let before = b.clone();
            s.begin(A, &[SCHEDULES], Some(5000), Some(&b)).unwrap();
            s.write(A, Write::Replace(new.clone())).unwrap();
            let got = commit(&s, A, &mut b).unwrap();
            assert_eq!(got, Outcome::schedules(want), "{new:?}");
            assert_eq!(b, before, "{new:?}");
            assert_eq!(s.rollback(A), Err(status::INVALID_IN_STATE), "{new:?}: still open");
        }
        let s = schedules();
        s.begin(A, &[SCHEDULES], Some(5000), Some(&bucket())).unwrap();
        let five = vec![sched(None, vec![]); 5];
        assert_eq!(s.write(A, Write::Replace(five)), Err(status::RESOURCE_EXHAUSTED));
    }

    #[test]
    fn activating_a_schedule_swaps_it_into_the_bucket_and_keeps_the_old_one() {
        let s = schedules();
        let mut b = bucket();
        let original = from_bucket(&b).unwrap().1;
        s.begin(A, &[SCHEDULES], Some(5000), Some(&b)).unwrap();
        s.write(
            A,
            Write::Replace(vec![sched(Some(b"s1"), original.clone()), sched(None, vec![heat(EVERY_DAY, 0, 1600)])]),
        )
        .unwrap();
        commit(&s, A, &mut b).unwrap();
        assert_eq!(from_bucket(&b).unwrap().1, original, "the active one unchanged");

        // The unit's own edit, before the switch: it goes with the old one.
        let mut edited = b.clone();
        edited["days"]["0"]["0"]["temp"] = json!(17.5);
        let current = edited.clone();
        assert_eq!(s.activate(b"s9", Some(&current), |_| Ok(())), Err(INVALID_COMMAND));
        s.activate(b"s2", Some(&current), |f| {
            edited.extend(f);
            Ok(())
        })
        .unwrap();
        assert_eq!(from_bucket(&edited).unwrap().1, vec![heat(EVERY_DAY, 0, 1600)]);
        let (list, active) = s.list(Some(&edited));
        assert_eq!(active.as_deref(), Some(&b"s2"[..]));
        assert_eq!(list[0].transitions[0], heat(MON, 0, 1750));

        let current = edited.clone();
        s.activate(b"s1", Some(&current), |f| {
            edited.extend(f);
            Ok(())
        })
        .unwrap();
        assert_eq!(edited["days"]["0"]["0"]["temp"], json!(17.5));
        assert_eq!(edited["days"]["1"], b["days"]["1"], "the untouched days as they were");
    }

    #[test]
    fn schedules_persist() {
        let path = std::env::temp_dir().join(format!("nestlocal-schedules-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut b = bucket();
        let s = Schedules::open(Some(path.clone()));
        s.begin(A, &[SCHEDULES], Some(5000), Some(&b)).unwrap();
        let first = s.list(Some(&b)).0.remove(0);
        s.write(A, Write::Replace(vec![first, sched(None, vec![heat(MON, 0, 1500)])])).unwrap();
        commit(&s, A, &mut b).unwrap();
        let again = Schedules::open(Some(path.clone()));
        assert_eq!(again.list(Some(&b)), s.list(Some(&b)));
        assert_eq!(again.list(Some(&b)).0.len(), 2);
        let _ = std::fs::remove_file(&path);
    }
}
