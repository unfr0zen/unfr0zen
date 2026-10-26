# Doom on the thermostat

Doom (the shareware episode) on the gen 3 and the gen 2, in place of their UI: unfr0zen's
`--doom` installs it, and **UNFR0ZEN → Doom** on the thermostat launches it.
Built from [doomgeneric](https://github.com/ozkl/doomgeneric)
(`platform/common/doom/doomgeneric`, a submodule) with this unit's platform layer,
`doomgeneric_nest.c`, as one static ARMv7 binary for both (`build.sh`, run by
`scripts/build-firmware.sh` into `build/doom-armv7`).

On the unit:

- `/nestlabs/sbin/doom`, the game; `/usr/share/doom/doom1.wad`, the shareware
  WAD (fetched from Debian's `doom-wad-shareware` by the app's build and
  checked by SHA-256; never committed).
- `/nestlabs/sbin/unfr0zen-doom` (`crates/nestlocal/rootfs/doom.sh`): stops
  nlclient, plays, starts nlclient again. nestlocal runs it when the screen
  leaves `/var/run/unfr0zen/doom.request` (`crates/nestlocal/src/doom.rs`).
- `/media/data/doom`: Doom's settings, saves and log.

The game:

- Picture: 320x200 scaled to the face's width; on the gen 3 (480x300) the
  circle cuts the status bar's outer corners, on the gen 2 it is shown as is.
- Controls: in a level you walk forward all along, the wheel (the gen 2's
  ring) turns and the button (the gen 2's glass press) fires; use is pressed now and then, so doors and switches open as you
  walk into them. Dead, the button restarts. In the menus the wheel moves and
  the button selects, answering yes to questions. Holding the button for three
  seconds quits.
- Sound: the unit has no audio output but its piezo, one tone at a time.
  Effects are Doom's PC speaker sounds; music is each song's highest note.
- While Doom runs nlclient is stopped, so the thermostat does not control the
  heating or cooling.
