# Automatic boot branding

CLI and GUI patching automatically recolors recognized Google boot artwork with
unfr0zen's ice palette. Shapes, alpha, sprite frames, geometry and timing stay
unchanged. Unknown or custom assets are skipped independently, without a UI option.

The supported splash and 168-frame PNGs match their original SHA-256 hashes.
Only their PLTE chunks and palette CRCs change. Procedural dot/arc colors match
one ARM function containing all four Google colors, validated by its instruction
fingerprint. Those edits share nlclient's undo journal and full original backup.
The 2019 Gen 2 and Gen 3 animation implementations do not match that fingerprint.

Automatic patching preserves the vendor-signed kernel FIT. If the live FIT
exactly matches the recolored version of its saved original, patching restores
that original and retains the backup. Custom kernels are left untouched.

The developer harness can recolor the supported 6.4-8 kernel logo using a CLUT224
structure and combined palette/pixel SHA-256 inside `/boot/kernel_fdt.itb`.
Pixel indices stay unchanged.
Only the affected LZO block is recompressed; the zImage length, decompressor,
trailer and FIT layout stay unchanged. A decompression round trip verifies the
result, and the kernel image SHA-256 is updated. Unknown logos, unsupported
compression, malformed FITs and replacements exceeding compressed space are skipped.

Vendor configuration hashes and signatures are retained, not regenerated.
QEMU boot acceptance does not validate physical-device signature enforcement.

PNG originals are saved once, root-only, and verified before replacement:

- `/etc/unfr0zen/boot/splash.original`
- `/etc/unfr0zen/boot/animation.original`

Kernel recovery uses `/etc/unfr0zen/boot/kernel_fdt.itb.original` when present.

Existing backups are never overwritten. A mismatching backup skips its artwork.
Dry runs report replacements and backups without writing either.

The developer harness also supports experimental kernel transformations:

```sh
cargo run -p unfr0zen-core --bin boot_branding -- \
  boot/kernel_fdt.itb kernel-from-unit.itb kernel-recolored.itb
```

Private fixture tests use `BOOT_BRANDING_FIXTURES`, a directory containing
`splash-stock.png`, `animation-stock.png` and `kernel-stock.itb`. They validate
unchanged PNG pixel/alpha chunks, kernel round trips, unknown-file skipping,
dry runs, one-time backups and failed backup writes. `NLCLIENT_STOCK` selects
the 6.4-8 executable for procedural-color and undo checks. Fixtures are not
stored in the repository.
