# Home screen themes

A theme redraws the gen 3's home screen: the dial nlclient shows when no
menu is open. It is a folder of Lua that the theme engine
(`crates/nestlocal-theme`) runs inside nlclient, drawing straight into its
GPU context at up to 60 frames a second. Everything else stays stock:
the ring and the button, the menus, the setpoints and the HVAC control. A
theme only reads the thermostat's state and draws it.

The UNFR0ZEN screen's **Themes** page picks the theme; **Stock** is
nlclient's own dial. The page lists the theme folders as they are when it
opens: a folder added or removed by hand shows within a few seconds.

If a theme fails (a Lua error, a missing font, a frame that runs too long)
the dial shows the target, **THEME ERROR**, the theme and why, and the menus
work as ever to pick another. A theme that failed to load is tried again
when it is picked again; one whose frame failed, also with nestlocal's next
state. A theme that stops the display (nlclient restarting) 3 times in a row
before it has run for 2 minutes is not started again until it is picked
again.

## The themes here

| Folder | Look |
|-|-|
| `hyperdrive` | A heads-up display in a dark editor palette, set in JetBrains Mono. |
| `nest-gen4` | Black, with a glow in the colour of what the system is doing. |
| `analog` | An old-school round thermostat: a printed cover that turns under an index, a bimetal needle for the room. |
| `lcd` | A round backlit LCD: seven-segment figures and a bar graph round the rim. |
| `weather` | The weather outside, animated: sky, sun and moon, clouds, rain, snow, fog, lightning. |
| `common` | Not a theme: helper modules every theme can use. |

Read `hyperdrive` first (one file of drawing, ~300 lines), then `lcd` for
static layout and `weather` for animation.

## A theme's files

```
mytheme/
  theme.lua      the manifest (required)
  home.lua       the drawing, by convention
  palette.lua    every colour, size and timing, by convention
  fonts/         the fonts it ships, with their licences
```

The folder's name is the theme's id: letters, digits, `-` and `_`.

### theme.lua

```lua
local home = require("home")

return {
  name = "My Theme",          -- shown in logs
  api = 1,                    -- the engine's theme API; must be 1
  design = 480,               -- the theme's own units across the dial (default 480)
  fonts = {
    -- name = file, or a list of files tried in order
    regular = "fonts/Inter-Regular.ttf",
    light = "nest-gen4:fonts/Figtree-Light.ttf",   -- a font of another theme
  },
  draw = home.draw,           -- function(gfx, ui) -> animating
}
```

A font file is a path inside the theme, or `other:path` inside the theme
`other` beside it. Ship fonts with an open licence (SIL OFL, Apache 2.0)
and their licence file next to them; never fonts copied off a product.

### draw(gfx, ui)

The engine calls `draw` for each frame it needs. It draws through `gfx`,
reads the state from `ui`, and returns `true` while something still moves
(a glide, a fade, falling rain): the engine then asks for the next frame at
once. Returning `false` stops frames until something changes (the
thermostat's state, the ring, the screen) or until the time asked with
`gfx:wake(seconds)`.

```lua
local home = {}

function home.draw(gfx, ui)
  local c = ui.dial.size / 2
  local t = ui.thermostat
  gfx:clear("#000000")
  gfx:text(string.format("%.1f", t.target), c, c,
           { font = "regular", size = 120, color = "#ffffff", align = "center", baseline = "middle" })
  return false
end

return home
```

Coordinates are design units: (0, 0) top left, (design, design) bottom
right, the dial's centre at (design / 2, design / 2). The screen is round:
only a circle of radius design / 2 shows. Angles are degrees, clockwise
from 12 o'clock.

## The Lua environment

- Lua 5.1 (LuaJIT on the unit), with only the `table`, `string` and `math`
  libraries. No `io`, `os`, `load`, `dofile` or bytecode: a theme cannot
  touch files or settings.
- `require("name")` loads `name.lua` from the theme (`a.b` for `a/b.lua`);
  `require("other:name")` loads a module of the theme `other`, for example
  `require("common:anim")`. Each module loads once and is shared by every
  frame, so module-level locals keep state between frames.
- `tr(text)`: the text in the language in use, for the theme's own words.
- `log(...)`, `print(...)`: to nlclient's debug log.
- `clock()`: seconds on a monotonic clock, to time the theme's own work.
- Loading the theme, and drawing one frame, may each take at most 250 ms;
  past that the theme fails.

## Limits

A theme runs inside nlclient, so the engine checks everything of it and
fails the theme, never the thermostat, on the first thing out of bounds.
The dial shows the error, as does nlclient's log (`THEME Lua theme=...`).

| What | Limit |
|-|-|
| Folder, font and layer names | 1 to 64 bytes; folders and fonts in letters, digits, `-` and `_`. |
| Every file | A regular file inside the themes' folder; a link leading out of it is refused. |
| A Lua file | 256 KiB of UTF-8 source. |
| A font file | 8 MiB. |
| An image | A PNG of 4 MiB and 1024 pixels a side; another is not drawn (`gfx:image` returns false), the reason logged once. |
| `theme.lua` | Returns a table: `api` the number 1, `name` one line of 1 to 64 bytes, `design` a number from 120 to 1920, `draw` a function, `fonts` 16 names at most, each a file or a list of 4 at most; the font found must exist. |
| `require` | A module that requires itself, through others or not, fails. |
| Lua's memory | 16 MiB. |
| Patterns | `find`, `match`, `gmatch` and `gsub` take 64 KiB of text, and refuse a pattern whose worst case is long: the text's length to the power of one more than the pattern's `*`, `+`, `-` and `?` must stay under ten million. Match short texts, or `find` plainly (`s:find(text, 1, true)`). |
| Numbers given to `gfx` | Finite and within a million. |
| `gfx:lines`, `gfx:polygon`, clips | 16384 numbers. |
| A text | 1024 bytes; its `size` and `spacing` within `design`, its `outline_width` within an eighth of it. |
| `gfx:radial_glow` | 255 steps. |
| `gfx:save` | 64 deep in a frame. |
| Layers | 64 names, each side twice `design` at most, 12 dials of area in all; keys of 1024 bytes. |
| `gfx:sprite` | A column at least, frames a unit wide and high at least. |
| `gfx:wake` | 0 seconds or more. |

## ui: what a theme reads

`ui` is rebuilt for every frame. Temperatures are in the unit shown
(`thermostat.unit`), never converted by the theme, except the weather's,
which are Celsius.

| Field | |
|-|-|
| `ui.now` | Seconds since the theme was first drawn, for animation. |
| `ui.screen` | Always `"home"`. |
| `ui.dial.size`, `ui.dial.px`, `ui.dial.scale` | The dial in design units, in pixels, and pixels per unit. |
| `ui.fan_on` | The fan runs. |
| `ui.changed.screen` | `ui.now` when the home screen last came into view (as the menus fold away). |
| `ui.changed.target` | When the target last changed. |
| `ui.changed.turned` | When the ring last turned; -100 before it has. |

### ui.thermostat

| Field | |
|-|-|
| `target` | The target, rounded to the unit's step (0.5 °C, 1 °F). |
| `nudge` | How far the ring has turned towards the next step, in the unit: draw `target + nudge` to follow the ring smoothly. |
| `current` | The room's temperature. |
| `min`, `max` | The lowest and highest targets (9 to 32 °C, 48.2 to 89.6 °F): the ends of a scale. |
| `unit` | `"C"` or `"F"`. |
| `mode` | `"heat"`, `"cool"`, `"heat-cool"`, `"eco"` or `"off"`. |
| `hvac` | `"heating"`, `"cooling"` or `"idle"`. |
| `low`, `high` | Heat-cool's ends. |
| `range` | In heat-cool, the end picked on the home screen: `"low"`, `"high"` or nil. |
| `range_editing` | The picked end is being changed (press to set). |
| `humidity` | Relative humidity in percent, or nil. |
| `leaf_on` | The setpoint saves energy. |
| `eco` | Eco is on: `target` is the Eco temperature. |
| `can_heat`, `can_cool` | What the wiring can run. |
| `stage` | The stage running, 1 to 3. |
| `aux`, `emergency`, `alt` | Auxiliary heat runs; emergency heat; alternate heat's stage (0 when off). |
| `preconditioning` | Early-On running: `"heat"`, `"cool"` or nil. |
| `sunblock` | Sunlight correction is active. |
| `safety` | The safety shutoff is active. |
| `time_to_target` | Minutes until the room reaches the target, or nil. |
| `outdoor` | Outside temperature in the unit, or nil. |
| `weather_image` | The unit's own picture for the weather (`"unit:Weather_*.png"`), or nil. |
| `online` | Always true. |

`drying`, `airwave`, `system_issue`, `press_to`, `forecast`, `starts_in`,
`delayed_for`, `lockout` and `farsight` exist for compatibility and keep
their defaults.

### ui.clock

| Field | |
|-|-|
| `hour`, `minute`, `second`, `weekday` | Local time; weekday 0 is Sunday. |
| `twelve_hour` | The unit shows 12-hour time. |
| `epoch` | Seconds since 1970, UTC. |
| `utc_offset` | Local time's offset from UTC, in seconds: the local hour of an epoch time is `((time + utc_offset) % 86400) // 3600`. |

A second changing alone draws no frame: a theme that shows seconds, or the
minute, asks for its frame with `gfx:wake`.

### ui.nest: everything nestlocal knows

| Field | |
|-|-|
| `ui.nest.device`, `ui.nest.shared` | The unit's device and shared buckets, as nlclient reports them (`current_humidity`, `fan_timer_timeout`, `hvac_heater_state`, …). |
| `ui.nest.state` | What MQTT's state topic shows, and Home Assistant with it: `mode`, `action`, `target`, `current_temperature`, `fan_mode`, `eco_mode`, `filter_runtime_h`, …, in Celsius. |
| `ui.nest.weather` | Home Assistant's weather, when it publishes it: `temp_c`, `humidity`, `condition` (Home Assistant's: `sunny`, `clear-night`, `partlycloudy`, `cloudy`, `fog`, `rainy`, `pouring`, `lightning`, `lightning-rainy`, `snowy`, `snowy-rainy`, `hail`, `windy`, `windy-variant`, `exceptional`), `icon` (the unit's coarser name), `sunrise` and `sunset` (epoch seconds, today's), `forecast.hourly[]` (`time`, `temp_c`, `humidity`) and `forecast.daily[]` (`date`, `temp_high_c`, `temp_low_c`, `humidity`). |

Any of them may be missing or empty: a theme draws something sensible
without them (the Weather theme falls back to a clear sky by the clock).

## gfx: how a theme draws

Colours are strings: `"#rgb"`, `"#rrggbb"` or `"#rrggbbaa"`. Most
arguments are design units; widths are line widths.

### Shapes

| Call | Draws |
|-|-|
| `gfx:clear(color)` | The whole screen in one colour, whatever the transform. |
| `gfx:circle(x, y, r, color)` | A filled disc. |
| `gfx:ring(x, y, r, width, color)` | A circle's outline. |
| `gfx:arc(x, y, r, from, to, width, color)` | An arc from angle `from` to `to`. |
| `gfx:gradient_arc(x, y, r, from, to, width, start, stop)` | An arc fading from colour `start` to `stop`. |
| `gfx:radial_glow(x, y, radius, steps)` | A white glow built of `steps` faint discs; tint it by drawing it into a layer. |
| `gfx:line(x1, y1, x2, y2, width, color[, round])` | One segment, round-capped if `round`. |
| `gfx:lines({x1, y1, x2, y2, ...}, width, color)` | Many segments in one stroke: far cheaper than as many `line` calls. |
| `gfx:rect(x, y, w, h, color[, radius])` | A rectangle, its corners rounded by `radius`. |
| `gfx:polygon({x1, y1, x2, y2, ...}, color)` | A filled polygon. Keep it convex (see below). |

### Text

`gfx:text(text, x, y, style)` draws and returns the advance width;
`gfx:measure(text, style)` returns the width and height without drawing;
`gfx:has_glyphs(text, style)` says whether the font has every character.
The style is a table:

| Field | Default | |
|-|-|-|
| `font` | `"regular"` | A name from the manifest's `fonts`. |
| `size` | 24 | In design units. |
| `color` | white | |
| `align` | `"left"` | `"left"`, `"center"` or `"right"` of x. |
| `baseline` | `"alphabetic"` | `"alphabetic"`, `"top"`, `"middle"` or `"bottom"` at y. |
| `spacing` | 0 | Extra space between letters. |
| `outline`, `outline_width` | none, 2 | A stroke round the letters, under them. |
| `snap` | true | Start on a whole pixel; false for text that moves smoothly. |

### Images

`gfx:image(file, x, y, w, h[, alpha])` draws a PNG of the theme (or
`unit:NAME`, one of the unit's own pictures, such as `weather_image`), and
returns whether it could. `gfx:sprite(file, frame, columns, w, h, x, y)`
draws one frame of a sprite sheet and returns its frame count.

### Transforms and opacity

`gfx:save()` and `gfx:restore()` keep and bring back the transform and
opacity. `gfx:translate(x, y)`, `gfx:rotate(degrees)` (clockwise) and
`gfx:scale(x[, y])` move what follows; `gfx:alpha(a)` multiplies the
opacity of what follows, until the next restore.

### Layers

A layer is an offscreen picture the theme draws once and paints as often
as it likes: the main tool for a fast theme.

| Call | |
|-|-|
| `gfx:layer(name, key, function(g) ... end[, w, h])` | Draws the function into layer `name` (`w` by `h` units, the dial by default), unless the layer already holds what `key` (a string, number or boolean) names. Change the key whenever what the layer shows changes. |
| `gfx:paint_layer(name, tint[, clip])` | Paints the layer at the origin (move it with `translate`), its colours multiplied by `tint`; only inside the polygon `clip` (layer coordinates) when given. Draw a layer in white to paint it in any colour. |
| `gfx:warp_layer(name, {x0, y0, x1, y1, x2, y2, x3, y3})` | Paints all of the layer onto the quadrilateral with those top-left, top-right, bottom-right and bottom-left corners, in perspective; opacity from `gfx:alpha`. |

On the unit a layer keeps its texture for as long as the theme is shown:
use a fixed set of names (one per digit or sprite is fine), never a name
made from a value that keeps changing, which would add a texture each time.

### Frames

`gfx:wake(seconds)` asks for another frame that many seconds from now (the
soonest of a frame's calls wins). Return `true` from `draw` instead while
something moves every frame.

## Making it fast

The unit's GPU is small, and a theme shares nlclient's thread and frame
budget:

- **Cache what does not change in layers.** A face, a scale, a block of text:
  draw them once into a layer keyed by what they show, and paint the layer
  each frame. Hyperdrive and Analog draw almost nothing else.
- **Paint layers rather than shapes.** A layer painted (or warped) is one
  textured quad, and the GPU draws a run of the same layer at the same tint
  and opacity in one go. Shapes go through a vector renderer, each its own
  draw. The Weather theme draws hundreds of raindrops as stamps of one small
  layer: a few draws a frame.
- **Batch strokes.** One `gfx:lines` with many segments, not many `gfx:line`.
- **Keep polygons convex.** The screen's framebuffer has no stencil, which
  concave fills and overlapping translucent strokes need; a theme that needs
  it is drawn through an offscreen texture instead, every frame, at a cost.
- **Stop when still.** Return `false` when nothing moves, and wake for the
  next change you know of (the next minute for a clock, a blink, a fade's
  end). A screen at rest should cost nothing.

## Getting it right

- **Both units.** Celsius steps by halves and Fahrenheit by whole degrees;
  `common:thermostat` formats them (`thermostat.digits`, `thermostat.text`).
  `min`, `max` and every temperature but the weather's come in the unit
  shown.
- **Every mode.** Heat, cool, heat-cool (with an end picked or not, being
  changed or not), eco and off; idle, heating and cooling.
- **The ring.** Turning it changes `target` (or the picked end in
  heat-cool) and `nudge` in between steps; `ui.changed.turned` says when.
  A theme should answer the ring at once.
- **Missing data.** No humidity, no weather, no forecast.
- **Originality.** Evoke a genre, not a product: no copied artwork, logos,
  names or a product's exact look.

The common modules help:

| Module | |
|-|-|
| `common:anim` | `clamp`, easing (`ease_out`, `ease_in_out`), `mix`, and `follower(rate, near)`: a value that glides to its target. |
| `common:color` | `alpha`, `with_alpha`, `mix` and `scale` for `"#rrggbb"` colours. |
| `common:geom` | `polar` (a point at an angle and radius), `push_line`, `push_spoke`, `ring_quad`, `ring_clips` (clips round a ring, for painting parts of a layer). |
| `common:thermostat` | Temperatures as text, the scale's fraction for a value, what the system is doing, the time as text, seconds to the next minute. |

## Trying a theme

Render a theme on a computer, without a thermostat, in every mode, in both
units and in a range of weather and times of day:

```sh
THEME_PREVIEWS=/tmp/previews THEME_PREVIEW_THEMES=mytheme \
  cargo test -p nestlocal-theme previews -- --ignored
```

It writes one PNG for each scene (`mytheme-heating.png`, `mytheme-rain-f.png`,
...) through the engine's software renderer: the same Lua and API as on the
unit. Check the unit itself for speed and for anything that moves.

To ship a theme with unfr0zen, add its folder here; on a unit, add it under
`/media/scratch/.unfr0zen/themes`. The Themes page lists
Stock, then every theme folder in the order the directory lists them, by
the `name` in its `theme.lua` (keep it on one line: `name = "My Theme",`);
`cargo test -p nestlocal-theme` loads and draws every theme folder in every
mode and both units.
