<!--
Title: `subsys: lowercase imperative summary` (see AGENTS.md). PRs are
squash-merged, so the title becomes the commit subject on main.
-->

## Description
<!-- One or two sentences: what this changes and why. Link the issue it closes. -->


## Testing
<!-- How you verified it: host tests, QEMU, which real units. Report hardware
verification separately from host tests. -->


## Artificial intelligence
<!-- Tick exactly one. If an AI coding tool wrote or changed any code, tests or
docs in this PR, name the tool and model and say which parts it produced. -->
- [ ] No AI coding tool was used for this PR.
- [ ] An AI coding tool was used. Tool and model: <!-- e.g. Claude Code, Opus 5.5 --> Parts it produced: <!-- files or areas -->

- [ ] I understand every line of this PR and can explain it in review.

## Protocols
<!-- Tick one. nestlocal exposes the same device over MQTT, Zigbee and Matter;
a change to one is expected in all three, plus docs/protocols/index.html. -->
- [ ] This PR does not touch MQTT, Zigbee or Matter.
- [ ] This PR covers MQTT, Zigbee and Matter, and updates `docs/protocols/index.html`.
- [ ] This PR covers only some of them. Justification for each one left out:
<!-- e.g. Matter: no cluster maps to this attribute -->

## Generations
<!-- Tick the generations this PR was considered for, and justify any left out. -->
- [ ] Gen 1 (`diamond`)
- [ ] Gen 2 (`j49`)
- [ ] Gen 3 (`diamond3`)
- [ ] Every generation this does not apply to is named below with the reason:
<!-- e.g. gen 3: the change is in the j49 NAND flasher -->

## Checklist
- [ ] My pull request is small.
- [ ] My pull request is one self-contained change, and each commit builds and passes its tests.
- [ ] I've added tests for observable behavior, including edge cases.
- [ ] `cargo test --workspace --locked` and `cargo clippy --workspace --locked --all-targets -- -D warnings` pass, with no new warnings.
- [ ] `docs/integrations/index.html` is updated if an option, its app label or integration behavior changed.
- [ ] No firmware dumps, credentials, pairing keys or private device logs are committed.
- [ ] I've reviewed my own branch (`git show --stat origin/main..HEAD`) before opening the PR.
