# Compact settings screen

The compact ABI uses 32-byte `nlString` objects and compact screen and layer
vtables. The tests identify this ABI in the private firmware directory and
use `build/custom-screen-legacy.o`, built with `CUSTOM_LEGACY_ABI`. They
check the menu capacity, compatibility with a metadata change and rejection
of an incompatible native getter.

```sh
NLCLIENT_REQUIRE_FIRMWARE=1 cargo test -p unfr0zen-core --test legacy_nlclient
```

`NLCLIENT_LEGACY_STOCK` and `NLCLIENT_LEGACY_OBJECT` select a specific
executable and compiled screen object.

`cargo test -p unfr0zen-core --test legacy_setup` also checks the complete
setup when `NLCLIENT_LEGACY_ROOT` names a private extracted root filesystem.
All writes stay in memory. If the extractor omits permissions,
`NLCLIENT_LEGACY_MODES` names a JSON map of relative file names to the original
UBIFS inode modes, as integer permission bits.

The 27-slot name array permits Schedule and up to two optional settings
tiles: Zigbee, Matter, or Doom. Themes have no reviewed preload for this
firmware. Matter uses Nest Pro's registration; Schedule uses the Software
registration, absent from the stock menu. System Info stays stock.
