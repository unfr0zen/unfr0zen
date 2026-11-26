# Gen 2 external-temperature verification

The j49 patch is separate from settings-screen ABI discovery. Only the exact
stock SHA-256 checked by `external_temperature::patch` is accepted. A matching version string,
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

## Firmware checks

Clang assembles `j49-5.9.4-5.S` as a relocatable object. The emitter test
compares instruction bytes, local labels and branches against the assembly.
The stock test checks the mapped hooks, the command table,
loopback address construction, teardown and journal replay in both directions.

```sh
NLCLIENT_REQUIRE_FIRMWARE=1 cargo test --locked -p unfr0zen-core --lib --test nlclient_firmware
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
