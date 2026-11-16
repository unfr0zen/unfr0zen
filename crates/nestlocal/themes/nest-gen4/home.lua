-- The home screen as the 4th gen Nest Learning Thermostat's: black, with a
-- soft glow behind the number in the colour of what the system is doing
-- while it runs; a faint track open at the bottom, temperatures rising
-- clockwise along it, with dots for the room and the target; the room's
-- number and the mode's symbol inside the target's dot; the target in
-- large type, the status under it, and a leaf for a setpoint that saves
-- energy.
--
-- Motion:
--   * changed digits of the number roll, up as it rises, down as it falls;
--   * turning the wheel swaps the dots for a scale of ticks, lit from the
--     room to the target, which fades a moment after the wheel stops;
--   * marks glide to new temperatures;
--   * the glow fades in and out and crossfades between heating and cooling;
--   * coming home, the track draws in from its ends as the rest fades up.
--
-- Cost: the glow, track, scale and each digit are layers drawn once and
-- painted where they go; the marks and the words are layers redrawn only
-- when they change. A still screen costs a few paints.
local anim = require("common:anim")
local color = require("common:color")
local geom = require("common:geom")
local palette = require("palette")

local colors, ring, ticks, glow, number = palette.colors, palette.ring, palette.ticks, palette.glow, palette.number
local status_style, leaf_style, motion = palette.status, palette.leaf, palette.motion
local polar, alpha = geom.polar, color.alpha

local WHITE = "#ffffff"
-- A time before anything happened, for state that has not changed yet.
local LONG_AGO = -100

-- Eco thresholds, in Celsius: heating to at most this, or cooling to at
-- least that, earns the leaf.
local ECO_HEAT_MAX, ECO_COOL_MIN = 18.5, 26

-- Figtree's metrics, as fractions of the font size: the top of a digit
-- above the baseline, the baseline below the top of a glyph's layer, the
-- padding either side of a glyph in its layer, and the window the rolling
-- digits show through (above and below the baseline).
local FIGTREE = { digit_top = 0.7, ascent = 0.85, pad = 0.06, window_top = 0.8, window_bottom = 0.07 }
-- Units between the whole degrees and the half.
local HALF_GAP = 1

-- The mode symbols' size; the room's number's font, half its height and
-- how far its baseline sits below the point it is placed at.
local SYMBOL = 18
local ROOM_LABEL_FONT = { font = "figtree_medium", size = 20 }
local ROOM_LABEL = { half_height = 7, drop = 6 }
-- A symbol overlaps the room's number when closer than this across and up
-- or down; once it does, it must move `hysteresis` units further to stop.
local OVERLAP = { x = 30, y = 18, hysteresis = 6 }

-- How close a glide (and the number's dimming) is to land.
local GLIDE_NEAR, LABEL_FADE_NEAR = 0.05, 0.01
-- A digit's roll takes this fraction of the time since the value before
-- it, so it keeps up with the wheel.
local ROLL_PACE = 0.9

-- Clips round the track and the marks: a little past the track's ends,
-- each quad at most `span` degrees.
local CLIP = { from = -150, to = 150, span = 30 }
local TRACK_CLIP = 8
local MARKS_CLIP = { inside = 95, outside = 20 }
local TICK_CLIP_PAD = 2
-- The bars of the targets' glow.
local GLOW_STEPS = 4
-- Units round the glow's square kept in its clip.
local GLOW_CLIP_PAD = 4
-- Below this the glow is not painted.
local GLOW_VISIBLE = 0.001
-- The layer the words and leaf are drawn into: its top-left on the dial
-- and its size.
local WORDS_BOX = { x = 90, y = 325, w = 300, h = 135 }

local home = {}

---------------------------------------------------------------------------
-- The thermostat
---------------------------------------------------------------------------

local function celsius(t, value)
  if t.unit == "F" then
    return (value - 32) * 5 / 9
  end
  return value
end

-- Whether the target saves energy, earning the leaf.
local function eco(t)
  if t.mode == "heat" then
    return celsius(t, t.target) <= ECO_HEAT_MAX
  end
  if t.mode == "cool" then
    return celsius(t, t.target) >= ECO_COOL_MIN
  end
  return false
end

local function running(t)
  return t.mode ~= "off" and (t.hvac == "heating" or t.hvac == "cooling")
end

-- The glow's colour for what the system is doing.
local function glow_color_for(t)
  return t.hvac == "cooling" and colors.cool_glow or colors.heat_glow
end

-- The colour of a heat-cool end's mark: dim while the other end is picked.
local function range_color(t, side)
  local other = side == "low" and "high" or "low"
  if t.range == other then
    return colors.dim
  end
  return side == "low" and colors.heat or colors.cool
end

-- The target's colour outside heat-cool.
local function target_color(t)
  return t.mode == "cool" and colors.cool or colors.heat
end

-- Whole degrees as text, and in Celsius "5" for a half, else nil.
local function digits(value, unit)
  if unit == "F" then
    return string.format("%d", math.floor(value + 0.5)), nil
  end
  local halves = math.floor(value * 2 + 0.5)
  return string.format("%d", math.floor(halves / 2)), halves % 2 == 1 and "5" or nil
end

-- A temperature as the room's number by its mark shows it: "21.5".
local function bare(value, unit)
  local whole, half = digits(value, unit)
  return whole .. (half and "." .. half or "")
end

-- A temperature as small text: "21.5°".
local function short(value, unit)
  return bare(value, unit) .. "°"
end

-- The track's angle for a temperature.
local function angle(t, value)
  local f = anim.clamp((value - t.min) / (t.max - t.min), 0, 1)
  return ring.from + f * (ring.to - ring.from)
end

-- The words under the number, and their colour.
local function status(t)
  if t.range then
    local side = t.range == "low" and "heat" or "cool"
    local text = (t.range_editing and "Press to set " or "Press to change ") .. side
    return text, t.range == "low" and colors.heat or colors.cool
  end
  if t.mode == "off" then
    return "Inside " .. short(t.current, t.unit), colors.secondary
  end
  if t.hvac == "heating" then
    return "Heating", colors.heat
  end
  if t.hvac == "cooling" then
    return "Cooling", colors.cool
  end
  if t.mode == "heat-cool" then
    return "Heat • Cool", colors.secondary
  end
  return t.mode == "cool" and "Cool" or "Heat", colors.secondary
end

---------------------------------------------------------------------------
-- Symbols, `s` units across, centred on (x, y)
---------------------------------------------------------------------------

-- Heat: three wavy lines rising.
local function heat_symbol(g, x, y, s, line_color)
  local segs = {}
  local steps = 12
  for i = -1, 1 do
    local px, py
    for j = 0, steps do
      local f = j / steps
      local qx = x + i * s * 0.32 + math.sin(f * 2 * math.pi) * s * 0.09
      local qy = y + s * 0.5 - f * s
      if px then
        geom.push_line(segs, px, py, qx, qy)
      end
      px, py = qx, qy
    end
  end
  g:lines(segs, s * 0.1, line_color)
end

-- Cool: a snowflake of six spokes, each with a pair of barbs.
local function cool_symbol(g, x, y, s, line_color)
  local segs, r = {}, s * 0.5
  for i = 0, 5 do
    local a = 60 * i
    local ex, ey = polar(0, r, a)
    geom.push_line(segs, x, y, x + ex, y + ey)
    local mx, my = polar(0, r * 0.62, a)
    for _, side in ipairs({ -45, 45 }) do
      local bx, by = polar(0, r * 0.3, a + side)
      geom.push_line(segs, x + mx, y + my, x + mx + bx, y + my + by)
    end
  end
  g:lines(segs, s * 0.09, line_color)
end

local LEAF_TILT = math.rad(40)

-- The leaf: two arcs meeting at the tip and the stem, tilted 40° right,
-- with its vein cut out in the background colour.
local function leaf_symbol(g, x, y, s, fill)
  local pts, steps = {}, 16
  local half = s * 0.5
  local cos_t, sin_t = math.cos(LEAF_TILT), math.sin(LEAF_TILT)
  for side = 0, 1 do
    for i = 0, steps do
      local f = side == 0 and i / steps or 1 - i / steps
      local along = -half + f * s
      local across = math.sin(f * math.pi) * s * 0.32 * (side == 0 and 1 or -1)
      pts[#pts + 1] = x + across * cos_t - along * sin_t
      pts[#pts + 1] = y + across * sin_t + along * cos_t
    end
  end
  g:polygon(pts, fill)
  local function on_axis(along)
    return x - along * sin_t, y + along * cos_t
  end
  local x1, y1 = on_axis(half * 0.75)
  local x2, y2 = on_axis(-half * 0.45)
  g:line(x1, y1, x2, y2, s * 0.08, colors.bg, true)
end

---------------------------------------------------------------------------
-- The rolling number
--
-- Each digit is drawn once into a layer of its own and painted through a
-- window round the number, so a changed digit can slide out of it as the
-- new one slides in. Digits sit centred in cells of one width so they stay
-- put as their neighbours change.
---------------------------------------------------------------------------

local measures = {}
local function measure(g, text, size)
  local key = size .. " " .. text
  if not measures[key] then
    measures[key] = g:measure(text, { font = "figtree", size = size })
  end
  return measures[key]
end

-- The baseline that puts a half at size `small` level at the top with
-- whole degrees at `size` on `base`.
local function half_base(base, size, small)
  return base - (size - small) * FIGTREE.digit_top
end

-- The layer holding `ch` at `size`, white on clear: its name, its padding
-- left of the glyph, and its width and height.
local function glyph(g, ch, size)
  local name = "glyph " .. ch .. " " .. size
  local pad = math.ceil(size * FIGTREE.pad)
  local w, h = math.ceil(measure(g, ch, size) + 2 * pad), math.ceil(size)
  g:layer(name, "1", function(l)
    l:text(ch, pad, size * FIGTREE.ascent, { font = "figtree", size = size, color = WHITE, align = "left" })
  end, w, h)
  return name, pad, w, h
end

-- Paints `ch` at `size` in `fill`, its left at x and its baseline at
-- `base` + dy, showing only between screen heights top and bottom.
local function paint_glyph(g, ch, x, base, size, fill, dy, top, bottom)
  local name, pad, w, h = glyph(g, ch, size)
  local lx, ly = math.floor(x - pad + 0.5), math.floor(base - size * FIGTREE.ascent + dy + 0.5)
  local y0, y1 = math.max(0, top - ly), math.min(h, bottom - ly)
  if y1 <= y0 then
    return
  end
  g:save()
  g:translate(lx, ly)
  g:paint_layer(name, fill, { 0, y0, w, y0, w, y1, 0, y1 })
  g:restore()
end

-- A digit's cell at `size`: number.cell of the widest digit.
local function cell(g, size)
  local widest = 0
  for d = 0, 9 do
    widest = math.max(widest, measure(g, tostring(d), size))
  end
  return widest * number.cell
end

-- The width of `text` with its digits in cells.
local function run_width(g, text, size)
  local w = 0
  for i = 1, #text do
    local ch = text:sub(i, i)
    w = w + (ch:match("%d") and cell(g, size) or measure(g, ch, size))
  end
  return w
end

-- The left of each character of `text` set from x, digits centred in
-- their cells.
local function lefts(g, text, size, x)
  local out, at = {}, x
  for i = 1, #text do
    local ch = text:sub(i, i)
    local w = measure(g, ch, size)
    if ch:match("%d") then
      local cw = cell(g, size)
      out[i] = at + (cw - w) / 2
      at = at + cw
    else
      out[i] = at
      at = at + w
    end
  end
  return out
end

-- Paints `old` (its left at old_x) turning into `new` (left at new_x) by
-- progress q from 0 to 1. Characters are paired from the right: a pair
-- that matches glides across; one that differs rolls, the old one out of
-- the window and the new one in, upwards when `dir` is 1, down when -1.
local function roll_run(g, old, new, old_x, new_x, base, size, fill, q, dir)
  local top, bottom = base - size * FIGTREE.window_top, base + size * FIGTREE.window_bottom
  local travel = bottom - top
  local old_lefts, new_lefts = lefts(g, old, size, old_x), lefts(g, new, size, new_x)
  local n = math.max(#old, #new)
  for i = 1, n do
    local oi, ni = #old - n + i, #new - n + i
    local oc = oi >= 1 and old:sub(oi, oi) or nil
    local nc = ni >= 1 and new:sub(ni, ni) or nil
    if oc == nc then
      local x = old_lefts[oi] + (new_lefts[ni] - old_lefts[oi]) * q
      paint_glyph(g, nc, x, base, size, fill, 0, top, bottom)
    else
      if oc and q < 1 then
        paint_glyph(g, oc, old_lefts[oi], base, size, fill, -dir * travel * q, top, bottom)
      end
      if nc then
        paint_glyph(g, nc, new_lefts[ni], base, size, fill, dir * travel * (1 - q), top, bottom)
      end
    end
  end
end

-- Each number's roll, by slot ("set", "low", "high"):
--   value          the value rolling in (or shown)
--   target         the latest value asked for
--   from, to       the digits rolling out and in: { whole, half or "" }
--   at, length     when the roll started and how long it takes
--   pace           how long the next roll takes
--   changed        when `target` last changed
--   dir            1 rolling up, -1 down
--
-- A new value rolls in at the wheel's pace: as long as the wait since the
-- value before (motion.roll after a pause, no quicker than
-- motion.roll_min). A roll under way when the value changes again is
-- hurried to its end at that pace, never cut, and the next goes straight
-- to the latest value, so the number keeps up with the wheel and stops
-- with it.
local rolls = {}

local function shown_digits(value, unit)
  local whole, half = digits(value, unit)
  return { whole, half or "" }
end

-- Advances `slot`'s roll towards `value`: the roll, its eased progress and
-- whether it is still moving.
local function advance_roll(slot, value, unit, now)
  local r = rolls[slot]
  if not r then
    r = {
      value = value,
      target = value,
      from = shown_digits(value, unit),
      to = shown_digits(value, unit),
      at = LONG_AGO,
      length = motion.roll,
      pace = motion.roll,
      changed = LONG_AGO,
      dir = 1,
    }
    rolls[slot] = r
  end
  if value ~= r.target then
    r.pace = anim.clamp((now - r.changed) * ROLL_PACE, motion.roll_min, motion.roll)
    r.changed, r.target = now, value
    local elapsed = now - r.at
    if elapsed < r.length then
      r.length = math.min(r.length, elapsed + r.pace * (1 - elapsed / r.length))
    end
  end
  if now - r.at >= r.length and r.value ~= r.target then
    r.dir = r.target > r.value and 1 or -1
    r.from, r.to, r.value = r.to, shown_digits(r.target, unit), r.target
    r.at, r.length = now, r.pace
  end
  local p = anim.clamp((now - r.at) / r.length, 0, 1)
  return r, anim.ease_in_out(p), p < 1 or r.value ~= r.target
end

-- A number rolling to `value`: whole degrees centred on x, the half
-- smaller after them, level at the top. Whether it moves.
local function rolling_number(g, slot, x, value, unit, base, size, fill, now)
  local r, q, moving = advance_roll(slot, value, unit, now)
  local small = size * number.half
  local old_w, new_w = run_width(g, r.from[1], size), run_width(g, r.to[1], size)
  local old_x, new_x = x - old_w / 2, x - new_w / 2
  roll_run(g, r.from[1], r.to[1], old_x, new_x, base, size, fill, q, r.dir)
  roll_run(
    g,
    r.from[2],
    r.to[2],
    old_x + old_w + HALF_GAP,
    new_x + new_w + HALF_GAP,
    half_base(base, size, small),
    small,
    fill,
    q,
    r.dir
  )
  return moving
end

-- The target, heat-cool's range, or "Off", at opacity `fade_in`. Whether
-- it moves.
local function draw_number(g, c, t, fade_in, now)
  local o = alpha(fade_in)
  if t.mode == "off" then
    g:text(
      tr("Off"),
      c,
      number.base - number.off_drop,
      { font = "figtree", size = number.size * number.off_scale, color = colors.text .. o, align = "center" }
    )
    return false
  elseif t.mode == "heat-cool" then
    local function tone(side)
      return ((t.range and t.range ~= side) and colors.dim or colors.text) .. o
    end
    local low = rolling_number(
      g,
      "low",
      c - number.range_gap,
      t.low,
      t.unit,
      number.range_base,
      number.range_size,
      tone("low"),
      now
    )
    local high = rolling_number(
      g,
      "high",
      c + number.range_gap,
      t.high,
      t.unit,
      number.range_base,
      number.range_size,
      tone("high"),
      now
    )
    g:circle(c, number.range_base - number.range_size * number.range_dot_rise, number.range_dot, colors.secondary .. o)
    return low or high
  end
  return rolling_number(g, "set", c, t.target, t.unit, number.base, number.size, colors.text .. o, now)
end

---------------------------------------------------------------------------
-- The track and the scale
---------------------------------------------------------------------------

-- Clips over the whole track's band, cached by radii.
local band_clips = {}
local function track_band(c, inner, outer)
  local key = table.concat({ c, inner, outer }, ":")
  if not band_clips[key] then
    band_clips[key] = geom.ring_clips(c, inner, outer, CLIP.from, CLIP.to, CLIP.span)
  end
  return band_clips[key]
end

-- The track at opacity `opacity`, its ends fading out; drawn in from both
-- ends to `extent` (0 to 1) while coming home.
local function draw_track(gfx, c, t, extent, opacity)
  local function draw(g, reach, line_color)
    local half = (ring.to - ring.from) / 2 * reach
    local left, right = ring.from + half, ring.to - half
    local stop = math.min(left, ring.from + ring.fade)
    if stop > ring.from then
      g:gradient_arc(
        c,
        c,
        ring.radius,
        ring.from,
        stop,
        ring.width,
        line_color .. "00",
        line_color .. alpha((stop - ring.from) / ring.fade)
      )
    end
    if left > stop then
      g:arc(c, c, ring.radius, stop, left, ring.width, line_color)
    end
    local start = math.max(right, ring.to - ring.fade)
    if start > right then
      g:arc(c, c, ring.radius, right, start, ring.width, line_color)
    end
    if start < ring.to then
      g:gradient_arc(
        c,
        c,
        ring.radius,
        start,
        ring.to,
        ring.width,
        line_color .. alpha((ring.to - start) / ring.fade),
        line_color .. "00"
      )
    end
  end
  local track_color = t.mode == "off" and colors.track_off or colors.track
  if extent >= 1 then
    gfx:layer("track", "1", function(g)
      draw(g, 1, WHITE)
    end)
    for _, clip in ipairs(track_band(c, ring.radius - TRACK_CLIP, ring.radius + TRACK_CLIP)) do
      gfx:paint_layer("track", track_color .. alpha(opacity), clip)
    end
  else
    gfx:save()
    gfx:alpha(opacity)
    draw(gfx, extent, track_color)
    gfx:restore()
  end
end

-- The scale at opacity `s`: a fine pale tick every ticks.step degrees,
-- bolder and white from the room to the target (across the range in
-- heat-cool), and longer bars for the room and the targets. Each kind of
-- tick is one layer, painted through clips whose edges fall between ticks.
local function draw_scale(gfx, c, t, a, s)
  local inner, outer = ticks.radius - ticks.length, ticks.radius
  local function tick_layer(name, width)
    gfx:layer(name, table.concat({ ticks.step, ticks.radius, ticks.length, width }, " "), function(g)
      local segs = {}
      for i = 0, math.floor((ring.to - ring.from) / ticks.step + 0.001) do
        geom.push_spoke(segs, c, ring.from + i * ticks.step, inner, outer)
      end
      g:lines(segs, width, WHITE)
    end)
  end
  tick_layer("ticks", ticks.width)
  tick_layer("lit_ticks", ticks.lit_width)

  local lo, hi
  if t.mode == "heat-cool" then
    lo, hi = math.min(a.room, a.low), math.max(a.room, a.high)
  else
    lo, hi = math.min(a.room, a.set), math.max(a.room, a.set)
  end
  -- The lit stretch runs from the tick nearest `lo` to the one nearest
  -- `hi`; clip edges sit half a step beyond ticks.
  local function nearest(at)
    return ring.from + math.floor((at - ring.from) / ticks.step + 0.5) * ticks.step
  end
  local lit_from, lit_to = nearest(lo) - ticks.step / 2, nearest(hi) + ticks.step / 2
  local first, last = ring.from - ticks.step / 2, ring.to + ticks.step / 2
  local function paint(name, fill, from, to)
    from, to = math.max(from, first), math.min(to, last)
    if to <= from then
      return
    end
    for _, clip in ipairs(geom.ring_clips(c, inner - TICK_CLIP_PAD, outer + TICK_CLIP_PAD, from, to, CLIP.span)) do
      gfx:paint_layer(name, fill, clip)
    end
  end
  local dim = colors.tick .. alpha(s * ticks.dim)
  paint("ticks", dim, first, lit_from)
  paint("lit_ticks", colors.text .. alpha(s), lit_from, lit_to)
  paint("ticks", dim, lit_to, last)

  -- A round-ended bar across the scale at angle `at`.
  local function bar(at, from, to, width, bar_color)
    local x1, y1 = polar(c, from, at)
    local x2, y2 = polar(c, to, at)
    gfx:line(x1, y1, x2, y2, width, bar_color, true)
  end
  bar(a.room, ticks.room_inner, ticks.room_outer, ticks.room_width, colors.room .. alpha(s))
  -- The targets' bars glow: wider bars under them, fainter the wider.
  local function target_bar(at, bar_color)
    for i = GLOW_STEPS, 1, -1 do
      local f = i / GLOW_STEPS
      bar(
        at,
        ticks.bar_inner,
        ticks.bar_outer,
        ticks.bar_width + ticks.glow * f,
        bar_color .. alpha(s * ticks.glow_alpha * (1 - f) * 2)
      )
    end
    bar(at, ticks.bar_inner, ticks.bar_outer, ticks.bar_width, bar_color .. alpha(s))
  end
  if t.mode == "heat-cool" then
    target_bar(a.low, range_color(t, "low"))
    target_bar(a.high, range_color(t, "high"))
  else
    target_bar(a.set, target_color(t))
  end
end

-- The dots on the track at rest, at opacity `opacity`.
local function draw_dots(gfx, c, t, a, opacity)
  local o = alpha(opacity)
  local function dot(at, dot_color)
    local x, y = polar(c, ring.radius, at)
    gfx:circle(x, y, ring.dot, dot_color .. o)
  end
  if t.mode == "heat-cool" then
    dot(a.low, range_color(t, "low"))
    dot(a.high, range_color(t, "high"))
  else
    dot(a.set, target_color(t))
  end
  dot(a.room, colors.room)
end

-- How much of the scale shows: all of it while the wheel turns and for
-- motion.hold seconds after, then fading out over motion.fade. Whether it
-- moves.
local function scale_opacity(ui)
  local turned = ui.changed.turned
  local since = ui.now - turned
  if turned < 0 or since >= motion.hold + motion.fade then
    return 0, false
  end
  if since < motion.hold then
    return 1, true
  end
  return 1 - anim.ease_in_out((since - motion.hold) / motion.fade), true
end

-- The marks' angles, gliding to where the thermostat puts them.
local followers = {
  set = anim.follower(motion.dot, GLIDE_NEAR),
  low = anim.follower(motion.dot, GLIDE_NEAR),
  high = anim.follower(motion.dot, GLIDE_NEAR),
  room = anim.follower(motion.room, GLIDE_NEAR),
}

-- The angles drawn now for "set", "low", "high" and "room", and whether
-- any is still gliding.
local function mark_angles(t, now)
  local values = { set = t.target + (t.nudge or 0), low = t.low, high = t.high, room = t.current }
  local a, moving = {}, false
  for _, k in ipairs({ "set", "low", "high", "room" }) do
    local m
    a[k], m = followers[k](angle(t, values[k]), now)
    moving = moving or m
  end
  return a, moving
end

---------------------------------------------------------------------------
-- The marks: the room's number and the mode's symbols inside the track
---------------------------------------------------------------------------

-- How far a box `hw` by `hh` units from its centre to its sides reaches
-- towards angle `a`.
local function box_reach(a, hw, hh)
  local r = math.rad(a)
  return hw * math.abs(math.sin(r)) + hh * math.abs(math.cos(r))
end

-- Where the room's number (`label_w` units wide) and each symbol stand,
-- as { x, y } by "label", "set", "low", "high": ticks.margin units inside
-- the dots, `clear` units further in while the scale's bars reach past
-- them.
local function mark_spots(c, a, clear, label_w)
  local edge = ring.radius - ring.dot - clear - ticks.margin
  local lx, ly = polar(c, edge - box_reach(a.room, label_w / 2, ROOM_LABEL.half_height), a.room)
  local spots = { label = { lx, ly } }
  for _, k in ipairs({ "set", "low", "high" }) do
    local sx, sy = polar(c, edge - box_reach(a[k], SYMBOL / 2, SYMBOL / 2), a[k])
    spots[k] = { sx, sy }
  end
  return spots
end

-- Whether a symbol at `spot` overlaps the room's number at `label`, with
-- `margin` units to spare.
local function overlaps(spot, label, margin)
  return math.abs(spot[1] - label[1]) < OVERLAP.x + margin and math.abs(spot[2] - label[2]) < OVERLAP.y + margin
end

-- Draws the mode's symbols and the room's number at opacity
-- `label_opacity`, at the spots `p` from place_marks. The number is drawn
-- last, over a symbol it overlaps.
local function draw_marks(g, t, p, label_opacity)
  if t.mode == "heat-cool" then
    heat_symbol(g, p.low[1], p.low[2], SYMBOL, range_color(t, "low"))
    cool_symbol(g, p.high[1], p.high[2], SYMBOL, range_color(t, "high"))
  else
    local symbol = t.mode == "cool" and cool_symbol or heat_symbol
    symbol(g, p.set[1], p.set[2], SYMBOL, target_color(t))
  end
  local style = {
    font = ROOM_LABEL_FONT.font,
    size = ROOM_LABEL_FONT.size,
    color = colors.room .. alpha(label_opacity),
    align = "center",
  }
  g:text(bare(t.current, t.unit), p.label[1], p.label[2] + ROOM_LABEL.drop, style)
end

-- How far the marks are pushed in for the scale's bars, and the room's
-- number's opacity while a symbol overlaps it.
local inset = anim.follower(motion.dot, GLIDE_NEAR)
local label_dim = anim.follower(motion.dot, LABEL_FADE_NEAR)
local covered = false

-- The marks' spots, pushed in past the scale's bars by its opacity `s`;
-- each stays on its own angle, however close the others come. While a
-- symbol overlaps the room's number, the number dims to ticks.under, until
-- the symbol is OVERLAP.hysteresis units clear. Returns the spots, the
-- number's opacity and whether anything is moving.
local function place_marks(g, c, t, a, s, now)
  -- In from the dots at rest to inside the bars while the scale shows.
  local clear, moving = inset((ring.radius - ring.dot - math.min(ticks.bar_inner, ticks.room_inner)) * s, now)
  local p = mark_spots(c, a, clear, (g:measure(bare(t.current, t.unit), ROOM_LABEL_FONT)))
  local drawn = t.mode == "heat-cool" and { "low", "high" } or { "set" }
  local over = false
  for _, k in ipairs(drawn) do
    over = over or overlaps(p[k], p.label, covered and OVERLAP.hysteresis or 0)
  end
  covered = over
  local shown, fading = label_dim(covered and ticks.under or 1, now)
  return p, shown, moving or fading
end

---------------------------------------------------------------------------
-- The glow
---------------------------------------------------------------------------

-- The glow's colour and strength (1 while the system runs, else 0), and
-- where they were when they last changed, to crossfade from.
local glow_state = { color = nil, strength = 0, from_color = nil, from_strength = 0, at = LONG_AGO }

-- The glow's colour and strength now, and whether it is fading.
local function glow_now(now)
  local p = anim.ease_in_out((now - glow_state.at) / motion.glow)
  local glow_color = glow_state.color
  if glow_state.strength == 0 then
    glow_color = glow_state.from_color
  elseif glow_state.from_strength > 0 then
    glow_color = color.mix(glow_state.from_color, glow_state.color, p)
  end
  return glow_color, anim.mix(glow_state.from_strength, glow_state.strength, p), p < 1
end

-- Paints the glow behind the number while the system runs, scaled by
-- `fade_in`. Whether it is fading.
local function paint_glow(gfx, ui, c, fade_in)
  local t = ui.thermostat
  local on = running(t)
  local light = glow_color_for(t)
  local want = on and light or (glow_state.color or light)
  local level = on and 1 or 0
  if glow_state.color == nil then
    glow_state.color, glow_state.from_color = want, want
    glow_state.strength, glow_state.from_strength = level, level
  elseif want ~= glow_state.color or level ~= glow_state.strength then
    glow_state.from_color, glow_state.from_strength = glow_now(ui.now)
    glow_state.color, glow_state.strength, glow_state.at = want, level, ui.now
  end
  local glow_color, strength, fading = glow_now(ui.now)
  if strength > GLOW_VISIBLE then
    local cy = c + glow.drop
    gfx:layer("glow", table.concat({ glow.radius, glow.steps }, " "), function(g)
      g:clear(colors.bg)
      g:radial_glow(c, cy, glow.radius, glow.steps)
    end)
    -- Only the glow's square: the layer beyond it is black, like the
    -- cleared screen.
    local r = glow.radius + GLOW_CLIP_PAD
    gfx:paint_layer(
      "glow",
      color.scale(glow_color, strength * fade_in),
      { c - r, cy - r, c + r, cy - r, c + r, cy + r, c - r, cy + r }
    )
  end
  return fading
end

---------------------------------------------------------------------------
-- The screen
---------------------------------------------------------------------------

-- The status words and the leaf.
local function draw_words(g, c, t)
  local text, text_color = status(t)
  g:text(
    tr(text),
    c,
    status_style.y,
    { font = "figtree_medium", size = status_style.size, color = text_color, align = "center" }
  )
  if eco(t) then
    leaf_symbol(g, c, leaf_style.y, leaf_style.size, colors.leaf)
  end
end

local function dial(gfx, ui, c)
  local t = ui.thermostat
  -- Coming home, the track draws in from its ends and the rest fades up.
  local intro = ui.screen == "home" and anim.ease_out((ui.now - ui.changed.screen - motion.intro_from) / motion.intro)
    or 1
  local animating = intro < 1
  animating = paint_glow(gfx, ui, c, intro) or animating

  if t.mode ~= "off" then
    local a, moving = mark_angles(t, ui.now)
    local s, scale_moving = scale_opacity(ui)
    if s < 1 then
      draw_track(gfx, c, t, intro, 1 - s)
    end
    if s > 0 then
      draw_scale(gfx, c, t, a, s * intro)
    end
    -- The scale's bars stand for the dots while it shows.
    if s < 1 then
      draw_dots(gfx, c, t, a, (1 - s) * intro)
    end

    local p, shown, gliding = place_marks(gfx, c, t, a, s, ui.now)
    moving = moving or gliding
    if moving or s > 0 or intro < 1 then
      gfx:save()
      gfx:alpha(intro)
      draw_marks(gfx, t, p, shown)
      gfx:restore()
    else
      local key = table.concat({
        t.mode,
        t.unit,
        t.current,
        t.range or "",
        string.format(
          "%.1f %.1f %.1f %.1f %.1f %.1f %.1f %.1f %.2f",
          p.label[1],
          p.label[2],
          p.set[1],
          p.set[2],
          p.low[1],
          p.low[2],
          p.high[1],
          p.high[2],
          shown
        ),
      }, "|")
      gfx:layer("marks", key, function(g)
        draw_marks(g, t, p, shown)
      end)
      for _, clip in ipairs(track_band(c, ring.radius - MARKS_CLIP.inside, ring.radius + MARKS_CLIP.outside)) do
        gfx:paint_layer("marks", WHITE, clip)
      end
    end
    animating = animating or moving or scale_moving
  else
    draw_track(gfx, c, t, intro, 1)
  end

  animating = draw_number(gfx, c, t, intro, ui.now) or animating

  local words_key = table.concat({ (status(t)), tostring(eco(t)) }, "|")
  gfx:layer("words", words_key, function(g)
    g:translate(-WORDS_BOX.x, -WORDS_BOX.y)
    draw_words(g, c, t)
  end, WORDS_BOX.w, WORDS_BOX.h)
  gfx:save()
  gfx:translate(WORDS_BOX.x, WORDS_BOX.y)
  gfx:paint_layer("words", WHITE .. alpha(intro))
  gfx:restore()
  return animating
end

-- Draws the home screen; true while anything moves.
function home.draw(gfx, ui)
  gfx:clear(colors.bg)
  return dial(gfx, ui, ui.dial.size / 2)
end

return home
