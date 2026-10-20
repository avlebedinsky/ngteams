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
  `X-MS-Migration: True`, `BehaviorOverride: redirectAs404`; the exact set fixed by Task 3). A 404 with `Location` (a
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

- [ ] tests first: a reviewed **expected-deny** list (host + method + path pattern; frames by socket host) changes
      only how a run is judged, never a verdict; a request on it is still denied and logged
- [ ] tests first: still unexpected, whatever the list says: any deny on a mapped target (the a–b chat), any deny
      whose cause isn't `Unclassified` (partial or refused membership), any class not on the list. The only
      exceptions are the two forced denies `ab_chat::deny_probes` sets up, each told apart by its rule's label: this
      run's enforcement probe (`THIS_RUN_PROBE`) and an earlier run's probe the web client sends again on load (the
      `PROBE_MARKER` rule, given a label of its own). Any other forced deny and every ordinary deny of a mapped write
      stay unexpected, and enforcement evidence still counts only `THIS_RUN_PROBE`
- [ ] build the list from the classes in the backlog item, each entry with the reason the scenario doesn't need it;
      test it against those classes and against a mapped write that must stay unexpected
- [ ] `ab-chat` and `echo-call` use it (`echo-call` keeps failing on the call follow-ups: they are needed, not
      expected)
- [ ] `cargo test -p proto-lab` and the gate pass; delete the backlog item

### Task 2: Is an arming timeout under load a real breach?
**Files:**
- Modify: `crates/proto-lab/src/arming.rs`, `crates/proto-lab/tests/browser.rs`, `crates/teams-fence/src/log.rs`,
  `crates/teams-fence/tests/log.rs` (or the crate's existing log tests)
- Delete: `docs/backlog/arming-timeout-under-load-counts-as-breach.md` (when done)

- [ ] settle the backlog item before any live run of this plan relies on a breach verdict: tests first (Chrome 155,
      a page whose held iframe sends a request and a frame as soon as it runs) on what happens to a target held by
      `waitForDebuggerOnStart` whose arming gave up before the resume and whose close failed or went unanswered
- [ ] tests first: `fence.log` gains an arming-failure record (target type, failed step, whether it was the resume,
      close result), written by `teams-fence` like `RequestRecord`; the capture records the same
- [ ] then, by that evidence, either make an unconfirmed close before the resume a warning (re-checked with
      `Target.getTargets`) or confirm the close (retry, or kill the browser). Nothing is loosened without that
      evidence; decide with Codex if it is ambiguous
- [ ] live: `proto-lab run ab-chat --headless` exits 0 with goal, enforcement, no breach and lab-only writes as
      before (with Task 1's list); record the run in this plan
- [ ] `cargo test -p proto-lab -p teams-fence` and the gate pass; delete the backlog item

### Task 3: Does the chat service take our own `ic3` token? (read-only)
**Files:**
- Create: `crates/proto-lab/src/commands/probe.rs`, `crates/proto-lab/tests/probe.rs`
- Modify: `crates/proto-lab/src/{main.rs,commands.rs}`, `crates/teams-fence/src/membership.rs` (a `ChatCredential`
  constructor from a bearer token and a base), `docs/protocol/auth.md`

- [ ] tests first: `ChatCredential::bearer(base, token)` accepts only a base `teams_fence::chat_service_path` accepts;
      its `Debug` prints no token
- [ ] tests first (wiremock): `proto-lab probe chat <alias>` refreshes the account's `ic3` token from the stored
      refresh token and sends only `GET`s to the chat base from the account's region GTMs: the chat service's own
      conversation-list read (one page; the path from the bundle mapping, recorded in `chat.md` by Task 7) and the
      fence's 1:1 thread read through `LiveMembershipSource` with that `ChatCredential`; it prints statuses, header
      names and token claims only (aud, exp, scopes, tenant), never a token or a body
- [ ] `--save <name>` writes the GET bodies as local reference copies under `~/.local/share/teams-lab/reference/`,
      through the same path checks as `export` (refuses any destination inside a git work tree)
- [ ] live, a and b: with and without `X-MS-Migration`, with and without `BehaviorOverride`; record which form the
      service accepts; save the conversation-list page as a reference copy for Task 7
- [ ] if it refuses a non-CAE token (a 401 naming a protocol or a claims challenge): decide with Codex between
      advertising CAE (`xms_cc` `CP1` on the refresh grant plus claims-challenge handling) and the non-CAE audience
      `https://ic3-non-cae.teams.office.com`; record the decision here and in `auth.md` question 8
- [ ] answer `auth.md` question 8 and the claims-challenge item; note what was seen of a region hop
- [ ] `cargo test -p proto-lab -p teams-fence` and the gate pass

### Task 4: `teams-auth` session: per-audience tokens and renewal
**Files:**
- Create: `crates/teams-auth/src/session.rs`, `crates/teams-auth/tests/session.rs`
- Modify: `crates/teams-auth/src/{lib.rs,authz.rs,token.rs}`, `crates/teams-fence/src/lab.rs` (keyring application
  name and region file path on `LabPaths`), `crates/proto-lab/src/commands/auth.rs` (uses them)

- [ ] move `KEYRING_APPLICATION` and `region_file` to `teams-fence::lab::LabPaths`; `proto-lab auth` and `status` keep
      working (their tests unchanged)
- [ ] tests first (wiremock, synthetic fixtures, a fake clock): an access token per audience, cached in memory,
      refreshed a margin before `exp`; a rotated refresh token is stored; concurrent callers share one refresh
- [ ] the skypetoken is renewed only after refreshing `api.spaces.skype.com` (`AuthzKind::TokenRefresh`), never
      outlives that token, and a renewal with an old Entra token is refused by our code before it is sent
- [ ] the session's part of the retry table (Technical Details): a forced refresh on request (for a caller's 401); a
      claims challenge (if Task 3 enabled CAE) passed to Entra up to 3 times; 429/503 → `TokenError::Throttled` with
      `Retry-After`, no hidden retry loop
- [ ] a renewal notification callers can subscribe to (Trouter re-authenticates on it, Task 16)
- [ ] the region GTMs come from the stored `Region` (trimmed), with a fallback for absent names
- [ ] tokens never reach `Debug`, errors or logs (tests check every fixture secret)
- [ ] `cargo test -p teams-auth -p teams-fence -p proto-lab` and the gate pass

### Task 5: Native request policy and the HTTP transport
**Files:**
- Create: `crates/teams-proto/src/{http.rs,read.rs,own.rs}`, `crates/teams-proto/tests/{http.rs,policy.rs}`,
  `crates/fence-seal/tests/ui/{proto_mock_origin.rs,proto_own_state.rs,proto_raw_request.rs}` (+ `.stderr`)
- Modify: `crates/teams-proto/{Cargo.toml,src/lib.rs,src/client.rs,src/write.rs}`, `crates/fence-seal/Cargo.toml`
  (`teams-proto` as a dependency, without `test-support`), `crates/fence-seal/tests/*`, `Cargo.toml` (workspace deps)

- [ ] tests first (trybuild in `fence-seal`): the transport sends only `Read` (GET), `OwnState` (a sealed, reviewed
      list) or `Authorized<Mutating>`; a non-GET request of any other kind doesn't compile, and `OwnState` can't be
      implemented outside `teams-proto`
- [ ] `Mutating` gains a sealed wire form (method, path relative to the chat base, body), and `Transport::deliver`
      returns the answer (status, the headers a write needs such as `Location`, a capped body)
- [ ] the HTTP transport (reqwest): fixed host allowlist (chat service hosts as `teams_fence::chat_service_path`,
      registrar, Trouter), no redirects, no proxy, `https` only, timeouts, body caps; per-service credentials from the
      Task 4 session; the transport's part of the retry table. Entra and authz are not on it (they stay in
      `teams-auth`'s own clients); AMS isn't either (nothing in M1 fetches from it unless Task 20 adds it)
- [ ] tests first: `teams-proto` gets a `test-support` feature (`["teams-fence/test-support"]`) and only under it a
      mock origin (plain `http` on loopback) for the transport; a `fence-seal` case proves a normal build can't set it
- [ ] every `OwnState` request is logged to `fence.log` (channel, method, URL without query) before it is sent; one
      that can't be logged isn't sent
- [ ] the own-state list starts empty; each entry is added only by the task that captured and reviewed it, in its
      exact shape (method, origin, path, body schema)
- [ ] tests on wiremock (through the mock origin): reads, a permitted write and its returned answer, an own-state
      request, 401/429 handling, the refusals; mutation-check the log rule
- [ ] `cargo test -p teams-proto -p fence-seal` and the gate pass

### Task 6: The fixture guard's passcode gaps before M1's fixtures
**Files:**
- Modify: `crates/fixture-guard/src/labels.rs`, `crates/fixture-guard/tests/fixture_guard.rs`,
  `docs/backlog/sanitizer-defence-in-depth-known-gaps.md`

- [ ] the M1 risk: Tasks 7–8 and 14 write many hand-made chat fixtures from reference copies full of message text,
      and the guard is what catches a value copied by hand. The guard's two label gaps are fixed; the sanitizer's own
      gaps stay in the backlog item (`worth: later`, they only affect local reference copies)
- [ ] tests first: a digit-prefixed passcode under a non-English label; a German `Kenncode`/`Besprechungskenncode`
      invitation without a join link; both flagged by the guard
- [ ] fix them as the backlog item proposes (passcode labels try the run first; `Kenncode` and `Besprechungskenncode`
      under `de`, with the comment's attribution corrected)
- [ ] remove those two gaps from the backlog item, keep the rest
- [ ] `cargo test -p fixture-guard` and the gate pass

### Task 7: Capture the 1:1 chat protocol beyond messages
**Files:**
- Create: `crates/proto-lab/src/scenario/ab_chat_full.rs`, `docs/protocol/chat.md`,
  `tests/fixtures/chatsvc/{conversations,messages,edit,delete,reaction,mention,reply}.json` (synthetic)
- Modify: `crates/proto-lab/src/{classify.rs,scenario/mod.rs}`, `crates/proto-lab/tests/fence_targets.rs`,
  `docs/protocol/fence-targets.md`

- [ ] a scenario `ab-chat-full` (the `ab-chat` fence, its forced-deny probe first, the Task 1 list): a sends, edits,
      reacts (add, remove), mentions b, replies with a quote, deletes; b reads and reacts; every write is on the a–b
      chat
- [ ] tests first for any mapping row the capture corrects (edit, delete, reaction, read receipt); a shape not in the
      table stays `Unknown`
- [ ] from the capture (read locally, exported to reference copies only) and Task 3's reference copies: the
      conversation list and its paging/sync links (from the probe if the web client only used CSA), message pages and
      `syncState`, message shapes (versions and `skypeeditedid`, deletes, `emotions`, `mentions` and the mention
      markup, quotes, `amsreferences`/`files` of received attachments), into `docs/protocol/chat.md`, each shape with
      its source; tick or correct the open rows of `fence-targets.md`
- [ ] synthetic fixtures of the same shapes, marked `_synthetic`; `cargo test -p fixture-guard` passes
- [ ] live run recorded in this plan (a breached run as in Development Approach); `cargo test -p proto-lab` and the
      gate pass

### Task 8: Group chats under the fence (a, b, c)
**Files:**
- Modify: `crates/teams-fence/src/membership.rs`, `crates/teams-fence/tests/membership.rs`,
  `crates/proto-lab/src/scenario/` (a new `abc-chat` scenario), `docs/protocol/fence-targets.md`
- Create: `tests/fixtures/chatsvc/thread-group*.json` (synthetic)

- [ ] live, reads only plus one allowed `ChatCreate` (`New`, members a, b, c): a creates the a–b–c group chat in the
      web client. The web client creates the chat when its first message is sent, and that message is denied
      (`Partial(Unsupported)` until this task's read exists) and would be resent on every later load: the scenario puts
      a `ForcedDeny` with a label of its own on a first-message marker, kept for every later run on these profiles, and
      expects it. The new chat's id goes to `<data_dir>/chats.toml`, never the repo
- [ ] capture the thread read, `thread_members` (paged form), `filtered`, member counts and roster versions for that
      chat as a, b and c see it
- [ ] decide with Codex what makes a group chat's member read **complete** (the evidence the 1:1 id gives for free:
      not a filtered view, not a page); record the decision and the evidence here and in `fence-targets.md`. Meeting
      chats, channels and any chat the evidence doesn't cover stay `Partial(Unsupported)`
- [ ] ⚠️ stop condition: if the evidence isn't sufficient, group chats stay `Partial(Unsupported)`, the finding is filed
      to `docs/backlog/`, and the M1 exit's group-chat part goes to the user (Decisions for the user). Until they
      answer, later tasks build and test group chats on the mock only, and Task 21 runs the 1:1 part
- [ ] tests first (wiremock, synthetic fixtures): complete only with that evidence; paging, a count above the list, a
      filtered marker, a mismatch between the reads → `Partial`; then implement it in `LiveMembershipSource`
- [ ] `abc-chat`: a messages the group, b and c show it; enforcement probe first; exit 0 under the Task 1 list; a
      breached run as in Development Approach
- [ ] `cargo test -p teams-fence -p fence-seal -p proto-lab` and the gate pass

### Task 9: `teams-mock`: the chat service from synthetic fixtures
**Files:**
- Create: `crates/teams-mock/{Cargo.toml,src/lib.rs,src/chatsvc.rs}`, `crates/teams-mock/tests/mock.rs`
- Modify: `Cargo.toml` (member, workspace deps)

- [ ] tests first: the mock serves the conversation list, threads (1:1 and group), message pages with sync links, and
      takes message posts, edits, deletes, reactions and read receipts, queuing each as an event for the other members
      (Task 16 adds the Trouter side that delivers them); it checks the headers Task 3 found and answers 401 without
      them
- [ ] everything from synthetic fixtures; no capture is read by a test
- [ ] dev-dependency only (never in a lab binary); clients reach it only through the Task 5 `test-support` mock
      origin; `[lints] workspace = true`, listed in `members`
- [ ] `cargo test -p teams-mock` and the gate pass

### Task 10: Chat service reads and the message model
**Files:**
- Create: `crates/teams-proto/src/chat/{mod.rs,model.rs,read.rs,html.rs}`, `crates/teams-proto/tests/chat_read.rs`
- Modify: `Cargo.toml` (the HTML parser in `[workspace.dependencies]`), `crates/teams-proto/Cargo.toml`

- [ ] tests first on the Task 7 fixtures: conversations (1:1, group; channels read but marked read-only), members,
      messages with versions, edits, deletes, reactions, mentions, quotes, references to received attachments (AMS
      images, file cards; parsed, not fetched); unknown fields kept (`serde(flatten)`), unknown message types as
      `Unknown(String)`, never a parse failure
- [ ] rich text: parse the HTML content into a small typed model (text runs, mentions, quotes, links, images); unknown
      markup kept as text; no script, style or event attribute survives
- [ ] paging and sync: follow the service's own `backwardLink`/`syncState` links only on the same chat base
- [ ] a 404 with `Location` is an error (`RegionMoved`, logged by status only), not followed (tests on the mock)
- [ ] `cargo test -p teams-proto` and the gate pass

### Task 11: `teams-store` (SQLite)
**Files:**
- Create: `crates/teams-store/{Cargo.toml,src/lib.rs,src/schema.rs,src/migrations/*.sql}`,
  `crates/teams-store/tests/store.rs`
- Modify: `Cargo.toml` (member, `rusqlite` in `[workspace.dependencies]`)

- [ ] tests first: migrations from empty; idempotent upserts of conversations, members and messages by id and version
      (an older version never overwrites a newer one); edits, soft deletes, reactions per user, read horizons, sync
      state per conversation; the outbox table (Task 13)
- [ ] choose the SQLite crate (`rusqlite` bundled unless a reason shows up), record it here; WAL; the file `0600`
- [ ] queries the core needs: conversation list by last activity, a page of messages, unread count
- [ ] `[lints] workspace = true`, listed in `members`
- [ ] `cargo test -p teams-store` and the gate pass

### Task 12: Chat writes as fenced requests
**Files:**
- Modify: `crates/teams-proto/src/write.rs`, `crates/teams-proto/tests/fence_types.rs`, `crates/fence-seal/tests/ui/*`,
  `crates/teams-fence/src/membership.rs`
- Create: `crates/teams-proto/tests/chat_write.rs`

- [ ] tests first: `SendMessage` (with mentions), `EditMessage`, `DeleteMessage`, `SetReaction`/`RemoveReaction`,
      `ReadReceipt`, `Typing`, `CreateChat` as sealed `Mutating` types with their wire forms; each target exactly as
      `fence-targets.md` (as corrected by Tasks 7–8); a permit for chat X can't send to chat Y
- [ ] a message's `content` and `properties.mentions` are both built from one typed rich-text value (Task 10's model);
      raw HTML can't be passed, so a mention can't be in the markup without being in the target; every mentioned MRI
      is an added participant
- [ ] the fence's live read gets the native client's own credential (`ChatCredential::bearer` from our `ic3` token and
      the region's chat base, the form Task 3 established); without it every write is `Partial(NoCredential)`
- [ ] bodies match the captured shapes (Task 7) key for key; tests compare with the synthetic fixtures
- [ ] tests on `teams-mock`: each write allowed for a lab chat and sent once; a chat with an outsider (a mock member
      outside the roster) denied, nothing sent, `fence.log` entry written
- [ ] `cargo test -p teams-proto -p teams-fence -p fence-seal` and the gate pass

### Task 13: Outbox with `clientmessageid` dedupe
**Files:**
- Create: `crates/teams-core/{Cargo.toml,src/lib.rs,src/outbox.rs}`, `crates/teams-core/tests/outbox.rs`
- Modify: `Cargo.toml` (member, workspace deps), `crates/teams-store/src/*`

- [ ] create the `teams-core` crate (`[lints] workspace = true`, listed in `members`) with the outbox as its first
      module; sync and the engine come in Tasks 17–18
- [ ] tests first: a command becomes an outbox row with a fresh `clientmessageid`; each attempt asks the fence again
      (a fresh permit, a fresh membership read); a denied attempt marks the row denied and sends nothing
- [ ] the outbox's part of the retry table (Technical Details), each case tested: 401, 429/503 with `Retry-After`,
      other 4xx (no retry), and a timeout whose message did arrive (found by `clientmessageid` in a sync, not sent
      again) or didn't (sent again with the same id)
- [ ] dedupe: our own message coming back (Trouter or sync) is matched by `clientmessageid` to its row, never stored
      twice
- [ ] **no resend across runs**: rows left pending by an earlier process are not sent on start; only an explicit
      command resends them (the web client's late resend in M0 is the case this rules out)
- [ ] `cargo test -p teams-core` and the gate pass

### Task 14: Trouter: capture and notes for messaging
**Files:**
- Create: `docs/protocol/trouter.md`, `tests/fixtures/trouter/*.json` (synthetic)
- Modify: `crates/proto-lab/src/{calling.rs,classify.rs,fence_ws.rs}` (a messaging own-state set),
  `crates/proto-lab/tests/calling.rs`

- [ ] tests first: a messaging own-state set for `proto-lab` (the Trouter frames of an `ab-chat`-style run) that
      allows the registrar registration only as `TeamsCDLWebWorker`, never `SkypeSpacesWeb`, and lets the web
      client's own `user.activity` out (presence showing is accepted); the `calling` set stays as it is
- [ ] capture (web client, `ab-chat`-style run with that set): b's Trouter session while a sends, edits and reacts;
      the registrar registration of the messaging stack; the deliveries for chat events (method, url, headers, body
      shape); the token renewal on a live socket (re-authenticate or reconnect)
- [ ] answer, with evidence, whether leaving chat deliveries unanswered harms the client: redelivery volume and timing,
      the socket closed, deliveries throttled or stopped. Decide with Codex; record it in `trouter.md`
- [ ] if it does harm: decide with Codex which delivery responses only acknowledge a chat event; that becomes a
      decision for the user (it revises the M0 rule that delivery responses stay denied). If it doesn't, deliveries
      stay unanswered and no response path is built. Nothing call-related is ever answered
- [ ] `user.activity`: whether the session needs it (does the service treat an endpoint that never sends it
      differently: deliveries, keep-alive, timeouts); if it does, it becomes a reviewed `OwnFrame` entry in its exact
      shape. Record what makes an account show as online (for M2's presence)
- [ ] write `trouter.md` in our own words: connect and regional move, `user.authenticate` in the first frame (the
      `ic3` token, `X-MS-Migration`), keep-alives, `trouter.connected` push URLs, registration and its TTL,
      deliveries, reconnect parameters, token renewal, presence
- [ ] synthetic fixtures; `cargo test -p fixture-guard` passes; live run recorded (a breached run as in Development
      Approach)
- [ ] `cargo test -p proto-lab` and the gate pass

### Task 15: The Trouter frame policy
**Files:**
- Create: `crates/teams-proto/src/frame.rs`, `crates/teams-proto/tests/frame.rs`,
  `crates/fence-seal/tests/ui/{proto_raw_frame.rs,proto_own_frame.rs}` (+ `.stderr`)
- Modify: `crates/teams-proto/src/lib.rs`, `crates/teams-fence/src/{log.rs,target.rs}`,
  `crates/teams-fence/tests/*`

- [ ] tests first (trybuild in `fence-seal`): a socket sink sends only `OwnFrame` (a sealed, reviewed list:
      `user.authenticate`, keep-alives, the entries Task 14 reviewed); an unreviewed frame (raw text, `user.activity`,
      a response built by hand) doesn't compile, and `OwnFrame` can't be implemented or constructed outside
      `teams-proto`
- [ ] tests first: `fence.log` gains a frame record (channel `WebSocket`, frame kind, never a token); every outgoing
      frame is logged before it is sent; one that can't be logged isn't sent
- [ ] only if Task 14 found harm **and** the user approved answering: a `DeliveryResponse` judged by the fence as a new
      `WriteKind` (a delivery acknowledgement) on `Scope::Conversation`, so `Fence::check` does the membership read and
      logs the decision; built only for a reviewed path on a complete, lab-only conversation; a `fence-seal` case
      proves nothing outside `teams-fence`/`teams-proto` can mint one. Otherwise this checkbox is struck and recorded
- [ ] `cargo test -p teams-proto -p teams-fence -p fence-seal` and the gate pass

### Task 16: `teams-trouter`: the native push channel
**Files:**
- Create: `crates/teams-trouter/{Cargo.toml,src/lib.rs,src/frames.rs,src/session.rs,src/registration.rs}`,
  `crates/teams-trouter/tests/{frames.rs,session.rs}`, `crates/teams-mock/src/trouter.rs`,
  `crates/fence-seal/tests/ui/trouter_mock_origin.rs` (+ `.stderr`)
- Modify: `Cargo.toml` (member, workspace deps), `crates/teams-mock/{src/lib.rs,tests/mock.rs}`,
  `crates/fence-seal/Cargo.toml` (`teams-trouter`, without `test-support`)

- [ ] the mock's Trouter endpoint, from the Task 14 fixtures: connect, authenticate, `trouter.connected`, ping,
      deliveries of the chat service mock's queued events
- [ ] `[lints] workspace = true`, listed in `members`; a `test-support` feature (`["teams-proto/test-support"]`) and
      only under it a mock origin (`ws` on loopback); a `fence-seal` case proves a normal build can't set it
- [ ] tests first: socket.io 0.9 frame codec on the Task 14 fixtures; authenticate as the first frame; acknowledged
      keep-alives; reconnect with backoff and the service's reconnect parameters; a delivery for an incoming call is
      **ignored, never answered** (the M0 echo-call policy), and so is every other delivery unless Task 15 built
      responses
- [ ] every outgoing frame goes through the Task 15 policy and is logged to `fence.log` first; the socket has no other
      way to send; `user.activity` only if Task 14 reviewed it
- [ ] the registrar registration (messaging stack only) and its renewal as `OwnState` requests (Task 5), the `ic3`
      token from the session
- [ ] token renewal: on the session's renewal notification the socket re-authenticates or reconnects (as Task 14
      showed) before the old token expires; tested with a fake clock on the mock
- [ ] deliveries → typed chat events (new message, edit, delete, reaction, read horizon, thread update); unknown ones
      kept as `Unknown` and logged by kind only
- [ ] tests on `teams-mock`: connect, register, receive a delivery, send no answer (or, if built, answer only a
      reviewed one on a lab-only chat), survive a dropped socket
- [ ] `cargo test -p teams-trouter -p teams-mock -p fence-seal` and the gate pass

### Task 17: Catch-up sync in `teams-core`
**Files:**
- Create: `crates/teams-core/src/sync.rs`, `crates/teams-core/tests/sync.rs`
- Modify: `crates/teams-core/{Cargo.toml,src/lib.rs}`

- [ ] tests first (on `teams-mock`): first sync fills the store; a sync after a gap (socket down, messages sent
      meanwhile) catches up exactly once; Trouter events and sync agree (no duplicates, versions respected); a
      reconnect triggers a catch-up
- [ ] the outbox's timeout check (Task 13) uses it to look for a `clientmessageid`
- [ ] `cargo test -p teams-core` and the gate pass

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

- [ ] reading references is in Task 10 (AMS images, file cards); this task is about **sending** (and fetching, which
      it needs)
- [ ] capture an image sent by a in the a–b chat in the web client, with the AMS object creation and upload denied
      first (what the client sends), then decide with Codex how an AMS object's target is read (its `permissions`
      naming the conversation) and whether its upload can be bound to it without a registry the fence can't verify;
      a breached run as in Development Approach
- [ ] if the evidence holds: put AMS on the transport's allowlist, map it, build `SendImage` as a fenced write, test on
      the mock, run it live in the a–b chat; otherwise file it to `docs/backlog/` and keep sending attachments out of
      M1 (OneDrive files go to M2 with Graph either way)
- [ ] tests for whatever is built; the gate passes

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
  (group chats on the mock only) or waits for more evidence.
- After Task 14, only if unanswered chat deliveries harm the client: whether the native client may answer reviewed
  chat deliveries on lab-only chats (this revises the M0 rule that delivery responses stay denied).
- Before M2: whether the user's own account passes Conditional Access with the native client (M0's open question 6 of
  `docs/protocol/auth.md`); that is the first step of the personal build, which agents never build or run.
