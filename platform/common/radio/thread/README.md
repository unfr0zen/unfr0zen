# Matter over Thread on EM3581 and EM357

Experimental radio and Linux transport for the Nest gen3 and gen2. In the GUI, enable
Matter and select **Thread support (replaces Heat Link)**, or pass
`--matter-thread` with `--matter` to the command-line tool, to install the radio
firmware, SPI bridge and OpenThread daemon. Thread and Zigbee disable each other's
checkbox. Both are off by default and require exclusive use of the onboard radio.

`nestlocal -matter-thread` exposes Matter's Thread Network Commissioning and
Thread Diagnostics clusters. The commissioner supplies an operational dataset
through `AddOrUpdateThreadNetwork` and attaches through `ConnectNetwork`.
Credentials are validated before staging and saved by Matter on successful
commissioning; fail-safe rollback reloads committed credentials. An empty
network store clears the radio's dataset once commissioning is idle.

Commissioning uses the unit's existing Wi-Fi connection or the DA14580 BLE
transport on supported firmware. There is no dataset field in the GUI; the
commissioner supplies Thread credentials over IP or BLE. Thread discovery includes
the protocol version through the daemon's `nest-discover` command. Matter's UDP
socket accepts both Wi-Fi and Thread traffic, with mDNS on Wi-Fi and SRP on
Thread. Phone commissioning compatibility and automatic transport failover
require device testing.

The service keeps OpenThread state under `/media/data/nestlocal-thread` (0700).
Matter's credential directory is also private when Thread is selected. Repatching
without Thread disables its startup and supervision without deleting saved
credentials. Stock wpantund is disabled while Thread or Zigbee owns the radio.

## Architecture

The radio runs one image for Thread and Zigbee (`../radio.bin`). It boots as
the OpenThread radio coprocessor in `rcp.c`; nestlocal's Zigbee worker asks for
the EmberZNet NCP with the RCP's SPI command 6, which reboots the radio into it
(`../personality.c`), and the next hardware reset brings the RCP back. One host
owns the radio at a time; switching between Thread and Zigbee needs no reflash.
The shared EmberZNet 6.6 SDK supplies the EM3581 PHY, calibration, hardware ACK and
receive interrupts. OpenThread supplies Spinel, CSMA backoff and retransmission.
The Zigbee stack is initialized for its driver services but does not join or
maintain a network. Incoming frames pass from the PHY interrupt into a bounded
queue and bypass Zigbee MAC processing. Startup and C runtime glue are shared
with the Zigbee NCP in `../runtime.c`.

The `thread-rcp` binary (`crates/nestlocal/src/bin/thread-rcp.rs`) carries HDLC
bytes over Nest's existing SPI wiring and exposes a pseudo-terminal. `ot-daemon`
runs the Thread stack on Linux and creates `thread0`, a 1280-byte-MTU IPv6 TUN
interface. `ot-ctl -I thread0` controls that instance. The installed service uses full-device router mode (`mode rdn`).
The Linux host uses OpenThread's Thread 1.3 profile with SRP enabled. The RCP
provides software transmit security and timed transmission, using the EM3581's
24 MHz DWT cycle counter for its microsecond clock. Certification and sleepy-end-
device timing conformance are outside this experimental implementation.

The gen2's EM357 (`PART=em357`) has too little flash and RAM for both
personalities: its Thread image (`PERSONALITY=thread`) is the RCP alone, with
OpenThread's buffers sized for its 12 KiB RAM (`../em357/openthread-rcp-config.h`),
and nestlocal flashes the Zigbee image (`PERSONALITY=zigbee`) in its place for
Zigbee. Its Linux 2.6.37 has no `CLOCK_BOOTTIME`, so the daemon runs on the
monotonic clock and nestlocal keeps the unit awake while Thread runs.

The bridge waits on the PTY, a local alarm socket and the radio's GPIO interrupt.
It loads the same `zbwake.ko` builds as Zigbee. The RCP asserts `nHOST_INT` for
queued Spinel data and scheduled host wakes. The Linux stack uses
`CLOCK_BOOTTIME`, so its clock advances during suspend. Before each idle wait,
it arms an EM3581 alarm 300 ms ahead of its next deadline and publishes an idle
record only after the bridge acknowledges the SPI command. nestlocal declines
sleep while the host is busy, the radio interrupt is low, or less than 200 ms
remain before that alarm. This policy also applies when Matter uses Wi-Fi
alongside `thread0`. The radio, bridge, daemon and nestlocal must all use matching
builds; the SPI handshake checks protocol version 2. `zbwake.ko` version 2 holds
the CPU awake for 100 ms on each IRQ to protect SPI transfers during suspend
preparation. Reboot after replacing an already loaded older module.

OpenThread router maintenance schedules frequent host wakes. IRQ-driven suspend
does not provide the same power profile as running a complete router stack on
the EM3581. The RCP has separate 64-entry short- and extended-address source-match
tables. The SDK PHY's receive-address ISR consults these tables before automatic
ACK transmission; disabling matching sets pending for every data poll. The actual
ACK pending result accompanies each received frame to OpenThread, allowing its
host-side indirect queue to deliver data to polling sleepy children. The build
redirects only the PHY object's child lookups, leaving SDK Zigbee state separate.
Physical ACK timing and sleepy-child delivery require hardware validation.

The built ARM tables and SDK early-ACK ISR can be tested without RF hardware:

```sh
python3 -m venv .work/thread-radio-test-venv
.work/thread-radio-test-venv/bin/pip install unicorn pyelftools
.work/thread-radio-test-venv/bin/python radio/thread/test-source-match.py build/radio/radio.elf
.work/thread-radio-test-venv/bin/python radio/thread/test-radio.py build/radio/radio.elf
.work/thread-radio-test-venv/bin/python radio/thread/test-personality.py build/radio/radio.elf
```

The radio supplies live calibrated RSSI, CCA threshold access and asynchronous
energy scans. Scans suppress automatic ACKs on the scan channel and restore the
previous receive channel and radio state on completion. Entropy uses the SDK's
strong random API backed by its radio entropy source; an unavailable source
returns an error without falling back to the HAL LFSR.

The daemon's `nest-diag` command supplies public diagnostics to Matter every two
seconds: role, channel, network identifiers, neighbors, routes, leader data,
security policy, dataset component presence and MAC counters. Network keys and
PSKc are excluded from the command output. Failed snapshots clear cached data.
The native diagnostic producer can be exercised with mocked OpenThread APIs:

```sh
c++ -std=c++11 -Wall -Wextra -Werror -I .work/openthread/include \
  platform/common/radio/thread/test-diagnostics.cpp -o .work/test-thread-diag
.work/test-thread-diag
```

Thread occupies the same radio as Zigbee and Heat Link. Neither `wpantund` nor
nestlocal's Zigbee worker may access it while the bridge runs. Wi-Fi can remain
available for SSH without carrying the Thread interface's packets.

## Build

The build uses the project's `unfr0zen-build` Docker image, the SDKs fetched by
[`../fetch-sdk.sh`](../fetch-sdk.sh), and the pinned OpenThread source checkout:

- [OpenThread](https://github.com/openthread/openthread/tree/3dde010460e5ee1cc06b07108e10c052caa754ed).

From the repository root:

```sh
platform/common/radio/fetch-sdk.sh
platform/common/radio/thread/fetch-openthread.sh
docker build -t unfr0zen-thread-build -f platform/common/radio/thread/Dockerfile platform/common/radio/thread
docker run --rm -v "$PWD:$PWD" -w "$PWD" \
  -e CARGO_HOME="$PWD/.work/cargo" unfr0zen-thread-build platform/common/radio/thread/build.sh
```

`build.sh` first rebuilds the `zbwake.ko` modules whose build is missing or
older than the sources in `platform/diamond3/zbwake/`.

`SDK`, `GSDK`, `OT`, `OT_BUILD`, `OT_HOST_BUILD`, `NESTLOCAL_ZBWAKE` and
`NESTLOCAL_RADIO` (`NESTLOCAL_RADIO_EM357` for the gen2's Zigbee image) can select existing checkouts, build directories and the
radio image nestlocal carries. Pass these through Docker when overriding them.
Outputs are `build/radio/radio.bin` and `radio.identity` (the gen3's radio
image, from `../Makefile`), `build/radio-em357-zigbee` and
`build/radio-em357-thread` (the gen2's), and `build/thread/thread-rcp`, `ot-daemon`, `ot-ctl` and `nestlocal`.
`radio.identity` contains the SHA-256 fingerprint of the compiled radio objects,
stack libraries and linker inputs. It is embedded in the image and installed beside
it as `thread-rcp.identity`. The bridge checks that identity before opening its PTY; a probe
mismatch triggers flashing and a second identity check, even when the existing
firmware uses the same SPI protocol version.

On the EM3581 the application starts at `0x08004000`; the image includes the
8 KiB prefix expected by Nest's loader at `0x08002000`. On the EM357 it starts
at `0x08002000` itself. The bootloader and manufacturing
storage are outside the application image.

## Device use

Keep `/usr/share/silabs/data/firmware/ip-modem-app.bin` available for rollback.
Use a shell cleanup trap that stops both host processes before restoring it with
`zb-loader --app-easyload` and resetting the radio. Nest's loader verifies the
application CRCs but its ConnectIP version query does not recognize this firmware;
validate the RCP handshake as well as every reported CRC.

Executables must be placed on an executable filesystem: stock `/tmp` is mounted
`noexec`. Manual research deployments can use the commands below without the installer.
Create `/run` for OpenThread's control socket if it is absent. Run the daemon
from a private persistent directory, such as `/media/data/nestlocal-thread`
(mode `0700`); its default `tmp` settings directory is relative to its working
directory and contains Thread credentials.
For manual deployment, install `radio.identity` as
`/usr/share/nestlocal/thread-rcp.identity`, or pass its path with the bridge's
`--identity FILE` argument.

After flashing, start `thread-rcp`; its first output line is the pseudo-terminal
path. Pass that path to the daemon:

```sh
ot-daemon -I thread0 -v \
  'spinel+hdlc+uart:///dev/pts/0?uart-baudrate=115200&no-reset'
```

Use the actual printed path. The bridge retains the startup notification until
the host's first request. `--trace` logs HDLC payloads and must not be used while
provisioning credentials or carrying private traffic.

Provision an active operational dataset through `ot-ctl dataset set active`,
then run `mode rn`, `ifconfig up`, and `thread start`. Keep the dataset in a
restricted file and omit it from logs and shell history. Inspect `state`,
`parent`, `ipaddr`, and `counters mac`; a real attachment requires a parent and
bidirectional traffic. Thread credentials and Matter fabric credentials are
independent.

## Matter transport

With `thread0` up and a child attached, provide a Linux IPv6 route through it to
the controller's network. On a unit with no existing IPv6 default route:

```sh
ip -6 route add default dev thread0
```

Run the built nestlocal with its usual settings plus `-matter-thread`. Stop the
existing instance and prevent its supervisor from restarting it first. This
flag implies `-matter`, rejects `-zigbee`, and requires sleep-manager registration
(`-wake-iface` must not be empty). It permits suspend only with an armed host
wake alarm and uses
`/run/openthread-thread0.sock` to maintain SRP registrations. A border router
providing an SRP server and DNS-SD advertising proxy is required.

The daemon is dedicated to this Matter service: nestlocal removes obsolete
Matter SRP records when the commissioning window or fabrics change. Wi-Fi
remains available for administration and Matter. Existing Matter fabric data is reused;
provisioning a Thread dataset does not replace a Matter controller. An unpaired
unit advertises its pairing service for on-network commissioning and, on
supported DA14580 firmware, BLE commissioning.

Validate controller writes and reported values while capturing UDP port 5540
on `thread0`. The address registered through SRP must also be present in
`ip -6 addr show dev thread0`; compare it with `ot-ctl -I thread0 ipaddr` when
provisioning the Linux interface. An attached radio or successful ICMP ping alone does not validate
Matter. Restore the original nestlocal binary, service supervision, routing and
radio firmware when ending an experimental deployment. Before stopping the
daemon, unregister its SRP host with `ot-ctl -I thread0 srp client host remove 1 1`
so the border router withdraws the experimental DNS-SD advertisements.

For Wi-Fi Matter alongside a Thread mesh router, use `-matter` without
`-matter-thread`, and configure the daemon with `mode rdn` and
`routereligible enable`. Once attached, `state router` requests promotion.
This is a Thread mesh router, not a Wi-Fi/LAN border router: border routing,
an SRP server and DNS-SD proxy services are not included in this host build.

QEMU can exercise Linux code, but cannot validate EM3581 RF, SPI interrupts or
hardware suspend/wake.
