# Native patch verification

Place the stock nlclient executables in `.work/firmware/`, or set
`NLCLIENT_FIRMWARE_DIR` to their directory's absolute path. The settings
tests enumerate its files and select the modern or compact screen object
using each ELF's ABI. They decode the patched factory branches, inspect the
settings menu, replay the journal and restore the original executable.

```sh
NLCLIENT_REQUIRE_FIRMWARE=1 cargo test --locked -p unfr0zen-core --test nlclient_firmware \
  --test legacy_nlclient --test j49_dark_theme --lib
```

Build the three screen objects with `platform/common/nlclient/build.sh build`.
`NLCLIENT_OBJECT`, `NLCLIENT_LEGACY_OBJECT` and `NLCLIENT_RCS_OBJECT`
override their paths. Strict mode requires the test objects, the private
samples used by the mutation and device-specific tests, Clang's ARM
assembler and `ld.lld`. Set `NLCLIENT_REFERENCE_LLD` to select the linker.
The linker tests compare the payload bytes with LLVM using both synthetic
bindings from the objects' undefined symbols and the actual bindings of each
stock executable.

For direct compatibility checks, set `NLCLIENT_REFERENCE_BUILDER` to the
absolute path of the `nlclient` developer binary built from the revision
being compared. Each settings test build uses the same stock and screen
object for both builders and compares the executable and decompressed journal
byte for byte. The reference builder must support the samples in the directory.

The [compact UI](legacy/README.md), [dark palette](dark-theme/README.md)
and [external temperature](external-temperature/README.md) tests cover
the additional constraints of those patches.

## Discovery reports

Settings discovery checks each build's registrations, object layout, native
instructions and the library calls required by their roles. The manifest
records the addresses, layouts, function fingerprints and import evidence.
A failed settings build saves its stock executable beside the run log.
`cargo run -p unfr0zen-core --bin nlclient -- --report STOCK` (or
`unfr0zen nlclient --discover STOCK`) prints the bindings and ABI evidence.
