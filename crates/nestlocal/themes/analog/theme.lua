-- Analog: the home screen as a classic round dial thermostat, set in
-- Archivo.
local home = require("home")

return {
  name = "Analog",
  api = 1,
  design = 480,
  fonts = {
    print = "fonts/Archivo-Medium.ttf",
    wide = "fonts/ArchivoExpanded-SemiBold.ttf",
  },
  draw = home.draw,
}
