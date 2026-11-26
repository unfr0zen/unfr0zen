# Unpatched firmware baselines

The optional j49 external-temperature input has its own
[golden and firmware-acceptance process](external-temperature/README.md).
Its fixed RCS bindings are not covered by settings-screen discovery alone.
The optional j49 dark theme has its own [golden](dark-theme/README.md).

The goldens were recorded from untouched stock executables extracted from
NAND dumps, or read from a unit by a run, with Debian clang 19.1.7 and
`ld.lld -O0`. No stock executable, NAND image, or proprietary payload is
committed. The corresponding private samples live in `.work/firmware/`:

| Sample                            | SHA256                                                             |
| --------------------------------- | ------------------------------------------------------------------ |
| `nlclient-genE-6.1-9` (6.1-9)     | `5da23c395b1ee0b258bc1d8a7a5a76931e778cf25afa8846e81245b38d088ca3` |
| `nlclient-genE-6.3-5` (6.3-5)     | `6bdf8634147c4adb4e0ac03dfc7fc8e3313c1c424420eb22f721a0012adaaf25` |
| `nlclient-genE-6.5-14` (6.5-14)   | `be7410378c46ea13c72d2db6ac71be92c44519b7bfe8a72490c691cef584afb4` |
| `nlclient-gen3-2025` (6.4-8)      | `d28f3e8ec232c4ef122f72ceb1028ddf480a0ad8454677bfc9e818a13cbb7c52` |
| `nlclient-gen3-6.3-5` (6.3-5)     | `7bc7e152ecee09ed993264f6d50c134b14a5464a3b4d294671c32f9ff1ec95e3` |
| `nlclient-gen3-6.5-14` (6.5-14)   | `3f1c68d3ab1a86dc3ea1807c7c0bd0e0e33475efb895d6cb17c5389bcabc94d2` |
| `nlclient-gen3-6.5-14-2` (6.5-14) | `ea07f0d31e27ed859e0e1307a7a4de493deaa346fb5b111277beef30d4664611` |
| `nlclient-gen3-6.0-6` (6.0-6)     | `5d58299c1219e632a534014e8853d4f1b796c9ae1d42a6f0dc68ee0c88e10c9a` |
| `nlclient-gen3-2019` (5.9.4-5)    | `bbbd94e1f70a4087c4a4a5581d32103788536e5f0facc58758183403a197f37d` |
| `nlclient-gen2-2019` (5.9.4-5)    | `67596bb71d0225a3b54966e96cc19bc469c0e593ee72e37bfe96fc30a3741bb7` |
| `nlclient-gen2-5.9.1-6` (5.9.1-6) | `fd5831409d01ef4a3f08a0cf5ae72dcbad61ee7629d81709cf9b63929dbf3a8d` |
| `nlclient-gen1-2019` (5.9.4-5)    | `e353b30291ac6825028ac03919d5ed5b1da75ad297a6ee6d47d6e227e7be0c87` |

Each golden records discovery (including ABI fingerprints and the library
calls each native makes), resolved native APIs, original size/program
headers/factory bytes, settings ordering, linked symbols and payload hash,
output hash, and decompressed undo-journal hash. `cargo run -p unfr0zen-core
--bin nlclient -- --golden STOCK build/custom-screen.o TOOLCHAIN` records a
sample's golden from the build image's object.
The 2025 reference was re-linked with `-O0`: only its payload/output stages
changed from the previous golden. Disabling lld string merging keeps input
string order, which the in-memory linker reproduces. The other two references
were recorded with that same setting. The subsequent PIE fix also updates
only payload/output: numeric layout bindings use ABS32 literals so the
loader cannot add its load bias to object sizes or offsets. Native pointers
keep their PC-relative addressing. These are independent Python/lld
reference results, recorded before deleting the Python implementation.

```sh
NLCLIENT_REQUIRE_GOLDENS=1 cargo test -p unfr0zen-core --test nlclient_golden \
  --test legacy_nlclient --test j49_dark_theme --test j49_external_temperature
```

`NLCLIENT_FIRMWARE_DIR` changes the private sample directory (an absolute
path: cargo runs the tests from the crate's directory);
`NLCLIENT_OBJECT` changes the compiled screen object (default
`build/custom-screen.o`). Use the build image's compiler to reproduce these
exact bytes. Golden linking needs the object but no firmware. The full-stage
checks need the original samples. Strict mode fails if any golden lacks its
sample, and if any sample in the directory has no golden (in `golden/`, or
`legacy/` for the 5.6-7 ABI), so every reviewed build is enforced; the 5.6-7
sample is found in that directory by its hash.
Ordinary public CI can check every payload without shipping firmware.
The gen3-6.3-5 and gen3-6.5-14-2 references record the linked payload
and output size; strict full-file verification also requires recording
their output and undo-journal hashes from the matching stock samples.

When `ld.lld` is present, a second test compares the Rust linker against
`ld.lld -O0` using exactly the same object and bindings for every variant.
`NLCLIENT_REFERENCE_LLD` selects its path. This is an optional developer
reference check, never part of recovery or a runtime requirement.

A relocation regression check requires every layout number to use ABS32.
Additional tests reject changed native code, changed activity code, bad
vtable adjustment metadata, unknown relocations and branches requiring
veneers. A synthetic address shift checks that discovery derives addresses
from the ELF rather than selecting an address map from its hash.

Keep these reference results fixed when porting. An intentional screen revision
requires review and a fresh independent reference before updating payload
hashes; never update discovery fingerprints simply to accept unknown firmware.

## Unreviewed builds

A build whose instruction shapes match no reviewed variant is patched on its
structure alone: the address, layout and call-shape checks every build
passes, each native's own shape (the loads, stores and calls the screen's
use of it assumes), and the imported symbols each native calls, which must
be a reviewed variant's for every native. Each variant in `abi_shapes.json`
records those imports beside its fingerprints; the fingerprints name the
build in the manifest and never block. Tests hold every reviewed build's
imports to every other's on all but one native (five for 6.5-14, whose
factories and event handlers call more), so that evidence stays what
survives a recompile. A run on such a build keeps its stock nlclient beside
the run log. `cargo run -p unfr0zen-core --bin nlclient -- --report STOCK`
(or `unfr0zen nlclient --discover STOCK`) prints what discovery finds in a
sample: the first step of reviewing a new variant, whose fingerprints and
imports it records.
