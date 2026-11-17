-- LCD: the home screen as a classic digital thermostat's backlit LCD,
-- seven-segment figures and printed labels. Labels set in Figtree.
local home = require("home")

return {
  name = "Lcd",
  api = 1,
  design = 480,
  fonts = {
    medium = "nest-gen4:fonts/Figtree-Medium.ttf",
  },
  draw = home.draw,
}
