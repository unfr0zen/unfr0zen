-- Hyperdrive: a heads-up display in the One Dark editor palette, set in
-- JetBrains Mono.
local home = require("home")

return {
  name = "Hyperdrive",
  api = 1,
  design = 480,
  fonts = {
    mono = "fonts/JetBrainsMono-Regular.ttf",
    mono_light = "fonts/JetBrainsMono-Light.ttf",
    mono_medium = "fonts/JetBrainsMono-Medium.ttf",
  },
  draw = home.draw,
}
