-- One Dark's colours and the home screen's layout, in design units of a
-- 480-unit dial. Angles are degrees clockwise from 12 o'clock.
return {
  colors = {
    -- One Dark, pushed apart: darker greys, the vivid variants of its
    -- accents and a near-white foreground.
    bg = "#0b0d10",
    dial = "#13161b",
    gutter = "#1c2027",
    rule = "#353b45",
    comment = "#7f848e",
    fg = "#d7dae0",
    bright = "#ffffff",
    red = "#ff6b7a",
    orange = "#ffa95e",
    yellow = "#ffd479",
    green = "#a8e66e",
    cyan = "#4fe0f0",
    blue = "#5cb8ff",
    purple = "#e070f0",
  },
  -- Segments every `step` degrees from `from` to `to`, `fill` of each step
  -- lit, between radii `inner` and `outer`.
  gauge = { from = -135, to = 135, step = 6, fill = 0.7, inner = 170, outer = 202 },
  -- `count` ticks round the dial, every `major`th one longer (from `inner`
  -- rather than `minor`), all ending at `outer`. A ring `rim` units outside
  -- them closes the bezel.
  bezel = { count = 120, major = 10, inner = 214, minor = 222, outer = 230, rim = 4 },
  -- A thin ring `inset` units inside the gauge.
  inner_ring = { inset = 12 },
  -- The target's pointer: a notch from `tip` to `base` units outside the
  -- gauge, `spread` degrees either side, and a hairline across the gauge
  -- from `hairline` units inside it.
  pointer = { tip = 4, base = 16, spread = 2.6, hairline = 8 },
  -- The room's marker: a diamond `size` units from centre to corner,
  -- `inset` units inside the gauge.
  marker = { inset = 9, size = 5 },
  -- The ring that turns while the system runs: `dashes` arcs `length`
  -- degrees long and `width` thick at `radius`, each with a thin inner arc
  -- `inner_gap` units inside, turning at `speed` degrees a second.
  spinner = { radius = 154, dashes = 3, length = 50, width = 3, inner_gap = 9, speed = 90 },
  -- Corner brackets round the readout: centred `drop` units below the
  -- middle, their corners `half_width` and `half_height` from there, arms
  -- `arm` units long.
  brackets = { half_width = 114, half_height = 84, drop = 10, arm = 18 },
  -- The readouts' baselines (`y`) and sizes.
  readout = {
    clock = { y = 124, size = 20 },
    status = { y = 162, size = 21 },
    -- The target, `shift` units left of centre to leave room for the unit
    -- after it, `unit_gap` units on with its top at `unit_y`.
    target = { y = 262, size = 104, shift = 12, unit_gap = 10, unit_y = 210, unit_size = 26 },
    off = { y = 262, size = 96 },
    -- Heat-cool's two ends, `gap` units either side of the slash.
    range = { y = 254, size = 56, gap = 14, slash_size = 50 },
    ambient = { y = 318, size = 19 },
    -- The mode in brackets `bracket_gap` units either side of it.
    mode = { y = 420, size = 22, bracket_gap = 50 },
  },
  -- Scan lines every `spacing` units, in `shade`.
  scan = { spacing = 4, shade = "#0000002e" },
}
