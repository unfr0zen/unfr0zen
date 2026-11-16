-- The sky: its gradient by the time of day and the cloud cover, the sun
-- or the moon on their arc, and stars on clear nights.
local anim = require("common:anim")
local color = require("common:color")
local palette = require("palette")
local sprites = require("sprites")

local sky_style, sun_times, orbit = palette.sky, palette.sun_times, palette.orbit
local sun_style, moon_style, star_style = palette.sun, palette.moon, palette.stars

local DAY = 86400

local sky = {}

-- Seconds into the local day of epoch seconds `time`.
local function day_seconds(clock, time)
  return (time + clock.utc_offset) % DAY
end

-- 0 to 1 as x passes from a - w to a + w, smoothly.
local function smooth(x, a, w)
  local p = anim.clamp((x - (a - w)) / (2 * w), 0, 1)
  return p * p * (3 - 2 * p)
end

-- The time of day: `daylight` (0 at night to 1 by day), `horizon` (how
-- much the horizon glows, round sunrise and sunset), and where the sun
-- (by day) or the moon (by night) is along its arc, 0 to 1.
function sky.time(clock, weather)
  local now = day_seconds(clock, clock.epoch)
  local rise, set = sun_times.rise * 3600, sun_times.set * 3600
  if weather.sunrise and weather.sunset then
    rise, set = day_seconds(clock, weather.sunrise), day_seconds(clock, weather.sunset)
  end
  -- A day that runs past midnight (far north or west in its time zone):
  -- the sunset and, after midnight, the time count on from the day before.
  if set <= rise then
    set = set + DAY
    if now < rise and now + DAY <= set then
      now = now + DAY
    end
  end
  local tw, glow = sun_times.twilight, sun_times.glow
  local daylight = math.min(smooth(now, rise, tw), 1 - smooth(now, set, tw))
  local near = math.min(math.abs(now - rise), math.abs(now - set))
  local horizon = math.exp(-(near / glow) ^ 2)
  local along
  if now >= rise and now <= set then
    along = (now - rise) / (set - rise)
  else
    local night = DAY - (set - rise)
    along = ((now - set) % DAY) / night
  end
  return { daylight = daylight, horizon = horizon, along = along, day = now >= rise and now <= set }
end

-- The sky's top and bottom colours for time `time` under weather `kind`.
function sky.colors(time, kind)
  local d, h = time.daylight, time.horizon * (1 - kind.overcast)
  local top = color.mix(sky_style.night[1], sky_style.day[1], d)
  local bottom = color.mix(sky_style.night[2], sky_style.day[2], d)
  top = color.mix(top, sky_style.horizon[1], h * 0.5)
  bottom = color.mix(bottom, sky_style.horizon[2], h)
  local grey_top = color.mix(sky_style.grey_night[1], sky_style.grey_day[1], d)
  local grey_bottom = color.mix(sky_style.grey_night[2], sky_style.grey_day[2], d)
  if kind.lightning then
    grey_top, grey_bottom = color.scale(grey_top, sky_style.storm), color.scale(grey_bottom, sky_style.storm)
  elseif kind.snow > 0 then
    grey_bottom = color.scale(grey_bottom, sky_style.snow)
  end
  return color.mix(top, grey_top, kind.overcast), color.mix(bottom, grey_bottom, kind.overcast)
end

-- Draws the gradient, cached until its colours change.
function sky.draw_gradient(gfx, size, top, bottom)
  gfx:layer("sky", top .. bottom, function(g)
    local band = size / sky_style.bands
    for i = 0, sky_style.bands - 1 do
      -- Band centres, eased so the horizon's colour stays low.
      local p = ((i + 0.5) / sky_style.bands) ^ 1.6
      g:rect(0, i * band, size, band + 1, color.mix(top, bottom, p))
    end
  end, size, size)
  gfx:paint_layer("sky", "#ffffff")
end

-- Where along its arc (0 to 1) a body is.
local function on_orbit(along)
  local x = anim.mix(orbit.left, orbit.right, along)
  local y = orbit.low - (orbit.low - orbit.high) * math.sin(math.pi * along)
  return x, y
end

-- The sun, glowing, by day under a clear enough sky.
function sky.draw_sun(gfx, time, kind)
  local shown = time.daylight * (1 - anim.clamp(kind.overcast * 1.6, 0, 1))
  if not time.day or shown <= 0.01 then
    return
  end
  local x, y = on_orbit(time.along)
  local r = sun_style.glow
  gfx:save()
  gfx:alpha(shown)
  sprites.stamp(gfx, "glow", x, y, r, color.with_alpha(sun_style.glow_color, 0.55))
  gfx:circle(x, y, sun_style.radius, sun_style.color)
  gfx:restore()
end

-- The moon, a crescent, by night under a clear enough sky; its dark side
-- in the sky's colour at `top`..`bottom`.
function sky.draw_moon(gfx, time, kind, size, top, bottom)
  local shown = (1 - time.daylight) * (1 - anim.clamp(kind.overcast * 1.6, 0, 1))
  if time.day or shown <= 0.01 then
    return
  end
  local x, y = on_orbit(time.along)
  gfx:save()
  gfx:alpha(shown)
  sprites.stamp(gfx, "glow", x, y, moon_style.radius * 4, "#c8d4ff30")
  gfx:circle(x, y, moon_style.radius, moon_style.color)
  local behind = color.mix(top, bottom, ((y / size) ^ 1.6))
  gfx:circle(x + moon_style.phase_offset, y - moon_style.phase_offset * 0.4, moon_style.radius, behind)
  gfx:restore()
end

-- Twinkling stars on clear nights, in groups that twinkle together.
function sky.draw_stars(gfx, time, kind, now)
  local shown = (1 - time.daylight) * (1 - kind.overcast)
  if shown <= 0.02 then
    return false
  end
  for group = 1, star_style.groups do
    local twinkle = 0.65 + 0.35 * math.sin(now * star_style.twinkle * (0.7 + group * 0.3) + group * 2.1)
    gfx:save()
    gfx:alpha(shown * twinkle)
    for i = group, star_style.count, star_style.groups do
      local x = sprites.random(i, 1) * 480
      local y = anim.mix(star_style.top, star_style.bottom, sprites.random(i, 2) ^ 1.4)
      sprites.stamp(gfx, "dot", x, y, 1 + sprites.random(i, 3) * 1.6)
    end
    gfx:restore()
  end
  return true
end

return sky
