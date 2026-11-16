-- The weather on screen: what each of Home Assistant's conditions brings
-- (cloud cover, rain, snow, fog, lightning, drops on the glass), and the
-- clouds, fog banks, rain, snow, drops and lightning themselves, each
-- stamped from a sprite (sprites.lua).
local anim = require("common:anim")
local color = require("common:color")
local palette = require("palette")
local sprites = require("sprites")

local random = sprites.random
local clouds_style, rain_style, snow_style = palette.clouds, palette.rain, palette.snow
local glass_style, lightning_style, fog_style = palette.glass, palette.lightning, palette.fog

local SIZE = 480

local weather = {}

-- What a condition brings: `overcast` (0 to 1), `clouds`, `rain` and
-- `snow` (their strength, 0 to 1), `wind` (the clouds' speed against
-- calm), and whether there is `fog`, `lightning` and drops on the `glass`.
local KINDS = {
  sunny = {},
  ["clear-night"] = {},
  clear = {},
  partlycloudy = { overcast = 0.2, clouds = 3 },
  cloudy = { overcast = 0.75, clouds = 6 },
  windy = { overcast = 0.3, clouds = 3, wind = 4 },
  ["windy-variant"] = { overcast = 0.6, clouds = 5, wind = 4 },
  fog = { overcast = 0.85, fog = true },
  rainy = { overcast = 0.75, clouds = 5, rain = 0.45, glass = true },
  rain = { overcast = 0.75, clouds = 5, rain = 0.45, glass = true },
  pouring = { overcast = 0.9, clouds = 6, rain = 1, glass = true, wind = 2 },
  lightning = { overcast = 0.9, clouds = 6, lightning = true },
  ["lightning-rainy"] = { overcast = 0.95, clouds = 6, rain = 0.85, lightning = true, glass = true, wind = 2 },
  tstorms = { overcast = 0.95, clouds = 6, rain = 0.85, lightning = true, glass = true, wind = 2 },
  snowy = { overcast = 0.7, clouds = 5, snow = 0.7 },
  snow = { overcast = 0.7, clouds = 5, snow = 0.7 },
  ["snowy-rainy"] = { overcast = 0.8, clouds = 5, rain = 0.4, snow = 0.4, glass = true },
  sleet = { overcast = 0.8, clouds = 5, rain = 0.4, snow = 0.4, glass = true },
  hail = { overcast = 0.85, clouds = 6, rain = 0.5, snow = 0.6, glass = true },
  exceptional = { overcast = 0.6, clouds = 4 },
}

-- Home Assistant's conditions as words.
local NAMES = {
  sunny = "Sunny",
  ["clear-night"] = "Clear",
  clear = "Clear",
  partlycloudy = "Partly cloudy",
  cloudy = "Cloudy",
  windy = "Windy",
  ["windy-variant"] = "Windy",
  fog = "Fog",
  rainy = "Rain",
  rain = "Rain",
  pouring = "Heavy rain",
  lightning = "Thunderstorm",
  ["lightning-rainy"] = "Thunderstorm",
  tstorms = "Thunderstorm",
  snowy = "Snow",
  snow = "Snow",
  ["snowy-rainy"] = "Sleet",
  sleet = "Sleet",
  hail = "Hail",
  exceptional = "Severe weather",
}

-- The weather `w` (ui.nest.weather) as a kind, with every field set.
function weather.kind(w)
  local name = w.condition or w.icon
  local k = KINDS[name] or {}
  return {
    name = NAMES[name],
    overcast = k.overcast or 0,
    clouds = k.clouds or 0,
    rain = k.rain or 0,
    snow = k.snow or 0,
    wind = k.wind or 1,
    fog = k.fog or false,
    lightning = k.lightning or false,
    glass = k.glass or false,
  }
end

-- Whether the kind moves fast enough to need every frame.
function weather.busy(kind)
  return kind.rain > 0 or kind.snow > 0 or kind.lightning or kind.glass
end

-- The clouds, drifting right and coming round again, in front-to-back
-- order of size; tinted for the time of day and the weather.
function weather.draw_clouds(gfx, kind, daylight, now)
  if kind.clouds == 0 then
    return
  end
  local tint = color.mix(clouds_style.night, clouds_style.day, daylight)
  if kind.lightning then
    tint = color.mix(tint, clouds_style.storm, 0.7)
  elseif kind.rain > 0 then
    tint = color.mix(tint, clouds_style.rain, 0.6)
  end
  for i = 1, kind.clouds do
    local depth = random(i, 4)
    local w = anim.mix(170, 320, depth)
    local speed = clouds_style.speed * kind.wind * (0.5 + depth)
    local span = SIZE + w
    local x = (random(i, 5) * span + now * speed) % span - w
    local y = anim.mix(clouds_style.top, clouds_style.bottom, random(i, 6)) - sprites.cloud_height(w) / 2
    local alpha = (0.55 + 0.4 * depth) * (0.5 + kind.overcast * 0.5)
    sprites.cloud(gfx, i % 2 + 1, x, y, w, color.with_alpha(tint, alpha))
  end
end

-- Fog banks drifting slowly across.
function weather.draw_fog(gfx, kind, now)
  if not kind.fog then
    return
  end
  for i = 1, fog_style.count do
    local r = fog_style.radius * (0.7 + 0.6 * random(i, 7))
    local span = SIZE + 2 * r
    local x = (random(i, 8) * span + now * fog_style.speed * (0.6 + random(i, 9))) % span - r
    local y = anim.mix(140, 420, (i - 0.5) / fog_style.count)
    sprites.stamp(gfx, "glow", x, y, r, color.with_alpha("#ffffff", fog_style.alpha))
  end
end

-- Rain at `depth` (1 far to 3 near): streaks slanting with the wind,
-- falling through the whole screen and coming round again.
local function rain_depth(gfx, kind, depth, now)
  local d = rain_style.depths[depth]
  local count, length, width, speed, alpha = d[1], d[2], d[3], d[4], d[5]
  count = math.floor(count * kind.rain + 0.5)
  if count == 0 then
    return
  end
  length = length * (0.6 + 0.4 * kind.rain)
  local slope = rain_style.slope * kind.wind
  local fall = SIZE + length
  gfx:save()
  gfx:alpha(alpha * (0.6 + 0.4 * kind.rain))
  for i = 1, count do
    local seed = depth * 1000 + i
    local y = (random(seed, 1) * fall + now * speed * (0.9 + 0.2 * random(seed, 2))) % fall - length
    -- Drifting down-wind (right) as it falls, along its streak.
    local x = random(seed, 3) * (SIZE + slope * SIZE) - slope * SIZE + slope * y
    local dx, half = slope * length, width / 2
    -- The streak's top trails up-wind of its bottom.
    sprites.warp(gfx, "drop", x - dx - half, y, x - dx + half, y, x + half, y + length, x - half, y + length)
  end
  gfx:restore()
end

-- Snow at `depth`: flakes drifting down, swaying.
local function snow_depth(gfx, kind, depth, now)
  local d = snow_style.depths[depth]
  local count, radius, speed, sway, alpha = d[1], d[2], d[3], d[4], d[5]
  count = math.floor(count * kind.snow + 0.5)
  if count == 0 then
    return
  end
  local fall = SIZE + 2 * radius
  gfx:save()
  gfx:alpha(alpha)
  for i = 1, count do
    local seed = depth * 2000 + i
    local y = (random(seed, 1) * fall + now * speed * (0.8 + 0.4 * random(seed, 2))) % fall - radius
    local x = random(seed, 3) * SIZE + math.sin(now * (0.6 + random(seed, 4)) + seed) * sway
    sprites.stamp(gfx, "dot", x, y, radius * 2)
  end
  gfx:restore()
end

-- The rain and snow behind the readout (the far depths) or in front of
-- it (the near one).
function weather.draw_precipitation(gfx, kind, now, front)
  local from, to = 1, 2
  if front then
    from, to = 3, 3
  end
  for depth = from, to do
    if kind.snow > 0 then
      snow_depth(gfx, kind, depth, now)
    end
    if kind.rain > 0 then
      rain_depth(gfx, kind, depth, now)
    end
  end
end

-- Drops on the glass: each lands, sits a while, some run down, then it
-- dries; the next lands elsewhere. Their timing comes from the time alone,
-- so each frame finds them where they are.
function weather.draw_glass(gfx, kind, now)
  if not kind.glass then
    return
  end
  local count = math.floor(glass_style.count * math.max(kind.rain, 0.4) + 0.5)
  local life_min, life_max = glass_style.life[1], glass_style.life[2]
  for i = 1, count do
    local life = anim.mix(life_min, life_max, random(i, 11))
    local t = now + random(i, 12) * life
    local cycle = math.floor(t / life)
    local age = (t % life) / life
    local seed = i * 100 + cycle
    local r = anim.mix(glass_style.radius[1], glass_style.radius[2], random(seed, 13))
    local x, y = 40 + random(seed, 14) * 400, 40 + random(seed, 15) * 400
    if random(seed, 16) < glass_style.slide and age > 0.4 then
      y = y + (age - 0.4) * life * glass_style.speed
      r = r * 0.85
    end
    local shown = math.min(age / 0.06, 1) * math.min((1 - age) / 0.25, 1)
    if shown > 0.02 then
      gfx:save()
      gfx:alpha(shown)
      sprites.stamp(gfx, "bead", x, y, r * (0.6 + 0.4 * math.min(age / 0.06, 1)))
      gfx:restore()
    end
  end
end

-- The lightning now, 0 to 1: a quick double flicker every few seconds.
local function flash(now)
  local every = lightning_style.every
  local n = math.floor(now / (every + lightning_style.spread))
  local at = n * (every + lightning_style.spread) + random(n, 21) * lightning_style.spread
  local since = now - at
  if since < 0 or since > lightning_style.length then
    return 0, n
  end
  local p = since / lightning_style.length
  local flicker = p < 0.15 and 1 or (p < 0.3 and 0.25 or (p < 0.45 and 0.8 or 1 - p))
  return flicker, n
end

-- A jagged bolt from the clouds, for flash `n`.
local function bolt(gfx, n, strength)
  local segs = {}
  local x, y = 90 + random(n, 22) * 300, 60
  while y < 330 do
    local nx, ny = x + (random(n * 7 + y, 23) - 0.5) * 60, y + 22 + random(n * 7 + y, 24) * 30
    segs[#segs + 1], segs[#segs + 2], segs[#segs + 3], segs[#segs + 4] = x, y, nx, ny
    x, y = nx, ny
  end
  gfx:lines(segs, 9, color.with_alpha(lightning_style.bolt, 0.25 * strength))
  gfx:lines(segs, 2.5, color.with_alpha(lightning_style.bolt, strength))
end

-- The sky lighting up behind the readout: the bolt and the flash.
function weather.draw_lightning(gfx, kind, now)
  if not kind.lightning then
    return
  end
  local strength, n = flash(now)
  if strength <= 0 then
    return
  end
  bolt(gfx, n, strength)
  gfx:rect(0, 0, SIZE, SIZE, color.with_alpha("#ffffff", lightning_style.alpha * strength))
end

return weather
