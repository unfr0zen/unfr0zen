# Gen 2 dark theme verification

The dark theme locates the gen 1 and gen 2 palette operations using UI class
vtables, calls, colour constants and their stores and uses. The complete patch
requires all palette roles. Each edited word must still hold its stock value
in the screen builder's output before any word is written.

The edits give the settings screens the colours the gen 3's nlclient sets in
the same functions:

- the 32 light screens' background colour (`0xffeff1e7`, passed to the
  background layer's SetColor) becomes black;
- the Settings carousel's tiles become `#00a1f2`, their titles white and
  their subtitles `#bfeaff`;
- Equipment's and Proview's background fades keep their alpha and lose
  their colour;
- the layer palette, set both by the nlLayer constructor and by
  the base palette call, takes the gen 3's roles: white text,
  `#c4c4c4` secondary text, `#666666` unselected rows and field outlines,
  `#00a3d9` selection, `#004157` fields being edited, black field fills and
  dialog backgrounds, `#e0e0e0` for the last slot;
- the message screen's list, the schedule's New/Cancel/Done menu, the
  wiring diagram's selected terminal, the progress ring, Nest Pro's
  information and nlChapterLayer's unselected rows take the gen 3's colours
  where they set their own;
- the disc drawn behind the boot logo and the restarting message becomes
  black;
- the letter wheel's selected-letter bubble becomes `#00a3d9`;
- Equipment detection's title, which it sets to black after the palette,
  becomes white.

## Firmware checks

The test derives the set of changed bytes from stock and patched copies,
checks that the same palette edits survive composition with both screen
patches, and verifies exact undo. It checks every gen 1 and gen 2 reference
executable and verifies that reordering independent palette stores leaves
the dark output unchanged.

```sh
NLCLIENT_REQUIRE_FIRMWARE=1 cargo test --locked -p unfr0zen-core --lib --test j49_dark_theme
```

## Boot logo

`crates/unfr0zen-core/tests/j49_logo.rs` recolours the kernel's logo in the
kernel with the USB serial port from `build/j49/uImage`, and in the stock
kernel of `J49_NAND`'s boot0 when set: the image keeps its length, only the
header's CRCs and the gzip stream change, and the logo reads back dark. The
logo is found by its `linux_logo` structure and accepted only with Nest's
palette, by its SHA-256.

```sh
J49_NAND=nand.bin NLCLIENT_REQUIRE_FIRMWARE=1 cargo test --locked --release -p unfr0zen-core --test j49_logo
```
