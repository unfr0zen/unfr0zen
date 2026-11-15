-- Analog: the home screen as an instrument, a needle on a printed scale.
-- Set in Figtree.
local home = require("home")

return {
  name = "Analog",
  api = 1,
  design = 480,
  fonts = {
    light = "nest-gen4:fonts/Figtree-Light.ttf",
    medium = "nest-gen4:fonts/Figtree-Medium.ttf",
  },
  draw = home.draw,
}
