-- Geometry on the round dial. Angles are degrees clockwise from 12 o'clock.
local geom = {}

-- The point `r` units from (c, c) at angle `degrees`.
function geom.polar(c, r, degrees)
  local a = math.rad(degrees)
  return c + r * math.sin(a), c - r * math.cos(a)
end

-- Appends the segment (x1, y1)-(x2, y2) to a flat list for gfx:lines.
function geom.push_line(list, x1, y1, x2, y2)
  list[#list + 1] = x1
  list[#list + 1] = y1
  list[#list + 1] = x2
  list[#list + 1] = y2
end

-- The radial segment at angle `a` from radius `inner` to `outer`,
-- appended to `list`.
function geom.push_spoke(list, c, a, inner, outer)
  local x1, y1 = geom.polar(c, inner, a)
  local x2, y2 = geom.polar(c, outer, a)
  geom.push_line(list, x1, y1, x2, y2)
end

-- The quad covering the ring from radius `inner` to `outer` between
-- angles a0 and a1: flat {x, y, ...}, inner edge first.
function geom.ring_quad(c, inner, outer, a0, a1)
  local x1, y1 = geom.polar(c, inner, a0)
  local x2, y2 = geom.polar(c, outer, a0)
  local x3, y3 = geom.polar(c, outer, a1)
  local x4, y4 = geom.polar(c, inner, a1)
  return { x1, y1, x2, y2, x3, y3, x4, y4 }
end

-- Convex quads covering the ring from radius `inner` to `outer` between
-- angles a0 and a1, none wider than `max_span` degrees. Their outer
-- corners reach past `outer` so the arc between them stays covered. Used
-- as clips for gfx:paint_layer, which keeps the GPU off the rest of the
-- layer.
function geom.ring_clips(c, inner, outer, a0, a1, max_span)
  local n = math.max(1, math.ceil((a1 - a0) / max_span))
  local span = (a1 - a0) / n
  local reach = outer / math.cos(math.rad(span / 2))
  local quads = {}
  for i = 0, n - 1 do
    quads[#quads + 1] = geom.ring_quad(c, inner, reach, a0 + i * span, a0 + (i + 1) * span)
  end
  return quads
end

return geom
