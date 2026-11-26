# 5.6-7 native UI baseline

The gen 3's 5.6-7 executable uses 32-byte `nlString` objects and compact
screen and layer vtables. `discovery/legacy.rs` resolves its bindings from the screen
registrations, vtables, imports and ARM instructions. The regression sample
has SHA-256 `e9b4f67b1e6c422a1bbe0c9bfd5aa6a63184010cc663b64374132553d393d741`.

The payload is `build/custom-screen-legacy.o`, compiled from the shared
screen source with `CUSTOM_LEGACY_ABI`, using Debian clang 19.1.7. Modern
builds keep their own payload and structural discovery. The legacy baseline
is separate from the modern ABI's cross-build evidence tests.

The private sample belongs at `.work/firmware/nlclient-gen3-5.6-7`.
Neither the executable nor its NAND dump is committed. To reproduce the
metadata-only golden:

```sh
cargo run -p unfr0zen-core --bin nlclient -- --golden \
  .work/firmware/nlclient-gen3-5.6-7 build/custom-screen-legacy.o \
  'Debian clang 19.1.7; ld.lld 19.1.7 -O0'
NLCLIENT_REQUIRE_GOLDENS=1 cargo test -p unfr0zen-core --test legacy_nlclient
```

`NLCLIENT_LEGACY_STOCK` and `NLCLIENT_LEGACY_OBJECT` override those paths.
Linking the golden needs only the compiled object. Stock-stage and mutation
checks need the private executable; strict mode requires both.

`cargo test -p unfr0zen-core --test legacy_setup` also checks the complete
setup when `NLCLIENT_LEGACY_ROOT` names a private extracted root filesystem.
All writes stay in memory. If the extractor omits permissions,
`NLCLIENT_LEGACY_MODES` names a JSON map of relative file names to the original
UBIFS inode modes, as integer permission bits.

The 27-slot name array permits Schedule and up to two optional settings
tiles: Zigbee, Matter, or Doom. Themes have no reviewed preload for this
firmware. Matter uses Nest Pro's registration; Schedule uses the Software
registration, absent from the stock menu. System Info stays stock.
