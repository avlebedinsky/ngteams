# ngteams

A native Microsoft Teams client for Linux in Rust + GPUI, written against Teams' internal APIs with the web client as
the reference. Two builds: the lab binaries, tested in a lab of three dedicated accounts, and the personal build
(`ngteams`), the user's own Teams. Layout, commands, each binary and the tests: [README.md](README.md).

## Hard rules

These protect real people, the user's account and the user's data. They hold for every agent and every change.

- **Lab accounts contact only each other.** From the lab browser or a lab binary, a write that reaches people (a
  message, reaction, call, chat created, member added or removed, invite, meeting, mail, share) goes only to a target
  whose members, read fresh and in full, are lab accounts or the echo service. When unsure whether a request reaches
  people, treat it as contacting. Everything else is allowed without asking: reads, the account's own session and
  settings, presence, status, typing, read receipts.
- **Calls go only to the lab, both ways**: lab accounts, the echo service, and the user's own accounts named as call
  guests in `lab.toml` (calls only, never a message or invite). Call writes are mapped only from captures.
- The full lab rule, which the fence implements, is [docs/lab-rules.md](docs/lab-rules.md); read it before changing
  fence, write or call code. A new request kind that passes its test goes ahead; changing a test, or widening whom a
  lab request may reach, needs the user's sign-off.
- **Arm the browser fence before anything runs live against the tenant.**
- **No write ever comes from a `cargo test` process**: a live test spawns a normally built lab binary.
- **Captures, tokens, debug logs and `fence.log` never enter the repo**, not even sanitized. Committed fixtures are
  synthetic and marked ([README](README.md#captures-and-fixtures)). Tokens never reach logs or stdout: print only aud,
  exp, scopes and tenant.
- **No token or credential goes to a third party** (a bot, an app's backend).
- **Agents run the personal build only read-only, and install it only when the user asks**
  ([details](docs/personal-build.md#what-agents-may-do-with-the-personal-build)):
  - start it only through `scripts/run-personal-read-only.sh`; its data dirs (`~/.local/share/ngteams/`,
    `~/.config/ngteams/`) are readable any time;
  - run `scripts/install-personal.sh` only on the user's explicit request, each time, never `cargo build -p ngteams`;
    afterwards start the installed `ngteams` detached if asked, and leave it alone;
  - never sign it in, drive, test or capture with it in sending mode, touch its keyring items or receive the user's
    tokens.
- **Every lab or test GUI runs in `crates/ui-sandbox`**, never on the user's display, bus or PipeWire.
- **Protocol notes are written in our own words** in `docs/protocol/`; Microsoft JS never enters the repo.

## The personal build

- **The personal build (`ngteams`) is the user's Teams** ([docs/personal-build.md](docs/personal-build.md)): it may do
  whatever the Teams client lets its user do, to any target. Its fence logs every send before it goes and denies none.
  Design its features as any desktop Teams client would; the lab rules aren't a design question there.
- Lab and personal stay apart: each refuses the other's data, keyring items and accounts, and a lab binary refuses to
  run with the `personal` or `test-support` feature built in.
- Mention a lab constraint only when the change touches lab code, in one sentence.

## Checks

CI requires 90% coverage per runtime crate ([CPU runner and hardware-test exception](docs/ci.md)). Choose additional
checks, waits and review each change needs from what it can break, and stop once they cover that. A small change
gets a small, fast check (the test that exercises it, or `cargo check` of
its crate), not every suite of every crate it touched; a change to shared API, the fence, the write path or calls
earns more. Rerun only what failed. Say what you ran and what you left out. Commands: [README.md](README.md#tests).

What the tooling enforces when it runs:

- clippy denies the workspace lints (`all`, `pedantic`, `unwrap`/`expect`/`panic`, the complexity limits in
  `clippy.toml`); `scripts/lint-report.sh` fails on an inner `#![allow]`/`#![expect]`.
- `cargo test -p fixture-guard` fails on an unmarked fixture or a secret in any `tests/fixtures/` or `docs/`.
- `scripts/coverage.sh --require-target` fails any runtime crate below 90% in lines, functions or regions; without
  that flag, the script checks the saved standalone floors in `coverage.toml`.
- A binary built with `test-support` (anything `cargo test`, `--all-targets` or `--all-features` built) refuses to run.

### Live runs

Live runs reach the real tenant, so these stay firm. Commands are in each binary's README section.

- Build the binary normally into `target/lab`, hold the live lock and an outer timeout.
- Live tests build only with `--features live` and run only with `PROTO_LAB_LIVE=1`. Lab browsers run headless and
  muted, one per lab profile at a time, none left running.
- A scenario clicks answer, decline or join only when made for it (`ab-call`, `abc-call`), only on its own call.
- **A fence breach is never a flake**: keep the run's output, check it and `fence.log` for anything sent unguarded,
  report it as breached, and report a later clean run beside it, not in its place. Don't raise a timeout or skip an
  arming step to make a run pass.

## Conventions

- Split a function over a `clippy.toml` limit; if it really can't be split, put an outer
  `#[expect(clippy::..., reason = "...")]` on that item. Thresholds only go down.
- Bump `rust-toolchain.toml` in a change of its own. Every crate (spikes too) has `[lints] workspace = true` and is in
  the root `members`; shared dependency versions live in `[workspace.dependencies]`.
- **Never edit a released store migration**: users' stores already ran it. Add a new one (`crates/teams-store`).
- Keep a value in a form no capture showed as unknown rather than guessing it (`teams_proto::chat::Field::Unknown`);
  a fixture covers a field's whole captured range.
- A new or changed endpoint updates its row in `docs/protocol/endpoints.md`.
- A new mapped contacting request gets a row in `crates/teams-fence/src/contacting*` and a case in
  `crates/teams-fence/tests/contacting.rs`. A writing scenario proves enforcement with a forced-deny probe first; a
  new deny class goes in `crates/proto-lab/src/scenario/expected.rs` with its reason.
- Check a new dependency that logs at `debug` or below against the compile-time log cap (`crates/teams-log`).

## Design invariants

Undoing one needs a new reason, not a cleanup; the why is in each path's module docs.

- Browser arming order and close-or-kill of an unarmable target: `crates/proto-lab/src/arming.rs`; the CDP client
  stays our own (`cdp.rs`).
- A contacting write goes only as `Authorized<_>` with a `WritePermit` from `Fence::check` for one exact target;
  `MembershipSource` and `Mutating` are sealed (`crates/fence-seal`). The transport is `https` only, no proxy or
  redirects, each token only to its own service, never resending a write (`docs/lab-rules.md`).
- The outbox is the only write path; an unknown outcome is checked, never blindly resent; nothing is resent across
  runs without `Store::resend` (`crates/teams-core/src/outbox.rs`).
- One engine per account (`crates/teams-core/src/engine.rs`); nothing else in the process opens the store file.
- Deliveries are never answered except a call's typed response (`FrameSink::respond`); only a `CallAction` answers,
  declines or joins a call, and no view does it, or turns a camera on, by itself.
- No log line carries a payload except the app's debug log; the logger redacts credentials (`crates/teams-log`).
- No view builds a request: views reach the engine only through `Core` (`crates/teams-ui/src/bridge.rs`).
- Message text is drawn only from sanitized `RichText`; the UI kit is held to `crates/teams-ui/tests/reach.rs`; no Zed
  code is copied (GPL).
- Call media is libwebrtc's (`crates/teams-media/src/rtc.rs`): relay-only ICE, SDP only in Teams' dialect.
- The sign-in window (`crates/teams-signin`) is Microsoft's page in a WPE WebKit view (`crates/teams-wpe`) on the
  system's `wpewebkit`, never vendored; every page request is decided and logged by its guard, and its only way out
  is the egress proxy.

## Where things live

- [README.md](README.md): layout, building, each binary's commands, the lab's files, captures, fixtures and tests.
- `docs/protocol/`: how each Teams service works, `endpoints.md`, `fence-targets.md`. `docs/lab-rules.md`: the lab
  rule in full. `docs/personal-build.md`: the personal build. `docs/backlog/`: deferred work. `docs/plans/`: plans as
  decided then, not the current design.
- Keep this file to rules and pointers: no dates, incidents or mechanism detail. A fix's why goes in a comment at the
  code, how a service works in `docs/protocol/`.
