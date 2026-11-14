-- The 4th gen's colours and the home screen's layout and motion, in
-- design units of a 480-unit dial. Angles are degrees clockwise from
-- 12 o'clock.
return {
  colors = {
    bg = "#000000",
    text = "#ffffff",
    secondary = "#9aa0a6",
    dim = "#5f6368",
    track = "#4b4d4c",
    track_off = "#242526",
    room = "#e8eaed",
    -- The scale's ticks outside the lit stretch.
    tick = "#a8b2b7",
    -- What the system is doing: the target's tick, symbol and words, and
    -- the glow behind the number.
    heat = "#e97f3a",
    heat_glow = "#d1651f",
    cool = "#b8dfea",
    cool_glow = "#659dc4",
    leaf = "#4fc36b",
  },
  -- The track, open at the bottom: the lowest temperature at `from`, the
  -- highest at `to`, `width` units thick, its ends fading over `fade`
  -- degrees. The dots on it are `dot` units in radius.
  ring = { radius = 216, width = 5, from = -138, to = 138, fade = 48, dot = 4 },
  -- The scale shown while the wheel turns: a tick every `step` degrees,
  -- `length` units long inward from the track, `width` thick at `dim`
  -- opacity, `lit_width` thick between the room and the target. The
  -- room's and the targets' ticks are bars `bar` units longer and
  -- `bar_width` thick.
  --
  -- The room's number and the mode's symbols stand `margin` units inside
  -- the dots (inside the bars while the scale shows). While a symbol
  -- overlaps the room's number, the number dims to `under` opacity.
  ticks = {
    step = 1.5, length = 16, width = 1.1, dim = 0.55, lit_width = 2, bar = 6, bar_width = 3.5,
    margin = 3, under = 0.3,
  },
  -- The glow behind the number: its radius, `drop` units below the
  -- middle, and the rings it is drawn in.
  glow = { radius = 206, drop = 6, steps = 64 },
  -- The target: its size and baseline, the half's size against it, and a
  -- digit's cell width against the widest digit. Heat-cool's ends at
  -- `range_size` on `range_base`, `range_gap` either side of the middle
  -- round a dot `range_dot` in radius, `range_dot_rise` of their size
  -- above the baseline. "Off" at `off_scale` of the size, `off_drop`
  -- units above the baseline.
  number = {
    size = 168, base = 300, half = 0.34, cell = 0.86,
    range_size = 108, range_base = 283, range_gap = 96, range_dot = 4, range_dot_rise = 0.34,
    off_scale = 0.82, off_drop = 6,
  },
  -- The words under the number, and the leaf under them.
  status = { size = 25, y = 362 },
  leaf = { y = 418, size = 30 },
  -- Seconds:
  --   roll, roll_min  a digit's roll after a pause, and the quickest one
  --                   while the wheel turns fast
  --   dot             time constant of the target's marks gliding
  --   room            the same for the room's
  --   glow            the glow fading or crossfading
  --   hold, fade      the scale staying after the wheel stops, then fading
  --   intro           the ring drawing in on coming home, `intro_from`
  --                   after the screen changed (as the menus fold away)
  motion = { roll = 0.3, roll_min = 0.06, dot = 0.08, room = 0.25, glow = 0.6, hold = 1.5, fade = 0.4, intro = 0.6, intro_from = 0.15 },
}
