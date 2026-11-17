-- Pixel's palette (PICO-8's), its states' colours and the grid's
-- proportions.
local P = {
  black = "#000000",
  navy = "#1d2b53",
  plum = "#7e2553",
  green = "#008751",
  brown = "#ab5236",
  grey = "#5f574f",
  silver = "#c2c3c7",
  white = "#fff1e8",
  red = "#ff004d",
  orange = "#ffa300",
  yellow = "#ffec27",
  lime = "#00e436",
  blue = "#29adff",
  lavender = "#83769c",
  pink = "#ff77a8",
  peach = "#ffccaa",
  -- PICO-8's extra shades: darker ones for the faces, two for the dial.
  ink = "#111d35",
  wine = "#422136",
  teal = "#125359",
  sea = "#065ab5",
  leaf = "#00b543",
}

return {
  palette = P,
  -- Each state's face, the bezel and unlit track on it, the dial's fill up
  -- to the target, the accent from the room to the target, its sprite and
  -- its word.
  states = {
    eco = { face = P.teal, track = P.green, fill = P.leaf, accent = P.lime, sprite = "leaf", word = "SAVING ENERGY" },
    heating = { face = P.wine, track = P.plum, fill = P.red, accent = P.orange, sprite = "flame", word = "HEATING" },
    cooling = { face = P.ink, track = P.navy, fill = P.sea, accent = P.blue, sprite = "snow", word = "COOLING" },
    idle = { face = P.ink, track = P.navy, fill = P.lavender, accent = P.silver, sprite = "heart", word = "IDLE" },
    off = { face = P.black, track = P.grey, fill = P.grey, accent = P.grey, sprite = "moon", word = "OFF" },
  },
  -- The grid: the face is drawn in `cell`-unit blocks.
  cell = 8,
  -- The dial, in cells from the middle: a ring from `from` to `to` degrees
  -- (clockwise from 12 o'clock) between radii `inner` and `outer`, its
  -- pointers `pointer` degrees either side of their value; the bezel's
  -- radii, the outer one the face's edge.
  dial = { from = -135, to = 135, inner = 23.5, outer = 27.5, pointer = 2.8 },
  bezel = { inner = 28.6, outer = 29.6 },
  -- The sprite: at most `width` units across, its middle, and how long
  -- each frame shows while the system runs, in seconds.
  sprite = { width = 60, y = 426, frame = 0.3 },
  -- The arcade's pace: everything that moves steps every `step` seconds.
  -- While the system runs, lights chase along the dial towards the target,
  -- one cell in `chase` lit, over bands of `band` degrees. Idle, the heart
  -- beats every `beat` seconds; saving energy, the leaf sways every `sway`.
  motion = { step = 0.125, chase = 4, band = 4.5, beat = 1.6, sway = 0.75 },
  -- Off, the stars round the moon: each a cell's offset from the sprite's
  -- middle and how many steps it takes to twinkle.
  stars = { { -9, -5, 7 }, { 8, -7, 11 }, { -12, 2, 9 }, { 11, 1, 13 }, { -5, -9, 17 }, { 4, -10, 5 } },
}
