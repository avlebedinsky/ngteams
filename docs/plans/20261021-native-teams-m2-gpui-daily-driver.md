# Native Teams for Linux — M2: GPUI Daily Driver

## Overview
- M2 puts a GPUI window on M1's core: a chat list, a chat view and compose, desktop notifications and a tray icon,
  local search, teams and channels, presence, names and pictures, files and the calendar (Graph), and a sign-in window
  on an embedded WebKitGTK webview. In the lab it runs as one lab account, under the fence, the way `teams-headless`
  does; every write the UI asks for is an engine action that goes out through the outbox and `Fence::check`.
- Roadmap line (from the M0 and M1 plans): sign-in in an embedded WebKitGTK webview, chat list, chat view and compose,
  notifications, presence, search, teams and channels, files and calendar (Graph), Adaptive Cards, tray. **Exit:**
  GPUI UI tests plus screenshots against the live lab. From here the user can switch to a personal build.
- What M1 changes in that line (each one decided by a hard rule or a finding, not by taste):
  - **Adaptive Cards go to the long tail with bots.** Every card in a chat comes from a bot or a connector; none is in
    the lab, so no capture can show the message shape a renderer would read, and every card action is a bot invoke,
    which carries the account's token to the bot's backend and stays denied
    (`docs/backlog/bot-invokes-and-message-extensions.md`, the user's decision of 2026-10-21).
  - **Search is local.** The server's people and message search (Substrate) lists other tenant users, so the hard
    rules never let it through in the lab. M2 searches the messages the account's store holds.
  - **Teams and channels are read-only in the lab.** The lab accounts share their tenant teams with people outside
    the lab, so channel writes stay unmapped and denied, unless the user approves a team of the 3 lab accounts only
    (Decisions for the user).
  - **Presence is read, not set.** Setting presence is a write the whole tenant sees; the native client sets none
    until the user decides otherwise (Decisions for the user). Reading the lab accounts' presence is a read by `POST`:
    the browser fence may allow it under the allowance rules, but the native client can't send it unless the user
    widens its hard rule (Decisions for the user).
  - **Images and files: sending is decided from a capture first,** as M1 Task 20 did for images
    (`docs/backlog/ams-image-sending.md`). No image or file has ever been received in the lab, so viewing them needs
    sending first.
- **Every write goes through the fence**, and the native client sends only what its sealed policy allows (CLAUDE.md's
  hard rules). The UI gets no request path of its own: the app has no HTTP or socket client, only `teams-core`'s
  engine. A new request class is added only as those rules allow, with its row in `docs/protocol/endpoints.md`: a
  Graph or CSA read is a `GET`, an own-state entry is reviewed in its captured shape. A native read by `POST`
  (profiles, presence) fits none of the rule's three lists, so M2 sends none unless the user widens that rule
  (Decisions for the user); until then the tasks that would use one take their interim path.
- **The personal build is the user's.** Agents never build or run it and never receive the user's tokens. M2 ends with
  its design written down and the user's decision on it; nothing that loosens the fence for it is written before.
- Out of scope: calls (M3: `docs/backlog/fence-call-follow-ups.md`), meetings (M4; a meeting link opens in the
  browser), bots, message extensions and Adaptive Cards (M5), channel writes (unless the user approves a lab-only
  team), setting presence (unless the user decides), server search, Loop, Whiteboard and tabs (M5's
  `teams-webhost`), group chats the native client creates (their enrolment:
  `docs/backlog/group-chat-member-read-completeness.md`).

## Context (from M1)
- M1 is done (`docs/plans/completed/20261020-native-teams-m1-headless-chat-core.md`, `docs/m1-acceptance.md`). What
  exists:
  - `teams-core::engine::start(Parts { store, client, outbox, push, options })` → `EngineHandle`: its own
    current-thread runtime on a thread of its own; `write(Action)` (`Send`, `Edit`, `Delete`, `React`, `Unreact`,
    `Read`, `CreateChat`), `sync`, `load_older`, store queries (`conversations`, `messages`, `message`, `reactions`,
    `members`, `outbox_entry`), each answering a `Reply` (a future on any executor, or `blocking_wait`); events
    through `next_event` (`Connected`, `Registration`, `Disconnected`, `CaughtUp`, `Message`, `Typing`, `ReadHorizon`,
    `ConversationChanged`, `Outbox`, `Failed`, …). A write answers with its row even when it waits or failed.
  - `teams-proto::chat`: the conversation and message model (`Field::Unknown` for unseen forms), `RichText` (text with
    marks, breaks, paragraphs, mentions, links, images, quotes; sanitized, depth- and size-capped) and `MessageText`
    (a message's markup, its mentions and its target from one value; no raw HTML, no images yet).
  - `teams-store`: conversations, members, messages by version, reactions, read horizons, unread counts, sync state,
    the outbox, runs; migrations up to `0003`.
  - `teams-trouter`: the push channel, its own frames only, deliveries never answered; `teams-auth::session::Session`
    (a token per audience, renewals); `teams-fence`'s `LiveMembershipSource::for_fence` with a `CredentialSource`.
  - `teams-headless` (the lab CLI, with a lib beside `main.rs` so its commands are tested on mock engines) and its
    live acceptance test, an orchestrator that spawns the normally built binary (`--target-dir target/lab`).
  - `teams-mock`: the chat service, the registrar and Trouter from synthetic fixtures, delivering each write to the
    other members' sockets; reached only through the `test-support` mock origins.
  - GPUI: `gpui-pre` 0.3.9 vendored with agterm-rs's patches (`scripts/vendor-gpui.sh`). The M0 spike
    (`docs/spikes/gpui-video.md`) runs GPUI on a niri nested in a headless weston (`spikes/gpui-video/src/sandbox.rs`:
    its own Wayland socket, screenshots through `niri msg`, never the desktop session); the spike binary refuses to
    open a window outside that sandbox.
  - On the machine: WebKitGTK 2.54 (`webkit2gtk-4.1` for GTK 3, `webkitgtk-6.0` for GTK 4), weston 15, niri 26.04,
    `dbus-daemon`.
- What M1 learned that shapes M2:
  - **Latency is the post.** Native a → b p95 1.37 s, of which the post (its fresh member read and the service) is
    about four fifths and the delivery about 0.25 s. The UI shows a message as sending at once, from its outbox row.
  - **The service sends three message-loss reports right after each connect**; each is caught up and acknowledged,
    harmlessly. A reconnect replays up to ~15 min of unanswered deliveries; the store's versions absorb them.
  - **A native read receipt moves the service's horizon, not the web client's "last read" mark**; the web client then
    opens a chat at that mark (`ab_chat::wait_for_newest`). The UI's unread counts come from the store's horizon.
  - **The lab's group chat is admitted by an accepted assumption** (CLAUDE.md); a group chat the native client
    creates is denied, so the UI can't offer creating one in the lab.
  - **Names**: a thread read gives MRIs, a message its sender's `imdisplayname`; the web client's mention picker named
    lab accounts from the middle tier's profile lookups (`users/fetchShortProfile`, `users/fetch`, a read by `POST`,
    allowed in the browser only for lab accounts). Graph tokens are issued for the lab accounts (M0 Task 14), but no
    Graph profile read was ever made.
  - **Only the `like` reaction was captured**; typing goes out as `Control/Typing` posts (its timing not written up),
    typing stopped (`Control/ClearTyping`) was never captured; the engine has no typing action.
  - **Teams' chat list came from CSA** (`chatsAndChannelsByIds`, a read by `POST`, denied); the chat service's own
    conversation list works natively and lists channels as read-only conversations. CSA's team reads by `GET`
    (`api/v3/teams/users/me`) are "seen, not reviewed".
  - **Two live-only bugs** (a signed delivery id, a base64url endpoint id) passed every offline test: fixtures have to
    cover a field's whole captured range.
- What stays owed from M1 (`docs/backlog/`):
  - `seeded-own-rules-ignore-the-query.md` (worth: yes): Task 1.
  - `catch-up-consumes-an-unreadable-entry-of-a-pending-row.md` (worth: yes): Task 2; a daily driver keeps the core
    running for days, so the window it describes is no longer rare.
  - `ams-image-sending.md` (later): Task 14.
  - `group-chat-member-read-completeness.md` (later): native group-chat enrolment stays out of M2 (roadmap; a change
    to a hard rule, so the user's).
  - `bot-invokes-and-message-extensions.md` (yes, M5): roadmap, with Adaptive Cards.
  - `fence-call-follow-ups.md` (M3), `one-time-code-sign-in-under-the-fence.md` and
    `sanitizer-defence-in-depth-known-gaps.md` (later), `containment-kill-cannot-order-an-unread-eof.md` (accepted):
    unchanged.
- Lab: a, b, c licensed and signed in, tokens in the Secret Service (`teams-lab`), the a–b–c group chat recorded in
  `chats.toml`; each account's store holds what M1's native runs synced.

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
- **every write goes through the fence**: the UI calls the engine, the engine the outbox, the outbox `Fence::check`.
  A view never builds a request. A new kind of write is mapped only from a capture, a response shape is handled only
  once a capture showed it, and a new read or own-state class is added only as CLAUDE.md's allowance rules say.
- **no window, bus or handler of the user's desktop**: every GUI test, offline or live, runs in the UI sandbox (a
  nested niri on a headless weston, a private D-Bus session bus, a no-op URI opener); a client is pointed at the
  sandbox's socket or bus explicitly, and a test checks the client honours it. The lab app refuses to open a window
  outside the sandbox. The lab's tokens stay in the Secret Service on the user's bus, which the sandboxed app never
  reaches: it gets them only through the credential broker (Task 4), a process outside the sandbox.
- **test-only reach stays behind `test-support`**, as in M1; the lab app refuses to run when built with it.
- **captures and screenshots of the live lab stay outside the repo**: captures under the lab's `captures/`, live
  screenshots under `<data_dir>/screenshots/` (they show lab and tenant data). Fixtures are synthetic-from-capture and
  pass `cargo test -p fixture-guard`; a fixture covers every form of a field the captures show.
- live runs come from a normal build (`--target-dir target/lab`), under the live lock with an outer `timeout
  --signal=INT`, one process per lab account and one browser per lab profile at a time; nothing is left running. No
  write ever comes from a `cargo test` process.
- **a breached live browser run** is handled as CLAUDE.md says.
- protocol notes go to `docs/protocol/` in our own words; every endpoint touched gets its row in `endpoints.md`.
- every version written (a crate, a toolchain) is looked up live at the time, never taken from memory.

## Testing Strategy
- **unit / view**: `cargo test --workspace`, offline. Views tested on GPUI's test context against a fake engine (a
  small trait in `teams-ui`); models against a real store file.
- **whole app, offline**: the app's views run in-process in the UI sandbox against `teams-mock` (two or three accounts,
  each its own engine, fence and `fence.log`), driven by the sandbox's virtual keyboard and pointer, read back by the
  app's view trace and by screenshots checked for coarse properties (window present, regions drawn). Screenshots of
  the offline runs are test outputs under `target/`, never committed.
- **fence suite**: as in M1, plus: the app crates depend on no HTTP, WebSocket or D-Bus-network client of their own
  (a manifest test), every write in a UI test reaches the mock through `Fence::check` with its `fence.log` line, and
  a `fence-seal` case for each new sealed class.
- **fixture guard**: `cargo test -p fixture-guard` on every `tests/fixtures/` and `docs/`.
- **live lab**: `--features live` tests, run only with `PROTO_LAB_LIVE=1`: an orchestrator that starts the UI sandbox,
  spawns the normally built app for a, b and c in it, drives them with the sandbox's input, reads their view traces and
  `fence.log`, and keeps screenshots under the lab's data dir; plus `proto-lab run …` for the captures.
- **latency**: send → shown on the other account's screen, per message, p50/p95/max over N = 20.

## Progress Tracking
- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

```
teams-app (lab binary: one account, GPUI) ── teams-ui (views, models) ── EngineHandle: actions in, events out
      │                                                                        │
      │ spawns, reads the code                                                  ▼
teams-signin (WebKitGTK, its own process) ──▶ teams-auth          teams-core ──▶ teams-proto / teams-trouter / store
ui-sandbox (tests only: weston + nested niri, private D-Bus, virtual input, screenshots, no-op opener)
```

- **`teams-ui`**: GPUI views and their models over an engine seam: the chat list, the chat view, compose, search,
  teams and channels, the calendar, settings. The bridge owns the engine handle; one background task drains its
  events and updates every window's models on GPUI's foreground; queries are `Reply` futures. No view builds a
  request.
- **`teams-app`**: the lab binary, `teams-app --lab <alias> --secrets-fd <n>`: `Fence::open_lab`, the account's
  `Session` over the credential broker's socket, `LiveMembershipSource::for_fence` with the transport as its
  `CredentialSource`, the store at `LabPaths::store_file`, Trouter. It refuses a `test-support` build, refuses to
  open a window outside the UI sandbox, and prints no token. Notifications and the tray go to the session bus it is
  given. Its lab build writes a view trace (ids and kinds of what each view shows, never message text) for the live
  test. `teams-app secrets --lab <alias>` is its credential broker, started outside the sandbox (Task 4).
- **`teams-signin`**: the sign-in window, a separate process (GTK's main loop apart from GPUI's): Microsoft's page in
  a WebKitGTK webview behind a request guard; it hands the redirect's code to its parent, and `teams-auth` does the
  rest (PKCE, exchange, the ID token's account check).
- **`ui-sandbox`**: the M0 spike's sandbox made a dev-dependency crate, with a private bus, virtual input and a no-op
  URI opener; `spikes/gpui-video` moves onto it.
- **`teams-core` grows** only the actions and queries the UI needs (typing, local search, names), each tested on the
  mock first.

## Technical Details
- **The bridge**: the engine stays on its own thread and runtime. `EngineHandle` has one event receiver,
  `next_event` (and `try_next_event`, `blocking_next_event`) takes `&mut self`, and `shutdown(self)` consumes it, so
  it can't sit behind an `Arc`: `teams-ui`'s bridge owns it. Its request side (writes and queries, each answering a
  `Reply`) is split off as a cloneable handle (`teams-core` grows that split, which M1 doesn't have: the request
  sender cloned, the receiver and the thread kept by the owner) and shared by every window behind a trait (`Core`), so views
  are tested on a fake. One background task drains `next_event` and fans each event out to every open window's models
  through GPUI's async context; a view asks for data with a query `Reply`. Shutdown ends the drain task and calls
  `shutdown` on the owned handle off GPUI's foreground (it blocks until the engine's thread is joined). A message the
  account sends shows at once from its outbox row (`Event::Outbox`), then as the message once it is sent.
- **Credentials: the broker.** `teams-headless` reaches the lab's tokens through `KeyringSecretStore`, which is `oo7`
  on the session bus named by `DBUS_SESSION_BUS_ADDRESS` (oo7 0.6 takes no other connection), and `Session` reads and
  writes the store for the whole run (a renewal stores the new tokens). The sandboxed app's session bus is the
  sandbox's, so it can't use that store. Instead `teams-app secrets --lab <alias>` runs outside the sandbox with the
  user's bus, holds the one `KeyringSecretStore`, and serves `get`, `put` and `delete` of that account's own
  `teams-lab` items, nothing else, on one end of a socket pair. The app (`--secrets-fd <n>`) gets the other end and
  uses it through a `SecretStore` of its own (`BrokerSecretStore`). The broker opens no window, starts no desktop
  client, and exits when the socket closes. Neither side logs a value, only the key's account and audience. The app
  has no fallback: without `--secrets-fd` it doesn't start, and it never opens `KeyringSecretStore` or a bus from its
  environment for secrets. So the desktop-bus rule holds: the sandboxed process reaches only the sandbox's bus, and
  the user's bus is reached only by the broker, for the lab's own items.
- **Reads the UI needs, by request class**: the store first (everything shown comes from it); new service reads are
  `GET`s added to the transport's allowlist host by host (Graph `https://graph.microsoft.com/v1.0/` with a Graph
  token from the `Session`, CSA's team reads with its `chatsvcagg` token), each probed read-only first
  (`proto-lab probe …`, statuses, key names and sizes only). A read by `POST` (profiles, presence) is none of the
  hard rule's three lists (a `GET` read, an `OwnState` entry, an `Authorized<Mutating>` write), so the native client
  sends none. A sealed native class for it (`PostRead`) is written only if the user widens that rule (Decisions for
  the user). Until then Tasks 8 and 13 take their interim paths: names from what the reads already give, presence
  not shown.
- **Writes the UI adds**: typing (`teams_proto::Typing`, already sealed and mapped; a new `Action::Typing`, sent no
  more often than the capture shows); nothing else new. Reactions offer only the keys a capture showed.
- **Outbox states in the UI**: sending, sent, waiting (`Retry-After`), denied by the fence, failed. "Send again" on a
  failed row and on an earlier session's row is the explicit `Store::resend`, never automatic.
- **Notifications and tray**: `org.freedesktop.Notifications` and a StatusNotifierItem over D-Bus (`zbus`), on the
  bus address the app is given; in tests the sandbox's private bus with a fake notification server and tray
  watcher.
- **Search**: SQLite FTS5 over the stored messages' plain text, in a new migration (the bundled SQLite's FTS5 checked
  first).
- **Sign-in**: WebKitGTK through `webkit2gtk` (GTK 3, `webkit2gtk-4.1`) or the GTK 4 bindings, whichever binds a web
  process extension's request hook; ephemeral website data, no service workers, no new windows; the `nativeclient`
  redirect caught in the navigation policy before it loads.
- **Screenshots**: offline under `target/ui-shots/`; live under `<data_dir>/screenshots/<time>/` (`0700`).

## What Goes Where
- **Implementation Steps**: code, tests and docs in this repo, plus lab commands the agent runs on its own.
- **Post-Completion**: steps that need a human or a decision.

## Implementation Steps

### Task 1: The seeded own-state rules match their captured query
**Files:**
- Modify: `crates/proto-lab/src/classify.rs`, `crates/proto-lab/tests/fence_http.rs` (or a new test file),
  `crates/proto-lab/src/commands/login.rs` (records a capture), `docs/protocol/endpoints.md`
- Delete or shrink: `docs/backlog/seeded-own-rules-ignore-the-query.md`

- [ ] tests first: the Entra token endpoint is own-state only with one of the two query key sets the captures show
      (`client-request-id` alone, or with `brk_client_id`, `brk_redirect_uri` and `client_id`), each key once; authz
      only with no query; any other key set or a repeated key is `Unknown`
- [ ] `login` records its run like a scenario (fail-closed, under `captures/login/<ts>/`), so the sign-in form's
      queries can be reviewed; until a capture shows them, the sign-in form keeps its path match and the backlog item
      keeps that part (tightening it blind could break `login`, which runs only when a session has expired)
- [ ] live: `proto-lab run load --headless` and `proto-lab status a b c` still pass (every token post still own-state)
- [ ] `cargo test -p proto-lab` and the gate pass; the backlog item deleted or cut to the sign-in form

### Task 2: An unreadable own entry in an ordinary catch-up
**Files:**
- Modify: `crates/teams-core/src/{sync.rs,outbox.rs}`, `crates/teams-store/src/{outbox.rs,schema.rs}`, a new migration,
  `crates/teams-core/tests/sync.rs`, `crates/teams-store/tests/store.rs`
- Delete: `docs/backlog/catch-up-consumes-an-unreadable-entry-of-a-pending-row.md`

- [ ] tests first (plain wiremock, as the backlog's regressions): an ordinary catch-up meets an unreadable entry
      carrying a pending row's `clientmessageid`, moves on, and a later check must not send the row again
- [ ] the fix the backlog proposes: the row records (a new migration) that such an entry was seen, and its check
      treats the row as possibly arrived: it is not sent again on its own, only by an explicit resend; decide with
      Codex if the item's options leave a real choice
- [ ] `cargo test -p teams-core -p teams-store` and the gate pass; delete the backlog item

### Task 3: The UI sandbox
**Files:**
- Create: `crates/ui-sandbox/{Cargo.toml,src/lib.rs}`, `crates/ui-sandbox/tests/sandbox.rs`
- Modify: `spikes/gpui-video/{Cargo.toml,src/sandbox.rs,tests/*}` (onto the crate), `Cargo.toml` (member)

- [ ] move the spike's sandbox into `ui-sandbox` (a dev-dependency only, `[lints] workspace = true`): headless weston
      with `--fake-seat`, a nested niri, its own `WAYLAND_DISPLAY` and `NIRI_SOCKET`, screenshots through `niri msg`;
      `spikes/gpui-video`'s tests pass on it unchanged
- [ ] tests first: a private D-Bus session bus (`dbus-daemon --session` on a socket in the sandbox dir) with a fake
      notification server and a StatusNotifierItem watcher that record what they get; the user's bus is never reached
      (the test plants a canary on the inherited address and checks it stays silent)
- [ ] tests first: virtual keyboard and pointer into the nested niri (the Wayland virtual-input protocols niri
      offers), checked on a test window; a no-op URI opener on the sandbox's `PATH` that records what it was asked to
      open
- [ ] nothing inherits the user's `WAYLAND_DISPLAY`, `DBUS_SESSION_BUS_ADDRESS`, `NIRI_SOCKET` or `XDG_RUNTIME_DIR`
      content (test); without weston 15 or niri the sandbox tests say so and skip, and fail under `GPUI_SMOKE_REQUIRE=1`
- [ ] tests first: the sandbox's spawn passes through only the file descriptors its caller names (the credential
      broker's socket, the view trace) and closes every other one it inherits. So the lab's credentials reach a
      sandboxed process only on the broker's socket (Technical Details), never by the user's bus
- [ ] `cargo test -p ui-sandbox -p gpui-video` and the gate pass

### Task 4: `teams-app` and `teams-ui`: the lab binary and the engine bridge
**Files:**
- Create: `crates/teams-ui/{Cargo.toml,src/lib.rs,src/bridge.rs,src/models.rs}`, `crates/teams-app/{Cargo.toml,
  src/main.rs,src/secrets.rs}`, tests in both
- Modify: `Cargo.toml` (members, workspace deps), `crates/teams-core/src/engine.rs` (the request side split off, tests
  first; a query only if one is missing), `crates/teams-auth/src/secrets.rs` (if the broker's `SecretStore` lives there)

- [ ] tests first: the credential broker (Technical Details): `teams-app secrets --lab <alias>` serves `get`, `put`
      and `delete` of that account's own `teams-lab` keys on a socket pair and refuses any other account or
      application; the app's `BrokerSecretStore` over it passes `Session`'s renewal tests (a renewal's new tokens
      stored through it); no value in either side's output or `Debug`; the broker opens no window and no desktop
      client and exits when the socket closes; offline tests serve a `MemorySecretStore`
- [ ] tests first: `teams-app --lab <alias> --secrets-fd <n>` builds its parts as `teams-headless` does
      (`Fence::open_lab`, `Session` over the broker's socket, the region, `LiveMembershipSource::for_fence` with the
      transport's credential, the store at `LabPaths::store_file`, Trouter); refuses to start without
      `--secrets-fd` and never opens `KeyringSecretStore` itself; refuses a `test-support` build; refuses to open a
      window unless its Wayland socket is inside the directory `TEAMS_APP_SANDBOX` names (as the GPUI spike does);
      prints no token
- [ ] tests first: the bridge owns the `EngineHandle`; one background task drains `next_event` and fans each event out
      to every window's models on GPUI's foreground (two windows on a fake core both get it); the windows share only
      the request side, behind the `Core` trait; a query answers through its `Reply`; shutdown ends the drain task and
      consumes the owned handle with `shutdown`, off the foreground, joining the engine's thread
- [ ] a manifest test: `teams-ui` and `teams-app` depend on no HTTP or WebSocket client (`reqwest`, `tungstenite`,
      `hyper`, …): every request goes through `teams-core`
- [ ] the lab build's view trace: one JSON line per view change (view, conversation and message ids, kinds, times;
      never text), on a file descriptor the caller passes
- [ ] an empty window opens in the sandbox on `teams-mock` (in-process test, `test-support`), and its screenshot shows
      it
- [ ] `[lints] workspace = true`, listed in `members`; `cargo test -p teams-ui -p teams-app` and the gate pass

### Task 5: The chat list
**Files:**
- Create: `crates/teams-ui/src/chat_list.rs`, tests
- Modify: `crates/teams-ui/src/models.rs`, `crates/teams-core/src/engine.rs` (unread counts per conversation, if not
  there), `crates/teams-store/src/*` (a query, if needed)

- [ ] tests first (view tests on a fake core; in-process on `teams-mock`): conversations by last activity, unread
      counts from the store's horizon, kinds (1:1 named by the other member, the group chat by its topic or members,
      channels marked read-only, the account's own streams hidden), live updates from events, the connection state
      (connected, reconnecting, offline) from the push channel's events
- [ ] opens from the store at once, then catches up; nothing shown comes from anywhere but the store
- [ ] `cargo test -p teams-ui` and the gate pass

### Task 6: The chat view
**Files:**
- Create: `crates/teams-ui/src/{chat_view.rs,rich_text.rs}`, tests

- [ ] tests first: a render test per `RichText` node (marks, breaks, paragraphs, mentions highlighted, links opened
      through the URI opener (a no-op in tests), quotes, an image as a placeholder until Task 14); edited and deleted
      markers; reactions with counts and the account's own; `Control/*` never shown
- [ ] the newest at the bottom; older pages on scrolling up (`load_older`); an unread separator at the account's read
      horizon; the typing indicator from `Event::Typing`, cleared by `TypingStopped` or a timeout
- [ ] a long chat (1 000 messages from the mock) scrolls without loading everything at once (a measured bound in the
      test)
- [ ] `cargo test -p teams-ui` and the gate pass

### Task 7: The chat shapes the UI still needs (capture)
**Files:**
- Modify: `crates/proto-lab/src/scenario/ab_chat_full.rs` (or a new scenario), `docs/protocol/chat.md`,
  `tests/fixtures/chatsvc/*` (synthetic), `docs/protocol/endpoints.md`

- [ ] live, under the fence, in the a–b chat: each of the web client's default reactions (not only `like`), set and
      taken back; typing while a message is typed (how often it is sent) and whether `Control/ClearTyping` goes out;
      a mention of b in the group chat (`abc`). Every write on the a–b or the recorded group chat
- [ ] tests first for any mapping row the capture corrects; shapes into `chat.md` with their source, synthetic
      fixtures of every form seen
- [ ] live run recorded here (a breached run as CLAUDE.md says); `cargo test -p proto-lab -p fixture-guard` and the
      gate pass

### Task 8: Names and pictures
**Files:**
- Create: `crates/proto-lab/src/commands/profile_probe.rs`, `docs/protocol/graph.md`
- Modify: `crates/teams-proto/src/{http.rs,read.rs}` (a Graph service, `GET` only), `crates/teams-core/src/*`, a
  store migration (names and pictures cached), `docs/protocol/endpoints.md`

- [ ] a read-only probe first (`proto-lab probe profile <alias>`): Graph `GET /v1.0/users/<id>` and its photo for the
      lab accounts with our own Graph token; statuses, key names and sizes only; written up in `graph.md`
- [ ] if Graph answers: tests first (wiremock, synthetic fixtures): `Service::Graph` on the transport's allowlist
      (that host exactly, `GET` only, the Graph audience from the `Session`); names and pictures cached in the store,
      refreshed by age; the chat list, the chat view and the mention picker use them
- [ ] if it doesn't: names come from what the reads already give (a message's `imdisplayname`, the account names in
      `lab.toml`), pictures as initials, cached the same way. The middle tier's profile lookups (a native read by
      `POST`) wait for the user's decision on `PostRead` (Decisions for the user). Only if the user widens the rule:
      the `PostRead` entry for them, decided with Codex, tests and a `fence-seal` case first
- [ ] `cargo test -p teams-proto -p teams-core -p teams-ui -p fixture-guard` and the gate pass

### Task 9: Compose and the writes from the UI
**Files:**
- Create: `crates/teams-ui/src/compose.rs`, tests
- Modify: `crates/teams-core/src/{engine.rs,outbox.rs}` (`Action::Typing`), `crates/teams-ui/src/chat_view.rs`,
  `docs/protocol/endpoints.md`

- [ ] tests first (in-process, two accounts on `teams-mock`): plain text with line breaks and links → `MessageText`; a
      mention from the chat's members (names from Task 8), only by MRI; a reply with its quote; editing and deleting
      the account's own messages; the reactions Task 7 captured; each reaches the other account through the mock and
      leaves its `fence.log` line
- [ ] typing as a new engine action, sent no more often than Task 7 measured and only while the compose box has text
      and focus; tests that it is fenced like any write
- [ ] the read receipt when a chat is shown with the window focused, up to the newest message shown, debounced
- [ ] each message's outbox state shown (sending, sent, waiting, denied by the fence, failed); "send again" on a
      failed row or an earlier session's row is an explicit `Store::resend`; a denied write (an outsider mentioned on
      the mock) shows as denied and nothing is sent
- [ ] creating a group chat is not offered in the lab build (a native create is denied: CLAUDE.md)
- [ ] `cargo test -p teams-core -p teams-ui` and the gate pass

### Task 10: Notifications and the tray
**Files:**
- Create: `crates/teams-ui/src/{notify.rs,tray.rs}`, tests
- Modify: `Cargo.toml` (`zbus` in `[workspace.dependencies]`, version looked up live)

- [ ] tests first, on the sandbox's private bus: a notification for a message from someone else in a chat that isn't
      shown and focused (a mention marked as such), none for the account's own or for a replayed delivery; activating
      it opens the chat; the tray shows the unread total, with open and quit
- [ ] the bus address is the one the app is given, and a test checks the app never falls back to another
- [ ] `cargo test -p teams-ui` and the gate pass

### Task 11: Local search
**Files:**
- Create: `crates/teams-ui/src/search.rs`, a store migration (FTS5), tests
- Modify: `crates/teams-store/src/*`, `crates/teams-core/src/engine.rs` (a search query)

- [ ] check that the bundled SQLite has FTS5; tests first: the stored messages' plain text indexed (edits replace,
      deletes remove), a query across chats with its hits in order, a hit opens its chat at the message
- [ ] the server's search is not used (Substrate lists other tenant users: never let through in the lab)
- [ ] `cargo test -p teams-store -p teams-core -p teams-ui` and the gate pass

### Task 12: Teams and channels, read-only
**Files:**
- Create: `docs/protocol/channels.md`, `tests/fixtures/csa/*` (synthetic), tests
- Modify: `crates/teams-proto/src/*` (CSA's team reads, `GET` only), `crates/teams-mock/src/*` (a team, its
  channels), `crates/teams-ui/src/*`, `docs/protocol/endpoints.md`

- [ ] a read-only probe first: CSA's `GET api/v3/teams/users/me` (teams and channels) with our `chatsvcagg` token, and
      a channel's messages through the chat service's conversation reads; shapes into `channels.md`, synthetic
      fixtures
- [ ] tests first: teams with their channels in the sidebar; a channel's posts and reply chains shown read-only; no
      composer for a channel in the lab build, and no channel write exists natively (no sealed type for it)
- [ ] `cargo test -p teams-proto -p teams-mock -p teams-ui -p fixture-guard` and the gate pass

### Task 13: Presence, read only
**Files:**
- Modify: `crates/proto-lab/src/post_read.rs`, `crates/proto-lab/tests/post_reads.rs`, `crates/teams-proto/src/*`,
  `crates/fence-seal/tests/ui/*`, `crates/teams-ui/src/*`, `docs/protocol/endpoints.md`, `docs/protocol/auth.md`

- [ ] tests first: the browser fence lets UPS's `POST v1/presence/getpresence/` out as a read by `POST` only in its
      captured shape and only when every MRI it names is a lab account
- [ ] live: capture its request and answer (an `ab-chat` run); allowed only if the answer names nobody but the
      accounts asked for; answer `auth.md` question 4 (the presence token's audience)
- [ ] natively, presence isn't shown in the lab (a "not available" state in the views, tested) until the user decides
      on `PostRead` (Decisions for the user); this task's browser capture is the evidence for that decision. Only if
      the user widens the rule and the capture allowed it: the native `PostRead` entry for it, polled at a bounded
      interval for the lab accounts in view, decided with Codex, tests and a `fence-seal` case first
- [ ] the account's own presence is not set and shown as not reported (Decisions for the user)
- [ ] `cargo test -p proto-lab -p teams-proto -p teams-ui -p fence-seal` and the gate pass

### Task 14: Images: settle sending, then show them
**Files:**
- Modify: as `docs/backlog/ams-image-sending.md` lists (`crates/proto-lab/src/classify.rs`,
  `crates/teams-proto/src/{http.rs,chat/*}`), `docs/protocol/chat.md`, `docs/protocol/endpoints.md`

- [ ] the backlog item's step 1: a reviewed one-time mapping of exactly the captured object creation (the async
      gateway host only, the captured headers and body, one `permissions` key that is the a–b chat), judged by
      `Fence::check`, uploads and forwards still denied; then a read-only `GET` of that object: does it list its
      readers, completely? Decide with Codex; a breached run as CLAUDE.md says
- [ ] only if that read is authoritative: steps 2 and 3 (the upload judged by a fresh read of the object's readers,
      the forward race assessed; the captured upload answer and message post; a native `SendImage`), then viewing:
      the media service's token exchange reviewed as own-state in its captured shape, images fetched by `GET` and shown
- [ ] otherwise: images stay deferred, shown as a placeholder with an "open in Teams on the web" link, and the backlog
      item records what this task learned
- [ ] tests for whatever is built; the gate passes

### Task 15: Files and the calendar (Graph, read-only)
**Files:**
- Create: `docs/backlog/` item if file sending is deferred
- Modify: `docs/protocol/{chat.md,graph.md,endpoints.md}`, `crates/teams-proto/src/*`, `crates/teams-ui/src/*`

- [ ] files: capture the web client sending a file in the a–b chat with every write of it denied (as M1 Task 20 did
      for images), write up what it sends in `chat.md`, and decide with Codex whether its sharing can be fenced (its
      readers are the chat's members); if not, file it to the backlog. A received file card (if a capture ever shows
      one) opens its link through the URI opener
- [ ] calendar: a read-only probe of Graph's calendar view for a lab account, then tests first: an agenda view of the
      account's events from Graph `GET`s; a meeting's join link opens in the browser (meetings are M4)
- [ ] `cargo test -p teams-proto -p teams-ui -p fixture-guard` and the gate pass

### Task 16: The sign-in window
**Files:**
- Create: `crates/teams-signin/{Cargo.toml,src/main.rs,src/guard.rs}` (and its web process extension), tests
- Modify: `crates/teams-auth/src/*` (the parent's side), `crates/teams-app/src/main.rs`

- [ ] tests first, offline, in the sandbox: a local fake sign-in page that redirects to the `nativeclient` URI with a
      code and the request's `state`; the redirect is caught in the navigation policy before it loads, the code is
      handed to the parent on a pipe (never logged), and `teams-auth` redeems it only for the account the ID token
      names
- [ ] tests first: the request guard lets out only the Entra sign-in hosts and their static hosts (from
      `endpoints.md`); a page's fetch, beacon, image, frame, worker and WebSocket to any other host are refused;
      website data is ephemeral, service workers off, no new windows
- [ ] decide with Codex whether the guard covers everything the page can send, as the browser fence does for Chrome
      (CLAUDE.md: nothing runs live against the tenant before the fence is armed). Only then: one live sign-in of a lab
      account through it, under the live lock, only Entra's sign-in form let through, stopped by any multi-factor
      prompt. Otherwise the webview stays offline-tested and the lab keeps signing in through `proto-lab auth`
- [ ] `cargo test -p teams-signin -p teams-auth` and the gate pass

### Task 17: The personal build: design, then the user decides
**Files:**
- Create: `docs/personal-build.md`

- [ ] write down what a personal build is: the user's account instead of a lab roster; what the fence becomes for it
      (every write still a sealed type, sent only from an explicit action in the UI, logged; which targets it may
      reach); what the lab can't show and the personal build would need (presence, the server's search, channel
      writes, sending files and images); how a personal binary is told apart from a lab binary, so that no agent's
      build can become one; and the Conditional Access question for the user's account (`auth.md` question 6)
- [ ] ⚠️ stop here for the user's decision (Decisions for the user). No code that loosens the fence for a personal
      build is written before it; agents never build or run one. Until then M2's exit is the lab's
- [ ] `cargo test -p fixture-guard` passes

### Task 18: Verify M2 acceptance criteria
**Files:**
- Create: `crates/teams-app/tests/ui_live.rs` (`required-features = ["live"]`), `docs/m2-acceptance.md`

- [ ] the live test is an orchestrator only: it starts the UI sandbox, spawns the app built by `cargo build -p
      teams-app --target-dir target/lab` (path from `TEAMS_APP_BIN`) for a, b and c, one app process each, drives them
      with the sandbox's input, reads their view traces and `fence.log`; it opens no fence, holds no token and sends
      nothing itself
- [ ] credentials: for each account the orchestrator starts that binary's broker (`teams-app secrets --lab <alias>`)
      outside the sandbox, with the user's session bus, and hands the socket's other end to the app it spawns in the
      sandbox (`--secrets-fd`). It never reads or writes the socket itself, and the app's environment carries only the
      sandbox's bus. Every broker has exited when the run ends (checked); no token in the orchestrator's, the
      brokers' or the apps' output
- [ ] (`PROTO_LAB_LIVE=1`): a types and sends N = 20 messages to b; each is shown in b's chat view; **send → shown
      p95 under 2 s** (p50/p95/max reported); an edit and a reaction shown; a message to the group chat shown by b
      and c; b's notification raised on the sandbox's bus for a chat it isn't viewing; b's app closed, a sends, b's
      app reopened shows every message once
- [ ] `fence.log` for the run: allows only on the lab chats, no deny of a mapped write, own-state and frames only from
      the reviewed lists; screenshots of each step under `<data_dir>/screenshots/<time>/`, never the repo
- [ ] `proto-lab run ab-chat --headless` and `abc-chat` still exit 0
- [ ] the gate passes; write `docs/m2-acceptance.md` (numbers, fence summary, flakes, any breached run next to its
      clean rerun)

### Task 19: [Final] Update documentation
- [ ] README.md: the M2 crates, `teams-app`, the UI sandbox, the live UI test
- [ ] CLAUDE.md: the patterns M2 settled (the UI sandbox, the bridge, the credential broker, the view trace, any new
      request class the user approved)
- [ ] `docs/protocol/endpoints.md` checked against every endpoint M2 touched
- [ ] write the M3 plan (calls), informed by M2
- [ ] move this plan to `docs/plans/completed/`

## Validation commands

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/lint-report.sh
PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1 cargo test --workspace
cargo test -p fixture-guard
cargo build -p proto-lab -p teams-headless -p teams-app --target-dir target/lab
target/lab/debug/proto-lab run ab-chat --headless
target/lab/debug/proto-lab run abc-chat --headless
TEAMS_APP_BIN=$PWD/target/lab/debug/teams-app PROTO_LAB_LIVE=1 \
  cargo test -p teams-app --features live --test ui_live
TEAMS_HEADLESS_BIN=$PWD/target/lab/debug/teams-headless PROTO_LAB_LIVE=1 \
  cargo test -p teams-headless --features live --test chat_live
```

Every live command above runs under `flock ~/.local/share/teams-lab/live.lock` with an outer `timeout --signal=INT`.

## Roadmap after M2
- **M3 — calls**: first resolve `docs/backlog/fence-call-follow-ups.md` (an authoritative call-membership read), then
  the `sdp-ngc-1.0` conversion and the echo call (`docs/spikes/echo-call.md`), then 1:1 and group media.
- **M4 — meetings**: joining scheduled meetings, lobby, roster, meeting chat, content sharing.
- **M5 — long tail**: breakout rooms, live captions, recording indicator, background blur (ONNX), PSTN,
  `teams-webhost` (WebKitGTK + JS SDK shim: tabs, Loop, Whiteboard), and **bots and message extensions**, with
  Adaptive Cards and their actions: the invokes carry the account's token to the bot's backend and are denied until a
  fence rule decides the target (bot and conversation) and the token
  (`docs/backlog/bot-invokes-and-message-extensions.md`; the user decided on 2026-10-21 to leave them for the long
  tail and not to forget them).
- **Fence work that waits for evidence or the user** (any milestone that needs it):
  - **native group-chat enrolment**: admitting a group chat the native client creates, by binding the id the service
    returns to the exact create the fence permitted, then the same fresh reads; a change to the group-chat hard rule
    (`docs/backlog/group-chat-member-read-completeness.md`);
  - **image sending**, if Task 14 leaves it deferred (`docs/backlog/ams-image-sending.md`), and file sending, if
    Task 15 does;
  - channel writes, if the user approves a team of the lab accounts only; the account's own presence, if the user
    decides; the server's search, in the personal build only.
- **Cross-cutting**: drift detection (a scheduled bundle fetch and a diff report), the sanitizer's known gaps,
  `login`'s one-time-code prompt.

## Post-Completion
*Items requiring a human or a decision — informational only.*

**Manual steps:**
- None expected for the lab: the accounts are signed in and `proto-lab auth` keeps their tokens. If Microsoft asks a
  lab account for a one-time code, `login` stops (`docs/backlog/one-time-code-sign-in-under-the-fence.md`) and the
  user is asked.

**Decisions for the user:**
- **The personal build** (Task 17): what its fence is, how it is told apart from a lab binary, and when the user
  switches to it. Agents never build or run it.
- **The account's own presence**: whether the native client may set it (a write the whole tenant sees), and how.
- **A native read by `POST` (`PostRead`)** (Tasks 8, 13): CLAUDE.md's hard rule "The native client sends only what
  its sealed policy allows" lets out three things only: a read (`GET`, no body), an entry of the sealed `OwnState`
  list, and an `Authorized<Mutating>` write. Nothing else can go out. A `POST` that reads other lab accounts' data
  (the middle tier's profile lookups, UPS's `getpresence`) is none of these, so the native client can't send it,
  though the browser fence may allow it under the 2026-10-21 allowance rules. What would change: the rule would gain
  a fourth sealed list, `teams-proto`'s `PostRead`, under the same 2026-10-21 rules applied natively. Each entry
  would be added only after a review, in its exact captured shape (host, method, path, every query parameter, the
  headers that change what it does, the body schema). Every MRI it names would be checked against the lab roster when
  the fence decides (`Fence::lab_accounts_only`), and any other id would make it `Unknown`. Its captured answer could
  hold only the accounts asked for. It would be logged to `fence.log` before it is sent, and not sent if it can't be
  logged. It would carry no token to a third party, and a `fence-seal` case would prove nothing else mints one. Why:
  Graph may not give names and pictures (Task 8), and presence has no `GET` (Task 13). Until the user decides, the
  native client sends no `POST` read: names come from what reads already give, pictures are initials, and presence
  isn't shown. CLAUDE.md isn't changed by this plan; if the user widens the rule, the rule's text changes first, then
  Tasks 8 and 13 build on it.
- **A lab-only team**: whether to create a team of a, b and c only (a group the tenant's directory shows), so that
  channel writes can be mapped and tested.
- **Native group-chat enrolment**: whether the group-chat rule may admit chats the native client creates, and on what
  binding (roadmap).
- Before the personal build: whether the user's own account passes Conditional Access with the native client
  (`docs/protocol/auth.md` question 6); the first step of the personal build, which agents never take.
