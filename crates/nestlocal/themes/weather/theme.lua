-- Weather: the home screen as a window on the weather outside, from Home
-- Assistant's, animated: sky, sun and moon, clouds, rain, snow, fog,
-- lightning and drops on the glass. Set in Figtree.
local home = require("home")

return {
  name = "Weather",
  api = 1,
  design = 480,
  fonts = {
    light = "nest-gen4:fonts/Figtree-Light.ttf",
    regular = "nest-gen4:fonts/Figtree-Regular.ttf",
    medium = "nest-gen4:fonts/Figtree-Medium.ttf",
  },
  draw = home.draw,
}
