# Gen 2 dark theme verification

The j49 dark theme is a fixed set of word edits to the stock nlclient
5.9.4-5. Only the exact stock SHA256 in `j49-5.9.4-5.json` is accepted, and
each edited word must hold its stock value in the screen builder's output
before any word is written.

The edits give the settings screens the colours the gen 3's nlclient sets in
the same functions:

- the 32 light screens' background colour (`0xffeff1e7`, passed to the
  background layer's SetColor at 0x17d838) becomes black;
- the Settings carousel's tiles become `#00a1f2`, their titles white and
  their subtitles `#bfeaff`;
- Equipment's and Proview's background fades keep their alpha and lose
  their colour;
- the layer palette, set both by the nlLayer constructor (0x2d7a98) and by
  the base palette call (0x2d7860), takes the gen 3's roles: white text,
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

## Golden checks

`j49-5.9.4-5.json` was produced by an independent script that finds the
background calls by scanning for SetColor calls, rather than from the Rust
table. It records every edit as (address, stock word, new word), and the
digest and size of the stock executable with only these edits applied.

The public test compares the Rust edit table with the golden. The strict test
also needs the stock j49 sample: it applies the patch to the stock binary and
compares the whole output's digest, refuses changed sites, and applies the
patch together with the ordinary settings screen and the external
temperature patch.

```sh
NLCLIENT_REQUIRE_GOLDENS=1 cargo test --locked -p unfr0zen-core --lib --test j49_dark_theme
```

## Boot logo

`crates/unfr0zen-core/tests/j49_logo.rs` recolours the kernel's logo in the
kernel with the USB serial port from `build/j49/uImage`, and in the stock
kernel of `J49_NAND`'s boot0 when set: the image keeps its length, only the
header's CRCs and the gzip stream change, and the logo reads back dark. The
logo is found by its `linux_logo` structure and accepted only with Nest's
palette, by its SHA-256.

```sh
J49_NAND=nand.bin NLCLIENT_REQUIRE_GOLDENS=1 cargo test --locked --release -p unfr0zen-core --test j49_logo
```
