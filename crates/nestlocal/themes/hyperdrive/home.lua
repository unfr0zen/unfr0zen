-- The home screen as a heads-up display: a bezel of ticks; a gauge of
-- segments lit from the room's temperature to the target in the colour of
-- what the system is doing (orange heating, cyan cooling), with the
-- target's pointer and the room's marker; a ring turning while the system
-- runs; monospace readouts; scan lines over it all.
--
-- What never changes is drawn once into layers. The gauge's segments are
-- one white layer, painted in pieces in each colour.
local anim = require("common:anim")
local geom = require("common:geom")
local palette = require("palette")

local colors, gauge, bezel = palette.colors, palette.gauge, palette.bezel
local pointer_style, marker_style, spinner = palette.pointer, palette.marker, palette.spinner
local brackets, readout, scan = palette.brackets, palette.readout, palette.scan
local polar = geom.polar

local WHITE = "#ffffff"
-- Clips round the gauge's segments reach this far past them, so their
-- antialiased edges are painted too.
local CLIP_MARGIN = 4
-- Widest a gauge clip may span; the spinner's clips are narrower, to keep
-- close to its thin arcs.
local GAUGE_CLIP_SPAN, SPINNER_CLIP_SPAN = 60, 30
-- The spinner's clips reach this many degrees past each dash and units
-- past its arcs.
local SPINNER_CLIP_PAD = 1

-- Celsius halves set as a smaller digit after the whole degrees: its size
-- against theirs, how far up its baseline sits (as a fraction of the size
-- difference, to align their tops) and the gap before it.
local HALF = { scale = 0.46, rise = 0.73, gap = 2 }

local MODE_NAMES = { heat = "HEAT", cool = "COOL", ["heat-cool"] = "AUTO", off = "OFF" }

local home = {}

-- The gauge's angle for a temperature.
local function angle(t, value)
  local f = anim.clamp((value - t.min) / (t.max - t.min), 0, 1)
  return gauge.from + f * (gauge.to - gauge.from)
end

-- What the system is doing: its accent colour and a word for it.
local function status(t)
  if t.range then
    local side = t.range == "low" and "HEAT" or "COOL"
    local accent = t.range == "low" and colors.orange or colors.cyan
    return accent, (t.range_editing and "SET " or "SELECT ") .. side
  end
  if t.mode == "off" then return colors.comment, "SYSTEM OFF" end
  if t.hvac == "heating" then return colors.orange, "HEATING" end
  if t.hvac == "cooling" then return colors.cyan, "COOLING" end
  return colors.green, "STANDBY"
end

local function running(t)
  return t.hvac == "heating" or t.hvac == "cooling"
end

-- A temperature as shown: whole degrees in Fahrenheit, halves in Celsius.
local function format_temperature(value, unit)
  if unit == "F" then return string.format("%d", math.floor(value + 0.5)) end
  return string.format("%.1f", math.floor(value * 2 + 0.5) / 2)
end

local function two_digits(n) return string.format("%02d", n) end

-- Draws a temperature in whole digits, a Celsius half after them smaller
-- and top-aligned. `align` is "left" (default), "center" or "right" about
-- x. Returns its width and its left.
local function draw_temperature(g, value, unit, x, base, size, color, align)
  local text = format_temperature(value, unit)
  local whole, half = text:match("^(.-)%.(.)$")
  whole = whole or text
  half = half == "5" and half or nil
  local big = { font = "mono_light", size = size, color = color }
  local small = { font = big.font, size = size * HALF.scale, color = color }
  local whole_w = g:measure(whole, big)
  local width = whole_w + (half and g:measure(half, small) + HALF.gap or 0)
  if align == "right" then x = x - width elseif align == "center" then x = x - width / 2 end
  g:text(whole, x, base, big)
  if half then
    g:text(half, x + whole_w + HALF.gap, base - (size - small.size) * HALF.rise, small)
  end
  return width, x
end

-- Paints, in `color`, the gauge segments whose middles lie between angles
-- a0 and a1.
local function paint_segments(gfx, c, a0, a1, color)
  local half = gauge.step / 2
  local first = math.ceil((a0 - gauge.from - half) / gauge.step - 1e-6)
  local last = math.floor((a1 - gauge.from - half) / gauge.step + 1e-6)
  if last < first then return end
  local from, to = gauge.from + first * gauge.step, gauge.from + (last + 1) * gauge.step
  local clips = geom.ring_clips(c, gauge.inner - CLIP_MARGIN, gauge.outer + CLIP_MARGIN, from, to, GAUGE_CLIP_SPAN)
  for _, clip in ipairs(clips) do
    gfx:paint_layer("segments", color, clip)
  end
end

-- The spinner's clips, each over a dash: painting through them leaves out
-- the transparent rest of its dial-sized layer.
local spinner_clips
local function get_spinner_clips(c)
  if spinner_clips then return spinner_clips end
  spinner_clips = {}
  local inner = spinner.radius - spinner.inner_gap - 2 * SPINNER_CLIP_PAD
  local outer = spinner.radius + spinner.width / 2 + SPINNER_CLIP_PAD
  for i = 0, spinner.dashes - 1 do
    local a0 = 360 * i / spinner.dashes - SPINNER_CLIP_PAD
    local a1 = a0 + spinner.length + 2 * SPINNER_CLIP_PAD
    for _, clip in ipairs(geom.ring_clips(c, inner, outer, a0, a1, SPINNER_CLIP_SPAN)) do
      spinner_clips[#spinner_clips + 1] = clip
    end
  end
  return spinner_clips
end

-- The bezel's ticks and rings, and the brackets round the readout.
local function draw_bezel(g, c)
  g:circle(c, c, c, colors.dial)
  local minor, major = {}, {}
  for i = 0, bezel.count - 1 do
    local is_major = i % bezel.major == 0
    local a = 360 * i / bezel.count
    geom.push_spoke(is_major and major or minor, c, a, is_major and bezel.inner or bezel.minor, bezel.outer)
  end
  g:lines(minor, 1.5, colors.rule)
  g:lines(major, 2.5, colors.comment)
  g:ring(c, c, bezel.outer + bezel.rim, 1.5, colors.rule)
  g:ring(c, c, gauge.inner - palette.inner_ring.inset, 1, colors.gutter)
  local segs = {}
  for _, corner in ipairs({ { -1, -1 }, { 1, -1 }, { -1, 1 }, { 1, 1 } }) do
    local sx, sy = corner[1], corner[2]
    local x, y = c + sx * brackets.half_width, c + brackets.drop + sy * brackets.half_height
    geom.push_line(segs, x, y, x - sx * brackets.arm, y)
    geom.push_line(segs, x, y, x, y - sy * brackets.arm)
  end
  g:lines(segs, 2, colors.rule)
end

-- The layers drawn once: the bezel, the gauge's segments and the spinner's
-- dashes in white, and the scan lines.
local function define_layers(gfx, c, size)
  gfx:layer("bezel", "1", function(g) draw_bezel(g, c) end)
  gfx:layer("segments", "1", function(g)
    local n = math.floor((gauge.to - gauge.from) / gauge.step + 0.5)
    for i = 0, n - 1 do
      local a0 = gauge.from + i * gauge.step + gauge.step * (1 - gauge.fill) / 2
      local a1 = a0 + gauge.step * gauge.fill
      g:polygon(geom.ring_quad(c, gauge.inner, gauge.outer, a0, a1), WHITE)
    end
  end)
  gfx:layer("spinner", "1", function(g)
    for i = 0, spinner.dashes - 1 do
      local a = 360 * i / spinner.dashes
      g:arc(c, c, spinner.radius, a, a + spinner.length, spinner.width, WHITE)
      g:arc(c, c, spinner.radius - spinner.inner_gap, a + spinner.length * 0.6, a + spinner.length, 1.5, WHITE)
    end
  end)
  gfx:layer("scan", tostring(scan.spacing), function(g)
    local segs = {}
    for y = 0, size, scan.spacing do geom.push_line(segs, 0, y, size, y) end
    g:lines(segs, 1, WHITE)
  end)
end

-- Label-value pairs, e.g. {"AMB", "21°", color}, as one run centred on x:
-- labels dim, values in their colours.
local function draw_label_values(g, x, y, items, size)
  local label = { font = "mono", size = size, color = colors.comment }
  local value = { font = "mono_medium", size = size }
  local spacing = size * 1.5
  local total = 0
  for i, item in ipairs(items) do
    total = total + g:measure(item[1] .. " ", label) + g:measure(item[2], value) + (i > 1 and spacing or 0)
  end
  local at = x - total / 2
  for i, item in ipairs(items) do
    if i > 1 then at = at + spacing end
    at = at + g:text(item[1] .. " ", at, y, label)
    value.color = item[3]
    at = at + g:text(item[2], at, y, value)
  end
end

-- The readouts: the time, the status, the target (or range, or OFF), the
-- room's temperature and humidity, and the mode.
local function draw_readout(g, c, t, clock)
  local accent, word = status(t)
  local hour = clock.hour
  if clock.twelve_hour then hour = (hour + 11) % 12 + 1 end
  g:text(two_digits(hour) .. ":" .. two_digits(clock.minute), c, readout.clock.y,
    { font = "mono", size = readout.clock.size, color = colors.comment, align = "center" })
  g:text("▸ " .. tr(word), c, readout.status.y,
    { font = "mono_medium", size = readout.status.size, spacing = 2, color = accent, align = "center" })

  if t.mode == "off" then
    g:text("OFF", c, readout.off.y, { font = "mono_light", size = readout.off.size, color = colors.comment, align = "center" })
  elseif t.mode == "heat-cool" then
    local range = readout.range
    -- The end not picked dims while one is.
    local function tone(color, side) return (t.range and t.range ~= side) and colors.rule or color end
    draw_temperature(g, t.low, t.unit, c - range.gap, range.y, range.size, tone(colors.orange, "low"), "right")
    g:text("/", c, range.y, { font = "mono_light", size = range.slash_size, color = colors.comment, align = "center" })
    draw_temperature(g, t.high, t.unit, c + range.gap, range.y, range.size, tone(colors.cyan, "high"))
  else
    local target = readout.target
    local w, x = draw_temperature(g, t.target, t.unit, c - target.shift, target.y, target.size, colors.bright, "center")
    g:text("°" .. t.unit, x + w + target.unit_gap, target.unit_y, { font = "mono", size = target.unit_size, color = colors.blue })
  end

  local items = { { "AMB", format_temperature(t.current, t.unit) .. "°", colors.green } }
  if t.humidity then
    items[2] = { "HUM", string.format("%d%%", math.floor(t.humidity + 0.5)), colors.purple }
  end
  draw_label_values(g, c, readout.ambient.y, items, readout.ambient.size)

  local mode = readout.mode
  g:text("[ ", c - mode.bracket_gap, mode.y, { font = "mono", size = mode.size, color = colors.comment, align = "right" })
  g:text(MODE_NAMES[t.mode] or string.upper(t.mode), c, mode.y,
    { font = "mono_medium", size = mode.size, spacing = 2, color = colors.purple, align = "center" })
  g:text(" ]", c + mode.bracket_gap, mode.y, { font = "mono", size = mode.size, color = colors.comment })
end

-- The target's pointer at angle a: a notch outside the gauge and a
-- hairline across it.
local function draw_pointer(gfx, c, a, color)
  local tip_x, tip_y = polar(c, gauge.outer + pointer_style.tip, a)
  local left_x, left_y = polar(c, gauge.outer + pointer_style.base, a - pointer_style.spread)
  local right_x, right_y = polar(c, gauge.outer + pointer_style.base, a + pointer_style.spread)
  gfx:polygon({ tip_x, tip_y, right_x, right_y, left_x, left_y }, color)
  local x1, y1 = polar(c, gauge.inner - pointer_style.hairline, a)
  local x2, y2 = polar(c, gauge.outer, a)
  gfx:line(x1, y1, x2, y2, 2, color)
end

-- The room's marker at angle a: a diamond inside the gauge, pointing out.
local function draw_marker(gfx, c, a, color)
  local x, y = polar(c, gauge.inner - marker_style.inset, a)
  -- Unit vectors outward (ox, oy) and across (-oy, ox).
  local ox, oy = math.sin(math.rad(a)), -math.cos(math.rad(a))
  local s = marker_style.size
  gfx:polygon({ x + ox * s, y + oy * s, x - oy * s, y + ox * s, x - ox * s, y - oy * s, x + oy * s, y - ox * s }, color)
end

-- The gauge, pointers and marker: drawn again only when the thermostat
-- changes.
local function draw_gauge(g, c, t, accent)
  g:clear(colors.bg)
  g:paint_layer("bezel", WHITE)
  g:paint_layer("segments", colors.gutter)
  local a_current = angle(t, t.current)
  if t.mode == "heat-cool" then
    local a_low, a_high = angle(t, t.low), angle(t, t.high)
    paint_segments(g, c, a_low, a_high, colors.rule)
    if running(t) then
      -- Lit from the end being acted on to the room.
      local a = t.hvac == "heating" and a_low or a_high
      paint_segments(g, c, math.min(a, a_current), math.max(a, a_current), accent)
    end
    draw_pointer(g, c, a_low, (t.range == "high") and colors.rule or colors.orange)
    draw_pointer(g, c, a_high, (t.range == "low") and colors.rule or colors.cyan)
  elseif t.mode ~= "off" then
    local a_target = angle(t, t.target)
    local near, far = math.min(a_target, a_current), math.max(a_target, a_current)
    paint_segments(g, c, gauge.from, near, colors.rule)
    paint_segments(g, c, near, far, accent)
    draw_pointer(g, c, a_target, colors.yellow)
  end
  draw_marker(g, c, a_current, colors.green)
end

local function dial(gfx, ui, c)
  local t, clock = ui.thermostat, ui.clock
  local accent = status(t)
  define_layers(gfx, c, ui.dial.size)

  local gauge_key = table.concat({ t.mode, t.hvac, t.target, t.current, t.unit, t.low, t.high, t.min, t.max,
                                   t.range or "", tostring(t.range_editing) }, "|")
  gfx:layer("base", gauge_key, function(g) draw_gauge(g, c, t, accent) end)
  gfx:paint_layer("base", WHITE)

  if running(t) then
    gfx:save()
    gfx:translate(c, c)
    gfx:rotate((ui.now * spinner.speed) % 360)
    gfx:translate(-c, -c)
    for _, clip in ipairs(get_spinner_clips(c)) do gfx:paint_layer("spinner", accent, clip) end
    gfx:restore()
  end

  local readout_key = table.concat({ gauge_key, tostring(t.humidity), clock.hour, clock.minute,
                                     tostring(clock.twelve_hour) }, "|")
  gfx:layer("overlay", readout_key, function(g)
    draw_readout(g, c, t, clock)
    g:paint_layer("scan", scan.shade)
  end)
  gfx:paint_layer("overlay", WHITE)
  return running(t)
end

-- Draws the home screen; true while the spinner turns.
function home.draw(gfx, ui)
  gfx:clear(colors.bg)
  return dial(gfx, ui, ui.dial.size / 2)
end

return home
