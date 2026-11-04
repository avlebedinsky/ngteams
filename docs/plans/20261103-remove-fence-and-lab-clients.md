# Remove the write fence and the lab clients: ngteams is the only client

## Overview
- The user's decision of 2026-11-03: the write fence (`teams-fence`, `fence-seal`, `fence.log`, the browser fence) and
  the dedicated lab clients (`teams-app`, `teams-headless`) go. `ngteams` becomes the only client binary, and all live
  testing runs through it, signed into lab accounts in the UI sandbox.
- The lab rule itself stands: lab accounts reach only each other, the echo service and the user's own account. The
  agent enforces it, not code. Before each live run it states which chats, calls and people the run reaches and
  confirms they are only those.
- The release build `ngteams` carries nothing of the lab or the fence: it doesn't read `lab.toml`, keeps no
  `fence.log` and makes no lab keep-apart checks. A stale `lab.toml` stopped it from starting on 2026-11-03.
- The debug log stays complete: with the `debug` level on, every API call the app makes is logged with its full
  request and response (headers and body, credentials redacted by the logger), and every Trouter frame both ways
  (user decision of 2026-10-22, restated 2026-11-03). `info` stays payload-free. This lives in `teams-log::wire` and
  the transport, not in the fence, and must survive the cut. Today `teams-auth`'s token and authz calls bypass it.
- `proto-lab` stays for web-client captures, bundle search and scenarios, without its browser fence.
- The user will provide new lab accounts once this is done. The current ones (labbot, labbot2, labbot3) may then be
  removed.

## Context (from discovery and the plan review)
- **General code in `crates/teams-fence`** (about 7.5k lines) that the shared crates use:
  - `Mri`, the chat, team, 1:1 and meeting id helpers, and `chat_service_path`;
  - the hosts and constants: `AFD_HOSTS`, `CALL_PROXY_HOST`, `CALL_START_URL`, `TEST_CALL_SCENARIO`,
    `TEAMS_ECHO_BOT_MRI`, `INVITATION_EVENTS`;
  - `log_url` and `path_shape`, the redacted URL shape of a log line;
  - `apart::overlapping` and the XDG resolution;
  - the personal `accounts.toml` (`PersonalAccount`, `PersonalPaths`);
  - `test_support_enabled()`, which the `test-support` features of `teams-proto` and `teams-core` chain into;
  - `lab.toml` parsing and `LabChats`, `ChatRecord`, `record_abc_chat` and `Alias`.
- **The fence's call tracking is app behavior** and must survive:
  - `Fence::observe_delivery` (`decision.rs:842-867`, `calls.rs`) turns a calling delivery into `Observed`. It takes
    an offer only when the callee is the account and the caller isn't, drops a redelivered invitation, ignores one
    whose ring ended, and binds the acceptance and renegotiation links.
  - `CallFloor` (`floor.rs`) keeps one call or ring at a time across the app's accounts over each fence's
    `CallRegistry`. `DenyReason::AnotherAccountInCall` applies it to a start, join or answer.
  - Users of that behavior:
    - `teams-call/src/machine.rs:1204-1228,1589` (offered, renegotiation, acceptance);
    - `teams-trouter/src/session.rs:1130` (drops `Observed::Ignored`);
    - `teams-core/src/calling.rs:574` (`let_go`).
  - Its tests: `crates/teams-fence/tests/{calls.rs,offers.rs}`.
- **The write path:** `teams-proto` (`Mutating`, `Authorized<_>`, `WritePermit`, `contacting`, `check_post_read`, the
  frame sink's `fence.log`), `teams-core`'s outbox (`Fence::check`), `teams-trouter` (`observe_delivery`,
  `check_delivery_response`), `teams-mock`, and `ngteams` (`Fence::open_personal`).
- **Other fence users:**
  - `teams-signin` and `teams-signin-extension` (its tests use `FenceLog`): the guard logs to `fence.log`; the lab's
    loopback window binary and its pipe protocol (`teams_auth::signin::sign_in_with_window`,
    `WindowRequest`/`WindowAnswer`, `teams-signin/tests/{binary.rs,answer_channel.rs}`).
  - `teams-log` (`Diagnostics.fence_log`, `MAX_FENCE_LINES`), with callers `ngteams/src/app.rs:97,111` and
    `teams-ui/src/settings.rs:1212`.
  - `scripts/run-personal-read-only.sh` points at `snapshot/data/fence.log`.
- **The `personal` features** of `teams-fence`, `teams-media` (`devices::pipewire::THIS_BUILD`, also read by
  `ui-sandbox-media-probe`, `ui-sandbox/tests/pipewire.rs`, `teams-media/tests/pipewire_reach.rs`) and
  `teams-signin`.
- **`ngteams`:**
  - It refuses a `test-support` build and one without the install marker `TEAMS_PERSONAL_INSTALL`, which only
    `scripts/install-personal.sh` sets (`refusal.rs`).
  - It already has `--data <dir>`, the credential broker (`ngteams secrets`), `ui-sandbox-run` and
    `run-personal-read-only.sh` as the model.
  - Its keyring namespace is `ngteams`.
  - `accounts.rs`' `Started<S>` is generic over the secret store.
- **`proto-lab`:**
  - `proto-lab login` already types a lab account's password from `loadAccounts` safely.
  - `proto-lab auth` already writes native tokens in teams-auth's `SecretKey` layout (to `teams-lab`), and catches
    Entra's redirect with `fence_http::RedirectCatch`.
  - Several scenarios hold a write back with a forced deny to capture its shape without sending it: `channel_create`,
    `team_manage`, `meeting_steps`, `ab_meeting_join`, `ab_call`/`abc_call` `--call`, `own-presence-*-held`.
  - `probe` and `thread-probe` read members through `LiveMembershipSource`.
- **Tests and tools tied to the lab clients:**
  - `crates/ngteams/tests/apart.rs` (lab binaries, `teams_core::lab`, proto-lab's fence, `teams_fence::personal`);
  - `crates/teams-ui/tests/reach.rs` (`APP_CRATES` names `teams-app`; `teams-fence` in `LINKED_CRATES`);
  - `crates/teams-core/tests/lab_startup.rs`;
  - `crates/ngteams/tests/fence.rs`, and the `fence.log` assertions in `ngteams/tests/support/*` and `read_only.rs`;
  - `teams-trouter/tests/session.rs:894`, `teams-core/tests/calls/failures.rs:288`;
  - `.claude/skills/teams-ui/SKILL.md` drives UI screenshots through `teams-app`'s `ui_tour` and `call_window` tests.

## Development Approach
- **testing approach**: Regular (code first, then tests). This is mostly deletion and moves: tests of kept behavior
  move with their code, and tests of the fence alone are deleted with it.
- After each task the workspace builds, `cargo clippy --workspace --all-targets` is clean, and the touched crates'
  tests pass, `ngteams`' `apart` and `teams-ui`'s `reach` included whenever crates or dependencies change.
- A test of kept behavior is never dropped; it moves. Kept behavior includes:
  - the id helpers, accounts.toml, the call tracking and the cross-account call floor;
  - the outbox's unknown-outcome rule;
  - the transport's https, no-proxy, no-redirect and one-token-per-service rules;
  - the test-support refusal;
  - the debug wire log (`teams-proto`'s `log_request`/`log_response`, `teams-trouter`'s frame logging), with its
    redaction tests.
- Don't add new guards in place of the fence (`.claude/planning-rules.md`: plan the feature, not guard layers).
- Size checks to each task (CLAUDE.md "Checks"). Don't edit other files in `docs/plans/`.

## Testing Strategy
- **unit and integration tests**: each crate's own suite. The mock-backed tests of `teams-core`, `teams-call`,
  `teams-ui` and `ngteams` (`teams-mock`) cover writes sent without a permit, and the call tests cover the moved call
  tracking.
- **UI**: `ngteams`' sandbox tests, joined by the window tests moved from `teams-app` (Task 1).
- **coverage**: every remaining runtime crate stays at 90% or more (`scripts/coverage.sh --require-target`).
- **live**: the smoke run of `ngteams` on two lab accounts (Post-Completion, after the agent's rule check).

## Progress Tracking
- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- keep the plan in sync with the work

## Solution Overview
- **Order:**
  1. Remove the lab clients first.
  2. Make `proto-lab` stand on its own, then remove its browser fence.
  3. Take `teams-signin` and `teams-media` off the fence and the `personal` features.
  4. Make the behavioral cut of the write path, moving the call tracking into `teams-call`.
  5. Move the rest of the general code mechanically and delete `teams-fence` and `fence-seal`.
  6. Give `ngteams` a lab mode and add the runner script.
  7. Update the docs.
- **Where the general code goes:**
  - `teams-proto`: `Mri`, the ids, `chat_service_path`, the hosts and constants above, `log_url`/`path_shape`, and
    `test_support_enabled()` (with `teams-core`'s `test-support = ["teams-proto/test-support"]`).
  - `teams-call`: the call tracking (offer filter, redelivery dedupe, ignore after ring end, acceptance and
    renegotiation link binding, one call per account), with `Delivery`/`Observed` as its own types.
  - `teams-core`: `CallFloor` across accounts, over the engines' `teams-call` state, for invitations and for start,
    join and answer.
  - `ngteams`: accounts.toml, the XDG resolution and `overlapping`.
  - `proto-lab`: `lab.toml` parsing and the lab chat records.
- **The write path after the cut:**
  - A write in `teams-proto` is a plain typed request sent through the transport.
  - The transport keeps `https` only, no proxy, no redirects, one token per service's host, no hidden retries.
  - The outbox keeps `clientmessageid`, the unknown-outcome rule and `Store::resend` across runs.
  - Trouter answers a call delivery only with `FrameSink::respond`'s typed response for that delivery's connection.
  - Read-only mode keeps refusing writes in the transport, logged to the diagnostic log in the old line's shape
    (method, host, path shape).
  - `teams-log`'s diagnostics drop the `fence.log` section.
  - Every send still goes through one of the transport's logged funnels, so at `debug` each call's full request and
    response reach the diagnostic log, whatever kind of write or read it is. No path that used to be a permit write
    may bypass it.
- **Lab runs of `ngteams`:**
  - **The lab build marker** (decided in this plan, following from the user's "all testing with ngteams"):
    - `scripts/run-lab.sh` builds `ngteams` into `target/lab` with `TEAMS_LAB_BUILD=1`, which only that script sets.
    - A binary with that marker runs only in lab mode: data under `~/.local/share/ngteams-lab/<alias>/`, keyring
      namespace `ngteams-lab`, window only in the UI sandbox. It refuses the user's own dirs and the `ngteams`
      namespace.
    - The install script's marker stays the only way to a binary that runs the user's account.
    - CLAUDE.md's "never `cargo build -p ngteams`" becomes "agents build `ngteams` only through
      `scripts/run-lab.sh`".
  - **Tokens:** `proto-lab auth` writes a lab account's native tokens into the `ngteams-lab` namespace instead of
    `teams-lab`. Its signed-in browser profile does the sign-in (it already types the password safely from
    `loadAccounts`), so no password is typed through the sandbox's control socket.
  - **The run:** `scripts/run-lab.sh <alias>` starts `ngteams secrets` outside the sandbox (that account in
    `ngteams-lab`: `get`, `put`, `delete`) and `ngteams --lab <alias>` inside its own sandbox (sending mode, the
    sandbox's PipeWire). Several aliases can run at once. It has the read-only runner's control verbs, and holds the
    live lock for as long as the run lives.
  - The user's own account is still run by agents only through `run-personal-read-only.sh`.

## What Goes Where
- **Implementation Steps** (`[ ]`): the code, tests, scripts and docs in this repo.
- **Post-Completion**: the user's new lab accounts, removing the old accounts' data, the first live smoke run, and
  `.claude/planning-rules.md` (the user's own file, still written around the fence).

## Implementation Steps

### Task 1: Remove the lab clients (teams-app, teams-headless) and what only they use

**Files:**
- Delete: `crates/teams-app/`, `crates/teams-headless/`, `crates/teams-core/src/lab.rs`,
  `crates/teams-core/tests/lab_startup.rs`
- Delete: `scripts/native-call-run.sh`, `scripts/own-availability-native-run.sh`, `scripts/own-presence-run.sh`
- Delete: `crates/proto-lab/src/scenario/{ab_call_native,ab_call_app,abc_call_mixed}.rs` and their tests
- Move: `crates/teams-app/tests/{ui_tour.rs,call_window.rs,window.rs,accounts_window.rs}` →
  `crates/ngteams/tests/` (adapted to ngteams' app)
- Modify: `Cargo.toml` (members, default-members), `coverage.toml`
- Modify: `crates/ngteams/tests/apart.rs`, `crates/teams-ui/tests/reach.rs`, `.claude/skills/teams-ui/SKILL.md`
- Modify: `crates/teams-ui` (anything only the lab app used, e.g. `LabPaths::ui_settings_file`)

- [x] delete both crates, `teams-core::lab` and the lab-only scripts and scenarios, and remove them from the workspace
      and the coverage floors
- [x] move the window and UI-tour tests into `ngteams`' tests, and point the teams-ui skill's screenshot workflow at
      them
- [x] `apart.rs`: drop the lab-binary checks; `reach.rs`: `APP_CRATES` = `teams-ui`, `ngteams`, with the
      log-install check on `crates/ngteams/src/app.rs`
- [x] tests of `teams-core`, `teams-ui`, `proto-lab` and `ngteams` (`apart`, the moved window tests) pass; clippy
      clean

Validation was scoped per CLAUDE.md's Checks: all four moved sandbox tests and `apart`; the full offline
`proto-lab` suite; `teams-core`'s `account_startup`; `teams-ui`'s `reach` and `kit`; `teams-fence`'s
`lab_config` (the existing workstation-config test stays ignored); and all-targets clippy for the five touched
crates. All passed. The shared account assembler remains covered; the deleted lab startup wrapper is gone.

### Task 2: proto-lab stands on its own (config, ids, test-support)

**Files:**
- Modify: `crates/proto-lab/src/config.rs` (gets `LabConfig`, `LabLocations`, `LabPaths`, `LabChats`, `ChatRecord`,
  `record_abc_chat` and `Alias` from `teams-fence::lab`)
- Modify: `crates/proto-lab/Cargo.toml` (its own `test-support` feature, on through a self dev-dependency as
  `teams-core` does)
- Modify: `crates/proto-lab/src/{main.rs,commands/*probe.rs,...}`

- [x] move `lab.toml` parsing and the lab chat records into `proto-lab::config`; the roster checks that only fed the
      fence go (accounts, UPNs, MRIs, `data_dir`, the echo MRI and contacts stay while scenarios read them)
- [x] proto-lab's test-support refusal (`main.rs`, the probes' mock origins) reads its own feature
- [x] move the config tests (parse, missing file, bad fields, chat records)
- [x] `cargo test -p proto-lab`, `ngteams`' `apart` and clippy pass

Validation: `cargo test -p proto-lab` passed 664 tests (the existing workstation-config test remains ignored);
`ngteams`' `apart` and `teams-ui`'s `reach` passed 23 tests. Final all-target proto-lab clippy and the focused
config/library rerun passed. The local config retains structural parsing, atomic recording and personal-directory
separation; cross-participant MRI uniqueness and person/bot roster policy are no longer config validation. The
browser fence temporarily keeps its internal config until Task 3 removes that consumer.

### Task 3: proto-lab without the browser fence

**Files:**
- Delete: `crates/proto-lab/src/fence_ws.rs`, `crates/proto-lab/src/scenario/expected.rs`
- Delete: `crates/proto-lab/tests/{fence_http,fence_ws,fence_targets,expected_denies,call_registry}.rs`
- Modify: `crates/proto-lab/src/fence_http.rs` → keep `RedirectCatch` (for `proto-lab auth`) and a capture hold (CDP
  `Fetch` pause-and-fail of a scenario's matching request), as capture features, in modules of their own
- Modify: `crates/proto-lab/src/{arming.rs,session.rs,browser.rs,cdp.rs,commands/probe.rs,commands/thread_probe.rs}`
  and the scenarios
- Modify: `crates/proto-lab/Cargo.toml` (no `teams-fence`)

- [x] remove the browser fence: membership checks, `fence.log`, expected-deny classes and forced-deny probes; keep the
      CDP setup captures need
- [x] hold-back scenarios use the capture hold to keep a write from being sent while its shape is recorded
- [x] `probe` and `thread-probe` read members with a plain GET instead of `LiveMembershipSource`
- [x] adapt tests: the hold, `RedirectCatch`, a capture with no fence
- [x] `cargo test -p proto-lab`, `ngteams`' `apart` and clippy pass; `proto-lab` no longer depends on `teams-fence`

Validation: 532 offline proto-lab tests passed across the full run and focused reruns; the existing workstation-only
config test remains ignored. Chrome capture, redirect, login, WebRTC and DSL tests ran with
`PROTO_LAB_REQUIRE_CHROME=1`. `ngteams`' `apart` (9), `teams-ui`'s `reach` (14), all-target Clippy for proto-lab and
ngteams, formatting, coverage-source metadata and the dependency-tree audit passed. The browser now records Network
and explicit hold events rather than policy verdicts; older capture files remain readable. The quiet/presence
activity experiment is a selective frame capture hold, without the old classifier, permission queue or audit.

### Task 4: teams-signin off the fence; the lab sign-in window goes

**Files:**
- Modify: `crates/teams-signin/src/{lib.rs,cli.rs,guard.rs,hosts.rs,main.rs,window.rs}`, its `Cargo.toml` and tests
  (`guard.rs`, `gate.rs`, `support/windows.rs`, `window.rs`)
- Modify: `crates/teams-signin-extension/Cargo.toml` (no `teams-fence`)
- Delete: the lab window binary, `teams-signin/tests/{binary.rs,answer_channel.rs}`, and
  `teams_auth::signin::sign_in_with_window` with `WindowRequest`/`WindowAnswer`

- [x] the guard logs its decisions to the diagnostic log in the old line's shape (no bodies); the host-gated window is
      the only mode; the `personal` feature and `--lab` go
- [x] delete the lab window binary and its pipe protocol
- [x] adapt the guard and window tests to the log lines
- [x] tests of `teams-signin`, `teams-signin-extension`, `teams-auth` and `ngteams` (`signin`), and clippy, pass

Validation: 201 tests across `teams-auth`, `teams-signin` and `teams-signin-extension`, both real WPE window
sandbox runs with `GPUI_SMOKE_REQUIRE=1`, 11 ngteams sign-in tests, 9 separation tests, 2 private window-service tests
and 14 UI dependency-audit tests passed. All-target Clippy for the four affected crates, the normal production-mode
sign-in library check, formatting and coverage metadata checks passed. The in-process handoff remains typed and
serialization-free (`SignInRequest`/`SignInAnswer`); the standalone binary and JSON pipe are gone. Offline loopback
mode exists only in test-support builds, which ngteams still refuses.

### Task 5: teams-media's PipeWire reach decided at run time

**Files:**
- Modify: `crates/teams-media/src/devices/pipewire.rs` (`connect`/`connect_with_camera_portal` take a `Reach`),
  `crates/teams-media/Cargo.toml`, `crates/teams-media/tests/pipewire_reach.rs`
- Modify: `crates/ui-sandbox/src/bin/ui-sandbox-media-probe/main.rs`, `crates/ui-sandbox/tests/pipewire.rs`
- Modify: `crates/ngteams/src/calling.rs`, `crates/ngteams/Cargo.toml`

- [x] remove `THIS_BUILD` and the `personal` feature; callers pass the `Reach` (ngteams: desktop normally, sandbox
      in lab and read-only modes; tests and ui-sandbox: sandbox)
- [x] adapt the reach tests
- [x] tests of `teams-media`, `ui-sandbox` (PipeWire) and `ngteams` (`calls`), and clippy, pass

Validation: 76 focused tests passed: media reach and camera routing (12), private PipeWire (6), the sandbox
camera portal (1), ngteams's library/startup/calls/app/separation tests (43), and the UI dependency audit (14).
PipeWire and portal tests ran with `GPUI_SMOKE_REQUIRE=1`; the ignored canary child is exercised by its parent.
All-target Clippy for teams-media, ui-sandbox and ngteams, formatting and coverage-source metadata checks passed.
Normal startup passes desktop reach explicitly; controller tests and sandbox probes pass sandbox reach. Read-only
startup still opens no devices. Task 9 will pass sandbox reach from the lab-mode startup it introduces.

### Task 6: The behavioral cut: writes without permits, call tracking in teams-call

**Files:**
- Modify: `crates/teams-proto/src/{write.rs,write/**,http.rs,client.rs,request.rs,post_read.rs,own.rs,frame.rs,lib.rs}`
- Create: `crates/teams-call/src/track.rs` (or similar): the call tracking from `teams-fence/src/{calls.rs,calls/**}`
- Create: `crates/teams-core/src/call_floor.rs`
- Modify: `crates/teams-call/src/machine.rs`, `crates/teams-trouter/src/session.rs`,
  `crates/teams-core/src/{outbox.rs,outbox/**,account.rs,engine.rs,calling.rs}`
- Modify: `crates/teams-mock/src/**`, `crates/ngteams/src/{engine.rs,main.rs,app.rs,read_only.rs,refusal.rs}`
- Modify: `crates/teams-log/src/diagnostics.rs`, `crates/teams-ui/src/settings.rs`,
  `scripts/run-personal-read-only.sh`
- Move: `crates/teams-fence/tests/{calls.rs,offers.rs}` (kept cases) → `crates/teams-call/tests/` and
  `crates/teams-core/tests/calls/`

- [x] `teams-proto`:
      - writes are plain typed requests: `Authorized<_>`, `WritePermit`, `Mutating` sealing, `contacting`
        classification, `check_post_read` and `fence.log` writes are removed;
      - `test_support_enabled()` lives here, and `teams-core`'s `test-support` chains to it;
      - the transport rules stay, with their tests.
- [x] `teams-call`: the call tracking moves here: offer filter, redelivery dedupe, ignore after ring end, acceptance
      and renegotiation link binding, one call per account. The machine and `teams-trouter` use it instead of
      `Fence::observe_delivery`. Only the lab roster check (`roster.admits`) is dropped.
- [x] `teams-core`:
      - the outbox sends without `Fence::check` and keeps its unknown-outcome and no-resend rules;
      - `CallFloor` keeps one call or ring across accounts for invitations and for start, join and answer;
      - `let_go` maps to the tracker's release.
- [x] `ngteams`, `teams-mock`, `teams-log` and `teams-ui`:
      - no personal fence;
      - the refusal reads `teams_proto::test_support_enabled()`;
      - read-only refusals go to the diagnostic log;
      - the diagnostics lose the `fence.log` section;
      - `run-personal-read-only.sh` points at the log instead.
- [x] tests:
      - move the call-tracking cases;
      - turn `ngteams/tests/fence.rs` into a plain-send test, or delete it;
      - replace the `fence.log` assertions (`ngteams/tests/support/*`, `read_only.rs`,
        `teams-trouter/tests/session.rs:894`, `teams-core/tests/calls/failures.rs:288`) with log-line or send
        assertions;
      - delete tests of fence decisions alone.
- [x] the debug wire log: every send path left after the cut (plain request, former permit write, post read, own
      state, call request, upload or ranged read) goes through `log_request`/`log_response`. A test sends one of each
      kind against the mock at `debug` and finds its full request and response in the log, credentials redacted, and
      nothing at `info`. The same holds for Trouter frames both ways.
- [x] `cargo check --workspace --all-targets`, clippy, and tests of `teams-proto`, `teams-call`, `teams-trouter`,
      `teams-core`, `teams-mock`, `teams-log`, `teams-ui` and `ngteams` pass

### Task 7: Move the remaining general code; delete teams-fence and fence-seal

**Files:**
- Create: `crates/teams-proto/src/ids.rs` (`Mri`, the ids, `chat_service_path`, hosts and constants,
  `log_url`/`path_shape`)
- Create: `crates/ngteams/src/accounts_file.rs` (or into `accounts.rs`): accounts.toml, the XDG resolution,
  `overlapping`
- Delete: `crates/teams-fence/`, `crates/fence-seal/`
- Modify: every crate's `Cargo.toml`, root `Cargo.toml`, `coverage.toml` (baselines and `[excluded] fence-seal`),
  `crates/teams-ui/tests/reach.rs` (`LINKED_CRATES`), `crates/ngteams/tests/apart.rs`

- [x] move the items with their tests and switch the imports; when `log_url` moves into `teams-proto`, have
      `teams-signin::diagnostics::record` call that shared helper and remove its duplicate `endpoint`, `opaque`
      and `authority` helpers, preserving the sign-in diagnostic credential-redaction tests
- [x] delete both crates and remove them from the workspace and coverage
- [x] `apart.rs` keeps what still holds: test-support only in dev-dependencies, the test-support refusal, no
      `proto-lab` in `ngteams`' tree
- [x] `cargo check --workspace --all-targets`, clippy, `cargo test --workspace` pass; `cargo tree -p ngteams` shows no
      `teams-fence` or `proto-lab`

Validation: 109 focused tests passed, including the moved account/XDG, IDs, chat-service and URL-shaping
cases; all 13 retained typed-construction compile-fail cases; production-only mock-origin/registrar
contracts with a passing feature-off baseline; UI compose/reach; sign-in redaction/gate; fixture guard; and
the real sandbox startup-controller test (`GPUI_SMOKE_REQUIRE=1`). Workspace all-target check and Clippy
(warnings denied), formatting, lint-report, dependency-tree and all 14 coverage-source review checks passed.
The complete workspace runtime suite is deferred to Task 12 per CLAUDE.md Checks; this mechanical move
reran the retained behavior it moved or adapted. Both obsolete crate directories and dependencies are gone.

### Task 8: Every API call in the debug log (teams-auth too)

**Files:**
- Modify: `crates/teams-auth/src/{token.rs,authz.rs}` (and any other `reqwest` client outside `teams-proto`'s
  transport: `rg reqwest crates/*/src`, lab-only `proto-lab` aside)
- Modify: `crates/teams-log/src/wire.rs` if the token endpoint's and authz's fields need redaction rules they lack
- Modify: `crates/teams-auth/Cargo.toml`, tests in `crates/teams-auth/tests/{token.rs,authz.rs}`

- [x] the token exchange and refresh, authz (skypetoken) and any other API call outside the transport log their full
      request and response through `teams_log::wire` at `debug`, as the transport does
- [x] the logger redacts their credentials: `code`, `code_verifier`, `refresh_token`, `access_token`, `id_token`,
      the skypetoken, `Authorization`, cookies
- [x] tests: at `debug` each call's request and response are in the log with every credential redacted; at `info`
      nothing of them is
- [x] tests of `teams-auth` and `teams-log`, and clippy, pass

Validation: all 211 tests in `teams-auth` and `teams-log` passed; the existing real Secret Service keyring
test remains ignored. Both installed-logger endpoint regressions passed again after test-helper lint cleanup.
Affected-crate all-target Clippy (warnings denied), formatting, lint-report, diff and coverage-source metadata
checks passed. The API-client audit found only token/authz outside the already logged protocol transport.
Known sent credentials are exchange-scoped logger context, masking raw/encoded error echoes before ordinary
redaction without changing request or response bytes; noncredential descriptions and trace text remain visible.

### Task 9: ngteams lab mode and the lab build marker

**Files:**
- Modify: `crates/ngteams/src/{cli.rs,main.rs,lib.rs,refusal.rs,accounts.rs,app.rs,read_only.rs}`
- Create: `crates/ngteams/tests/lab_mode.rs`

- [x] `--lab <alias>`:
      - data and config under `~/.local/share/ngteams-lab/<alias>/`;
      - keyring namespace `ngteams-lab`, hardcoded;
      - sending mode, window only in the UI sandbox, sandbox PipeWire;
      - sign-in, refresh and forget through the broker store.
- [x] `ngteams secrets --lab <alias>` serves that account in `ngteams-lab` with `get`, `put` and `delete`, reading the
      lab dir's accounts; the read-only broker keeps `get` only on `ngteams`
- [x] the lab build marker:
      - `TEAMS_LAB_BUILD` (compile time) lets a binary run only with `--lab`;
      - lab dirs that overlap the user's own `ngteams` dirs are refused (`overlapping`);
      - the install marker still gates every other mode.
- [x] in-process tests, as `tests/read_only.rs` does:
      - the dirs and namespace;
      - the marker's refusals (lab marker without `--lab`; `--lab` in an install build is allowed);
      - the overlap refusal and the broker scope per mode;
      - a mock-backed send in lab mode.
- [x] tests of `ngteams` and clippy pass

Validation: 99 focused tests passed: ngteams library/startup/account/app/call/read-only/sign-in/separation
checks and the new lab path/marker, broker lifecycle, mock-backed send and self-exec descriptor tests (77),
plus UI account/reach tests (22). All-target Clippy for ngteams and teams-ui (warnings denied), formatting,
lint-report, whitespace and all 14 coverage-source reviews passed. Lab account operations use the selected
broker store; personal operations keep fresh lazy-keyring lifetimes. A lab alias selects its single configured
account directly, remains writable and can forget it, but cannot add another account. Sign-in self-exec
preserves broker/trace originals while ordinary child processes inherit neither. No live services were used.

### Task 10: scripts/run-lab.sh and lab tokens from proto-lab auth

**Files:**
- Create: `scripts/run-lab.sh`
- Modify: `crates/proto-lab/src/commands/auth.rs` (the keyring namespace `ngteams-lab`, and the account in the layout
  `ngteams --lab` reads)

- [x] `proto-lab auth <alias>` stores the native tokens and the lab account's entry where `ngteams --lab <alias>`
      finds them
- [x] `run-lab.sh <alias> [--pipewire]`:
      - builds `ngteams` into `target/lab` with `TEAMS_LAB_BUILD=1`, a normal build, not `--all-targets`;
      - starts the broker outside the sandbox and `ngteams --lab <alias>` in its own UI sandbox under
        `~/.local/state/ngteams-lab/<alias>/`, with several aliases at once;
      - holds the live lock while it runs;
      - has the control verbs `--shot`, `--key`, `--type`, `--click`, `--windows`, `--status` and `--stop`.
- [x] tests for `auth`'s new target (mock); shellcheck clean on the script

Validation: 52 focused tests passed: 13 auth-controller/provisioning cases, 30 retained config tests
(one existing workstation-only case stays ignored), and 9 synthetic launcher lifecycle/control tests.
All-target proto-lab Clippy (warnings denied), formatting, lint-report, metadata, Bash syntax and
ShellCheck passed. The launcher preserves custom XDG data/config roots inside the clean sandbox,
retains account files when data/state roots coincide, and supports --build-only without a live start.
Concurrent aliases share the fixed legacy live lock; each holder owns its alias lock until cleanup.
A TERM-only shutdown deadline defaults to 1800 seconds (--timeout overrides); stopped/failed runs
retain diagnostics. Actual tenant/keyring/browser sessions and a normal lab build were not run.

### Task 11: The lab rule as the agent's per-run check; docs

**Files:**
- Modify: `CLAUDE.md`, `docs/lab-rules.md`, `docs/personal-build.md`, `README.md`, `docs/ci.md`
- Modify: `docs/protocol/endpoints.md` (no fence or contacting columns); delete or fold
  `docs/protocol/fence-targets.md`; `docs/protocol/{calling.md,calling-echo.md,trouter.md,auth.md,calendar.md}` (no
  `teams-headless` commands)
- Modify: `.claude/skills/task-dispatch/SKILL.md` (no fence breach), module docs that described the fence

- [x] `docs/lab-rules.md`: who lab accounts may reach (unchanged), and the agent's check before each live run. The
      agent lists the chats, calls and people the run reaches, and runs it only if all are lab accounts, the echo
      service or the user's own account.
- [x] `CLAUDE.md`:
      - hard rules: drop the browser-fence arming, `fence.log` and forced-deny items;
      - agents build `ngteams` only through `run-lab.sh` and drive it freely with lab accounts in the sandbox; they
        run the user's own account only read-only;
      - design invariants: drop the permit, sealing and arming items; keep the outbox, transport, delivery-response,
        views-through-`Core`, `RichText`, libwebrtc and WPE sign-in items;
      - conventions: drop the contacting-row and forced-deny items.
- [x] README (layout, the lab section with `lab.toml`'s shape and the `ngteams-lab` paths), the personal-build doc
      and the CI doc match the code
- [x] `cargo test -p fixture-guard` passes

Validation: fixture guard passed 17 tests after the docs changes; the existing UI compose and own-presence
targets passed 67 tests for corrected diagnostic labels. Formatting, lint-report, diff whitespace, all 14
coverage-source reviews and 399 local Markdown file/anchor checks passed. Endpoint method/path cells are
unchanged across all 270 inventory table rows. The old fence-targets path is a short historical redirect to
chat-service.md, preserving archived links without current policy instructions. Only source comments, two
test names and three diagnostic labels changed outside documentation; request/state behavior is unchanged.
Full workspace runtime and coverage validation remain Task 12. The user-owned planning-rules file and all
other plan files were left untouched.

### Task 12: Verify acceptance criteria
- [x] `cargo tree -p ngteams` has no `teams-fence` or `proto-lab`; `rg -i 'fence|lab\.toml' crates/ngteams` finds
      nothing stale
- [x] at `debug` the app's log has every API call's full request and response and every Trouter frame, credentials
      redacted (the tests of Tasks 6 and 8 pass)
- [x] `cargo check -p ngteams --release` passes (only `run-lab.sh` and the install script build it)
- [x] `cargo clippy --workspace --all-targets`, `scripts/lint-report.sh` and `cargo test --workspace` pass
- [x] `scripts/coverage.sh --require-target` passes for every remaining runtime crate

Acceptance evidence (2026-11-04, offline, sandbox tools required):
- `cargo tree -p ngteams --prefix none` contains no removed fence crates or `proto-lab`; the final stale-reference
  audit of `crates/ngteams` is empty. Dedicated HTTP, token/authz and bidirectional Trouter wire tests passed.
- `cargo check -p ngteams --release --locked`, final workspace all-target Clippy with `-D warnings`, lint-report,
  runner tests, formatting and whitespace checks passed.
- The actual full `cargo test --workspace --locked --no-fail-fast -- --test-threads 1` run found four stale
  assertion targets after the earlier removals. All four corrected targets passed their reruns (52 tests), and
  the serialized full workspace doctest rerun passed. A subsequent complete instrumented runtime run passed
  every test. The original ordinary-run exit 101 is retained in `target/task12-workspace-tests.status`.
- The first strict all-crate coverage export passed every runtime test and exposed five coverage gaps. Added
  behavioral scenario, team-dialog, account-path, call-floor and sandbox-control tests; strict coverage of those
  five crates then passed with no runtime failures. The other twelve crates passed against the original complete
  export, with all 189 source-file hashes unchanged. No target, baseline or runtime-source exclusion was lowered.
  The added tracker mapping review covers only a verified `cfg(test)` module absent from LLVM's test-file export.
- Exact counts and unrounded percentages are retained in `target/task12-final-coverage.json`; original exports are
  in `target/coverage/continuous/all/` and `target/coverage/continuous/ngteams+proto-lab+teams-core+teams-ui+ui-sandbox/`.

| Runtime crate | Lines % | Functions % | Regions % |
| --- | ---: | ---: | ---: |
| fixture-guard | 95.883 | 91.603 | 92.181 |
| ngteams | 92.819 | 90.909 | 90.060 |
| proto-lab | 93.870 | 92.743 | 90.179 |
| teams-auth | 97.235 | 97.097 | 94.882 |
| teams-call | 95.523 | 97.052 | 92.473 |
| teams-core | 91.883 | 94.488 | 90.001 |
| teams-log | 95.854 | 96.023 | 94.693 |
| teams-media | 92.147 | 91.789 | 91.293 |
| teams-mock | 94.475 | 94.017 | 93.498 |
| teams-proto | 96.562 | 95.534 | 94.880 |
| teams-signin | 92.385 | 93.548 | 91.856 |
| teams-signin-extension | 100.000 | 100.000 | 99.029 |
| teams-store | 97.912 | 100.000 | 90.085 |
| teams-trouter | 93.111 | 96.000 | 92.095 |
| teams-ui | 92.522 | 90.141 | 91.910 |
| teams-wpe | 93.200 | 93.681 | 93.310 |
| ui-sandbox | 94.493 | 92.040 | 90.437 |

### Task 13: [Final] Update documentation
- [x] README and CLAUDE.md re-read against the final code
- [x] move this plan to `docs/plans/completed/` (the harness performs the move after finalization and stats)

Final reread: README and CLAUDE match the implemented client/protocol CLIs, lab runner controls and lifetime
locks, native provisioning paths, credential-store scopes, build markers, diagnostics, read-only behavior and
coverage rules. No further documentation correction was needed. All 18 local Markdown file/anchor links in
those two files resolve, and the runner's read-only `--help` matches its documented controls. No runtime build,
live run or repeated acceptance test was needed for this final documentation check.

## Upstream integration acceptance (separate from Task 12)

Integration combines validated plan tip `950e032a06632dd0d8651c85a761a33eb70bdf69` with incoming main tip
`9a45dde92a15155f2399bb047473b99d829fdacc` in a cumulative two-parent merge. Task 12's figures above certify
only the earlier plan tip. Fresh integrated checks and exact coverage figures are recorded here when complete.
No live tenant, personal installation, user credentials or personal display are used for these checks.

The selected plan's agent reach rule supersedes incoming global roster admission. Intrinsic authenticated actor,
request target, owner, direct grant, service audience, role, schema and call lifecycle predicates remain. Browser
call tooling compares fresh observed participants with the scenario's explicitly declared intended participants;
normal chat operations and native clients have no global lab roster guard. The canonical `teams-call` tracker
owns call context and connection-bound responses; no parallel lab call registry remains.

| Incoming commit | Preserved behavior and integrated evidence destination |
| --- | --- |
| `0f6fc73` | Canonical `.agents/skills/task-dispatch`, Python status server/tests and `.claude` symlink; editing the skill does not activate dispatch. |
| `b776918` | English shared dispatch instructions and examples; status Python and shell checks. |
| `9ad1b68` | People search click opens the intended chat; `teams-ui/tests/search.rs`. |
| `0e1f042` | Independent actual browser identity/selected-thread evidence scoped to declared call scenarios; `proto-lab/src/action_targets/tests.rs` and scenario DSL tests. |
| `1dd8fe3` | Intended call conversation is checked with the action; `proto-lab` surviving `ab_call` scenario/control tests. |
| `2c63629` | Complete fresh recipients compared to declared participants for each call action; shared pure `teams-proto::chat_members` parser and browser evidence tests. |
| `047b071` | Actual selected-call/visible-control assertions retained in surviving web scenarios; obsolete app/native/mixed clients remain deleted. |
| `75e1ff7` | Dropped files → staged compose → Core/outbox → private own-drive upload/direct grants → message links; attachment transport/Core tests, `attachment_window`, sandbox input and moved tour steps. |
| `65c5eac` | Draft text/files retained while sends are pending and after failure; Core attachment and UI compose/window tests. |
| `e33ea63` | Seen-message events update unread store/badges; Core sync and UI chat-list tests. |
| `ac0281a` | Actor/call/connection/callback/end binding retained in canonical tracker, Core floor and Trouter; tracker and typed-response lifecycle regressions replace policy registry tests. |
| `e39da18` | Native channel/workflow foundation has real HttpTransport methods and UI/Core consumers; protocol HTTP, Core team/channel and UI window tests. |
| `326928a` | Workflow lifecycle, permissions, access, connections and run/history states; workflow protocol and UI form/window tests. Read-only, first-party and version checks retained. |
| `84c7369` | Actual authenticated creator/actor evidence remains in retained browser/native tooling and attachment owner/recipient binding; obsolete client fixture entrypoints remain deleted. |
| `726e335` | Caption writes and real media receive → Core → UI, language/reset/end behavior and recorder actor/audience/scope checks; protocol captions, Core calls/captions, media and call UI tests. |
| `00d4291` | Channel menu/dialog → Core context → typed transport/outbox, including freshness/privacy/role checks and reconciliation; channel/team protocol/Core and UI window tests. |
| `de81896` | Correct native service audiences and dialog error/retry/confirmation behavior; actual transport checks and channel workflow/dialog tests. |
| `db7ab91` | Preferences remain account scoped; authenticated `own_account` required. Released migration0017 retained byte for byte, schema16→17 and existing17 opening covered by store tests. |
| `9a45dde` | Validated native trigger inputs, required/type/default/schema and first-party runtime URLs reach actual HTTP execution; workflow protocol and UI inputs/window tests. |

Released migration `0017_channel_management.sql` SHA256:
`7f93be2c0766a8d9e1bdeb3dfa331a2e46b2fe24d891eeaf802b28568f380351` (matches incoming main exactly).

Normal integrated validation (2026-11-04, offline):

- The complete ordinary workspace run finished with status101 in `target/integration-workspace-tests.log`:
  eleven failing targets identified stale policy/diagnostic/schema assertions, missing synthetic actor evidence,
  asynchronous seek-status ordering, and channel-list/favorite-row integration defects. The original result is
  retained. All affected targets passed their final reruns; producer packages and fixture guard also passed a
  complete rerun, including compile contracts and real isolated Chrome scenarios.
- Channel selection now distinguishes local notifications from completed successful snapshots; an unlisted new
  channel survives until it has actually appeared in the list, and later server removal still closes it. Favorite
  channels retain their rich row and share the management menu with an explicit opening-row anchor, so duplicate
  visible locations show exactly one popup. Existing opening/search assertions and new lifecycle/menu regressions
  pass. Views still hand domain actions to Core; an all-UI audit rejects concrete HTTP request/transport builders.
- Final focused Core, media-player, store-search, protocol/call/action-evidence and ten UI targets passed. The
  143-step native UI tour and normal `cargo check -p ngteams --release --locked --offline` passed. Current themed
  attachment/channel/workflow captures, clean management/workflow tour shots and the actual rich-favorite popup
  were inspected in the sandbox; no blocking visual defect was found.
- The existing narrow 640px Calls settings layout still wraps device labels; its settings sources are unchanged
  from the validated plan tip. Compare `target/ui-shots/ngteams-call-settings.png` with the clean 1280px
  `target/ui-shots/tour-settings-calls.png`. Widening the window is the current workaround; a later layout fix can
  stack device labels above their pickers at narrow widths. This pre-existing minor defect does not block this plan.

All 17 current runtime crates pass the strict 90% line, function and region gates on the integrated source.
The complete counts and full precision percentages are in `target/integration-final-runtime-summary.json`;
rounded percentages and per-crate evidence provenance follow. These are fresh integrated measurements, not the
historical Task 12 results.

| Runtime crate | Lines % | Functions % | Regions % | Evidence freeze |
| --- | ---: | ---: | ---: | --- |
| `fixture-guard` | 95.8834 | 91.6031 | 92.1807 | freeze01 |
| `ngteams` | 92.8188 | 90.9091 | 90.0596 | freeze03 |
| `proto-lab` | 93.8102 | 92.5076 | 90.1370 | freeze01 |
| `teams-auth` | 97.2350 | 97.0968 | 94.8823 | freeze01 |
| `teams-call` | 95.1432 | 96.5863 | 92.3719 | freeze01 |
| `teams-core` | 92.3992 | 93.6364 | 90.3747 | freeze03 |
| `teams-log` | 95.8545 | 96.0227 | 94.6931 | freeze01 |
| `teams-media` | 92.3568 | 91.9807 | 91.5115 | freeze01 |
| `teams-mock` | 94.5012 | 94.0845 | 93.5421 | freeze01 |
| `teams-proto` | 95.2213 | 94.4635 | 93.4425 | freeze01 |
| `teams-signin` | 92.3854 | 93.5484 | 91.8560 | freeze01 |
| `teams-signin-extension` | 100.0000 | 100.0000 | 99.0291 | freeze01 |
| `teams-store` | 97.9314 | 100.0000 | 90.1629 | freeze02 |
| `teams-trouter` | 93.0831 | 96.0000 | 92.1500 | freeze01 |
| `teams-ui` | 92.4991 | 90.5158 | 91.9362 | freeze03 |
| `teams-wpe` | 93.2001 | 93.6809 | 93.3099 | freeze01 |
| `ui-sandbox` | 94.3376 | 91.8317 | 90.4056 | freeze02 |

The final source inventory has 1,169 hashed source/test/build/configuration files, plus an archived source copy
(`target/integration-source-freeze-03.{json,zip}`). The frozen final Core/UI/native-consumer run completed with
status 0: `scripts/coverage.sh --require-target --webrtc-cache <preserved-sdk> -p ngteams -p teams-core -p teams-ui`,
with four build jobs, incremental compilation disabled and Cargo offline. Every runtime target passed, including
all 36 real native call cases, all 26 channel/workflow administration cases, native accounts/call/main windows,
the 143-step tour and all UI suites. The LLVM export and 197 profiles are retained in `target/integration-coverage-03/`.

Evidence reuse is scoped by actual source and dependency closure, not averages or a synthetic union of counters:

- Twelve other runtime crates retain the fresh integrated full-run measurements from freeze01, individually
  re-gated with status 0. They have no Core/UI production dependency or changed source/build/support inputs.
- Store and Sandbox retain the passing runtime tests and strict measurements from freeze02, individually
  re-gated with status 0. Only two external Core test files changed afterward.
- `target/integration-preserved-14-{closure,summary}.json` records these proofs and exact per-crate provenance.
  The earlier closure reconstructs the freeze01 Core source hash by removing only two appended functions inside
  its existing `cfg(test)` module, and reconstructs the coverage-policy hash by restoring only a test-file audit
  hash. Runtime production bytes, thresholds, exclusions and shared inputs did not change between measurements.
  All 53,045 reused native SDK include/library payload files also match the preserved reference byte for byte.
- The complete first integrated run/export/profiles remain in `target/integration-coverage-01/` with status 101:
  every target except the Sandbox audio test passed. Freeze02 then passed every runtime test, including audio,
  but retained report status 1 for Core/UI metric gaps. Its four-crate selection omitted the native `ngteams`
  consumer tests; measured deltas exposed that missing reach. The final three-crate run restores those real
  windows/call/tour observations and adds measured Core domain/media lifecycle cases. Both prior results remain
  available rather than being rewritten as acceptance.

The audio correction is confined to the external test estimator. A captured failed synthetic WAV shows the
expected 48,000 periodic tone samples plus 128 inserted zero samples, which shift phase: a coherent 500 ms
component reads 0.438645 while the same complete half-second's mean of all fifty 10 ms components reads 0.497271.
The stimulus remains one second at 1 kHz/0.5, the tolerance remains 0.5 ± 0.05, silence counts in every average,
and the 1,500 ms recording wait is unchanged. Regression cases reject silence, another frequency, a weak 0.44
tone, 10 ms/200 ms fragments and sustained 20% dropouts; complete windows also reject the old partial-tail false
positive. Fresh instrumentation passes the original microphone, camera, echo delay/attenuation and session-canary
assertions. The original 0.277992 failure has no retained WAV, so a shared cause is plausible rather than proven;
this amplitude contract does not prove uninterrupted or lossless playback. Diagnostic WAV/log/analysis and the
independent signed-off discussion are retained in `target/integration-audio-*` and the run-state discussion archive.

Final normal gates also pass on this source: workspace all-target Clippy with warnings denied, workspace
all-target check, formatting and lint report, plus `cargo check -p ngteams --release --locked --offline`
(`target/integration-*-pre03.{log,status}`). The lint report retains 21 existing complexity exceptions and adds
none. The full producer/fixture/typed-compile-contract/isolated-Chrome checks and Python/shell/vendor checks above
cover unchanged inputs. All frozen hashes still match after the final instrumented run.


## Post-Completion
*Needs the user or the live tenant; no checkboxes*

**New lab accounts:**
- The user provides new lab accounts. They go into `lab.toml`, and their browser profiles are signed in (`proto-lab
  login`) and given native tokens (`proto-lab auth`).
- After that, the old accounts' data may be removed: `~/.local/share/teams-lab/profiles/{a,b,c}`, their `teams-lab`
  keyring items, their stores and their `lab.toml` entries.

**First live smoke run (agent, after its rule check):**
- The check: the 1:1 chat of a and b, and a call in it, reach only lab accounts.
- a and b in `run-lab.sh --pipewire`: a messages b and b sees it; a calls b and b answers, with audio and video both
  ways; then hang up.

**The user's planning rules:**
- `.claude/planning-rules.md` still describes the fence as built and settled. The user may want to rewrite it.

**Install:**
- `scripts/install-personal.sh` only when the user asks.
