//! The Thermostat cluster on the thermostat endpoint, read from the unit's
//! buckets and written through the same commands as MQTT's set/ topics.
//!
//! Every Matter temperature is in hundredths of a degree Celsius. The
//! cluster advertises heating, cooling and auto on every unit; which of them
//! the unit's equipment can do goes in ControlSequenceOfOperation, which is
//! what controllers read the modes from. Eco is the one preset (PRES), and
//! the eco temperatures are also the unoccupied setpoints (OCC).

use std::sync::Arc;

use rs_matter::dm::clusters::decl::thermostat::{
    self, ClusterHandler, ControlSequenceOfOperationEnum, OccupancyBitmap, PresetScenarioEnum,
    PresetStructArrayBuilder, PresetStructBuilder, PresetTypeFeaturesBitmap, PresetTypeStructArrayBuilder,
    PresetTypeStructBuilder, RelayStateBitmap, SetpointRaiseLowerModeEnum, SystemModeEnum,
};
use rs_matter::dm::{ArrayAttributeRead, Cluster, Dataver, InvokeContext, ReadContext, WriteContext};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{Nullable, NullableBuilder, Octets, OctetsBuilder, TLVBuilderParent};
use rs_matter::with;

use super::{centi, failed, Nest};

/// The unit's setpoint range, as the MQTT climate entity has it.
const MIN: i16 = 900;
const MAX: i16 = 3200;
/// The least gap the unit keeps between a range's low and high, 0.1 °C.
const DEAD_BAND: i8 = 15;
/// The eco temperatures' own bounds, as the MQTT number entities have them:
/// the unoccupied heating setpoint at most, the cooling one at least.
const ECO_HEAT_MAX: i16 = 2100;
const ECO_COOL_MIN: i16 = 2400;
const ECO: &[u8] = b"eco";

pub struct Thermostat {
    dataver: Dataver,
    nest: Arc<Nest>,
}

impl Thermostat {
    pub fn new(dataver: Dataver, nest: Arc<Nest>) -> Self {
        Self { dataver, nest }
    }

    /// The setpoint a heating (or cooling) write changes, for the unit's mode.
    fn setpoint_command(&self, heating: bool) -> &'static str {
        match (self.nest.view().mode(), heating) {
            ("heat_cool", true) => "target_low",
            ("heat_cool", false) => "target_high",
            _ => "target",
        }
    }

    fn set(&self, name: &str, arg: &str) -> Result<(), Error> {
        self.nest.apply(name, arg).map_err(failed)
    }

    /// Writes an occupied setpoint inside the advertised limits and, in
    /// range mode, a dead band clear of the other one.
    fn set_occupied(&self, heating: bool, value: i16) -> Result<(), Error> {
        let view = self.nest.view();
        let gap = i16::from(DEAD_BAND) * 10;
        let clear = view.mode() != "heat_cool" || if heating {
            view.shown_centi("target_high").is_none_or(|high| value <= high - gap)
        } else {
            view.shown_centi("target_low").is_none_or(|low| value >= low + gap)
        };
        if !(MIN..=MAX).contains(&value) || !clear {
            return Err(ErrorCode::ConstraintError.into());
        }
        self.set(self.setpoint_command(heating), &degrees(value))
    }

    fn sequence(&self) -> ControlSequenceOfOperationEnum {
        let view = self.nest.view();
        match (view.shared_flag("can_heat"), view.shared_flag("can_cool")) {
            (true, false) => ControlSequenceOfOperationEnum::HeatingOnly,
            (false, true) => ControlSequenceOfOperationEnum::CoolingOnly,
            _ => ControlSequenceOfOperationEnum::CoolingAndHeating,
        }
    }
}

impl ClusterHandler for Thermostat {
    const CLUSTER: Cluster<'static> = thermostat::FULL_CLUSTER
        .with_features(
            thermostat::Feature::HEATING.bits()
                | thermostat::Feature::COOLING.bits()
                | thermostat::Feature::AUTO_MODE.bits()
                | thermostat::Feature::OCCUPANCY.bits()
                | thermostat::Feature::PRESETS.bits(),
        )
        // `required` is only what every thermostat has: what the features
        // make mandatory is listed too.
        .with_attrs(with!(
            required;
            thermostat::AttributeId::OutdoorTemperature
                | thermostat::AttributeId::Occupancy
                | thermostat::AttributeId::OccupiedHeatingSetpoint
                | thermostat::AttributeId::OccupiedCoolingSetpoint
                | thermostat::AttributeId::UnoccupiedHeatingSetpoint
                | thermostat::AttributeId::UnoccupiedCoolingSetpoint
                | thermostat::AttributeId::MinSetpointDeadBand
                | thermostat::AttributeId::PresetTypes
                | thermostat::AttributeId::NumberOfPresets
                | thermostat::AttributeId::ActivePresetHandle
                | thermostat::AttributeId::Presets
                | thermostat::AttributeId::AbsMinHeatSetpointLimit
                | thermostat::AttributeId::AbsMaxHeatSetpointLimit
                | thermostat::AttributeId::AbsMinCoolSetpointLimit
                | thermostat::AttributeId::AbsMaxCoolSetpointLimit
                | thermostat::AttributeId::MinHeatSetpointLimit
                | thermostat::AttributeId::MaxHeatSetpointLimit
                | thermostat::AttributeId::MinCoolSetpointLimit
                | thermostat::AttributeId::MaxCoolSetpointLimit
                | thermostat::AttributeId::ThermostatRunningState
        ))
        .with_cmds(with!(
            thermostat::CommandId::SetpointRaiseLower | thermostat::CommandId::SetActivePresetRequest
        ));

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
        let away = self.nest.view().shared_flag("auto_away");
        Ok(if away { OccupancyBitmap::empty() } else { OccupancyBitmap::OCCUPIED })
    }

    fn abs_min_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(MIN)
    }

    fn abs_max_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(MAX)
    }

    fn abs_min_cool_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(MIN)
    }

    fn abs_max_cool_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(MAX)
    }

    fn min_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(MIN)
    }

    fn max_heat_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(MAX)
    }

    fn min_cool_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(MIN)
    }

    fn max_cool_setpoint_limit(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(MAX)
    }

    fn min_setpoint_dead_band(&self, _ctx: impl ReadContext) -> Result<i8, Error> {
        Ok(DEAD_BAND)
    }

    fn occupied_heating_setpoint(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        let view = self.nest.view();
        let field = if view.mode() == "heat_cool" { "target_low" } else { "target" };
        Ok(view.shown_centi(field).unwrap_or(2000))
    }

    fn occupied_cooling_setpoint(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        let view = self.nest.view();
        let field = if view.mode() == "heat_cool" { "target_high" } else { "target" };
        Ok(view.shown_centi(field).unwrap_or(2400))
    }

    fn unoccupied_heating_setpoint(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.nest.view().shown_centi("away_temperature_low").unwrap_or(MIN))
    }

    fn unoccupied_cooling_setpoint(&self, _ctx: impl ReadContext) -> Result<i16, Error> {
        Ok(self.nest.view().shown_centi("away_temperature_high").unwrap_or(MAX))
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
        let view = self.nest.view();
        let (low, high) = (view.shown_centi("away_temperature_low"), view.shown_centi("away_temperature_high"));
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
        use ControlSequenceOfOperationEnum::{CoolingAndHeating, CoolingOnly, HeatingOnly};
        // Only the modes the control sequence allows.
        let sequence = self.sequence();
        let mode = match value {
            SystemModeEnum::Off => "off",
            SystemModeEnum::Heat if sequence != CoolingOnly => "heat",
            SystemModeEnum::Cool if sequence != HeatingOnly => "cool",
            SystemModeEnum::Auto if sequence == CoolingAndHeating => "heat_cool",
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

    // The heating and cooling limits are all Matter is told; the eco bounds
    // keep the two more than a dead band apart.
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

    fn set_control_sequence_of_operation(&self, _ctx: impl WriteContext, _value: ControlSequenceOfOperationEnum)
        -> Result<(), Error> {
        // The unit's wiring decides it.
        Err(ErrorCode::UnsupportedAccess.into())
    }

    fn handle_setpoint_raise_lower(&self, _ctx: impl InvokeContext,
                                   request: thermostat::SetpointRaiseLowerRequest<'_>) -> Result<(), Error> {
        // Past the limits or the dead band, a setpoint stops at them: the
        // spec's clamp, not an error.
        let delta = i16::from(request.amount()?) * 10; // 0.1 °C steps
        let view = self.nest.view();
        if view.mode() != "heat_cool" {
            return match view.shown_centi("target") {
                Some(t) => self.set("target", &degrees((t + delta).clamp(MIN, MAX))),
                None => Ok(()),
            };
        }
        let (Some(low), Some(high)) = (view.shown_centi("target_low"), view.shown_centi("target_high")) else {
            return Ok(());
        };
        let mode = request.mode()?;
        let (heat, cool) = (mode != SetpointRaiseLowerModeEnum::Cool, mode != SetpointRaiseLowerModeEnum::Heat);
        let mut new_low = if heat { (low + delta).clamp(MIN, MAX) } else { low };
        let mut new_high = if cool { (high + delta).clamp(MIN, MAX) } else { high };
        let gap = i16::from(DEAD_BAND) * 10;
        if new_high - new_low < gap {
            if delta > 0 { new_low = new_high - gap } else { new_high = new_low + gap }
        }
        if heat {
            self.set("target_low", &degrees(new_low))?;
        }
        if cool {
            self.set("target_high", &degrees(new_high))?;
        }
        Ok(())
    }

    fn handle_set_active_preset_request(&self, _ctx: impl InvokeContext,
                                        request: thermostat::SetActivePresetRequestRequest<'_>) -> Result<(), Error> {
        match request.preset_handle()?.into_option() {
            None => self.set("preset", "none"),
            Some(handle) if handle.0 == ECO => self.set("preset", "eco"),
            Some(_) => Err(ErrorCode::NotFound.into()),
        }
    }

    fn handle_set_weekly_schedule(&self, _ctx: impl InvokeContext, _request: thermostat::SetWeeklyScheduleRequest<'_>)
        -> Result<(), Error> {
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

    fn handle_set_active_schedule_request(&self, _ctx: impl InvokeContext,
                                          _request: thermostat::SetActiveScheduleRequestRequest<'_>)
        -> Result<(), Error> {
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

    fn handle_remove_thermostat_suggestion(&self, _ctx: impl InvokeContext,
                                           _request: thermostat::RemoveThermostatSuggestionRequest<'_>)
        -> Result<(), Error> {
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

    fn thermostat(dev: crate::json::Fields, sh: crate::json::Fields) -> Thermostat {
        Thermostat::new(Dataver::new(0), nest(dev, sh))
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
    fn a_single_setpoint_mode_reads_as_the_screen_shows_it() {
        let t = thermostat(
            fields!{"temperature_scale" => "C", "away_temperature_low" => 15.2, "away_temperature_high" => 28.0},
            fields!{"target_temperature_type" => "heat", "target_temperature" => 21.38377, "current_temperature" => 18.2,
                    "can_heat" => true, "can_cool" => false, "hvac_heater_state" => true, "hvac_fan_state" => true,
                    "auto_away" => 1});
        assert_eq!(t.local_temperature(ctx(1)).unwrap(), Nullable::some(1800));
        // Heating and cooling both show the one target.
        assert_eq!(t.occupied_heating_setpoint(ctx(1)).unwrap(), 2150);
        assert_eq!(t.occupied_cooling_setpoint(ctx(1)).unwrap(), 2150);
        assert_eq!(t.unoccupied_heating_setpoint(ctx(1)).unwrap(), 1500);
        assert_eq!(t.unoccupied_cooling_setpoint(ctx(1)).unwrap(), 2800);
        assert_eq!(t.system_mode(ctx(1)).unwrap(), SystemModeEnum::Heat);
        assert_eq!(t.control_sequence_of_operation(ctx(1)).unwrap(), ControlSequenceOfOperationEnum::HeatingOnly);
        assert_eq!(t.thermostat_running_state(ctx(1)).unwrap(), RelayStateBitmap::HEAT | RelayStateBitmap::FAN);
        assert_eq!(t.occupancy(ctx(1)).unwrap(), OccupancyBitmap::empty());
    }

    #[test]
    fn range_mode_reads_its_low_and_high() {
        let t = thermostat(
            fields!{"temperature_scale" => "C"},
            fields!{"target_temperature_type" => "range", "target_temperature" => 21.0,
                    "target_temperature_low" => 19.0, "target_temperature_high" => 24.2,
                    "can_heat" => true, "can_cool" => true, "hvac_ac_state" => true, "auto_away" => 0});
        assert_eq!(t.occupied_heating_setpoint(ctx(1)).unwrap(), 1900);
        assert_eq!(t.occupied_cooling_setpoint(ctx(1)).unwrap(), 2400);
        assert_eq!(t.system_mode(ctx(1)).unwrap(), SystemModeEnum::Auto);
        assert_eq!(t.control_sequence_of_operation(ctx(1)).unwrap(),
                   ControlSequenceOfOperationEnum::CoolingAndHeating);
        assert_eq!(t.thermostat_running_state(ctx(1)).unwrap(), RelayStateBitmap::COOL);
        assert_eq!(t.occupancy(ctx(1)).unwrap(), OccupancyBitmap::OCCUPIED);
    }

    #[test]
    fn an_unreported_or_idle_unit_reads_as_nothing_running() {
        // Off: the equipment's relays are not what it is doing.
        let t = thermostat(fields!{}, fields!{"target_temperature_type" => "off", "hvac_heater_state" => true,
                                              "can_cool" => true});
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
        let t = thermostat(fields!{}, fields!{"target_temperature_type" => "off"});
        for mode in [SystemModeEnum::Heat, SystemModeEnum::Cool, SystemModeEnum::Auto, SystemModeEnum::Off] {
            t.set_system_mode(ctx(1), mode).unwrap();
            assert_eq!(t.system_mode(ctx(1)).unwrap(), mode);
        }
        for unsupported in [SystemModeEnum::EmergencyHeat, SystemModeEnum::FanOnly, SystemModeEnum::Dry] {
            assert_eq!(code(t.set_system_mode(ctx(1), unsupported)), ErrorCode::ConstraintError, "{unsupported:?}");
            assert_eq!(t.system_mode(ctx(1)).unwrap(), SystemModeEnum::Off);
        }
        // The equipment's wiring decides the sequence.
        assert_eq!(code(t.set_control_sequence_of_operation(ctx(1), ControlSequenceOfOperationEnum::HeatingOnly)),
                   ErrorCode::UnsupportedAccess);
    }

    #[test]
    fn setpoint_writes_change_the_setpoint_the_mode_uses() {
        let t = thermostat(fields!{"temperature_scale" => "C"},
                           fields!{"target_temperature_type" => "heat", "target_temperature" => 20.0,
                                   "target_temperature_low" => 18.0, "target_temperature_high" => 25.0});
        // To the unit's half degrees: 21.26 °C is 21.5.
        t.set_occupied_heating_setpoint(ctx(1), 2126).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(21.5));
        t.set_occupied_cooling_setpoint(ctx(1), 2240).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(22.5));
        assert_eq!((shared(&t, "target_temperature_low"), shared(&t, "target_temperature_high")), (Some(18.0), Some(25.0)));

        t.set_system_mode(ctx(1), SystemModeEnum::Auto).unwrap();
        t.set_occupied_heating_setpoint(ctx(1), 1900).unwrap();
        t.set_occupied_cooling_setpoint(ctx(1), 2650).unwrap();
        assert_eq!((shared(&t, "target_temperature_low"), shared(&t, "target_temperature_high")), (Some(19.0), Some(26.5)));
        assert_eq!(shared(&t, "target_temperature"), Some(22.5));
        assert_eq!((t.occupied_heating_setpoint(ctx(1)).unwrap(), t.occupied_cooling_setpoint(ctx(1)).unwrap()),
                   (1900, 2650));

        t.set_unoccupied_heating_setpoint(ctx(1), 1220).unwrap();
        t.set_unoccupied_cooling_setpoint(ctx(1), 2900).unwrap();
        assert_eq!(field(&t.nest, "device", "away_temperature_low").as_f64(), Some(12.0));
        assert_eq!(field(&t.nest, "device", "away_temperature_high").as_f64(), Some(29.0));
    }

    #[test]
    fn setpoints_the_unit_cannot_take_are_refused() {
        let t = thermostat(fields!{}, fields!{"target_temperature_type" => "heat", "target_temperature" => 20.0});
        for centi in [i16::MIN, -500, 0, 400, 3600, i16::MAX] {
            assert_eq!(code(t.set_occupied_heating_setpoint(ctx(1), centi)), ErrorCode::ConstraintError, "{centi}");
            assert_eq!(code(t.set_unoccupied_cooling_setpoint(ctx(1), centi)), ErrorCode::ConstraintError, "{centi}");
        }
        assert_eq!(shared(&t, "target_temperature"), Some(20.0));
        assert_eq!(field(&t.nest, "device", "away_temperature_high"), serde_json::Value::Null);
    }

    #[test]
    fn setpoints_outside_the_advertised_limits_are_refused() {
        let t = thermostat(fields!{"temperature_scale" => "C"},
                           fields!{"target_temperature_type" => "heat", "target_temperature" => 20.0});
        // The unit takes 4.5-35 °C; Matter is told 9-32.
        for centi in [500, 850, 3250, 3500] {
            assert_eq!(code(t.set_occupied_heating_setpoint(ctx(1), centi)), ErrorCode::ConstraintError, "{centi}");
            assert_eq!(code(t.set_occupied_cooling_setpoint(ctx(1), centi)), ErrorCode::ConstraintError, "{centi}");
        }
        assert_eq!(shared(&t, "target_temperature"), Some(20.0));
        t.set_occupied_heating_setpoint(ctx(1), MIN).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(9.0));
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
    fn range_setpoints_keep_the_dead_band() {
        let t = thermostat(fields!{"temperature_scale" => "C"},
                           fields!{"target_temperature_type" => "range", "target_temperature_low" => 19.0,
                                   "target_temperature_high" => 24.0});
        assert_eq!(code(t.set_occupied_heating_setpoint(ctx(1), 2260)), ErrorCode::ConstraintError);
        assert_eq!(code(t.set_occupied_cooling_setpoint(ctx(1), 2040)), ErrorCode::ConstraintError);
        assert_eq!((shared(&t, "target_temperature_low"), shared(&t, "target_temperature_high")), (Some(19.0), Some(24.0)));
        t.set_occupied_heating_setpoint(ctx(1), 2250).unwrap();
        assert_eq!(code(t.set_occupied_cooling_setpoint(ctx(1), 2350)), ErrorCode::ConstraintError);
        t.set_occupied_cooling_setpoint(ctx(1), 2400).unwrap();
        assert_eq!((shared(&t, "target_temperature_low"), shared(&t, "target_temperature_high")), (Some(22.5), Some(24.0)));
    }

    #[test]
    fn system_modes_the_equipment_lacks_are_refused() {
        use SystemModeEnum::{Auto, Cool, Heat, Off};
        for (can_heat, can_cool, refused) in [(true, false, [Cool, Auto]), (false, true, [Heat, Auto])] {
            let t = thermostat(fields!{}, fields!{"target_temperature_type" => "off", "can_heat" => can_heat,
                                                  "can_cool" => can_cool});
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
        let t = Thermostat::new(Dataver::new(0), std::sync::Arc::new(Nest { bridge }));
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
        let t = thermostat(fields!{"temperature_scale" => "C"},
                           fields!{"target_temperature_type" => "range", "target_temperature_low" => 19.0,
                                   "target_temperature_high" => 24.0});
        raise_lower(&t, Heat, 10).unwrap();
        assert_eq!((shared(&t, "target_temperature_low"), shared(&t, "target_temperature_high")), (Some(20.0), Some(24.0)));
        raise_lower(&t, Cool, -15).unwrap();
        assert_eq!((shared(&t, "target_temperature_low"), shared(&t, "target_temperature_high")), (Some(20.0), Some(22.5)));
        raise_lower(&t, Both, 5).unwrap();
        assert_eq!((shared(&t, "target_temperature_low"), shared(&t, "target_temperature_high")), (Some(20.5), Some(23.0)));

        // One setpoint: the target moves whichever the request names.
        let t = thermostat(fields!{"temperature_scale" => "C"},
                           fields!{"target_temperature_type" => "cool", "target_temperature" => 24.0});
        raise_lower(&t, Both, -20).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(22.0));
    }

    #[test]
    fn setpoint_raise_lower_stops_at_the_limits_and_the_dead_band() {
        use SetpointRaiseLowerModeEnum::{Both, Cool, Heat};
        let low_high = |t: &Thermostat| (shared(t, "target_temperature_low"), shared(t, "target_temperature_high"));
        let t = thermostat(fields!{"temperature_scale" => "C"},
                           fields!{"target_temperature_type" => "heat", "target_temperature" => 31.5});
        raise_lower(&t, Heat, 20).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(32.0));
        raise_lower(&t, Heat, -127).unwrap();
        raise_lower(&t, Heat, -127).unwrap();
        assert_eq!(shared(&t, "target_temperature"), Some(9.0));

        let t = thermostat(fields!{"temperature_scale" => "C"},
                           fields!{"target_temperature_type" => "range", "target_temperature_low" => 21.0,
                                   "target_temperature_high" => 23.5});
        raise_lower(&t, Heat, 20).unwrap();
        assert_eq!(low_high(&t), (Some(22.0), Some(23.5)));
        raise_lower(&t, Cool, -30).unwrap();
        assert_eq!(low_high(&t), (Some(22.0), Some(23.5)));
        // Both up against the top: the high stops at it, the low a dead band under.
        let t = thermostat(fields!{"temperature_scale" => "C"},
                           fields!{"target_temperature_type" => "range", "target_temperature_low" => 30.0,
                                   "target_temperature_high" => 31.5});
        raise_lower(&t, Both, 20).unwrap();
        assert_eq!(low_high(&t), (Some(30.5), Some(32.0)));
        let t = thermostat(fields!{"temperature_scale" => "C"},
                           fields!{"target_temperature_type" => "range", "target_temperature_low" => 10.0,
                                   "target_temperature_high" => 11.5});
        raise_lower(&t, Both, -20).unwrap();
        assert_eq!(low_high(&t), (Some(9.0), Some(10.5)));
    }

    #[test]
    fn eco_is_the_one_preset_with_the_eco_temperatures() {
        let t = thermostat(fields!{"temperature_scale" => "C", "away_temperature_low" => 15.0,
                                   "away_temperature_high" => 28.4, "eco" => json!({"mode": "schedule"})},
                           fields!{"target_temperature_type" => "heat"});
        assert_eq!(active_preset(&t), None);

        let select = |handle: &[u8]| {
            let mut tlv = vec![0x15];
            match handle {
                [] => tlv.extend([0x34, 0x00]),
                h => { tlv.extend([0x30, 0x00, h.len() as u8]); tlv.extend(h); }
            }
            tlv.push(0x18);
            t.handle_set_active_preset_request(ctx(1),
                thermostat::SetActivePresetRequestRequest::new(TLVElement::new(&tlv)))
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
        assert_eq!(presets[0].cooling_setpoint().unwrap(), Some(2850));
    }
}
