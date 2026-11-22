//! The Thermostat cluster on the thermostat endpoint, read from the unit's
//! buckets and written through the same commands as MQTT's set/ topics.
//!
//! Every Matter temperature is in hundredths of a degree Celsius. The
//! features follow the equipment the unit reported when Matter started:
//! heating, cooling, or both and auto; controllers offer the modes from them.
//! Eco is the one preset (PRES), and the eco temperatures are also the
//! unoccupied setpoints (OCC).
//!
//! With auto, heating stays a dead band below cooling: the limits and the
//! setpoints alike. Outside its range mode the unit has one target, the
//! mode's setpoint (cooling in cool and on a unit that only cools, heating
//! otherwise); the other setpoint is the range's end. A value the unit holds
//! past a limit (set on the unit, over MQTT or Zigbee) reads as the limit.

use std::sync::Arc;

use rs_matter::dm::clusters::decl::thermostat::{
    self, ClusterHandler, ControlSequenceOfOperationEnum, OccupancyBitmap, PresetScenarioEnum,
    PresetStructArrayBuilder, PresetStructBuilder, PresetTypeFeaturesBitmap, PresetTypeStructArrayBuilder,
    PresetTypeStructBuilder, RelayStateBitmap, SetpointRaiseLowerModeEnum, SystemModeEnum,
};
use rs_matter::dm::{
    ArrayAttributeRead, Attribute, Cluster, Dataver, InvokeContext, Quality, ReadContext, WriteContext,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{Nullable, NullableBuilder, Octets, OctetsBuilder, TLVBuilderParent};
use rs_matter::with;

use super::{centi, failed, Nest, View};

/// The unit's setpoint range, as the MQTT climate entity has it.
const MIN: i16 = 900;
const MAX: i16 = 3200;
/// The least gap the unit keeps between a range's low and high, 0.1 °C.
const DEAD_BAND: i8 = 15;
const GAP: i16 = DEAD_BAND as i16 * 10;
/// The eco temperatures' own bounds, as the MQTT number entities have them:
/// the unoccupied heating setpoint at most, the cooling one at least.
const ECO_HEAT_MAX: i16 = 2100;
const ECO_COOL_MIN: i16 = 2400;
const ECO: &[u8] = b"eco";

/// What the unit's equipment does, as the cluster's features have it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Equipment {
    Heat,
    Cool,
    Both,
}

impl Equipment {
    /// From the unit's can_heat and can_cool; neither reads as both.
    pub fn of(nest: &Nest) -> Self {
        let view = nest.view();
        match (view.shared_flag("can_heat"), view.shared_flag("can_cool")) {
            (true, false) => Self::Heat,
            (false, true) => Self::Cool,
            _ => Self::Both,
        }
    }

    /// The cluster with this equipment's features.
    pub const fn cluster(self) -> Cluster<'static> {
        let modes = match self {
            Self::Heat => thermostat::Feature::HEATING.bits(),
            Self::Cool => thermostat::Feature::COOLING.bits(),
            Self::Both => {
                thermostat::Feature::HEATING.bits()
                    | thermostat::Feature::COOLING.bits()
                    | thermostat::Feature::AUTO_MODE.bits()
            }
        };
        Thermostat::CLUSTER
            .with_features(modes | thermostat::Feature::OCCUPANCY.bits() | thermostat::Feature::PRESETS.bits())
    }

    fn heats(self) -> bool {
        self != Self::Cool
    }

    fn cools(self) -> bool {
        self != Self::Heat
    }

    /// The heating setpoint's limits: with auto, a dead band under cooling's.
    fn heat_limits(self) -> (i16, i16) {
        (MIN, if self == Self::Both { MAX - GAP } else { MAX })
    }

    /// The cooling setpoint's limits: with auto, a dead band over heating's.
    fn cool_limits(self) -> (i16, i16) {
        (if self == Self::Both { MIN + GAP } else { MIN }, MAX)
    }
}

/// The attributes every thermostat has, OutdoorTemperature, and those the
/// cluster's features make mandatory.
fn attributes(attr: &Attribute, _revision: u16, features: u32) -> bool {
    use thermostat::AttributeId as A;
    let has = |feature: thermostat::Feature| features & feature.bits() != 0;
    if !attr.quality.contains(Quality::OPTIONAL) {
        return true;
    }
    match A::try_from(attr.id) {
        Ok(
            A::OutdoorTemperature
            | A::Occupancy
            | A::ThermostatRunningState
            | A::PresetTypes
            | A::NumberOfPresets
            | A::ActivePresetHandle
            | A::Presets,
        ) => true,
        Ok(
            A::OccupiedHeatingSetpoint
            | A::UnoccupiedHeatingSetpoint
            | A::AbsMinHeatSetpointLimit
            | A::AbsMaxHeatSetpointLimit
            | A::MinHeatSetpointLimit
            | A::MaxHeatSetpointLimit,
        ) => has(thermostat::Feature::HEATING),
        Ok(
            A::OccupiedCoolingSetpoint
            | A::UnoccupiedCoolingSetpoint
            | A::AbsMinCoolSetpointLimit
            | A::AbsMaxCoolSetpointLimit
            | A::MinCoolSetpointLimit
            | A::MaxCoolSetpointLimit,
        ) => has(thermostat::Feature::COOLING),
        Ok(A::MinSetpointDeadBand) => has(thermostat::Feature::AUTO_MODE),
        _ => false,
    }
}

pub struct Thermostat {
    dataver: Dataver,
    nest: Arc<Nest>,
    equipment: Equipment,
}

impl Thermostat {
    pub fn new(dataver: Dataver, nest: Arc<Nest>, equipment: Equipment) -> Self {
        Self { dataver, nest, equipment }
    }

    fn set(&self, name: &str, arg: &str) -> Result<(), Error> {
        self.nest.apply(name, arg).map_err(failed)
    }

    /// Whether the unit's one target is the cooling setpoint, outside range mode.
    fn target_cools(&self, mode: &str) -> bool {
        mode == "cool" || (self.equipment == Equipment::Cool && mode != "heat")
    }

    /// Heating and cooling inside their limits and, with auto, a dead band
    /// apart: the side kept is cooling's if `keep_cooling`.
    fn apart(&self, heat: i16, cool: i16, keep_cooling: bool) -> (i16, i16) {
        let ((heat_min, heat_max), (cool_min, cool_max)) = (self.equipment.heat_limits(), self.equipment.cool_limits());
        let (heat, cool) = (heat.clamp(heat_min, heat_max), cool.clamp(cool_min, cool_max));
        match self.equipment {
            Equipment::Both if cool - heat < GAP && keep_cooling => (cool - GAP, cool),
            Equipment::Both if cool - heat < GAP => (heat, heat + GAP),
            _ => (heat, cool),
        }
    }

    /// The regular heating and cooling setpoints, used when adjusting the schedule.
    fn regular_targets(&self, view: &View) -> (i16, i16) {
        let mode = view.mode();
        let low = view.shown_centi("target_low").unwrap_or(2000);
        let high = view.shown_centi("target_high").unwrap_or(2400);
        match view.shown_centi("target") {
            Some(target) if mode != "heat_cool" && self.target_cools(mode) => self.apart(low, target, true),
            Some(target) if mode != "heat_cool" => self.apart(target, high, false),
            _ => self.apart(low, high, false),
        }
    }

    fn occupied(&self) -> (i16, i16) {
        let view = self.nest.view();
        let (heat, cool) = self.regular_targets(&view);
        let Some((eco_heat, eco_cool)) = crate::bridge::eco_targets(&view.state) else {
            return (heat, cool);
        };
        let (eco_heat, eco_cool) = match view.mode() {
            "heat" => (eco_heat, None),
            "cool" => (None, eco_cool),
            "heat_cool" => (eco_heat, eco_cool),
            _ => return (heat, cool),
        };
        // Occupied setpoints require a number even when an Eco limit is off.
        self.apart(eco_heat.map_or(heat, centi), eco_cool.map_or(cool, centi), eco_heat.is_none() && eco_cool.is_some())
    }

    /// Sets the occupied setpoints given, to the unit's half degrees. One
    /// that comes within the dead band of the other moves the other.
    /// Outside range mode, the side that is not the target moves only when
    /// written: the target otherwise keeps it clear as it reads.
    fn change(&self, heat: Option<i16>, cool: Option<i16>) -> Result<(), Error> {
        let view = self.nest.view();
        let mode = view.mode();
        let (old_heat, old_cool) = self.regular_targets(&view);
        let (new_heat, new_cool) = self.apart(heat.map_or(old_heat, half), cool.map_or(old_cool, half), heat.is_none());
        // Off eco: the setpoints written are now the ones in force.
        if view.state_str("preset") == "eco" {
            self.set("preset", "none")?;
        }
        let ranged = mode == "heat_cool";
        let target_cools = self.target_cools(mode);
        let sides = [(false, heat, new_heat, old_heat), (true, cool, new_cool, old_cool)];
        // Whichever moves up goes first, so the two never cross on the way.
        let order = if new_cool > old_cool { [1, 0] } else { [0, 1] };
        for (cooling, written, new, old) in order.map(|i| sides[i]) {
            let target = !ranged && cooling == target_cools;
            if written.is_none() && (new == old || !(ranged || target)) {
                continue;
            }
            let name = match (target, cooling) {
                (true, _) => "target",
                (false, true) => "target_high",
                (false, false) => "target_low",
            };
            self.set(name, &degrees(new))?;
        }
        Ok(())
    }

    /// Writes an occupied setpoint inside its limits.
    fn set_occupied(&self, heating: bool, value: i16) -> Result<(), Error> {
        let (min, max) = if heating { self.equipment.heat_limits() } else { self.equipment.cool_limits() };
        if !(min..=max).contains(&value) {
            return Err(ErrorCode::ConstraintError.into());
        }
        if heating {
            self.change(Some(value), None)
        } else {
            self.change(None, Some(value))
        }
    }

    /// The unoccupied (eco) heating and cooling setpoints, inside the limits.
    fn unoccupied(&self) -> (i16, i16) {
        let view = self.nest.view();
        let ((heat_min, heat_max), (cool_min, cool_max)) = (self.equipment.heat_limits(), self.equipment.cool_limits());
        (
            view.shown_centi("away_temperature_low").unwrap_or(MIN).clamp(heat_min, heat_max),
            view.shown_centi("away_temperature_high").unwrap_or(MAX).clamp(cool_min, cool_max),
        )
    }

    fn sequence(&self) -> ControlSequenceOfOperationEnum {
        match self.equipment {
            Equipment::Heat => ControlSequenceOfOperationEnum::HeatingOnly,
            Equipment::Cool => ControlSequenceOfOperationEnum::CoolingOnly,
            Equipment::Both => ControlSequenceOfOperationEnum::CoolingAndHeating,
        }
    }
}

impl ClusterHandler for Thermostat {
    /// Every feature; the node has the unit's (`Equipment::cluster`).
    const CLUSTER: Cluster<'static> = thermostat::FULL_CLUSTER
        .with_features(
            thermostat::Feature::HEATING.bits()
                | thermostat::Feature::COOLING.bits()
                | thermostat::Feature::AUTO_MODE.bits()
                | thermostat::Feature::OCCUPANCY.bits()
                | thermostat::Feature::PRESETS.bits(),
        )
        .with_attrs(attributes)
        .with_cmds(with!(thermostat::CommandId::SetpointRaiseLower | thermostat::CommandId::SetActivePresetRequest));

    fn dataver(&self) -> u32 {
        self.dataver.get()
    }

    fn dataver_changed(&self) {
        self.dataver.changed();
    }

    fn local_temperature(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::new(self.nest.view().shown_centi("current_temperature")))
    }

    fn outdoor_temperature(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::new(self.nest.outdoor_temperature().map(centi)))
    }

    fn occupancy(&self, _ctx: impl ReadContext) -> Result<OccupancyBitmap, Error> {
        Ok(if self.nest.bridge.occupancy.occupied() { OccupancyBitmap::OCCUPIED } else { OccupancyBitmap::empty() })
    }

    fn abs_min_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.equipment.heat_limits().0)
    }

    fn abs_max_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.equipment.heat_limits().1)
    }

    fn abs_min_cool_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.equipment.cool_limits().0)
    }

    fn abs_max_cool_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.equipment.cool_limits().1)
    }

    fn min_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.equipment.heat_limits().0)
    }

    fn max_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.equipment.heat_limits().1)
    }

    fn min_cool_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.equipment.cool_limits().0)
    }

    fn max_cool_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.equipment.cool_limits().1)
    }

    fn min_setpoint_dead_band(&self, _ctx: impl ReadContext) -> Result<i8, Error> {
        Ok(DEAD_BAND)
    }

    fn occupied_heating_setpoint(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.occupied().0)
    }

    fn occupied_cooling_setpoint(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.occupied().1)
    }

    fn unoccupied_heating_setpoint(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.unoccupied().0)
    }

    fn unoccupied_cooling_setpoint(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.unoccupied().1)
    }

    fn control_sequence_of_operation(&self, _ctx: impl ReadContext) -> Result<ControlSequenceOfOperationEnum, Error> {
        Ok(self.sequence())
    }

    fn system_mode(&self, _ctx: impl ReadContext) -> Result<SystemModeEnum, Error> {
        Ok(match self.nest.view().mode() {
            "heat" => SystemModeEnum::Heat,
            "cool" => SystemModeEnum::Cool,
            "heat_cool" => SystemModeEnum::Auto,
            _ => SystemModeEnum::Off,
        })
    }

    fn thermostat_running_state(&self, _ctx: impl ReadContext) -> Result<RelayStateBitmap, Error> {
        let view = self.nest.view();
        let mut state = RelayStateBitmap::empty();
        match view.state_str("action") {
            "heating" => state |= RelayStateBitmap::HEAT,
            "cooling" => state |= RelayStateBitmap::COOL,
            _ => {}
        }
        if view.state_flag("fan_running") {
            state |= RelayStateBitmap::FAN;
        }
        if view.state_flag("heat_stage_2") {
            state |= RelayStateBitmap::HEAT_STAGE_2;
        }
        if view.state_flag("cool_stage_2") {
            state |= RelayStateBitmap::COOL_STAGE_2;
        }
        Ok(state)
    }

    fn preset_types<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: ArrayAttributeRead<PresetTypeStructArrayBuilder<P>, PresetTypeStructBuilder<P>>,
    ) -> Result<P, Error> {
        // One preset type: user-defined (the unit's eco), named.
        macro_rules! one {
            ($b:expr) => {
                $b.preset_scenario(PresetScenarioEnum::UserDefined)?
                    .number_of_presets(1)?
                    .preset_type_features(PresetTypeFeaturesBitmap::SUPPORTS_NAMES)?
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

    fn number_of_presets(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        Ok(1)
    }

    fn presets<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: ArrayAttributeRead<PresetStructArrayBuilder<P>, PresetStructBuilder<P>>,
    ) -> Result<P, Error> {
        let (low, high) = self.unoccupied();
        let low = self.equipment.heats().then_some(low);
        let high = self.equipment.cools().then_some(high);
        macro_rules! eco {
            ($b:expr) => {
                $b.preset_handle(Nullable::some(Octets(ECO)))?
                    .preset_scenario(PresetScenarioEnum::UserDefined)?
                    .name(Some(Nullable::some("eco")))?
                    .cooling_setpoint(high)?
                    .heating_setpoint(low)?
                    .built_in(Nullable::some(true))?
                    .end()
            };
        }
        match builder {
            ArrayAttributeRead::ReadAll(array) => eco!(array.push()?)?.end(),
            ArrayAttributeRead::ReadOne(0, item) => eco!(item),
            ArrayAttributeRead::ReadOne(..) => Err(ErrorCode::ConstraintError.into()),
            ArrayAttributeRead::ReadNone(array) => array.end(),
        }
    }

    fn active_preset_handle<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: NullableBuilder<P, OctetsBuilder<P>>,
    ) -> Result<P, Error> {
        if self.nest.view().state_str("preset") == "eco" {
            builder.non_null()?.set(Octets(ECO))
        } else {
            builder.null()
        }
    }

    fn set_system_mode(&self, _ctx: impl WriteContext, value: SystemModeEnum) -> Result<(), Error> {
        // Only the modes the equipment has.
        let mode = match value {
            SystemModeEnum::Off => "off",
            SystemModeEnum::Heat if self.equipment.heats() => "heat",
            SystemModeEnum::Cool if self.equipment.cools() => "cool",
            SystemModeEnum::Auto if self.equipment == Equipment::Both => "heat_cool",
            _ => return Err(ErrorCode::ConstraintError.into()),
        };
        self.set("mode", mode)
    }

    fn set_occupied_heating_setpoint(&self, _ctx: impl WriteContext, value: i16) -> Result<(), Error> {
        self.set_occupied(true, value)
    }

    fn set_occupied_cooling_setpoint(&self, _ctx: impl WriteContext, value: i16) -> Result<(), Error> {
        self.set_occupied(false, value)
    }

    // The eco bounds sit inside the heating and cooling limits and keep the
    // two more than a dead band apart.
    fn set_unoccupied_heating_setpoint(&self, _ctx: impl WriteContext, value: i16) -> Result<(), Error> {
        if !(MIN..=ECO_HEAT_MAX).contains(&value) {
            return Err(ErrorCode::ConstraintError.into());
        }
        self.set("away_temperature_low", &degrees(value))
    }

    fn set_unoccupied_cooling_setpoint(&self, _ctx: impl WriteContext, value: i16) -> Result<(), Error> {
        if !(ECO_COOL_MIN..=MAX).contains(&value) {
            return Err(ErrorCode::ConstraintError.into());
        }
        self.set("away_temperature_high", &degrees(value))
    }

    fn set_control_sequence_of_operation(
        &self,
        _ctx: impl WriteContext,
        _value: ControlSequenceOfOperationEnum,
    ) -> Result<(), Error> {
        // The unit's wiring decides it: the spec has writes ignored.
        Ok(())
    }

    fn handle_setpoint_raise_lower(
        &self,
        _ctx: impl InvokeContext,
        request: thermostat::SetpointRaiseLowerRequest<'_>,
    ) -> Result<(), Error> {
        // Past its limits a setpoint stops at them, the spec's clamp; one
        // that comes within the dead band of the other moves the other.
        let delta = i16::from(request.amount()?) * 10; // 0.1 °C steps
        let view = self.nest.view();
        let reported = if view.mode() == "heat_cool" {
            view.shown_centi("target_low").is_some() && view.shown_centi("target_high").is_some()
        } else {
            view.shown_centi("target").is_some()
        };
        if !reported {
            return Ok(());
        }
        let mode = request.mode()?;
        let heat = mode != SetpointRaiseLowerModeEnum::Cool && self.equipment.heats();
        let cool = mode != SetpointRaiseLowerModeEnum::Heat && self.equipment.cools();
        if !heat && !cool {
            return Err(ErrorCode::InvalidCommand.into());
        }
        let ((heat_min, heat_max), (cool_min, cool_max)) = (self.equipment.heat_limits(), self.equipment.cool_limits());
        let (low, high) = self.regular_targets(&view);
        self.change(
            heat.then(|| (low + delta).clamp(heat_min, heat_max)),
            cool.then(|| (high + delta).clamp(cool_min, cool_max)),
        )
    }

    fn handle_set_active_preset_request(
        &self,
        _ctx: impl InvokeContext,
        request: thermostat::SetActivePresetRequestRequest<'_>,
    ) -> Result<(), Error> {
        match request.preset_handle()?.into_option() {
            None => self.set("preset", "none"),
            Some(handle) if handle.0 == ECO => self.set("preset", "eco"),
            Some(_) => Err(ErrorCode::NotFound.into()),
        }
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

    fn handle_set_active_schedule_request(
        &self,
        _ctx: impl InvokeContext,
        _request: thermostat::SetActiveScheduleRequestRequest<'_>,
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

    fn handle_atomic_request<P: TLVBuilderParent>(
        &self,
        _ctx: impl InvokeContext,
        _request: thermostat::AtomicRequestRequest<'_>,
        _response: thermostat::AtomicResponseBuilder<P>,
    ) -> Result<P, Error> {
        // The one preset is built in; there is nothing to edit atomically.
        Err(ErrorCode::CommandNotFound.into())
    }
}

/// A Matter temperature to the unit's half degrees, as the commands round it.
fn half(centi: i16) -> i16 {
    ((f64::from(centi) / 50.0).round() * 50.0) as i16
}

/// A Matter temperature as the °C argument the commands take.
fn degrees(centi: i16) -> String {
    format!("{}", f64::from(centi) / 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields;
    use crate::matter::tests::{ctx, field, nest};
    use rs_matter::tlv::{FromTLV, TLVArray, TLVElement, TLVTag, TLVWriteParent};
    use rs_matter::utils::storage::WriteBuf;
    use serde_json::json;

    /// A unit with the equipment its fields give.
    fn thermostat(dev: crate::json::Fields, sh: crate::json::Fields) -> Thermostat {
        let nest = nest(dev, sh);
        let equipment = Equipment::of(&nest);
        Thermostat::new(Dataver::new(0), nest, equipment)
    }

    fn code(result: Result<(), Error>) -> ErrorCode {
        result.expect_err("accepted").code()
    }

    fn shared(t: &Thermostat, key: &str) -> Option<f64> {
        field(&t.nest, "shared", key).as_f64()
    }

    fn active_preset(t: &Thermostat) -> Option<Vec<u8>> {
        let mut buf = [0u8; 64];
        let mut wb = WriteBuf::new(&mut buf);
        t.active_preset_handle(ctx(1), NullableBuilder::new(TLVWriteParent::new("test", &mut wb), &TLVTag::Anonymous))
            .unwrap();
        let len = wb.get_tail();
        let handle = Nullable::<Octets>::from_tlv(&TLVElement::new(&buf[..len])).unwrap();
        handle.into_option().map(|o| o.0.to_vec())
    }

    #[test]
    fn a_single_setpoint_mode_reads_its_temperatures() {
        let t = thermostat(
            fields! {"temperature_scale" => "C", "away_temperature_low" => 15.2, "away_temperature_high" => 28.0},
            fields! {"target_temperature_type" => "heat", "target_temperature" => 21.38377, "current_temperature" => 18.2,
            "can_heat" => true, "can_cool" => false, "hvac_heater_state" => true, "hvac_fan_state" => true},
        );
        assert_eq!(t.local_temperature(ctx(1)).unwrap(), Nullable::some(1820));
        assert_eq!(t.occupied_heating_setpoint(ctx(1)).unwrap(), 2138);
        assert_eq!(t.unoccupied_heating_setpoint(ctx(1)).unwrap(), 1520);
        assert_eq!(t.unoccupied_cooling_setpoint(ctx(1)).unwrap(), 2800);
        assert_eq!(t.system_mode(ctx(1)).unwrap(), SystemModeEnum::Heat);
        assert_eq!(t.control_sequence_of_operation(ctx(1)).unwrap(), ControlSequenceOfOperationEnum::HeatingOnly);
        assert_eq!(t.thermostat_running_state(ctx(1)).unwrap(), RelayStateBitmap::HEAT | RelayStateBitmap::FAN);
        assert_eq!(t.occupancy(ctx(1)).unwrap(), OccupancyBitmap::empty());
    }

    #[test]
    fn range_mode_reads_its_low_and_high() {
        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "range", "target_temperature" => 21.0,
            "target_temperature_low" => 19.0, "target_temperature_high" => 24.2,
            "can_heat" => true, "can_cool" => true, "hvac_ac_state" => true},
        );
        t.nest.bridge.occupancy.detected(chrono::Utc::now().timestamp());
        assert_eq!(t.occupied_heating_setpoint(ctx(1)).unwrap(), 1900);
        assert_eq!(t.occupied_cooling_setpoint(ctx(1)).unwrap(), 2420);
        assert_eq!(t.system_mode(ctx(1)).unwrap(), SystemModeEnum::Auto);
        assert_eq!(t.control_sequence_of_operation(ctx(1)).unwrap(), ControlSequenceOfOperationEnum::CoolingAndHeating);
        assert_eq!(t.thermostat_running_state(ctx(1)).unwrap(), RelayStateBitmap::COOL);
        assert_eq!(t.occupancy(ctx(1)).unwrap(), OccupancyBitmap::OCCUPIED);
    }

    #[test]
    fn features_follow_the_equipment() {
        use thermostat::AttributeId as A;
        let has = |equipment: Equipment, id: A| {
            let cluster = equipment.cluster();
            cluster.attributes.iter().any(|a| a.id == id as u32 && (cluster.with_attrs)(a, 0, cluster.feature_map))
        };
        let (heat, cool, auto) =
            (thermostat::Feature::HEATING, thermostat::Feature::COOLING, thermostat::Feature::AUTO_MODE);
        for (equipment, features, absent) in [
            (Equipment::Heat, heat, [A::OccupiedCoolingSetpoint, A::MinCoolSetpointLimit, A::MinSetpointDeadBand]),
            (Equipment::Cool, cool, [A::OccupiedHeatingSetpoint, A::MaxHeatSetpointLimit, A::MinSetpointDeadBand]),
        ] {
            let modes = equipment.cluster().feature_map & (heat | cool | auto).bits();
            assert_eq!(modes, features.bits(), "{equipment:?}");
            for id in absent {
                assert!(!has(equipment, id), "{equipment:?} {id:?}");
            }
            assert!(has(equipment, A::ControlSequenceOfOperation) && has(equipment, A::Presets));
        }
        assert_eq!(Equipment::Both.cluster().feature_map & (heat | cool | auto).bits(), (heat | cool | auto).bits());
        assert!(has(Equipment::Both, A::MinSetpointDeadBand) && has(Equipment::Both, A::OccupiedCoolingSetpoint));

        // A unit that only heats offers its whole range: no dead band.
        let t = thermostat(fields! {}, fields! {"can_heat" => true});
        assert_eq!(t.equipment, Equipment::Heat);
        assert_eq!(
            (t.min_heat_setpoint_limit(ctx(1)).unwrap(), t.max_heat_setpoint_limit(ctx(1)).unwrap()),
            (MIN, MAX)
        );
        assert_eq!(t.control_sequence_of_operation(ctx(1)).unwrap(), ControlSequenceOfOperationEnum::HeatingOnly);
    }

    #[test]
    fn heating_stays_a_dead_band_below_cooling() {
        let t = thermostat(fields! {"temperature_scale" => "C"}, fields! {});
        let gap = i16::from(t.min_setpoint_dead_band(ctx(1)).unwrap()) * 10;
        let pairs = [
            (t.abs_min_heat_setpoint_limit(ctx(1)), t.abs_min_cool_setpoint_limit(ctx(1))),
            (t.abs_max_heat_setpoint_limit(ctx(1)), t.abs_max_cool_setpoint_limit(ctx(1))),
            (t.min_heat_setpoint_limit(ctx(1)), t.min_cool_setpoint_limit(ctx(1))),
            (t.max_heat_setpoint_limit(ctx(1)), t.max_cool_setpoint_limit(ctx(1))),
        ];
        for (heat, cool) in pairs {
            assert!(heat.unwrap() <= cool.unwrap() - gap);
        }
        // Outside range mode the target is one side, the range's end the
        // other, moved clear of the target when it is not.
        for (mode, target, low, high, want) in [
            ("heat", 20.37, 18.0, 24.0, (2037, 2400)),
            ("heat", 24.0, 18.0, 24.5, (2400, 2550)),
            ("cool", 24.0, 18.0, 26.0, (1800, 2400)),
            ("cool", 19.0, 18.0, 26.0, (1750, 1900)),
            ("off", 18.0, 17.0, 19.0, (1800, 1950)),
            // Set on the unit past the limits: they read as the limits.
            ("heat", 31.5, 18.0, 24.0, (3050, 3200)),
            ("heat_cool", 0.0, 4.5, 35.0, (900, 3200)),
        ] {
            let t = thermostat(
                fields! {"temperature_scale" => "C"},
                fields! {"target_temperature_type" => if mode == "heat_cool" { "range" } else { mode },
                "target_temperature" => target, "target_temperature_low" => low, "target_temperature_high" => high},
            );
            let read = (t.occupied_heating_setpoint(ctx(1)).unwrap(), t.occupied_cooling_setpoint(ctx(1)).unwrap());
            assert_eq!(read, want, "{mode} {target}");
        }
    }

    #[test]
    fn second_stages_run_beside_the_first() {
        let t = thermostat(
            fields! {"has_x2_heat" => true},
            fields! {"target_temperature_type" => "heat", "can_heat" => true, "hvac_heater_state" => true,
            "hvac_heat_x2_state" => true},
        );
        assert_eq!(
            t.thermostat_running_state(ctx(1)).unwrap(),
            RelayStateBitmap::HEAT | RelayStateBitmap::HEAT_STAGE_2
        );
        let t = thermostat(
            fields! {"has_x2_cool" => true},
            fields! {"target_temperature_type" => "cool", "can_cool" => true, "hvac_cool_x2_state" => true},
        );
        assert_eq!(
            t.thermostat_running_state(ctx(1)).unwrap(),
            RelayStateBitmap::COOL | RelayStateBitmap::COOL_STAGE_2
        );
    }

    #[test]
    fn an_unreported_or_idle_unit_reads_as_nothing_running() {
        // Off: the equipment's relays are not what it is doing.
        let t = thermostat(
            fields! {},
            fields! {"target_temperature_type" => "off", "hvac_heater_state" => true,
            "can_cool" => true},
        );
        assert_eq!(t.system_mode(ctx(1)).unwrap(), SystemModeEnum::Off);
        assert_eq!(t.thermostat_running_state(ctx(1)).unwrap(), RelayStateBitmap::empty());
        assert_eq!(t.control_sequence_of_operation(ctx(1)).unwrap(), ControlSequenceOfOperationEnum::CoolingOnly);
        assert_eq!(t.local_temperature(ctx(1)).unwrap(), Nullable::none());
        assert_eq!(t.occupied_heating_setpoint(ctx(1)).unwrap(), 2000);
        assert_eq!(t.occupied_cooling_setpoint(ctx(1)).unwrap(), 2400);
        assert_eq!(t.unoccupied_heating_setpoint(ctx(1)).unwrap(), MIN);
        assert_eq!(t.unoccupied_cooling_setpoint(ctx(1)).unwrap(), MAX);
    }

    #[test]
    fn system_modes_written_read_back() {
        let t = thermostat(fields! {}, fields! {"target_temperature_type" => "off"});
        for mode in [SystemModeEnum::Heat, SystemModeEnum::Cool, SystemModeEnum::Auto, SystemModeEnum::Off] {
            t.set_system_mode(ctx(1), mode).unwrap();
            assert_eq!(t.system_mode(ctx(1)).unwrap(), mode);
        }
        for unsupported in [SystemModeEnum::EmergencyHeat, SystemModeEnum::FanOnly, SystemModeEnum::Dry] {
            assert_eq!(code(t.set_system_mode(ctx(1), unsupported)), ErrorCode::ConstraintError, "{unsupported:?}");
            assert_eq!(t.system_mode(ctx(1)).unwrap(), SystemModeEnum::Off);
        }
        // The equipment's wiring decides the sequence: a write is ignored.
        t.set_control_sequence_of_operation(ctx(1), ControlSequenceOfOperationEnum::HeatingOnly).unwrap();
        assert_eq!(t.control_sequence_of_operation(ctx(1)).unwrap(), ControlSequenceOfOperationEnum::CoolingAndHeating);
    }

    #[test]
    fn setpoint_writes_change_the_setpoint_the_mode_uses() {
        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "heat", "target_temperature" => 20.0,
            "target_temperature_low" => 18.0, "target_temperature_high" => 25.0},
        );
        let low_high = |t: &Thermostat| (shared(t, "target_temperature_low"), shared(t, "target_temperature_high"));
        let read = |t: &Thermostat| {
            (t.occupied_heating_setpoint(ctx(1)).unwrap(), t.occupied_cooling_setpoint(ctx(1)).unwrap())
        };
        // To the unit's half degrees: 21.26 °C is 21.5.
        t.set_occupied_heating_setpoint(ctx(1), 2126).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(21.5));
        // In heat, cooling is the range's high: the target stays.
        t.set_occupied_cooling_setpoint(ctx(1), 2400).unwrap();
        assert_eq!((shared(&t, "target_temperature"), low_high(&t)), (Some(21.5), (Some(18.0), Some(24.0))));
        assert_eq!(read(&t), (2150, 2400));
        // Within the dead band of the target, it moves the target.
        t.set_occupied_cooling_setpoint(ctx(1), 2200).unwrap();
        assert_eq!((shared(&t, "target_temperature"), low_high(&t)), (Some(20.5), (Some(18.0), Some(22.0))));
        assert_eq!(read(&t), (2050, 2200));

        // In cool, the reverse.
        t.set_system_mode(ctx(1), SystemModeEnum::Cool).unwrap();
        t.set_occupied_heating_setpoint(ctx(1), 2100).unwrap();
        assert_eq!((shared(&t, "target_temperature"), low_high(&t)), (Some(22.5), (Some(21.0), Some(22.0))));
        t.set_occupied_cooling_setpoint(ctx(1), 2600).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(26.0));
        assert_eq!(read(&t), (2100, 2600));

        t.set_system_mode(ctx(1), SystemModeEnum::Auto).unwrap();
        t.set_occupied_heating_setpoint(ctx(1), 1900).unwrap();
        t.set_occupied_cooling_setpoint(ctx(1), 2650).unwrap();
        assert_eq!(low_high(&t), (Some(19.0), Some(26.5)));
        assert_eq!(shared(&t, "target_temperature"), Some(26.0));
        assert_eq!(read(&t), (1900, 2650));

        t.set_unoccupied_heating_setpoint(ctx(1), 1220).unwrap();
        t.set_unoccupied_cooling_setpoint(ctx(1), 2900).unwrap();
        assert_eq!(field(&t.nest, "device", "away_temperature_low").as_f64(), Some(12.0));
        assert_eq!(field(&t.nest, "device", "away_temperature_high").as_f64(), Some(29.0));

        // Off on a unit that only cools: cooling is the target.
        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "off", "target_temperature" => 22.0, "can_cool" => true},
        );
        t.set_occupied_cooling_setpoint(ctx(1), 2400).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(24.0));
        assert_eq!(t.occupied_cooling_setpoint(ctx(1)).unwrap(), 2400);
    }

    #[test]
    fn setpoints_the_unit_cannot_take_are_refused() {
        let t = thermostat(fields! {}, fields! {"target_temperature_type" => "heat", "target_temperature" => 20.0});
        for centi in [i16::MIN, -500, 0, 400, 3600, i16::MAX] {
            assert_eq!(code(t.set_occupied_heating_setpoint(ctx(1), centi)), ErrorCode::ConstraintError, "{centi}");
            assert_eq!(code(t.set_unoccupied_cooling_setpoint(ctx(1), centi)), ErrorCode::ConstraintError, "{centi}");
        }
        assert_eq!(shared(&t, "target_temperature"), Some(20.0));
        assert_eq!(field(&t.nest, "device", "away_temperature_high"), serde_json::Value::Null);
    }

    #[test]
    fn setpoints_outside_the_advertised_limits_are_refused() {
        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "heat", "target_temperature" => 20.0},
        );
        // The unit takes 4.5-35 °C; Matter is told 9-30.5 to heat to,
        // 10.5-32 to cool to.
        for centi in [500, 850, 3100, 3250, 3500] {
            assert_eq!(code(t.set_occupied_heating_setpoint(ctx(1), centi)), ErrorCode::ConstraintError, "{centi}");
        }
        for centi in [500, 1000, 3250, 3500] {
            assert_eq!(code(t.set_occupied_cooling_setpoint(ctx(1), centi)), ErrorCode::ConstraintError, "{centi}");
        }
        assert_eq!(shared(&t, "target_temperature"), Some(20.0));
        t.set_occupied_heating_setpoint(ctx(1), MIN).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(9.0));
        t.set_occupied_heating_setpoint(ctx(1), 3050).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(30.5));
        t.set_system_mode(ctx(1), SystemModeEnum::Cool).unwrap();
        t.set_occupied_cooling_setpoint(ctx(1), 1050).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(10.5));
        t.set_occupied_cooling_setpoint(ctx(1), MAX).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(32.0));

        // Eco: 9-21 °C to heat to, 24-32 °C to cool to.
        for centi in [850, 2150, 2500] {
            assert_eq!(code(t.set_unoccupied_heating_setpoint(ctx(1), centi)), ErrorCode::ConstraintError, "{centi}");
        }
        for centi in [2000, 2350, 3250] {
            assert_eq!(code(t.set_unoccupied_cooling_setpoint(ctx(1), centi)), ErrorCode::ConstraintError, "{centi}");
        }
        assert_eq!(field(&t.nest, "device", "away_temperature_low"), serde_json::Value::Null);
        assert_eq!(field(&t.nest, "device", "away_temperature_high"), serde_json::Value::Null);
        t.set_unoccupied_heating_setpoint(ctx(1), 2100).unwrap();
        t.set_unoccupied_cooling_setpoint(ctx(1), 2400).unwrap();
        assert_eq!(field(&t.nest, "device", "away_temperature_low").as_f64(), Some(21.0));
        assert_eq!(field(&t.nest, "device", "away_temperature_high").as_f64(), Some(24.0));
    }

    #[test]
    fn range_setpoints_move_the_other_clear_of_the_dead_band() {
        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "range", "target_temperature_low" => 19.0,
            "target_temperature_high" => 24.0},
        );
        let low_high = |t: &Thermostat| (shared(t, "target_temperature_low"), shared(t, "target_temperature_high"));
        // 22.6 °C is 22.5 to the unit, clear of the high.
        t.set_occupied_heating_setpoint(ctx(1), 2260).unwrap();
        assert_eq!(low_high(&t), (Some(22.5), Some(24.0)));
        // The side written stays; the other moves.
        t.set_occupied_heating_setpoint(ctx(1), 2300).unwrap();
        assert_eq!(low_high(&t), (Some(23.0), Some(24.5)));
        t.set_occupied_cooling_setpoint(ctx(1), 2040).unwrap();
        assert_eq!(low_high(&t), (Some(19.0), Some(20.5)));

        // Rounded as the unit stores it: 22.3 °C is 22.5, too close to 23.8.
        let t = thermostat(
            fields! {"temperature_scale" => "F"},
            fields! {"target_temperature_type" => "range", "target_temperature_low" => 19.0,
            "target_temperature_high" => 23.8},
        );
        t.set_occupied_heating_setpoint(ctx(1), 2230).unwrap();
        assert_eq!(low_high(&t), (Some(22.5), Some(24.0)));
    }

    #[test]
    fn system_modes_the_equipment_lacks_are_refused() {
        use SystemModeEnum::{Auto, Cool, Heat, Off};
        for (can_heat, can_cool, refused) in [(true, false, [Cool, Auto]), (false, true, [Heat, Auto])] {
            let t = thermostat(
                fields! {},
                fields! {"target_temperature_type" => "off", "can_heat" => can_heat,
                "can_cool" => can_cool},
            );
            for mode in refused {
                assert_eq!(code(t.set_system_mode(ctx(1), mode)), ErrorCode::ConstraintError, "{mode:?}");
                assert_eq!(t.system_mode(ctx(1)).unwrap(), Off);
            }
            let allowed = if can_heat { Heat } else { Cool };
            t.set_system_mode(ctx(1), allowed).unwrap();
            assert_eq!(t.system_mode(ctx(1)).unwrap(), allowed);
        }
    }

    #[test]
    fn writes_before_the_unit_reported_are_refused() {
        let bridge = crate::bridge::tests::test_bridge();
        let t = Thermostat::new(Dataver::new(0), std::sync::Arc::new(Nest { bridge }), Equipment::Both);
        assert_eq!(code(t.set_system_mode(ctx(1), SystemModeEnum::Heat)), ErrorCode::ConstraintError);
        assert_eq!(code(t.set_occupied_heating_setpoint(ctx(1), 2000)), ErrorCode::ConstraintError);
    }

    fn raise_lower(t: &Thermostat, mode: SetpointRaiseLowerModeEnum, amount: i8) -> Result<(), Error> {
        let tlv = [0x15, 0x24, 0x00, mode as u8, 0x20, 0x01, amount as u8, 0x18];
        t.handle_setpoint_raise_lower(ctx(1), thermostat::SetpointRaiseLowerRequest::new(TLVElement::new(&tlv)))
    }

    #[test]
    fn setpoint_raise_lower_steps_in_tenths_of_a_degree() {
        use SetpointRaiseLowerModeEnum::{Both, Cool, Heat};
        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "range", "target_temperature_low" => 19.0,
            "target_temperature_high" => 24.0},
        );
        raise_lower(&t, Heat, 10).unwrap();
        assert_eq!(
            (shared(&t, "target_temperature_low"), shared(&t, "target_temperature_high")),
            (Some(20.0), Some(24.0))
        );
        raise_lower(&t, Cool, -15).unwrap();
        assert_eq!(
            (shared(&t, "target_temperature_low"), shared(&t, "target_temperature_high")),
            (Some(20.0), Some(22.5))
        );
        raise_lower(&t, Both, 5).unwrap();
        assert_eq!(
            (shared(&t, "target_temperature_low"), shared(&t, "target_temperature_high")),
            (Some(20.5), Some(23.0))
        );

        // In cool, the target is cooling.
        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "cool", "target_temperature" => 24.0},
        );
        raise_lower(&t, Cool, -20).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(22.0));

        // A unit that only heats has no cooling side to move.
        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "heat", "target_temperature" => 20.0, "can_heat" => true},
        );
        assert_eq!(raise_lower(&t, Cool, 10).unwrap_err().code(), ErrorCode::InvalidCommand);
        raise_lower(&t, Both, 10).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(21.0));
    }

    #[test]
    fn setpoint_raise_lower_stops_at_the_limits_and_the_dead_band() {
        use SetpointRaiseLowerModeEnum::{Both, Cool, Heat};
        let low_high = |t: &Thermostat| (shared(t, "target_temperature_low"), shared(t, "target_temperature_high"));
        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "heat", "target_temperature" => 29.5},
        );
        raise_lower(&t, Heat, 20).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(30.5));
        raise_lower(&t, Heat, -127).unwrap();
        raise_lower(&t, Heat, -127).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(9.0));
        // Set past the limit on the unit: it reads as the limit, and steps from there.
        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "heat", "target_temperature" => 31.5},
        );
        raise_lower(&t, Heat, 20).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(30.5));
        raise_lower(&t, Heat, -5).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(30.0));
        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "cool", "target_temperature" => 12.0},
        );
        raise_lower(&t, Cool, -30).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(10.5));

        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "range", "target_temperature_low" => 21.0,
            "target_temperature_high" => 23.5},
        );
        // Into the dead band: the side named moves, the other a dead band from it.
        raise_lower(&t, Heat, 20).unwrap();
        assert_eq!(low_high(&t), (Some(23.0), Some(24.5)));
        raise_lower(&t, Cool, -30).unwrap();
        assert_eq!(low_high(&t), (Some(20.0), Some(21.5)));
        // Both up against the top: the high stops at it, the low a dead band under.
        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "range", "target_temperature_low" => 30.0,
            "target_temperature_high" => 31.5},
        );
        raise_lower(&t, Both, 20).unwrap();
        assert_eq!(low_high(&t), (Some(30.5), Some(32.0)));
        let t = thermostat(
            fields! {"temperature_scale" => "C"},
            fields! {"target_temperature_type" => "range", "target_temperature_low" => 10.0,
            "target_temperature_high" => 11.5},
        );
        raise_lower(&t, Both, -20).unwrap();
        assert_eq!(low_high(&t), (Some(9.0), Some(10.5)));
    }

    #[test]
    fn eco_setpoints_after_preset_change() {
        let t = thermostat(
            fields! {"temperature_scale" => "C", "away_temperature_low" => 16,
            "away_temperature_high" => 29, "away_temperature_low_enabled" => true,
            "away_temperature_high_enabled" => true},
            fields! {"target_temperature_type" => "range", "target_temperature" => 22,
            "target_temperature_low" => 20, "target_temperature_high" => 24,
            "can_heat" => true, "can_cool" => true},
        );
        let targets = || (t.occupied_heating_setpoint(ctx(1)).unwrap(), t.occupied_cooling_setpoint(ctx(1)).unwrap());
        assert_eq!(targets(), (2000, 2400));
        t.set("preset", "eco").unwrap();
        assert_eq!(active_preset(&t).as_deref(), Some(ECO));
        assert_eq!(targets(), (1600, 2900));
        t.set("away_temperature_high_enabled", "off").unwrap();
        assert_eq!(targets(), (1600, 2400));
        t.set("preset", "none").unwrap();
        assert_eq!(targets(), (2000, 2400));
        t.set("mode", "cool").unwrap();
        t.set("away_temperature_high_enabled", "on").unwrap();
        t.set("preset", "eco").unwrap();
        assert_eq!(targets(), (2000, 2900));
        t.set_occupied_cooling_setpoint(ctx(1), 2500).unwrap();
        assert_eq!(active_preset(&t), None);
        assert_eq!(targets(), (2000, 2500));
    }

    #[test]
    fn eco_is_the_one_preset_with_the_eco_temperatures() {
        let t = thermostat(
            fields! {"temperature_scale" => "C", "away_temperature_low" => 15.0,
            "away_temperature_high" => 28.4, "eco" => json!({"mode": "schedule"})},
            fields! {"target_temperature_type" => "heat"},
        );
        assert_eq!(active_preset(&t), None);

        let select = |handle: &[u8]| {
            let mut tlv = vec![0x15];
            match handle {
                [] => tlv.extend([0x34, 0x00]),
                h => {
                    tlv.extend([0x30, 0x00, h.len() as u8]);
                    tlv.extend(h);
                }
            }
            tlv.push(0x18);
            t.handle_set_active_preset_request(
                ctx(1),
                thermostat::SetActivePresetRequestRequest::new(TLVElement::new(&tlv)),
            )
        };
        select(ECO).unwrap();
        assert_eq!(field(&t.nest, "device", "eco")["mode"], json!("manual-eco"));
        assert_eq!(active_preset(&t).as_deref(), Some(ECO));
        assert_eq!(code(select(b"away")), ErrorCode::NotFound);
        assert_eq!(field(&t.nest, "device", "eco")["mode"], json!("manual-eco"));
        select(&[]).unwrap();
        assert_eq!(field(&t.nest, "device", "eco")["mode"], json!("schedule"));
        assert_eq!(active_preset(&t), None);

        // The preset the active handle names, holding the eco temperatures.
        let mut buf = [0u8; 256];
        let mut wb = WriteBuf::new(&mut buf);
        let array = PresetStructArrayBuilder::new(TLVWriteParent::new("test", &mut wb), &TLVTag::Anonymous).unwrap();
        t.presets(ctx(1), ArrayAttributeRead::ReadAll(array)).unwrap();
        let len = wb.get_tail();
        let presets: TLVArray<'_, thermostat::PresetStruct<'_>> = TLVArray::new(TLVElement::new(&buf[..len])).unwrap();
        let presets: Vec<_> = presets.iter().map(Result::unwrap).collect();
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].preset_handle().unwrap().into_option().map(|h| h.0), Some(ECO));
        assert_eq!(presets[0].heating_setpoint().unwrap(), Some(1500));
        assert_eq!(presets[0].cooling_setpoint().unwrap(), Some(2840));
    }

    fn preset_setpoints(t: &Thermostat) -> (Option<i16>, Option<i16>) {
        let mut buf = [0u8; 256];
        let mut wb = WriteBuf::new(&mut buf);
        let array = PresetStructArrayBuilder::new(TLVWriteParent::new("test", &mut wb), &TLVTag::Anonymous).unwrap();
        t.presets(ctx(1), ArrayAttributeRead::ReadAll(array)).unwrap();
        let len = wb.get_tail();
        let presets: TLVArray<'_, thermostat::PresetStruct<'_>> = TLVArray::new(TLVElement::new(&buf[..len])).unwrap();
        let eco = presets.iter().next().unwrap().unwrap();
        (eco.heating_setpoint().unwrap(), eco.cooling_setpoint().unwrap())
    }

    #[test]
    fn eco_temperatures_past_the_limits_read_as_the_limits() {
        // MQTT and Zigbee take eco heat down to 4.5 °C.
        let t = thermostat(
            fields! {"temperature_scale" => "C", "away_temperature_low" => 4.5, "away_temperature_high" => 28.0},
            fields! {"target_temperature_type" => "heat"},
        );
        assert_eq!(t.unoccupied_heating_setpoint(ctx(1)).unwrap(), MIN);
        assert_eq!(preset_setpoints(&t), (Some(MIN), Some(2800)));
        // A unit that only heats has no cooling setpoint in its preset.
        let t = thermostat(
            fields! {"temperature_scale" => "C", "away_temperature_low" => 15.0, "away_temperature_high" => 28.0},
            fields! {"target_temperature_type" => "heat", "can_heat" => true},
        );
        assert_eq!(preset_setpoints(&t), (Some(1500), None));
    }

    #[test]
    fn a_setpoint_written_in_eco_leaves_eco() {
        let t = thermostat(
            fields! {"temperature_scale" => "C", "eco" => json!({"mode": "manual-eco"})},
            fields! {"target_temperature_type" => "heat", "target_temperature" => 20.0},
        );
        assert_eq!(active_preset(&t).as_deref(), Some(ECO));
        t.set_occupied_heating_setpoint(ctx(1), 2200).unwrap();
        assert_eq!(active_preset(&t), None);
        assert_eq!(field(&t.nest, "device", "eco")["mode"], json!("schedule"));
        assert_eq!(shared(&t, "target_temperature"), Some(22.0));
    }
}
