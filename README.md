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

The tests enable `teams-fence`'s `test-support` feature through dev-dependencies, and cargo unifies it into everything
built together with them (`cargo test`, `--all-targets`, `--all-features`), the `proto-lab` binary included. Such a
binary refuses to run; `cargo build -p proto-lab` or `cargo run -p proto-lab` builds one without it.
