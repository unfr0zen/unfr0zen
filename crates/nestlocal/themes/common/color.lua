-- Colours as "#rrggbb" strings, the form gfx takes; "#rrggbbaa" adds alpha.
local anim = require("anim")

local color = {}

local function rgb(hex)
  return tonumber(hex:sub(2, 3), 16), tonumber(hex:sub(4, 5), 16), tonumber(hex:sub(6, 7), 16)
end

local function byte(v)
  return math.floor(anim.clamp(v, 0, 255) + 0.5)
end

-- The two hex digits of opacity `v` (0 to 1), to append to "#rrggbb".
function color.alpha(v)
  return string.format("%02x", math.floor(255 * anim.clamp(v, 0, 1) + 0.5))
end

-- `hex` at opacity `v` (0 to 1).
function color.with_alpha(hex, v)
  return hex .. color.alpha(v)
end

-- Between colours a and b, by p from 0 to 1.
function color.mix(a, b, p)
  if a == b then return a end
  local r1, g1, b1 = rgb(a)
  local r2, g2, b2 = rgb(b)
  return string.format("#%02x%02x%02x",
    byte(anim.mix(r1, r2, p)), byte(anim.mix(g1, g2, p)), byte(anim.mix(b1, b2, p)))
end

-- `hex` with each channel scaled by `amount`, clamped to 255.
function color.scale(hex, amount)
  local r, g, b = rgb(hex)
  return string.format("#%02x%02x%02x", byte(r * amount), byte(g * amount), byte(b * amount))
end

return color
