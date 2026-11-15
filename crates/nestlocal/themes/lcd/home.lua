-- The home screen as a round digital thermostat LCD: the whole screen is
-- the glass, in a bezel ring; a bar graph of blocks round the rim lit up
-- to the target, a pointer at the room's temperature; seven-segment
-- figures for the target, the time, the room and the humidity; printed
-- labels for the modes with the one in use dark; and a backlight that
-- comes on as the ring turns and fades a while after.
--
-- The glass's colour is painted every frame (the backlight fades);
-- everything printed on it is one layer, redrawn when it changes.
local anim = require("common:anim")
local color = require("common:color")
local geom = require("common:geom")
local thermostat = require("common:thermostat")
local palette = require("palette")
local segments = require("segments")

local colors, glass, bars, layout, motion = palette.colors, palette.glass, palette.bars, palette.layout, palette.motion

local WHITE = "#ffffff"
local WEEKDAYS = { "SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT" }
-- The modes printed along the bottom, and which of the thermostat's each
-- stands for.
local MODES = { { "HEAT", "heat" }, { "COOL", "cool" }, { "AUTO", "heat-cool" }, { "ECO", "eco" }, { "OFF", "off" } }

local home = {}

local function label(g, text, x, y, fill, align)
  g:text(text, x, y, { font = "medium", size = layout.label_size, color = fill, align = align or "left", spacing = 1 })
end

-- A temperature as segments: whole degrees, and in Celsius the tenths.
local function figures(value, unit)
  local whole, half = thermostat.digits(value, unit)
  if unit == "F" then return whole, nil end
  return whole, half or "0"
end

-- The bar graph: blocks lit from the start of the scale up to the
-- target (in heat-cool, between the two ends), the rest faint; and the
-- room's pointer inside it.
local function draw_bars(g, c, t, ink, faint)
  local step = (bars.to - bars.from) / bars.count
  local lo, hi = t.min, t.target
  if t.mode == "heat-cool" then lo, hi = t.low, t.high end
  if t.mode == "off" then hi = t.min - 1 end
  for i = 0, bars.count - 1 do
    local a0 = bars.from + i * step + step * (1 - bars.fill) / 2
    local value = t.min + (i + 0.5) / bars.count * (t.max - t.min)
    local lit = value >= lo and value <= hi
    g:polygon(geom.ring_quad(c, bars.inner, bars.outer, a0, a0 + step * bars.fill), lit and ink or faint)
  end
  local a = anim.mix(bars.from, bars.to, thermostat.fraction(t, t.current))
  local tip_x, tip_y = geom.polar(c, bars.inner - bars.pointer - bars.pointer_depth, a)
  local spread = 2.2
  local lx, ly = geom.polar(c, bars.inner - bars.pointer, a - spread)
  local rx, ry = geom.polar(c, bars.inner - bars.pointer, a + spread)
  g:polygon({ tip_x, tip_y, rx, ry, lx, ly }, ink)
end

-- The target in large figures centred on c, the tenths smaller and level
-- with their tops; the unit after them, level with their tops too.
local function draw_target(g, c, value, unit, top, h, ink)
  local whole, tenth = figures(value, unit)
  local small = h * layout.target.half
  local w = segments.measure(whole, h)
  local tw = tenth and segments.measure(tenth, small) + h * 0.12 or 0
  local x = c - (w + tw) / 2
  segments.draw(g, whole .. (tenth and "." or ""), x, top, h, ink)
  if tenth then segments.draw(g, tenth, x + w + h * 0.12, top, small, ink) end
  g:text("°" .. unit, x + w + tw + h * 0.06, top, { font = "medium", size = layout.unit_size, color = ink,
                                                 baseline = "top", spacing = 1 })
end

-- What is printed: the time, the bar graph, the target (or range, or
-- OFF), the modes, the room and the humidity. `blink` shows the running
-- mode's "ON".
local function draw_print(g, c, t, clock, blink)
  local ink, faint = colors.ink, color.with_alpha(colors.ink, colors.ghost)
  local dim = color.with_alpha(colors.ink, colors.ghost * 2.5)
  draw_bars(g, c, t, ink, faint)

  local cl = layout.clock
  local time = thermostat.time_text(clock):gsub("^(%d):", " %1:")
  local tw = segments.measure(time, cl.h)
  local day = tr(WEEKDAYS[clock.weekday + 1])
  local day_w = g:measure(day, { font = "medium", size = layout.label_size, spacing = 1 })
  local left = c - (tw + day_w + 10) / 2
  label(g, day, left, cl.y + cl.h, ink)
  segments.draw(g, time, left + day_w + 10, cl.y, cl.h, ink)

  if t.mode == "off" then
    local h = layout.target.h
    segments.draw(g, "OFF", c - segments.measure("OFF", h) / 2, layout.target.y, h, ink)
  elseif t.mode == "heat-cool" then
    local r = layout.range
    for i, side in ipairs({ "low", "high" }) do
      local value = side == "low" and t.low or t.high
      local w, tenth = figures(value, t.unit)
      local text = w .. (tenth and "." .. tenth or "")
      local width = segments.measure(text, r.h)
      local x = i == 1 and c - r.gap - width or c + r.gap
      local shown = (t.range and t.range ~= side) and dim or ink
      label(g, side == "low" and tr("HEAT TO") or tr("COOL TO"), x, r.label_y, shown)
      segments.draw(g, text, x, r.y, r.h, shown)
    end
  else
    label(g, tr("SET"), layout.target.label_x, layout.target.label_y, ink)
    draw_target(g, c, t.target, t.unit, layout.target.y, layout.target.h, ink)
  end

  -- The modes, centred; the running one says "ON", blinking.
  local modes = layout.modes
  local running = thermostat.running(t)
  local style = { font = "medium", size = layout.label_size, spacing = 1 }
  local texts, total = {}, 0
  for i, m in ipairs(MODES) do
    local active = t.mode == m[2] or (m[2] == "eco" and t.eco)
    texts[i] = { tr(m[1]) .. ((active and running) and " " .. tr("ON") or ""), active }
    total = total + g:measure(texts[i][1], style) + (i > 1 and modes.gap or 0)
  end
  local x = c - total / 2
  for _, m in ipairs(texts) do
    local text = m[1]
    -- Blinking leaves the space "ON" takes.
    if m[2] and running and not blink then text = text:gsub(" " .. tr("ON") .. "$", "") end
    label(g, text, x, modes.y, m[2] and ink or dim)
    x = x + g:measure(m[1], style) + modes.gap
  end

  -- The room and the humidity, side by side under the modes.
  local room = layout.room
  local whole, tenth = figures(t.current, t.unit)
  local inside = whole .. (tenth and "." .. tenth or "")
  local iw = segments.measure(inside, room.h)
  local humidity = t.humidity and string.format("%d", math.floor(t.humidity + 0.5))
  local hw = humidity and segments.measure(humidity, room.h) + 16 or 0
  local inside_label = tr("INSIDE")
  local lw = g:measure(inside_label, style) + 8
  local total_w = lw + iw + (humidity and room.gap + hw or 0)
  local at = c - total_w / 2
  label(g, inside_label, at, room.y + room.h, ink)
  segments.draw(g, inside, at + lw, room.y, room.h, ink)
  if humidity then
    local hx = at + lw + iw + room.gap
    segments.draw(g, humidity, hx, room.y, room.h, ink)
    label(g, "%", hx + hw, room.y + room.h, ink, "right")
  end
end

-- How lit the backlight is (0 to 1): on while the ring turns and for
-- motion.backlight seconds after, and as the screen comes home; then
-- fading. Whether it moves, and how long it stays on.
local function backlight(ui)
  local since = ui.now - math.max(ui.changed.turned, ui.changed.screen)
  if since < 0 then since = 0 end
  if since < motion.backlight then return 1, false, motion.backlight - since end
  local p = (since - motion.backlight) / motion.fade
  if p >= 1 then return 0, false, nil end
  return 1 - anim.ease_in_out(p), true, nil
end

function home.draw(gfx, ui)
  local c = ui.dial.size / 2
  local t, clock = ui.thermostat, ui.clock
  gfx:clear(colors.bg)

  gfx:layer("bezel", "1", function(g)
    g:circle(c, c, glass.radius + glass.bezel, colors.bezel_edge)
    g:circle(c, c, glass.radius + glass.bezel - 2, colors.bezel)
  end)
  gfx:paint_layer("bezel", WHITE)
  local light, fading, hold = backlight(ui)
  gfx:circle(c, c, glass.radius, color.mix(colors.panel, colors.lit, light))
  -- The glass's inner shadow under the bezel.
  gfx:ring(c, c, glass.radius - glass.shade / 2, glass.shade, colors.shade)

  local running = thermostat.running(t)
  local blink = not running or (ui.now % motion.blink) < motion.blink / 2
  local key = table.concat({ t.mode, t.unit, t.target, t.low, t.high, t.current, t.min, t.max, tostring(t.humidity),
                             t.hvac, t.range or "", tostring(t.range_editing), tostring(t.eco), clock.weekday,
                             clock.hour, clock.minute, tostring(clock.twelve_hour), tostring(blink) }, "|")
  gfx:layer("print", key, function(g) draw_print(g, c, t, clock, blink) end)
  gfx:paint_layer("print", WHITE)

  -- The next frame: while the backlight fades, at once; else when the
  -- "ON" blinks, the backlight goes out or the minute turns.
  if fading then return true end
  local wake = thermostat.next_minute(clock)
  if running then wake = math.min(wake, motion.blink / 2 - ui.now % (motion.blink / 2)) end
  if hold then wake = math.min(wake, hold) end
  gfx:wake(wake)
  return false
end

return home
