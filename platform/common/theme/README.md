# Customizable home screen themes (gen 3)

With `--themess` (the app's **Customizable home screen themes**), the UNFR0ZEN
screen's **Themes** page picks how the home screen's dial is drawn: Stock
(nlclient's own) or one of nestlocal's Lua themes (`crates/nestlocal/themes`).
A theme draws in nlclient's own GLES2 context, on the GPU, at the panel's rate
while it moves; nlclient keeps the ring, the button, the menus and the
setpoints.

| Part | What it does |
|-|-|
| `theme.c` → `/nestlabs/lib/libunfr0zen-theme.so` | Preloaded into nlclient beside `liblocalcloud.so`. Hooks the dial's, its figures', the background's and the thermostat screen's vtable slots, replaces the dial's draw with the theme's, and asks nlclient for a redraw each frame while the theme moves, about once a second while it stands still. |
| `crates/nestlocal-theme` → `/nestlabs/lib/libnestlocal_theme.so` | The theme host and painters (femtovg and batched quads, LuaJIT), loaded by the preload on nlclient's GL thread. |
| `crates/nestlocal/src/theme.rs` (`-themes`) | Unpacks the themes, offers them on the Themes page, keeps the choice in `/media/data/nestlocal.theme`, and hands the library the theme with the device and shared buckets. |
| `unfr0zen-core`'s `nlclient::theme` | Writes `/etc/unfr0zen/theme.bindings` for the unit's stock nlclient at the patch. |

## How the theme is drawn

The dial's draw places a 480×480 marker image
(`/var/run/unfr0zen/theme-marker.png`, every pixel RGBA `12 34 56 ff`) where
the dial would show. The preload tracks the texture nlclient uploads it into;
at full size that texture's draw becomes the theme's, straight into the
framebuffer nlclient has bound, and scaled into the menus the theme draws into
the texture instead. The preload saves and restores every piece of GL state
the library can touch, and stock's own drawing runs whenever no theme is
chosen or the library fails.

Each frame the theme reads the native control model through the preload
(`nestlocal_native_theme_v1`): target, room temperature, heat-cool ends and the
end picked or being edited (the slider layer's word at `0xec`), mode, the
heating or cooling backdrop, humidity and the leaf. The ring's turns reach the
theme from the thermostat screen's event hook.

The library tells the preload when the theme needs its next frame
(`nestlocal_theme_next`): at once while it animates, when it asked with
`gfx:wake`, otherwise never; standing still the theme is redrawn about once a
second to follow the clock, nestlocal's state and nlclient's model, and a turn
of the ring asks for a frame at once. A theme that fails draws stock until
nestlocal publishes a new `theme.native.json`, when it is tried again.

Redraws go through nlclient's event queue as a handler returning nlClient's
redraw result (1), which draws only while the display is lit: never the
light-display result (5), which re-arms the backlight's ramp. The display
sleeps on nlclient's own timer, theme or not.

nlclient's framebuffer has no stencil, which femtovg needs for concave fills
and overlapping translucent strokes. A frame drawn straight to it that enables
the stencil test is drawn again through femtovg's own render target before it
is shown, and that theme's later frames go through it too.

## Bindings

Discovery finds the dial, its event handler, its figures and the natives the
preload calls in any build; the background layer, brightness manager, views,
displayed temperature and humidity, and the leaf come from the builds reviewed
by hand (`REVIEWED` in `nlclient/theme.rs`). Another build is refused at the patch. The preload
hooks nothing without a complete `/etc/unfr0zen/theme.bindings` (`name value`
lines) or when a vtable slot does not hold the stock method it names.

## Building

`platform/common/radio/thread/nestlocal.sh` runs `build.sh build/theme` in the build
image. The preload links against nothing but libc, libdl and libpthread: its
GL calls resolve against nlclient's own libGLESv2 when it is loaded. The
library builds `crates/nestlocal-theme` with LuaJIT for the Cortex-A9 against
glibc 2.19; LuaJIT's 32-bit build tools run under `qemu-i386` on arm64 hosts
(`host-cc.py`).
