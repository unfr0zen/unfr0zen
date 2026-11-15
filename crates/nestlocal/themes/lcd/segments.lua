-- Seven-segment characters, drawn as polygons: each segment a bar with
-- pointed ends, slanted like an LCD's. Unlit segments are drawn faintly,
-- as an LCD shows them.
local palette = require("palette")

local style, colors = palette.segment, palette.colors

local segments = {}

-- Which segments each character lights: a top, b top right, c bottom
-- right, d bottom, e bottom left, f top left, g middle.
local LIT = {
  ["0"] = "abcdef", ["1"] = "bc", ["2"] = "abdeg", ["3"] = "abcdg", ["4"] = "bcfg",
  ["5"] = "acdfg", ["6"] = "acdefg", ["7"] = "abc", ["8"] = "abcdefg", ["9"] = "abcdfg",
  ["-"] = "g", [" "] = "", O = "abcdef", F = "aefg", L = "def", H = "bcefg", o = "cdeg",
}

-- A digit's width for height h, and the step to the next.
function segments.width(h)
  return h * style.width
end

local function step(h)
  return h * (style.width + style.spacing)
end

-- The width of `text` at height h: digits a step each, "." and ":" none
-- of their own.
function segments.measure(text, h)
  local w = 0
  for ch in text:gmatch(".") do
    if ch ~= "." and ch ~= ":" then w = w + step(h) end
  end
  return w - h * style.spacing
end

-- A bar from (x1, y1) to (x2, y2), `t` thick, with pointed ends, its
-- points slanted for a character whose baseline is at `base`.
local function bar(g, x1, y1, x2, y2, t, base, fill)
  local len = math.sqrt((x2 - x1) ^ 2 + (y2 - y1) ^ 2)
  local ux, uy = (x2 - x1) / len, (y2 - y1) / len
  local px, py = -uy * t / 2, ux * t / 2
  local ax, ay = ux * t / 2, uy * t / 2
  local slant = math.tan(math.rad(style.slant))
  local pts = { x1, y1, x1 + ax + px, y1 + ay + py, x2 - ax + px, y2 - ay + py,
                x2, y2, x2 - ax - px, y2 - ay - py, x1 + ax - px, y1 + ay - py }
  for i = 1, #pts, 2 do pts[i] = pts[i] + (base - pts[i + 1]) * slant end
  g:polygon(pts, fill)
end

-- The ends of each segment of a character `h` high at (x, y), `t` thick.
local function ends(x, y, w, h, t)
  local gap = t * style.gap
  local l, r = x + t / 2, x + w - t / 2
  local top, mid, bottom = y + t / 2, y + h / 2, y + h - t / 2
  return {
    a = { l + gap, top, r - gap, top },
    b = { r, top + gap, r, mid - gap },
    c = { r, mid + gap, r, bottom - gap },
    d = { l + gap, bottom, r - gap, bottom },
    e = { l, mid + gap, l, bottom - gap },
    f = { l, top + gap, l, mid - gap },
    g = { l + gap, mid, r - gap, mid },
  }
end

-- Draws character `ch` `h` high with its top left at (x, y), lit
-- segments in `ink`, the rest faint.
local function character(g, ch, x, y, h, ink)
  local t, w = h * style.thickness, segments.width(h)
  local lit = LIT[ch] or ""
  local ghost = ink .. string.format("%02x", math.floor(255 * colors.ghost))
  for name, e in pairs(ends(x, y, w, h, t)) do
    bar(g, e[1], e[2], e[3], e[4], t, y + h, lit:find(name, 1, true) and ink or ghost)
  end
end

-- A dot `h` high's segments thick, its middle at (x, y).
local function dot(g, x, y, h, fill)
  local s = h * style.thickness
  g:rect(x - s / 2, y - s / 2, s, s, fill)
end

-- Draws `text` `h` high with its top left at (x, y): digits, "-", " ",
-- "O", "F", "L", "H", "o", a "." after a character, a ":" between two.
-- Returns its width.
function segments.draw(g, text, x, y, h, ink)
  local at, s = x, step(h)
  local slant = math.tan(math.rad(style.slant))
  for ch in text:gmatch(".") do
    if ch == "." then
      dot(g, at - h * style.spacing / 2, y + h - h * style.thickness / 2, h, ink)
    elseif ch == ":" then
      local cx = at - h * style.spacing / 2
      dot(g, cx + h * 0.3 * slant, y + h * 0.32, h, ink)
      dot(g, cx + h * 0.1 * slant, y + h * 0.72, h, ink)
    else
      character(g, ch, at, y, h, ink)
      at = at + s
    end
  end
  return at - x - h * style.spacing
end

return segments
