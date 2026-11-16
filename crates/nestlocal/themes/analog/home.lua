-- The home screen as an old-school round thermostat: a cover printed with
-- the scale, a red index that travels round its edge to the setpoint as the
-- ring turns (one for each end in heat-cool), a glass window in the middle
-- showing the room's temperature on a needle driven by a bimetal coil, a
-- mode switch and a pilot lamp that lights while the system runs. No digits
-- but the printed scales.
--
-- The cover, the window and the glass are layers drawn once for a unit and
-- range; the switch is a layer redrawn when the mode changes; the index,
-- needle and lamp are drawn every frame.
local anim = require("common:anim")
local color = require("common:color")
local geom = require("common:geom")
local thermostat = require("common:thermostat")
local palette = require("palette")

local colors, cover, scale, index = palette.colors, palette.cover, palette.scale, palette.index
local window, thermometer, needle, coil = palette.window, palette.thermometer, palette.needle, palette.coil
local switch, lamp, motion = palette.switch, palette.lamp, palette.motion
local polar = geom.polar

local WHITE = "#ffffff"
-- How close a glide is to land, in degrees of temperature.
local NEAR = 0.01
-- The switch's positions, and the mode each stands for.
local POSITIONS = { { "HEAT", "heat" }, { "AUTO", "heat-cool" }, { "COOL", "cool" }, { "OFF", "off" } }

local home = {}

-- Whether `v` is a multiple of `step`.
local function multiple(v, step)
  return math.abs(v / step - math.floor(v / step + 0.5)) < 1e-6
end

-- Calls f(v, major) for each tick of `steps` ({ minor, major }) from the
-- thermostat's lowest to its highest target.
local function each_tick(t, steps, f)
  local minor, major = steps[t.unit][1], steps[t.unit][2]
  for v = math.ceil(t.min / minor - 1e-6) * minor, t.max + 1e-6, minor do
    f(v, multiple(v, major))
  end
end

-- The cover's angle for a temperature.
local function scale_angle(t, value)
  return anim.mix(scale.from, scale.to, thermostat.fraction(t, value))
end

-- The cover: shading and the scale, rising clockwise.
local function draw_cover(g, c, t)
  for i = cover.shade_steps, 0, -1 do
    local f = i / cover.shade_steps
    g:circle(c, c, anim.mix(cover.inner, cover.radius, f), color.mix(colors.cover, colors.cover_edge, f ^ 3))
  end
  local minor, major = {}, {}
  each_tick(t, scale.steps, function(v, is_major)
    local a = scale_angle(t, v)
    geom.push_spoke(
      is_major and major or minor,
      c,
      a,
      scale.outer - (is_major and scale.major_length or scale.minor_length),
      scale.outer
    )
    if is_major then
      local x, y = polar(c, scale.label, a)
      g:text(
        string.format("%d", math.floor(v + 0.5)),
        x,
        y,
        { font = "medium", size = scale.label_size, color = colors.print, align = "center", baseline = "middle" }
      )
    end
  end)
  g:lines(minor, scale.minor_width, colors.print)
  g:lines(major, scale.major_width, colors.print)
end

-- The thermometer's angle in the window for a temperature.
local function needle_angle(t, value)
  return anim.mix(thermometer.from, thermometer.to, thermostat.fraction(t, value))
end

-- The window: its face, the thermometer's printed arc and the bimetal coil.
local function draw_window(g, c, t)
  g:circle(c, c, window.radius, colors.window)
  local minor, major = {}, {}
  each_tick(t, thermometer.steps, function(v, is_major)
    local a = needle_angle(t, v)
    geom.push_spoke(
      is_major and major or minor,
      c,
      a,
      thermometer.outer - (is_major and thermometer.major_length or thermometer.minor_length),
      thermometer.outer
    )
    if is_major then
      local x, y = polar(c, thermometer.label, a)
      g:text(
        string.format("%d", math.floor(v + 0.5)),
        x,
        y,
        { font = "medium", size = thermometer.label_size, color = colors.print, align = "center", baseline = "middle" }
      )
    end
  end)
  g:lines(minor, 1.5, colors.print)
  g:lines(major, 2.5, colors.print)
  g:arc(c, c, thermometer.outer, thermometer.from, thermometer.to, 1.5, colors.print)
  -- The coil: a spiral wound out from the pivot.
  local segs, px, py = {}, nil, nil
  for i = 0, coil.steps do
    local f = i / coil.steps
    local x, y = polar(c, anim.mix(coil.inner, coil.outer, f), f * coil.turns * 360)
    if px then
      geom.push_line(segs, px, py, x, y)
    end
    px, py = x, y
  end
  g:lines(segs, coil.width, colors.coil)
end

-- The glass over the window: its rim and the light on its upper left.
local function draw_glass(g, c)
  local r = window.radius - 16
  g:gradient_arc(c, c, r, -78, -40, 9, color.with_alpha(colors.glint, 0), color.with_alpha(colors.glint, 0.55))
  g:gradient_arc(c, c, r, -40, -8, 9, color.with_alpha(colors.glint, 0.55), color.with_alpha(colors.glint, 0))
  g:ring(c, c, window.radius, window.rim, colors.window_rim)
end

-- The mode switch: its words, the track and the knob under the mode.
local function draw_switch(g, c, t)
  local style = { font = "medium", size = switch.size, color = colors.print, spacing = 1 }
  local widths, total = {}, 0
  for i, p in ipairs(POSITIONS) do
    widths[i] = g:measure(tr(p[1]), style)
    total = total + widths[i] + (i > 1 and switch.gap or 0)
  end
  local x, y = c - total / 2, c + switch.y
  g:rect(x - 4, c + switch.track_y, total + 8, switch.track_height, colors.switch_track, switch.track_height / 2)
  for i, p in ipairs(POSITIONS) do
    g:text(tr(p[1]), x, y, style)
    if t.mode == p[2] then
      local mid = x + widths[i] / 2
      g:rect(mid - switch.knob, c + switch.track_y - 2, 2 * switch.knob, switch.track_height + 4, colors.knob, 3)
    end
    x = x + widths[i] + switch.gap
  end
end

-- The index at angle `a` on the cover's edge, pointing in, with a hairline
-- across the ticks.
local function draw_index(g, c, a, fill)
  local tip_x, tip_y = polar(c, index.tip, a)
  local lx, ly = polar(c, index.base, a - index.spread)
  local rx, ry = polar(c, index.base, a + index.spread)
  g:polygon({ tip_x, tip_y, rx, ry, lx, ly }, fill)
  local x1, y1 = polar(c, index.line, a)
  g:line(tip_x, tip_y, x1, y1, index.line_width, fill)
end

-- The needle at angle `a`, with a short tail past the pivot.
local function draw_needle(g, c, a)
  local r = math.rad(a)
  local ux, uy = math.sin(r), -math.cos(r)
  local px, py = -uy * needle.width / 2, ux * needle.width / 2
  local tip_x, tip_y = c + ux * needle.length, c + uy * needle.length
  local tail_x, tail_y = c - ux * needle.tail, c - uy * needle.tail
  g:polygon(
    { tip_x, tip_y, c + px, c + py, tail_x + px, tail_y + py, tail_x - px, tail_y - py, c - px, c - py },
    colors.needle
  )
  g:circle(c, c, needle.hub, colors.needle)
end

-- The pilot lamp: lit in the colour of what runs.
local function draw_lamp(g, c, t)
  local x, y = c, c + lamp.y
  if t.hvac == "heating" or t.hvac == "cooling" then
    local lit = t.hvac == "heating" and colors.heat or colors.cool
    g:circle(x, y, lamp.radius * 2.2, color.with_alpha(lit, 0.25))
    g:circle(x, y, lamp.radius, lit)
  else
    g:circle(x, y, lamp.radius, colors.lamp_off)
  end
end

-- The indexes' followers, by setpoint, and the needle's.
local followers = {
  set = anim.follower(motion.index, NEAR),
  low = anim.follower(motion.index, NEAR),
  high = anim.follower(motion.index, NEAR),
}
local needle_follower = anim.follower(motion.needle, NEAR)

function home.draw(gfx, ui)
  local c = ui.dial.size / 2
  local t, now = ui.thermostat, ui.now
  gfx:clear(colors.bg)
  local key = table.concat({ t.unit, t.min, t.max }, "|")
  gfx:layer("cover", key, function(g)
    draw_cover(g, c, t)
  end)
  gfx:layer("window", key, function(g)
    draw_window(g, c, t)
  end)
  gfx:layer("glass", "1", function(g)
    draw_glass(g, c)
  end)
  gfx:layer("switch", t.mode, function(g)
    draw_switch(g, c, t)
  end)

  gfx:paint_layer("cover", WHITE)
  -- The indexes follow the ring: the target, or each of heat-cool's ends,
  -- the one not picked dimmed.
  local turning = false
  local function place(name, value, fill)
    local shown, moving = followers[name](value, now)
    turning = turning or moving
    draw_index(gfx, c, scale_angle(t, shown), fill)
  end
  if t.mode == "heat-cool" then
    for _, side in ipairs({ "low", "high" }) do
      local dimmed = t.range and t.range ~= side
      place(
        side,
        side == "low" and t.low or t.high,
        dimmed and color.with_alpha(colors.index, index.dim) or colors.index
      )
    end
  elseif t.mode ~= "off" then
    place("set", t.target + (t.nudge or 0), colors.index)
  end

  gfx:paint_layer("window", WHITE)
  draw_lamp(gfx, c, t)
  gfx:paint_layer("switch", WHITE)
  local room, settling = needle_follower(t.current, now)
  draw_needle(gfx, c, needle_angle(t, room))
  gfx:paint_layer("glass", WHITE)
  return turning or settling
end

return home
