-- Nest 4th Gen: the 2024 Nest Learning Thermostat's home screen, black
-- with a glow in the colour of what the system is doing, set in Figtree.
local home = require("home")

return {
  name = "Nest 4th Gen",
  api = 1,
  design = 480,
  fonts = {
    figtree_light = "fonts/Figtree-Light.ttf",
    figtree = "fonts/Figtree-Regular.ttf",
    figtree_medium = "fonts/Figtree-Medium.ttf",
  },
  draw = home.draw,
}
