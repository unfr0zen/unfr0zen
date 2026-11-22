//! The Thermostat cluster's Schedules (MSCH), as the thermostat and the
//! water heater both serve them. A
//! controller changes the list in an atomic write (AtomicRequest begin, a write of
//! Schedules, commit). The cluster handler says what its schedule is now
//! and how a committed one is applied; this keeps the write in between.
//!
//! An atomic write belongs to one controller (its fabric and node): another's
//! begin gets BUSY and its writes INVALID_IN_STATE, and while it is open the
//! writer reads its pending Schedules, the others the unit's. It times out
//! on the clock that counts the time the unit sleeps (timer.rs).

use std::sync::{Mutex, MutexGuard};

use embassy_time::{Duration, Instant};
use rs_matter::dm::clusters::decl::globals::AtomicRequestTypeEnum;
use rs_matter::dm::clusters::decl::thermostat::{
    self, ScheduleDayOfWeekBitmap, ScheduleStructArrayBuilder, ScheduleStructBuilder, SystemModeEnum,
};
use rs_matter::dm::{ArrayAttributeRead, ArrayAttributeWrite, InvokeContext, OperationContext};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::im::IMStatusCode;
use rs_matter::tlv::{Nullable, Octets, TLVArray, TLVBuilderParent};

use crate::logf;

/// Schedules' attribute ID, the one an atomic write may hold.
pub const SCHEDULES: u32 = 0x0051;
/// The longest an atomic write stays open, ms.
const ATOMIC_TIMEOUT: u16 = 30_000;

/// A transition: its day bits (bit 0 Sunday), minutes after midnight,
/// heating and cooling setpoints, the system mode it sets, and whether it
/// sets a preset.
#[derive(Clone, Debug, PartialEq)]
pub struct Transition {
    pub days: u8,
    pub minutes: u16,
    pub heat: Option<i16>,
    pub cool: Option<i16>,
    pub mode: Option<SystemModeEnum>,
    pub preset: bool,
}

/// A schedule as Schedules has it, or as written until a commit checks it.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub name: Option<String>,
    pub handle: Option<Vec<u8>>,
    pub mode: SystemModeEnum,
    pub built_in: Option<bool>,
    pub preset: bool,
    pub transitions: Vec<Transition>,
}

/// Who an atomic write belongs to: the fabric index, and the peer's node
/// and CATs as rs-matter's accessor shows them.
pub type Writer = (u8, String);

/// An open atomic write of Schedules: whose, until when, the schedules
/// written so far, and whether anything has been written.
pub struct Atomic {
    pub writer: Writer,
    pub until: Instant,
    initial: Vec<Pending>,
    pub pending: Vec<Pending>,
    pub written: bool,
}

/// One cluster's Schedules writes.
#[derive(Default)]
pub struct Schedules {
    open: Mutex<Option<Atomic>>,
}

impl Schedules {
    /// The open atomic write, after dropping one that timed out.
    pub fn atomic(&self) -> MutexGuard<'_, Option<Atomic>> {
        let mut atomic = self.open.lock().unwrap();
        if atomic.as_ref().is_some_and(|a| a.until <= Instant::now()) {
            *atomic = None;
        }
        atomic
    }

    /// Schedules as `writer` reads it: its pending copy while its atomic
    /// write is open, else `current`.
    pub fn shown(&self, writer: &Writer, current: impl FnOnce() -> Vec<Pending>) -> Vec<Pending> {
        match self.atomic().as_ref() {
            Some(open) if open.writer == *writer => open.pending.clone(),
            _ => current(),
        }
    }

    /// Opens an atomic write for `writer`, holding a copy of Schedules as it
    /// is (`current`): BUSY while another's is open, INVALID_IN_STATE while
    /// its own is, which stays as it was.
    pub fn begin(&self, writer: &Writer, timeout: Option<u16>, current: Vec<Pending>) -> Result<u16, ErrorCode> {
        let timeout = timeout.ok_or(ErrorCode::InvalidCommand)?.min(ATOMIC_TIMEOUT);
        let mut atomic = self.atomic();
        match atomic.as_ref() {
            Some(open) if open.writer != *writer => return Err(ErrorCode::Busy),
            Some(_) => return Err(ErrorCode::InvalidInState),
            None => {}
        }
        let until = Instant::now() + Duration::from_millis(u64::from(timeout));
        *atomic =
            Some(Atomic { writer: writer.clone(), until, initial: current.clone(), pending: current, written: false });
        Ok(timeout)
    }

    /// A write of Schedules, into `writer`'s pending copy: only inside its
    /// own atomic write.
    pub fn write(
        &self,
        writer: &Writer,
        capacity: usize,
        value: ArrayAttributeWrite<TLVArray<'_, thermostat::ScheduleStruct<'_>>, thermostat::ScheduleStruct<'_>>,
    ) -> Result<(), Error> {
        let mut atomic = self.atomic();
        let Some(open) = atomic.as_mut().filter(|a| a.writer == *writer) else {
            return Err(ErrorCode::InvalidInState.into());
        };
        // Each endpoint advertises its own capacity.
        match value {
            ArrayAttributeWrite::Replace(list) => {
                let list = list.iter().map(|s| pending(&s?)).collect::<Result<Vec<_>, Error>>()?;
                if list.len() > capacity {
                    return Err(ErrorCode::ResourceExhausted.into());
                }
                open.pending = list;
            }
            ArrayAttributeWrite::Add(s) => {
                let s = pending(&s)?;
                if open.pending.len() >= capacity {
                    return Err(ErrorCode::ResourceExhausted.into());
                }
                open.pending.push(s);
            }
            ArrayAttributeWrite::Update(i, s) => {
                *open.pending.get_mut(usize::from(i)).ok_or(ErrorCode::ConstraintError)? = pending(&s)?;
            }
            ArrayAttributeWrite::Remove(i) => {
                if usize::from(i) >= open.pending.len() {
                    return Err(ErrorCode::ConstraintError.into());
                }
                open.pending.remove(usize::from(i));
            }
        }
        open.written = true;
        Ok(())
    }

    /// Ends `writer`'s atomic write: a commit applies what it wrote through
    /// `apply` (nothing written, nothing changes), a rollback drops it.
    pub fn finish(
        &self,
        writer: &Writer,
        commit: bool,
        apply: impl FnOnce(&[Pending], &[Pending]) -> Result<(), ErrorCode>,
    ) -> Result<IMStatusCode, ErrorCode> {
        let mut atomic = self.atomic();
        let open = atomic.take_if(|a| a.writer == *writer).ok_or(ErrorCode::InvalidInState)?;
        if !commit || !open.written {
            return Ok(IMStatusCode::Success);
        }
        Ok(match apply(&open.initial, &open.pending) {
            Ok(()) => IMStatusCode::Success,
            Err(e) => {
                logf!("matter: Schedules commit refused, {e:?}: {:?}", open.pending);
                IMStatusCode::from(e)
            }
        })
    }

    /// AtomicRequest for Schedules: a begin copies `current`, a commit goes
    /// through `apply`. After a rollback or a refused commit `refused` runs:
    /// subscribers, which may hold what was written, are to get Schedules as
    /// the unit has it.
    pub fn atomic_request<P: TLVBuilderParent>(
        &self,
        ctx: &impl InvokeContext,
        request: &thermostat::AtomicRequestRequest<'_>,
        response: thermostat::AtomicResponseBuilder<P>,
        current: impl FnOnce() -> Vec<Pending>,
        apply: impl FnOnce(&[Pending], &[Pending]) -> Result<(), ErrorCode>,
        refused: impl FnOnce(),
    ) -> Result<P, Error> {
        let writer = writer(ctx, ctx.cmd().fab_idx);
        let attributes = request.attribute_requests()?.iter().collect::<Result<Vec<u32>, _>>()?;
        if attributes.is_empty() || attributes.iter().enumerate().any(|(i, a)| attributes[..i].contains(a)) {
            return Err(ErrorCode::InvalidCommand.into());
        }
        // Schedules alone takes an atomic write.
        if attributes.iter().any(|&a| a != SCHEDULES) {
            let status = |a| if a == SCHEDULES { IMStatusCode::Success } else { IMStatusCode::UnsupportedAttribute };
            return answer(response, IMStatusCode::Failure, &attributes, status, None);
        }
        let status = match request.request_type()? {
            AtomicRequestTypeEnum::BeginWrite => match self.begin(&writer, request.timeout()?, current()) {
                Ok(timeout) => {
                    return answer(
                        response,
                        IMStatusCode::Success,
                        &attributes,
                        |_| IMStatusCode::Success,
                        Some(timeout),
                    )
                }
                Err(ErrorCode::InvalidCommand) => return Err(ErrorCode::InvalidCommand.into()),
                Err(e) => IMStatusCode::from(e),
            },
            kind => {
                let status = self.finish(&writer, kind == AtomicRequestTypeEnum::CommitWrite, apply)?;
                if kind == AtomicRequestTypeEnum::RollbackWrite || status != IMStatusCode::Success {
                    refused();
                }
                status
            }
        };
        let overall = if status == IMStatusCode::Success { IMStatusCode::Success } else { IMStatusCode::Failure };
        answer(response, overall, &attributes, |_| status, None)
    }
}

/// Schedules for a read: `list`, whole or one of it.
pub fn read<P: TLVBuilderParent>(
    list: &[Pending],
    builder: ArrayAttributeRead<ScheduleStructArrayBuilder<P>, ScheduleStructBuilder<P>>,
) -> Result<P, Error> {
    fn one<P: TLVBuilderParent>(b: ScheduleStructBuilder<P>, s: &Pending) -> Result<P, Error> {
        let mut transitions = b
            .schedule_handle(Nullable::new(s.handle.as_deref().map(Octets)))?
            .system_mode(s.mode)?
            .name(s.name.as_deref())?
            .preset_handle(None)?
            .transitions()?;
        for t in &s.transitions {
            transitions = transitions
                .push()?
                .day_of_week(ScheduleDayOfWeekBitmap::from_bits_truncate(t.days))?
                .transition_time(t.minutes)?
                .preset_handle(None)?
                .system_mode(t.mode)?
                .cooling_setpoint(t.cool)?
                .heating_setpoint(t.heat)?
                .end()?;
        }
        transitions.end()?.built_in(Nullable::new(s.built_in))?.end()
    }
    match builder {
        ArrayAttributeRead::ReadAll(mut array) => {
            for s in list {
                array = one(array.push()?, s)?;
            }
            array.end()
        }
        ArrayAttributeRead::ReadOne(i, item) => match list.get(usize::from(i)) {
            Some(s) => one(item, s),
            None => Err(ErrorCode::ConstraintError.into()),
        },
        ArrayAttributeRead::ReadNone(array) => array.end(),
    }
}

/// Who an operation comes from, for an atomic write: its fabric index and
/// the peer's node and CATs (none without a session).
pub fn writer(ctx: &impl OperationContext, fabric: u8) -> Writer {
    (fabric, ctx.accessor().map(|a| a.subjects().to_string()).unwrap_or_default())
}

/// schedule.rs's day (0 Monday) as a ScheduleDayOfWeekBitmap bit.
pub fn day_bit(day: usize) -> u8 {
    1 << ((day + 1) % 7)
}

/// A written ScheduleStruct, as an atomic write holds it.
pub fn pending(s: &thermostat::ScheduleStruct<'_>) -> Result<Pending, Error> {
    let name = s.name()?.map(str::to_owned);
    if name.as_ref().is_some_and(|n| n.len() > 64) {
        return Err(ErrorCode::ConstraintError.into());
    }
    let transitions = s
        .transitions()?
        .iter()
        .map(|t| {
            let t = t?;
            Ok(Transition {
                days: t.day_of_week()?.bits(),
                minutes: t.transition_time()?,
                heat: t.heating_setpoint()?,
                cool: t.cooling_setpoint()?,
                mode: t.system_mode()?,
                preset: t.preset_handle()?.is_some(),
            })
        })
        .collect::<Result<_, Error>>()?;
    Ok(Pending {
        name,
        handle: s.schedule_handle()?.into_option().map(|h| h.0.to_vec()),
        mode: s.system_mode()?,
        built_in: s.built_in()?.into_option(),
        preset: s.preset_handle()?.is_some(),
        transitions,
    })
}

/// An AtomicResponse: the overall status, and one for each attribute.
fn answer<P: TLVBuilderParent>(
    response: thermostat::AtomicResponseBuilder<P>,
    overall: IMStatusCode,
    attributes: &[u32],
    status: impl Fn(u32) -> IMStatusCode,
    timeout: Option<u16>,
) -> Result<P, Error> {
    let mut each = response.status_code(overall as u8)?.attribute_status()?;
    for &a in attributes {
        each = each.push()?.attribute_id(a)?.status_code(status(a) as u8)?.end()?;
    }
    each.end()?.timeout(timeout)?.end()
}
