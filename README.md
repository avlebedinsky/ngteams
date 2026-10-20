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
| `crates/fixture-guard` | scans `tests/fixtures/` and `docs/` for tokens, signatures, emails, real MRIs and GUIDs |
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
is described in `crates/proto-lab/src/capture.rs`). Captures never enter the repo, not even sanitized.

`cargo run -p proto-lab -- export <capture> <name>` sanitizes one into a local **reference copy**,
`reference/<name>.jsonl` under the lab's data dir (`~/.local/share/teams-lab/reference/` by default; one JSON array for
a `.json` name; the file `0600`, its directories `0700`). It refuses any destination inside a git work tree. It writes
nothing unless no word (3 characters or more) of a name found in the capture is left in the result, at any layer of
encoding, and the fixture guard passes it. A word of a name that is also an ordinary word left anywhere refuses the
export too; a name never seen under a name field, or a shorter word, is outside that check. A word of a name that is
also a label of a host (other than a public Microsoft protocol host such as `teams.microsoft.com`, which is never taken
for a name) refuses it as well, rather than writing the host destroyed. Teams meeting codes and join passcodes (of
letters only too) become `<meeting:N>` and `<token:N>` wherever they show, invitations read in every language the web
client has locale strings for, and the fixture guard refuses a join link, a meeting id or a passcode left in the clear.
The sanitizer is defence in depth, with known gaps (`docs/backlog/sanitizer-defence-in-depth-known-gaps.md`).

Committed fixtures in `tests/fixtures/` are written **synthetic-from-capture**: an agent reads the reference copy
locally and writes a fixture of the same shape with made-up values. Every fixture declares itself synthetic: a JSON
file has a top-level `"_synthetic"` note (what it is shaped like, that the values are made up), a JSONL file has one
on its first line (a header line of its own, or on every line), and any other text file starts with a `_synthetic: …`
line in its own comment syntax. `cargo test -p fixture-guard` fails on an unmarked fixture
(`committed_fixtures_are_marked_synthetic`) and on anything the guard finds in `tests/fixtures/` or `docs/`
(`committed_fixtures_and_docs_are_clean`). Crate-local test inputs (`crates/*/tests/fixtures/`) are not fixtures.

## Test-only builds

The tests enable `teams-fence`'s `test-support` feature through dev-dependencies, and cargo unifies it into everything
built together with them (`cargo test`, `--all-targets`, `--all-features`), the `proto-lab` binary included. Such a
binary refuses to run; `cargo build -p proto-lab` or `cargo run -p proto-lab` builds one without it.
