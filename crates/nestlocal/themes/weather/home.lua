-- The home screen as a window on the weather outside: the sky as it is
-- now (the time of day from the real sunrise and sunset, the cloud cover),
-- the sun or the moon on their arc, stars on clear nights, drifting
-- clouds, fog, rain, snow, lightning and drops on the glass; over it the
-- clock, the outside temperature, the target and the next hours.
--
-- Depth, back to front: sky, stars, sun or moon, clouds, fog, far rain and
-- snow, lightning, the readout, near rain and snow, drops on the glass.
-- Without weather from Home Assistant it shows a clear sky for the time of
-- day and leaves the outside out.
local thermostat = require("common:thermostat")
local palette = require("palette")
local sky = require("sky")
local sprites = require("sprites")
local weather = require("weather")

local colors, text = palette.colors, palette.text

local home = {}

-- The outside temperature, rounded as the unit shows its own.
local function outside_text(celsius, unit)
  return thermostat.text(thermostat.shown(celsius, unit), unit == "F" and "F" or "C") .. "°"
end

-- Text with a soft shadow under it, to read on any sky.
local function shadowed(g, s, x, y, style)
  local color = style.color
  style.color = colors.shadow
  g:text(s, x + 1.5, y + 2, style)
  style.color = color
  return g:text(s, x, y, style)
end

-- A temperature in large type centred on x: whole degrees, a Celsius half
-- smaller and level with their tops.
local function big_number(g, value, unit, x, y, size, color)
  local whole, half = thermostat.digits(value, unit)
  local big = { font = "light", size = size, color = color }
  local small = { font = "light", size = size * text.target.half, color = color }
  local w = g:measure(whole, big)
  local hw = half and g:measure(half, small) or 0
  local left = x - (w + hw) / 2
  shadowed(g, whole, left, y, big)
  if half then
    shadowed(g, half, left + w + 2, y - (size - small.size) * 0.7, small)
  end
end

-- What the system is doing, in words and their colour.
local function status(t)
  if t.range then
    local side = t.range == "low" and "heat" or "cool"
    return (t.range_editing and "Press to set " or "Press to change ") .. side,
      t.range == "low" and colors.heat or colors.cool
  end
  local state = thermostat.state(t)
  if state == "heating" then
    return "Heating", colors.heat
  end
  if state == "cooling" then
    return "Cooling", colors.cool
  end
  if state == "off" then
    return "Off", colors.soft
  end
  if state == "heat-cool" then
    return "Heat · Cool", colors.soft
  end
  if state == "eco" then
    return "Eco", colors.soft
  end
  return state == "cool" and "Cool" or "Heat", colors.soft
end

-- The next hours' forecast: those still to come, as many as fit.
local function upcoming(w, clock)
  local out = {}
  local hourly = w.forecast and w.forecast.hourly or {}
  for _, h in ipairs(hourly) do
    if h.time and h.temp_c and h.time > clock.epoch - 1800 and #out < text.forecast.count then
      out[#out + 1] = h
    end
  end
  return out
end

-- The readout: drawn into a layer, again only when what it shows changes.
local function readout(g, c, t, w, kind, clock)
  local s = text
  g:text(
    thermostat.time_text(clock),
    c,
    s.clock.y,
    { font = "medium", size = s.clock.size, color = colors.faint, align = "center" }
  )
  if w.temp_c then
    local line = outside_text(w.temp_c, t.unit) .. (kind.name and "  ·  " .. tr(kind.name) or "")
    shadowed(g, line, c, s.outside.y, { font = "medium", size = s.outside.size, color = colors.text, align = "center" })
  end

  if t.mode == "off" then
    shadowed(g, tr("Off"), c, s.off.y, { font = "light", size = s.off.size, color = colors.text, align = "center" })
  elseif t.mode == "heat-cool" then
    local function tone(side)
      return (t.range and t.range ~= side) and colors.dim or colors.text
    end
    big_number(g, t.low, t.unit, c - s.range.gap, s.range.y, s.range.size, tone("low"))
    big_number(g, t.high, t.unit, c + s.range.gap, s.range.y, s.range.size, tone("high"))
    shadowed(
      g,
      "·",
      c,
      s.range.y - s.range.size * 0.25,
      { font = "light", size = s.range.size * 0.5, color = colors.faint, align = "center" }
    )
  else
    big_number(g, t.target, t.unit, c, s.target.y, s.target.size, colors.text)
  end

  local words, accent = status(t)
  shadowed(g, tr(words), c, s.status.y, { font = "medium", size = s.status.size, color = accent, align = "center" })
  local inside = tr("Inside") .. " " .. thermostat.text(t.current, t.unit) .. "°"
  if t.humidity then
    inside = inside .. "  ·  " .. string.format("%d%%", math.floor(t.humidity + 0.5))
  end
  shadowed(g, inside, c, s.inside.y, { font = "regular", size = s.inside.size, color = colors.soft, align = "center" })

  local hours = upcoming(w, clock)
  local f = s.forecast
  for i, h in ipairs(hours) do
    local x = c + (i - (#hours + 1) / 2) * f.step
    local hour = thermostat.hour_text(clock, thermostat.local_hour(clock, h.time))
    shadowed(g, hour, x, f.hour_y, { font = "regular", size = f.hour_size, color = colors.faint, align = "center" })
    shadowed(
      g,
      outside_text(h.temp_c, t.unit),
      x,
      f.temp_y,
      { font = "medium", size = f.temp_size, color = colors.text, align = "center" }
    )
  end
end

-- The readout's key: everything it shows.
local function readout_key(t, w, clock)
  local parts = {
    t.mode,
    t.unit,
    t.target,
    t.low,
    t.high,
    t.current,
    tostring(t.humidity),
    t.hvac,
    t.range or "",
    tostring(t.range_editing),
    clock.hour,
    clock.minute,
    tostring(clock.twelve_hour),
    tostring(w.temp_c),
    tostring(w.condition or w.icon),
  }
  for _, h in ipairs(upcoming(w, clock)) do
    parts[#parts + 1] = h.time .. ":" .. h.temp_c
  end
  return table.concat(parts, "|")
end

function home.draw(gfx, ui)
  local size = ui.dial.size
  local c = size / 2
  local t, clock, now = ui.thermostat, ui.clock, ui.now
  local w = ui.nest.weather or {}
  local kind = weather.kind(w)
  local time = sky.time(clock, w)

  sprites.define(gfx)
  local top, bottom = sky.colors(time, kind)
  sky.draw_gradient(gfx, size, top, bottom)
  local twinkling = sky.draw_stars(gfx, time, kind, now)
  sky.draw_sun(gfx, time, kind)
  sky.draw_moon(gfx, time, kind, size, top, bottom)
  weather.draw_clouds(gfx, kind, time.daylight, now)
  weather.draw_fog(gfx, kind, now)
  weather.draw_precipitation(gfx, kind, now, false)
  weather.draw_lightning(gfx, kind, now)

  sprites.stamp(
    gfx,
    "glow",
    c,
    c + 20,
    palette.scrim.radius,
    "#000000" .. string.format("%02x", math.floor(255 * palette.scrim.alpha))
  )
  gfx:layer("readout", readout_key(t, w, clock), function(g)
    readout(g, c, t, w, kind, clock)
  end)
  gfx:paint_layer("readout", "#ffffff")

  weather.draw_precipitation(gfx, kind, now, true)
  weather.draw_glass(gfx, kind, now)

  -- Precipitation needs every frame; drifting clouds, fog and stars a
  -- calmer rate; a clear sky only the next minute's clock and light.
  if weather.busy(kind) then
    return true
  end
  if kind.clouds > 0 or kind.fog or twinkling then
    gfx:wake(1 / palette.rate.calm)
  else
    gfx:wake(thermostat.next_minute(clock))
  end
  return false
end

return home
