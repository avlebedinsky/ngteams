# Native Teams for Linux — M1: Headless Chat Core

## Overview
- M1 builds the chat client without a UI: our own Rust code signs a lab account in, reads its chats, sends, edits,
  deletes and reacts to messages, receives them over Teams' push channel (Trouter), keeps everything in a local SQLite
  store, and catches up after a disconnect. The GPUI client (M2) is built on top of it.
- Roadmap line (from the M0 plan): `teams-store` (SQLite), `teams-trouter`, chatsvc (conversations, messages,
  send/edit/delete/react, rich text, mentions, attachments), an outbox with `clientmessageid` dedupe, catch-up sync,
  `teams-mock`. **Exit:** A→B delivery in under 2 s, covering edits, reactions and a 3-person group chat, automated.
  The group-chat part depends on Task 8 finding evidence that a group chat's member read is complete; if it doesn't,
  the user decides the exit (Decisions for the user).
- **Every write goes through the fence.** The native client has no browser fence around it, so its protection is the
  type-level one from M0: every request that others could notice is a sealed `teams_proto::Mutating` type sent only
  with a `WritePermit` from `Fence::check`, every other non-read request is on a reviewed own-state list, and the
  transport can send nothing else. Sign-in and token renewal (Entra and authz POSTs in `teams-auth`'s own clients) are
  the one reviewed exception, as in M0's `proto-lab auth`: they touch only the account's own credentials. The Trouter
  socket sends only sealed, reviewed own-state frames (authenticate, keep-alive), each logged to `fence.log` before it
  is sent; deliveries stay unanswered unless Task 14 shows that harms the client and the user approves answering them.
  Writes reach only the 3 lab accounts.
- The first tasks close what M0 left open and M1 depends on: a scenario verdict that means pass/fail (the
  expected-deny list, and whether an arming timeout under load is a real breach), whether the chat service takes
  **our** `ic3` token, and token renewal.
- Out of scope: channels and teams (the lab accounts share tenant teams with people outside the lab, so channel writes
  stay unmapped and denied), meetings, calls (the call follow-ups stay in `docs/backlog/fence-call-follow-ups.md`
  for M3), setting presence (a lab account showing online while a client runs is accepted: the user's decision of
  2026-10-19, confirmed 2026-10-20), Graph, fetching or
  sending attachments beyond what Task 20 decides, any UI.

## Context (from M0)
- M0 is done (`docs/plans/completed/20261019-native-teams-m0-lab-and-spikes.md`, `docs/m0-acceptance.md`). What exists:
  - `teams-fence`: `Fence::open_lab`, `WritePermit`, `fence.log` (`Entry` for decisions, `RequestRecord` with
    `Channel::{Http, WebSocket}` and `RequestOutcome::{Own, Unclassified, SentWithoutAllow, Forced}`; appends are
    serialized within one process only); membership sources `FailClosed` and `LiveMembershipSource` (1:1 chats only:
    `Complete` when the thread read lists exactly the two members the chat id names; group/meeting chats, channels,
    teams and calls are `Partial(Unsupported)`). `MembershipSource` and `Mutating` are sealed; `fence-seal` proves it.
    `LiveMembershipSource::with_mock_origin` exists only under `test-support`. `ChatCredential` is built only by
    `from_request(url, headers)` today. `LabPaths` lives in `teams-fence::lab`.
  - `teams-proto`: `Mutating` (sealed, no wire form yet), `Authorized`, `Client::write`, the `Transport` trait
    (`deliver` returns `()`; no live transport yet); requests `SendMessage`, `StartCall`, `AddToCall`. No features.
  - `teams-auth`: PKCE, `nativeclient` redirect, code exchange bound to the ID token's account, refresh,
    `AuthzClient` (`ExplicitLogin`/`TokenRefresh`), `Region` (GTMs), `SecretStore` (`KeyringSecretStore`).
    `proto-lab auth` stores each lab account's refresh token and skypetoken under application `teams-lab`
    (`proto_lab::commands::auth::KEYRING_APPLICATION`) and the region in `region_file` (`<data_dir>/auth/<alias>.json`).
  - `proto-lab`: the armed browser fence, captures, export to local reference copies, `Classifier::mapped` (the chat
    service's writes on chats), scenarios `load`, `ab-chat`, `echo-call`, the DSL `Driver`, `ForcedDeny`. Its
    `calling` classifier lets the web client's Trouter session frames out as `OwnKind::TrouterSession`, the client's own
    `user.activity` included.
  - `spikes/echo-call`: an incoming-call policy (ignored, never answered) and the Trouter/registrar shapes the calling
    stack used.
- What M0 learned that shapes M1:
  - **The chat service takes `Authorization: Bearer` with an Entra token for `https://ic3.teams.office.com`**
    (plus `X-MS-Migration: True` and `BehaviorOverride: redirectAs404`), not a skypetoken; no `RegistrationToken` was
    ever sent. The captured token was the web client's CAE one; whether the service takes **our** non-CAE `ic3` token
    is open (`docs/protocol/auth.md` question 8).
  - **A skypetoken expires with the Entra token it was exchanged from** (authz copies its `exp`): ours lasts ~2 h, so
    renewal is "refresh `api.spaces.skype.com`, then authz with `TokenRefresh`". The skypetoken is still needed for a
    few middle-tier calls, `/trap/tokens` and the AMS exchanges (images).
  - **Trouter authenticates in its first frame**: a `user.authenticate` event carrying `Authorization: Bearer <ic3>`
    and `X-MS-Migration: True`; the WebSocket handshake carries no credential. Endpoints register with the registrar
    (`POST <teams>/registrar/prod/V2/registrations`, Trouter transports); the messaging stack as
    `TeamsCDLWebWorker`. Responses to the service's deliveries stay unknown: which ones only acknowledge isn't
    established, and when they are denied the service redelivers (`docs/protocol/calling-echo.md`).
  - Chat base: GTM `chatServiceAfd` (`<AFD host>/api/chatsvc/amer`); no region hop was seen (question 9). GTM values
    are kept as returned and may carry stray whitespace.
  - Writes as the web client sends them: chat create (`POST v1/threads`, members in the body), message
    (`RichText/Html`), typing, read receipt (`consumptionhorizon`). Edit, delete, reactions, member changes and reply
    chains are mapped from the bundle but **not exercised**; group-chat reads and `thread_members` paging **not seen**,
    and nothing static says whether the plain thread read lists every member (`docs/protocol/fence-targets.md`).
  - The web client's chat list came through CSA (`chatsAndChannelsByIds`, a POST the fence denied); no chat-service
    conversation-list read was seen.
  - A failed send stays unsent in the web client and **goes out again on a later load**: our outbox must never resend
    on its own across runs, and a scenario whose send is denied must cover that message with a forced deny for good.
  - `run ab-chat` reaches its goal and passes live enforcement but **exits 1** on ~450–575 unreviewed fail-closed denies
    (`docs/backlog/ab-chat-exit-code-counts-unreviewed-denies.md`).
  - Captures never enter the repo; fixtures are synthetic-from-capture with a `_synthetic` marker; the sanitizer has
    known gaps (`docs/backlog/sanitizer-defence-in-depth-known-gaps.md`).
- Lab: a, b, c licensed (Microsoft 365 Business Basic), `status` ok, skypetokens for all three (region `amer`,
  partition `amer03`). The echo service is approved in `lab.toml` but not needed in M1.

## Development Approach
- **testing approach**: TDD (tests first)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional - they are a required part of the checklist
  - write unit tests for new functions/methods
  - write unit tests for modified functions/methods
  - add new test cases for new code paths
  - update existing test cases if behavior changes
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** - no exceptions
- **CRITICAL: every task ends with the full gate** (`cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  --all-features -- -D warnings`, `scripts/lint-report.sh`, `cargo test --workspace`); "the gate passes" in a task's
  last checkbox means all four
- **CRITICAL: update this plan file when scope changes during implementation**
- run tests after each change
- **every write goes through the fence**: a sealed `Mutating` type, a `WritePermit` from `Fence::check` for its exact
  target, a fresh membership read. A request that isn't a read, a reviewed own-state request or a permitted write can't
  be sent. Anything unknown, partial or paginated is denied; a new kind of write is mapped only from a capture, and a
  response shape is handled only once a capture showed it.
- **test-only reach stays behind `test-support`**: a mock origin for any transport or socket exists only under a
  `test-support` feature that enables `teams-fence/test-support`, so `teams_fence::test_support_enabled()` and a lab
  binary's refusal to run cover it; `fence-seal` proves a normal build can't set it.
- **captures stay outside the repo**; fixtures are written synthetic-from-capture and pass
  `cargo test -p fixture-guard`. Each fixture's source (which capture or reference copy gave its shape) is named in
  the protocol note that describes it.
- live runs come from a normal build (`cargo build -p <crate>`), headless, one browser per lab profile at a time; a
  native lab binary takes its fence from `Fence::open_lab` and its tokens from the Secret Service (`teams-lab`). No
  write ever comes from a `cargo test` process: a live test only spawns a normally built binary and reads its output
  and `fence.log`. `cargo test` rebuilds a package's binaries into `target/debug/` with `test-support` unified in, so a
  live test's binary is built into its own target dir (`--target-dir target/lab`) and passed by path.
- **a breached live run** (any live browser run in this plan) is handled as CLAUDE.md says: keep its printed output,
  check it and `fence.log` for anything sent unguarded, report it as breached with a link to the backlog item (until
  Task 2 settles it), and report a later clean run next to it, never in its place.
- protocol notes go to `docs/protocol/` in our own words; Microsoft JS is never copied.

## Testing Strategy
- **unit / replay**: `cargo test --workspace`, offline. HTTP on `wiremock`; whole-client tests against `teams-mock`
  (chat service + Trouter replayed from synthetic fixtures), two native clients talking through it, reached through
  the `test-support` mock origin.
- **fence suite**: as in M0, plus the native request policy (compile-fail cases: no request sent without a permit or
  an own-state kind, no Trouter frame sent that isn't a reviewed own-state frame, no mock origin in a normal build),
  the group-chat membership read and concurrent `fence.log` appends from several processes.
- **fixture guard**: `cargo test -p fixture-guard` on every `tests/fixtures/` and `docs/`.
- **live lab**: `--features live` tests that run only with `PROTO_LAB_LIVE=1`, plus `proto-lab run …` and the
  `teams-headless` lab binary against a, b and c. Live tests open no fence of their own and send nothing themselves.
- **latency**: the live delivery test measures send → receive per message and reports p50/p95/max over N = 50.

## Progress Tracking
- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

```
teams-headless (lab CLI) ──commands──▶ teams-core ──▶ teams-auth (tokens) / teams-proto (chatsvc) / teams-trouter
                                          │  ▲                     │ every write: Fence::check → WritePermit
                                          ▼  │ events              ▼
                                     teams-store (SQLite) ◀── catch-up sync + Trouter deliveries
teams-fence (roster, fence.log, live membership)      teams-mock (chat service + Trouter replay, tests only)
```

- **`teams-auth` session**: one refresh token per account; access tokens per audience in memory, refreshed before
  expiry; the skypetoken renewed after its Entra token. Sign-in and renewal POSTs stay in `teams-auth`'s own clients
  (the reviewed exception). Which layer retries what is fixed in Technical Details.
- **`teams-proto`**: the live HTTP transport. Requests come in three sealed classes: `Read` (GET), `OwnState` (a
  reviewed list of the account's own-state requests, logged to `fence.log`), `Mutating` (only as `Authorized`, logged
  by `Fence::check`, with a sealed wire form). Fixed host allowlist, no redirects, no proxy, `https` only; a mock
  origin only under `test-support`.
- **`teams-trouter`**: socket.io 0.9 over WSS, `user.authenticate` in the first frame, keep-alives, the registrar
  registration of the messaging stack only, deliveries turned into events. Its socket sends only `OwnFrame`s
  (authenticate, keep-alive, anything else only once reviewed), each logged to `fence.log` first. Deliveries are not
  answered; a `DeliveryResponse` exists only if Task 14 shows unanswered chat deliveries harm the client and the user
  approves (that revises the M0 rule that delivery responses stay denied). Incoming calls are never answered;
  `user.activity` is sent only if Task 14 shows the session needs it (the account showing online is accepted).
- **`teams-store`**: conversations, members, messages with versions (edits, deletes), reactions, read horizons, sync
  state, the outbox.
- **`teams-core`**: commands → outbox → fence → transport; events (Trouter, sync) → store; catch-up on start and after
  every reconnect.
- **`teams-mock`**: a local chat service and Trouter that replay synthetic fixtures and route one client's writes to the
  others' sockets, so the whole client is tested offline.

## Technical Details
- **Chat requests** (base = GTM `chatServiceAfd`): headers as the web client sends them (`Authorization: Bearer <ic3>`,
  `X-MS-Migration: True`, `BehaviorOverride: redirectAs404`; the exact set fixed by Task 3: the service took our
  non-CAE token in every combination of the two extra headers, so the web client's full set stays). A 404 with `Location` (a
  region hop, never seen) is an error logged by status only, not followed, until a capture shows its shape; following
  it later must move the fence's `ChatCredential` base with the reads and writes.
- **Message post** (`POST v1/users/ME/conversations/<id>/messages`): `content` (HTML), `messagetype` `RichText/Html`,
  `contenttype` `Text`, `clientmessageid` (a random numeric string, kept across retries), `imdisplayname`,
  `properties` (`mentions`, `files`, …). Answers 201 with `OriginalArrivalTime` and the message id in `Location`;
  `Transport::deliver` returns that answer.
- **Targets**: every chat write is `Conversation(<id>)`; a message's mentioned MRIs are added participants, so a
  mention of anyone outside the chat's lab members is denied; chat create is `New` with the body's members.
- **Retries, by layer**:
  - transport (reads and own-state): 401 → the session refreshes, one retry; 429/503 → wait `Retry-After` (capped),
    then one retry; other errors are returned.
  - session (Entra, authz): 429/503 → `TokenError::Throttled` with `Retry-After`, no hidden loop.
  - outbox (writes): each attempt is a fresh `Fence::check`; 401 → refresh, then one new attempt; 429/503 → a new
    attempt after `Retry-After`; any other 4xx → failed, no retry; a network error or timeout → sync the chat first and
    look for the row's `clientmessageid`; found → done, not found → a new attempt with the same `clientmessageid`
    (whether the service dedupes by it is unverified, so the check comes first).
- **Store**: SQLite through `rusqlite` (bundled) unless Task 11 finds a reason otherwise; one file per account under
  `<data_dir>/store/<alias>.sqlite` (`0600`), outside the repo; schema versioned by migrations.
- **Lab binary**: `crates/teams-headless`, a normal build only (`test-support` builds refuse to run, as `proto-lab`).
- **Lab paths**: the keyring application name and the region file move from `proto-lab` to `teams-fence::lab`
  (`LabPaths`), so `teams-headless` and the `teams-auth` session don't depend on `proto-lab`. The a–b–c group chat id
  is kept under the lab's data dir (`<data_dir>/chats.toml`), never in the repo.

## What Goes Where
- **Implementation Steps**: code, tests and docs in this repo, plus lab commands the agent runs on its own.
- **Post-Completion**: steps that need a human or a decision.

## Implementation Steps

### Task 1: An expected-deny list for scenario verdicts
**Files:**
- Modify: `crates/proto-lab/src/scenario/{mod.rs,ab_chat.rs,echo_call.rs}`
- Create: `crates/proto-lab/src/scenario/expected.rs`, `crates/proto-lab/tests/expected_denies.rs`
- Delete: `docs/backlog/ab-chat-exit-code-counts-unreviewed-denies.md` (when done)

- [x] tests first: a reviewed **expected-deny** list (host + method + path pattern; frames by socket host) changes
      only how a run is judged, never a verdict; a request on it is still denied and logged
- [x] tests first: still unexpected, whatever the list says: any deny on a mapped target (the a–b chat), any deny
      whose cause isn't `Unclassified` (partial or refused membership), any class not on the list. The only
      exceptions are the two forced denies `ab_chat::deny_probes` sets up, each told apart by its rule's label: this
      run's enforcement probe (`THIS_RUN_PROBE`) and an earlier run's probe the web client sends again on load (the
      `PROBE_MARKER` rule, given a label of its own). Any other forced deny and every ordinary deny of a mapped write
      stay unexpected, and enforcement evidence still counts only `THIS_RUN_PROBE`
- [x] build the list from the classes in the backlog item, each entry with the reason the scenario doesn't need it;
      test it against those classes and against a mapped write that must stay unexpected
      (also on it: CSA's `chatsAndChannelsByIds`, a chat-list read by `POST` denied in nearly every run and listed in
      `docs/protocol/fence-targets.md`, though the backlog item didn't name it; the captured writes on a tenant team's
      thread stay off the list)
- [x] `ab-chat` and `echo-call` use it (`echo-call` keeps failing on the call follow-ups: they are needed, not
      expected)
- [x] `cargo test -p proto-lab` and the gate pass; delete the backlog item

### Task 2: Is an arming timeout under load a real breach?
**Files:**
- Modify: `crates/proto-lab/src/arming.rs`, `crates/proto-lab/tests/browser.rs`, `crates/teams-fence/src/log.rs`,
  `crates/teams-fence/tests/log.rs` (or the crate's existing log tests)
- Delete: `docs/backlog/arming-timeout-under-load-counts-as-breach.md` (when done)

- [x] settle the backlog item before any live run of this plan relies on a breach verdict: tests first (Chrome 155,
      a page whose held iframe sends a request and a frame as soon as it runs) on what happens to a target held by
      `waitForDebuggerOnStart` whose arming gave up before the resume and whose close failed or went unanswered
      (`tests/browser.rs::a_held_target_runs_only_once_its_parent_s_auto_attach_ends_or_its_connection_drops`: a held
      cross-site iframe or dedicated worker sends nothing when left alone, when its close is refused — Chrome 155
      refuses to close a dedicated worker —, when its session is detached or its parent navigates; it runs once its
      parent's auto-attach is turned off or the connection holding it drops. Closing a held iframe closes its whole tab.
      A held out-of-process iframe isn't listed by `Target.getTargets`, so that can't tell held from gone)
- [x] tests first: `fence.log` gains an arming-failure record (target type, failed step, whether it was the resume,
      close result), written by `teams-fence` like `RequestRecord`; the capture records the same
      (`teams_fence::ArmingRecord`: also the cause, whether the target attached held, and the containment; verdict
      `breach` or `contained`)
- [x] then, by that evidence, either make an unconfirmed close before the resume a warning (re-checked with
      `Target.getTargets`) or confirm the close (retry, or kill the browser). Nothing is loosened without that
      evidence; decide with Codex if it is ambiguous
      (decided with Codex: **kill the browser**. `getTargets` gives no "still held" signal, so no re-check; a close
      not confirmed within `STEP_TIMEOUT` makes the fence `SIGKILL` the browser's whole process group while its
      connection still holds the target, then reap it. A held, never-resumed target closed or killed that way is a
      warning, not a breach, and a killed browser fails the run; resume sent, not held at attach, or neither closed
      nor killed (browser already gone, connection lost first, kill or reap failed) stays a breach. Closing the fence
      now waits for armings too)
- [x] live: `proto-lab run ab-chat --headless` exits 0 with goal, enforcement, no breach and lab-only writes as
      before (with Task 1's list); record the run in this plan
      (2026-10-20 19:11, load average ~32, `target/lab` normal build: exit 0; goal reached, enforcement passed (probe
      denied and failed in a's page, b's copy has the message and not the probe), no breach, no arming failure; a 329
      and b 232 denies, all expected; `fence.log`: the only mapped allows are 2 messages and 4 typing on the a–b 1:1
      chat, both of whose members are lab accounts; 10 forced denies)
- [x] `cargo test -p proto-lab -p teams-fence` and the gate pass; delete the backlog item

### Task 3: Does the chat service take our own `ic3` token? (read-only)
**Files:**
- Create: `crates/proto-lab/src/commands/probe.rs`, `crates/proto-lab/tests/probe.rs`
- Modify: `crates/proto-lab/src/{main.rs,commands.rs}`, `crates/teams-fence/src/membership.rs` (a `ChatCredential`
  constructor from a bearer token and a base), `docs/protocol/auth.md`

- [x] tests first: `ChatCredential::bearer(base, token)` accepts only a base `teams_fence::chat_service_path` accepts;
      its `Debug` prints no token
- [x] tests first (wiremock): `proto-lab probe chat <alias>` refreshes the account's `ic3` token from the stored
      refresh token and sends only `GET`s to the chat base from the account's region GTMs: the chat service's own
      conversation-list read (one page; the path from the bundle mapping, recorded in `chat.md` by Task 7) and the
      fence's 1:1 thread read through `LiveMembershipSource` with that `ChatCredential`; it prints statuses, header
      names and token claims only (aud, exp, scopes, tenant), never a token or a body
      (the bundle's `myconversations` read: `GET v1/users/ME/conversations?view=msnp24Equivalent&pageSize=200&
      startTime=1&targetType=All`, made once per header form; the 1:1 chat is the one with `--with`, b for a and a for
      b and c by default; a test reaches the mock only through `Origin::Mock`, refused without `test-support`)
- [x] `--save <name>` writes the GET bodies as local reference copies under `~/.local/share/teams-lab/reference/`,
      through the same path checks as `export` (refuses any destination inside a git work tree)
      (the conversation-list answers, through the new `export::export_events`: the same sanitizer, name check, guard
      and path checks as `export`; the thread read's body stays inside `LiveMembershipSource`, which returns only a
      verdict)
- [x] live, a and b: with and without `X-MS-Migration`, with and without `BehaviorOverride`; record which form the
      service accepts; save the conversation-list page as a reference copy for Task 7
      (2026-10-20, `target/lab` normal build, `proto-lab probe chat a --save chat/probe-conversations-a` and the same
      for b: our non-CAE `ic3` token (~2 h, no `xms_cc`) got 200 on the conversation list in **all four** forms, and
      the fence's thread read of the a–b chat was complete, for both accounts; no 401, no `WWW-Authenticate`, no
      `Location`. Reference copies `reference/chat/probe-conversations-{a,b}.jsonl`, the export's checks found
      nothing. The native client keeps the web client's form: `Authorization: Bearer <ic3>`, `X-MS-Migration: True`,
      `BehaviorOverride: redirectAs404`)
- [x] if it refuses a non-CAE token (a 401 naming a protocol or a claims challenge): decide with Codex between
      advertising CAE (`xms_cc` `CP1` on the refresh grant plus claims-challenge handling) and the non-CAE audience
      `https://ic3-non-cae.teams.office.com`; record the decision here and in `auth.md` question 8
      (not needed: the service took the non-CAE token; neither CAE nor the non-CAE audience, recorded in `auth.md`)
- [x] answer `auth.md` question 8 and the claims-challenge item; note what was seen of a region hop
- [x] `cargo test -p proto-lab -p teams-fence` and the gate pass

### Task 4: `teams-auth` session: per-audience tokens and renewal
**Files:**
- Create: `crates/teams-auth/src/session.rs`, `crates/teams-auth/tests/session.rs`
- Modify: `crates/teams-auth/src/{lib.rs,authz.rs,token.rs}`, `crates/teams-fence/src/lab.rs` (keyring application
  name and region file path on `LabPaths`), `crates/proto-lab/src/commands/auth.rs` (uses them)

- [x] move `KEYRING_APPLICATION` and `region_file` to `teams-fence::lab::LabPaths`; `proto-lab auth` and `status` keep
      working (their tests unchanged)
      (`LabPaths::KEYRING_APPLICATION`, `LabPaths::region_file`; `proto-lab`'s names stay as thin aliases of them, and
      `spikes/echo-call` uses `LabPaths` too)
- [x] tests first (wiremock, synthetic fixtures, a fake clock): an access token per audience, cached in memory,
      refreshed a margin before `exp`; a rotated refresh token is stored; concurrent callers share one refresh
- [x] the skypetoken is renewed only after refreshing `api.spaces.skype.com` (`AuthzKind::TokenRefresh`), never
      outlives that token, and a renewal with an old Entra token is refused by our code before it is sent
- [x] the session's part of the retry table (Technical Details): a forced refresh on request (for a caller's 401); a
      claims challenge (if Task 3 enabled CAE) passed to Entra up to 3 times; 429/503 → `TokenError::Throttled` with
      `Retry-After`, no hidden retry loop
      (`Session::refresh_rejected` and `Session::skypetoken_rejected`; no claims-challenge handling, since Task 3 left
      CAE off; authz's 429/503 during a renewal are `Throttled` too)
- [x] a renewal notification callers can subscribe to (Trouter re-authenticates on it, Task 16)
- [x] the region GTMs come from the stored `Region` (trimmed), with a fallback for absent names
      (`Region::base`: `chatServiceAfd` → `chatService`, `mtImageService` → `middleTier`; `proto-lab probe` uses it)
- [x] tokens never reach `Debug`, errors or logs (tests check every fixture secret)
- [x] `cargo test -p teams-auth -p teams-fence -p proto-lab` and the gate pass

### Task 5: Native request policy and the HTTP transport
**Files:**
- Create: `crates/teams-proto/src/{http.rs,read.rs,own.rs}`, `crates/teams-proto/tests/{http.rs,policy.rs}`,
  `crates/fence-seal/tests/ui/{proto_mock_origin.rs,proto_own_state.rs,proto_raw_request.rs}` (+ `.stderr`)
- Modify: `crates/teams-proto/{Cargo.toml,src/lib.rs,src/client.rs,src/write.rs}`, `crates/fence-seal/Cargo.toml`
  (`teams-proto` as a dependency, without `test-support`), `crates/fence-seal/tests/*`, `Cargo.toml` (workspace deps)

- [x] tests first (trybuild in `fence-seal`): the transport sends only `Read` (GET), `OwnState` (a sealed, reviewed
      list) or `Authorized<Mutating>`; a non-GET request of any other kind doesn't compile, and `OwnState` can't be
      implemented outside `teams-proto`
      (`Transport` has exactly `read`, `own_state` and `deliver`; a `Read` names only a `Service` and an `Endpoint`, so
      any type may implement it and it is always a `GET` with no body; `proto_raw_request` shows a post shaped as a
      read is accepted neither as an own-state request nor as a write, and that the test-only `ProbeOwnState` is
      absent from a normal build)
- [x] `Mutating` gains a sealed wire form (method, path relative to the chat base, body), and `Transport::deliver`
      returns the answer (status, the headers a write needs such as `Location`, a capped body)
      (`Wire`, made only by the sealed supertrait; `SendMessage` posts `content`, `messagetype` `RichText/Html`,
      `contenttype` `Text` for now, Tasks 12–13 complete the body; `StartCall`/`AddToCall` have no native route
      (`HttpError::NoRoute`) before M3. `Response`: status, `Location`, `Retry-After`, `Content-Type`, body up to
      `MAX_BODY` (4 MiB); its `Debug` shows the body's length only)
- [x] the HTTP transport (reqwest): fixed host allowlist (chat service hosts as `teams_fence::chat_service_path`,
      registrar, Trouter), no redirects, no proxy, `https` only, timeouts, body caps; per-service credentials from the
      Task 4 session; the transport's part of the retry table. Entra and authz are not on it (they stay in
      `teams-auth`'s own clients); AMS isn't either (nothing in M1 fetches from it unless Task 20 adds it)
      (`HttpTransport<C: Credentials>`, `Credentials` implemented for `teams_auth::session::Session`; chat service:
      `Bearer <ic3>`, `X-MS-Migration`, `BehaviorOverride`; registrar on the chat base's AFD host: `Bearer <ic3>`,
      `X-MS-Migration` (from the echo-call capture, recorded in `auth.md`); Trouter's HTTP side: no credential. Reads and
      own-state: one retry in all, after a 401 (the session refreshes the refused token) or a 429/503 (`Retry-After`,
      capped at 30 s, 1 s without one); a write's answer comes back whatever its status, never resent, a 401 refreshes
      the token for the outbox's next attempt)
- [x] tests first: `teams-proto` gets a `test-support` feature (`["teams-fence/test-support"]`) and only under it a
      mock origin (plain `http` on loopback) for the transport; a `fence-seal` case proves a normal build can't set it
- [x] every `OwnState` request is logged to `fence.log` (channel, method, URL without query) before it is sent; one
      that can't be logged isn't sent
      (each send, a retry included, as a `RequestRecord` with `RequestOutcome::Own(<kind>)`; the URL logged is the one
      built and checked, never the mock's)
- [x] the own-state list starts empty; each entry is added only by the task that captured and reviewed it, in its
      exact shape (method, origin, path, body schema)
- [x] tests on wiremock (through the mock origin): reads, a permitted write and its returned answer, an own-state
      request, 401/429 handling, the refusals; mutation-check the log rule
      (mutation-checked: sending an own-state request whose log failed, and logging it after the send, each fail the
      tests)
- [x] `cargo test -p teams-proto -p fence-seal` and the gate pass
      (no new workspace dependency was needed; `teams_auth::token::retry_after` became public for the transport)

### Task 6: The fixture guard's passcode gaps before M1's fixtures
**Files:**
- Modify: `crates/fixture-guard/src/labels.rs`, `crates/fixture-guard/tests/fixture_guard.rs`,
  `docs/backlog/sanitizer-defence-in-depth-known-gaps.md`

- [x] the M1 risk: Tasks 7–8 and 14 write many hand-made chat fixtures from reference copies full of message text,
      and the guard is what catches a value copied by hand. The guard's two label gaps are fixed; the sanitizer's own
      gaps stay in the backlog item (`worth: later`, they only affect local reference copies)
- [x] tests first: a digit-prefixed passcode under a non-English label; a German `Kenncode`/`Besprechungskenncode`
      invitation without a join link; both flagged by the guard
- [x] fix them as the backlog item proposes (passcode labels try the run first; `Kenncode` and `Besprechungskenncode`
      under `de`, with the comment's attribution corrected)
- [x] remove those two gaps from the backlog item, keep the rest
- [x] `cargo test -p fixture-guard` and the gate pass (parallel run: fmt, `cargo clippy -p fixture-guard
      --all-targets`, `scripts/lint-report.sh`, `cargo test -p fixture-guard` and proto-lab's sanitize tests here; the
      workspace-wide clippy and test run after the merge)

### Task 7: Capture the 1:1 chat protocol beyond messages
**Files:**
- Create: `crates/proto-lab/src/scenario/ab_chat_full.rs`, `docs/protocol/chat.md`,
  `tests/fixtures/chatsvc/{conversations,messages,edit,delete,reaction,mention,reply}.json` (synthetic)
- Modify: `crates/proto-lab/src/{classify.rs,scenario/mod.rs}`, `crates/proto-lab/tests/fence_targets.rs`,
  `docs/protocol/fence-targets.md`

- [x] a scenario `ab-chat-full` (the `ab-chat` fence, its forced-deny probe first, the Task 1 list): a sends, edits,
      reacts (add, remove), mentions b, replies with a quote, deletes; b reads and reacts; every write is on the a–b
      chat
      ⚠️ no mention: under the fence the mention picker offers nobody but a message extension (b shows as "Unknown
      User": the profile lookups behind it are denied `POST`s). The scenario does no mention; its wire form is the
      bundle's, marked not captured (`docs/backlog/mention-not-captured-under-the-fence.md`, settled with Codex). a
      also reacts to its reply and leaves the like, so b's read shows a reaction with its user.
- [x] tests first for any mapping row the capture corrects (edit, delete, reaction, read receipt); a shape not in the
      table stays `Unknown` — the capture matched every row; corrected: an edit is the whole message again, so the
      mapping now needs its body (a message `messagetype`), and a message's or edit's `properties.mentions` MRIs are
      added participants (any other `mentions` form is `Unknown`)
- [x] from the capture (read locally, exported to reference copies only) and Task 3's reference copies: the
      conversation list and its paging/sync links (from the probe if the web client only used CSA), message pages and
      `syncState`, message shapes (versions and `skypeeditedid`, deletes, `emotions`, `mentions` and the mention
      markup, quotes, `amsreferences`/`files` of received attachments), into `docs/protocol/chat.md`, each shape with
      its source; tick or correct the open rows of `fence-targets.md` — mentions from the bundle only; no received
      attachment was seen (all `amsreferences` `[]`, `files` `"[]"`); the group-chat row stays Task 8's
- [x] synthetic fixtures of the same shapes, marked `_synthetic`; `cargo test -p fixture-guard` passes
- [x] live run recorded in this plan (a breached run as in Development Approach); `cargo test -p proto-lab` and the
      gate pass (parallel run: crate-scoped gate; workspace gate after merge)
      Live runs of 2026-10-20 (`proto-lab run ab-chat-full --headless`, normal build, under the lab's live lock;
      none breached, every allowed write on the a–b chat, no lab browser left):
      - 19:41Z (the first agent's): enforcement passed; send, edit, react, unreact, quoted reply and delete done; the
        mention failed (picker, above); b read the history and reacted, sent no read receipt; exit 1 on two bot
        invokes. The shapes in `chat.md` come mostly from this run.
      - 20:20Z: failed before enforcement (a deep link reloaded the page under the compose-box wait); only denies.
        The DSL's waits now go on across a navigation (`dsl::navigated_away`).
      - 20:30Z: enforcement passed; the edit box rebuilt its element and lost the scenario's mark, the next steps
        typed into the open edit box (a garbled edit on the lab chat), reply/delete/b's read failed; exit 1. The edit
        now types into the focus (`rewrite_editor`) and cancels a box it leaves open.
      - 20:51Z (confirming): every step done (a: send, edit, react, unreact, reply, react to the reply, delete; b:
        read, react; b's receipt came during its reaction); exit 1 only on nine denied `isread` puts on a's own
        activity feed (`48:notifications`), reviewed afterwards and added to the expected-deny list with the
        `composeExtension/fetchCommands` bot invokes (both stay denied).
      - 21:37Z (after the review fixes: b's probe check, case-folded optional keys, `crossPostChannels`/`onbehalfof`
        only in their captured forms): **exit 0**, `ab-chat-full: ok`. Enforcement passed on both sides (a's probe
        denied and failed in a's page; b's copy of the chat had a's message and not the probe); every step done (a:
        send, edit, react, unreact, reply, react to the reply, delete; b: read, react; b sent no read receipt within
        30 s). `fence.log`: 26 allowed writes, all on the a–b chat (6 message, 4 typing, 2 edit, 2 delete, 8
        reaction, 4 read receipt), 20 own Entra token posts, 13 forced denies (the probes), 888 unclassified denies,
        all expected by the reviewed list; no breach. The web client's posts and edits kept `crossPostChannels: []`
        and `onbehalfof: null` (none fell to `Unknown`). No lab browser left.

      📝 The mention, after the user's decision of 2026-10-21 (side work on branch `m1-task-7-profile-reads`): loosen
      the browser fence for the mention picker's people lookups, which only read. Under the user's rule that a denied
      request may be loosened only when nothing it does or names leaves the a–b–c lab, the fence got a reviewed
      read-by-`POST` list, a class of its own (`crates/proto-lab/src/post_read.rs`, `fence.log` `post_read`): the
      middle tier's `users/fetchShortProfile` and `users/fetch` in their captured shape (query, headers, a JSON list of
      MRIs), let through only when every MRI is a lab account of the roster, on `Classifier::mapped` (and so
      `calling`), asked after the write mapping. Substrate's people search and the bot invokes stay denied
      (`docs/backlog/bot-invokes-and-message-extensions.md`). `ab-chat-full` mentions b again: `@`, the picker's
      highlighted suggestion checked to be b by its object id, b's name typed, checked again, Enter; the post's
      `properties.mentions` must name b, and b's read must show it. Live runs (normal build, headless, under the live
      lock, no breach, no lab browser left):
      - 2026-10-20 22:07Z: the picker offered b (the profile lookups passed: 26 `post_read` allows, every answer naming
        only the requested lab account), but the step's own name read threw in the page (a newline in its script);
        every other step done but b's read, which waited for the mention; exit 1.
      - 22:32Z: **the mention captured**, every step done (a: send, edit, react, unreact, mention, reply, react to the
        reply, delete; b: read with the mention, react). `fence.log` (entries; each request is decided on two
        sessions): every allowed write on the a–b chat (8 message, 2 of them the mention with b added, 6 typing, 2
        edit, 2 delete, 8 reaction, 4 read receipt), 2 `post_read` allows (`user_fetch` of a lab account; the client
        had the rest cached), the one lookup naming a bot denied; exit 1 only on b marking its own `48:mentions` item read
        (`isread`), reviewed and added to the expected-deny list (still denied). The mention's wire form is in
        `docs/protocol/chat.md` and `tests/fixtures/chatsvc/mention.json` (rewritten from this capture); the
        classifier's mention rule already read that form.
      - 22:40Z: a's chat never opened from its deep link within 180 s (machine under load, an export running); failed
        before enforcement, only denies.
      - A confirming run after the `48:mentions` review queued on the live lock from 22:56Z and was withdrawn at
        00:50Z, never started: another agent's `ab-image` run held the lock the whole time. ⚠️ So no exit-0 run with
        the mention yet; the next live run of `ab-chat-full` is that confirmation.
      - 2026-10-21 01:42Z (confirming, after the review fixes of `m1-task-7-profile-reads-r1`: a `PausedRequest` keeps
        header values only for those the list reads, never a token or cookie; the lookup's `x-ms-object-id` pinned to
        its captured empty value, `x-ms-migration`, `x-ms-test-user`, `x-ms-client-request-type`, `x-ms-client-type`
        to theirs, `x-ms-client-caller` a short name with no object id in it): **exit 0**, every step done (a: send,
        edit, react, unreact, mention (the picker offered b, picked, the post's `properties.mentions` named b), reply,
        react to the reply, delete; b: read, react; no read receipt within 30 s), the probe denied in a's page and never
        at b. `fence.log` (903 entries): 36 allowed writes, all on the a–b chat (8 message, 2 of them the mention with
        b added, 6 typing, 2 edit, 2 delete, 8 reaction, 10 read receipt), 4 own Entra token posts, 15 forced denies
        (the probes), 848 unclassified denies, all expected by the reviewed list; no breach, no lab browser left. No
        `post_read` allow this time: the client had the lab accounts' profiles cached, and the one lookup it sent named
        a bot and was denied (its headers carried every pinned value as captured).

### Task 8: Group chats under the fence (a, b, c)
**Files:**
- Modify: `crates/teams-fence/src/membership.rs`, `crates/teams-fence/tests/membership.rs`,
  `crates/proto-lab/src/scenario/` (a new `abc-chat` scenario), `docs/protocol/fence-targets.md`
- Create: `tests/fixtures/chatsvc/thread-group*.json` (synthetic)

- [x] live, reads only plus one allowed `ChatCreate` (`New`, members a, b, c): a creates the a–b–c group chat in the
      web client. The web client creates the chat when its first message is sent, and that message is denied
      (`Partial(Unsupported)` until this task's read exists) and would be resent on every later load: the scenario puts
      a `ForcedDeny` with a label of its own on a first-message marker, kept for every later run on these profiles, and
      expects it. The new chat's id goes to `<data_dir>/chats.toml`, never the repo
      (2026-10-20, `target/lab` normal build, load average ~50–95: `proto-lab run abc-chat --headless --timeout 400`
      exit 0, no breach, no arming failure. a opened the web client straight at `l/chat/0/0?users=<b>,<c>`; the fence
      allowed `POST v1/threads` as `ChatCreate` with the three lab accounts added (`fence.log`: `allow chat_create new
      added 3`, from the browser and the page session); the first message, marker `lab fence probe abc-chat first
      message ` (it begins with `ab-chat`'s probe marker, so `ab-chat` and `abc-chat` force-deny it when it is resent;
      anywhere else the fence denies it as a group chat write), was force-denied under its own label and failed in a's page; the id (`19:<32 hex>@thread.v2`) is in
      `chats.toml` (`0600`, `teams_fence::lab::{LabChats, record_abc_chat}`). Two earlier attempts failed before
      anything was sent, no breach: the deep link, navigated to after the first load, reloads the whole client, which
      didn't finish within 60 s and then 180 s on the loaded machine; the scenario now loads each account straight at
      its deep link, and its load waits take `--timeout`)
- [x] capture the thread read, `thread_members` (paged form), `filtered`, member counts and roster versions for that
      chat as a, b and c see it
      (new read-only `proto-lab probe thread <alias> [--save <name>]`: GETs with the account's own `ic3` token of the
      thread, the batch read, the member list at `pageSize` 100 and 1 (next pages followed only on the same list, on
      the chat base), the filtered view and the own view, then the fence's own read; prints key names and counts only.
      Live for a, b and c, reference copies `reference/chat/thread-group-{a,b,c}.jsonl`. All three saw the same: the
      thread read lists a, b, c with no count and no paging key; the member list the same three with
      `totalMemberCount` 3 and no next link at `pageSize` 1 too (no paging form seen); `rosterVersion` = `eTag` on the
      thread, 0 on the list, the `eTag`s equal; the filtered view 403 `AclCheckFailed` for every member. Written up in
      `fence-targets.md`, synthetic fixtures `tests/fixtures/chatsvc/thread-group{,-members,-filtered}.json`)
- [x] decide with Codex what makes a group chat's member read **complete** (the evidence the 1:1 id gives for free:
      not a filtered view, not a page); record the decision and the evidence here and in `fence-targets.md`. Meeting
      chats, channels and any chat the evidence doesn't cover stay `Partial(Unsupported)`
      (decided with Codex: **not sufficient**. The candidate paired read (thread read and member list unpaged, the same
      distinct members, `totalMemberCount` equal to them, equal `eTag`s, `threadType` `chat`) shows the endpoints agree,
      not that what they agree on is the whole membership: one caller-visible subset, listed and counted by both, fits
      the same answers, and a 3-member roster every viewer sees alike can't tell them apart; no paging form was seen;
      the `eTag`s' meaning across endpoints is unknown. A read-only look at a large tenant thread would show only the
      paging form, for another thread template and with other people's member lists, so it wasn't done. Transcript
      `~/.local/state/plan-exec-revmux/20261020-native-teams-m1-headless-chat-core/discussions/task-8-group-complete/`)
- [x] ⚠️ stop condition: if the evidence isn't sufficient, group chats stay `Partial(Unsupported)`, the finding is filed
      to `docs/backlog/`, and the M1 exit's group-chat part goes to the user (Decisions for the user). Until they
      answer, later tasks build and test group chats on the mock only, and Task 21 runs the 1:1 part
      (⚠️ **hit**: group chats stay `Partial(Unsupported)`; filed as `docs/backlog/group-chat-member-read-completeness.md`;
      the M1 exit's group-chat part is a pending decision for the user, below. Meanwhile later tasks use the mock for
      group chats and Task 21 runs the 1:1 part)
- [x] tests first (wiremock, synthetic fixtures): complete only with that evidence; paging, a count above the list, a
      filtered marker, a mismatch between the reads → `Partial`; then implement it in `LiveMembershipSource`
      (without the evidence there is no complete case to implement, and Codex and Claude agreed not to land the
      candidate rule unwired. Instead the tests pin the decision against the live shapes: the group fixtures served as
      the service answered, the source still answers `Partial(Unsupported)` without a read and the fence denies a
      message to a lab-only group chat; `LiveMembershipSource::reads_in_full` (only 1:1 chats) and
      `teams_fence::is_group_chat_id` are tested in `crates/teams-fence/tests/membership.rs`, `chats.toml` in
      `tests/lab_config.rs`, the probe in `crates/proto-lab/tests/probe.rs`, the scenario's offline parts in
      `crates/proto-lab/tests/abc_chat.rs`)
- [x] `abc-chat`: a messages the group, b and c show it; enforcement probe first; exit 0 under the Task 1 list; a
      breached run as in Development Approach
      (built: `crates/proto-lab/src/scenario/abc_chat.rs`, its creation part run live, above. The messaging part is
      blocked by the ⚠️ stop condition and was **not run**: while `reads_in_full` says no for a group chat, the run fails
      before it opens a browser, because a's message would be denied and then resent by the web client on every later
      load of a's profile, failing every scenario there. It runs once the user's decision and the evidence allow it.
      Checked live from the normal build with the recorded id: `proto-lab run abc-chat --headless` exits 1 with that
      reason before any browser opens)
- [x] `cargo test -p teams-fence -p fence-seal -p proto-lab` and the gate pass (parallel run: crate-scoped gate;
      workspace gate after merge. `cargo fmt --all --check`, `cargo clippy -p teams-fence -p fence-seal -p proto-lab
      --all-targets -- -D warnings`, `scripts/lint-report.sh`, `cargo test -p teams-fence -p fence-seal -p proto-lab -p
      fixture-guard`, all pinned to 4 cores)

### Task 9: `teams-mock`: the chat service from synthetic fixtures
**Files:**
- Create: `crates/teams-mock/{Cargo.toml,src/lib.rs,src/chatsvc.rs}`, `crates/teams-mock/tests/mock.rs`
- Modify: `Cargo.toml` (member, workspace deps)

- [x] tests first: the mock serves the conversation list, threads (1:1 and group), message pages with sync links, and
      takes message posts, edits, deletes, reactions and read receipts, queuing each as an event for the other members
      (Task 16 adds the Trouter side that delivers them); it checks the headers Task 3 found and answers 401 without
      them
      (`ChatServiceMock::start()` on wiremock with one catch-all responder over its state; extra modules
      `fixtures.rs`, `world.rs`, `reads.rs`, `writes.rs`. World: a, b, c and an outsider x; the a–b chat with
      `messages.json`'s messages, empty a–c and a–x chats, the a–b–c group chat of `thread-group.json`, and a's
      channel and notifications stream (read-only). Reads: the list and its sync link (it names the caller's MRI),
      first page, sync and backward links (`backwardLink`, the bundle's name; the mock's link tokens are its own),
      the read around a time, thread read, member list, filtered view (403). Writes: posts (201 with
      `OriginalArrivalTime` and a `Location` on the regional host) and typing (201 with `OriginalArrivalTime`), edits, soft deletes, reactions, read receipts,
      and chat creation (`POST v1/threads`, for Task 12), each read back as its fixture recorded; events are typed
      `ChatEvent`s (`take_events(mri)`), the Trouter wire form waiting for Tasks 14/16. Headers: the `ic3` bearer
      token with `X-MS-Migration: True` and `BehaviorOverride: redirectAs404`, else 401; the thread read takes the
      token alone, the form the fence's own read sends and Task 3 saw answered in full. Unmodelled requests 404,
      malformed bodies 400, non-members, read-only conversations and other people's messages 403. Tests: the
      fixtures replayed, each write read back, the fence's live read finding the mock's 1:1 chats complete and
      denying the outsider's, a permitted `SendMessage` through `HttpTransport::with_mock_origin` reaching b)
- [x] everything from synthetic fixtures; no capture is read by a test
      (the fixtures are embedded with `include_str!` from `tests/fixtures/chatsvc/`; a test checks that the crate's
      sources include nothing else and name no capture or lab data path)
- [x] dev-dependency only (never in a lab binary); clients reach it only through the Task 5 `test-support` mock
      origin; `[lints] workspace = true`, listed in `members`
      (`teams-mock` in `[workspace.dependencies]`; a test reads every member's manifest and fails if any takes it
      outside `[dev-dependencies]`, target-specific tables included)
- [x] `cargo test -p teams-mock` and the gate pass

### Task 10: Chat service reads and the message model
**Files:**
- Create: `crates/teams-proto/src/chat/{mod.rs,model.rs,read.rs,html.rs}`, `crates/teams-proto/tests/chat_read.rs`
- Modify: `Cargo.toml` (the HTML parser in `[workspace.dependencies]`), `crates/teams-proto/Cargo.toml`

- [x] tests first on the Task 7 fixtures: conversations (1:1, group; channels read but marked read-only), members,
      messages with versions, edits, deletes, reactions, mentions, quotes, references to received attachments (AMS
      images, file cards; parsed, not fetched); unknown fields kept (`serde(flatten)`), unknown message types as
      `Unknown(String)`, never a parse failure
      (`teams_proto::chat`: `ConversationList`/`MessagePage::from_body`, `Conversation` with `kind()` (`OneToOne`,
      `Group` from `threadType` `chat` and the id as the fence reads it, `Channel` for `space`, `Notifications`,
      `Unknown(threadType)`; all but 1:1 and group read-only, a meeting chat included: its `threadType` wasn't seen),
      `consumption_horizon()`, `last_message()`; `Message` with `sender()`, versions, `edit_time()`/`delete_time()`,
      `reactions()`, `mentions()` (bundle form, not captured), `quotes()`, `files()` and `ams_references` (entries
      kept raw: no received attachment was ever captured), `thread_activity()` (the `AddMember` XML); `Thread`,
      `MemberList`. A property in a form no capture showed is `Field::Unknown(value)`; a list entry that isn't the
      captured shape goes to the page's `unreadable` and the rest is read. Written in one pass with the tests rather
      than strictly tests first)
- [x] rich text: parse the HTML content into a small typed model (text runs, mentions, quotes, links, images); unknown
      markup kept as text; no script, style or event attribute survives
      (`scraper` 0.27.0 without default features (html5ever) and `ego-tree` 0.11.0 in `[workspace.dependencies]`,
      `time` 0.3.55 for the ISO times; `RichText` of `Node::{Text{marks}, Break, Paragraph, Mention, Link, Image,
      Quote}`; links only `http`/`https`/`mailto`, images only `https`; `script`, `style`, `template`, frames,
      `object`/`embed` and the head dropped with their content; walked without recursion, containers capped at
      `MAX_DEPTH` 32, content read up to `MAX_CONTENT` 64 KiB (`truncated`), since html5ever's parse cost grows with
      the square of the nesting depth)
- [x] paging and sync: follow the service's own `backwardLink`/`syncState` links only on the same chat base
      (`ChatReads` over `Client<HttpTransport<_>>`: `conversations`/`conversations_from`, `messages`/`messages_from`,
      `messages_around`, `thread`, `members`. A link is followed only with the transport's own chat base (scheme,
      host, port, base path exactly, no fragment) and the path of the read it was handed out for (the same
      conversation's messages, or the conversation list); else `ChatReadError::Link` and nothing is sent)
- [x] a 404 with `Location` is an error (`RegionMoved`, logged by status only), not followed (tests on the mock)
      (`ChatReadError::RegionMoved { status }` keeps no `Location`; tested on wiremock directly, since `teams-mock`
      has no region hop and was left unchanged; one request reached it)
- [x] `cargo test -p teams-proto` and the gate pass (parallel run: crate-scoped gate; workspace gate after merge:
      `cargo fmt --all --check`, `cargo clippy -p teams-proto --all-targets -- -D warnings`, `scripts/lint-report.sh`,
      `cargo test -p teams-proto -p fence-seal -p fixture-guard`, pinned to 4 cores)

### Task 11: `teams-store` (SQLite)
**Files:**
- Create: `crates/teams-store/{Cargo.toml,src/lib.rs,src/schema.rs,src/migrations/*.sql}`,
  `crates/teams-store/tests/store.rs`
- Modify: `Cargo.toml` (member, `rusqlite` in `[workspace.dependencies]`)

- [x] tests first: migrations from empty; idempotent upserts of conversations, members and messages by id and version
      (an older version never overwrites a newer one); edits, soft deletes, reactions per user, read horizons, sync
      state per conversation; the outbox table (Task 13)
- [x] choose the SQLite crate (`rusqlite` bundled unless a reason shows up), record it here; WAL; the file `0600`
      — chosen: `rusqlite` 0.40.2 with `bundled` (SQLite built from source, the same version everywhere); no reason
      showed up against it. The directory is made `0700`, the file `0600` before SQLite opens it (its `-wal`/`-shm`
      take that mode), and a looser existing file is tightened
- [x] queries the core needs: conversation list by last activity, a page of messages, unread count
- [x] `[lints] workspace = true`, listed in `members`
- [x] `cargo test -p teams-store` and the gate pass (run ahead of Tasks 7–10 and 13 in parallel: fmt, clippy on
      `teams-store`, `lint-report.sh` and `cargo test -p teams-store`; the workspace-wide clippy and test run on the
      merged branch). The row types are the store's own (Tasks 10 and 13 map onto them): extra modules
      `conversations.rs`, `messages.rs`, `state.rs`, `outbox.rs`, `file.rs`

### Task 12: Chat writes as fenced requests
**Files:**
- Modify: `crates/teams-proto/src/write.rs`, `crates/teams-proto/tests/fence_types.rs`, `crates/fence-seal/tests/ui/*`,
  `crates/teams-fence/src/membership.rs`
- Create: `crates/teams-proto/tests/chat_write.rs`

- [x] tests first: `SendMessage` (with mentions), `EditMessage`, `DeleteMessage`, `SetReaction`/`RemoveReaction`,
      `ReadReceipt`, `Typing`, `CreateChat` as sealed `Mutating` types with their wire forms; each target exactly as
      `fence-targets.md` (as corrected by Tasks 7–8); a permit for chat X can't send to chat Y
      (`teams-proto/src/write/chat.rs`, re-exported from `write.rs`; an `Author` (MRI, `imdisplayname`) and the
      times are fields of the request, so its wire form is built from its own fields; `CreateChat::one_to_one` and
      `::group` (three or more members); typing stopped isn't there, not captured. Written in one pass with the tests)
- [x] a message's `content` and `properties.mentions` are both built from one typed rich-text value (Task 10's model);
      raw HTML can't be passed, so a mention can't be in the markup without being in the target; every mentioned MRI
      is an added participant
      (`chat::MessageText`: a `RichText` and the MRI of each mention by its `item_id`, rendered once into `content`,
      `properties.mentions` (renumbered `0`, `1`, … as they appear) and a reply quote's `qtdMsgs`; refused: a mention
      naming nobody, an MRI no mention shows, a mention twice, anyone but an `8:orgid:` account, images (Task 20),
      links but `http`/`https`/`mailto`, an incomplete reply quote, truncated or too deep or too long text. A quote
      adds nobody, as the mapping. The mention form is the bundle's, built only in `mention_span`/`mention_entry`
      (`chat/compose.rs`), to be replaced by a capture (`docs/backlog/mention-not-captured-under-the-fence.md`).
      `fence-seal`'s `proto_raw_html` shows raw HTML doesn't compile. The outbox's `Command::Send` now holds a
      `MessageText` and `Outbox::new` an `Author`; `RichText` and its nodes gained serde for the stored command)
- [x] the fence's live read gets the native client's own credential (`ChatCredential::bearer` from our `ic3` token and
      the region's chat base, the form Task 3 established); without it every write is `Partial(NoCredential)`
      (`teams_fence::CredentialSource`, asked on every read through `LiveMembershipSource::set_credential_source`;
      `HttpTransport` implements it from its session's current `ic3` token and its chat base, so a renewed token is
      used at once; no token is no credential)
- [x] bodies match the captured shapes (Task 7) key for key; tests compare with the synthetic fixtures
      (equal, not only key for key, to `mention.json`, `reply.json`, `delete.json`, `reaction.json` and the
      `writes.json` message, read receipt, typing and new-chat cases; to `edit.json` but for its made-up `version` and
      `edittime`, which aren't one instant there)
- [x] tests on `teams-mock`: each write allowed for a lab chat and sent once; a chat with an outsider (a mock member
      outside the roster) denied, nothing sent, `fence.log` entry written
      (also: a mention of the outsider in the a–b chat and a chat create with them denied; a group chat write denied
      `Partial(Unsupported)` while a new a–b–c group may be created; no credential, or a source without a token,
      `Partial(NoCredential)` with no request at all)
- [x] `cargo test -p teams-proto -p teams-fence -p fence-seal` and the gate pass (parallel run: crate-scoped gate;
      workspace gate after merge: `cargo fmt --all --check`, `cargo clippy -p teams-proto -p teams-fence -p teams-core
      -p teams-mock -p fence-seal --all-targets -- -D warnings`, `scripts/lint-report.sh`, `cargo test` on those
      crates and `fixture-guard`; `proto-lab`'s clippy too, as it builds on the changed API)

### Task 13: Outbox with `clientmessageid` dedupe
**Files:**
- Create: `crates/teams-core/{Cargo.toml,src/lib.rs,src/outbox.rs}`, `crates/teams-core/tests/outbox.rs`
- Modify: `Cargo.toml` (member, workspace deps), `crates/teams-store/src/*`

- [x] create the `teams-core` crate (`[lints] workspace = true`, listed in `members`) with the outbox as its first
      module; sync and the engine come in Tasks 17–18
      (`Command` holds only `Send` for now, the one chat write with a sealed request; Task 12 adds the others. The
      sync is a seam, `ConversationSync`, which Task 17 implements. `SendMessage` gained `with_client_message_id`,
      posted as `clientmessageid`; Task 12 completes the rest of the body. No store change was needed)
- [x] tests first: a command becomes an outbox row with a fresh `clientmessageid`; each attempt asks the fence again
      (a fresh permit, a fresh membership read); a denied attempt marks the row denied and sends nothing
      (`clientmessageid`: a random 19-digit number; a membership that changed between attempts denies the later one)
- [x] the outbox's part of the retry table (Technical Details), each case tested: 401, 429/503 with `Retry-After`,
      other 4xx (no retry), and a timeout whose message did arrive (found by `clientmessageid` in a sync, not sent
      again) or didn't (sent again with the same id)
      (a 5xx other than 503 is handled as a timeout, since it may have been stored; every attempt after the first
      starts with the sync check, whatever ended the one before; a sync that fails sends nothing and leaves the row
      waiting; a row is failed after `MAX_ATTEMPTS` = 5. A kind that can't be looked for in a sync isn't retried
      after an unknown outcome: it is failed. Tested through the mock origin on wiremock, a timeout by a transport
      wrapper that loses the answer or the request)
- [x] dedupe: our own message coming back (Trouter or sync) is matched by `clientmessageid` to its row, never stored
      twice
      (`Outbox::receive` stores the message by id and version and matches it only when we sent it, in the row's
      conversation; a row already sent with a message id keeps it; a message back before the post's answer wins)
- [x] **no resend across runs**: rows left pending by an earlier process are not sent on start; only an explicit
      command resends them (the web client's late resend in M0 is the case this rules out)
      (enforced by the store's runs; `Outbox::send` refuses another run's row before asking the fence. After
      `Store::resend`, a row the earlier run had attempted is looked for in a sync before it goes out again)
- [x] `cargo test -p teams-core` and the gate pass (parallel run: crate-scoped gate; workspace gate after merge:
      fmt, clippy and test on `teams-core`, `teams-proto` and `fence-seal`, `lint-report.sh`, `fixture-guard`)

### Task 14: Trouter: capture and notes for messaging
**Files:**
- Create: `docs/protocol/trouter.md`, `tests/fixtures/trouter/*.json` (synthetic)
- Modify: `crates/proto-lab/src/{calling.rs,classify.rs,fence_ws.rs}` (a messaging own-state set),
  `crates/proto-lab/tests/calling.rs`

- [x] tests first: a messaging own-state set for `proto-lab` (the Trouter frames of an `ab-chat`-style run) that
      allows the registrar registration only as `TeamsCDLWebWorker`, never `SkypeSpacesWeb`, and lets the web
      client's own `user.activity` out (presence showing is accepted); the `calling` set stays as it is
      (`Classifier::messaging`, `FrameClassifier::messaging` and `.without_activity()`, `calling::messaging_request`;
      the scenarios `ab-trouter` and `ab-trouter-quiet` in `scenario/ab_trouter.rs`, judged by the Task 1 list plus
      their own `TROUTER_RUN_REQUESTS`)
- [x] capture (web client, `ab-chat`-style run with that set): b's Trouter session while a sends, edits and reacts;
      the registrar registration of the messaging stack; the deliveries for chat events (method, url, headers, body
      shape); the token renewal on a live socket (re-authenticate or reconnect)
      (⚠️ one gap, recorded in `trouter.md`: the renewal was not capturable live, the web client's CAE `ic3` token
      living ~24 h, so it rests on the bundle: no re-authentication of an open socket, a renewed token takes a
      reconnect. The reaction, missing from runs 1–12 (the step looked for `default-reaction-*`), was captured live
      in run 13 after the review fix, from the hover bar's `message-actions-like`: answered `200`, delivered to b's
      open socket 0.9 s after it went out; the removal and delete shapes still come from an `ab-chat-full` run's
      events replayed to b's endpoints)
- [x] answer, with evidence, whether leaving chat deliveries unanswered harms the client: redelivery volume and timing,
      the socket closed, deliveries throttled or stopped. Decide with Codex; record it in `trouter.md`
      (no operational harm shown: on a stable socket no redelivery in 16 min, no close, throttling or stop; on every
      reconnect a replay of ≤ ~15 min of unanswered chat events (`mstore`, `ttl=900`), absorbed by the catch-up sync
      and idempotent store; decided with Codex, transcript in the run's `discussions/task-14-unanswered-deliveries`)
- [x] if it does harm: decide with Codex which delivery responses only acknowledge a chat event; that becomes a
      decision for the user (it revises the M0 rule that delivery responses stay denied). If it doesn't, deliveries
      stay unanswered and no response path is built. Nothing call-related is ever answered
      (it doesn't: deliveries stay unanswered, no response path, so Task 15's `DeliveryResponse` box is to be struck
      and the user decision after Task 14 doesn't arise)
- [x] `user.activity`: whether the session needs it (does the service treat an endpoint that never sends it
      differently: deliveries, keep-alive, timeouts); if it does, it becomes a reviewed `OwnFrame` entry in its exact
      shape. Record what makes an account show as online (for M2's presence)
      (it doesn't: `ab-trouter-quiet` sockets that never sent it got live deliveries; only the web client's own 10 s
      ack wait made it reconnect. No `OwnFrame` entry. Online presence: the web client's UPS endpoint `PUT` and
      `user.activity` recorded, which of them makes an account show online untested since the fence denies the PUT)
- [x] write `trouter.md` in our own words: connect and regional move, `user.authenticate` in the first frame (the
      `ic3` token, `X-MS-Migration`), keep-alives, `trouter.connected` push URLs, registration and its TTL,
      deliveries, reconnect parameters, token renewal, presence
- [x] synthetic fixtures; `cargo test -p fixture-guard` passes; live run recorded (a breached run as in Development
      Approach)
      (`tests/fixtures/trouter/{session,registration,deliveries}.json`. Live, 2026-10-20 19:09–21:37 UTC, 12 runs of
      `ab-trouter[-quiet]`, table in `trouter.md`: **3 breached** (runs 2, 7: an iframe's resume unanswered within
      10 s; run 4: two `browser_ui` targets' auto-attach unanswered; captures and `fence.log` show nothing sent
      unguarded), 2 contained, 3 failed on a's compose box under load; runs 5 and 9–12 clean: message delivered to b
      by push in every one, the edit in 5 and 10–12, no unexpected deny after run 5. None exited 0 (the reaction
      step). Every allowed write was on the a–b 1:1 chat. After the review fixes, run 13 (2026-10-20 22:14 UTC):
      **exit 0**, no breach, message, edit and reaction delivered live to b, 16 min observed without a redelivery;
      `fence.log`: allowed writes only a message, typing, an edit and a reaction on the a–b chat)
- [x] `cargo test -p proto-lab` and the gate pass (parallel run: crate-scoped gate; workspace gate after merge)

### Task 15: The Trouter frame policy
**Files:**
- Create: `crates/teams-proto/src/frame.rs`, `crates/teams-proto/tests/frame.rs`,
  `crates/fence-seal/tests/ui/{proto_raw_frame.rs,proto_own_frame.rs}` (+ `.stderr`)
  (➕ `proto_frame_literal.rs`)
- Modify: `crates/teams-proto/src/lib.rs`, `crates/teams-fence/src/{log.rs,target.rs}`,
  `crates/teams-fence/tests/*` (➕ `docs/protocol/trouter.md`, `tests/fixtures/trouter/session.json`)

- [x] tests first (trybuild in `fence-seal`): a socket sink sends only `OwnFrame` (a sealed, reviewed list:
      `user.authenticate`, keep-alives, the entries Task 14 reviewed); an unreviewed frame (raw text, `user.activity`,
      a response built by hand) doesn't compile, and `OwnFrame` can't be implemented or constructed outside
      `teams-proto`
      (`FrameSink<W: FrameWriter>` with only `send<F: OwnFrame>`; the list: `Authenticate` (`user.authenticate`, the
      messaging stack's three headers, the seven `connectparams` when there are some), `Ping`, `MessageLossAck`
      (`trouter.processed_message_loss`, never an empty list) and `Disconnect` (`0::`), the Trouter session frames
      Task 14 reviewed, without `user.activity`; the sink numbers acknowledged events itself; the frame's text is a
      `FrameText` only the sink makes, its `Debug` the length only. `proto_raw_frame`: raw text, `user.activity`, a
      hand-built delivery answer, a `FrameText` made outside; `proto_own_frame`: implementing `OwnFrame`; a third case,
      `proto_frame_literal`: a reviewed frame built by struct literal (rustc reports that privacy error only in a file
      with no other error). The fixture's first-socket `user.authenticate` gained the `X-Ms-Test-User` header every
      messaging-stack socket sent in the captures)
- [x] tests first: `fence.log` gains a frame record (channel `WebSocket`, frame kind, never a token); every outgoing
      frame is logged before it is sent; one that can't be logged isn't sent
      (`FrameRecord` / `FenceLog::record_frame`: `{"verdict":"allow","event":"frame","channel":"web_socket","url":<the
      socket without its query>,"own":<kind>}`; mutation-checked: logging after the send, and sending when the log
      failed, each fail the tests)
- [x] ~~only if Task 14 found harm **and** the user approved answering: a `DeliveryResponse` judged by the fence as a
      new `WriteKind` (a delivery acknowledgement) on `Scope::Conversation`, so `Fence::check` does the membership read
      and logs the decision; built only for a reviewed path on a complete, lab-only conversation; a `fence-seal` case
      proves nothing outside `teams-fence`/`teams-proto` can mint one. Otherwise this checkbox is struck and recorded~~
      (struck: Task 14 found no harm, deliveries stay unanswered, decided with Codex; no `DeliveryResponse`, no new
      `WriteKind`, so `teams-fence/src/target.rs` is unchanged)
- [x] `cargo test -p teams-proto -p teams-fence -p fence-seal` and the gate pass (parallel run: crate-scoped gate;
      workspace gate after merge; `cargo test -p fixture-guard` and `proto-lab`'s `calling` tests too, for the fixture
      change)

### Task 16: `teams-trouter`: the native push channel
**Files:**
- Create: `crates/teams-trouter/{Cargo.toml,src/lib.rs,src/frames.rs,src/session.rs,src/registration.rs}`,
  `crates/teams-trouter/tests/{frames.rs,session.rs}`, `crates/teams-mock/src/trouter.rs`,
  `crates/fence-seal/tests/ui/trouter_mock_origin.rs` (+ `.stderr`)
  (➕ `crates/teams-trouter/src/{events.rs,socket.rs,credentials.rs}`)
- Modify: `Cargo.toml` (member, workspace deps), `crates/teams-mock/{src/lib.rs,tests/mock.rs}`,
  `crates/fence-seal/Cargo.toml` (`teams-trouter`, without `test-support`)
  (➕ `crates/teams-proto/src/{own.rs,http.rs,lib.rs}` and `tests/http.rs`: the registration is a sealed own-state
  entry, so it lives in `teams-proto`, and the registrar gets the messaging stack's `X-Ms-Test-User: False`;
  `crates/teams-mock/{Cargo.toml,src/world.rs,src/chatsvc.rs}`; `crates/fence-seal/tests/ui/{proto_own_state,
  proto_raw_request}.stderr` (rustc now names the one own-state entry); `tests/fixtures/trouter/deliveries.json` (an
  eighth delivery, the account's own read horizon); `docs/protocol/trouter.md`)

- [x] the mock's Trouter endpoint, from the Task 14 fixtures: connect, authenticate, `trouter.connected`, ping,
      deliveries of the chat service mock's queued events
      (`ChatServiceMock::start_trouter` → `TrouterMock`, its own loopback server: `1::`, `user.authenticate` checked
      (late → `timeout`, other headers or token → `unauthorized`), `trouter.connected` from `session.json` on a made-up
      regional host, the message-loss report on a reconnect, pongs, deliveries in `deliveries.json`'s shape only once
      a registration names the endpoint, the delivered events stored and replayed on every later connect (`mstore`);
      read receipts and chat creation stay queued (no capture of their delivery to another member); the registrar
      `POST /registrar/prod/V2/registrations` on the chat mock's origin (202 for the messaging stack's body, 400 else,
      401 without the token); test hooks: socket records, drop, disconnect with a reason, an incoming call. The HTTP
      mock's API and answers are unchanged)
- [x] `[lints] workspace = true`, listed in `members`; a `test-support` feature (`["teams-proto/test-support"]`) and
      only under it a mock origin (`ws` on loopback); a `fence-seal` case proves a normal build can't set it
      (`Trouter::with_mock_origin`; `fence-seal/tests/ui/trouter_mock_origin.rs`)
- [x] tests first: socket.io 0.9 frame codec on the Task 14 fixtures; authenticate as the first frame; acknowledged
      keep-alives; reconnect with backoff and the service's reconnect parameters; a delivery for an incoming call is
      **ignored, never answered** (the M0 echo-call policy), and so is every other delivery unless Task 15 built
      responses
      (Task 15 built none: every delivery is unanswered. `tests/frames.rs`: every session frame, `trouter.connected`,
      the loss report, all eight deliveries, the host allowlist. `tests/session.rs`: the first frame is
      `user.authenticate` with the token and no `connectparams`, the connect query, pings answered, a missing pong →
      reconnect, a dropped socket → regional reconnect with the `connectparams`, the same endpoint, the loss report
      acknowledged after `caught_up`, the stored replay; `unauthorized` → a new token, `dup` twice → a backoff, a
      refused connect → growing backoff; an incoming call `Ignored`, never answered)
- [x] every outgoing frame goes through the Task 15 policy and is logged to `fence.log` first; the socket has no other
      way to send; `user.activity` only if Task 14 reviewed it
      (the write half goes into the `FrameSink` at once, wrapped in the crate-private `SocketWriter`; every session test
      checks that each frame the mock got is a reviewed own frame with its `fence.log` entry, in order; a source test
      checks no other module touches a sink; an unloggable frame isn't sent and stops the client. No `user.activity`:
      Task 14 didn't review it)
- [x] the registrar registration (messaging stack only) and its renewal as `OwnState` requests (Task 5), the `ic3`
      token from the session
      (`teams_proto::EndpointRegistration`, the captured body, `TeamsCDLWebWorker_2.6`; on every `trouter.connected`,
      again 30 s before its 3600 s end, 123 s after a failure; tested: shape and log order in `teams-proto`, retry and
      renewal on a flaky registrar, registration on the mock)
- [x] token renewal: on the session's renewal notification the socket re-authenticates or reconnects (as Task 14
      showed) before the old token expires; tested with a fake clock on the mock
      (reconnects, as Task 14 showed: 5 min before expiry by the clock, checked every 30 s, and at once on the session's
      announcement of a newer `ic3` token; tested with a fake clock on fake tokens and on the real `Session` over a mock
      Entra)
- [x] deliveries → typed chat events (new message, edit, delete, reaction, read horizon, thread update); unknown ones
      kept as `Unknown` and logged by kind only
      (`ChatEvent::{Message, Typing, MessageUpdate{Edit|Delete|Reaction|Other}, ReadHorizon, ConversationUpdate,
      Unknown}`, told apart as runs 12–13's captures showed, `trouter.md`)
- [x] tests on `teams-mock`: connect, register, receive a delivery, send no answer (or, if built, answer only a
      reviewed one on a lab-only chat), survive a dropped socket
- [x] `cargo test -p teams-trouter -p teams-mock -p fence-seal` and the gate pass (parallel run: crate-scoped gate;
      workspace gate after merge; `teams-proto`, `fixture-guard` and `proto-lab`'s `calling` tests too, for the
      own-state entry and the fixture)

### Task 17: Catch-up sync in `teams-core`
**Files:**
- Create: `crates/teams-core/src/sync.rs`, `crates/teams-core/tests/sync.rs`
- Modify: `crates/teams-core/{Cargo.toml,src/lib.rs}`

- [x] tests first (on `teams-mock`): first sync fills the store; a sync after a gap (socket down, messages sent
      meanwhile) catches up exactly once; Trouter events and sync agree (no duplicates, versions respected); a
      reconnect triggers a catch-up
      (`teams_core::sync::CatchUp`: the conversation list (whole, then its `syncState` link), each new or changed
      conversation upserted after its messages synced, its read horizon and its members (thread read when the list's
      `rosterVersion` is newer); the list's link moves on only when every conversation synced. Messages: the newest page
      first (its `backwardLink` kept, `load_older` pages back), then the conversation's sync link; a sync answer's own
      `backwardLink` (never captured) is followed while its pages bring anything new, up to `MAX_PAGES`, the rest left
      as older history. Everything goes through `receive_message` → `Outbox::receive`, the path a Trouter delivery
      takes; a message whose `version` can't be read (unknown or missing) isn't stored, a reaction set with an
      unreadable time isn't merged. `Gaps` says when a catch-up is due (on start, after every drop of the push
      channel); the engine (Task 18) wires it to Trouter's connects, which Task 16 builds. Deliveries are played by
      the mock's queued events' resources; plain wiremock for the shapes the mock doesn't serve)
- [x] the outbox's timeout check (Task 13) uses it to look for a `clientmessageid`
      (`CatchUp` implements `ConversationSync`; tested with a lost answer (found, posted once) and a dropped request
      (sent again with the same id))
- [x] `cargo test -p teams-core` and the gate pass (parallel run: crate-scoped gate; workspace gate after merge:
      fmt, clippy and test on `teams-core`, `lint-report.sh`, pinned to 4 cores)

### Task 18: The `teams-core` engine
**Files:**
- Create: `crates/teams-core/src/engine.rs`, `crates/teams-core/tests/engine.rs`
- Modify: `crates/teams-core/src/lib.rs`

- [ ] the engine: commands (send, edit, delete, react, read, create chat) → outbox → fence → transport; events → store;
      tokio on its own runtime, reachable through channels (the shape M2's GPUI bridge needs)
- [ ] tests first, two native clients on the mock: a sends, b receives through its Trouter socket; edit and reaction
      arrive; a group of three
- [ ] `cargo test -p teams-core` and the gate pass

### Task 19: The `teams-headless` lab binary
**Files:**
- Create: `crates/teams-headless/{Cargo.toml,src/main.rs}`, `crates/teams-headless/tests/cli.rs`
- Modify: `Cargo.toml` (member), `crates/teams-fence/src/log.rs` (cross-process appends), `crates/teams-fence/tests/*`,
  `README.md`

- [ ] ⚠️ prerequisite before the first live run: the user has added permission rules for `teams-headless` like those
      for `proto-lab` (Post-Completion)
- [ ] `[lints] workspace = true`, listed in `members`
- [ ] `teams-headless <alias> sync|list|show <chat>|send <chat> <text>|edit|delete|react|watch`: `Fence::open_lab`,
      tokens from the Secret Service (`teams-lab`, through `LabPaths`), the store under the lab's data dir; refuses to
      run when built with `test-support` (which every mock origin implies); prints no token. `send` prints its start
      time, `OriginalArrivalTime` and message id; `watch --json` prints each received event with its receive time
- [ ] tests first: `fence.log` appends from several processes at once (a, b and c each run their own binary) stay
      whole lines: an advisory lock around each append and around `terminate_last_line`; a test with concurrent
      writer processes
- [ ] live: a sends one message to b in the a–b chat with `send`, b sees it with `watch`; `fence.log` checked
- [ ] `cargo test -p teams-headless -p teams-fence` and the gate pass; `cargo build -p teams-headless` runs `--help`

### Task 20: Attachments: decide sending, then build it or defer it
**Files:**
- Modify: `docs/protocol/chat.md`, `crates/teams-proto/src/chat/*` (if built)

- [x] reading references is in Task 10 (AMS images, file cards); this task is about **sending** (and fetching, which
      it needs)
- [x] capture an image sent by a in the a–b chat in the web client, with the AMS object creation and upload denied
      first (what the client sends), then decide with Codex how an AMS object's target is read (its `permissions`
      naming the conversation) and whether its upload can be bound to it without a registry the fence can't verify;
      a breached run as in Development Approach
      (new scenario `proto-lab run ab-image` (`crates/proto-lab/src/scenario/ab_image.rs`): `ab-chat`'s fence and
      probes, every write to `…/v<n>/objects…` force-denied under its own label from before a's page loads; a pastes a
      made-up PNG the scenario generates (`test_png`) with a caption and sends it; a's page is loaded again and an
      unsent image deleted if shown; b reads the chat. Live, normal build, headless, under the live lock, 2026-10-21:
      - 22:51Z: failed before anything of the image: b's browser was killed to contain a worker whose arming went
        unanswered for 10 s (load ~25; `resume_sent: false`, contained, no breach). a sent nothing but its load-time
        probe resends, force-denied.
      - 22:52Z: the image went through the whole capture (below), then the run **hung for about two hours** and held
        the live lock: reloading a's page, `LabSession::navigate`'s `Page.navigate` was never answered, because the web
        client put up a `beforeunload` dialog for the pending image, which a headless browser shows to nobody and
        which nothing answered; the navigation was the one page call without a bound. Stopped with SIGINT, which
        ended `proto-lab` without dropping its browsers: both lab browsers were left and killed by hand. `fence.log`
        over its whole window: allowed writes only on the a–b chat (the plain message and typing), 16 own Entra
        token posts, no breach, no containment, nothing sent unguarded. Fixed: every navigation is bounded
        (`dsl::navigate_tab`: 90 s for `LabSession::navigate`, the call timeout for `Driver::navigate`) and accepts a
        `beforeunload` dialog holding it after 5 s; `proto-lab` runs each command `until_stopped`, so SIGINT/SIGTERM
        drop it and its browsers (`stop.rs`); tests with a page that asks before it is left and a self-signalled
        test. Live runs since go under the lock with `timeout --signal=INT`.
      - 01:08Z: every step done in 11 min, no breach: enforcement passed (a's probe denied and failed in a's page, b's
        copy without it); the paste took; the create denied 4 times; no caption post; after the reload no retry and
        no pending image left. Exit 1 only on b's six denied `isread` puts on its own mentions feed (`48:mentions`),
        reviewed afterwards and added to the expected-deny list next to the activity feed's (still denied).
      What the client sends (`docs/protocol/chat.md`, "Images"): `POST https://us-prod.asyncgw.teams.microsoft.com/
      v1/objects/` with the `ic3` bearer token and `X-MS-Migration`, body `type` `pish/image`, `permissions`
      `{<a–b chat id>: ["read"]}`, `sharingMode` `Inline`, `filename`; retried with backoff, then pending; no upload,
      no message post. Decided with Codex: the creation's target is readable from `permissions`, but the upload names
      only the object and `forward` can widen an object's readers, so binding it needs an authoritative read of the
      object's readers that was never captured (a create-to-upload handle is provenance, not the current audience),
      and every shape past the creation is uncaptured. Transcript
      `~/.local/state/plan-exec-revmux/20261020-native-teams-m1-headless-chat-core/discussions/task-20-ams-image-sending/`)
- [x] if the evidence holds: put AMS on the transport's allowlist, map it, build `SendImage` as a fenced write, test on
      the mock, run it live in the a–b chat; otherwise file it to `docs/backlog/` and keep sending attachments out of
      M1 (OneDrive files go to M2 with Graph either way)
      (⚠️ the evidence doesn't hold: **deferred**, `docs/backlog/ams-image-sending.md` (the two blockers and the steps
      that would settle them). Nothing on `HttpTransport`'s allowlist, no mapping, no `SendImage`; AMS object writes
      stay `Unknown` everywhere (`fence-targets.md`). `teams-proto` untouched, so nothing conflicts with Task 12)
- [x] tests for whatever is built; the gate passes (parallel run: crate-scoped gate; workspace gate after merge)
      (`crates/proto-lab/tests/ab_image.rs`: the test image, the forced-deny rules, the policy; `scenario_dsl.rs`: the
      paste/drop into a compose box, marking an unsent image, navigations held by a `beforeunload` dialog;
      `tests/stop.rs`; `expected_denies.rs`: the mentions feed read. fmt, `cargo clippy -p proto-lab --all-targets`,
      `scripts/lint-report.sh`, `cargo test -p proto-lab -p fixture-guard`. `scenario_dsl`'s
      `keys_are_pressed_with_their_modifiers` typed before its page was parsed and failed every time under load ~20; it
      now waits for its box. `capture::a_recorded_run_holds_…` failed twice in full runs (`results is not defined`,
      seen by earlier tasks too) and passed solo)

### Task 21: Verify M1 acceptance criteria
**Files:**
- Create: `crates/teams-headless/tests/chat_live.rs` (`required-features = ["live"]`), `docs/m1-acceptance.md`

- [ ] the live test is an orchestrator only: it opens no fence, holds no token and sends nothing. It spawns the
      binary built by `cargo build -p teams-headless --target-dir target/lab` (path from `TEAMS_HEADLESS_BIN`; it
      fails if unset), reads its output and parses `fence.log`. A binary built with `test-support` refuses to run, so
      the test fails rather than writes
- [ ] (`PROTO_LAB_LIVE=1`): native a sends N = 50 messages to b in the a–b chat; native b receives each over Trouter;
      **send → receive p95 under 2 s** (p50/p95/max reported)
- [ ] an edit and a reaction (both arrive on b); a 3-person group chat of a, b and c (each message reaches both
      others), unless the user decided otherwise after Task 8
- [ ] a catch-up check: b's socket closed, a sends, b reconnects and has every message exactly once
- [ ] `fence.log` for the run: allows only on the lab chats, no deny of a mapped write, no own-state request or
      Trouter frame off the list, no delivery response (or, if Task 15 built them, only for lab chats), lines whole
- [ ] `proto-lab run ab-chat --headless` and `abc-chat` exit 0
- [ ] the gate passes; write `docs/m1-acceptance.md` (numbers, fence summary, flakes, any breached run next to its
      clean rerun)

### Task 22: [Final] Update documentation
- [ ] README.md: the M1 crates, `teams-headless`, the new scenarios and `probe`, tests
- [ ] CLAUDE.md: a hard rule for the native client (what the browser-fence rule says for the browser: the request
      and frame policy, no write from a `cargo test` process, mock origins only under `test-support`); patterns
      discovered during M1 (the outbox rule, the retry table, Trouter deliveries and presence)
- [ ] write the M2 plan (GPUI daily driver), informed by M1
- [ ] move this plan to `docs/plans/completed/`

## Validation commands

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/lint-report.sh
PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1 cargo test --workspace
cargo test -p fixture-guard
cargo build -p proto-lab
target/debug/proto-lab run ab-chat --headless
target/debug/proto-lab run abc-chat --headless
PROTO_LAB_LIVE=1 cargo test -p proto-lab --features live --test auth_live
cargo build -p teams-headless --target-dir target/lab
TEAMS_HEADLESS_BIN=target/lab/debug/teams-headless PROTO_LAB_LIVE=1 \
  cargo test -p teams-headless --features live --test chat_live
```

## Roadmap after M1
- **M2 — GPUI daily driver** on `teams-core`: sign-in in an embedded WebKitGTK webview, chat list, chat view and
  compose, notifications, presence, search, teams and channels, files and calendar (Graph), Adaptive Cards, tray.
- **M3 — calls**: first resolve `docs/backlog/fence-call-follow-ups.md` (an authoritative call-membership read), then
  the `sdp-ngc-1.0` conversion and the echo call (`docs/spikes/echo-call.md`), then 1:1 and group media.
- **M4 — meetings**, **M5 — long tail**: as in the M0 plan's roadmap.

## Post-Completion
*Items requiring a human or a decision — informational only.*

**Manual steps:**
- None expected: the lab accounts are licensed and signed in, and `proto-lab auth` keeps their tokens. If Microsoft
  asks a lab account for a one-time code, `login` stops (`docs/backlog/one-time-code-sign-in-under-the-fence.md`) and
  the user is asked.
- **Before Task 19's first live run**: the user adds permission rules for `teams-headless` like those for `proto-lab`.

**Decisions for the user:**
- Approving any new service participant (none planned in M1).
- After Task 8, only if the evidence doesn't show a group chat's member read complete: whether M1 exits on 1:1 chats
  (group chats on the mock only) or waits for more evidence. **Open since Task 8 (2026-10-20): the evidence didn't
  show it** (`docs/backlog/group-chat-member-read-completeness.md`).
- After Task 14, only if unanswered chat deliveries harm the client: whether the native client may answer reviewed
  chat deliveries on lab-only chats (this revises the M0 rule that delivery responses stay denied).
- Before M2: whether the user's own account passes Conditional Access with the native client (M0's open question 6 of
  `docs/protocol/auth.md`); that is the first step of the personal build, which agents never build or run.
