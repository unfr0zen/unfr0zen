-- The Weather theme's colours, layout and the weather's motion, in design
-- units of a 480-unit dial. Times are seconds, speeds units a second.
return {
  -- The sky from top to bottom: clear by night and by day, the warm
  -- horizon round sunrise and sunset, and overcast by night and by day.
  sky = {
    night = { "#060a1c", "#17244b" },
    day = { "#2a6ccc", "#8ec2ee" },
    horizon = { "#3a3c78", "#f2a463" },
    grey_night = { "#0d1016", "#222732" },
    grey_day = { "#566270", "#a1abb6" },
    -- How much darker a storm's sky is, and how much lighter snow's.
    storm = 0.55,
    snow = 1.18,
    -- Bands the gradient is drawn in.
    bands = 48,
  },
  -- Sunrise and sunset when the weather gives none (local hours), and the
  -- time either side of them over which day turns to night, and the
  -- horizon glows.
  sun_times = { rise = 7, set = 18.5, twilight = 2400, glow = 3000 },
  -- The sun and the moon cross the sky on an arc from `left` to `right`,
  -- `low` units down at its ends and `high` at its top.
  orbit = { left = 70, right = 410, low = 300, high = 30 },
  sun = { radius = 24, glow = 120, color = "#fff4d6", glow_color = "#ffd27a" },
  moon = { radius = 17, color = "#e9edf7", phase_offset = 9 },
  stars = { count = 70, top = 20, bottom = 300, groups = 3, twinkle = 1.3 },
  -- Clouds: `far` and `near` sprites, drifting `speed` units a second (by
  -- the wind's factor), lit white by day and slate by night.
  clouds = { speed = 7, day = "#ffffff", night = "#4a5268", rain = "#9aa3ae", storm = "#5d6472",
             top = 40, bottom = 230 },
  -- Rain: streaks `slope` units across for each one down, in three depths
  -- of { count, length, width, speed, alpha }.
  rain = {
    color = "#d8e8ff", slope = 0.14,
    depths = { { 130, 20, 1.5, 620, 0.38 }, { 90, 28, 2, 820, 0.55 }, { 45, 40, 2.8, 1050, 0.75 } },
  },
  -- Snow: flakes in three depths of { count, radius, speed, sway, alpha }.
  snow = {
    color = "#ffffff",
    depths = { { 90, 1.6, 22, 8, 0.45 }, { 60, 2.6, 36, 14, 0.7 }, { 28, 4.2, 55, 22, 0.9 } },
  },
  -- Drops on the glass: up to `count` at once, each `life` seconds,
  -- `slide` of them running down at `speed`.
  glass = { count = 26, radius = { 3, 9 }, life = { 3, 7 }, slide = 0.35, speed = 90 },
  -- Lightning: a flash every `every` to `every` + `spread` seconds,
  -- lasting `length`, at most `alpha` white.
  lightning = { every = 5, spread = 7, length = 0.45, alpha = 0.5, bolt = "#f4f1ff" },
  -- Fog: `count` soft banks drifting across.
  fog = { count = 6, radius = 200, speed = 9, alpha = 0.28 },
  -- Frames a second for calm skies (drifting clouds, fog, twinkling
  -- stars); precipitation takes every frame.
  rate = { calm = 20 },
  colors = {
    text = "#ffffff",
    soft = "#ffffffd0",
    faint = "#ffffffa0",
    shadow = "#00000060",
    heat = "#ffb067",
    cool = "#9fd8ff",
    dim = "#ffffff70",
  },
  -- A darker disc behind the readout, so it reads on a bright sky.
  scrim = { radius = 220, alpha = 0.32 },
  -- Baselines and sizes.
  text = {
    clock = { y = 70, size = 19 },
    outside = { y = 104, size = 21 },
    target = { y = 250, size = 128, half = 0.36 },
    range = { y = 238, size = 84, gap = 70 },
    off = { y = 240, size = 96 },
    status = { y = 286, size = 21 },
    inside = { y = 318, size = 18 },
    forecast = { hour_y = 372, temp_y = 400, hour_size = 15, temp_size = 19, step = 66, count = 5 },
  },
}
