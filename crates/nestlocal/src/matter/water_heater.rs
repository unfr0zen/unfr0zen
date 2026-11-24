//! The unit's hot water as a Matter Water Heater (endpoint WATER_HEATER, on
//! a unit with hot water control), as Home Assistant's water heater entity
//! reads it:
//!
//! - Thermostat (heating, MSCH): SystemMode Heat while hot water follows
//!   its schedule, Off while it is off (MQTT's hot_water_schedule). The
//!   schedule is Schedules' one built-in schedule, handle "hotwater": each
//!   on period of hotwater.rs is a transition to Heat at its start and one
//!   to Off at its end (on the next day for a period past midnight), with
//!   no setpoints. The hot water temperature and its setpoint read what the
//!   unit has, on a unit that reports them; neither is written.
//! - Water Heater Management: the boiler heats it (HeaterTypes); HeatDemand
//!   while hot water is on; Boost for a duration up to MQTT's longest
//!   (hot_water_boost), BoostState while one runs, CancelBoost; BoostStarted
//!   and BoostEnded as one starts and ends (report_boost).
//! - Water Heater Mode: Off, or Timed (following the schedule).

use std::sync::Arc;

use rs_matter::dm::clusters::decl::globals::{ModeOptionStructArrayBuilder, ModeOptionStructBuilder};
use rs_matter::dm::clusters::decl::thermostat::{
    self, ControlSequenceOfOperationEnum, ScheduleStructArrayBuilder, ScheduleStructBuilder,
    ScheduleTypeFeaturesBitmap, ScheduleTypeStructArrayBuilder, ScheduleTypeStructBuilder, SystemModeEnum,
};
use rs_matter::dm::clusters::decl::water_heater_management::{
    self as management, BoostStateEnum, WaterHeaterHeatSourceBitmap,
};
use rs_matter::dm::clusters::decl::water_heater_mode::{self as mode, ModeTag};
use rs_matter::dm::DeviceType;
use rs_matter::dm::{
    ArrayAttributeRead, ArrayAttributeWrite, Cluster, Dataver, EventEmitter, InvokeContext, ReadContext, WriteContext,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{Nullable, NullableBuilder, Octets, OctetsBuilder, TLVArray, TLVBuilderParent};
use rs_matter::with;

use super::schedules::{self as msch, day_bit, Pending, Schedules, Transition, SCHEDULES};
use super::{failed, Nest};
use crate::hotwater::{self, Period};

/// The endpoint the water heater is on.
pub const WATER_HEATER: u16 = 8;
pub const DEV_TYPE_WATER_HEATER: DeviceType = DeviceType { dtype: 0x050F, drev: 1 };

/// The schedule's handle.
const SCHEDULE: &[u8] = b"hotwater";
/// The hot water setpoint's limits, as the Thermostat cluster advertises them.
const MIN: i16 = 3000;
const MAX: i16 = 7000;
/// The setpoint shown on a unit that reports none.
const SETPOINT: i16 = 6000;
const DAY: u32 = 24 * 60;
/// A day's own periods' transitions, and the off of one from the day before.
const TRANSITIONS_PER_DAY: usize = hotwater::DAILY * 2 + 1;
const WEEK: u32 = 7 * DAY;
/// Water Heater Mode's modes: off, or following the schedule.
const MODE_OFF: u8 = 0;
const MODE_TIMED: u8 = 1;

fn following(nest: &Nest) -> bool {
    nest.view().device_str("hot_water_mode") != "off"
}

fn set(nest: &Nest, name: &str, arg: &str) -> Result<(), Error> {
    nest.apply(name, arg).map_err(failed)
}

/// The water heater's Thermostat cluster.
pub struct HotWater {
    dataver: Dataver,
    nest: Arc<Nest>,
    schedules: Schedules,
}

impl HotWater {
    pub fn new(dataver: Dataver, nest: Arc<Nest>) -> Self {
        Self { dataver, nest, schedules: Schedules::default() }
    }

    /// Schedules as the unit has it: the hot water schedule, an on and an
    /// off transition for each period; none before nlclient has synced it.
    fn current(&self) -> Vec<Pending> {
        let Some(bucket) = self.nest.hot_water() else {
            return Vec::new();
        };
        Self::from_bucket(&bucket)
    }

    fn from_bucket(bucket: &crate::json::Fields) -> Vec<Pending> {
        let transition = |day: usize, minutes: u16, mode| Transition {
            days: day_bit(day),
            minutes,
            heat: None,
            cool: None,
            mode: Some(mode),
            preset: false,
        };
        let periods = (0..7).flat_map(|d| {
            hotwater::day(bucket, d).into_iter().map(move |(on, off)| {
                let start = d as u32 * DAY + u32::from(on);
                (start, start + (u32::from(off) + DAY - u32::from(on) - 1) % DAY + 1)
            })
        });
        let transitions = coalesce(periods.collect())
            .into_iter()
            .flat_map(|(start, end)| {
                let at = |m: u32| ((m / DAY % 7) as usize, (m % DAY) as u16);
                let ((on_day, on), (off_day, off)) = (at(start), at(end));
                [transition(on_day, on, SystemModeEnum::Heat), transition(off_day, off, SystemModeEnum::Off)]
            })
            .collect::<Vec<_>>();
        // No periods is no schedule: a schedule of no transitions makes Home
        // Assistant's Matter server drop the node's endpoints.
        if transitions.is_empty() {
            return Vec::new();
        }
        vec![Pending {
            name: None,
            handle: Some(SCHEDULE.to_vec()),
            mode: SystemModeEnum::Heat,
            built_in: Some(true),
            preset: false,
            transitions,
        }]
    }

    /// Applies an atomic write's schedules: the one, as the whole week's on
    /// periods, each a Heat transition and the Off one after it.
    #[cfg(test)]
    fn commit(&self, pending: &[Pending]) -> Result<(), ErrorCode> {
        self.commit_checked(&self.current(), pending)
    }

    fn commit_checked(&self, initial: &[Pending], pending: &[Pending]) -> Result<(), ErrorCode> {
        // No schedule is a week without periods.
        let none = Pending {
            name: None,
            handle: None,
            mode: SystemModeEnum::Heat,
            built_in: None,
            preset: false,
            transitions: Vec::new(),
        };
        let one = match pending {
            [] => &none,
            [one] => one,
            _ => return Err(ErrorCode::ResourceExhausted),
        };
        let handle_ok = one.handle.as_deref().is_none_or(|h| h == SCHEDULE);
        if one.name.is_some()
            || !handle_ok
            || one.mode != SystemModeEnum::Heat
            || one.built_in == Some(false)
            || one.preset
        {
            return Err(ErrorCode::ConstraintError);
        }
        // Each transition on each of its days, as minutes into the week from
        // Monday, Heat or not.
        let mut marks = Vec::new();
        for t in &one.transitions {
            // On: Heat, given or as the schedule's, with or without a
            // setpoint (the unit heats to its own); off: Off, without one.
            let heat = match (t.mode, t.heat) {
                (Some(SystemModeEnum::Heat) | None, _) => true,
                (Some(SystemModeEnum::Off), None) => false,
                _ => return Err(ErrorCode::ConstraintError),
            };
            let plain = t.cool.is_none() && !t.preset;
            if !plain || t.days == 0 || t.days & 0x80 != 0 || u32::from(t.minutes) >= DAY {
                return Err(ErrorCode::ConstraintError);
            }
            for bit in (0..7).filter(|b| t.days & (1 << b) != 0) {
                marks.push(((bit + 6) % 7 * DAY + u32::from(t.minutes), heat));
            }
        }
        marks.sort();
        if (0..7).any(|d| marks.iter().filter(|(m, _)| m / DAY == d).count() > TRANSITIONS_PER_DAY) {
            return Err(ErrorCode::ResourceExhausted);
        }
        let periods = pair(&marks).ok_or(ErrorCode::ConstraintError)?;
        let mut week: Vec<Vec<Period>> = vec![Vec::new(); 7];
        for (start, end) in periods {
            week[(start / DAY) as usize].push(((start % DAY) as u16, (end % DAY) as u16));
        }
        if week.iter().any(|d| d.len() > hotwater::DAILY) {
            return Err(ErrorCode::ResourceExhausted);
        }
        let named: Vec<_> = week.into_iter().enumerate().collect();
        // MQTT and Zigbee read and replace this same bucket under the command lock.
        let _applying = self.nest.bridge.applying.lock().unwrap();
        let original = self.nest.bridge.hot_water_schedule().ok_or(ErrorCode::InvalidInState)?;
        if Self::from_bucket(&original.value) != initial {
            return Err(ErrorCode::InvalidInState);
        }
        let fields = hotwater::write(&original.value, &hotwater::command(&named), chrono::Utc::now().timestamp())
            .map_err(|_| ErrorCode::ConstraintError)?;
        self.nest
            .bridge
            .store
            .update(&original.key, 0, 0, false, |objects, o| {
                let dev = objects.get(&format!("device.{}", self.nest.bridge.serial));
                if !crate::json::truthy(dev.and_then(|d| d.value.get("has_hot_water_control"))) {
                    return Err("no hot water control".into());
                }
                if o.revision != original.revision {
                    return Err("hot water schedule changed".into());
                }
                o.value.extend(fields);
                Ok(())
            })
            .map_err(|_| ErrorCode::InvalidInState)?;
        Ok(())
    }
}

/// Periods (start, end minutes into the week from Monday, sorted by start)
/// with each that ends as the next starts joined to it, while the two last
/// under a day: Schedules can't carry an off and an on at the same minute.
fn coalesce(mut periods: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    periods.sort();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (start, end) in periods {
        match out.last_mut() {
            Some(last) if last.1 == start && end - last.0 < DAY => last.1 = end,
            _ => out.push((start, end)),
        }
    }
    // Sunday night's period running into Monday's first, round the week.
    if out.len() > 1 {
        let (first, last) = (out[0], out[out.len() - 1]);
        if last.1 == first.0 + WEEK && first.1 + WEEK - last.0 < DAY {
            out.remove(0);
            *out.last_mut().unwrap() = (last.0, first.1 + WEEK);
        }
    }
    out
}

/// The on periods of a week's marks (minutes into the week, sorted, and
/// whether each turns hot water on): each on paired with the off after it,
/// round the week's end, as (start, end) minutes into the week. None unless
/// they alternate and each period lasts under a day.
fn pair(marks: &[(u32, bool)]) -> Option<Vec<(u32, u32)>> {
    if marks.is_empty() {
        return Some(Vec::new());
    }
    let first = marks.iter().position(|(_, heat)| *heat)?;
    let turned: Vec<_> = marks[first..].iter().chain(&marks[..first]).collect();
    if turned.len() % 2 != 0 {
        return None;
    }
    turned
        .chunks(2)
        .map(|c| {
            let (&(on, heat_on), &(off, heat_off)) = (c[0], c[1]);
            let length = (off + WEEK - on) % WEEK;
            (heat_on && !heat_off && length > 0 && length < DAY).then_some((on, off))
        })
        .collect()
}

impl thermostat::ClusterHandler for HotWater {
    const CLUSTER: Cluster<'static> = thermostat::FULL_CLUSTER
        .with_features(thermostat::Feature::HEATING.bits() | thermostat::Feature::MATTER_SCHEDULE_CONFIGURATION.bits())
        .with_attrs(with!(
            required;
            thermostat::AttributeId::OccupiedHeatingSetpoint
                | thermostat::AttributeId::AbsMinHeatSetpointLimit
                | thermostat::AttributeId::AbsMaxHeatSetpointLimit
                | thermostat::AttributeId::MinHeatSetpointLimit
                | thermostat::AttributeId::MaxHeatSetpointLimit
                | thermostat::AttributeId::ScheduleTypes
                | thermostat::AttributeId::NumberOfSchedules
                | thermostat::AttributeId::NumberOfScheduleTransitions
                | thermostat::AttributeId::NumberOfScheduleTransitionPerDay
                | thermostat::AttributeId::ActiveScheduleHandle
                | thermostat::AttributeId::Schedules
        ))
        .with_cmds(with!(
            thermostat::CommandId::SetpointRaiseLower
                | thermostat::CommandId::SetActiveScheduleRequest
                | thermostat::CommandId::AtomicRequest
        ));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    fn local_temperature(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        let view = self.nest.view();
        let reported = view.device_flag("has_hot_water_temperature");
        Ok(Nullable::new(reported.then(|| view.device_centi("current_water_temperature")).flatten()))
    }

    fn occupied_heating_setpoint(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        let view = self.nest.view();
        let reported = view.device_flag("has_hot_water_temperature").then(|| view.device_centi("dhw_setpoint"));
        Ok(reported.flatten().map_or(SETPOINT, |s| s.clamp(MIN, MAX)))
    }

    fn abs_min_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(MIN)
    }

    fn abs_max_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(MAX)
    }

    fn min_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(MIN)
    }

    fn max_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(MAX)
    }

    fn control_sequence_of_operation(&self, _ctx: impl ReadContext) -> Result<ControlSequenceOfOperationEnum, Error> {
        Ok(ControlSequenceOfOperationEnum::HeatingOnly)
    }

    fn system_mode(&self, _ctx: impl ReadContext) -> Result<SystemModeEnum, Error> {
        Ok(if following(&self.nest) { SystemModeEnum::Heat } else { SystemModeEnum::Off })
    }

    fn set_system_mode(&self, _ctx: impl WriteContext, value: SystemModeEnum) -> Result<(), Error> {
        match value {
            SystemModeEnum::Heat => set(&self.nest, "hot_water_schedule", "ON"),
            SystemModeEnum::Off => set(&self.nest, "hot_water_schedule", "OFF"),
            _ => Err(ErrorCode::ConstraintError.into()),
        }
    }

    // The unit takes no hot water setpoint.
    fn set_occupied_heating_setpoint(&self, _ctx: impl WriteContext, _value: i16) -> Result<(), Error> {
        Err(ErrorCode::UnsupportedAccess.into())
    }

    fn set_control_sequence_of_operation(
        &self,
        _ctx: impl WriteContext,
        _value: ControlSequenceOfOperationEnum,
    ) -> Result<(), Error> {
        Err(ErrorCode::UnsupportedAccess.into())
    }

    fn schedule_types<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: ArrayAttributeRead<ScheduleTypeStructArrayBuilder<P>, ScheduleTypeStructBuilder<P>>,
    ) -> Result<P, Error> {
        // One schedule, of transitions to Heat and to Off.
        macro_rules! one {
            ($b:expr) => {
                $b.system_mode(SystemModeEnum::Heat)?
                    .number_of_schedules(1)?
                    .schedule_type_features(ScheduleTypeFeaturesBitmap::SUPPORTS_OFF)?
                    .end()
            };
        }
        match builder {
            ArrayAttributeRead::ReadAll(array) => one!(array.push()?)?.end(),
            ArrayAttributeRead::ReadOne(0, item) => one!(item),
            ArrayAttributeRead::ReadOne(..) => Err(ErrorCode::ConstraintError.into()),
            ArrayAttributeRead::ReadNone(array) => array.end(),
        }
    }

    fn number_of_schedules(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        Ok(1)
    }

    fn number_of_schedule_transitions(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        Ok((hotwater::DAILY * 2 * 7) as u8)
    }

    fn number_of_schedule_transition_per_day(&self, _ctx: impl ReadContext) -> Result<Nullable<u8>, Error> {
        Ok(Nullable::some(TRANSITIONS_PER_DAY as u8))
    }

    fn active_schedule_handle<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: NullableBuilder<P, OctetsBuilder<P>>,
    ) -> Result<P, Error> {
        if !self.current().is_empty() {
            builder.non_null()?.set(Octets(SCHEDULE))
        } else {
            builder.null()
        }
    }

    fn schedules<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: ArrayAttributeRead<ScheduleStructArrayBuilder<P>, ScheduleStructBuilder<P>>,
    ) -> Result<P, Error> {
        let writer = msch::writer(&ctx, ctx.attr().fab_idx);
        msch::read(&self.schedules.shown(&writer, || self.current()), builder)
    }

    fn set_schedules(
        &self,
        ctx: impl WriteContext,
        value: ArrayAttributeWrite<TLVArray<'_, thermostat::ScheduleStruct<'_>>, thermostat::ScheduleStruct<'_>>,
    ) -> Result<(), Error> {
        self.schedules
            .write(&msch::writer(&ctx, ctx.attr().fab_idx), 1, value)
            .inspect_err(|e| crate::logf!("matter: hot water Schedules write refused, {e:?}"))
    }

    fn handle_setpoint_raise_lower(
        &self,
        _ctx: impl InvokeContext,
        _request: thermostat::SetpointRaiseLowerRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::ConstraintError.into())
    }

    fn handle_set_active_schedule_request(
        &self,
        _ctx: impl InvokeContext,
        request: thermostat::SetActiveScheduleRequestRequest<'_>,
    ) -> Result<(), Error> {
        // The one schedule is always active.
        if request.schedule_handle()?.0 == SCHEDULE && !self.current().is_empty() {
            Ok(())
        } else {
            Err(ErrorCode::NotFound.into())
        }
    }

    fn handle_atomic_request<P: TLVBuilderParent>(
        &self,
        ctx: impl InvokeContext,
        request: thermostat::AtomicRequestRequest<'_>,
        response: thermostat::AtomicResponseBuilder<P>,
    ) -> Result<P, Error> {
        let refused = || {
            self.dataver.changed();
            ctx.notify_own_attr_changed(SCHEDULES);
        };
        self.schedules.atomic_request(
            &ctx,
            &request,
            response,
            || self.current(),
            |initial, p| self.commit_checked(initial, p),
            refused,
        )
    }

    fn handle_set_weekly_schedule(
        &self,
        _ctx: impl InvokeContext,
        _request: thermostat::SetWeeklyScheduleRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    fn handle_get_weekly_schedule<P: TLVBuilderParent>(
        &self,
        _ctx: impl InvokeContext,
        _request: thermostat::GetWeeklyScheduleRequest<'_>,
        _response: thermostat::GetWeeklyScheduleResponseBuilder<P>,
    ) -> Result<P, Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    fn handle_clear_weekly_schedule(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    fn handle_set_active_preset_request(
        &self,
        _ctx: impl InvokeContext,
        _request: thermostat::SetActivePresetRequestRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    fn handle_add_thermostat_suggestion<P: TLVBuilderParent>(
        &self,
        _ctx: impl InvokeContext,
        _request: thermostat::AddThermostatSuggestionRequest<'_>,
        _response: thermostat::AddThermostatSuggestionResponseBuilder<P>,
    ) -> Result<P, Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    fn handle_remove_thermostat_suggestion(
        &self,
        _ctx: impl InvokeContext,
        _request: thermostat::RemoveThermostatSuggestionRequest<'_>,
    ) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }
}

/// Water Heater Management: what heats the water, whether it is heating,
/// and boost.
pub struct Management {
    pub dataver: Dataver,
    pub nest: Arc<Nest>,
}

impl Management {
    /// Whether a boost runs now.
    fn boosting(&self) -> bool {
        let end = self.nest.view().device_number("hot_water_boost_time_to_end").unwrap_or(0.0);
        end > chrono::Utc::now().timestamp() as f64
    }
}

impl management::ClusterHandler for Management {
    const CLUSTER: Cluster<'static> = management::FULL_CLUSTER
        .with_features(0)
        .with_attrs(with!(required))
        .with_cmds(with!(management::CommandId::Boost | management::CommandId::CancelBoost));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    fn heater_types(&self, _ctx: impl ReadContext) -> Result<WaterHeaterHeatSourceBitmap, Error> {
        Ok(WaterHeaterHeatSourceBitmap::BOILER)
    }

    fn heat_demand(&self, _ctx: impl ReadContext) -> Result<WaterHeaterHeatSourceBitmap, Error> {
        let on = self.nest.view().device_flag("hot_water_active");
        Ok(if on { WaterHeaterHeatSourceBitmap::BOILER } else { WaterHeaterHeatSourceBitmap::empty() })
    }

    fn boost_state(&self, _ctx: impl ReadContext) -> Result<BoostStateEnum, Error> {
        Ok(if self.boosting() { BoostStateEnum::Active } else { BoostStateEnum::Inactive })
    }

    fn handle_boost(&self, _ctx: impl InvokeContext, request: management::BoostRequest<'_>) -> Result<(), Error> {
        // Whole minutes, as the unit counts its boost, heating as the unit
        // does: it has no one-shot boost, nor one to another temperature or
        // fill (an emergency boost is the one it has).
        let info = request.boost_info()?;
        let seconds = info.duration()?;
        let longest = crate::bridge::HOT_WATER_BOOST_MAX_MINUTES;
        let minutes = seconds / 60;
        let unsupported = info.one_shot()? == Some(true)
            || info.temporary_setpoint()?.is_some()
            || info.target_percentage()?.is_some()
            || info.target_reheat()?.is_some();
        if unsupported || seconds % 60 != 0 || minutes == 0 || i64::from(minutes) > longest {
            return Err(ErrorCode::ConstraintError.into());
        }
        set(&self.nest, "hot_water_boost", &minutes.to_string())
    }

    fn handle_cancel_boost(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        set(&self.nest, "hot_water_boost", "OFF")
    }
}

/// Emits BoostStarted or BoostEnded when a boost has started or ended since
/// `was`, from whichever side started it (the unit's screen, MQTT, Zigbee,
/// Matter) or by running out; `was` then holds whether one runs. The first
/// call only records it.
pub fn report_boost(emitter: impl EventEmitter, nest: &Nest, was: &mut Option<bool>) -> Result<(), Error> {
    let dev = nest.view().dev;
    let running = crate::bridge::hot_water_boost(&dev).is_some();
    let before = was.replace(running);
    match (before, running) {
        (Some(false), true) => {
            let end = dev.get("hot_water_boost_time_to_end").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
            let seconds = (end - chrono::Utc::now().timestamp() as f64).max(0.0).round() as u32;
            management::BoostStarted::emit_for(emitter, WATER_HEATER, |event| {
                event
                    .boost_info()?
                    .duration(seconds)?
                    .one_shot(None)?
                    .emergency_boost(None)?
                    .temporary_setpoint(None)?
                    .target_percentage(None)?
                    .target_reheat(None)?
                    .end()?
                    .end()
            })?;
        }
        (Some(true), false) => {
            management::BoostEnded::emit_for(emitter, WATER_HEATER, |event| event.end())?;
        }
        _ => {}
    }
    Ok(())
}

/// Water Heater Mode: off, or following the schedule.
pub struct Mode {
    pub dataver: Dataver,
    pub nest: Arc<Nest>,
}

impl mode::ClusterHandler for Mode {
    const CLUSTER: Cluster<'static> =
        mode::FULL_CLUSTER.with_features(0).with_attrs(with!(required)).with_cmds(with!(mode::CommandId::ChangeToMode));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    fn supported_modes<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: ArrayAttributeRead<ModeOptionStructArrayBuilder<P>, ModeOptionStructBuilder<P>>,
    ) -> Result<P, Error> {
        const MODES: [(&str, u8, ModeTag); 2] =
            [("Off", MODE_OFF, ModeTag::Off), ("Schedule", MODE_TIMED, ModeTag::Timed)];
        fn one<P: TLVBuilderParent>(
            b: ModeOptionStructBuilder<P>,
            (label, id, tag): (&str, u8, ModeTag),
        ) -> Result<P, Error> {
            b.label(label)?.mode(id)?.mode_tags()?.push()?.mfg_code(None)?.value(tag as u16)?.end()?.end()?.end()
        }
        match builder {
            ArrayAttributeRead::ReadAll(mut array) => {
                for m in MODES {
                    array = one(array.push()?, m)?;
                }
                array.end()
            }
            ArrayAttributeRead::ReadOne(i, item) => match MODES.get(usize::from(i)) {
                Some(&m) => one(item, m),
                None => Err(ErrorCode::ConstraintError.into()),
            },
            ArrayAttributeRead::ReadNone(array) => array.end(),
        }
    }

    fn current_mode(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        Ok(if following(&self.nest) { MODE_TIMED } else { MODE_OFF })
    }

    fn handle_change_to_mode<P: TLVBuilderParent>(
        &self,
        _ctx: impl InvokeContext,
        request: mode::ChangeToModeRequest<'_>,
        response: mode::ChangeToModeResponseBuilder<P>,
    ) -> Result<P, Error> {
        // Mode Base's statuses: 0 success, 1 unsupported mode, 2 generic failure.
        let status = match request.new_mode()? {
            MODE_OFF => set(&self.nest, "hot_water_schedule", "OFF").map_or(2, |()| 0),
            MODE_TIMED => set(&self.nest, "hot_water_schedule", "ON").map_or(2, |()| 0),
            _ => 1,
        };
        response.status(status)?.status_text(None)?.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields;
    use crate::matter::tests::{ctx, field, nest};
    use rs_matter::dm::clusters::decl::thermostat::ClusterHandler as _;
    use rs_matter::dm::clusters::decl::water_heater_management::ClusterHandler as _;
    use rs_matter::dm::clusters::decl::water_heater_mode::ClusterHandler as _;
    use rs_matter::tlv::{TLVElement, TLVTag, TLVWriteParent};
    use rs_matter::utils::storage::WriteBuf;
    use serde_json::json;

    const MONDAY: u8 = 0x02;
    const TUESDAY: u8 = 0x04;
    const SUNDAY: u8 = 0x01;

    fn unit(dev: crate::json::Fields) -> Arc<Nest> {
        let nest = nest(dev, fields! {"can_heat" => true});
        let interval = |sd: i64, st: i64, ed: i64, et: i64| json!({"start_day": sd, "start_time": st, "end_day": ed, "end_time": et});
        // Monday 06:00-08:00, Sunday 23:00-01:00 (Monday).
        let hw = fields! {"mode" => "hotwater", "name" => "HOTWATER",
        "intervals" => json!([interval(0, 21600, 0, 28800), interval(6, 82800, 0, 3600)])};
        nest.bridge.store.merge("custom_schedule.hotwater", &hw, 0, 0, true);
        nest
    }

    fn on(days: u8, minutes: u16) -> Transition {
        Transition { days, minutes, heat: None, cool: None, mode: Some(SystemModeEnum::Heat), preset: false }
    }

    fn off(days: u8, minutes: u16) -> Transition {
        Transition { mode: Some(SystemModeEnum::Off), ..on(days, minutes) }
    }

    fn schedule(transitions: Vec<Transition>) -> Vec<Pending> {
        vec![Pending {
            name: None,
            handle: Some(SCHEDULE.to_vec()),
            mode: SystemModeEnum::Heat,
            built_in: None,
            preset: false,
            transitions,
        }]
    }

    fn days(nest: &Nest) -> Vec<Vec<Period>> {
        let hw = nest.hot_water().unwrap();
        (0..7).map(|d| hotwater::day(&hw, d)).collect()
    }

    #[test]
    fn schedules_shows_each_period_as_heat_then_off() {
        let h = HotWater::new(Dataver::new(0), unit(fields! {"has_hot_water_control" => true}));
        assert_eq!(
            h.current()[0].transitions,
            [on(MONDAY, 360), off(MONDAY, 480), on(SUNDAY, 1380), off(MONDAY, 60)],
            "past midnight, the off is the next day's"
        );
        let none = HotWater::new(Dataver::new(0), nest(fields! {}, fields! {}));
        assert_eq!(none.current(), []);
    }

    #[test]
    fn a_commit_replaces_the_week_of_periods() {
        let n = unit(fields! {"has_hot_water_control" => true});
        let h = HotWater::new(Dataver::new(0), n.clone());
        // Tuesday 06:00-07:30 twice a week (Tuesday and Sunday's bits), and
        // Monday 23:30 until 00:30 Tuesday.
        let week = vec![on(TUESDAY | SUNDAY, 360), off(TUESDAY | SUNDAY, 450), on(MONDAY, 1410), off(TUESDAY, 30)];
        h.commit(&schedule(week)).unwrap();
        let d = days(&n);
        assert_eq!((&d[0], &d[1], &d[6]), (&vec![(1410, 30)], &vec![(360, 450)], &vec![(360, 450)]));
        assert!(d[2..6].iter().all(Vec::is_empty));
        // Sunday 23:00 round the week's end into Monday 01:00.
        h.commit(&schedule(vec![off(MONDAY, 60), on(SUNDAY, 1380)])).unwrap();
        assert_eq!(days(&n)[6], [(1380, 60)]);
        h.commit(&schedule(vec![])).unwrap();
        assert!(days(&n).iter().all(Vec::is_empty), "none at all");
    }

    #[test]
    fn a_commit_refuses_what_is_no_week_of_periods() {
        let n = unit(fields! {"has_hot_water_control" => true});
        let h = HotWater::new(Dataver::new(0), n.clone());
        let before = days(&n);
        let seven: Vec<_> = (0..7u16).flat_map(|i| [on(MONDAY, i * 60), off(MONDAY, i * 60 + 30)]).collect();
        for (week, code) in [
            (vec![on(MONDAY, 360), on(MONDAY, 400), off(MONDAY, 480)], ErrorCode::ConstraintError),
            (vec![on(MONDAY, 360)], ErrorCode::ConstraintError),
            (vec![on(MONDAY, 360), off(TUESDAY, 400)], ErrorCode::ConstraintError), // a day or more
            (vec![on(MONDAY, 360), Transition { heat: Some(5000), ..off(MONDAY, 400) }], ErrorCode::ConstraintError),
            (
                vec![on(MONDAY, 360), Transition { mode: Some(SystemModeEnum::Cool), ..off(MONDAY, 400) }],
                ErrorCode::ConstraintError,
            ),
            (seven, ErrorCode::ResourceExhausted),
        ] {
            assert_eq!(h.commit(&schedule(week.clone())), Err(code), "{week:?}");
        }
        assert_eq!(days(&n), before);
    }

    #[test]
    fn periods_that_touch_show_as_one() {
        let h = (1440, 1440 + 360);
        // Monday 06:00-08:00 and 08:00-10:00; Sunday 23:00-00:00 into Monday's 00:00-01:00.
        assert_eq!(coalesce(vec![(360, 480), (480, 600)]), [(360, 600)]);
        assert_eq!(coalesce(vec![(0, 60), (6 * 1440 + 1380, WEEK)]), [(6 * 1440 + 1380, WEEK + 60)]);
        assert_eq!(coalesce(vec![(360, 480), (500, 600)]), [(360, 480), (500, 600)], "a gap");
        assert_eq!(coalesce(vec![(0, 1000), (1000, 1500)]), [(0, 1000), (1000, 1500)], "a day or more joined");
        assert_eq!(coalesce(vec![h]), [h]);
    }

    #[test]
    fn on_transitions_may_leave_out_heat_or_carry_a_setpoint() {
        let n = unit(fields! {"has_hot_water_control" => true});
        let h = HotWater::new(Dataver::new(0), n.clone());
        let by_default = Transition { mode: None, ..on(TUESDAY, 360) };
        let with_setpoint = Transition { heat: Some(6000), ..on(TUESDAY, 600) };
        h.commit(&schedule(vec![by_default, off(TUESDAY, 420), with_setpoint, off(TUESDAY, 660)])).unwrap();
        assert_eq!(days(&n)[1], [(360, 420), (600, 660)]);
    }

    #[test]
    fn a_day_takes_its_six_periods_and_the_night_before() {
        let n = unit(fields! {"has_hot_water_control" => true});
        let h = HotWater::new(Dataver::new(0), n.clone());
        let mut week = vec![on(MONDAY, 1380), off(TUESDAY, 60)];
        week.extend((0..6u16).flat_map(|i| [on(TUESDAY, 120 + i * 120), off(TUESDAY, 180 + i * 120)]));
        h.commit(&schedule(week)).unwrap();
        assert_eq!(days(&n)[1].len(), 6);
        assert_eq!(h.current()[0].transitions.iter().filter(|t| t.days == TUESDAY).count(), TRANSITIONS_PER_DAY);
    }

    #[test]
    fn system_mode_and_water_heater_mode_follow_the_hot_water_switch() {
        let n = unit(fields! {"has_hot_water_control" => true, "hot_water_mode" => "schedule"});
        let (h, m) = (HotWater::new(Dataver::new(0), n.clone()), Mode { dataver: Dataver::new(0), nest: n.clone() });
        assert_eq!(
            (h.system_mode(ctx(8)).unwrap(), m.current_mode(ctx(8)).unwrap()),
            (SystemModeEnum::Heat, MODE_TIMED)
        );
        h.set_system_mode(ctx(8), SystemModeEnum::Off).unwrap();
        assert_eq!(field(&n, "device", "hot_water_mode"), json!("off"));
        assert_eq!((h.system_mode(ctx(8)).unwrap(), m.current_mode(ctx(8)).unwrap()), (SystemModeEnum::Off, MODE_OFF));
        assert!(h.set_system_mode(ctx(8), SystemModeEnum::Cool).is_err());
        let change = |mode: u8| {
            let tlv = [0x15, 0x24, 0x00, mode, 0x18];
            let mut buf = [0u8; 64];
            let mut wb = WriteBuf::new(&mut buf);
            let response =
                mode::ChangeToModeResponseBuilder::new(TLVWriteParent::new("test", &mut wb), &TLVTag::Anonymous)
                    .unwrap();
            m.handle_change_to_mode(ctx(8), mode::ChangeToModeRequest::new(TLVElement::new(&tlv)), response).unwrap();
            let len = wb.get_tail();
            mode::ChangeToModeResponse::new(TLVElement::new(&buf[..len])).status().unwrap()
        };
        assert_eq!(change(MODE_TIMED), 0);
        assert_eq!(field(&n, "device", "hot_water_mode"), json!("schedule"));
        assert_eq!(change(7), 1, "no such mode");
    }

    #[test]
    fn boost_runs_for_its_duration_up_to_the_longest() {
        let n = unit(fields! {"has_hot_water_control" => true});
        let w = Management { dataver: Dataver::new(0), nest: n.clone() };
        let boost = |seconds: u32| {
            let mut tlv = vec![0x15, 0x35, 0x00, 0x26, 0x00];
            tlv.extend(seconds.to_le_bytes());
            tlv.extend([0x18, 0x18]);
            w.handle_boost(ctx(8), management::BoostRequest::new(TLVElement::new(&tlv)))
        };
        assert_eq!(w.boost_state(ctx(8)).unwrap(), BoostStateEnum::Inactive);
        for minutes in [1, 37, 75, 120] {
            boost(minutes * 60).unwrap();
            let left = field(&n, "device", "hot_water_boost_time_to_end").as_f64().unwrap()
                - chrono::Utc::now().timestamp() as f64;
            assert!((f64::from(minutes * 60) - 2.0..=f64::from(minutes * 60)).contains(&left), "{left}");
            assert_eq!(w.boost_state(ctx(8)).unwrap(), BoostStateEnum::Active);
        }
        let end = field(&n, "device", "hot_water_boost_time_to_end");
        for seconds in [0, 90, 121 * 60, u32::MAX] {
            assert!(boost(seconds).is_err(), "{seconds} seconds");
            assert_eq!(field(&n, "device", "hot_water_boost_time_to_end"), end);
        }
        let one_shot = {
            let mut tlv = vec![0x15, 0x35, 0x00, 0x26, 0x00];
            tlv.extend(1800u32.to_le_bytes());
            tlv.extend([0x29, 0x01, 0x18, 0x18]);
            w.handle_boost(ctx(8), management::BoostRequest::new(TLVElement::new(&tlv)))
        };
        assert!(one_shot.is_err(), "the unit has no one-shot boost");
        w.handle_cancel_boost(ctx(8)).unwrap();
        assert_eq!(w.boost_state(ctx(8)).unwrap(), BoostStateEnum::Inactive);
        assert_eq!(w.heater_types(ctx(8)).unwrap(), WaterHeaterHeatSourceBitmap::BOILER);
        assert_eq!(w.heat_demand(ctx(8)).unwrap(), WaterHeaterHeatSourceBitmap::empty());
    }

    /// The events emitted: endpoint, cluster, event.
    #[derive(Default)]
    struct Events(std::cell::RefCell<Vec<(u16, u32, u32)>>);

    impl EventEmitter for Events {
        fn emit_event<F>(
            &self,
            endpoint: u16,
            cluster: u32,
            event: u32,
            _: rs_matter::im::EventPriority,
            _: F,
        ) -> Result<rs_matter::dm::EventNumber, Error>
        where
            F: FnOnce(rs_matter::im::events::EventTLVWrite<'_>) -> Result<(), Error>,
        {
            self.0.borrow_mut().push((endpoint, cluster, event));
            Ok(0)
        }
    }

    #[test]
    fn a_boost_from_anywhere_starts_and_ends_with_its_events() {
        let n = unit(fields! {"has_hot_water_control" => true});
        let events = Events::default();
        let mut was = None;
        report_boost(&events, &n, &mut was).unwrap();
        assert!(events.0.borrow().is_empty(), "the first look only records");
        let ids = |events: &Events| events.0.borrow().iter().map(|e| e.2).collect::<Vec<_>>();
        let cluster = management::FULL_CLUSTER.id;
        // A boost from MQTT, as from the unit's screen: BoostStarted, once.
        n.apply("hot_water_boost", "60").unwrap();
        report_boost(&events, &n, &mut was).unwrap();
        report_boost(&events, &n, &mut was).unwrap();
        assert_eq!(*events.0.borrow(), [(WATER_HEATER, cluster, management::EventId::BoostStarted as u32)]);
        // Cancelled, or run out: BoostEnded.
        n.apply("hot_water_boost", "off").unwrap();
        report_boost(&events, &n, &mut was).unwrap();
        assert_eq!(ids(&events), [management::EventId::BoostStarted as u32, management::EventId::BoostEnded as u32]);
    }

    #[test]
    fn the_water_heater_comes_with_hot_water_control() {
        let ids = |hot_water| {
            let switches =
                crate::matter::extras::switches(&unit(fields! {"has_hot_water_control" => true, "has_fan" => true}));
            super::super::node_for_network(true, super::super::Equipment::Both, hot_water, &switches, false, true)
                .endpoints
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>()
        };
        assert!(!ids(false).contains(&WATER_HEATER));
        assert!(ids(true).contains(&WATER_HEATER));
        // Ascending, as rs-matter serves them: a wildcard read skips an endpoint
        // listed after a higher one, and controllers then never see it.
        for hot_water in [false, true] {
            let ids = ids(hot_water);
            assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:?}");
        }
    }
}
