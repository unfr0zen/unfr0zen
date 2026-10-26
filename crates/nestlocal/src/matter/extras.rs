//! Everything beside the Thermostat cluster, each on the endpoint Home
//! Assistant reads it from: humidity and the display's °C/°F on the
//! thermostat's own; the backplate temperature, the battery, the fan, the fan
//! timer and the unit's switches on endpoints of their own, each named by a
//! fixed label (ha_entitylabel, which Home Assistant names the test vendor's
//! entities by).

use std::sync::Arc;

use rs_matter::dm::clusters::decl::globals::{LabelStructArrayBuilder, LabelStructBuilder};
use rs_matter::dm::clusters::decl::{
    fan_control, fixed_label, mode_select, on_off, power_source, relative_humidity_measurement,
    temperature_measurement, thermostat_user_interface_configuration as ui_config,
};
use rs_matter::dm::{ArrayAttributeRead, Cluster, Dataver, InvokeContext, ReadContext, WriteContext};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{Nullable, TLVBuilderParent, ToTLVArrayBuilder, ToTLVBuilder, Utf8StrBuilder};
use rs_matter::with;

use super::{failed, Nest, THERMOSTAT};

pub const BACKPLATE: u16 = 2;
pub const BATTERY: u16 = 3;
pub const FAN: u16 = 4;
pub const FAN_TIMER: u16 = 5;
/// The first switch's endpoint; the others follow.
pub const SWITCHES: u16 = 10;

/// The fan timer's choices, minutes, as MQTT's select has them.
const FAN_TIMER_MINUTES: [u32; 8] = [15, 30, 45, 60, 120, 240, 480, 720];

/// A switch: its endpoint, the command (and device field) it sets, its name.
#[derive(Clone, Copy)]
pub struct Switch {
    pub endpoint: u16,
    pub key: &'static str,
    pub name: &'static str,
}

/// The switches this unit's equipment has, as MQTT discovery offers them.
pub fn switches(nest: &Nest) -> Vec<Switch> {
    let view = nest.view();
    let mut all = vec![
        ("learning_mode", "Auto-Schedule"),
        ("auto_away_enable", "Home/Away Assist"),
        ("sunlight_correction_enabled", "Sunblock"),
        ("preconditioning_enabled", "Early-On"),
    ];
    if view.shared_flag("can_heat") {
        all.push(("away_temperature_low_enabled", "Eco heat limit"));
    }
    if view.shared_flag("can_cool") {
        all.push(("away_temperature_high_enabled", "Eco cool limit"));
    }
    if view.device_flag("has_humidifier") || view.device_flag("has_dehumidifier") {
        all.push(("target_humidity_enabled", "Humidity control"));
    }
    all.into_iter().zip(SWITCHES..).map(|((key, name), endpoint)| Switch { endpoint, key, name }).collect()
}

macro_rules! dataver {
    () => {
        fn dataver(&self) -> u32 {
            self.dataver.get()
        }

        fn dataver_changed(&self) {
            self.dataver.changed();
        }
    };
}

/// The names of the endpoints that are not the thermostat.
pub struct Labels {
    pub dataver: Dataver,
    pub names: Vec<(u16, &'static str)>,
}

impl fixed_label::ClusterHandler for Labels {
    const CLUSTER: Cluster<'static> = fixed_label::FULL_CLUSTER.with_attrs(with!(required));

    dataver!();

    fn label_list<P: TLVBuilderParent>(
        &self,
        ctx: impl ReadContext,
        builder: ArrayAttributeRead<LabelStructArrayBuilder<P>, LabelStructBuilder<P>>,
    ) -> Result<P, Error> {
        let endpoint = ctx.attr().endpoint_id;
        let name = self.names.iter().find(|(e, _)| *e == endpoint).map(|(_, n)| *n);
        match (builder, name) {
            (ArrayAttributeRead::ReadAll(array), Some(name)) => {
                array.push()?.label("ha_entitylabel")?.value(name)?.end()?.end()
            }
            (ArrayAttributeRead::ReadAll(array), None) => array.end(),
            (ArrayAttributeRead::ReadOne(0, item), Some(name)) => item.label("ha_entitylabel")?.value(name)?.end(),
            (ArrayAttributeRead::ReadOne(..), _) => Err(ErrorCode::ConstraintError.into()),
            (ArrayAttributeRead::ReadNone(array), _) => array.end(),
        }
    }
}

/// The room's humidity, on the thermostat's endpoint (the climate entity's
/// current humidity, and a sensor).
pub struct Humidity {
    pub dataver: Dataver,
    pub nest: Arc<Nest>,
}

impl relative_humidity_measurement::ClusterHandler for Humidity {
    const CLUSTER: Cluster<'static> = relative_humidity_measurement::FULL_CLUSTER.with_attrs(with!(required));

    dataver!();

    fn measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        let humidity = self.nest.view().device_number("current_humidity");
        Ok(Nullable::new(humidity.map(|h| (h * 100.0).round() as u16)))
    }

    fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(0))
    }

    fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::some(10000))
    }
}

/// The unit's display in °C or °F.
pub struct Display {
    pub dataver: Dataver,
    pub nest: Arc<Nest>,
}

impl ui_config::ClusterHandler for Display {
    const CLUSTER: Cluster<'static> = ui_config::FULL_CLUSTER.with_attrs(with!(required));

    dataver!();

    fn temperature_display_mode(&self, _ctx: impl ReadContext) -> Result<ui_config::TemperatureDisplayModeEnum, Error> {
        Ok(if self.nest.view().device_str("temperature_scale") == "F" {
            ui_config::TemperatureDisplayModeEnum::Fahrenheit
        } else {
            ui_config::TemperatureDisplayModeEnum::Celsius
        })
    }

    fn set_temperature_display_mode(&self, _ctx: impl WriteContext, value: ui_config::TemperatureDisplayModeEnum)
        -> Result<(), Error> {
        let scale = match value {
            ui_config::TemperatureDisplayModeEnum::Fahrenheit => "F",
            ui_config::TemperatureDisplayModeEnum::Celsius => "C",
        };
        self.nest.apply("temperature_scale", scale).map_err(failed)
    }

    fn keypad_lockout(&self, _ctx: impl ReadContext) -> Result<ui_config::KeypadLockoutEnum, Error> {
        Ok(ui_config::KeypadLockoutEnum::NoLockout)
    }

    fn set_keypad_lockout(&self, _ctx: impl WriteContext, _value: ui_config::KeypadLockoutEnum) -> Result<(), Error> {
        Err(ErrorCode::UnsupportedAccess.into())
    }
}

/// The backplate's own temperature sensor.
pub struct Backplate {
    pub dataver: Dataver,
    pub nest: Arc<Nest>,
}

impl temperature_measurement::ClusterHandler for Backplate {
    const CLUSTER: Cluster<'static> = temperature_measurement::FULL_CLUSTER.with_attrs(with!(required));

    dataver!();

    fn measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::new(self.nest.view().device_centi("backplate_temperature")))
    }

    fn min_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::some(-1000))
    }

    fn max_measured_value(&self, _ctx: impl ReadContext) -> Result<Nullable<i16>, Error> {
        Ok(Nullable::some(6000))
    }
}

/// The display's battery, charged from the wiring: its voltage.
pub struct Battery {
    pub dataver: Dataver,
    pub nest: Arc<Nest>,
}

impl power_source::ClusterHandler for Battery {
    const CLUSTER: Cluster<'static> = power_source::FULL_CLUSTER
        .with_features(power_source::Feature::BATTERY.bits())
        .with_attrs(with!(
            required;
            power_source::AttributeId::BatVoltage
                | power_source::AttributeId::BatChargeLevel
                | power_source::AttributeId::BatReplacementNeeded
                | power_source::AttributeId::BatReplaceability
        ));

    dataver!();

    fn status(&self, _ctx: impl ReadContext) -> Result<power_source::PowerSourceStatusEnum, Error> {
        Ok(power_source::PowerSourceStatusEnum::Active)
    }

    fn order(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        Ok(0)
    }

    fn description<P: TLVBuilderParent>(&self, _ctx: impl ReadContext, builder: Utf8StrBuilder<P>) -> Result<P, Error> {
        builder.set("Battery")
    }

    /// It powers the thermostat.
    fn endpoint_list<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: ArrayAttributeRead<ToTLVArrayBuilder<P, u16>, ToTLVBuilder<P, u16>>,
    ) -> Result<P, Error> {
        match builder {
            ArrayAttributeRead::ReadAll(array) => array.push(&THERMOSTAT)?.end(),
            ArrayAttributeRead::ReadOne(0, item) => item.set(&THERMOSTAT),
            ArrayAttributeRead::ReadOne(..) => Err(ErrorCode::ConstraintError.into()),
            ArrayAttributeRead::ReadNone(array) => array.end(),
        }
    }

    fn bat_voltage(&self, _ctx: impl ReadContext) -> Result<Nullable<u32>, Error> {
        let volts = self.nest.view().device_number("battery_level");
        Ok(Nullable::new(volts.map(|v| (v * 1000.0).round() as u32)))
    }

    fn bat_charge_level(&self, _ctx: impl ReadContext) -> Result<power_source::BatChargeLevelEnum, Error> {
        // The unit runs on its wiring; under 3.6 V it starts to shut things down.
        Ok(match self.nest.view().device_number("battery_level") {
            Some(v) if v < 3.5 => power_source::BatChargeLevelEnum::Critical,
            Some(v) if v < 3.6 => power_source::BatChargeLevelEnum::Warning,
            _ => power_source::BatChargeLevelEnum::OK,
        })
    }

    fn bat_replacement_needed(&self, _ctx: impl ReadContext) -> Result<bool, Error> {
        Ok(false)
    }

    fn bat_replaceability(&self, _ctx: impl ReadContext) -> Result<power_source::BatReplaceabilityEnum, Error> {
        Ok(power_source::BatReplaceabilityEnum::NotReplaceable)
    }
}

/// The fan: on runs it for the fan timer's duration; off leaves it to the
/// heating and cooling (MQTT's fan mode on and auto).
pub struct Fan {
    pub dataver: Dataver,
    pub nest: Arc<Nest>,
}

impl Fan {
    fn on(&self) -> bool {
        self.nest.view().state_str("fan_mode") == "on"
    }

    fn set(&self, on: bool) -> Result<(), Error> {
        self.nest.apply("fan_mode", if on { "on" } else { "auto" }).map_err(failed)
    }
}

impl fan_control::ClusterHandler for Fan {
    const CLUSTER: Cluster<'static> = fan_control::FULL_CLUSTER.with_attrs(with!(required));

    dataver!();

    fn fan_mode(&self, _ctx: impl ReadContext) -> Result<fan_control::FanModeEnum, Error> {
        Ok(if self.on() { fan_control::FanModeEnum::High } else { fan_control::FanModeEnum::Off })
    }

    fn set_fan_mode(&self, _ctx: impl WriteContext, value: fan_control::FanModeEnum) -> Result<(), Error> {
        // OffHigh, and On, which the spec makes the highest speed.
        match value {
            fan_control::FanModeEnum::Off => self.set(false),
            fan_control::FanModeEnum::High | fan_control::FanModeEnum::On => self.set(true),
            _ => Err(ErrorCode::ConstraintError.into()),
        }
    }

    fn fan_mode_sequence(&self, _ctx: impl ReadContext) -> Result<fan_control::FanModeSequenceEnum, Error> {
        Ok(fan_control::FanModeSequenceEnum::OffHigh)
    }

    fn percent_setting(&self, _ctx: impl ReadContext) -> Result<Nullable<u8>, Error> {
        Ok(Nullable::some(if self.on() { 100 } else { 0 }))
    }

    fn set_percent_setting(&self, _ctx: impl WriteContext, value: Nullable<u8>) -> Result<(), Error> {
        self.set(value.into_option().is_some_and(|p| p > 0))
    }

    fn percent_current(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        Ok(if self.nest.view().state_flag("fan_running") { 100 } else { 0 })
    }

    fn handle_step(&self, _ctx: impl InvokeContext, _request: fan_control::StepRequest<'_>) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }
}

/// How long the fan runs when turned on.
pub struct FanTimer {
    pub dataver: Dataver,
    pub nest: Arc<Nest>,
}

impl mode_select::ClusterHandler for FanTimer {
    const CLUSTER: Cluster<'static> = mode_select::FULL_CLUSTER
        .with_attrs(with!(required))
        .with_cmds(with!(mode_select::CommandId::ChangeToMode));

    dataver!();

    fn description<P: TLVBuilderParent>(&self, _ctx: impl ReadContext, builder: Utf8StrBuilder<P>) -> Result<P, Error> {
        builder.set("Fan timer duration")
    }

    fn standard_namespace(&self, _ctx: impl ReadContext) -> Result<Nullable<u16>, Error> {
        Ok(Nullable::none())
    }

    fn supported_modes<P: TLVBuilderParent>(
        &self,
        _ctx: impl ReadContext,
        builder: ArrayAttributeRead<mode_select::ModeOptionStructArrayBuilder<P>, mode_select::ModeOptionStructBuilder<P>>,
    ) -> Result<P, Error> {
        macro_rules! option {
            ($b:expr, $i:expr, $minutes:expr) => {
                $b.label(&format!("{} min", $minutes))?.mode($i as u8)?.semantic_tags()?.end()?.end()
            };
        }
        match builder {
            ArrayAttributeRead::ReadAll(mut array) => {
                for (i, minutes) in FAN_TIMER_MINUTES.iter().enumerate() {
                    array = option!(array.push()?, i, minutes)?;
                }
                array.end()
            }
            ArrayAttributeRead::ReadOne(i, item) if (i as usize) < FAN_TIMER_MINUTES.len() => {
                option!(item, i, FAN_TIMER_MINUTES[i as usize])
            }
            ArrayAttributeRead::ReadOne(..) => Err(ErrorCode::ConstraintError.into()),
            ArrayAttributeRead::ReadNone(array) => array.end(),
        }
    }

    fn current_mode(&self, _ctx: impl ReadContext) -> Result<u8, Error> {
        let minutes = self.nest.view().device_number("fan_timer_duration").map_or(15.0, |s| s / 60.0);
        let nearest = FAN_TIMER_MINUTES.iter().enumerate()
            .min_by_key(|(_, m)| ((**m as f64) - minutes).abs() as u64)
            .map_or(0, |(i, _)| i);
        Ok(nearest as u8)
    }

    fn handle_change_to_mode(&self, _ctx: impl InvokeContext, request: mode_select::ChangeToModeRequest<'_>)
        -> Result<(), Error> {
        let minutes = FAN_TIMER_MINUTES.get(request.new_mode()? as usize).ok_or(ErrorCode::ConstraintError)?;
        self.nest.apply("fan_timer_duration", &minutes.to_string()).map_err(failed)
    }
}

/// The unit's on/off settings, a switch each.
pub struct Switches {
    pub dataver: Dataver,
    pub nest: Arc<Nest>,
    pub all: Vec<Switch>,
}

impl Switches {
    fn key(&self, endpoint: u16) -> Result<&'static str, Error> {
        self.all.iter().find(|s| s.endpoint == endpoint).map(|s| s.key).ok_or_else(|| ErrorCode::NotFound.into())
    }

    fn set(&self, endpoint: u16, on: Option<bool>) -> Result<(), Error> {
        let key = self.key(endpoint)?;
        let on = on.unwrap_or_else(|| !self.nest.view().device_flag(key));
        self.nest.apply(key, if on { "ON" } else { "OFF" }).map_err(failed)
    }
}

impl on_off::ClusterHandler for Switches {
    const CLUSTER: Cluster<'static> = on_off::FULL_CLUSTER
        .with_attrs(with!(required))
        .with_cmds(with!(on_off::CommandId::Off | on_off::CommandId::On | on_off::CommandId::Toggle));

    dataver!();

    fn on_off(&self, ctx: impl ReadContext) -> Result<bool, Error> {
        Ok(self.nest.view().device_flag(self.key(ctx.attr().endpoint_id)?))
    }

    fn handle_on(&self, ctx: impl InvokeContext) -> Result<(), Error> {
        self.set(ctx.cmd().endpoint_id, Some(true))
    }

    fn handle_off(&self, ctx: impl InvokeContext) -> Result<(), Error> {
        self.set(ctx.cmd().endpoint_id, Some(false))
    }

    fn handle_toggle(&self, ctx: impl InvokeContext) -> Result<(), Error> {
        self.set(ctx.cmd().endpoint_id, None)
    }

    fn handle_off_with_effect(&self, _ctx: impl InvokeContext, _request: on_off::OffWithEffectRequest<'_>)
        -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    fn handle_on_with_recall_global_scene(&self, _ctx: impl InvokeContext) -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }

    fn handle_on_with_timed_off(&self, _ctx: impl InvokeContext, _request: on_off::OnWithTimedOffRequest<'_>)
        -> Result<(), Error> {
        Err(ErrorCode::CommandNotFound.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields;
    use crate::json::Fields;
    use crate::matter::tests::{ctx, field, nest};
    use rs_matter::dm::clusters::decl::globals::LabelStruct;
    use fan_control::ClusterHandler as _;
    use fixed_label::ClusterHandler as _;
    use mode_select::ClusterHandler as _;
    use on_off::ClusterHandler as _;
    use power_source::ClusterHandler as _;
    use relative_humidity_measurement::ClusterHandler as _;
    use temperature_measurement::ClusterHandler as _;
    use ui_config::ClusterHandler as _;
    use rs_matter::tlv::{TLVArray, TLVElement, TLVTag, TLVWriteParent};
    use rs_matter::utils::storage::WriteBuf;
    use serde_json::Value;

    fn code(result: Result<(), Error>) -> ErrorCode {
        result.expect_err("accepted").code()
    }

    #[test]
    fn switches_follow_the_equipment_on_consecutive_endpoints() {
        let keys = |dev: Fields, sh: Fields| {
            switches(&nest(dev, sh)).iter().map(|s| (s.endpoint, s.key)).collect::<Vec<_>>()
        };
        let common = [(10, "learning_mode"), (11, "auto_away_enable"), (12, "sunlight_correction_enabled"),
                      (13, "preconditioning_enabled")];
        assert_eq!(keys(fields!{}, fields!{}), common);
        assert_eq!(keys(fields!{}, fields!{"can_cool" => true})[4..], [(14, "away_temperature_high_enabled")]);
        assert_eq!(keys(fields!{"has_dehumidifier" => true}, fields!{"can_heat" => true, "can_cool" => true})[4..],
                   [(14, "away_temperature_low_enabled"), (15, "away_temperature_high_enabled"),
                    (16, "target_humidity_enabled")]);
    }

    #[test]
    fn each_switch_sets_its_own_setting() {
        let unit = nest(fields!{"learning_mode" => true, "auto_away_enable" => false}, fields!{});
        let s = Switches { dataver: Dataver::new(0), nest: unit.clone(), all: switches(&unit) };
        assert!(s.on_off(ctx(10)).unwrap());
        assert!(!s.on_off(ctx(11)).unwrap());
        s.handle_off(ctx(10)).unwrap();
        s.handle_on(ctx(11)).unwrap();
        assert_eq!(field(&unit, "device", "learning_mode"), Value::Bool(false));
        assert_eq!(field(&unit, "device", "auto_away_enable"), Value::Bool(true));
        s.handle_toggle(ctx(12)).unwrap();
        assert!(s.on_off(ctx(12)).unwrap());
        s.handle_toggle(ctx(12)).unwrap();
        assert!(!s.on_off(ctx(12)).unwrap());
        // An endpoint past this unit's switches.
        assert_eq!(code(s.handle_on(ctx(14))), ErrorCode::NotFound);
        assert_eq!(s.on_off(ctx(14)).expect_err("read").code(), ErrorCode::NotFound);
    }

    #[test]
    fn sensors_in_their_clusters_units() {
        let unit = nest(fields!{"current_humidity" => 41.26, "backplate_temperature" => 22.137,
                                "battery_level" => 3.9215}, fields!{});
        let humidity = Humidity { dataver: Dataver::new(0), nest: unit.clone() };
        assert_eq!(humidity.measured_value(ctx(1)).unwrap(), Nullable::some(4126));
        // The backplate's own reading, not rounded as the screen shows it.
        let backplate = Backplate { dataver: Dataver::new(0), nest: unit.clone() };
        assert_eq!(backplate.measured_value(ctx(2)).unwrap(), Nullable::some(2214));
        let battery = Battery { dataver: Dataver::new(0), nest: unit };
        assert_eq!(battery.bat_voltage(ctx(3)).unwrap(), Nullable::some(3922));

        let unit = nest(fields!{}, fields!{});
        assert_eq!(Humidity { dataver: Dataver::new(0), nest: unit.clone() }.measured_value(ctx(1)).unwrap(),
                   Nullable::none());
        assert_eq!(Backplate { dataver: Dataver::new(0), nest: unit.clone() }.measured_value(ctx(2)).unwrap(),
                   Nullable::none());
        assert_eq!(Battery { dataver: Dataver::new(0), nest: unit }.bat_voltage(ctx(3)).unwrap(), Nullable::none());
    }

    #[test]
    fn battery_charge_level_by_voltage() {
        use power_source::BatChargeLevelEnum::{Critical, Warning, OK};
        for (volts, want) in [(Some(3.2), Critical), (Some(3.499), Critical), (Some(3.5), Warning),
                              (Some(3.599), Warning), (Some(3.6), OK), (Some(3.95), OK), (None, OK)] {
            let dev = volts.map_or_else(Fields::new, |v| fields!{"battery_level" => v});
            let battery = Battery { dataver: Dataver::new(0), nest: nest(dev, fields!{}) };
            assert_eq!(battery.bat_charge_level(ctx(3)).unwrap(), want, "{volts:?}");
        }
    }

    #[test]
    fn display_scale_round_trips() {
        use ui_config::TemperatureDisplayModeEnum::{Celsius, Fahrenheit};
        let display = Display { dataver: Dataver::new(0), nest: nest(fields!{}, fields!{}) };
        assert_eq!(display.temperature_display_mode(ctx(1)).unwrap(), Celsius);
        display.set_temperature_display_mode(ctx(1), Fahrenheit).unwrap();
        assert_eq!(field(&display.nest, "device", "temperature_scale"), Value::from("F"));
        assert_eq!(display.temperature_display_mode(ctx(1)).unwrap(), Fahrenheit);
        display.set_temperature_display_mode(ctx(1), Celsius).unwrap();
        assert_eq!(display.temperature_display_mode(ctx(1)).unwrap(), Celsius);
        assert_eq!(code(display.set_keypad_lockout(ctx(1), ui_config::KeypadLockoutEnum::Lockout1)),
                   ErrorCode::UnsupportedAccess);
    }

    #[test]
    fn fan_on_runs_the_timer_and_off_leaves_it_to_the_equipment() {
        use fan_control::FanModeEnum::{High, Off};
        let fan = Fan { dataver: Dataver::new(0), nest: nest(fields!{"fan_timer_duration" => 1800}, fields!{}) };
        assert_eq!((fan.fan_mode(ctx(4)).unwrap(), fan.percent_setting(ctx(4)).unwrap()), (Off, Nullable::some(0)));
        fan.set_fan_mode(ctx(4), High).unwrap();
        let timeout = field(&fan.nest, "device", "fan_timer_timeout").as_f64().unwrap();
        let ahead = timeout - chrono::Utc::now().timestamp() as f64;
        assert!((1795.0..=1801.0).contains(&ahead), "fan on for {ahead} s, want the timer's 1800");
        assert_eq!((fan.fan_mode(ctx(4)).unwrap(), fan.percent_setting(ctx(4)).unwrap()), (High, Nullable::some(100)));
        fan.set_fan_mode(ctx(4), Off).unwrap();
        assert_eq!(field(&fan.nest, "device", "fan_timer_timeout").as_f64(), Some(0.0));
        assert_eq!(fan.fan_mode(ctx(4)).unwrap(), Off);

        for (percent, on) in [(Nullable::some(1), true), (Nullable::some(0), false), (Nullable::some(100), true),
                              (Nullable::none(), false)] {
            fan.set_percent_setting(ctx(4), percent.clone()).unwrap();
            assert_eq!(fan.fan_mode(ctx(4)).unwrap() == High, on, "{percent:?}");
        }
    }

    #[test]
    fn fan_modes_outside_off_high_are_refused() {
        use fan_control::FanModeEnum::{Auto, High, Low, Medium, Off, On, Smart};
        let fan = Fan { dataver: Dataver::new(0), nest: nest(fields!{"fan_timer_duration" => 1800}, fields!{}) };
        for mode in [Low, Medium, Auto, Smart] {
            assert_eq!(fan.set_fan_mode(ctx(4), mode).expect_err("accepted").code(), ErrorCode::ConstraintError,
                       "{mode:?}");
            assert_eq!(fan.fan_mode(ctx(4)).unwrap(), Off);
        }
        assert_eq!(field(&fan.nest, "device", "fan_timer_timeout"), serde_json::Value::Null);
        // On is High at its one speed.
        fan.set_fan_mode(ctx(4), On).unwrap();
        assert_eq!(fan.fan_mode(ctx(4)).unwrap(), High);
    }

    #[test]
    fn fan_running_is_what_the_equipment_does() {
        let fan = Fan { dataver: Dataver::new(0), nest: nest(fields!{}, fields!{"hvac_fan_state" => true}) };
        assert_eq!(fan.percent_current(ctx(4)).unwrap(), 100);
        assert_eq!(fan.fan_mode(ctx(4)).unwrap(), fan_control::FanModeEnum::Off);
        let fan = Fan { dataver: Dataver::new(0), nest: nest(fields!{}, fields!{"hvac_fan_state" => false}) };
        assert_eq!(fan.percent_current(ctx(4)).unwrap(), 0);
    }

    #[test]
    fn fan_timer_shows_the_nearest_choice_and_sets_minutes() {
        for (seconds, mode) in [(None, 0), (Some(900.0), 0), (Some(1500.0), 1), (Some(2700.0), 2), (Some(5400.0), 3),
                                (Some(28800.0), 6), (Some(86400.0), 7), (Some(60.0), 0)] {
            let dev = seconds.map_or_else(Fields::new, |s| fields!{"fan_timer_duration" => s});
            let timer = FanTimer { dataver: Dataver::new(0), nest: nest(dev, fields!{}) };
            assert_eq!(timer.current_mode(ctx(5)).unwrap(), mode, "{seconds:?} s");
        }
        let timer = FanTimer { dataver: Dataver::new(0), nest: nest(fields!{"fan_timer_duration" => 900}, fields!{}) };
        let change = |mode: u8| timer.handle_change_to_mode(ctx(5),
            mode_select::ChangeToModeRequest::new(TLVElement::new(&[0x15, 0x24, 0x00, mode, 0x18])));
        for mode in 0..FAN_TIMER_MINUTES.len() as u8 {
            change(mode).unwrap();
            assert_eq!(field(&timer.nest, "device", "fan_timer_duration").as_f64(),
                       Some(f64::from(FAN_TIMER_MINUTES[mode as usize] * 60)));
            assert_eq!(timer.current_mode(ctx(5)).unwrap(), mode);
        }
        assert_eq!(code(change(FAN_TIMER_MINUTES.len() as u8)), ErrorCode::ConstraintError);
        assert_eq!(timer.current_mode(ctx(5)).unwrap(), 7);
    }

    #[test]
    fn labels_name_each_endpoint_beside_the_thermostat() {
        let labels = Labels { dataver: Dataver::new(0), names: vec![(BACKPLATE, "Backplate temperature"),
                                                                     (SWITCHES, "Auto-Schedule")] };
        let read = |endpoint: u16| {
            let mut buf = [0u8; 128];
            let mut wb = WriteBuf::new(&mut buf);
            let array = LabelStructArrayBuilder::new(TLVWriteParent::new("test", &mut wb), &TLVTag::Anonymous).unwrap();
            labels.label_list(ctx(endpoint), ArrayAttributeRead::ReadAll(array)).unwrap();
            let len = wb.get_tail();
            let list: TLVArray<'_, LabelStruct<'_>> = TLVArray::new(TLVElement::new(&buf[..len])).unwrap();
            list.iter().map(|l| {
                let l = l.unwrap();
                (l.label().unwrap().to_string(), l.value().unwrap().to_string())
            }).collect::<Vec<_>>()
        };
        assert_eq!(read(SWITCHES), [("ha_entitylabel".to_string(), "Auto-Schedule".to_string())]);
        assert_eq!(read(BACKPLATE)[0].1, "Backplate temperature");
        assert!(read(SWITCHES + 1).is_empty());
    }
}
