# Contributing to unfr0zen

Thanks for helping. This page covers how to propose a change, test it and get
it merged, then how to report a problem. The repository's conventions in full
are in [AGENTS.md](AGENTS.md); building is in [BUILDING.md](BUILDING.md).

## Contributing

A pull request is one small, self-contained change. For anything large, open
an issue first so the approach can be agreed before you write it.

### Every protocol

nestlocal serves the same thermostat over MQTT, Zigbee and Matter, and each
should be able to do what the others do. To keep them from drifting apart, a
feature is accepted only when it works over all three, unless one of them
cannot carry it or doing so would add disproportionate complexity. Say which,
and why, in the pull request.

### Every generation

A feature is expected on every generation unfr0zen supports, for the same
reason. A generation may be left out only when its hardware or firmware
cannot support the feature, or supporting it would add disproportionate
complexity. Name each one left out, and why, in the pull request.

### Setting up

```sh
git submodule update --init
utils/build-firmware.sh
cargo build --release -p unfr0zen-gui
pre-commit install
```

[BUILDING.md](BUILDING.md) lists the dependencies for each platform; macOS
builds the firmware in Docker.

### Branches and commits

- Branch from `main`: `feature/<short-name>` for a feature,
  `bugfix/<short-name>` for a fix.
- Each commit is one logical change, and builds and passes its tests on its
  own.
- Subjects are `subsys: lowercase imperative summary`, at most 72 columns, no
  trailing period: `nestlocal: wake the screen from home assistant`. The
  prefix names the area changed (`app:`, `nestlocal:`, `j49:`, `platform:`,
  `docs:`, ...; the table is in [AGENTS.md](AGENTS.md#commits-and-pr-titles),
  and `git log --format=%s -20 -- <path>` shows precedent).
- A body is optional: at most three lines, wrapped at 72 columns, saying why
  the change is needed.
- Update your branch with `git rebase origin/main`, never by merging `main`
  into it. Fold review fixes into the commit they belong to rather than adding
  fixup commits.
- Stage explicit paths (`git add <file>`), not `git add -A`.
- Comments and docs describe the code as it is, not how it got there.
- Update [docs/integrations/index.html](docs/integrations/index.html) when an
  option, its app label or integration behavior changes.
- Never commit firmware dumps, credentials, pairing keys or private device
  logs.

### The pull request

The title follows the commit subject format: pull requests are squash-merged
and the title becomes the commit subject on `main`. Before opening it, run the
checks under [Testing](#testing) and review your branch with
`git show --stat origin/main..HEAD`. Then fill in every section of the
template:

- **Description:** one or two sentences on what changes and why, linking the
  issue it closes.
- **Testing:** how you verified it (see [Testing](#testing)).
- **Artificial intelligence:** whether an AI coding tool was used, and if so
  the tool, model and the parts it produced.
- **Protocols:** that the change covers MQTT, Zigbee and Matter (see
  [Every protocol](#every-protocol)), with
  [docs/protocols/index.html](docs/protocols/index.html) updated, or the
  justification for each one left out.
- **Generations:** which generations the change covers (see
  [Every generation](#every-generation)), and the justification for each one
  left out.

CI runs the same checks and builds every firmware module and the app for
Linux, Windows and macOS. A pull request merges only when it is green.

## Testing

### On the host

Run these before opening a pull request; CI runs them too:

```sh
cargo test --workspace --locked
cargo clippy --workspace --locked --all-targets -- -D warnings
clang -std=c11 -Wall -Wextra -Werror platform/common/nlclient/tests/custom_flow_test.c -o /tmp/custom-flow-test
/tmp/custom-flow-test
python3 -m pytest platform/common/radio/tests
pre-commit run --all-files
```

- Add tests for observable behavior, including edge cases, named after what
  they check. Rust tests use `#[test]`; integration tests live in
  `crates/unfr0zen-core/tests/`.
- A change to a binary patch follows
  [platform/common/nlclient/tests/README.md](platform/common/nlclient/tests/README.md);
  its strict golden tests need private firmware samples.

### On a thermostat

Say in the pull request which units you tested on: generation and Nest
firmware version, or QEMU. Report this separately from the host tests, and
say plainly when a change has not been tried on hardware.

- Keep the unit's saved network credentials while testing.
- Thread and Zigbee each need the onboard radio to themselves: say which one
  the unit ran, and the configuration it was left in.

## On the matter of artificial intelligence

You are most welcome to have an AI proofread an issue, or help with a pull
request, provided you understand every word and every line of what you submit.
The issue forms ask you to agree to this, and the pull request template asks
whether an AI coding tool was used, which one, and for which parts. Issues
that fall short of this rather modest standard are closed without notice.

## Issues

Blank issues are disabled: pick the form that fits.

| Form        | For                                             |
| ----------- | ----------------------------------------------- |
| Bug Report  | Something unexpected happened                   |
| Enhancement | A change, update or addition to a feature       |
| Feature     | A new idea, or functionality that doesn't exist |

For a bug, say which part is affected (recovery and flashing, the app,
nestlocal over MQTT, Matter or Zigbee, or the thermostat UI), the steps that
reproduce it, and, for a regression, the last version that worked. Attach the
`unfr0zen.log` of the failing run: without it, a report is mostly guesswork.
Each run writes one to `logs/<timestamp>/unfr0zen.log` in:

- macOS: `~/Library/Application Support/unfr0zen`
- Linux: `~/.local/share/unfr0zen` (or `$XDG_DATA_HOME/unfr0zen`)
- Windows: `%APPDATA%\unfr0zen`

Check that logs and screenshots carry no credentials, pairing keys or network
passwords before you attach them.

## Security vulnerabilities

Report them privately through a
[security advisory](https://github.com/unfr0zen/unfr0zen/security/advisories/new),
never in a public issue, pull request or commit.

## Questions

Ask on [Discord](https://discord.gg/fBqjAFARYg). Issues are for bugs,
enhancements and features.

## License

unfr0zen is licensed under the [GNU General Public License v3.0](LICENSE). By
contributing, you agree that your contributions are licensed under it too.
