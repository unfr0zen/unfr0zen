# Repository Guidelines

## Project Structure & Module Organization

This Rust 2021 workspace builds USB recovery tools for Nest thermostats:

- `crates/unfr0zen-core`: firmware parsing, patching, and recovery logic.
- `crates/unfr0zen-host`: USB workflows, embedded assets, and host orchestration.
- Device-specific code sits in one module per device (`diamond3`, `j49`) in core and host; `unfr0zen-host::device` identifies the unit on USB by its identity in any mode and the run dispatches to its device's flow.
- `crates/unfr0zen-gui` and `crates/unfr0zen-cli`: the `unfr0zen` executable (egui window) and the terminal interface, a library it runs when given arguments on Linux and macOS; Windows has the window only.
- `crates/nestlocal`: device service implementing MQTT, Matter, and Zigbee; startup scripts live in `rootfs/`.
- `platform/`: what is built for and installed on the thermostat, by device:
  - `common/`: both generations. `nlclient/` (native settings screen, assets, binary discovery references, C tests), `radio/` (radio firmware, each radio's board, linker and OpenThread files in `em3581/` and `em357/`; OpenThread integration in `thread/`), `doom/` (doomgeneric submodule plus its platform layer).
  - `diamond3/`: the gen 3. `bootstrap/` (ROM stage and boot wrapper), `uboot/` (recovery U-Boot submodule, patches and build scripts), `initramfs-init`, `zbwake/` (kernel wake module for each gen 3 release).
  - `j49/`: the gen 2. x-loader build and bare-metal NAND flasher, loaded through x-loader's USB loader, its USB-serial kernel, `zbwake/` module, and dropbear for `--ssh`.
- `scripts/`: `build-firmware.sh` builds all of `platform/` into `build/`; `make_macos_app.sh` and `install_linux.sh` package the app.
- `assets/`: app icons, the Linux `.desktop` entry, and the README screenshot.
- `integrations/`: files users install on their side: Home Assistant blueprints (`home-assistant/`) and the Zigbee2MQTT converter (`zigbee2mqtt/`).
- `client-ng/`: `client-ng`, a replacement for stock nlclient (Rust engine, Lua skins, GLES2), developed on nest-qemu; a submodule (the private `unfr0zen/client-ng`), still a member of this Cargo workspace. Commit its changes in the submodule, then the new pointer here. `client-ng/README.md` describes it and `client-ng/TESTING.md` the build, emulator and stock-comparison workflow.

Generated outputs belong in ignored `build/`, `target/`, `dist/`, or `.work/` directories.

User documentation for integrations and options (app and CLI) is the website's `integrations/index.html` (repository `unfr0zen/unfr0zen.com`); update it when an option, its app label, or integration behavior changes.

## Build, Test, and Development Commands

Run commands from the repository root:

```sh
git submodule update --init
scripts/build-firmware.sh
cargo build --release -p unfr0zen-gui
cargo run -p unfr0zen-gui -- --help
cargo test --workspace --locked
sh scripts/make_macos_app.sh
```

The client builds separately for the unit (`client-ng/build.sh`) and runs on nest-qemu through `client-ng/tools/nest.py`; see `client-ng/TESTING.md`.

The firmware build supplies assets embedded by the host crates; build it before workspace tests or application builds. macOS uses Docker for firmware compilation. See `BUILDING.md` for platform dependencies. The packaging command produces `dist/unfr0zen.app`.

## Coding Style & Naming Conventions

Match surrounding style and use four-space indentation in Rust. Use `snake_case` for functions/modules, `PascalCase` for types, and `SCREAMING_SNAKE_CASE` for constants. Keep changes focused; avoid unrelated formatting or refactoring. Comments explain current behavior and lasting constraints, not debugging history.

## Testing Guidelines

Rust tests use `#[test]` and integration tests under `crates/unfr0zen-core/tests/`. Name tests after observable behavior. CI also runs:

```sh
clang -std=c11 -Wall -Wextra -Werror platform/common/nlclient/tests/custom_flow_test.c -o /tmp/custom-flow-test
/tmp/custom-flow-test
python3 -m unittest discover -s platform/common/radio/tests -v
cargo run --locked -q -p client-ng -- --check --skins client-ng/skins --menus client-ng/config/menus.lua
cargo clippy --workspace --locked --all-targets -- -D warnings
```

For binary patch changes, follow `platform/common/nlclient/tests/README.md`; strict golden tests require private firmware samples. Report hardware verification separately from host tests. No numeric coverage threshold is specified.

## Commits and PR titles

    subsys: lowercase imperative summary

Everything after the colon is **lowercase**. The subject fits in 72 columns, no trailing period. The subsystem prefix matches the area the change touches — check `git log --format=%s -20 -- <path>` for precedent. Common prefixes:

| Prefix | Area |
|-|-|
| `app:` | the unfr0zen application (GUI, CLI, self-update, host library) |
| `nestlocal:` | the on-device service; nest deeper with `nestlocal: thread:` |
| `client-ng:` | the thermostat UI submodule |
| `ci:` | GitHub Actions workflows and build infrastructure |
| `radio:` | radio firmware and Thread/Zigbee protocol |
| `j49:` | gen 2 device-specific code |
| `platform:` | build scripts, bootstrap, x-loader, kernel modules |
| `docs:` | documentation only |

Never `WIP`, `fix`, or bare verbs without a subsystem prefix.

Body is optional — omit it for self-explanatory changes. When present, wrap at 72 columns, at most three lines: why the change is needed and any non-obvious choice.

- One logical change per commit, and every commit builds and passes its tests on its own. Fold review fixes into the commit that owns the lines instead of adding fixup commits; without an interactive terminal: `git commit --fixup=<sha>`, then `GIT_SEQUENCE_EDITOR=true GIT_EDITOR=true git rebase -i --autosquash origin/main`.
- Update a branch with `git rebase origin/main`, never by merging `main` into it.
- Stage explicit paths (`git add <file>`) rather than `git add -A` or `commit -a`, which pick up stray files. Before pushing, review `git show --stat origin/main..HEAD`.
- Branches: `feature/<short-name>` for features, `bugfix/<short-name>` for fixes.

## Pull requests

- A PR is one self-contained change. PRs are squash-merged: the PR title becomes the commit subject on `main` (with ` (#N)` appended), so it must follow the `subsys: lowercase summary` format above.
- PR body is short — one or two sentences describing the change. No padding.
- CI (`.github/workflows/build.yml`) builds the firmware and every app target and runs the tests above; a PR merges only when it is green.
- Never describe an undisclosed vulnerability in a public issue, PR or commit.

## Security & Device Configuration

Do not commit firmware dumps, credentials, pairing keys, or private device logs. Preserve saved network credentials during device testing. Thread and Zigbee require exclusive ownership of the onboard radio; document the final device configuration after hardware tests.
