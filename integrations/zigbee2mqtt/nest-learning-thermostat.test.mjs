import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { runInNewContext } from "node:vm";

// Exercise the converter's own adapters without a Zigbee2MQTT installation.
const source = readFileSync(new URL("./nest-learning-thermostat.mjs", import.meta.url), "utf8")
  .replace(/^import .*;\n/gm, "")
  .replace("export default [definition, gen2];", "[definition, gen2];");
const [definition] = runInNewContext(source, {
  exposes: { presets: {}, access: {} },
  fz: {},
  tz: {},
  logger: { debug() {} },
  isDummyDevice: () => true,
});
const writer = (key) => definition.toZigbee.find((c) => c?.key.includes(key));
function endpoint(mode) {
  const writes = [];
  return {
    writes,
    read: async () => ({ programingOperMode: mode }),
    write: async (cluster, values) => writes.push([cluster, { ...values }]),
  };
}

test("preset preserves the schedule bit whether running or paused", async () => {
  for (const schedule of [0, 1]) {
    for (const preset of ["eco", "none"]) {
      const entity = endpoint(schedule | (preset === "eco" ? 0 : 4));
      await writer("preset").convertSet(entity, "preset", preset);
      assert.deepEqual(entity.writes, [
        ["hvacThermostat", { programingOperMode: schedule | (preset === "eco" ? 4 : 0) }],
      ]);
    }
  }
});

test("repeating a preset never writes the bitmap or cancels a hold", async () => {
  for (const schedule of [0, 1]) {
    for (const preset of ["eco", "none"]) {
      const entity = endpoint(schedule | (preset === "eco" ? 4 : 0));
      await writer("preset").convertSet(entity, "preset", preset);
      assert.equal(entity.writes.length, 0);
    }
  }
});

test("schedule switch preserves the independent economy bit", async () => {
  for (const economy of [0, 4]) {
    for (const enabled of ["ON", "OFF"]) {
      const entity = endpoint(economy | (enabled === "ON" ? 0 : 1));
      await writer("schedule_enabled").convertSet(entity, "schedule_enabled", enabled);
      assert.deepEqual(entity.writes, [
        ["hvacThermostat", { programingOperMode: economy | (enabled === "ON" ? 1 : 0) }],
      ]);
    }
  }
});

test("bitmap writes require a fresh read and reject invalid inputs before writing", async () => {
  for (const [key, value] of [
    ["preset", "invalid"],
    ["schedule_enabled", "invalid"],
    ["hold", "invalid"],
    ["hold_duration", 0],
    ["hold_duration", 1441],
    ["hold_duration", 1.5],
  ]) {
    const entity = endpoint(1);
    await assert.rejects(writer(key).convertSet(entity, key, value));
    assert.equal(entity.writes.length, 0);
  }
  for (const [key, value] of [
    ["preset", "eco"],
    ["schedule_enabled", "ON"],
  ]) {
    const entity = endpoint(undefined);
    await assert.rejects(writer(key).convertSet(entity, key, value), /unavailable/);
    assert.equal(entity.writes.length, 0);
  }
});

test("hold and duration use separate standard attributes", async () => {
  const entity = endpoint(1);
  await writer("hold_duration").convertSet(entity, "hold_duration", 30);
  await writer("hold").convertSet(entity, "hold", "ON");
  await writer("hold").convertSet(entity, "hold", "OFF");
  assert.deepEqual(entity.writes, [
    ["hvacThermostat", { tempSetpointHoldDuration: 30 }],
    ["hvacThermostat", { tempSetpointHold: 1 }],
    ["hvacThermostat", { tempSetpointHold: 0 }],
  ]);
});

test("hot water boost accepts whole minutes instead of preset choices", async () => {
  const entity = endpoint(1);
  const meta = {
    device: {
      getEndpoint: (id) => {
        assert.equal(id, 44);
        return entity;
      },
    },
  };
  for (const minutes of [0, 1, 37, 75, 120]) {
    const result = await writer("hot_water_boost").convertSet(null, "hot_water_boost", minutes, meta);
    assert.equal(result.state.hot_water_boost, minutes);
    assert.deepEqual(entity.writes.at(-1), ["genAnalogOutput", { presentValue: minutes }]);
  }
  assert.equal(
    (await writer("hot_water_boost").convertSet(null, "hot_water_boost", "off", meta)).state.hot_water_boost,
    0,
  );
  for (const bad of [-1, 121, 1.5, "", "37junk", true, null, NaN, Infinity]) {
    const before = entity.writes.length;
    await assert.rejects(writer("hot_water_boost").convertSet(null, "hot_water_boost", bad, meta));
    assert.equal(entity.writes.length, before);
  }
});

test("hot water boost reports exact numeric minutes and zero when stopped", () => {
  const message = (minutes) => ({ endpoint: { ID: 44 }, data: { presentValue: minutes } });
  const converter = definition.fromZigbee.find(
    (c) => c?.cluster === "genAnalogOutput" && c.convert(null, message(30))?.hot_water_boost !== undefined,
  );
  for (const minutes of [0, 1, 37, 75, 120]) {
    assert.equal(converter.convert(null, message(minutes)).hot_water_boost, minutes);
  }
  for (const bad of [NaN, Infinity, -1, 1.5]) {
    assert.equal(converter.convert(null, message(bad)), undefined);
  }
});

test("hold expiry reads Nest's manufacturer attribute and converts its epoch", async () => {
  const requests = [];
  await writer("hold_until").convertGet({ read: async (...args) => requests.push(args) });
  assert.equal(requests[0][0], "hvacThermostat");
  assert.equal(requests[0][1][0], 0xf000);
  assert.equal(requests[0][2].manufacturerCode, 0x10d6);
  const converter = definition.fromZigbee.find(
    (c) => c?.cluster === "hvacThermostat" && c.convert(null, { data: { 61440: 3600 } })?.hold_until !== undefined,
  );
  assert.equal(converter.convert(null, { data: { 61440: 3600 } }).hold_until, 946688400);
  assert.equal(converter.convert(null, { data: { 61440: 0xffffffff } }).hold_until, 0);
});

test("reports expose schedule, hold and remembered duration independently", () => {
  const state = {};
  for (const converter of definition.fromZigbee.filter(
    (c) => c?.cluster === "hvacThermostat" && c.type.includes("attributeReport"),
  )) {
    Object.assign(
      state,
      converter.convert(null, { data: { programingOperMode: 5, tempSetpointHold: 0, tempSetpointHoldDuration: 120 } }),
    );
  }
  assert.deepEqual(state, { preset: "eco", schedule_enabled: "ON", hold: "OFF", hold_duration: 120 });
});

test("configure completes on firmware that refuses the hold expiry attribute", async () => {
  const reads = [];
  const endpoint = {
    supportsInputCluster: () => true,
    bind: async () => {},
    unbind: async () => {},
    configureReporting: async () => {},
    read: async (cluster, _attributes, options) => {
      if (options?.manufacturerCode) {
        throw new Error("UNSUP_MANUF_GENERAL_COMMAND");
      }
      reads.push(cluster);
    },
  };
  await definition.configure({ getEndpoint: () => endpoint }, {});
  assert.ok(reads.includes("msTemperatureMeasurement"), "reads after the refused one still run");
});

test("the power source says whether the unit has outside power", () => {
  const power = definition.fromZigbee.find((c) => c?.cluster === "genBasic");
  for (const [source, external] of [
    [0x01, true],
    [0x03, false],
    [0x81, true],
  ]) {
    assert.equal(power.convert({}, { data: { powerSource: source } }).external_power, external, String(source));
  }
  assert.equal(power.convert({}, { data: { modelId: "Nest" } }), undefined);
});

test("configure reports the battery's charge and reads the power source again", async () => {
  const done = [];
  const endpoint = {
    supportsInputCluster: () => true,
    bind: async () => {},
    unbind: async () => {},
    configureReporting: async (cluster, [{ attribute }]) => done.push(`report ${cluster} ${attribute}`),
    read: async (cluster, attributes) => done.push(`read ${cluster} ${attributes.join()}`),
  };
  await definition.configure({ getEndpoint: () => endpoint }, {});
  assert.ok(done.includes("report genPowerCfg batteryPercentageRemaining"));
  assert.ok(done.includes("read genBasic powerSource"));
});
