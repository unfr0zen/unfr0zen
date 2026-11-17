-- Pixel: the home screen as 8-bit pixel art in the PICO-8 palette, set in
-- Press Start 2P.
local home = require("home")

return {
  name = "Pixel",
  api = 1,
  design = 480,
  fonts = {
    pixel = "fonts/PressStart2P-Regular.ttf",
  },
  draw = home.draw,
}
