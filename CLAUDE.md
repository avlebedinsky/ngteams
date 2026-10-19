# ms-teams

A native Microsoft Teams client for Linux in Rust + GPUI, written against Teams' internal APIs with the web client as
the reference. The current milestone is M0: a protocol lab with a hard write fence, and risk spikes
(`docs/plans/20261019-native-teams-m0-lab-and-spikes.md`). Commands and layout are in [README.md](README.md).

## Hard rules

These hold for every agent and every change, with no exceptions.

- **Writes go only through the fence, and only within the 3 lab accounts.** A write is anything others could notice:
  messages, edits, deletes, reactions, typing, read receipts, calls and answering a call, invites, meetings, member
  changes, chat creation, channel posts, bot replies. It is allowed only when every participant of its target, fetched
  fresh, is a lab account or an approved service participant. Anything unknown, partial or paginated is denied.
- **Nothing runs live against the tenant before the browser fence is armed** (M0 Tasks 6–7): HTTP interception on
  every host and target, the WebSocket guard, the service worker neutralized. Until the write-target mapping exists
  (M0 Task 11) the fence runs fail-closed: every mutating request is denied except the seeded own-state list.
- **Raw captures and tokens never enter the repo.** Captures live under `~/.local/share/teams-lab/captures/`;
  fixtures are committed only after sanitizing, and the fixture guard (`crates/fixture-guard`) must pass on
  `tests/fixtures/` and `docs/`. Tokens never reach logs or stdout: print only aud, exp, scopes and tenant.
- **Agents never build or run a personal build and never receive the user's tokens.** Lab binaries get a
  `WritePermit` only from `teams-fence`: their fence comes from `Fence::open_lab` (roster from `lab.toml`, the lab's
  `fence.log`), and `teams-fence`'s `test-support` feature (any roster, any log, fake membership sources) is enabled
  only under `[dev-dependencies]`, never as a normal dependency.
- **Protocol notes are written in our own words** in `docs/protocol/`. Microsoft JS is never copied into the repo.

## Lint rules

Same as agterm-rs:

- A function over a complexity limit (`clippy.toml`) is split, not excused. The only exception is an outer
  `#[expect(clippy::..., reason = "...")]` on that one item.
- `#[allow]` and inner `#![allow]`/`#![expect]` are refused (`scripts/lint-report.sh`).
- `clippy.toml` thresholds are lowered, never raised.
- A `rust-toolchain.toml` bump is a change of its own.
- Every crate, spikes included, has `[lints] workspace = true` and is listed in the root `members`; shared dependency
  versions live in `[workspace.dependencies]`.

## Gate

Every change passes all four:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/lint-report.sh
cargo test --workspace
```
