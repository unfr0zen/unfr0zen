-- Animation helpers: clamping, easing, and values that glide to a target.
local anim = {}

-- Frames further apart than this many seconds move a follower no further,
-- so a stalled frame does not make it jump.
local MAX_STEP = 0.1

function anim.clamp(x, lo, hi)
  if x < lo then
    return lo
  elseif x > hi then
    return hi
  end
  return x
end

-- Eases progress p (0 to 1): fast, then settling.
function anim.ease_out(p)
  p = anim.clamp(p, 0, 1)
  return 1 - (1 - p) ^ 3
end

-- Eases progress p (0 to 1): slow at both ends.
function anim.ease_in_out(p)
  p = anim.clamp(p, 0, 1)
  return p < 0.5 and 4 * p * p * p or 1 - (-2 * p + 2) ^ 3 / 2
end

-- Between a and b, by p from 0 to 1.
function anim.mix(a, b, p)
  return a + (b - a) * p
end

-- A value that follows its target exponentially with time constant `rate`
-- seconds, and lands on it once within `near` (default 0.002).
-- Returns `step(target, now)`, which gives the value and whether it is
-- still moving. The first call lands on the target at once.
function anim.follower(rate, near)
  near = near or 0.002
  local value, last
  return function(target, now)
    if value == nil or last == nil then
      value, last = target, now
      return value, false
    end
    local dt = anim.clamp(now - last, 0, MAX_STEP)
    last = now
    value = value + (target - value) * (1 - math.exp(-dt / rate))
    if math.abs(target - value) < near then
      value = target
      return value, false
    end
    return value, true
  end
end

return anim
