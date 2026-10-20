# ms-teams

A native Microsoft Teams client for Linux in Rust + GPUI, written against Teams' internal APIs with the web client as
the reference. M0 (a protocol lab with a hard write fence, and risk spikes) is done: its plan is
`docs/plans/completed/20261019-native-teams-m0-lab-and-spikes.md`, its outcome `docs/m0-acceptance.md`. The next
milestone is M1, the headless chat core (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`). Commands and
layout are in [README.md](README.md).

## Hard rules

These hold for every agent and every change, with no exceptions.

- **Writes go only through the fence, and only within the 3 lab accounts.** A write is anything others could notice:
  messages, edits, deletes, reactions, typing, read receipts, calls and answering a call, invites, meetings, member
  changes, chat creation, channel posts, bot replies. It is allowed only when every participant of its target, fetched
  fresh, is a lab account or an approved service participant. Anything unknown, partial or paginated is denied.
- **Nothing runs live against the tenant before the browser fence is armed** (M0 Tasks 6–7): HTTP interception on
  every host and target, the WebSocket guard, the service worker neutralized. Until the write-target mapping exists
  (M0 Task 11) the fence runs fail-closed: every mutating request is denied except the seeded own-state list.
- **Captures and tokens never enter the repo, not even sanitized.** Captures live under
  `~/.local/share/teams-lab/captures/`. `proto-lab export` only writes a sanitized local reference copy under
  `~/.local/share/teams-lab/reference/` (it refuses any destination inside a git work tree). Committed fixtures are
  written synthetic-from-capture: read the reference copy locally, write a fixture of the same shape with made-up
  values, and mark it synthetic (a top-level `"_synthetic"` note in JSON, on the first line of JSONL, a
  `_synthetic:` first-line header in other text). The fixture guard (`crates/fixture-guard`) must pass on every
  `tests/fixtures/` (top-level, crates', spikes') and `docs/`, and every fixture in them must carry the marker, apart
  from the non-Teams test inputs listed in `fixture_guard::synthetic::TEST_INPUTS`. The sanitizer is
  defence in depth, not permission to commit its output. Tokens never reach logs or stdout: print only aud, exp,
  scopes and tenant.
- **Agents never build or run a personal build and never receive the user's tokens.** Lab binaries get a
  `WritePermit` only from `teams-fence`: their fence comes from `Fence::open_lab` (roster from `lab.toml`, the lab's
  `fence.log`), and `teams-fence`'s `test-support` feature (any roster, any log, fake membership sources) is enabled
  only under `[dev-dependencies]`, never as a normal dependency.
- **Protocol notes are written in our own words** in `docs/protocol/`. Microsoft JS is never copied into the repo.

## Patterns learned in M0

What the code does and why. A change that undoes one of these needs a new reason, not a cleanup.

### Arming the lab browser (`crates/proto-lab/src/arming.rs`)

- **Order**: `Fetch.enable` with `*` on the **browser** session first, then browser-level `Target.setAutoAttach` with
  `waitForDebuggerOnStart`, then the service workers neutralized through a blank armed tab, and only then pages. Each
  attached target is armed by its type, and `Runtime.runIfWaitingForDebugger` is always its last step.
- **Fetch on two levels.** Every target with a `Fetch` domain gets `*` on its own session, and the browser session
  pauses every request again; both run the same decision, and a deny at either fails it. Per-target `Fetch` alone lets
  a `sendBeacon`/`fetch(keepalive)` from `pagehide` out when the tab closes. A dedicated worker has no `Fetch`: its
  requests pause on its page's session (and the browser's).
- **WebSocket guard** (`assets/ws_guard.js`, `fence_ws.rs`) in every scope, before the scope's code runs: pages and
  iframes through `Runtime.enable` + `Page.enable` + a new-document script with `runImmediately`, workers through a
  `Runtime.evaluate` that must report `armed`. Frames wait per socket and go out only on an explicit allow, answered
  with the run's token; a send on a socket made in another realm is refused (`foreign`). `Network.webSocketFrameSent`
  is only an audit: a frame no allow covers is a breach.
- **Service workers only through the browser.** Target-level auto-attach filters them out: one attached twice while
  paused answers nothing until it runs. At run start they are unregistered and their caches cleared
  (`Storage.clearDataForOrigin` works only on a page session), and pages bypass them (`Network.setBypassServiceWorker`
  needs `Network.enable` on the same session).
- **Worklets** (the calling stack's audio) have no network API and no `Fetch`: armed with `Resume` only.
- **A target that can't be armed is closed, never resumed.** Every step times out after 10 s. A target closed before
  its resume ran nothing (a warning); one whose resume was sent, or that couldn't be closed, is a breach.
- The browser lives and dies with the fence: `ArmedBrowser` owns it, a lost CDP connection kills it, and `close` waits
  for decisions still running (a live membership read) before the run's denies are read. Reporting and NEL are
  disabled by flag (their uploads bypass `Fetch`). The CDP client stays our own: `chromiumoxide` resumes held targets
  by itself.

### The sealed fence API (`crates/teams-fence`, `crates/fence-seal`)

- A lab binary's fence is `Fence::open_lab()` (roster from `lab.toml`, the lab's `fence.log`); its membership source is
  `FailClosed` (every fetched scope `Partial(NoSource)`) or `LiveMembershipSource` (1:1 chats only, complete only when
  the members are exactly the two the chat id names). `MembershipSource` and `Mutating` are sealed: a new source lives
  in `teams-fence`, a new write type in `teams-proto`, and `fence-seal`'s trybuild cases prove nothing else can mint a
  permit. A binary built with `test-support` (`cargo test`, `--all-targets`, `--all-features` unify it in) refuses to
  run, so live runs use `cargo build -p <crate>`.
- One permit covers one write to one exact `Target` (kind, scope, added participants); `Authorized::new` checks it.
  A request the fence can't read with certainty is `Unknown` and denied: bodies are read strictly (case-folded
  duplicate keys make it `Unknown`), and shapes are allowlisted from captures (the echo call start), never guessed.
- Writes on an existing call can't be fenced yet: no authoritative call-membership read exists
  (`docs/backlog/fence-call-follow-ups.md`). Don't stand in a registry of what the fence permitted.

### Captures, reference copies, fixtures

- Captures (`captures/<scenario>/<ts>/events.jsonl`) and `proto-lab export`'s reference copies (`reference/<name>`)
  live under the lab's data dir. Export refuses any destination inside a git work tree and warns about what its checks
  find (`--strict` refuses instead). Neither is ever copied into the repo.
- A committed fixture is written by hand from a reference copy: the same shape, made-up values, a `_synthetic` marker.
  Run `cargo test -p fixture-guard` after touching any `tests/fixtures/` or `docs/`: it checks the marker and scans
  for tokens, signatures, emails, MRIs, GUIDs (except `PUBLIC_GUIDS`), meeting links and passcodes.
- `fence.log` keeps scope ids verbatim (a 1:1 chat id names both object ids): it is lab data, not sanitized material.

### Scenarios (`crates/proto-lab/src/scenario/`)

- Steps use the DSL `Driver` (`dsl.rs`): `data-tid`/CSS waits, request and failure waits; every page call is bounded
  (`CALL_TIMEOUT`), and a timeout names its step.
- A writing scenario proves enforcement live first: a `ForcedDeny` (method + URL regex + body marker, denied before
  classification, logged `forced`) on a probe that must fail in the page (`net::ERR_BLOCKED_BY_CLIENT`, matched by
  `networkId`) and never reach the other account. The web client resends a failed send on a later load, so the forced
  deny covers the marker for the whole run, and this run's probe carries a label of its own.
- A scenario's `DenyPolicy` says which denies it expects (`load`: all; `ab-chat`: forced + telemetry); any other deny
  fails the run. So `ab-chat` and `echo-call` exit 1 on the web client's unreviewed fail-closed denies
  (`docs/backlog/ab-chat-exit-code-counts-unreviewed-denies.md`): judge them by their output and `fence.log`, not the
  exit code.
- Classifiers are chosen per run: `seeded` (status, load, auth), `sign_in` (login only), `mapped` (ab-chat), `calling`
  (echo-call). Own-state is added only after a review, in the exact shape a capture showed.

### Live runs

- From a normal build, headless (`--headless`), under the armed fence, **one browser per lab profile at a time**;
  no browser is left running afterwards. Writes reach only the 3 lab accounts and the approved echo service.
- **Never answer a call.** No scenario clicks answer, the fence doesn't map `acceptance`, and Trouter delivery
  responses stay denied. A native Trouter client ignores incoming calls the same way.
- Live tests are built only with `--features live` and run only with `PROTO_LAB_LIVE=1`.
- Tokens: our client is the native `1fec8e78-…` with the `nativeclient` redirect. The chat service takes
  `Authorization: Bearer` with an `https://ic3.teams.office.com` token, not a skypetoken, and a skypetoken expires with
  the Entra token it was exchanged from (`docs/protocol/auth.md`).

### Chrome under load

The machine is often loaded by other agents' builds (load average 30–75), and then a few things flake:

- an iframe whose arming step gets no answer within 10 s: closed, and a breach of that run unless it was closed
  before its resume;
- `scenario_dsl::a_request_is_waited_for_even_when_it_came_first_and_matched_once` (`net::ERR_ABORTED`);
- `browser::a_browser_that_exits_early_is_reported`.

Rerun that one test or run before calling it a failure; never raise a timeout or skip an arming step to make it pass.

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
