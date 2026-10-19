# ms-teams

A native Microsoft Teams client for Linux in Rust + GPUI, for work/school accounts. Neither official client has native
UI code to port, so this one is written against Teams' internal APIs, with the web client as the reference.

Status: M0, the protocol lab and risk spikes (`docs/plans/20261019-native-teams-m0-lab-and-spikes.md`). Hard rules for
contributors and agents are in [CLAUDE.md](CLAUDE.md).

## Layout

| path | contents |
|---|---|
| `crates/teams-fence` | the write fence: a write is allowed only within the 3 lab accounts; the lab config (`lab.toml`) |
| `crates/fence-seal` | compile checks without `test-support`: no other crate can mint a `WritePermit`, and a lab binary has a membership source (`FailClosed`) |
| `crates/teams-auth` | PKCE code exchange, token refresh and storage, skypetoken |
| `crates/teams-proto` | Teams internal API requests; mutating ones need a `WritePermit` |
| `crates/proto-lab` | the lab harness CLI |
| `crates/fixture-guard` | scans `tests/fixtures/` and `docs/` for tokens, emails and real MRIs |
| `spikes/gpui-video` | GPUI spike: the Wayland smoke window (M0 Task 15), then video rendering |
| `tests/fixtures/` | sanitized captures |
| `docs/protocol/`, `docs/spikes/` | protocol notes and spike write-ups |
| `tools/` | interim Node tools (lab sign-in, Conditional Access probe) |

## Test

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/lint-report.sh
cargo test --workspace
```

The browser tests run headless Chrome for Testing on throwaway profiles. Install the pinned build once with
`cargo run -p proto-lab -- chrome install`, or point `PROTO_LAB_CHROME` at a Chrome binary; without one they're skipped,
unless `PROTO_LAB_REQUIRE_CHROME=1`. The build goes into the lab's `chrome/` dir: under `data_dir` when
`~/.config/teams-lab/lab.toml` sets one, else `~/.local/share/teams-lab/chrome/`. No lab config is needed for this, but
an invalid one is refused; the tests look in the same place.

The GPUI smoke tests (`spikes/gpui-video/tests/smoke.rs`) start their own headless weston with a niri nested in it and
never touch the desktop session. They need weston 15.0 or later (for `--fake-seat`) and niri on `PATH`; without them
they're skipped, unless `GPUI_SMOKE_REQUIRE=1`. They run the window once on every GPU, chosen with `ZED_DEVICE_ID`.

## Captures and fixtures

A recorded run writes `captures/<scenario>/<ts>/events.jsonl` under the lab's data dir, never into the repo (the format
is described in `crates/proto-lab/src/capture.rs`). `cargo run -p proto-lab -- export <capture> <fixture-name>`
sanitizes one into the repository's `tests/fixtures/<fixture-name>.jsonl` (or one JSON array for a `.json` name; the
workspace root is found from the current directory, even inside a crate, or `--fixtures-dir` names another) and writes
nothing unless the fixture guard passes the result.

## Test-only builds

The tests enable `teams-fence`'s `test-support` feature through dev-dependencies, and cargo unifies it into everything
built together with them (`cargo test`, `--all-targets`, `--all-features`), the `proto-lab` binary included. Such a
binary refuses to run; `cargo build -p proto-lab` or `cargo run -p proto-lab` builds one without it.
