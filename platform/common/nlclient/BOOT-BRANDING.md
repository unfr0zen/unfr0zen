# Boot artwork

The setup keeps Google's stock boot artwork. It recolors nothing: the splash,
the 168-frame animation, the procedural dot/arc colors in nlclient and the
kernel logo all stay as Google ships them.

A unit can still carry ice-palette artwork with its originals saved here:

- `/etc/unfr0zen/boot/splash.original`
- `/etc/unfr0zen/boot/animation.original`
- `/etc/unfr0zen/boot/kernel_fdt.itb.original`

For each, when the live file exactly matches the ice-palette recoloring of its
saved original, the setup restores that original and keeps the backup. Any
other artwork, custom or unknown, is left untouched. Dry runs report the
restores without writing them. The vendor-signed kernel FIT is restored byte for
byte, so its configuration hashes and signatures stay valid.

nlclient is rebuilt from its stock original on every setup, so its boot-animation
colors are always Google's.

The developer harness produces the ice-palette form of a supported file, which
the setup recognizes:

```sh
cargo run -p unfr0zen-core --bin boot_branding -- \
  boot/kernel_fdt.itb kernel-from-unit.itb kernel-recolored.itb
```

Private fixture tests use `BOOT_BRANDING_FIXTURES`, a directory containing
`splash-stock.png`, `animation-stock.png` and `kernel-stock.itb`. They validate
unchanged PNG pixel/alpha chunks, kernel round trips, stock artwork left
untouched, and restores only from matching backups. Fixtures are not stored in
the repository.
