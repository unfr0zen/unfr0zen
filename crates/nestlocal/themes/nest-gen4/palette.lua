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
    heat = "#f1ad8f",
    heat_glow = "#ff8667",
    cool = "#b8dfea",
    cool_glow = "#507c9b",
    leaf = "#4fc36b",
  },
  -- The track, open at the bottom: the lowest temperature at `from`, the
  -- highest at `to`, `width` units thick, its ends fading over `fade`
  -- degrees. The dots on it are `dot` units in radius. Its outer edge is
  -- the scale's outer end (ticks.radius), so the scale grows inwards from it.
  ring = { radius = 229.5, width = 5, from = -150, to = 150, fade = 48, dot = 4 },
  -- The scale shown while the wheel turns, as the 4th gen draws it: a tick
  -- every `step` degrees, from `radius` `length` units in, `width` thick at
  -- `dim` opacity and `lit_width` thick from the room to the target.
  -- Round-ended bars stand for the room (`room_inner` to `room_outer`,
  -- `room_width` thick) and the targets (`bar_inner` to `bar_outer`,
  -- `bar_width` thick in their colours, glowing `glow` units wider at
  -- `glow_alpha`), a little past the ticks at both ends.
  --
  -- The room's number and the mode's symbols stand `margin` units inside
  -- the dots (inside the bars while the scale shows). While a symbol
  -- overlaps the room's number, the number dims to `under` opacity.
  ticks = {
    step = 2.6,
    radius = 232,
    length = 27,
    width = 1.6,
    dim = 0.55,
    lit_width = 2.5,
    room_inner = 202,
    room_outer = 234,
    room_width = 4.5,
    bar_inner = 198,
    bar_outer = 236,
    bar_width = 7,
    glow = 8,
    glow_alpha = 0.35,
    margin = 3,
    under = 0.3,
  },
  -- The glow behind the number: its radius, `drop` units below the
  -- middle, and the rings it is drawn in.
  glow = { radius = 210, drop = 6, steps = 120 },
  -- The target: its size and baseline, the half's size against it, and a
  -- digit's cell width against the widest digit. Heat-cool's ends at
  -- `range_size` on `range_base`, `range_gap` either side of the middle
  -- round a dot `range_dot` in radius, `range_dot_rise` of their size
  -- above the baseline. "Off" at `off_scale` of the size, `off_drop`
  -- units above the baseline.
  number = {
    size = 168,
    base = 300,
    half = 0.34,
    cell = 0.86,
    range_size = 108,
    range_base = 283,
    range_gap = 96,
    range_dot = 4,
    range_dot_rise = 0.34,
    off_scale = 0.82,
    off_drop = 6,
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
  motion = {
    roll = 0.3,
    roll_min = 0.06,
    dot = 0.08,
    room = 0.25,
    glow = 0.6,
    hold = 1.5,
    fade = 0.4,
    intro = 0.6,
    intro_from = 0.15,
  },
}
