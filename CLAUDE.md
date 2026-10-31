# ngteams

A native Microsoft Teams client for Linux in Rust + GPUI, written against Teams' internal APIs with the web client as
the reference and tested in a lab of three dedicated accounts. Next: M4, meetings and the calendar
(`docs/plans/20261026-native-teams-m4-meetings.md`). Commands and layout: [README.md](README.md).

## Hard rules

These hold for every agent and every change. **Lab accounts contact only each other (and call the user's own accounts
named as call guests); everything else is allowed.** The rules on writes, calls and the browser fence govern the lab
browser and every lab binary (HTTP and Trouter frames alike); the personal build (`ngteams`) is under its own two rules
below. The rules on sealed types, captures and tokens, agents and protocol notes hold for both builds.

- **A change that fits a rule's test goes ahead** without asking: a new read, own-state request or frame that passes
  its rule's test, or in the lab a new contacting write kind (a meeting's, say) mapped from a capture and let through
  only by the contacting write's test below; a rule's examples are today's, not a closed list. Only a change to a test
  itself, or in the lab widening whom a request may name or reach, needs the user's sign-off; the personal build has
  no such limit.
- **A contacting write goes only to the lab.** It is one that reaches people: a message (post, reply, edit, delete,
  forward, channel post, scheduled or drafted send), a reaction, a call (start, join, answer), a chat created, a member
  added or removed, an invite, a meeting, a mail, a share. It is allowed only when every participant of its target,
  read fresh and complete (no paging key), is one of the 3 lab accounts (`lab.toml`'s MRIs) or an approved service
  participant (the echo service, by MRI); for a call alone, a call guest too. A target whose members can't be read in
  full, or a write the fence can't tie to a target with certainty, is denied.
- **Everything else is allowed**, with no capture or review: reads by any method (a `POST` search or people lookup
  too), the account's own session, tokens and settings (out-of-office included), presence, a status note, typing and
  read receipts.
- **When unsure whether a request reaches people, treat it as contacting.** On a people-reaching service (chat,
  calling, Graph, Outlook, SharePoint, AMS, the middle tier's team, meeting and invite paths, CSA) a mutating request
  is contacting unless known not to be (typing, read receipts, own feed, settings, endpoints); unreadable is denied.
- **No token or credential goes to a third party** (a bot, an app's backend; a bot invoke stays denied). Sign-in and
  renewal stay in `teams-auth`'s own clients and touch only the account's own credentials.
- **Calls go only to the lab, both ways.** A call's participants are its caller, callees and whoever it adds; every
  call write is mapped only from a capture, in its captured shape.
  - **Call guests** (the user's own accounts in `lab.toml`'s `call_guests`, by MRI) are approved for calls only (start,
    join, answer, decline, `add`, and what a call sends once let in), in their 1:1 chat with a lab account or a chat of
    lab accounts plus them. A guest is never the caller, joiner, answerer or adder of a lab write, and nothing else of
    the lab reaches one (no message, reaction, chat created, member added or invite, even in a chat holding one).
  - Each entry is a fresh decision. A **start** passes when every callee is a lab account, call guest or the echo
    service and its chat holds only lab accounts and call guests by a fresh, complete read; it is tied to its chat by
    what it carries (the chat it names, or for a 1:1 start naming only its callee, the two accounts' 1:1 chat), never
    by client context; the test call's start, naming no chat, passes only in its captured shape. A **join** passes
    only in the chat it names when that chat's fresh, complete read lists the joiner and only lab accounts and call
    guests. An **`add`** is judged per invitee like a start.
  - An incoming invitation is **offered** only when readable, addressed to the account, and naming a caller and
    participants that are all lab accounts, call guests or the echo service. Only an offered invitation is answered or
    declined (a decline rejects the call as the web client does), tied by the call leg the answer names, until its
    ring ends. Every other incoming call is ignored: no answer, decline, join or scenario click.
  - Once an entry is let in, everything the call sends goes with no further decision, each send logged (follow-ups,
    `leave` and end, the callee's ring-time sends, Trouter delivery responses), still as sealed types in their captured
    shape, a delivery response only as a typed `OwnFrame` for its delivery. No transfer, forwarding or replacement is
    mapped; the roster isn't watched; media (TURN, ICE, DTLS-SRTP) isn't classified.
- **Nothing runs live against the tenant before the browser fence is armed**: HTTP interception on every host and
  target, the WebSocket guard, the service worker neutralized.
- **The native client sends a contacting write only as `Authorized<_>`** with a `WritePermit` from `Fence::check` for
  its exact target, logged to `fence.log`; only a sealed `Mutating` type in `teams-proto` builds one. A lab binary's
  transport refuses any request `teams_fence::contacting` doesn't call `None` (the personal build sends it, logged). The
  transport is `https` only, no proxy, no redirects, to its services' bases, each token only to its own service's host,
  and never resends a write (the HTTP client's retries stay off). A mock origin exists only under `test-support`, which
  a lab binary refuses. **No write ever comes from a `cargo test` process**: a live test spawns a normally built lab
  binary (`--target-dir target/lab`) and reads its output and `fence.log`. Both builds; only the fence policy differs.
- **Captures and tokens never enter the repo, not even sanitized.** Captures stay in
  `~/.local/share/teams-lab/captures/`; `proto-lab export` writes sanitized reference copies only to
  `~/.local/share/teams-lab/reference/`, never into a git work tree. A committed fixture is synthetic: a reference
  copy's shape with made-up values, marked (`"_synthetic"` top-level in JSON, first line of JSONL, a `_synthetic:`
  first-line header elsewhere). `crates/fixture-guard` must pass on every `tests/fixtures/` and `docs/`; every fixture
  carries the marker except `fixture_guard::synthetic::TEST_INPUTS`. The sanitizer is defence in depth, not
  permission. Debug logs and `fence.log` of either build are lab or user data like captures: never in the repo, not
  even sanitized. Tokens never reach logs or stdout: print only aud, exp, scopes and tenant. Both builds (the personal
  one makes no captures).
- **Lab binaries get a `WritePermit` only from the lab policy** (`Fence::open_lab`, `Fence::open_lab_account`: roster
  from `lab.toml`, the lab's `fence.log`); `teams-fence`'s `test-support` is only a `[dev-dependencies]` feature.
- **The personal build (`ngteams`) is the user's Teams** ([docs/personal-build.md](docs/personal-build.md)): the user's
  own accounts on their desktop, each with its own engine and fence; one binary, `ngteams` (its sign-in window included); data in
  `~/.local/share/ngteams/` and `~/.config/ngteams/`; Secret Service items under `ngteams`; no broker, no sandbox. It
  may do whatever the Teams client lets its user do, to any target. **Its fence logs every send and denies none** (the
  personal policy, behind the `personal` feature only `crates/ngteams` enables, opened only by `Fence::open_personal`,
  one per account): writes go through the outbox as sealed `Mutating` types under `Fence::check`'s permit, and every
  write, plain request, `POST` read and frame is logged before it goes (unloggable means unsent); an embedded engine
  with its own networking is logged where cheap, never a blocker. It sends what the user did or turned on and the
  follow-ups that needs, one call at a time across accounts; its sign-in window reaches only Entra's sign-in hosts and
  the identity provider hosts `accounts.toml` names. A lab binary refuses the personal data, keyring items and
  accounts and refuses to run with `personal` or `test-support` built in; `ngteams` refuses the lab's.
- **Agents run the personal build only read-only, and install it only when the user asks.**
  - Compile `ngteams` in the gate and test it on `teams-mock`; any other `ngteams` binary an agent
    leaves is a `test-support` one, which refuses to run.
  - Only on the user's explicit request, each time: `scripts/install-personal.sh` (never `cargo build -p ngteams`),
    then optionally start the installed `ngteams` detached, output discarded, and leave it alone.
  - Otherwise start it only through `scripts/run-personal-read-only.sh`; screenshot, navigate, read its logs and store.
    `~/.local/share/ngteams/` and `~/.config/ngteams/` are readable any time. Read-only mode sends nothing anyone sees
    or that changes the account; the app enforces it.
  - **Never sign it in, drive, test or capture with it in sending mode, touch its keyring items or receive the user's
    tokens.**
- **Protocol notes are written in our own words** in `docs/protocol/`. Microsoft JS is never copied into the repo.

## Weighing the rules

- The rules guard the lab. For the personal build the fence is an audit log, not a limit: design its features as any
  desktop Teams client would; never present the fence, `fence.log` or the lab rule as an obstacle or design question.
- Bring up a lab constraint only when the change touches lab code, in one sentence; answers about how something works
  or about personal-build features carry no fence caveats the question didn't ask about.

## Commands and gate

Every change passes all four, for the crates it touched (a file under the crate's directory changed):

```sh
cargo fmt --all --check
cargo clippy -p <crate> [-p <crate> ...] --all-targets --all-features -- -D warnings
scripts/lint-report.sh
cargo test -p <crate> [-p <crate> ...]
```

- A change to any `tests/fixtures/` or `docs/` also runs `cargo test -p fixture-guard`; `--workspace` only when asked.
- Live-run binaries: `cargo build [--release] -p <crate> --target-dir target/lab` (a `test-support` build refuses to
  run; `cargo test`, `--all-targets` and `--all-features` unify it in).
- Every live command holds the live lock and an outer timeout:
  `flock ~/.local/share/teams-lab/live.lock timeout --signal=INT --kill-after=60 600 scripts/native-call-run.sh …`.
- Live tests build only with `--features live` and run only with `PROTO_LAB_LIVE=1`. Lab browsers run headless and
  muted, one per lab profile at a time, none left running.
- A scenario clicks answer, decline or join only when made for it (`ab-call`, `abc-call`), only on its own call.
- **A fence breach is never a flake**: keep the run's output, check it and `fence.log` for anything sent unguarded,
  report it as breached, and any later clean run beside it, never in its place. Never raise a timeout or skip an
  arming step to make a test pass.

## Code conventions

- Split a function over a `clippy.toml` limit; the only exception is an outer `#[expect(clippy::..., reason = "...")]`
  on that item. `#[allow]` and inner `#![allow]`/`#![expect]` are refused. Thresholds are only lowered.
- A `rust-toolchain.toml` bump is a change of its own. Every crate (spikes too) has `[lints] workspace = true` and is
  in the root `members`; shared dependency versions live in `[workspace.dependencies]`. The one exception to the lints
  is generated gir bindings (`wpe-webkit-sys`, `wpe-platform-sys`, `wpe-javascriptcore-sys`), listed in
  `scripts/lint-report.sh`'s `EXEMPT_CRATES`; their safe wrapper (`teams-wpe`) is under every lint.
- Never edit a released store migration; add a new one (`crates/teams-store`).
- Keep a value in a form no capture showed, never guess it (`teams_proto::chat::Field::Unknown`); a fixture covers a
  field's whole captured range, not only its common form.
- A new or changed endpoint updates its row in `docs/protocol/endpoints.md` in the same change.
- A new mapped contacting request is a row in `crates/teams-fence/src/contacting*` plus a case in
  `crates/teams-fence/tests/contacting.rs`. A writing scenario first proves enforcement with a forced-deny probe; a new
  deny class goes on `crates/proto-lab/src/scenario/expected.rs` with its reason, never waved through.
- Check a new dependency that logs at `debug` or below against the log cap (`teams-log`, `teams-trouter`).

## Design invariants

Undoing one needs a new reason, not a cleanup; the why is in each path's module docs.

- Browser arming order, two-level `Fetch`, close-or-kill of an unarmable target: `crates/proto-lab/src/arming.rs`; the
  CDP client stays our own (`cdp.rs`).
- `MembershipSource` and `Mutating` are sealed (`crates/fence-seal`); one permit covers one write to one exact target.
- The outbox is the only write path; an unknown outcome is checked, never blindly resent; nothing is resent across
  runs without `Store::resend` (`crates/teams-core/src/outbox.rs`).
- One engine per account (`crates/teams-core/src/engine.rs`); nothing else in the process opens the store file.
- Deliveries are never answered except a call's typed response (`FrameSink::respond`); only a `CallAction` answers,
  declines or joins a call, and nothing in a view does it, or turns a camera on, by itself.
- No log line carries a payload except the app's debug log; the logger itself redacts credentials (`crates/teams-log`).
- No view builds a request: the app crates have no HTTP/WebSocket client; views reach the engine only through `Core`
  (`crates/teams-ui/src/bridge.rs`).
- Every lab or test GUI runs in `crates/ui-sandbox`, never on the user's display, bus or PipeWire.
- Message text is drawn only from sanitized `RichText`; the UI kit is held to `crates/teams-ui/tests/reach.rs`; no Zed
  code is copied (GPL).
- Call media is libwebrtc's (`crates/teams-media/src/rtc.rs`): relay-only ICE, SDP only in Teams' dialect.
- The sign-in window (`crates/teams-signin`) is Microsoft's page in a WPE WebKit view (`crates/teams-wpe`, from
  ngterm) drawn in a GPUI window of the app that asks; its engine is that app's executable run again as a worker
  (`teams_wpe::worker::serve_if_asked` early in `main`), on the system's `wpewebkit` (2.54 or newer, found by
  pkg-config; its helpers are started from `/usr/lib/wpe-webkit-2.0/`, so it is never vendored or linked statically).
  Every request a page sends is decided and logged to `fence.log` by the guard before it goes
  (`teams-signin-extension` in every web process); the guard isn't a complete fence
  (`docs/backlog/webkit-sign-in-guard-redirects.md`), so the network session's only way out is the egress proxy. The
  lab's window (`teams-signin --lab`) stays on loopback; the lab signs in through `proto-lab auth`. The personal
  window opens in `ngteams` itself, bound to Entra's and the identity provider's hosts (`egress::HostGate`).

## Where things live

- [README.md](README.md): layout, building, each binary's commands, captures, fixtures and tests. `docs/protocol/`:
  how each Teams service works, `endpoints.md`, `fence-targets.md`. `docs/backlog/`: deferred work. `docs/plans/`:
  plans as decided then, not the current design. Lab: `~/.local/share/teams-lab/`, `~/.config/teams-lab/lab.toml`.
