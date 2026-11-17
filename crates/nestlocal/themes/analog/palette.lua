-- The Analog theme's colours and the dial's proportions.
return {
  -- The plastic cover by state: its shadowed edge, its body, its lit side,
  -- and the inner face's metal from dark to light.
  covers = {
    on = {
      edge = "#8c7a5c",
      body = "#e4d7bb",
      lit = "#f7efdc",
      metal_dark = "#b39a66",
      metal = "#d8c493",
      metal_lit = "#f1e4bd",
    },
    off = {
      edge = "#6f6c66",
      body = "#c9c6bf",
      lit = "#e2e0db",
      metal_dark = "#8f8b84",
      metal = "#b3afa8",
      metal_lit = "#d4d1cb",
    },
  },
  colors = {
    wall = "#14110d",
    -- Printing on the cover and the thermometer's card.
    ink = "#3a2c1e",
    ink_soft = "#7a6650",
    card = "#f6f0e2",
    card_edge = "#a8946f",
    needle = "#1d1813",
    hairline = "#c4231b",
    brass = "#c9a24e",
    brass_lit = "#f3dc9a",
    brass_dark = "#7c5f22",
    heat = "#e0321e",
    cool = "#2f7fd6",
    eco = "#3e9b4a",
    lamp_off = "#8a7d6a",
    off_ink = "#5e5a54",
  },
  -- The turning scale on the cover: degrees of arc it spans from min to
  -- max, the radii of its ticks and numbers, and the follower's seconds.
  scale = { span = 240, tick_outer = 229, tick_minor = 220, tick_major = 213, numbers = 195, follow = 0.12 },
  -- Radii of the cover, the groove round the inner face, and the face.
  cover = { outer = 236, groove = 171, inner = 165 },
  -- The room thermometer: its card's arc (degrees either side of 12
  -- o'clock) and radii, the needle's length and the follower's seconds.
  thermometer = { half = 58, inner = 106, outer = 150, needle = 146, follow = 0.4 },
}
