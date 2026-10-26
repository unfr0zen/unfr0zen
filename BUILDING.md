# Building unfr0zen

The first firmware build needs internet access: it downloads busybox, Nest's
kernel and x-loader sources, the Silicon Labs SDK and OpenThread at pinned
versions.

| Device | How it is patched |
|-|-|
| `diamond3` (gen 3) | Recovery U-Boot over the ROM's serial download, then the slot's own kernel with an initramfs patches its UBIFS root. |
| `j49` (gen 2) | Nest's own x-loader over the ROM's USB boot loads a NAND flasher; the host appends the changes to the slot's JFFS2 root in its erased blocks. |
| `diamond` (gen 1, experimental) | Diamond's x-loader uses the same OMAP USB/NAND flasher and JFFS2 path; a stock Gen 1 NAND and physical unit are still needed for validation. |

## Dependencies

### macOS

```sh
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Plus Docker, any of:
- [Docker Desktop](https://docs.docker.com/desktop/setup/install/mac-install/)
- Colima: `brew install colima docker && colima start --memory 8`
- Podman: `brew install podman && podman machine init --memory 8192 && podman machine start`, then put a `docker` → `podman` symlink on your `PATH`

Give the Docker VM at least 8 GiB of memory. With 2 GiB the nestlocal build is killed.

### Linux

```sh
# Debian / Ubuntu
sudo apt install build-essential pkg-config git curl bc bison flex libssl-dev \
    gcc-arm-none-eabi binutils-arm-none-eabi libnewlib-arm-none-eabi \
    libstdc++-arm-none-eabi-dev cmake ninja-build python3-pil libudev-dev clang
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# zig 0.16.0: unpack https://ziglang.org/download/ and put zig on your PATH

# Arch
sudo pacman -S --needed base-devel pkgconf git curl bc bison flex openssl \
    arm-none-eabi-gcc arm-none-eabi-binutils arm-none-eabi-newlib cmake ninja \
    python-pillow rustup zig clang
rustup default stable

# All distributions
cargo install --locked cargo-zigbuild@0.23.4
```

Plus [Docker](https://docs.docker.com/engine/install/) for the 1st and 2nd
generations' x-loaders and the 2nd generation's USB-serial kernel. Without
their x-loaders the app cannot reach those boards.

### Windows

- [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/), workload **Desktop development with C++**
- [Rust](https://rustup.rs) (`rustup-init.exe`, default MSVC toolchain)
- [Git for Windows](https://git-scm.com/download/win)

The firmware can't be built on Windows: build it on macOS or Linux and copy
`build/` into the repository.

## Build

Firmware, nestlocal and Doom (macOS / Linux), into `build/`:

```sh
git submodule update --init
scripts/build-firmware.sh
```

The app, which embeds `build/`:

```sh
cargo build --release -p unfr0zen-gui
```

The executable is `target/release/unfr0zen` (`unfr0zen.exe` on Windows). On
Linux and macOS, run with arguments, it is the command-line tool.

The version comes from `git describe --tag --always --dirty`.

### Packaging

- macOS universal app bundle: `sh scripts/make_macos_app.sh` → `dist/unfr0zen.app`
- Linux desktop entry and icon: `sh scripts/install_linux.sh`
