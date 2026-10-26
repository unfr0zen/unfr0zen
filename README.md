# unfr0zen
[![GitHub Release](https://img.shields.io/github/v/release/unfr0zen/unfr0zen?include_prereleases)](https://github.com/unfr0zen/unfr0zen/releases)
[![GitHub License](https://img.shields.io/github/license/unfr0zen/unfr0zen)](LICENSE)
[![Discord](https://img.shields.io/badge/Discord-join-5865F2?logo=discord&logoColor=white)](https://discord.gg/4YC7Yj3Tx)
[![Buy Me A Coffee](https://img.shields.io/badge/Buy%20Me%20A%20Coffee-donate-yellow.svg)](https://buymeacoffee.com/willtm)

Root and free the Nest Learning Thermostat (1st, 2nd and 3rd generation) over USB.
Local control over MQTT, Matter (Wi-Fi, Thread) or Zigbee, no Google cloud needed.

[The unfr0zen app on macOS](assets/app.png)

## Device Support

| Device | Codename | SoC |
|-|-|-|
| Nest Learning Thermostat (3rd generation) | `diamond3` | i.MX6SoloX |
| Nest Learning Thermostat (2nd generation) | `j49` | TI AM3703 |
| Nest Learning Thermostat (1st generation, experimental) | `diamond` | TI AM3703 |

The Gen 1 Diamond path has no stock NAND or physical-unit verification. Diamond
and j49 share the ROM identity `0451:d00e`, which is taken as j49; their app
identities differ. Start a Gen 1 while it runs its firmware, or pass
`--device diamond` to the CLI before USB boot.
Gen 1 uses the Diamond x-loader and the OMAP NAND/JFFS2 path. USB shell, Zigbee
and Matter over Thread are unavailable: its CC2530 radio uses UART, not the
Gen 2 SPI protocol. MQTT and Matter over Wi-Fi still need Gen 1 firmware testing.
On the 2nd generation, Zigbee and
Matter over Thread are untested on a unit, and Matter keeps the unit awake.

## Computer Requirements
1. **A USB data cable.** Gen 1 uses mini-USB; Gen 2 and 3 use micro-USB. Charge-only cables won't work.
2. **Windows (x86_64, arm64), macOS 13+ (universal, arm64, x86_64) or Linux (x86_64, arm64, riscv64; glibc 2.28+).**

## Installing
Download the latest build from the [releases page](https://github.com/unfr0zen/unfr0zen/releases).

- **Windows:** unzip and run `unfr0zen.exe`. 1st and 2nd generation: bind the WinUSB
  driver to the boot ROM (`0451:d00e`) once with [Zadig](https://zadig.akeo.ie).
- **macOS:** unzip and open `unfr0zen.app`. It is signed ad hoc, not notarized: run
  `xattr -dr com.apple.quarantine unfr0zen.app`, or allow it under System Settings → Privacy & Security.
- **Linux:** `tar xf unfr0zen-linux-x86_64-vX.Y.Z.tar.gz && ./unfr0zen`, after the one-time USB setup below.

<details>
<summary>Linux: USB access and runtime dependencies (click to expand)</summary>

Without a udev rule only root can open the unit. Keep the `70-` prefix: the rule
has to run before systemd's `73-seat-late.rules` for `uaccess` to take effect.

```sh
sudo tee /etc/udev/rules.d/70-unfr0zen.rules > /dev/null <<'EOF'
# i.MX ROM serial download, recovery U-Boot
ATTRS{idVendor}=="15a2", ATTRS{idProduct}=="0071", TAG+="uaccess", ENV{ID_MM_DEVICE_IGNORE}="1"
ATTRS{idVendor}=="18d1", ATTRS{idProduct}=="4ee0", TAG+="uaccess", ENV{ID_MM_DEVICE_IGNORE}="1"
# unfr0zen initramfs shell and the Nest application's console
ATTRS{idVendor}=="0525", ATTRS{idProduct}=="a4a7", TAG+="uaccess", ENV{ID_MM_DEVICE_IGNORE}="1"
ATTRS{idVendor}=="0525", ATTRS{idProduct}=="b4a4", TAG+="uaccess", ENV{ID_MM_DEVICE_IGNORE}="1"
ATTRS{idVendor}=="2464", ATTRS{idProduct}=="0030", TAG+="uaccess", ENV{ID_MM_DEVICE_IGNORE}="1"
ATTRS{idVendor}=="2464", ATTRS{idProduct}=="0031", TAG+="uaccess", ENV{ID_MM_DEVICE_IGNORE}="1"
# diamond/j49: OMAP boot ROM (and x-loader, the flasher), each application's gadget
ATTRS{idVendor}=="0451", ATTRS{idProduct}=="d00e", TAG+="uaccess", ENV{ID_MM_DEVICE_IGNORE}="1"
ATTRS{idVendor}=="2464", ATTRS{idProduct}=="0001", TAG+="uaccess", ENV{ID_MM_DEVICE_IGNORE}="1"
ATTRS{idVendor}=="2464", ATTRS{idProduct}=="0002", TAG+="uaccess", ENV{ID_MM_DEVICE_IGNORE}="1"
EOF
sudo udevadm control --reload-rules && sudo udevadm trigger
```

Reconnect the unit afterwards if it is already plugged in.

| Dependency | Why |
|-|-|
| A Wayland or X11 desktop with OpenGL or Vulkan drivers (Arch: `mesa`, or your GPU's driver) | Draws the window. |
| `xdg-desktop-portal` and the backend for your desktop (Arch: `xdg-desktop-portal-gtk`, `-kde` or `-gnome`) | The file picker for the MQTT CA certificate. |
| `libudev` (part of systemd) | USB access. |

</details>

## Usage
**App:** open it, tick the integrations, plug in the thermostat and follow the
prompts. Its mode menu also backs up and restores the whole NAND.

**CLI (Linux and macOS):** the same executable, run with arguments, works from a
terminal, over SSH or headless, with a few options the app doesn't have
(`--doom`, `--dry-run`, `--backup`, `verify`). On macOS it is
`unfr0zen.app/Contents/MacOS/unfr0zen`. Windows has the app only.
```
unfr0zen [OPTIONS] [COMMAND]

Commands:
  backup    Back up the whole NAND to a file, checked with a second read
  verify    Compare a NAND backup with the NAND
  restore   Write a verified NAND backup back to the NAND
  boot      Boot the application firmware (3rd generation)
  uboot     Load recovery U-Boot over USB and leave it at its prompt (3rd generation)
  nlclient  Patch a local stock nlclient, or restore it from its undo journal

Common options:
  --device <GEN>         diamond, j49 or diamond3 instead of the USB identity
  --backup              Save the original root volume before patching
  --dry-run             List the changes, write nothing
  --mqtt <URL>          Relay to this MQTT broker
  --matter              Serve the unit over Matter (default code 0347-100-5534)
  --matter-thread       Serve Matter over Thread too (excludes --zigbee)
  --zigbee              Join a Zigbee network as a router
  --doom                Install Doom, launched from UNFR0ZEN → Doom
  --ssh                 Run an SSH server on the unit
  --block-cloud         Cut the unit off from Nest's and Google's servers
```
Run `unfr0zen --help` for every option.

Setting up MQTT, Matter, Thread, Zigbee and Doom, with the app's and the CLI's
options side by side: [unfr0zen.com/integrations](https://unfr0zen.com/integrations/).

## Building
See [BUILDING.md](BUILDING.md).

## Disclaimers
unfr0zen is provided **as is, without any warranty**. We are **NOT** responsible
for any data loss, a bricked thermostat, damage to your heating or cooling
system, or anything else that happens to your device. By using unfr0zen you
accept full responsibility for the result.
- Back up first: the app's Back up NAND (or `unfr0zen backup`) saves the whole NAND, Restore NAND (`unfr0zen restore`) puts it back.
- Modifying the thermostat voids its warranty and may break Google's services for it.
- Doom takes over the UI: while it runs the thermostat does not control the heating or cooling.
- Matter uses the test vendor ID (0xFFF1) and test attestation; controllers treat the unit as uncertified.
- unfr0zen is not affiliated with, endorsed or sponsored by Google LLC or Nest Labs.
  Nest and Nest Learning Thermostat are trademarks of Google LLC.

## Troubleshooting
When opening an issue, include:
- How you ran it (app or CLI, with the options used) and the OS of your computer
- The full log of the run
- What the thermostat screen showed

Create an issue here: https://github.com/unfr0zen/unfr0zen/issues/new

## License
unfr0zen is licensed under the [GNU General Public License v3.0](LICENSE).
Bundled third-party components keep their own licenses: U-Boot and its patches
(GPL-2.0+), busybox (GPL-2.0), doomgeneric (GPL-2.0), the `zbwake` kernel
module (GPL-2.0). DOOM1.WAD is id Software's shareware episode, redistributed
under its shareware terms.

## Credits
- [U-Boot (NXP i.MX)](https://github.com/nxp-imx/uboot-imx)
- [BusyBox](https://busybox.net)
- [rs-matter](https://github.com/project-chip/rs-matter)
- [OpenThread](https://github.com/openthread/openthread)
- [Silicon Labs Gecko SDK / EmberZNet](https://github.com/SiliconLabs/gecko_sdk)
- [doomgeneric](https://github.com/ozkl/doomgeneric) and id Software's DOOM
