# Contributing

Chat is on [Discord](https://amazynk.com/discord/). Bugs go to
[issues](https://github.com/amazynk/stockroom/issues/new/choose), and security problems go
privately, as [SECURITY.md](SECURITY.md) describes. Everyone taking part is expected to
follow the [code of conduct](CODE_OF_CONDUCT.md).

## Before you send a change

[README.md](README.md) says how Stockroom installs and where each part lives, and
[PLAN.md](PLAN.md) has the goals, the milestones and what is not done. These have to pass,
as they do in CI:

```sh
cargo fmt --all -- --check
cargo lint                 # clippy over the workspace, warnings as errors
cargo test --workspace
```

- No `unsafe` in handwritten code. The one crate that allows it, `suite-ui-markup`, holds
  only the code Slint generates.
- Everything that touches the system (the registry, `hdiutil`, `schtasks`, `launchctl`,
  paths) goes through the traits in `suite-platform`, so that it can be tested on any system
  against the stand-ins in `suite-testkit`. A change to an installer path needs a test there.
- Tests never touch the network or the real home folder: they use the fake release server
  and `--root`.
- If `/tmp` is mounted `noexec`, set `TMPDIR=$PWD/target/tmp`: the tests run the apps they
  install.
- New or updated dependencies have to pass `cargo deny check`.
- After changing a string in the window, run `python3 crates/suite-ui-markup/tools/extract.py`
  (see [TRANSLATING.md](crates/suite-ui-markup/TRANSLATING.md)).

## An app's entry in the list

An app is one file in [`catalog/apps`](catalog/apps), with its icon beside it. The README's
"Catalog and publishing" section describes the fields and where releases come from. Only
apps whose source is public are listed.

## Commits

One commit does one thing. The subject is short and imperative, and the body says what
changed in behaviour and how it was tested.
