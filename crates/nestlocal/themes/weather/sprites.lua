-- The weather's sprites: soft shapes drawn once into layers, then stamped
-- wherever they go. A stamp is one textured quad, and the GPU draws a run
-- of stamps of the same sprite at the same opacity in one go, so hundreds
-- of raindrops or flakes cost a few draws a frame.
local palette = require("palette")

local sprites = {}

local WHITE = "#ffffff"
-- Each sprite's layer size, in units.
local SIZE = { dot = 32, glow = 128, drop = 16, bead = 48, cloud = 256 }
local CLOUD_HEIGHT = 112
-- The rings a soft disc is built of.
local SOFT_RINGS = 16

-- A pseudo-random number from 0 to 1, the same for the same i and k.
function sprites.random(i, k)
  local x = math.sin(i * 12.9898 + k * 78.233) * 43758.5453
  return x - math.floor(x)
end

-- A disc fading from opaque at its middle to clear at its edge.
local function soft_disc(g, c, r, falloff)
  for i = SOFT_RINGS, 1, -1 do
    local f = i / SOFT_RINGS
    g:circle(c, c, r * f, WHITE .. string.format("%02x", math.floor(255 * (1 - f ^ falloff) / SOFT_RINGS * 2.2)))
  end
end

-- A cloud: overlapping puffs on a flat base, `seed` shaping it.
local function cloud(g, seed)
  local w, h = SIZE.cloud, CLOUD_HEIGHT
  local base = h * 0.78
  g:rect(w * 0.12, base - h * 0.22, w * 0.76, h * 0.22, WHITE, h * 0.11)
  for i = 1, 5 do
    local x = w * (0.18 + 0.64 * (i - 1) / 4)
    local r = h * (0.2 + 0.2 * sprites.random(seed * 10 + i, 1)) * (i == 3 and 1.35 or 1)
    g:circle(x, base - r * 0.75, r, WHITE)
  end
end

-- Draws the sprites' layers (once: their keys never change).
function sprites.define(gfx)
  gfx:layer("dot", "1", function(g) soft_disc(g, SIZE.dot / 2, SIZE.dot / 2, 1.2) end, SIZE.dot, SIZE.dot)
  gfx:layer("glow", "1", function(g) soft_disc(g, SIZE.glow / 2, SIZE.glow / 2, 0.6) end, SIZE.glow, SIZE.glow)
  -- A raindrop's streak, faint at its top.
  gfx:layer("drop", "1", function(g)
    local x, steps = SIZE.drop / 2, 8
    for i = 1, steps do
      local y0, y1 = (i - 1) / steps * 64, i / steps * 64
      g:line(x, y0, x, y1, 4, palette.rain.color .. string.format("%02x", math.floor(255 * i / steps)))
    end
  end, SIZE.drop, 64)
  -- A drop on the glass: dark rim, clear middle, a glint.
  gfx:layer("bead", "1", function(g)
    local c, r = SIZE.bead / 2, SIZE.bead / 2 - 3
    g:circle(c, c + 1.5, r, "#0000002a")
    g:circle(c, c, r, "#ffffff22")
    g:ring(c, c, r - 1, 2, "#ffffff55")
    g:circle(c - r * 0.35, c - r * 0.4, r * 0.22, "#ffffffc0")
  end, SIZE.bead, SIZE.bead)
  for seed = 1, 2 do
    gfx:layer("cloud" .. seed, "1", function(g) cloud(g, seed) end, SIZE.cloud, CLOUD_HEIGHT)
  end
end

-- The four corners a stamp is given; reused, as stamps come by the
-- hundred each frame.
local corners = { 0, 0, 0, 0, 0, 0, 0, 0 }

-- Stamps sprite `name` as the quad with corners (x0, y0) top left, (x1,
-- y1) top right, (x2, y2) bottom right, (x3, y3) bottom left.
function sprites.warp(gfx, name, x0, y0, x1, y1, x2, y2, x3, y3)
  corners[1], corners[2], corners[3], corners[4] = x0, y0, x1, y1
  corners[5], corners[6], corners[7], corners[8] = x2, y2, x3, y3
  gfx:warp_layer(name, corners)
end

-- Stamps sprite `name` `r` units across from its middle at (x, y): in its
-- own colour, or tinted `tint` (a separate draw).
function sprites.stamp(gfx, name, x, y, r, tint)
  if tint then
    local scale = 2 * r / SIZE[name]
    gfx:save()
    gfx:translate(x - r, y - r)
    gfx:scale(scale)
    gfx:paint_layer(name, tint)
    gfx:restore()
    return
  end
  sprites.warp(gfx, name, x - r, y - r, x + r, y - r, x + r, y + r, x - r, y + r)
end

-- Stamps cloud `seed` `w` units wide with its top left at (x, y), tinted.
function sprites.cloud(gfx, seed, x, y, w, tint)
  local scale = w / SIZE.cloud
  gfx:save()
  gfx:translate(x, y)
  gfx:scale(scale)
  gfx:paint_layer("cloud" .. seed, tint)
  gfx:restore()
end

-- A cloud's height for width `w`.
function sprites.cloud_height(w)
  return w * CLOUD_HEIGHT / SIZE.cloud
end

return sprites
