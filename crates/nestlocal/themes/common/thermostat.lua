-- The thermostat's readings as themes show them.
local anim = require("anim")

local thermostat = {}

-- Whole degrees as text, and in Celsius "5" for a half, else nil.
function thermostat.digits(value, unit)
  if unit == "F" then return string.format("%d", math.floor(value + 0.5)), nil end
  local halves = math.floor(value * 2 + 0.5)
  return string.format("%d", math.floor(halves / 2)), halves % 2 == 1 and "5" or nil
end

-- A temperature as plain text: "21.5", "21" or, in Fahrenheit, "70".
function thermostat.text(value, unit)
  local whole, half = thermostat.digits(value, unit)
  return whole .. (half and "." .. half or "")
end

-- A Celsius reading (as nestlocal gives the weather's) in the unit shown.
function thermostat.shown(celsius, unit)
  if unit == "F" then return celsius * 9 / 5 + 32 end
  return celsius
end

-- Where `value` falls between the thermostat's lowest and highest
-- targets, from 0 to 1.
function thermostat.fraction(t, value)
  return anim.clamp((value - t.min) / (t.max - t.min), 0, 1)
end

-- Whether the system is heating or cooling.
function thermostat.running(t)
  return t.mode ~= "off" and (t.hvac == "heating" or t.hvac == "cooling")
end

-- What the screen says the system is doing: "heating", "cooling",
-- "heat", "cool", "heat-cool", "eco" or "off", and in heat-cool the end
-- picked ("low" or "high") and whether it is being changed.
function thermostat.state(t)
  if t.mode == "off" then return "off" end
  if t.hvac == "heating" then return "heating" end
  if t.hvac == "cooling" then return "cooling" end
  return t.mode
end

-- The local hour (0 to 23) of epoch seconds `time`.
function thermostat.local_hour(clock, time)
  return math.floor(((time + clock.utc_offset) % 86400) / 3600)
end

-- An hour as the clock shows hours: "15" or "3 PM".
function thermostat.hour_text(clock, hour)
  if not clock.twelve_hour then return string.format("%02d", hour) end
  return string.format("%d %s", (hour + 11) % 12 + 1, hour < 12 and "AM" or "PM")
end

-- The time as the clock shows it: "15:04" or "3:04".
function thermostat.time_text(clock)
  local hour = clock.hour
  if clock.twelve_hour then return string.format("%d:%02d", (hour + 11) % 12 + 1, clock.minute) end
  return string.format("%02d:%02d", hour, clock.minute)
end

-- Seconds until the clock's next minute.
function thermostat.next_minute(clock)
  return 60 - clock.second
end

return thermostat
