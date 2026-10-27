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

// The picture Zigbee2MQTT's frontend shows: the unit heating, on client-ng's
// classic face (200x200 WebP).
const PICTURE = 'data:image/webp;base64,UklGRuAbAABXRUJQVlA4WAoAAAAQAAAAxwAAxwAAQUxQSCcGAAARClrMtqluoxklqtwUvOoexaojy41UppTJicrcWl2GwvJeccpeLrdJysvgZWbm3cByrHqxaJUTVY61ykbqUXSszEXo+JyZf6S9+s9MREwA8pj2GRBZdkfFi9/+9mf6otWGzPMwa6YP138br7jrstJiPyXwzTPGXRWLVx9rMrPMZm1mTlS/eP9V4wrz4aLDCze8lsxkmSO1+vPt2IJwPkCM6WvfS5nM4Tr17lo0IEGD0UeTLcyl+tCBaJDCwFj6xD8mc7U+/MRSQ3gFpTsSpsdBXb9pkk9khcs/bWbczHywvFBUgzckLI+rSuNgAdGSylOMwycrS1AsdNz+c4zTeu8YKpCRqpFxvEGNFEXogVMe5zUWicB/e5IJUN3m5x2d/qnlCVHhdMq14LY0E2Z6S5BfedHfmFBVNI9TgYpmJliFmkszapiAq6fzp2D1BSZkXV7AmVDcYoJWceRK6Y9M4HURftAbzzGh61WUEz1jLZ7g9caeXOj3qMWErw7040D4XQbCt0KuK/7KAyJqlw3+zgMjalcNr/MAWTvcRfonD5RKu2ZgjQdMNdAl4a8ZOL8e4IrgJwygnwZd4H+ZgfQlv+N8VW0wQeVzGF3d6gEV11JnLW/2wKrQUVNPeYDVUxxUVOuBVoccU/CsB9w4OqXcgo662yHTGxl4G8scUVjNAFwdcEDeTgZiRXO3tBlGanHO+v/kAflHzNUmBuYtNDelF+CULstJn48YoFXvXNxkQar1xhwUJTxQa2VfzAO2sm3YSWilhtlEqxi4FdpTch5eDeNsoXsYwA+gHaPOQex8iR0VHsgrbRiYgtmpod1by4B+f7eMeqglA91ZbkFNreiG70MG9o96dm1qBm5qSte2eIBXXTISkNPYlaUm5PSSLtAnPNBr7Cx4GHb/BDuLmrDT0c72M+CrTowE9PSlHZW1QO+/so7WeuBXHeS/B793dbtwCn6pcLuFJvz0gnYbmQRuIIh5b8jAa4qg8YcMJA2C4zMyoMYjXpOVAY0EH/Ck8H5CX5SDF9BfIwfVWJySgxM60iQHTZHLTDnQl5Vn5UDfU8UkUb0mC69Wy0J1vSzow7KAGVloMmVBW7Kg2mQB5cH73yrKAmZlQZuyoDOyoI7JwtGELOhqWah+TRZerpIFVd4mB6gvM+VAXxZpkoOmSHFKDlLaXysHGunLcvAykgfkAAlenZUBjQTHZ2RAjUcs/FMGUBHMf0sGdD4ixmRAISIuNOGnF7YbkIJfSrXr8QH8sEc73AA/JB3OaIGent6RkYCexo5wL/SQdLrEhJ1e3Fn/v2H3Z6AzfAx2j5IuLjEhpxd3xUhA7jfsCm6CHJIuT8rArWli13wfwA1V13C5BTW1gnTTqIfaoUB3cC3UYtjtgSmYnRraPayAWSWxseQcxBpK7KB7IXaA2oFjGuDVOI7YSvfCC6k9OPIUtPRwYvf90HqA2F50CFbJkH14iwUpdQvmsM8nkNIqF1iahlN6Oskp3QKnHZgbDP4CJQySXEebYaSimPO8ChhV5eUOA9UQqu1PnDj9AnwaZxBnllvQURodWvAsdF7s7RQM1cEGw8S5kVOQ0RF08opmuLSsREfTda1QsWLUWejb2waU/T2J0y95AyZv9EPnh76AyFch4sbib+HxXTFx58BaaNQNJm4d/hMslEb3DquBhBqGbh74NRy+VujuAZ9C4ZMwcXvw5TYI4Ev9ifv9Va3iQ+UnPPSt+090ap0P+UiXnxKbRkq4OaVOZKgIT0NxS1QqjoSvBXc3iOn83QWEu6XfiQgjhMeBHf+KBlUh4TNdWCcWXEAJt/WmC+LQGCA8p5EPWsWAH0QI73uvSohAq94owKJYincpVYSCHFbVwDNUw4g46ZgDDbzC/WMoESotqTzJI1WpkYh3cCxp8UUlYxoFHVjx4b/8yHy4QhOB+yI7k608MBObpviI6AOXPXu01V3m4SeWGgSEtOiyR5MtbtGH9keDlADSmBn76KTptIvH3l493SDw9BUvjr31RybrDKsp+dq6uaF8AlZtTLzmgRdrUhkza5e+mD72bXzjZSVGHoEv9ReXXVZe9dJ39cearTZsh1kzfbj+23jFXcumhntTwmMAVlA4IJIVAAAQWgCdASrIAMgAPmEukkckIqGhJBMbkIAMCUEg14YGPvP7+q7qDyPavy56Cnljvv05p0ew39j/WPy0+dX+E9Xv6L/4fuAfpP/tP7h6uXrL8wH66ftr7s34Z+8H+4/2X/M+4B/U/6V/6+wz9AX9mfS6/an4Pv2t/cD2g//17AGs89u/+g8Tf0X3GNyfDfWs/Hv8P2RdofAL/Hv6v/tPZfhjOEe9HqFfG+c/BeeIT597Af87/xHrN/5n7Xei37A9hL9b+uD+3vs3suRVP3frZlo5mQUz7E/7XThx0ix2ce+ePXx8wscSNf/n2fZGgbC5oafeWybExUmvf5i4MssBS/jD38zgq38bqTrFCgtIBmO5B38EB6DE5B65NzfDu5zfZKUISogJDYIg68Wm3dxmiTfC1FolgBCf8fOBNImOW4sRTs5artN5nnrtSe28LL8hp92fR6coAWhHa+yTMifwa7WbA5Jb1kPm0ULUNteU4dN1K5+Qi/m9Z/FpkbPqjkK6Xez7IkJmWBPomqojaM80fWdONecNc8BRzQOOWughM0hA877q4k+0ffVMfS157aUZQWd6Lj7Z8jb4MVt7vjoAczesdKXlWir0aBZR2L9yPhNohwKqgvO8nQQJaYIjQKlZCtE0dlNEuPMYF4z3LH5xFi62L0bTjOiHhPntxJc8LT3ko6zI8FbM7WBf2vTVGRDD9KgWGTx0XDDIXWfZel7s0ZJRrHCX4ZSh6cB/VbZIY/yaiBGaBtss8RJ5Od2mCGTbsnbcPMjkD+ak0H2peW4EO/gomi9pnaudVE9VfPF/k7QFfe9+ALvwwcxf7LykvAbBlBwTQw3LK/WOzGK1elMpsTIkXChE4D8qBdjEkp5AQI5JZYQO+3OSEO2c9YPX0Gl0JrLO9iKbJ5AbySX30lTjn1y7////lohzxPp4YzMoBrifqwqqup+5ddnWhcX9ZjIlV17nLi4KWa/+KPAA/rDIGr4JfxXidgee2BamaLR6oskO91DTQ62c500A4cblAVMTpBuDMH8ApSxZc9XXJwpxxOZiJkNPpDZbWLUpZCPeGI4RdkjnmrXuVAbaO3phjh62NE+8wn3Qf/Ilbh6Jln6dYbR0jgWJnKeNPpH5fAyFMO6o0TobM0JqTZusS/bXlzrSQR3wqY9UhYLPaX77eCXF3IBkqlDyZRmxgcKVy7w/vPZwx6Ld9XBLjrakgrsfWZouRc+edM0SI5N34UdevsnvPl5KawLWCFI4u0Qy5QiVylcf+lAEaG0LzKtRLl3fT05YcF++evFdcAL2ELpzTvmqqGIfp51lClVo3Q5HXqMddAg7eCs4IdzkMOmvnK2Kpt3G0NgsQp0HPgXs/h91IruF/+UWaPNF/oNszBDXtfn1OWQ/bYDX1Poatk4LdJulVYYQYJ2DXHKcO2fXDtcxGs8cz4A9/y73rZZexwM1cTnF9lCp7O4d4CPkE1OVAkao7/7CdprHVQGcLneUn+EAGoaZwpabBVmCbOLTJ61Jx6xOANeRyPQzRmMsW44kx6CjozMB3km4kEFSL3Yd7JFtmQvCEh77bcN2hKe/KEr0iq8KnM/oS04DCHiX3KglT639ZrCieYX/Ei+fpEe+ouEYUYkYCkwvGlaefzyfBWAt2VEREm2ww6KH2HdyBxlJpscDTukbC5fNbDeqUDFkBewPqS7KSAsdK1QGSjpymhx1Q3JmWVNXMDN/L0xOrs3WzZ7ZRGGweRexVIGQncp0ZBBhO2+QcLwsUCXFQBiAHSbYUh1UTgsIvtfqjhk49IilHYgUd3WGh38DMj3Gb9cs9/WPtwS/6DoPMjzpJjf1k6mno5z4g3gbz8Lye0Z2zRlpdWOgcuVzbA1ZeCYnni9k2myTMEd9v1x2x3zu/tUFw6mxN3p4DYOv2lZL99spAdJv1pgTnXE1lBEktJmMMdySwnH0heENJaEWLRduib8VZ+fbYsGxacBEUvGBzbJVvkZ6yxC4TY0HS9xgeQ/SDMyAYViRlBtUxhRb+uPctib0m9Gs8NehvkYuzN+o+Oi2HrcEyt8nfDrFjah0PYLp2Ek2gTuWdE4vRMnZoFHHd1Y//4AzQwpY9220cA3wic2mLJrqqTQMLZJT5yJ+MdBwxRNE+r60us+9Tqm0n6GmoJeuJCUTZWK+/Zei8rohgEcB7gIcUbl8ao5I4Ej0Qed0HIUIkXF2cI7tD715eNOfuTEe/vjjrF9eYztKfVSogzEArGMppi/lLinOGj8B3lw0dEj0yii9H/etAEH/IakPR9AF4m+ty2vIHQAucxA5pBImXkgcmHNnvDm3xo3Ut7uvuC2EwkayYU6TI8E/NWX3b3WqQ6z2pKITI3mWh97E7i87KfIwZOsPd7+rypq6MEPFWYevzncwDY69LiorqmI7RXDDQxjCp7NUwVxmwC8OyQFIsi9xHR/38V6xcSqX3w0FLn2fVHtZNlaG1s4gYyz25apg1peguSeRXxRw5wL2y7gpNpe2oPhmoULfDTRzomZ52m04xH7fyGbnAY1+O0tWl/n7Lq1lgLjpVqU7QqZQhfZUh1U9E81ETSFTYO6QsmeEVQqiTkiVtFZZLjTYe632HsY0Y/31aTgYrd2veYKZliOiGhbaKSd2Sr6oWCmqOs941Nvt53MH8vnYq5KQTW08VgyZgkLUOWIm7Rs00ZvzJtr2sOR1lcraguMMSIxKNlUaoGCmb/mjxzIOC5efrpC1z+PK13EhHdJND13KHUd8XIK2C0vxyovHRRp7waXgxWYIgO5cBeEk4WHxCZaO+qI1a80gf/sV2l0UiUV3RrG6VzON/+ZlzlIXHYfyStiY//xnq6TIaH98L/JoAoDgUMNzfbz7sCKE7nXSQdRRC7RFF1qTsVIrS7PpJXDc9S4ayuVu7Fycx/2CRVG6b6sIIHC082G1AXeIplOHdbibW4keRh/uC8mcVMIWtzPBzHfQ808PqDleW3Wb+kwaIH7SBKdIyo9NS0F8i7cuZOF5Wj0tHqsXqFTBtAyMJBjMqC+M9HcuytH/DsMzjPGab29Jw7OyUbE+B+VSO/a5Xo1FAlZv/MtMFxfCL/mrCyavNsUxZtEiCdlh1wGf0mYLk0mF+XcvaFtQ50qWi54z1xEaVwDXQKYQ8Cn40pcjxmUh9ZexuosH8FdiReQJVDFzWBPeIqZNtn1ED8peHjl6s9uOAtfpxtDco5/cSv4j6Ih34R/8+JiFuLB3VBca2XaxnZVB1JKxlsjpkVIMxzU44PDRiYHkX/Z6A8/1DvxxeYulJcg0LXd8BT16+We6WW/yGHbVCJveeym+ik5mFgbD0u73iIpfqP7YUdfuSKJFoasGx9lqR/WrMug5jC1GTeSm2yfR4hK5cDy2qYCu5hmubspeSVo0/f+zpJBWoxtpCaUVCbB2kHBLEQ2GUPjnCl1C4lTyeo8x7qFkhoUCtcmSLkDdj7ZUFMzb321gTHcJOBGZxx2NcL/gDZBh9RkVbs2UYLm6S5EqJn5PTQrw1cpw8ID9Vyeh8Pg6TldjzeVu2U/y3Ujt5+lhSE/FajHY1k1ByPPkFdp3ANUIhdAjzd/IoT5ts/Xv+iissFLOrM+XxO12cdcNkgEo3iNEhX27gLxOUqDOp4zCtbUc8utmw3gD9ILQWHj3RcElYtt/66XjEdd4RBaYXS3QO8KciRlrwENhtcbPRhRF4CjZkz4v2qjrd+WQrq5SUHzDrMc6PWM0HQv4wgWO8xZXlPaEn2VC3ySOJEoF/9DO47+nVsF3a1ey8BoVNT9GfHJVQ34ucMoUMtKumpqq/Q4BVXOVCSwVBewzja1tsAFvNunkDdw+mSaF2X9FKXfNTBtBTa+4eKlezTSQxnr8T7hux00+Mry0AK90DA2OjKYzcK/pMMCQKoAvd8sP+5otgpntiqOrPBQ2YI0wmD3sV0+AnuuLhBme8Rl9rLVMpRUIPd4ve6R3kRDTNcMr7uwazje2V9oV1YXzvFr7rUOG5UOB1QmYRRqt3KnqOca7kSmVFg4ZG1lYqwI+c12mpsXyD4cJdjy96pMBpFm80VnIF3rYYPxYC4Tl8kLm+2rzrfHzH+yFuWuILLxWxuSKfbVRN7AyEOaMk8Imp7evogGdqWzax66gQbD16EI1D4dGV85/fKVxdxysljR8lXspj7wmzSrBXnD4VGDGJEwmHAJRIgLjLHONTEi31ty6xLbExVqTFEknAdf5E2HtZUQ8Kg9nnPQoqZkuUmo7juJy1uNm+pCgtIrXvIwVmF8PJsMAN4seDRmeWRGe8avUqflTLDF4DqBR2BPVAH8DBuJBqnrx7qImbiWK3/GElv3zCfmy93vWKIaLEfw7ODlQsOR29+9tcfv9AyxQQxX88AsSZn60PQdono/fv7AeBFVRX5ufd0UZSYCQ/Z+/iccORGJINZuox3NWUtZ+m23tA2M7yeBJ0XfapP8nq4+Xa2Z1LFsMFiy15FTlyw4gr2VD78tdvC1x50azjuwU3hhxSjszXvNFVcTfbsnpD5sFshpawJYBSvQsxXlvflC4vlDyVEHp1XbZnCJ+FIby+QbyhI4HEDs25bDr/lNSiR+3xeSYPO/paMo/9ynqMAosgC68Lwie1esKAW4s3Z47XpUDmZfBKs91BOykb6SagOoQbZVxMejWF0E4AllUTMKhKEBqLg9j9OMtV6BnxrfoKXuSCqVf+NQ2AipW/UdU/yLSLn0hnBRaa4IwH6PWarTcvqSCxJcaDBrKxO4KlWOjxA9v62RLhtfJA/K9kiNyRw6W6U69p+FrxJIIMcw6VNBG8eU8pWwQlicqPurdiUMo5ZjCmnF9j99jTo9BvHG4qBCah87T5PqRP9PRU9EhSvR1KrQ30vocWIYDdzdv6vIFu0qvT4lzttDB2v8NzOaPC2Hfe628TtE8kBt+fVqdfXUshdOu5oi9RCrhSGYBsWhbUkD+mAmSYfhiv4G133GB2P+y1EaDWIGsKqVoHeKqJNqczWKQjZycbC0xemJEuH2/wMbnT3f0nEYiljt0HB3BLpxtuCssqEMiza5SbEdaYXECsBNwNCkiE/i8ZIPGBLIa8Aw9/kB100/MqkIuG9Ya7aAnNEW8n24mXxzSaPygKpkaUxHBzGHjQnOUGAgUozj42Lv6ulJ+NuDumPoyzbPD7/xK7H7bcniX/0M5HqzovOZyP+oCErh2pD/2zW5rdIn4knysG+BFw2XsKObkrcmHP9pCMSfIGnvKtDZjosHlUc4JYtkcpWQYW5ZpvrOS6jkXrGOE96ld31RY/T1rRwrqXmmM+XelOKK83yopTRSFGl0z65FMRmUXTUseb374RJ+kf5ZSqVI3BkWD1SzeVILkgb4VnSCwf/Og2wPs30fGxmza2Droc8lQZfoIMoN9Um5OV5mHMrKoef1vIZ69Dha6dnSyo/OS1O+oozmC6bi1a6k5wnD+aDKfXGuq4snMGLQC9ISF/x66nqKHytG6yGqrulzXH7yE5MytsKMsY2DmLkONnZm4wZqN6lQgwKMLJFE3Dw/zZa7KDC+75uLUPVhvdJI7n4Xba4dziOD8pPQyq0k+Jzo0Tp0ny3Y6ethv51MCNNyQvWAyV/h5CAvfrIMUzx4wcn17pjKqWcEE6U/OwPcq2Z0KQKHtnmF9wiyaMdEDUxSVmOoja9LBtmRS4wLfGMVFtSlGYxLts/a7ie2apNgzSifllJgh4TvCnQN1s1vpCM01T/JABD78+gPSiHhMMDHGAM1PzoenjnEvZe/cGw5p/es45eAIU+RxAiy+npxDWSABi82tmdRTTGkT7mxl5Q1FKpL5CbbbF0CPAC/nMpNofle435PUglhEihwDKsrHpNr2B0i7aVA/rOapMYqulW2nH0ZiiiYYODYCjdxE0358crF12g1I8vkBtnepkQ/6WkxtVtQu5hWa8z9nWrtJ8wfufqIg9p2cCgGXJkM6sgfbT1Fu3dHLOk9JXzz00zIN1IoqLj5W5z+k0/Tz3uu7qNTPqafowAdnjbwgAj+PNIulF0JLWfMDaUIquYfh4fWLnEOjJowVkzSMUlRDfHXnKlbwT221SExXmOxicfPd+9Xwng9ji83oXAwGneoZGFzoHaVtZ3KPm4qcMlh0Z1LD0pMiqkUFCgD/K80uU78vtCAlubmapYdaYp+J/flox8HseJhLDGnLGBIxcZHf9ravcNhV35OfnTOGYnTwl4hRIaepqi4hEKMPzRGunha7qrAuMPUOQrLp6QZK/UAoRBNDRjq7nq+8RI9SM0uJLE5mjWm0FJDSH44U+Y0bcq3WBzNTNlLwfAsimZOkd1HCuBa7zv5LUtSJsUJxfJD2tFefnePnNb/DzPa18qxaI3cWj7cVECzWSJim9DVB0/BbHGV4kwKmxn2yG8DZFjPZLGtZZiP/cN9ZQ0JtqrYxcMvjSb0Q8+WmF/wV7ZOjQSh4AKZJSwm4q27akcGKRuoFUyVFDOYOvtD1ICxYBul3Zw61D4G5QxOA0nV/cw7isXFgNxS5ydbnAqOMnXTOyoPlPh2ijh3z8O7la6xA00tkadVfF2oVsI5qIGfbGKu2uqJvwMqAp7nbtraqzaDwYakXe5I7R3H18F5BfV1NXc4sVgyH/8/DWDpvlqtw1QzFGIX2/UVKJX49M/oEHiZDinFAIDkxZ9WUP7xOHrdd4PFAarbYK/kguXY8DbnMraTiBMyELgK7aMkn9Z0Mf/1NwLDuQEGYztYh+86am7W6uERsfeOkuoijNJHgwp4iHy0ZHRzojV5t09tOun3et4UofyA+qximU+Kr4F3tLS9R94i7DQxSCRqTRz81c5rqX7aPqU/xAGpGMAmn6/0BI2YsQ5oIfskwa01Q/+i5AOJCES2kXYAIZaDsYfpJlu/OrVraCTI+fc765KfcP+GT6wsQ8CQl5Uh2dN49mLH7EatK0y3WY7BZLl/qz7emohTkCUFfOjcAF5L0sI/0SnGlgoSPZ9PO+XhE2GdjRW+byI01jKAnbKXUsw/hGHyU5Ehz8QxMZ8fuxL7AS1LEE+57Pzbof3L4J11YDYw/HE6KkFdyzZH424uiMVcH6GKAWJGOA1ruJQ0s7s92tdM3SQRKKT2syoW+RlRVvsp3b70pOUePOFaZ+PrDx0szSiaHy794EXlSpF1NfN/REuMxR/Qub8lOCsoIvRQz5JgX8YQVa3Ao3foHcm8ALQKSVOqWi6GJ1DeHkHhZDSOlBRdU5JugFVoAAT/vi//6Q984p5kWqvf+gyupy0xMo8+i+DR0vygvfUs1b7l583OrClIV1ZnA5k3vQ50zMFJAPIBValhx4Oh0IFW3k291cxfPYGLM5n2u9qzJOFI7VTSpBfTWDjjCheiFYEhqInYFOdfNfGb+6g4Xs47Gi9uHCtws3wf/ZKNyAAAAAA==';

const definition = {
    zigbeeModel: ['Nest Learning Thermostat'],
    model: 'Nest Learning Thermostat (3rd gen, unfr0zen)',
    vendor: 'Google Nest',
    description: 'Nest Learning Thermostat, 3rd generation, freed by unfr0zen (nestlocal)',
    icon: PICTURE,
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
