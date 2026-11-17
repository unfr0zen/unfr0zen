# Customizable home screen themes (gen 2 and gen 3)

With `--themes` (the app's **Customizable home screen themes**), the UNFR0ZEN
screen's **Themes** page picks how the home screen's dial is drawn: Stock
(nlclient's own) or one of nestlocal's Lua themes (`crates/nestlocal/themes`).
On the gen 3 a theme draws in nlclient's own GLES2 context, on the GPU, at
the panel's rate while it moves; the gen 2 has no GPU, and draws it on the
CPU. nlclient keeps the ring, the button, the menus and the setpoints.

| Part                                               | What it does                                                                                                                                                                                                                                                                                         |
| -------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `theme.c` → `/nestlabs/lib/libunfr0zen-theme.so`   | Preloaded into nlclient beside `liblocalcloud.so`. Hooks the dial's, its figures', the background's and the thermostat screen's vtable slots, replaces the dial's draw with the theme's, and asks nlclient for a redraw each frame while the theme moves, about once a second while it stands still. |
| `crates/nestlocal-theme` → `libnestlocal_theme.so` | The theme host and painters (femtovg and batched quads on the GPU, tiny-skia on the CPU; LuaJIT), loaded by the preload on nlclient's UI thread.                                                                                                                                                     |
| `crates/nestlocal/src/theme.rs` (`-themes`)        | Offers the installed themes on the Themes page, keeps the choice in `/media/data/nestlocal.theme`, and hands the library the theme with the device and shared buckets, the fields of MQTT's state topic and Home Assistant's weather.                                                                |
| `unfr0zen-core`'s `nlclient::theme`                | Writes `/etc/unfr0zen/theme.bindings` for the unit's stock nlclient at setup.                                                                                                                                                                                                                        |

## How the theme is drawn

The dial's draw places a marker image the display's size (480×480 on the gen
3, 320×320 on the gen 2; `/var/run/unfr0zen/theme-marker.png`, every pixel
RGBA `12 34 56 ff`) where the dial would show.

On the gen 2 nlclient draws on the CPU into the framebuffer's pages. The
library paints the theme's frame with tiny-skia straight into the marker
image's pixels (`nestlocal_theme_draw_pixels`), which nlclient's software
context then draws as the dial would be: placed, and scaled and faded into
the menus, where the frame drawn last moves with the dial. A frame nothing
changed is not painted again: the image keeps it.

In GLES2 the preload tracks the texture nlclient uploads the marker into;
at full size that texture's draw becomes the theme's, straight into the
framebuffer nlclient has bound, and scaled into the menus the theme draws into
the texture instead. The preload saves and restores every piece of GL state
the library can touch, and stock's own drawing runs whenever no theme is
chosen or the library fails.

Each frame the theme reads the native control model through the preload
(`nestlocal_native_theme_v1`): target, room temperature, heat-cool ends and the
end picked or being edited (the slider layer's word at `0xec`), mode, the
heating or cooling backdrop, humidity and the leaf. The ring's turns reach the
theme from the thermostat screen's event hook. The rest comes from nestlocal:
`ui.thermostat` takes the stages running, Early-On, Sunblock, the time to
target, the outside temperature and stock's weather picture from it, and
`ui.nest` holds all of it as nestlocal gives it: `device` and `shared` (the
unit's buckets), `state` (what MQTT's state topic, and so Home Assistant,
shows) and `weather` (Home Assistant's outdoor readings, its condition,
sunrise, sunset and forecast). `ui.clock` gives the local time with its
epoch seconds and UTC offset, for the times the weather gives as epoch
seconds.

A theme that animates for long, as with rain or snow, draws its particles as
stamps of layers it drew once (`gfx:warp_layer`): each is one textured quad,
and runs of the same layer at the same opacity are one GPU draw (on the CPU,
one copy of the layer).

The library tells the preload when the theme needs its next frame
(`nestlocal_theme_next`): at once while it animates, when it asked with
`gfx:wake`, otherwise never; standing still the theme is redrawn about once a
second to follow the clock, nestlocal's state and nlclient's model, and a turn
of the ring asks for a frame at once. A theme that cannot load or draw leaves
the dial to the library's diagnostic (the target, the theme and its error);
the library itself failing draws stock until nestlocal publishes a new
`theme.native.json`, when it is tried again. A theme nlclient does not survive
3 starts in a row is refused until it is picked again
(`/media/scratch/.unfr0zen/theme.guard`), or until a setup or update, which
installs the themes anew, forgets the count.

Redraws go through nlclient's event queue as a handler returning nlClient's
redraw result (1), which draws only while the display is lit: never the
light-display result (5), which re-arms the backlight's ramp. The display
sleeps on nlclient's own timer, theme or not.

In GLES2, nlclient's framebuffer has no stencil, which femtovg needs for concave fills
and overlapping translucent strokes. A frame drawn straight to it that enables
the stencil test is drawn again through femtovg's own render target before it
is shown, and that theme's later frames go through it too.

## Bindings

Discovery finds the dial, its event handler, its figures and the natives the
preload calls in any build; the background layer, brightness manager, views,
displayed temperature and humidity, the leaf, the globals' setpoint, unit and
mode getters, the dial's size and whether nlclient draws on the CPU come from
the builds reviewed by hand (`REVIEWED` in `nlclient/theme.rs`): the gen 3's
6.4-8 and 6.5-14, the gen 2's 5.9.4-5. Another build is refused at setup. The preload
hooks nothing without a complete `/etc/unfr0zen/theme.bindings` (`name value`
lines) or when a vtable slot does not hold the stock method it names.

## Building

`platform/common/radio/thread/nestlocal.sh` runs `build.sh build/theme` in the build
image. The preload links against nothing but libc, libdl and libpthread: its
GL calls resolve against nlclient's own libGLESv2 when it is loaded, and are
never made on the gen 2. Both build for the gen 2's Cortex-A8, which the gen
3's A9 runs too, against glibc 2.19. The library builds
`crates/nestlocal-theme` with LuaJIT, linked to be bound lazily, and the
preload loads it so: glibc 2.19's loader binds a library at once only when
its `.rel.plt` follows its `.rel.dyn`, which zig's linker does not lay out.
LuaJIT's 32-bit build tools run under `qemu-i386` on arm64 hosts
(`host-cc.py`).
