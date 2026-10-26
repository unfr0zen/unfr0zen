// Zigbee2MQTT external converter for a Nest Learning Thermostat (3rd gen) set
// up with unfr0zen (https://github.com/unfr0zen/unfr0zen), Zigbee checked.
//
// The thermostat is endpoint 1, in standard clusters: Thermostat (eco is the
// economy bit of its programming operation mode, the eco temperatures are the
// unoccupied setpoints), Fan Control (with a fan), Thermostat User Interface
// Configuration, Relative Humidity and the battery's voltage. The rest have
// endpoints of their own, present as the unit's equipment calls for:
//
//   2   backplate temperature   Temperature Measurement
//   3   fan timer duration      Analog Output, minutes (with a fan)
//   4   target humidity         Analog Output, % (with a humidifier or dehumidifier)
//   5   time to temperature     Analog Input, seconds left (0: none)
//   6   filter runtime          Analog Input, hours (with an air filter)
//   10  Auto-Schedule           On/Off
//   11  Home/Away Assist        On/Off
//   12  Sunblock                On/Off
//   13  Early-On                On/Off
//   14  eco heat limit          On/Off (to heat)
//   15  eco cool limit          On/Off (to cool)
//   16  humidity control        On/Off (with a humidifier or dehumidifier)
//   20  leaf                    Binary Input
//   21  Early-On active         Binary Input
//   22  Sunblock active         Binary Input
//   23  safety shutoff          Binary Input
//   24  filter replacement      Binary Input (with an air filter)
//   25  hot water               Binary Input (with hot water control)
//   26  humidifier running      Binary Input (with a humidifier)
//   27  dehumidifier running    Binary Input (with a dehumidifier)
//
// The Thermostat cluster's Clear Weekly Schedule clears the unit's schedule.
//
// Home Assistant gets the entities unfr0zen's MQTT relay announces, under
// the same names: the climate has one target in heat or cool and the range
// in heat_cool (ZCL's auto), eco its one preset.
//
// Needs Zigbee2MQTT 2.12.1 or later. Install: copy to Zigbee2MQTT's
// data/external_converters/ and restart it.

import * as exposes from 'zigbee-herdsman-converters/lib/exposes';
import * as fz from 'zigbee-herdsman-converters/converters/fromZigbee';
import * as tz from 'zigbee-herdsman-converters/converters/toZigbee';
import {isDummyDevice} from 'zigbee-herdsman-converters/lib/utils';

const e = exposes.presets;
const ea = exposes.access;

const THERMOSTAT = 1;
const BACKPLATE = 2;
const OUTPUTS = {fan_timer_duration: 3, target_humidity: 4};
const INPUTS = {time_to_target: 5, filter_runtime: 6};
const SWITCHES = {
    auto_schedule: 10,
    home_away_assist: 11,
    sunblock: 12,
    early_on: 13,
    eco_heat_limit: 14,
    eco_cool_limit: 15,
    humidity_control: 16,
};
// Label, icon, description.
const SWITCH_LABELS = {
    auto_schedule: ['Auto-Schedule', 'mdi:school', 'The thermostat learns the schedule'],
    home_away_assist: ['Home/Away Assist', 'mdi:home-account', 'Eco when no one is home'],
    sunblock: ['Sunblock', 'mdi:weather-sunny', 'Corrects for direct sunlight'],
    early_on: ['Early-On', 'mdi:clock-fast', 'Starts early to reach the target on time'],
    eco_heat_limit: ['Eco heat limit', 'mdi:leaf', 'Heats to the eco heat temperature while in eco'],
    eco_cool_limit: ['Eco cool limit', 'mdi:leaf', 'Cools to the eco cool temperature while in eco'],
    humidity_control: ['Humidity control', 'mdi:water-percent', 'Runs the (de)humidifier'],
};
const STATES = {
    leaf: 20,
    early_on_active: 21,
    sunblock_active: 22,
    safety_shutoff: 23,
    filter_replacement: 24,
    hot_water: 25,
    humidifier: 26,
    dehumidifier: 27,
};
// Label, Home Assistant metadata (a name beside a device class, which
// Home Assistant would name it after otherwise), description.
const STATE_LABELS = {
    leaf: ['Leaf', {icon: 'mdi:leaf'}, 'The leaf shows: an energy-saving temperature'],
    early_on_active: ['Early-On active', {icon: 'mdi:clock-fast'}, 'Early-On is starting early'],
    sunblock_active: ['Sunblock active', {icon: 'mdi:weather-sunny'}, 'Sunblock is correcting for sunlight'],
    safety_shutoff: ['Safety shutoff', {deviceClass: 'problem', name: 'Safety shutoff', entityCategory: 'diagnostic'},
        'The system was shut off for safety'],
    filter_replacement: ['Filter replacement', {deviceClass: 'problem', name: 'Filter replacement'}, 'The air filter needs replacing'],
    hot_water: ['Hot water', {deviceClass: 'heat', name: 'Hot water'}, 'Hot water is heating'],
    humidifier: ['Humidifier', {deviceClass: 'running', name: 'Humidifier'}, 'The humidifier runs'],
    dehumidifier: ['Dehumidifier', {deviceClass: 'running', name: 'Dehumidifier'}, 'The dehumidifier runs'],
};

// ThermostatProgrammingOperationMode: following its schedule, economy.
const SCHEDULE = 0x01;
const ECONOMY = 0x04;
// ThermostatRunningState: heat, cool and fan on.
const RUNNING = {heating: 0x0001, cooling: 0x0002, fan_running: 0x0004};

const byEndpoint = (map, id) => Object.keys(map).find((name) => map[name] === id);

const nest = {
    // Eco, as the climate's preset.
    preset: {
        cluster: 'hvacThermostat',
        type: ['attributeReport', 'readResponse'],
        convert: (model, msg) => {
            if (msg.data.programingOperMode !== undefined) {
                return {preset: msg.data.programingOperMode & ECONOMY ? 'eco' : 'none'};
            }
        },
    },
    // What runs, as binary sensors beside the climate's action.
    running: {
        cluster: 'hvacThermostat',
        type: ['attributeReport', 'readResponse'],
        convert: (model, msg) => {
            if (msg.data.runningState !== undefined) {
                return Object.fromEntries(Object.entries(RUNNING).map(([name, bit]) => [name, Boolean(msg.data.runningState & bit)]));
            }
        },
    },
    backplate: {
        cluster: 'msTemperatureMeasurement',
        type: ['attributeReport', 'readResponse'],
        convert: (model, msg) => {
            if (msg.endpoint.ID === BACKPLATE && msg.data.measuredValue !== undefined && msg.data.measuredValue !== -32768) {
                return {backplate_temperature: msg.data.measuredValue / 100};
            }
        },
    },
    outputs: {
        cluster: 'genAnalogOutput',
        type: ['attributeReport', 'readResponse'],
        convert: (model, msg) => {
            const name = byEndpoint(OUTPUTS, msg.endpoint.ID);
            if (name && msg.data.presentValue !== undefined) {
                return {[name]: Math.round(msg.data.presentValue)};
            }
        },
    },
    inputs: {
        cluster: 'genAnalogInput',
        type: ['attributeReport', 'readResponse'],
        convert: (model, msg) => {
            const value = msg.data.presentValue;
            if (value === undefined) {
                return;
            }
            if (msg.endpoint.ID === INPUTS.time_to_target) {
                return {time_to_target: value > 0 ? new Date(Date.now() + value * 1000).toISOString() : null};
            }
            if (msg.endpoint.ID === INPUTS.filter_runtime) {
                return {filter_runtime: Math.round(value * 10) / 10};
            }
        },
    },
    states: {
        cluster: 'genBinaryInput',
        type: ['attributeReport', 'readResponse'],
        convert: (model, msg) => {
            const name = byEndpoint(STATES, msg.endpoint.ID);
            if (name && msg.data.presentValue !== undefined) {
                return {[name]: Boolean(msg.data.presentValue)};
            }
        },
    },
    switches: {
        cluster: 'genOnOff',
        type: ['attributeReport', 'readResponse'],
        convert: (model, msg) => {
            const name = byEndpoint(SWITCHES, msg.endpoint.ID);
            if (name && msg.data.onOff !== undefined) {
                return {[name]: msg.data.onOff ? 'ON' : 'OFF'};
            }
        },
    },
};

const set = {
    preset: {
        key: ['preset'],
        convertSet: async (entity, key, value) => {
            await entity.write('hvacThermostat', {programingOperMode: value === 'eco' ? ECONOMY : SCHEDULE});
            return {state: {preset: value}};
        },
        convertGet: async (entity) => {
            await entity.read('hvacThermostat', ['programingOperMode']);
        },
    },
    outputs: {
        key: Object.keys(OUTPUTS),
        convertSet: async (entity, key, value, meta) => {
            await meta.device.getEndpoint(OUTPUTS[key]).write('genAnalogOutput', {presentValue: Number(value)});
            return {state: {[key]: Number(value)}};
        },
        convertGet: async (entity, key, meta) => {
            await meta.device.getEndpoint(OUTPUTS[key]).read('genAnalogOutput', ['presentValue']);
        },
    },
    clear_schedule: {
        key: ['clear_schedule'],
        convertSet: async (entity) => {
            await entity.command('hvacThermostat', 'clearWeeklySchedule', {});
        },
    },
    switches: {
        key: Object.keys(SWITCHES),
        convertSet: async (entity, key, value, meta) => {
            const state = String(value).toUpperCase();
            const command = {ON: 'on', OFF: 'off', TOGGLE: 'toggle'}[state];
            if (!command) {
                throw new Error(`${key}: expected ON, OFF or TOGGLE, got '${value}'`);
            }
            await meta.device.getEndpoint(SWITCHES[key]).command('genOnOff', command, {});
            if (command !== 'toggle') {
                return {state: {[key]: state}};
            }
        },
        convertGet: async (entity, key, meta) => {
            await meta.device.getEndpoint(SWITCHES[key]).read('genOnOff', ['onOff']);
        },
    },
};

/** Whether the unit has an endpoint, or a cluster on it (all, for the docs' dummy device). */
function has(device, id, cluster) {
    if (isDummyDevice(device)) {
        return true;
    }
    const endpoint = device.getEndpoint(id);
    return Boolean(endpoint) && (!cluster || endpoint.supportsInputCluster(cluster));
}

const definition = {
    zigbeeModel: ['Nest Learning Thermostat'],
    model: 'Nest Learning Thermostat (3rd gen, unfr0zen)',
    vendor: 'Google Nest',
    description: 'Nest Learning Thermostat, 3rd generation, freed by unfr0zen (nestlocal)',
    fromZigbee: [fz.thermostat, fz.fan, fz.humidity, fz.battery, fz.hvac_user_interface, nest.preset, nest.running,
        nest.backplate, nest.outputs, nest.inputs, nest.states, nest.switches],
    toZigbee: [tz.thermostat_occupied_heating_setpoint, tz.thermostat_occupied_cooling_setpoint,
        tz.thermostat_unoccupied_heating_setpoint, tz.thermostat_unoccupied_cooling_setpoint, tz.thermostat_system_mode,
        tz.thermostat_local_temperature, tz.thermostat_running_state, tz.fan_mode, tz.thermostat_temperature_display_mode,
        set.preset, set.outputs, set.clear_schedule, set.switches],
    exposes: (device) => {
        // What the unit's wiring can do: cooling only (0), heating only (2), both.
        const sequence = isDummyDevice(device) ? 4 :
            device.getEndpoint(THERMOSTAT)?.getClusterAttributeValue('hvacThermostat', 'ctrlSeqeOfOper');
        const [canHeat, canCool] = [sequence !== 0, sequence !== 2];
        const modes = ['off', ...(canHeat ? ['heat'] : []), ...(canCool ? ['cool'] : []), ...(canHeat && canCool ? ['auto'] : [])];
        const climate = e.climate()
            .withLocalTemperature()
            .withSetpoint('occupied_heating_setpoint', 9, 32, 0.5)
            .withSetpoint('occupied_cooling_setpoint', 9, 32, 0.5)
            .withSystemMode(modes)
            .withRunningState(['idle', 'heat', 'cool', 'fan_only'])
            .withPreset(['none', 'eco'], 'Eco: the eco temperatures apply');
        if (has(device, THERMOSTAT, 'hvacFanCtrl')) {
            climate.withFanMode(['on', 'auto']);
        }
        const result = [
            climate,
            e.humidity(),
            e.battery_voltage().withLabel('Battery voltage').withHomeAssistant({name: 'Battery voltage'}),
            e.binary('occupancy', ea.STATE, true, false).withDescription('Someone is home, as Home/Away Assist sees it'),
            e.enum('temperature_display_mode', ea.ALL, ['celsius', 'fahrenheit']).withLabel('Temperature scale')
                .withCategory('config').withDescription('The screen\'s units'),
            e.numeric('backplate_temperature', ea.STATE).withUnit('°C').withCategory('diagnostic')
                .withHomeAssistant({deviceClass: 'temperature', name: 'Backplate temperature'})
                .withDescription('Temperature at the backplate'),
        ];
        const running = {heating: [canHeat, 'Heating', 'heat'], cooling: [canCool, 'Cooling', 'cold'],
            fan_running: [has(device, THERMOSTAT, 'hvacFanCtrl'), 'Fan', 'running']};
        for (const [name, [present, label, deviceClass]] of Object.entries(running)) {
            if (present) {
                result.push(e.binary(name, ea.STATE, true, false).withLabel(label).withHomeAssistant({deviceClass, name: label})
                    .withDescription(`${label} runs`));
            }
        }
        if (canHeat) {
            result.push(e.numeric('unoccupied_heating_setpoint', ea.ALL).withLabel('Eco heat temperature').withUnit('°C')
                .withValueMin(4.5).withValueMax(21).withValueStep(0.5).withCategory('config')
                .withDescription('Eco heats to this'));
        }
        if (canCool) {
            result.push(e.numeric('unoccupied_cooling_setpoint', ea.ALL).withLabel('Eco cool temperature').withUnit('°C')
                .withValueMin(24).withValueMax(32).withValueStep(0.5).withCategory('config')
                .withDescription('Eco cools to this'));
        }
        if (has(device, OUTPUTS.fan_timer_duration)) {
            result.push(e.numeric('fan_timer_duration', ea.ALL).withUnit('min').withValueMin(15).withValueMax(1440)
                .withValueStep(15).withCategory('config').withHomeAssistant({icon: 'mdi:fan-clock'})
                .withDescription('How long the fan runs when turned on'));
        }
        if (has(device, OUTPUTS.target_humidity)) {
            result.push(e.numeric('target_humidity', ea.ALL).withUnit('%').withValueMin(10).withValueMax(60)
                .withValueStep(5).withCategory('config').withDescription('Humidity the (de)humidifier aims for'));
        }
        result.push(e.text('time_to_target', ea.STATE).withLabel('Time to temperature')
            .withHomeAssistant({deviceClass: 'timestamp', name: 'Time to temperature'}).withDescription('When the target is reached'));
        if (has(device, INPUTS.filter_runtime)) {
            result.push(e.numeric('filter_runtime', ea.STATE).withUnit('h').withCategory('diagnostic')
                .withLabel('Filter runtime').withHomeAssistant({deviceClass: 'duration', name: 'Filter runtime'})
                .withDescription('How long the air filter has run'));
        }
        for (const [name, id] of Object.entries(STATES)) {
            if (has(device, id)) {
                const [label, homeassistant, description] = STATE_LABELS[name];
                result.push(e.binary(name, ea.STATE, true, false).withLabel(label).withHomeAssistant(homeassistant)
                    .withDescription(description));
            }
        }
        result.push(e.enum('clear_schedule', ea.SET, ['clear']).withLabel('Clear Nest schedule').withCategory('config')
            .withHomeAssistant({icon: 'mdi:calendar-remove'})
            .withDescription('Empties the schedule and turns Auto-Schedule off: Home Assistant schedules'));
        for (const [name, id] of Object.entries(SWITCHES)) {
            if (has(device, id)) {
                const [label, icon, description] = SWITCH_LABELS[name];
                result.push(e.binary(name, ea.ALL, 'ON', 'OFF').withLabel(label).withCategory('config')
                    .withHomeAssistant({icon}).withDescription(description));
            }
        }
        return result;
    },
    meta: {
        // Home Assistant's climate as the MQTT relay announces it: one
        // target in heat or cool (the unit takes either setpoint as it), the
        // range in heat_cool, eco the one preset (Home Assistant refuses a
        // 'none' among them, and with it the whole entity).
        overrideHaDiscoveryPayload: (payload) => {
            if (!payload.mode_command_topic?.endsWith('/system_mode')) {
                return;
            }
            const range = payload.modes.includes('auto');
            payload.modes = payload.modes.map((mode) => (mode === 'auto' ? 'heat_cool' : mode));
            payload.mode_state_template = '{{ \'heat_cool\' if value_json.system_mode == \'auto\' else value_json.system_mode }}';
            payload.mode_command_template = '{{ \'auto\' if value == \'heat_cool\' else value }}';
            payload.temperature_command_topic = payload.temperature_low_command_topic;
            payload.temperature_state_topic = payload.temperature_low_state_topic;
            payload.temperature_state_template =
                '{{ value_json.occupied_heating_setpoint if value_json.system_mode in [\'heat\', \'cool\'] else None }}';
            if (range) {
                payload.temperature_low_state_template =
                    '{{ value_json.occupied_heating_setpoint if value_json.system_mode == \'auto\' else None }}';
                payload.temperature_high_state_template =
                    '{{ value_json.occupied_cooling_setpoint if value_json.system_mode == \'auto\' else None }}';
            } else {
                for (const end of ['low', 'high']) {
                    for (const key of ['command_topic', 'state_topic', 'state_template']) {
                        delete payload[`temperature_${end}_${key}`];
                    }
                }
            }
            payload.preset_modes = payload.preset_modes.filter((preset) => preset !== 'none');
        },
    },
    configure: async (device, coordinatorEndpoint) => {
        const report = async (id, cluster, attributes) => {
            const endpoint = device.getEndpoint(id);
            if (!endpoint || !endpoint.supportsInputCluster(cluster)) {
                return;
            }
            await endpoint.bind(cluster, coordinatorEndpoint);
            // One at a time: all of the thermostat's at once overflow a frame.
            for (const attribute of attributes) {
                await endpoint.configureReporting(cluster,
                    [{attribute, minimumReportInterval: 0, maximumReportInterval: 3600, reportableChange: 0}]);
            }
            await endpoint.read(cluster, attributes);
        };
        await report(THERMOSTAT, 'hvacThermostat', ['localTemp', 'occupancy', 'occupiedHeatingSetpoint',
            'occupiedCoolingSetpoint', 'unoccupiedHeatingSetpoint', 'unoccupiedCoolingSetpoint', 'ctrlSeqeOfOper',
            'systemMode', 'programingOperMode', 'runningState']);
        await report(THERMOSTAT, 'hvacFanCtrl', ['fanMode']);
        await report(THERMOSTAT, 'hvacUserInterfaceCfg', ['tempDisplayMode']);
        await report(THERMOSTAT, 'msRelativeHumidity', ['measuredValue']);
        await report(THERMOSTAT, 'genPowerCfg', ['batteryVoltage']);
        await report(BACKPLATE, 'msTemperatureMeasurement', ['measuredValue']);
        for (const id of Object.values(OUTPUTS)) {
            await report(id, 'genAnalogOutput', ['presentValue']);
        }
        for (const id of Object.values(SWITCHES)) {
            await report(id, 'genOnOff', ['onOff']);
        }
        // The unit reports to the coordinator bound or not, and its binding
        // table (16 entries) is about full: these are only read.
        const read = async (id, cluster) => {
            if (device.getEndpoint(id)?.supportsInputCluster(cluster)) {
                await device.getEndpoint(id).read(cluster, ['presentValue']);
            }
        };
        for (const id of Object.values(INPUTS)) {
            await read(id, 'genAnalogInput');
        }
        for (const id of Object.values(STATES)) {
            await read(id, 'genBinaryInput');
        }
    },
};

export default definition;
