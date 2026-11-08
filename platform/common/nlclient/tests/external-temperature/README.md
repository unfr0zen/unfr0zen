# Gen 2 external-temperature verification

The j49 patch is separate from settings-screen ABI discovery. Only the exact
stock SHA256 in `j49-5.9.4-5.json` is accepted. A matching version string,
similar instructions or a modified executable with the same layout is not
sufficient. Recovery restores an existing patch through its verified undo
journal before checking this identity.

Every edited instruction/table word and the appended executable segment's
layout are checked before editing. The patch enables the loopback RCS input,
registers the dormant native views for the main menu's Active Sensor chooser,
omits the duplicate Settings carousel entry, and
suppresses the built-in mock sensors. Native control tests sample validity
separately from value, including 0 °C. The input accepts only temperature
commands, checks association before acknowledgement, frames split TCP writes,
and bounds command size and idle time. Stock teardown owns stop/join/delete;
SO_REUSEADDR permits restart with old connections in TIME_WAIT.

## Golden checks

`j49-5.9.4-5.S` and `j49-5.9.4-5.ld` are an independent assembly/linker
reference, not inputs to the Rust emitter. They bind the compiled
`rcs_screen.c` object against the reviewed native APIs. The JSON records:

- the stock and ordinary-screen digests;
- the independent machine-code and linked C helper digests and symbols;
- every changed word, the complete output digest and size;
- the independently serialized, decompressed UFP1 undo-journal digest.

No firmware dump is committed. Public tests compare emitted instructions and
the compiled helper to the reference without a private firmware sample.
Strict tests additionally need the stock j49 sample and both screen objects.
They check the full executable and journal against the golden, refuse changed
patch sites/unknown stock bytes, verify mappings, and check forward/reverse
application. The volume tests cover dry runs, cloud blocking, idempotence and
removing the option while preserving the ordinary settings screen.

```sh
NLCLIENT_REQUIRE_GOLDENS=1 cargo test --locked -p unfr0zen-core --lib --test j49_external_temperature --test nlclient_golden
```

`NLCLIENT_RCS_OBJECT` overrides `build/j49-rcs-screen.o`. The parent README
describes the stock sample directory and `NLCLIENT_OBJECT` override.

The host C tests need no proprietary firmware:

```sh
clang -std=c11 -Wall -Wextra -Werror platform/common/nlclient/tests/rcs_screen_test.c -o .work/rcs-screen-test
.work/rcs-screen-test
cargo test --locked -p nestlocal sensor::
```

They cover view registration/failure/idempotence, the association gate,
fragmented/newline/EOF/oversized input, timeout/stop/error paths and clearing
stale acknowledgements. Rust sensor tests cover canonical IDs, actual delivery
acknowledgements, first-reading selection, native-startup versus manual OFF,
location updates, repeat/expiry behavior and concurrent new readings.

## Independent reference build

Run from the repository root inside the build image (Debian clang 19.1.7,
GNU ARM bare-metal linker and LLVM objcopy):

```sh
mkdir -p .work/j49-reference
sh platform/common/nlclient/build.sh build
clang --target=arm-none-eabi -march=armv7-a -c \
  platform/common/nlclient/tests/external-temperature/j49-5.9.4-5.S \
  -o .work/j49-reference/reference.o
llvm-objcopy --set-section-flags .rodata.str1.1=alloc,load,readonly,data,contents \
  build/j49-rcs-screen.o .work/j49-reference/reference-ui.o
arm-none-eabi-ld \
  -T platform/common/nlclient/tests/external-temperature/j49-5.9.4-5.ld \
  .work/j49-reference/reference.o .work/j49-reference/reference-ui.o \
  -o .work/j49-reference/reference.elf
llvm-objcopy -O binary --only-section=.text --only-section=.rodata \
  .work/j49-reference/reference.elf .work/j49-reference/reference.bin
sha256sum .work/j49-reference/reference.bin
```

Clearing SHF_MERGE/SHF_STRINGS on the reference copy prevents GNU ld from
suffix-merging strings: the in-memory linker's contract keeps input strings,
as `ld.lld -O0` does. It does not change the object embedded in the app. The
result must match `payload_sha256`, not merely the payload length. Construct
the complete patched file independently from these bytes and the reviewed
sites before recording a new output/journal reference.

## Accepting another build

1. Keep the original executable private and identify its full SHA256.
2. Review the hooks, live registers, native signatures, client/global/view
   offsets, association and sample paths, selector callbacks, control validity,
   loopback bind, polling, socket APIs and stop/join/delete lifecycle in that
   executable's disassembly. Settings-screen discovery does not establish
   these RCS-specific layouts.
3. Independently assemble/link with those bindings and construct the complete
   patched executable and journal. Record a separate golden and review every
   changed byte and the mapped segment; do not replace a golden merely because
   the Rust emitter produces different output.
4. Add explicit acceptance and negative tests for altered bytes/layouts. Run
   strict full-file/journal checks and ARM execution checks for startup,
   failure/fallback paths, 0 °C, registers and stack alignment.
5. Validate in isolated QEMU: one correctly named/selectable sensor; real
   MQTT-to-native control and relay response; split/oversized/stalled peers;
   manual backplate choice; repeat delivery, expiry and recovery; native and
   service restarts/cold boot; option removal and reinstallation. Keep network
   credentials and unrelated firmware untouched. Report hardware separately.

Never broaden acceptance by adding a version string or changing only a hash.
