-- The LCD theme's colours, layout and motion, in design units of a
-- 480-unit dial. Angles are degrees clockwise from 12 o'clock.
return {
  colors = {
    bg = "#000000",
    bezel = "#1a1c1e",
    bezel_edge = "#33363b",
    -- The glass unlit, and backlit while the ring turns.
    panel = "#7c8772",
    lit = "#a6ead0",
    -- Segments and printed labels; unlit segments show at `ghost`.
    ink = "#141a16",
    ghost = 0.08,
    shade = "#00000022",
  },
  -- The round glass inside a bezel ring `bezel` units wide.
  glass = { radius = 226, bezel = 10, shade = 8 },
  -- Seven-segment digits: thickness as a fraction of the height, the
  -- slant (degrees), the gap between segments and between digits, both as
  -- fractions of the thickness and the height.
  segment = { thickness = 0.13, slant = 7, gap = 0.12, width = 0.54, spacing = 0.16 },
  -- The bar graph round the rim: `count` blocks from `from` to `to`
  -- between radii `inner` and `outer`, each `fill` of its step; lit up to
  -- the target. The room's pointer, a small block `pointer` units inside.
  bars = { from = -135, to = 135, count = 46, inner = 192, outer = 214, fill = 0.66, pointer = 10, pointer_depth = 9 },
  -- Where each readout sits: its top (`y`) and digit height (`h`);
  -- labels' baselines and sizes.
  layout = {
    clock = { y = 104, h = 28, label_y = 96 },
    target = { y = 158, h = 120, half = 0.58, label_y = 176, label_x = 96 },
    range = { y = 178, h = 80, gap = 18, label_y = 168 },
    modes = { y = 318, gap = 18 },
    room = { y = 346, h = 28, label_y = 340, gap = 30 },
    label_size = 14,
    -- The target's unit, beside its top.
    unit_size = 20,
  },
  -- Seconds: the backlight stays on after the ring stops, then fades;
  -- the running mode's "ON" blinks with this period.
  motion = { backlight = 8, fade = 0.6, blink = 1 },
}
