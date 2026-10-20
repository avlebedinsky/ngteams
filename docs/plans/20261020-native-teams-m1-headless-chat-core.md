# Native Teams for Linux — M1: Headless Chat Core

## Overview
- M1 builds the chat client without a UI: our own Rust code signs a lab account in, reads its chats, sends, edits,
  deletes and reacts to messages, receives them over Teams' push channel (Trouter), keeps everything in a local SQLite
  store, and catches up after a disconnect. The GPUI client (M2) is built on top of it.
- Roadmap line (from the M0 plan): `teams-store` (SQLite), `teams-trouter`, chatsvc (conversations, messages,
  send/edit/delete/react, rich text, mentions, attachments), an outbox with `clientmessageid` dedupe, catch-up sync,
  `teams-mock`. **Exit:** A→B delivery in under 2 s, covering edits, reactions and a 3-person group chat, automated.
- **Every write goes through the fence.** The native client has no browser fence around it, so its protection is the
  type-level one from M0: every request that others could notice is a sealed `teams_proto::Mutating` type sent only
  with a `WritePermit` from `Fence::check`, every other non-read request is on a reviewed own-state list, and the
  transport can send nothing else. The same holds for the Trouter socket: every outgoing frame is a sealed, reviewed
  own-state frame (authenticate, keep-alive) or a delivery response the fence judged, and each is logged to
  `fence.log` before it is sent. Writes reach only the 3 lab accounts.
- The first tasks close what M0 left open and M1 depends on: a scenario verdict that means pass/fail (the
  expected-deny list, and whether an arming timeout under load is a real breach), whether the chat service takes
  **our** `ic3` token, and token renewal.
- Out of scope: channels and teams (the lab accounts share tenant teams with people outside the lab, so channel writes
  stay unmapped and denied), meetings, calls (the call follow-ups stay in `docs/backlog/fence-call-follow-ups.md`
  for M3), presence, Graph and files beyond reading attachments, any UI.

## Context (from M0)
- M0 is done (`docs/plans/completed/20261019-native-teams-m0-lab-and-spikes.md`, `docs/m0-acceptance.md`). What exists:
  - `teams-fence`: `Fence::open_lab`, `WritePermit`, `fence.log`; membership sources `FailClosed` and
    `LiveMembershipSource` (1:1 chats only: `Complete` when the thread read lists exactly the two members the chat id
    names; group/meeting chats, channels, teams and calls are `Partial(Unsupported)`). `MembershipSource` and
    `Mutating` are sealed; `fence-seal` proves it.
  - `teams-proto`: `Mutating` (sealed), `Authorized`, `Client::write`, the `Transport` trait (no live transport yet);
    requests `SendMessage`, `StartCall`, `AddToCall`.
  - `teams-auth`: PKCE, `nativeclient` redirect, code exchange bound to the ID token's account, refresh,
    `AuthzClient` (`ExplicitLogin`/`TokenRefresh`), `Region` (GTMs), `SecretStore` (`KeyringSecretStore` under
    application `teams-lab`). `proto-lab auth` stores each lab account's refresh token and skypetoken there.
  - `proto-lab`: the armed browser fence, captures, export to local reference copies, `Classifier::mapped` (the chat
    service's writes on chats), scenarios `load`, `ab-chat`, `echo-call`, the DSL `Driver`, `ForcedDeny`.
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
    established (`docs/protocol/calling-echo.md`).
  - Chat base: GTM `chatServiceAfd` (`<AFD host>/api/chatsvc/amer`); no region hop was seen (question 9). GTM values
    are kept as returned and may carry stray whitespace.
  - Writes as the web client sends them: chat create (`POST v1/threads`, members in the body), message
    (`RichText/Html`), typing, read receipt (`consumptionhorizon`). Edit, delete, reactions, member changes and reply
    chains are mapped from the bundle but **not exercised**; group-chat reads and `thread_members` paging **not seen**
    (`docs/protocol/fence-targets.md`).
  - A failed send stays unsent in the web client and **goes out again on a later load**: our outbox must never resend
    on its own across runs.
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
- **CRITICAL: update this plan file when scope changes during implementation**
- run tests after each change
- **every write goes through the fence**: a sealed `Mutating` type, a `WritePermit` from `Fence::check` for its exact
  target, a fresh membership read. A request that isn't a read, a reviewed own-state request or a permitted write can't
  be sent. Anything unknown, partial or paginated is denied; a new kind of write is mapped only from a capture.
- **captures stay outside the repo**; fixtures are written synthetic-from-capture and pass
  `cargo test -p fixture-guard`.
- live runs come from a normal build (`cargo build -p <crate>`), headless, one browser per lab profile at a time; a
  native lab binary takes its fence from `Fence::open_lab` and its tokens from the Secret Service (`teams-lab`).
- protocol notes go to `docs/protocol/` in our own words; Microsoft JS is never copied.

## Testing Strategy
- **unit / replay**: `cargo test --workspace`, offline. HTTP on `wiremock`; whole-client tests against `teams-mock`
  (chat service + Trouter replayed from synthetic fixtures), two native clients talking through it.
- **fence suite**: as in M0, plus the native request policy (compile-fail cases: no request sent without a permit or
  an own-state kind, no Trouter frame sent that isn't a reviewed own-state frame or a judged delivery response) and the
  group-chat membership read.
- **fixture guard**: `cargo test -p fixture-guard` on every `tests/fixtures/` and `docs/`.
- **live lab**: `--features live` tests that run only with `PROTO_LAB_LIVE=1`, plus `proto-lab run …` and the
  `teams-headless` lab binary against a, b and c.
- **latency**: the live delivery test measures send → receive per message and reports p50/p95/max.

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
  expiry; the skypetoken renewed after its Entra token; 401 → refresh and one retry; 429 → `Retry-After`.
- **`teams-proto`**: the live HTTP transport. Requests come in three sealed classes: `Read` (GET), `OwnState` (a
  reviewed list of the account's own-state requests, logged to `fence.log`), `Mutating` (only as `Authorized`, logged
  by `Fence::check`). Fixed host allowlist, no redirects, no proxy.
- **`teams-trouter`**: socket.io 0.9 over WSS, `user.authenticate` in the first frame, keep-alives, the registrar
  registration, deliveries turned into events. Its socket sends only the sealed frame classes of the Task 4 policy,
  each logged to `fence.log` first: `OwnFrame` (authenticate, keep-alive, anything else only once reviewed) and
  `DeliveryResponse`, built only for a delivery on a reviewed path whose conversation the fence judged complete and
  lab-only. Incoming calls are never answered; `user.activity` is not sent (the lab never sets presence).
- **`teams-store`**: conversations, members, messages with versions (edits, deletes), reactions, read horizons, sync
  state, the outbox.
- **`teams-core`**: commands → outbox → fence → transport; events (Trouter, sync) → store; catch-up on start and after
  every reconnect.
- **`teams-mock`**: a local chat service and Trouter that replay synthetic fixtures and route one client's writes to the
  others' sockets, so the whole client is tested offline.

## Technical Details
- **Chat requests** (base = GTM `chatServiceAfd`): headers as the web client sends them (`Authorization: Bearer <ic3>`,
  `X-MS-Migration: True`, `BehaviorOverride: redirectAs404`; the exact set fixed by Task 2). A 404 with `Location`
  re-bases on the region it names, only onto a host `teams_fence::chat_service_path` accepts.
- **Message post** (`POST v1/users/ME/conversations/<id>/messages`): `content` (HTML), `messagetype` `RichText/Html`,
  `contenttype` `Text`, `clientmessageid` (a random numeric string, kept across retries), `imdisplayname`,
  `properties` (`mentions`, `files`, …). Answers 201 with `OriginalArrivalTime`.
- **Targets**: every chat write is `Conversation(<id>)`; a message's mentioned MRIs are added participants, so a
  mention of anyone outside the chat's lab members is denied; chat create is `New` with the body's members.
- **Store**: SQLite through `rusqlite` (bundled) unless Task 10 finds a reason otherwise; one file per account under
  `<data_dir>/store/<alias>.sqlite` (`0600`), outside the repo; schema versioned by migrations.
- **Lab binary**: `crates/teams-headless`, a normal build only (`test-support` builds refuse to run, as `proto-lab`).

## What Goes Where
- **Implementation Steps**: code, tests and docs in this repo, plus lab commands the agent runs on its own.
- **Post-Completion**: steps that need a human or a decision.

## Implementation Steps

### Task 1: A scenario verdict that means pass/fail (expected denies, arming breaches)
**Files:**
- Modify: `crates/proto-lab/src/scenario/{mod.rs,ab_chat.rs,echo_call.rs}`, `crates/proto-lab/src/arming.rs`,
  `crates/proto-lab/tests/browser.rs`
- Create: `crates/proto-lab/src/scenario/expected.rs`, `crates/proto-lab/tests/expected_denies.rs`
- Delete: `docs/backlog/ab-chat-exit-code-counts-unreviewed-denies.md`,
  `docs/backlog/arming-timeout-under-load-counts-as-breach.md` (each when done)

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
- [ ] settle `docs/backlog/arming-timeout-under-load-counts-as-breach.md` before any live run of this plan (this
      task's included) relies on a breach verdict: tests first (Chrome 155, a page whose held iframe sends a request and a frame as soon as it
      runs) on what happens to a target held by `waitForDebuggerOnStart` whose arming gave up before the resume and
      whose close failed or went unanswered; record every arming failure (target type, failed step, whether it was the
      resume, close result) in the capture and `fence.log`; then, by that evidence, either make an unconfirmed close
      before the resume a warning (re-checked with `Target.getTargets`) or confirm the close (retry, or kill the
      browser). Nothing is loosened without that evidence; decide with Codex if it is ambiguous
- [ ] live: `proto-lab run ab-chat --headless` exits 0 with goal, enforcement, no breach and lab-only writes as
      before; record the run in this plan
- [ ] `cargo test -p proto-lab` passes; delete both backlog items

### Task 2: Does the chat service take our own `ic3` token? (read-only)
**Files:**
- Create: `crates/proto-lab/src/commands/probe.rs`, `crates/proto-lab/tests/probe.rs`
- Modify: `crates/proto-lab/src/main.rs`, `docs/protocol/auth.md`

- [ ] tests first (wiremock): `proto-lab probe chat <alias>` refreshes the account's `ic3` token from the stored
      refresh token and sends only `GET`s to the chat base from the account's region GTMs: the conversation list (one
      page) and the fence's 1:1 thread read through `LiveMembershipSource` with a `ChatCredential` built from that
      token; it prints statuses, header names and token claims only (aud, exp, scopes, tenant), never a token or a
      body
- [ ] live, a and b: with and without `X-MS-Migration`, with and without `BehaviorOverride`; record which form the
      service accepts
- [ ] if it refuses a non-CAE token (a 401 naming a protocol or a claims challenge): decide with Codex between
      advertising CAE (`xms_cc` `CP1` on the refresh grant plus claims-challenge handling) and the non-CAE audience
      `https://ic3-non-cae.teams.office.com`; record the decision here and in `auth.md` question 8
- [ ] answer `auth.md` question 8 and the claims-challenge item; note what was seen of a region hop
- [ ] `cargo test -p proto-lab` passes

### Task 3: `teams-auth` session: per-audience tokens and renewal
**Files:**
- Create: `crates/teams-auth/src/session.rs`, `crates/teams-auth/tests/session.rs`
- Modify: `crates/teams-auth/src/{lib.rs,authz.rs,token.rs}`

- [ ] tests first (wiremock, synthetic fixtures, a fake clock): an access token per audience, cached in memory,
      refreshed a margin before `exp`; a rotated refresh token is stored; concurrent callers share one refresh
- [ ] the skypetoken is renewed only after refreshing `api.spaces.skype.com` (`AuthzKind::TokenRefresh`), never
      outlives that token, and a renewal with an old Entra token is refused by our code before it is sent
- [ ] 401 → refresh and one retry; a claims challenge (if Task 2 enabled CAE) passed to Entra up to 3 times; 429/503
      → `TokenError::Throttled` with `Retry-After`, no hidden retry loop
- [ ] the region GTMs come from the stored `Region` (trimmed), with a fallback for absent names
- [ ] tokens never reach `Debug`, errors or logs (tests check every fixture secret)
- [ ] `cargo test -p teams-auth` passes

### Task 4: Native request policy and the HTTP transport
**Files:**
- Create: `crates/teams-proto/src/{http.rs,read.rs,own.rs,frame.rs}`, `crates/teams-proto/tests/{http.rs,policy.rs}`,
  `crates/teams-proto/tests/ui/*` (new trybuild cases)
- Modify: `crates/teams-proto/src/{lib.rs,client.rs,write.rs}`, `crates/fence-seal/tests/*`

- [ ] tests first (trybuild): the transport sends only `Read` (GET), `OwnState` (a sealed, reviewed list) or
      `Authorized<Mutating>`; a non-GET request of any other kind doesn't compile, and `OwnState` can't be implemented
      outside `teams-proto`
- [ ] the HTTP transport (reqwest): fixed host allowlist (chat service hosts as `teams_fence::chat_service_path`,
      registrar, Trouter, authz, AMS for reads), no redirects, no proxy, `https` only, timeouts, body caps; per-service
      credentials from the Task 3 session
- [ ] every `OwnState` request is logged to `fence.log` (channel, method, URL without query) before it is sent; one
      that can't be logged isn't sent
- [ ] tests first (trybuild): the WebSocket side of the same policy, for `teams-trouter` (Task 14). A socket sink sends
      only `OwnFrame` (a sealed, reviewed list: `user.authenticate`, keep-alives) or a `DeliveryResponse`; an
      unreviewed frame (raw text, `user.activity`, a response built by hand) doesn't compile, and neither type can be
      implemented or constructed outside `teams-proto`. A `DeliveryResponse` is built only by a fence check on the
      delivery it answers: its path is on the reviewed list (Task 13) and its conversation's membership read is
      complete and lab-only; anything else (an incoming call, an unknown path, a partial read) gets no response
- [ ] every outgoing frame is logged to `fence.log` (channel, frame kind, delivery path and scope for a response,
      never a token) before it is sent; one that can't be logged isn't sent
- [ ] the own-state lists (requests and frames) start empty; each entry is added only by the task that captured and
      reviewed it, in its exact shape (method, origin, path, body schema; frame kind and schema)
- [ ] tests on wiremock: reads, a permitted write, an own-state request, the refusals; mutation-check the log rule
- [ ] `cargo test -p teams-proto -p fence-seal` passes

### Task 5: Close the sanitizer's known gaps before M1's exports
**Files:**
- Modify: `crates/fixture-guard/src/labels.rs`, `crates/proto-lab/src/sanitize/{meeting.rs,words.rs,backstop.rs}`,
  `crates/proto-lab/tests/sanitize.rs`, `crates/fixture-guard/tests/fixture_guard.rs`
- Delete: `docs/backlog/sanitizer-defence-in-depth-known-gaps.md` (when done)

- [ ] tests first, one per gap in the backlog item: a digit-prefixed passcode under a non-English label; a German
      `Kenncode`/`Besprechungskenncode` invitation without a join link; dotted names whose last part is a TLD
      (`<first>.kim`, three lowercase labels)
- [ ] fix each as the backlog item proposes; the property test `nothing_planted_survives_in_any_encoding` gains the
      three shapes
- [ ] M1 exports chat captures full of names and message text, so this goes before Task 6; the rule stays: reference
      copies never leave the machine, fixtures are synthetic
- [ ] `cargo test -p proto-lab -p fixture-guard` passes; delete the backlog item

### Task 6: Capture the 1:1 chat protocol beyond messages
**Files:**
- Create: `crates/proto-lab/src/scenario/ab_chat_full.rs`, `docs/protocol/chat.md`,
  `tests/fixtures/chatsvc/{conversations,messages,edit,delete,reaction,mention,reply}.json` (synthetic)
- Modify: `crates/proto-lab/src/{classify.rs,scenario/mod.rs}`, `crates/proto-lab/tests/fence_targets.rs`,
  `docs/protocol/fence-targets.md`

- [ ] a scenario `ab-chat-full` (the `ab-chat` fence, its forced-deny probe first, the Task 1 policy): a sends, edits,
      reacts (add, remove), mentions b, replies with a quote, deletes; b reads and reacts; every write is on the a–b
      chat
- [ ] tests first for any mapping row the capture corrects (edit, delete, reaction, read receipt); a shape not in the
      table stays `Unknown`
- [ ] from the capture (read locally, exported to reference copies only): the conversation list and its paging/sync
      links, message pages and `syncState`, message shapes (versions and `skypeeditedid`, deletes, `emotions`,
      `mentions` and the mention markup, quotes, `amsreferences`/`files` of received attachments), into
      `docs/protocol/chat.md`; tick or correct the open rows of `fence-targets.md`
- [ ] synthetic fixtures of the same shapes, marked `_synthetic`; `cargo test -p fixture-guard` passes
- [ ] live run recorded in this plan; `cargo test -p proto-lab` passes

### Task 7: Group chats under the fence (a, b, c)
**Files:**
- Modify: `crates/teams-fence/src/membership.rs`, `crates/teams-fence/tests/membership.rs`,
  `crates/proto-lab/src/scenario/` (a new `abc-chat` scenario), `docs/protocol/fence-targets.md`
- Create: `tests/fixtures/chatsvc/thread-group*.json` (synthetic)

- [ ] live, reads only plus one allowed `ChatCreate` (`New`, members a, b, c): a creates the a–b–c group chat in the web
      client; capture the thread read, `thread_members` (paged form), `filtered`, member counts and roster versions
      for that chat as a, b and c see it
- [ ] decide with Codex what makes a group chat's member read **complete** (the evidence the 1:1 id gives for free:
      not a filtered view, not a page); record the decision and the evidence here and in `fence-targets.md`. Meeting
      chats, channels and any chat the evidence doesn't cover stay `Partial(Unsupported)`
- [ ] tests first (wiremock, synthetic fixtures): complete only with that evidence; paging, a count above the list, a
      filtered marker, a mismatch between the reads → `Partial`; then implement it in `LiveMembershipSource`
- [ ] `abc-chat`: a messages the group, b and c show it; enforcement probe first; exit 0 under the Task 1 policy
- [ ] `cargo test -p teams-fence -p fence-seal -p proto-lab` passes

### Task 8: `teams-mock`: the chat service and Trouter from synthetic fixtures
**Files:**
- Create: `crates/teams-mock/{Cargo.toml,src/lib.rs,src/chatsvc.rs,src/trouter.rs}`, `crates/teams-mock/tests/mock.rs`
- Modify: `Cargo.toml` (member, workspace deps)

- [ ] tests first: the mock serves the conversation list, threads (1:1 and group), message pages with sync links, and
      takes message posts, edits, deletes, reactions and read receipts, routing each as a delivery to the other
      members' Trouter sockets; it checks the headers Task 2 found and answers 401 without them
- [ ] a Trouter endpoint speaking the socket.io 0.9 frames of `docs/protocol/calling-echo.md` (connect, authenticate,
      `trouter.connected`, ping, deliveries, responses), refined by Task 13
- [ ] everything from synthetic fixtures; no capture is read by a test
- [ ] dev-dependency only (never in a lab binary); `[lints] workspace = true`, listed in `members`
- [ ] `cargo test -p teams-mock` passes

### Task 9: Chat service reads and the message model
**Files:**
- Create: `crates/teams-proto/src/chat/{mod.rs,model.rs,read.rs,html.rs}`, `crates/teams-proto/tests/chat_read.rs`

- [ ] tests first on the Task 6 fixtures: conversations (1:1, group; channels read but marked read-only), members,
      messages with versions, edits, deletes, reactions, mentions, quotes, received attachments (AMS images, file
      cards); unknown fields kept (`serde(flatten)`), unknown message types as `Unknown(String)`, never a parse failure
- [ ] rich text: parse the HTML content into a small model (text runs, mentions, quotes, links, images); unknown markup
      kept as text; no script, style or event attribute survives
- [ ] paging and sync: follow the service's own `backwardLink`/`syncState` links only on the same chat base
- [ ] the region hop: a 404 with `Location` re-bases reads (tests on the mock)
- [ ] `cargo test -p teams-proto` passes

### Task 10: `teams-store` (SQLite)
**Files:**
- Create: `crates/teams-store/{Cargo.toml,src/lib.rs,src/schema.rs,src/migrations/*.sql}`,
  `crates/teams-store/tests/store.rs`
- Modify: `Cargo.toml` (member, workspace deps)

- [ ] tests first: migrations from empty; idempotent upserts of conversations, members and messages by id and version
      (an older version never overwrites a newer one); edits, soft deletes, reactions per user, read horizons, sync
      state per conversation; the outbox table (Task 12)
- [ ] choose the SQLite crate (`rusqlite` bundled unless a reason shows up), record it here; WAL; the file `0600`
- [ ] queries the core needs: conversation list by last activity, a page of messages, unread count
- [ ] `cargo test -p teams-store` passes

### Task 11: Chat writes as fenced requests
**Files:**
- Modify: `crates/teams-proto/src/write.rs`, `crates/teams-proto/tests/fence_types.rs`, `crates/teams-proto/tests/ui/*`,
  `crates/teams-fence/src/membership.rs`
- Create: `crates/teams-proto/tests/chat_write.rs`

- [ ] tests first: `SendMessage` (with mentions), `EditMessage`, `DeleteMessage`, `SetReaction`/`RemoveReaction`,
      `ReadReceipt`, `Typing`, `CreateChat` as sealed `Mutating` types; each target exactly as `fence-targets.md`
      (as corrected by Tasks 6–7); a message's mentioned MRIs are added participants; a permit for chat X can't send
      to chat Y; the wire body is built only from the fields `target()` reads
- [ ] the fence's live read gets the native client's own credential (`ChatCredential` from our `ic3` token and the
      region's chat base, the form Task 2 established); without it every write is `Partial(NoCredential)`
- [ ] bodies match the captured shapes (Task 6) key for key; tests compare with the synthetic fixtures
- [ ] tests on `teams-mock`: each write allowed for a lab chat and sent once; a chat with an outsider (a mock member
      outside the roster) denied, nothing sent, `fence.log` entry written
- [ ] `cargo test -p teams-proto -p teams-fence -p fence-seal` passes

### Task 12: Outbox with `clientmessageid` dedupe
**Files:**
- Create: `crates/teams-core/{Cargo.toml,src/lib.rs,src/outbox.rs}`, `crates/teams-core/tests/outbox.rs`
- Modify: `Cargo.toml` (member, workspace deps), `crates/teams-store/src/*`

- [ ] create the `teams-core` crate (`[lints] workspace = true`, listed in `members`) with the outbox as its first
      module; sync and the engine come in Task 15
- [ ] tests first: a command becomes an outbox row with a fresh `clientmessageid`; each attempt asks the fence again
      (a fresh permit, a fresh membership read); a network error retries with the same `clientmessageid`; a 4xx doesn't
      retry; a denied attempt marks the row denied and sends nothing
- [ ] dedupe: our own message coming back (Trouter or sync) is matched by `clientmessageid` to its row, never stored
      twice
- [ ] **no resend across runs**: rows left pending by an earlier process are not sent on start; only an explicit
      command resends them (the web client's late resend in M0 is the case this rules out)
- [ ] `cargo test -p teams-core` passes

### Task 13: Trouter: capture and notes for messaging
**Files:**
- Create: `docs/protocol/trouter.md`, `tests/fixtures/trouter/*.json` (synthetic)
- Modify: `crates/proto-lab/src/{calling.rs,classify.rs,fence_ws.rs}` (a messaging own-state set, if reviewed),
  `crates/proto-lab/tests/calling.rs`

- [ ] capture (web client, `ab-chat`-style run): b's Trouter session while a sends, edits and reacts; the registrar
      registration of the messaging stack (`TeamsCDLWebWorker`); the deliveries for chat events (method, url, headers,
      body shape) and what the service does when their responses are denied (redelivery timing)
- [ ] decide with Codex which delivery responses only acknowledge a chat event and may become the reviewed paths of
      the Task 4 frame policy, and which (incoming calls, anything unseen) are never answered; record the evidence;
      nothing call-related is answered
- [ ] write `trouter.md` in our own words: connect and regional move, `user.authenticate` in the first frame (the
      `ic3` token, `X-MS-Migration`), keep-alives, `trouter.connected` push URLs, registration and its TTL,
      deliveries and responses, reconnect parameters, whether `user.activity` is needed (the lab never sets presence)
- [ ] synthetic fixtures; `cargo test -p fixture-guard` passes

### Task 14: `teams-trouter`: the native push channel
**Files:**
- Create: `crates/teams-trouter/{Cargo.toml,src/lib.rs,src/frames.rs,src/session.rs,src/registration.rs}`,
  `crates/teams-trouter/tests/{frames.rs,session.rs}`
- Modify: `Cargo.toml` (member, workspace deps), `crates/teams-proto/src/frame.rs` (the reviewed frame entries)

- [ ] tests first: socket.io 0.9 frame codec on the Task 13 fixtures; authenticate as the first frame; acknowledged
      keep-alives; reconnect with backoff and the service's reconnect parameters; a delivery for an incoming call is
      **ignored, never answered** (the M0 echo-call policy), and so is any delivery path not on the reviewed list
- [ ] every outgoing frame goes through the Task 4 frame policy and is logged to `fence.log` first: authenticate and
      keep-alives as `OwnFrame`, delivery responses only as a fence-judged `DeliveryResponse` on a Task 13 reviewed
      path; the socket has no other way to send
- [ ] the registrar registration and its renewal as `OwnState` requests (Task 4), the `ic3` token from the session
- [ ] deliveries → typed chat events (new message, edit, delete, reaction, read horizon, thread update); unknown ones
      kept as `Unknown` and logged by kind only
- [ ] tests on `teams-mock`: connect, register, receive a delivery, acknowledge it only if reviewed and its chat is
      lab-only, never answer one from a chat with an outsider (no frame sent, `fence.log` entry written), survive a
      dropped socket
- [ ] `cargo test -p teams-trouter` passes

### Task 15: Catch-up sync, `teams-core` and the `teams-headless` lab binary
**Files:**
- Create: `crates/teams-core/src/{sync.rs,engine.rs}`, `crates/teams-core/tests/{sync.rs,engine.rs}`,
  `crates/teams-headless/{Cargo.toml,src/main.rs}`
- Modify: `crates/teams-core/{Cargo.toml,src/lib.rs}` (from Task 12), `Cargo.toml` (`teams-headless` as a member),
  `README.md`

- [ ] tests first (on `teams-mock`): first sync fills the store; a sync after a gap (socket down, messages sent
      meanwhile) catches up exactly once; Trouter events and sync agree (no duplicates, versions respected); a
      reconnect triggers a catch-up
- [ ] the engine: commands (send, edit, delete, react, read, create chat) → outbox → fence → transport; events → store;
      tokio on its own runtime, reachable through channels (the shape M2's GPUI bridge needs)
- [ ] two native clients on the mock: a sends, b receives through its Trouter socket; edit and reaction arrive; a
      group of three
- [ ] `teams-headless <alias> sync|list|show <chat>|send <chat> <text>|edit|delete|react|watch`: `Fence::open_lab`,
      tokens from the Secret Service (`teams-lab`), the store under the lab's data dir; refuses to run when built with
      `test-support`; prints no token
- [ ] `cargo test -p teams-core -p teams-headless` passes; `cargo build -p teams-headless` runs `--help`

### Task 16: Attachments: decide sending, then build it or defer it
**Files:**
- Modify: `docs/protocol/chat.md`, `crates/teams-proto/src/chat/*` (if built)

- [ ] reading is in Task 9 (AMS images over `skype_token`, file cards); this task is about **sending**
- [ ] capture an image sent by a in the a–b chat in the web client, with the AMS object creation and upload denied
      first (what the client sends), then decide with Codex how an AMS object's target is read (its `permissions`
      naming the conversation) and whether its upload can be bound to it without a registry the fence can't verify
- [ ] if the evidence holds: map it, build `SendImage` as a fenced write, test on the mock, run it live in the a–b
      chat; otherwise file it to `docs/backlog/` and keep sending attachments out of M1 (OneDrive files go to M2 with
      Graph either way)
- [ ] tests for whatever is built; `cargo test --workspace` passes

### Task 17: Verify M1 acceptance criteria
**Files:**
- Create: `crates/teams-headless/tests/chat_live.rs` (`required-features = ["live"]`), `docs/m1-acceptance.md`

- [ ] the live test (`PROTO_LAB_LIVE=1`, normal tokens from the Secret Service, fence from `Fence::open_lab`): native
      a sends N messages to b in the a–b chat; native b receives each over Trouter; **send → receive p95 under 2 s**
- [ ] the same test covers an edit and a reaction (both arrive on b), and a 3-person group chat of a, b and c (each
      message reaches both others)
- [ ] a catch-up check: b's socket closed, a sends, b reconnects and has every message exactly once
- [ ] `fence.log` for the run: allows only on the lab chats, no deny of a mapped write, no own-state request or
      Trouter frame off the list, delivery responses only for lab chats
- [ ] `proto-lab run ab-chat --headless` and `abc-chat` exit 0
- [ ] the full gate passes; write `docs/m1-acceptance.md` (numbers, fence summary, flakes)

### Task 18: [Final] Update documentation
- [ ] README.md: the M1 crates, `teams-headless`, the new scenarios and `probe`, tests
- [ ] CLAUDE.md: patterns discovered during M1 (the native request policy, the outbox rule, Trouter answers)
- [ ] write the M2 plan (GPUI daily driver), informed by M1
- [ ] move this plan to `docs/plans/completed/`

## Validation commands

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/lint-report.sh
PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1 cargo test --workspace
cargo test -p fixture-guard
cargo build -p proto-lab -p teams-headless
target/debug/proto-lab run ab-chat --headless
target/debug/proto-lab run abc-chat --headless
PROTO_LAB_LIVE=1 cargo test -p proto-lab --features live --test auth_live
PROTO_LAB_LIVE=1 cargo test -p teams-headless --features live --test chat_live
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
- Before an autonomous run, the user adds permission rules for `teams-headless` like those for `proto-lab`.

**Decisions for the user:**
- Approving any new service participant (none planned in M1).
- Before M2: whether the user's own account passes Conditional Access with the native client (M0's open question 6 of
  `docs/protocol/auth.md`); that is the first step of the personal build, which agents never build or run.
