-- The home screen as 8-bit pixel art, every shape on a coarse grid of
-- blocks: a stepped bezel and a ring of blocks, its unlit cells dithered,
-- filled to the target in what the system is doing (warm when heating,
-- cool when cooling, green saving energy), the target a white spoke across
-- it and the room a yellow marker inside; the mode in a box, the target in
-- a pixel font and a sprite for the state: a flame heating, a snowflake
-- cooling, a leaf saving energy, a heart idle and a moon off.
--
-- It moves as an arcade does, a step at a time: while the system runs,
-- lights chase along the dial towards the target and the sprite flickers,
-- embers rising off the flame or flakes falling past the snowflake; idle,
-- the heart beats; saving energy, the leaf sways; off, stars twinkle round
-- the moon.
--
-- The face, the ring's lit cells, each step of the chase, the readouts and
-- the sprite's frames are layers, drawn again only when what they show
-- changes.
local anim = require("common:anim")
local theme = require("palette")
local sprites = require("sprites")
local P, S, D, B, MO = theme.palette, theme.states, theme.dial, theme.bezel, theme.motion
local CELL = theme.cell

local home = {}

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

local function state_of(t)
  if t.mode == "off" then
    return S.off
  end
  if eco(t) then
    return S.eco
  end
  return S[t.hvac] or S.idle
end

local function number(value, unit)
  if unit == "F" then
    return string.format("%d", math.floor(value + 0.5))
  end
  local halves = math.floor(value * 2 + 0.5)
  return halves % 2 == 0 and string.format("%d", halves / 2) or string.format("%.1f", halves / 2)
end

local function polar(c, r, degrees)
  local a = math.rad(degrees)
  return c + r * math.sin(a), c - r * math.cos(a)
end

local function angle(t, value)
  local f = anim.clamp((value - t.min) / (t.max - t.min), 0, 1)
  return D.from + f * (D.to - D.from)
end

-- Fills the cells for which `inside(i, j)` holds, a rect per run in a row.
local function blocks(g, n, inside, color)
  for j = 0, n - 1 do
    local start
    for i = 0, n do
      local on = i < n and inside(i, j)
      if on and not start then
        start = i
      end
      if not on and start then
        g:rect(start * CELL, j * CELL, (i - start) * CELL, CELL, color)
        start = nil
      end
    end
  end
end

-- A cell's distance from the middle, in cells, and its angle.
local function where(c, i, j)
  local dx, dy = (i + 0.5) * CELL - c, (j + 0.5) * CELL - c
  return math.sqrt(dx * dx + dy * dy) / CELL, math.deg(math.atan2(dx, -dy))
end

-- The dial's cells, row by row: each `{ i, angle }`, left to right.
local ring_rows, ring_for
local function ring(c, n)
  if ring_for == n then
    return ring_rows
  end
  ring_rows, ring_for = {}, n
  for j = 0, n - 1 do
    local row = {}
    for i = 0, n - 1 do
      local r, a = where(c, i, j)
      if r >= D.inner and r < D.outer and a >= D.from and a <= D.to then
        row[#row + 1] = { i, a }
      end
    end
    ring_rows[j] = row
  end
  return ring_rows
end

-- The face in a state's colours: the stepped disc, a dot every four cells,
-- the bezel and the dial's unlit cells, dithered.
local function face_layer(g, c, n, state)
  blocks(g, n, function(i, j)
    return where(c, i, j) < B.outer
  end, state.face)
  blocks(g, n, function(i, j)
    local r = where(c, i, j)
    return r >= B.inner and r < B.outer
  end, state.track)
  blocks(g, n, function(i, j)
    return i % 4 == 1 and j % 4 == 1 and where(c, i, j) < D.inner - 2
  end, state.track .. "90")
  for j, row in pairs(ring(c, n)) do
    for _, cell in ipairs(row) do
      if (cell[1] + j) % 2 == 0 then
        g:rect(cell[1] * CELL, j * CELL, CELL, CELL, state.track)
      end
    end
  end
end

-- The colour of the dial's cell at angle a, or nil to leave it unlit.
local function lit(t, state, a)
  local function near(value)
    return math.abs(a - angle(t, value)) <= D.pointer
  end
  local function between(v1, v2)
    local a1, a2 = angle(t, v1), angle(t, v2)
    return a >= math.min(a1, a2) and a <= math.max(a1, a2)
  end
  local running = t.hvac == "heating" or t.hvac == "cooling"
  if t.mode == "heat-cool" then
    if near(t.low) then
      return t.range == "low" and P.white or P.orange
    end
    if near(t.high) then
      return t.range == "high" and P.white or P.blue
    end
    if running and between(t.current, t.hvac == "heating" and t.low or t.high) then
      return state.accent
    end
    if between(t.low, t.high) then
      return state.fill
    end
    return nil
  end
  if near(t.target) then
    return P.white
  end
  if running and between(t.current, t.target) then
    return state.accent
  end
  if t.mode == "cool" then
    return a >= angle(t, t.target) and state.fill or nil
  end
  return a <= angle(t, t.target) and state.fill or nil
end

-- The dial's lit cells, a rect per run of one colour in a row.
local function dial_layer(g, c, n, t, state)
  for j, row in pairs(ring(c, n)) do
    local start, last, color = nil, nil, nil
    local function flush()
      if start and color then
        g:rect(start * CELL, j * CELL, (last - start + 1) * CELL, CELL, color)
      end
    end
    for _, cell in ipairs(row) do
      local here = lit(t, state, cell[2])
      if not last or here ~= color or cell[1] ~= last + 1 then
        flush()
        start, color = cell[1], here
      end
      last = cell[1]
    end
    if last then
      flush()
    end
  end
end

-- Which way the chase runs along the dial: towards the target (or the
-- range's end the system works to), +1 clockwise, -1 back.
local function chase_way(t)
  local goal = t.target
  if t.mode == "heat-cool" then
    goal = t.hvac == "heating" and t.low or t.high
  end
  return angle(t, goal) >= angle(t, t.current) and 1 or -1
end

-- One step of the chase: the running cells of band `k` (of MO.chase), lit
-- brighter. Painted in turn, the bands make the lights run.
local function chase_layer(g, c, n, t, state, k)
  for j, row in pairs(ring(c, n)) do
    for _, cell in ipairs(row) do
      local band = math.floor((cell[2] - D.from) / MO.band)
      if band % MO.chase == k and lit(t, state, cell[2]) == state.accent then
        g:rect(cell[1] * CELL, j * CELL, CELL, CELL, state == S.cooling and P.white or P.yellow)
      end
    end
  end
end

-- The room's marker: a plus of five cells inside the dial, on the grid.
local function marker(gfx, c, t, color)
  local x, y = polar(c, (D.inner - 2.5) * CELL, angle(t, t.current))
  x, y = math.floor(x / CELL) * CELL, math.floor(y / CELL) * CELL
  gfx:rect(x - CELL, y, 3 * CELL, CELL, color)
  gfx:rect(x, y - CELL, CELL, 3 * CELL, color)
end

local MODE_NAME = { heat = "HEAT", cool = "COOL", ["heat-cool"] = "AUTO", off = "OFF" }
local MODE_COLOR = { heat = P.orange, cool = P.blue, ["heat-cool"] = P.pink, off = P.grey }

local function px(size, color, align)
  return { font = "pixel", size = size, color = color, align = align }
end

-- A box of blocks round a word, its corners cut, as an arcade label.
local function label_box(g, c, y, word, color)
  local style = px(16, P.white)
  local w = g:measure(word, style)
  local bw = math.ceil((w + 24) / 8) * 8
  local x0, y0, h, s = c - bw / 2, y - 24, 32, 4
  g:rect(x0 + s, y0, bw - 2 * s, s, color)
  g:rect(x0 + s, y0 + h - s, bw - 2 * s, s, color)
  g:rect(x0, y0 + s, s, h - 2 * s, color)
  g:rect(x0 + bw - s, y0 + s, s, h - 2 * s, color)
  g:text(word, c - w / 2 + 2, y, style)
end

-- The target: whole degrees large, the half smaller, a degree sign.
local function big(g, c, y, t, color)
  local value = number(t.target, t.unit)
  local whole, half = value:match("^(%d+)(.*)$")
  local style, small = px(96, color), px(32, color)
  local w = g:measure(whole, style)
  local deg = 24
  -- Centred with the degree sign: the half hangs after, so the digits stay put.
  local x = c - (w + deg) / 2
  g:text(whole, x, y, style)
  if half ~= "" then
    g:text(half, x + w, y - (style.size - small.size) / 8, small)
  end
  -- The degree sign as a ring of blocks.
  local dx, dy, s = x + w + 6, y - 84, 6
  g:rect(dx + s, dy, 2 * s, s, color)
  g:rect(dx + s, dy + 3 * s, 2 * s, s, color)
  g:rect(dx, dy + s, s, 2 * s, color)
  g:rect(dx + 3 * s, dy + s, s, 2 * s, color)
end

-- Text no wider than `width`: at `size`, or half of it.
local function fit(g, s, x, y, size, color, width)
  local style = px(size, color, "center")
  if g:measure(s, style) > width then
    style.size = size / 2
  end
  g:text(s, x, y, style)
end

-- The readouts, which change only with the thermostat.
local function readout(g, c, t, state)
  if t.mode == "off" then
    g:text("OFF", c, 280, px(64, P.grey, "center"))
  else
    label_box(g, c, 120, MODE_NAME[t.mode] or string.upper(t.mode), MODE_COLOR[t.mode] or P.grey)
    local word, color = state.word, state == S.idle and P.white or state.accent
    if t.range then
      local name = t.range == "low" and "HEAT" or "COOL"
      word = (t.range_editing and "SET " or "SELECT ") .. name
      color = t.range == "low" and P.orange or P.blue
    end
    fit(g, word, c, 176, 16, color, 300)
  end
  if t.mode == "heat-cool" then
    local low, high = number(t.low, t.unit), number(t.high, t.unit)
    -- The two ends and a dash between them, as one run centred on c.
    local size = math.min(48, math.floor(280 / (#low + #high + 1) / 8) * 8)
    local function tone(color, side)
      return (t.range and t.range ~= side) and P.grey or color
    end
    local lo, hi = px(size, tone(P.orange, "low")), px(size, tone(P.blue, "high"))
    local x = c - (g:measure(low, lo) + size + g:measure(high, hi)) / 2
    x = x + g:text(low, x, 272, lo)
    g:rect(x + size / 4, 272 - size * 3 / 8 - size / 16, size / 2, size / 8, P.silver)
    g:text(high, x + size, 272, hi)
  elseif t.mode ~= "off" then
    big(g, c, 286, t, P.white)
  end
  -- The room's temperature: its word, a space and the value.
  local room, value = px(16, P.silver), px(16, P.yellow)
  local deg = number(t.current, t.unit) .. "°"
  local w = g:measure("ROOM", room) + 16 + g:measure(deg, value)
  local x = c - w / 2
  x = x + g:text("ROOM", x, 340, room) + 16
  g:text(deg, x, 340, value)
end

-- A sprite's blocks, `size` units each, centred on x, y, in the second
-- frame when `second`.
local function sprite(g, art, x, y, size, second)
  local h, w = #art.rows, #art.rows[1]
  local x0, y0 = x - w * size / 2, y - h * size / 2
  local swap = second and type(art.flicker) == "table" and art.flicker
  local mirror = second and art.flicker == "mirror"
  for j = 1, h do
    local row = mirror and art.rows[j]:reverse() or art.rows[j]
    local start, ch = nil, nil
    for i = 1, w + 1 do
      local here = i <= w and row:sub(i, i) or "."
      if here ~= ch then
        if ch and ch ~= "." then
          local color = sprites.colors[swap and swap[ch] or ch]
          g:rect(x0 + (start - 1) * size, y0 + (j - 1) * size, (i - start) * size, size, color)
        end
        start, ch = i, here
      end
    end
  end
end

-- A pseudo-random number from 0 to 1, the same for the same i and k.
local function random(i, k)
  local x = math.sin(i * 12.9898 + k * 78.233) * 43758.5453
  return x - math.floor(x)
end

-- Blocks of `size` rising (`way` -1) or falling (+1) round the sprite at
-- step `step`: embers off the flame, flakes past the snowflake.
local function drift(gfx, c, step, size, way, colors)
  for i = 1, 5 do
    local life = 6 + i % 3
    local age = (step + i * 3) % life
    local pass = math.floor((step + i * 3) / life)
    local x = c + (math.floor(random(pass, i) * 9) - 4) * size * 2
    local y = theme.sprite.y + way * (age - life / 2) * size * 2
    gfx:rect(x, y, size, size, colors[(age + i) % #colors + 1])
  end
end

-- The stars round the moon, each lit on its own beat.
local function stars(gfx, c, step, size)
  for i, star in ipairs(theme.stars) do
    local phase = (step + i * 2) % star[3]
    if phase < 2 then
      local x, y = c + star[1] * size, theme.sprite.y + star[2] * size
      local color = phase == 0 and P.white or P.silver
      gfx:rect(x, y, size, size, color)
      if phase == 0 then
        gfx:rect(x - size, y, size, size, P.lavender)
        gfx:rect(x + size, y, size, size, P.lavender)
        gfx:rect(x, y - size, size, size, P.lavender)
        gfx:rect(x, y + size, size, size, P.lavender)
      end
    end
  end
end

local function face(gfx, ui, c)
  local t = ui.thermostat
  local state = state_of(t)
  local n = math.floor(ui.dial.size / CELL + 0.5)
  gfx:layer("face", state.face .. state.track, function(g)
    face_layer(g, c, n, state)
  end)
  gfx:paint_layer("face", "#ffffff")
  local key = table.concat(
    { state.word, t.mode, t.hvac, t.target, t.current, t.unit, t.low, t.high, t.range or "", tostring(t.range_editing) },
    "|"
  )
  if t.mode ~= "off" then
    gfx:layer("dial", key, function(g)
      dial_layer(g, c, n, t, state)
    end)
    gfx:paint_layer("dial", "#ffffff")
  end
  marker(gfx, c, t, t.mode == "off" and P.silver or P.yellow)

  gfx:layer("readout", key, function(g)
    readout(g, c, t, state)
  end)
  gfx:paint_layer("readout", "#ffffff")

  local running = t.mode ~= "off" and (t.hvac == "heating" or t.hvac == "cooling")
  local step = math.floor(ui.now / MO.step)
  if running then
    -- The chase, one band lit at a time, running towards the target.
    local k = (chase_way(t) * step) % MO.chase
    local name = "chase" .. k
    gfx:layer(name, key, function(g)
      chase_layer(g, c, n, t, state, k)
    end)
    gfx:paint_layer(name, "#ffffff")
  end

  local art = sprites[state.sprite]
  local second = running and art.flicker and math.floor(ui.now / theme.sprite.frame) % 2 == 1
  local size = math.floor(theme.sprite.width / #art.rows[1])
  -- Idle, the heart's double beat: a block bigger for a step, twice.
  local beat = state == S.idle and t.mode ~= "off"
  local q = beat and math.floor((ui.now % MO.beat) / MO.step)
  local thump = beat and (q == 0 or q == 2)
  local name = "sprite" .. (second and "2" or thump and "3" or "1")
  local s = thump and size + 1 or size
  gfx:save()
  -- Saving energy, the leaf sways a block either way.
  if state == S.eco then
    gfx:translate((math.floor(ui.now / MO.sway) % 2 == 0 and -1 or 1) * size, 0)
  end
  gfx:layer(name, state.sprite, function(g)
    sprite(g, art, c, theme.sprite.y, s, second)
  end)
  gfx:paint_layer(name, "#ffffff")
  gfx:restore()
  if running and state == S.heating then
    drift(gfx, c, step, size, -1, { P.yellow, P.orange, P.red })
  elseif running and state == S.cooling then
    drift(gfx, c, step, size, 1, { P.white, P.blue, P.silver })
  elseif t.mode == "off" then
    stars(gfx, c, step, size)
  end
  -- The next step, never a frame sooner.
  gfx:wake(MO.step - ui.now % MO.step)
  return false
end

-- Draws the home screen.
function home.draw(gfx, ui)
  local c = ui.dial.size / 2
  gfx:clear("#000000")
  return face(gfx, ui, c)
end

return home
