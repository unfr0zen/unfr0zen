-- The home screen as a classic round dial thermostat: a domed plastic
-- cover printed with the temperature scale, which turns to bring the
-- target under a lens and red hairline at 12 o'clock (in heat-cool, red
-- and blue tabs on the cover mark the range); inside it a brushed metal
-- face with the room thermometer's card and its needle, the target,
-- what the system is doing, a jewel lamp lit red heating or blue
-- cooling, and the eco leaf in green. Off, the cover and face turn
-- grey. A clear dome's highlights lie over it all.
--
-- The cover, the scale, the face and the gloss are layers drawn once;
-- the scale is painted turned, and the needle and pivot are drawn each
-- frame.
local anim = require("common:anim")
local hex = require("common:color")
local theme = require("palette")
local C, S, K, T = theme.colors, theme.scale, theme.cover, theme.thermometer

local home = {}

local function polar(c, r, degrees)
  local a = math.rad(degrees)
  return c + r * math.sin(a), c - r * math.cos(a)
end

local function celsius(t, value)
  if t.unit == "F" then
    return (value - 32) * 5 / 9
  end
  return value
end

-- The eco leaf: a setpoint that saves energy.
local function eco(t)
  if t.mode == "heat" then
    return celsius(t, t.target) <= 18.5
  end
  if t.mode == "cool" then
    return celsius(t, t.target) >= 26
  end
  return false
end

local function number(value, unit)
  if unit == "F" then
    return string.format("%d", math.floor(value + 0.5))
  end
  local halves = math.floor(value * 2 + 0.5)
  return halves % 2 == 0 and string.format("%d", halves / 2) or string.format("%.1f", halves / 2)
end

-- A temperature's angle on the cover's scale, 0 at its middle: the scale
-- rises counter-clockwise, so the cover turns with the wheel.
local function scale_angle(t, value)
  return (0.5 - (value - t.min) / (t.max - t.min)) * S.span
end

-- A temperature's angle on the thermometer's card.
local function card_angle(t, value)
  return (anim.clamp((value - t.min) / (t.max - t.min), 0, 1) * 2 - 1) * T.half
end

-- What the cover's top reads: the target, or in heat-cool the end picked,
-- else the middle of the range.
local function reading(t)
  if t.mode ~= "heat-cool" then
    return t.target
  end
  if t.range then
    return t[t.range]
  end
  return (t.low + t.high) / 2
end

-- Discs from radius r0 to r1, their middles drifting up and left, shading
-- from one colour to the next: a dome lit from the top left.
local function dome(g, c, r0, r1, drift, stops)
  local n = 72
  for i = 0, n do
    local p = i / n
    local r = r0 + (r1 - r0) * p
    local at = p * (#stops - 1)
    local k = math.min(#stops - 1, math.floor(at))
    local color = hex.mix(stops[k + 1], stops[k + 2] or stops[k + 1], at - k)
    g:circle(c - drift * 0.7 * p, c - drift * p, r, color)
  end
end

-- An arc band from angle a0 to a1, between radii r_in and r_out; `taper`
-- narrows it to nothing at its ends.
local function crescent(c, a0, a1, r_in, r_out, taper)
  local pts, n = {}, 40
  for i = 0, n do
    local x, y = polar(c, r_out, a0 + (a1 - a0) * i / n)
    pts[#pts + 1] = x
    pts[#pts + 1] = y
  end
  for i = n, 0, -1 do
    local p = i / n
    local r = taper and r_out - (r_out - r_in) * math.sin(math.pi * p) or r_in
    local x, y = polar(c, r, a0 + (a1 - a0) * p)
    pts[#pts + 1] = x
    pts[#pts + 1] = y
  end
  return pts
end

-- The cover and the inner face's metal, without anything printed on them.
local function body(g, c, look)
  g:circle(c, c, c, "#1d1913")
  g:circle(c, c, K.outer + 2, look.edge)
  dome(g, c, K.outer, K.groove - 14, 18, { look.edge, look.body, look.body, look.lit })
  g:ring(c, c, K.outer, 1.5, "#00000040")
  -- The groove round the face, its lower side catching the light.
  g:ring(c, c, K.groove + 1, 9, "#0000002a")
  g:ring(c, c, K.groove, 4, "#0000004a")
  g:arc(c, c, K.groove + 4, 95, 265, 1.6, "#ffffff90")
  g:circle(c, c, K.inner + 2, look.metal_dark)
  g:circle(c, c, K.inner + 0.5, "#00000040")
  dome(g, c, K.inner, 40, 26, { look.metal_dark, look.metal, look.metal_lit })
  -- Brushed: fine rings, alternately lighter and darker.
  for r = 6, K.inner - 2, 2 do
    g:ring(c, c, r, 0.8, (r % 4 == 0) and "#ffffff0c" or "#00000009")
  end
end

-- The cover's printing, round its middle: a tick a degree (half a degree
-- in Celsius), longer every whole and every five, a number every five, and
-- the unit at the bottom.
local function scale(g, c, t)
  local step = t.unit == "F" and 1 or 0.5
  local minor, mid, major = {}, {}, {}
  local v = math.ceil(t.min / step) * step
  while v <= t.max + 1e-6 do
    local a = scale_angle(t, v)
    local five = math.abs(v / 5 - math.floor(v / 5 + 0.5)) < 1e-6
    local whole = math.abs(v - math.floor(v + 0.5)) < 1e-6
    local list = five and major or ((whole and step < 1) and mid or minor)
    local r = five and S.tick_major or (list == mid and S.tick_minor - 3 or S.tick_minor)
    local x1, y1 = polar(c, r, a)
    local x2, y2 = polar(c, S.tick_outer, a)
    list[#list + 1] = x1
    list[#list + 1] = y1
    list[#list + 1] = x2
    list[#list + 1] = y2
    if five then
      local x, y = polar(c, S.numbers, a)
      g:save()
      g:translate(x, y)
      g:rotate(a)
      g:text(
        string.format("%d", math.floor(v + 0.5)),
        0,
        0,
        { font = "print", size = 25, color = C.ink, align = "center", baseline = "middle" }
      )
      g:restore()
    end
    v = v + step
  end
  g:lines(minor, 1.4, C.ink_soft)
  g:lines(mid, 1.8, C.ink)
  g:lines(major, 2.6, C.ink)
  g:arc(c, c, S.tick_outer + 1, -S.span / 2 - 1, S.span / 2 + 1, 1.2, C.ink_soft)
  local x, y = polar(c, S.numbers + 4, 180)
  g:save()
  g:translate(x, y)
  g:text("°" .. t.unit, 0, 0, { font = "wide", size = 18, color = C.ink, align = "center", baseline = "middle" })
  g:restore()
end

-- The thermometer's card: a cream arc above the pivot, a tick every two
-- degrees Fahrenheit or every one Celsius, a number every ten or five.
local function card(g, c, t, look)
  local function ink(color)
    return look.off and C.off_ink or color
  end
  g:polygon(crescent(c, -T.half - 6, T.half + 6, T.inner, T.outer + 4), "#00000040")
  g:polygon(crescent(c, -T.half - 5, T.half + 5, T.inner + 1, T.outer + 3), look.off and "#e6e4df" or C.card)
  g:arc(c, c, T.outer + 3, -T.half - 5, T.half + 5, 1.5, C.card_edge)
  g:arc(c, c, T.inner + 1, -T.half - 5, T.half + 5, 1.5, C.card_edge)
  g:arc(c, c, T.outer, -T.half - 4, T.half + 4, 5, "#0000001a")
  local step, every = 2, 10
  if t.unit ~= "F" then
    step, every = 1, 5
  end
  local minor, major = {}, {}
  for v = math.ceil(t.min / step) * step, t.max, step do
    local a = card_angle(t, v)
    local big = v % every == 0
    local x1, y1 = polar(c, big and T.outer - 16 or T.outer - 9, a)
    local x2, y2 = polar(c, T.outer - 1, a)
    local list = big and major or minor
    list[#list + 1] = x1
    list[#list + 1] = y1
    list[#list + 1] = x2
    list[#list + 1] = y2
    if big then
      local x, y = polar(c, T.inner + 15, a)
      g:save()
      g:translate(x, y)
      g:rotate(a)
      g:text(
        string.format("%d", v),
        0,
        0,
        { font = "print", size = 15, color = ink(C.ink), align = "center", baseline = "middle" }
      )
      g:restore()
    end
  end
  g:lines(minor, 1.2, ink(C.ink_soft))
  g:lines(major, 2, ink(C.ink))
end

-- The jewel lamp: a chrome bezel and its glass, lit in `color`.
local function lamp(g, x, y, color)
  if color then
    g:circle(x, y, 19, color .. "26")
    g:circle(x, y, 14, color .. "40")
  end
  g:circle(x, y + 1, 10.5, "#00000050")
  g:circle(x, y, 10, "#8e8778")
  g:ring(x, y, 9, 1.5, "#f4efe4")
  g:circle(x, y, 7, color or C.lamp_off)
  g:circle(x, y + 2, 4.5, color and "#ffffff30" or "#00000020")
  g:circle(x - 2.4, y - 2.6, 2.4, "#ffffffc0")
end

-- The eco leaf, tipped to the right, `s` units long.
local function leaf(g, x, y, s, color)
  local pts, n = {}, 16
  for i = 0, n do
    local p = i / n
    pts[#pts + 1] = -s / 2 + s * p
    pts[#pts + 1] = -s * 0.32 * math.sin(math.pi * p)
  end
  for i = n, 0, -1 do
    local p = i / n
    pts[#pts + 1] = -s / 2 + s * p
    pts[#pts + 1] = s * 0.32 * math.sin(math.pi * p)
  end
  g:save()
  g:translate(x, y)
  g:rotate(-40)
  g:polygon(pts, color)
  g:line(-s / 2 - 3, 0, s * 0.35, 0, 1.4, "#ffffff90")
  g:restore()
end

-- What the system is doing, as a word, and its colour.
local function status(t)
  if t.range then
    local name = t.range == "low" and "heat" or "cool"
    return string.upper((t.range_editing and "Press to set " or "Press to change ") .. name),
      t.range == "low" and C.heat or C.cool
  end
  if t.mode == "off" then
    return nil
  end
  if t.hvac == "heating" then
    return "HEATING", C.heat
  end
  if t.hvac == "cooling" then
    return "COOLING", C.cool
  end
  if t.mode == "heat-cool" then
    return "AUTO", C.ink_soft
  end
  return t.mode == "cool" and "COOL" or "HEAT", C.ink_soft
end

-- The face's printing that follows the thermostat: the target, the status
-- and the lamp with the leaf beside it.
local function readout(g, c, t)
  local big = { font = "wide", size = 40, color = C.ink, align = "center" }
  local y = c + 66
  if t.mode == "off" then
    big.color = C.off_ink
    g:text("OFF", c, y, big)
  elseif t.mode == "heat-cool" then
    local function tone(color, side)
      return (t.range and t.range ~= side) and C.ink_soft .. "80" or color
    end
    local style = { font = "wide", size = 34 }
    style.color, style.align = tone(C.heat, "low"), "right"
    g:text(number(t.low, t.unit), c - 14, y, style)
    g:text("·", c, y - 2, { font = "wide", size = 34, color = C.ink_soft, align = "center" })
    style.color, style.align = tone(C.cool, "high"), "left"
    g:text(number(t.high, t.unit), c + 14, y, style)
  else
    -- The whole degrees centred; a half hangs after the degree sign, so
    -- the digits stay put as it comes and goes.
    local whole, half = number(t.target, t.unit):match("^(%-?%d+)(.*)$")
    local w = g:measure(whole, big)
    g:text(whole, c, y, big)
    g:text("°", c + w / 2 + 1, y - 18, { font = "wide", size = 24, color = C.ink })
    if half ~= "" then
      g:text(half, c + w / 2 + 14, y, { font = "wide", size = 24, color = C.ink })
    end
  end
  local word, color = status(t)
  if word then
    local style = { font = "wide", size = 12, spacing = 2, color = color, align = "center" }
    local w = g:measure(word, style)
    if w > 250 then
      style.size = style.size * 250 / w
    end
    g:text(word, c, c + 92, style)
  end
  local lit = (t.mode ~= "off") and ((t.hvac == "heating" and C.heat) or (t.hvac == "cooling" and C.cool)) or nil
  if t.mode ~= "off" and eco(t) then
    lamp(g, c - 16, c + 122, lit)
    leaf(g, c + 18, c + 122, 26, C.eco)
  else
    lamp(g, c, c + 122, lit)
  end
end

-- The clear dome's highlights and shadows, each a stack of faint
-- crescents for a soft edge.
local function soft(g, c, a0, a1, r_in, r_out, color, layers)
  for i = 1, layers do
    local f = i / layers
    g:polygon(
      crescent(
        c,
        a0 + (a1 - a0) * 0.2 * (1 - f),
        a1 - (a1 - a0) * 0.2 * (1 - f),
        r_out - (r_out - r_in) * f,
        r_out,
        true
      ),
      color
    )
  end
end

local function gloss(g, c)
  soft(g, c, -82, -4, K.outer - 34, K.outer - 5, "#ffffff16", 4)
  soft(g, c, 100, 172, K.outer - 30, K.outer - 2, "#0000000c", 4)
  soft(g, c, -86, -8, K.inner - 40, K.inner - 5, "#ffffff12", 4)
  soft(g, c, 96, 176, K.inner - 30, K.inner - 2, "#0000000c", 4)
  for i = 1, 4 do
    g:circle(c - 92, c - 98, 14 - i * 3, "#ffffff14")
  end
end

-- The lens over the scale at 12 o'clock, its hairline and the brass index
-- above it; the hairline in its colour, when given.
local function lens(gfx, c, hairline)
  local top, bottom = c - K.outer + 4, c - K.groove - 4
  gfx:rect(c - 22, top + 3, 44, bottom - top, "#00000022", 9)
  gfx:rect(c - 22, top, 44, bottom - top, "#5a4a3240", 9)
  gfx:rect(c - 21, top + 1, 42, bottom - top - 2, "#fff8e830", 8)
  gfx:rect(c - 18, top + 3, 8, bottom - top - 8, "#ffffff40", 4)
  if hairline then
    gfx:line(c, top + 8, c, bottom - 2, 1.6, hairline)
  end
  local x0, y0 = c, top + 9
  gfx:polygon({ x0 - 9, 0, x0 + 9, 0, x0, y0 + 2 }, "#00000050")
  gfx:polygon({ x0 - 8, 0, x0 + 8, 0, x0, y0 }, C.brass)
  gfx:polygon({ x0 - 8, 0, x0 - 1, 0, x0, y0 }, C.brass_lit)
end

-- A tab on the cover at angle a marking an end of the heat-cool range: a
-- coloured wedge over the scale's ticks, pointing in.
local function tab(gfx, c, a, color)
  local function at(list, dx, dy)
    local pts = {}
    for _, p in ipairs(list) do
      local x, y = polar(c, p[1], a + p[2])
      pts[#pts + 1] = x + dx
      pts[#pts + 1] = y + dy
    end
    return pts
  end
  local shape = { { K.outer - 1, -3.4 }, { K.outer - 1, 3.4 }, { S.tick_major - 6, 0.0 } }
  gfx:polygon(at(shape, 1.5, 2.5), "#00000048")
  gfx:polygon(at(shape, 0, 0), color)
  gfx:polygon(at({ { K.outer - 1, -3.4 }, { K.outer - 1, -0.6 }, { S.tick_major - 6, 0.0 } }, 0, 0), "#ffffff40")
end

-- The needle from the pivot to angle a, with its shadow, and the cap.
local function needle(gfx, c, a, off)
  local function shape(dx, dy)
    local s, k = math.sin(math.rad(a)), -math.cos(math.rad(a))
    local px, py = -k, s
    local tip, tail = T.needle, -22
    return {
      c + dx + s * tip,
      c + dy + k * tip,
      c + dx + s * 40 + px * 1.6,
      c + dy + k * 40 + py * 1.6,
      c + dx + s * tail + px * 3.4,
      c + dy + k * tail + py * 3.4,
      c + dx + s * tail - px * 3.4,
      c + dy + k * tail - py * 3.4,
      c + dx + s * 40 - px * 1.6,
      c + dy + k * 40 - py * 1.6,
    }
  end
  gfx:polygon(shape(3, 5), "#00000040")
  gfx:polygon(shape(0, 0), off and C.off_ink or C.needle)
  gfx:circle(c + 1.5, c + 3, 14, "#00000040")
  gfx:circle(c, c, 13, C.brass_dark)
  gfx:circle(c, c, 11, C.brass)
  gfx:circle(c - 1, c - 1.5, 7, hex.mix(C.brass, C.brass_lit, 0.5))
  gfx:circle(c - 3, c - 4, 2.6, "#ffffffb0")
end

local turn = anim.follower(S.follow, 0.02)
local swing = anim.follower(T.follow, 0.02)

local function face(gfx, ui, c)
  local t = ui.thermostat
  local off = t.mode == "off"
  local look = off and theme.covers.off or theme.covers.on
  local lkey = off and "off" or "on"
  gfx:layer("body", lkey, function(g)
    body(g, c, look)
  end)
  gfx:paint_layer("body", "#ffffff")

  local a, turning = turn(-scale_angle(t, reading(t)), ui.now)
  gfx:layer("scale", table.concat({ t.unit, t.min, t.max }, "|"), function(g)
    scale(g, c, t)
  end)
  gfx:save()
  gfx:translate(c, c)
  gfx:rotate(a)
  gfx:translate(-c, -c)
  gfx:paint_layer("scale", off and "#9a968e" or "#ffffff")
  if t.mode == "heat-cool" then
    tab(gfx, c, scale_angle(t, t.low), (t.range == "high") and hex.mix(C.heat, look.body, 0.55) or C.heat)
    tab(gfx, c, scale_angle(t, t.high), (t.range == "low") and hex.mix(C.cool, look.body, 0.55) or C.cool)
  end
  gfx:restore()
  lens(gfx, c, (t.mode ~= "heat-cool" or t.range ~= nil) and (off and C.off_ink or C.hairline))

  gfx:layer("card", table.concat({ lkey, t.unit, t.min, t.max }, "|"), function(g)
    card(g, c, t, { off = off })
  end)
  gfx:paint_layer("card", "#ffffff")
  local key =
    table.concat({ t.mode, t.hvac, t.target, t.unit, t.low, t.high, t.range or "", tostring(t.range_editing) }, "|")
  gfx:layer("readout", key, function(g)
    readout(g, c, t)
  end)
  gfx:paint_layer("readout", "#ffffff")
  local n, swinging = swing(card_angle(t, t.current), ui.now)
  needle(gfx, c, n, off)

  gfx:layer("gloss", "1", function(g)
    gloss(g, c)
  end)
  gfx:paint_layer("gloss", "#ffffff")
  return turning or swinging
end

-- Draws the home screen.
function home.draw(gfx, ui)
  local c = ui.dial.size / 2
  gfx:clear("#000000")
  return face(gfx, ui, c)
end

return home
