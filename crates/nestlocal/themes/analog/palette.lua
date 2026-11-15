-- The Analog theme's colours, layout and motion, in design units of a
-- 480-unit dial. Angles are degrees clockwise from 12 o'clock.
return {
  colors = {
    bg = "#121314",
    -- The cover: off-white, darker towards its edge, printed in charcoal.
    cover = "#e6e1d5",
    cover_edge = "#c9c2b2",
    print = "#2a2a2a",
    -- The setpoint's index (each of heat-cool's ends has one), and the
    -- heat and cool colours of the pilot lamp.
    index = "#d0342c",
    heat = "#e2662c",
    cool = "#3d8fd6",
    -- The window: its face, rim, needle, bimetal coil and the glass's
    -- light.
    window = "#f4f1ea",
    window_rim = "#3f3f3f",
    needle = "#1a1a1a",
    coil = "#b5713a",
    glint = "#ffffff",
    -- The switch and the pilot lamp.
    switch_track = "#d6d0c2",
    knob = "#2a2a2a",
    lamp_off = "#8a857a",
  },
  -- The cover: its radius, the ring printed with the scale (between `inner`
  -- and the edge), and its shading towards the edge.
  cover = { radius = 236, inner = 160, shade_steps = 10 },
  -- The scale on the cover, from the lowest target at `from` to the highest
  -- at `to`: ticks from `outer` inwards, every `minor`,
  -- longer every `major` with a number centred `label` units out; in each
  -- unit.
  scale = { from = -150, to = 150, outer = 226, minor_length = 9, major_length = 18, minor_width = 2, major_width = 3.5,
            label = 182, label_size = 28, steps = { C = { 1, 5 }, F = { 2, 10 } } },
  -- The index at the setpoint, round the cover's edge: a notch from `tip`
  -- to `base` radius, `spread` degrees wide, and a hairline across the
  -- ticks to `line`; heat-cool's end not picked shows at `dim` opacity.
  index = { tip = 214, base = 244, spread = 3.6, line = 200, line_width = 2.5, dim = 0.35 },
  -- The window: its radius and rim; the thermometer's printed arc (angles
  -- either side of its top), ticks and numbers; the needle's length, tail,
  -- width and hub; the bimetal coil at its pivot.
  window = { radius = 152, rim = 4 },
  thermometer = { from = -62, to = 62, outer = 128, minor_length = 6, major_length = 12, label = 105,
                  label_size = 15, steps = { C = { 1, 10 }, F = { 2, 10 } } },
  needle = { length = 126, tail = 24, width = 3, hub = 7 },
  coil = { turns = 3.2, inner = 5, outer = 24, width = 3, steps = 90 },
  -- The mode switch below the pivot: its words' baseline and size and the
  -- gap between them, the track under them and the knob's half width; the
  -- pilot lamp above the pivot.
  switch = { y = 92, size = 14, gap = 15, track_y = 106, track_height = 6, knob = 12 },
  lamp = { y = -56, radius = 6 },
  -- Time constants (seconds): the index follows the ring; the needle lags
  -- like a bimetal coil.
  motion = { index = 0.06, needle = 1.2 },
}
