# Native Teams for Linux — M2: GPUI Daily Driver

## Overview
- **Goal (the user's, 2026-10-21): a working app the user uses as their daily driver for Teams chats.** M2 puts a
  GPUI window on M1's core: a chat list, a chat view and compose, desktop notifications and a tray icon, local
  search, names and pictures, presence (others' shown, the account's own set), images and received files, teams and
  channels read-only, and a sign-in window on an embedded WebKitGTK webview. It is built and proven on the lab first
  (one lab account per process, under the fence, the way `teams-headless` runs), then turned into a personal build
  for the user's own account under rules the user approves.
- **Exit:** GPUI UI tests and the live lab acceptance (Task 25) pass, the personal build's rules are approved and
  written into CLAUDE.md (Task 26), the personal build is implemented and tested on the mock (Task 27), and the user
  builds it, signs in and uses it (Post-Completion). Agents never run the personal build against the tenant, never
  produce an installed or release personal binary, and never receive the user's tokens (what "build" means for the
  gate: Task 26). The daily driver may ship with images and files as placeholders, if Tasks 21 and 23 leave them
  deferred.
- What M1 changes in the roadmap line (each one decided by a hard rule, a finding or the user):
  - **Adaptive Cards go to the long tail with bots.** Every card in a chat comes from a bot or a connector; none is in
    the lab, so no capture can show the message shape a renderer would read, and every card action is a bot invoke,
    which carries the account's token to the bot's backend and stays denied
    (`docs/backlog/bot-invokes-and-message-extensions.md`, the user's decision of 2026-10-21).
  - **Search is local.** The server's people and message search (Substrate) lists other tenant users, so the hard
    rules never let it through in the lab. M2 searches the messages the account's store holds, in both builds.
  - **Teams and channels are read-only in M2**, in both builds (the user's decision of 2026-10-21): the lab accounts
    share their tenant teams with people outside the lab, so channel writes stay unmapped and denied
    (`docs/backlog/channel-writes-need-a-lab-only-team.md`, Task 16).
  - **The calendar leaves M2** (the user's decision of 2026-10-21: the daily driver is for chats). A meeting link in a
    chat opens in the browser.
  - **A native read by `POST` becomes a sealed class of its own, `PostRead`** (the user's decision of 2026-10-21,
    Task 11): names and presence need the middle tier's profile lookups and UPS's `getpresence`, which change nothing
    but are `POST`s, so today's rule lets the native client send neither.
  - **The account's own presence is researched and set** (the user's decision of 2026-10-21, Tasks 18–20): an app that
    sets none could show its user offline all day.
  - **Images and files: sending is decided from a capture first,** as M1 Task 20 did for images
    (`docs/backlog/ams-image-sending.md`). No image or file has ever been received in the lab, so viewing needs a
    sending path or a captured shape first.
- **Every write goes through the fence**, and the native client sends only what its sealed policy allows (CLAUDE.md's
  hard rules). The UI gets no request path of its own: the app has no HTTP or socket client, only `teams-core`'s
  engine (and GPUI's own HTTP client stays the null one, Task 6). A new request class is added only as those rules
  allow, with its row in `docs/protocol/endpoints.md`.
- **Zed is a design reference only.** Zed's UI crates (`ui`, `theme`, `markdown`, `picker`, `editor`, `agent_ui`,
  the removed chat panel) are GPL-3.0-or-later; no Zed code is copied into this repo. The building blocks come from
  `gpui-component` (Apache-2.0, built for `gpui-pre =0.3.9`, Task 7) and gpui-pre's own examples (Apache-2.0).
- Out of scope: calls (M3: `docs/backlog/fence-call-follow-ups.md`), meetings (M4; a meeting link opens in the
  browser), the calendar, bots, message extensions and Adaptive Cards (M5), channel writes, server search, starting
  new chats (people search is Substrate), Loop, Whiteboard and tabs (M5's `teams-webhost`), group chats the native
  client creates (their enrolment: `docs/backlog/group-chat-member-read-completeness.md`).

## Decisions of 2026-10-21 (brainstorm with the user)
- M2 keeps both the sign-in window (Task 24) and the personal build (Tasks 26–27); the personal build comes after the
  app is proven on the lab.
- Scope beyond the core (chat list, chat view, compose, notifications, tray, local search, names): images, received
  files, presence (others' and the account's own), teams and channels read-only. The calendar moves to a later
  milestone.
- `PostRead`: approved as a fourth sealed list of the native client. Lab builds: every MRI it names a lab account at
  decision time. The personal build's version is decided with its rules (Task 26).
- Own presence: researched in the lab and set by a sealed write. A lab account's own availability is seen
  tenant-wide, so no participant check can pass for it; the user approved a narrow exception: a lab account may set
  its own availability (in its captured shapes, availability values only; never a status note, out-of-office or a
  profile field). Task 18 writes it into CLAUDE.md before any code.
- Channels: read-only in M2, in both builds.
- Look: Teams-like and compact: three panes (a left rail, the list, the conversation), light and dark following the
  system. Keyboard-first: a chat switcher (Ctrl+K) and shortcuts for reply, edit and react.
- UI kit: `gpui-component` after an audit (Task 7); our own widgets on gpui-pre's examples if it fails.
- Personal build direction (Task 26 writes it up formally for approval): a separate `teams` binary on the same
  `teams-ui`; the account signed in through the sign-in window; its own keyring name (`teams-personal`) and data dir
  (`~/.local/share/teams/`); no broker, no sandbox; writes still sealed types under a personal fence (the target a
  conversation the account is a member of, every write logged, read receipts and typing behind settings, presence
  following the app); reads include `PostRead` without the lab limit; lab and personal binaries refuse each other's
  data dir and keyring name; agents compile it and test it on the mock, never run it against the tenant.

## Context (from M1)
- M1 is done (`docs/plans/completed/20261020-native-teams-m1-headless-chat-core.md`, `docs/m1-acceptance.md`). What
  exists:
  - `teams-core::engine::start(Parts { store, client, outbox, push, options })` → `EngineHandle`: its own
    current-thread runtime on a thread of its own; `write(Action)` (`Send`, `Edit`, `Delete`, `React`, `Unreact`,
    `Read`, `CreateChat`), `sync`, `load_older`, store queries (`conversations`, `messages`, `message`, `reactions`,
    `members`, `outbox_entry`), each answering a `Reply` (a future on any executor, or `blocking_wait`); events
    through `next_event` (`Connected`, `Registration`, `Disconnected`, `PushStopped`, `CaughtUp`, `CatchUpFailed`,
    `Message`, `Typing { typing }`, `ReadHorizon`, `ConversationChanged`, `Outbox`, `Failed`). A write answers with
    its row even when it waits or failed. There is no unread-count query yet: the store has
    `unread_count(conversation, me)`.
  - `teams-proto::chat`: the conversation and message model (`Field::Unknown` for unseen forms), `RichText` (text with
    marks, breaks, paragraphs, mentions, links, images, quotes; sanitized, depth- and size-capped) and `MessageText`
    (a message's markup, its mentions and its target from one value; no raw HTML, no images yet). `Typing` is a
    sealed, mapped write; `Control/ClearTyping` is known as a message type but has no write (never captured).
  - `teams-store`: conversations, members, messages by version, reactions, read horizons, unread counts, sync state,
    the outbox (`Store::resend`, `outbox_left_by_earlier_runs`), runs (each open store is a run holding its own
    lock; several processes may open one account's store at once, so nothing yet stops two engines on one account),
    `read_horizon`; migrations up to `0003`. The engine exposes none of `resend`, the earlier runs' rows, the read
    horizon or the unread count, and `CatchUpReport` carries counts only (not which conversations changed);
    `Event::Message` comes only from push deliveries.
  - `teams-trouter`: the push channel, its own frames only (no `user.activity`), deliveries never answered;
    `teams-auth::session::Session` (a token per audience, renewals) over a `SecretStore` (`KeyringSecretStore`,
    `MemorySecretStore`); `teams-fence`'s `LiveMembershipSource::for_fence` with a `CredentialSource`,
    `Fence::lab_accounts_only`.
  - `teams-headless` (the lab CLI, with a lib beside `main.rs` so its commands are tested on mock engines) and its
    live acceptance test, an orchestrator that spawns the normally built binary (`--target-dir target/lab`).
  - `teams-mock`: the chat service, the registrar and Trouter from synthetic fixtures, delivering each write to the
    other members' sockets; reached only through the `test-support` mock origins.
  - GPUI: `gpui-pre` 0.3.9 vendored with agterm-rs's patches (`scripts/vendor-gpui.sh`); the vendored copy has no
    `examples/`, the crate's own are in the cargo registry (`input.rs`, `view_example/`, `list_example.rs`,
    `system_notifications.rs`). The M0 spike (`docs/spikes/gpui-video.md`) runs GPUI on a niri nested in a headless
    weston (`spikes/gpui-video/src/sandbox.rs`: its own Wayland socket, screenshots through `niri msg`, never the
    desktop session); the spike binary refuses to open a window outside that sandbox. niri offers
    `zwlr_virtual_pointer_manager_v1` and `zwp_virtual_keyboard_manager_v1`.
  - GPUI's Linux platform: `App` starts with a `NullHttpClient` (`set_http_client` would give it a real one); it
    opens a URI through the portal on the session bus, then `xdg-open`; it has `show_system_notification`; it links
    `ashpd` and `zbus` in the async-io flavour, which is why `oo7` is built with `async-std` (workspace features
    unify: a `tokio` flavour of either breaks every workspace build with GPUI in it).
  - On the machine: WebKitGTK 2.54 (`webkit2gtk-4.1` for GTK 3, `webkitgtk-6.0` for GTK 4), weston 15, niri 26.04,
    `dbus-daemon`; `gpui-component` 0.7.3 in the cargo registry (Apache-2.0, depends on `gpui-pre =0.3.9`).
- Design references (study only, GPL, nothing copied):
  - Zed's channel chat, removed in `3a437fd888` (2025-09-09); the last tree with it is `96c429d2c3`
    (`crates/collab_ui/src/chat_panel.rs`, `chat_panel/message_editor.rs`, `crates/channel/src/channel_chat.rs`; the
    local Zed checkout at `../../temp/zed` is shallow, so read them with `gh api -H "Accept:
    application/vnd.github.raw" "repos/zed-industries/zed/contents/<path>?ref=96c429d2c3"`): a bottom-aligned
    `ListState`, older pages loaded when fewer than 50 rows are above the view, a sender's messages grouped within
    5 minutes, a "New messages" divider at the read mark, reply previews that jump to the original, hover actions,
    rich text cached per message id, Enter sends and Shift-Enter breaks the line, mentions completed from members.
  - Zed's `agent_ui` thread view (`../../temp/zed/crates/agent_ui/src/conversation_view/thread_view.rs`): a tail-
    following `ListState` (`FollowMode::Tail`), rows remeasured when they grow, the scroll position saved through
    `cx.defer`.
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
  - **Presence** (`docs/protocol/trouter.md`, "`user.activity` and presence"): the web client sends `user.activity`
    (`active`/`inactive`; accepted in the browser by the user's decision of 2026-10-19), `PUT …/ups/<geo>/v1/me/
    endpoints/` (availability) and `POST …/v1/pubsub/subscriptions/<epid>` (others' presence), and reads others with
    `POST v1/presence/getpresence/`; every presence request was denied, so what makes an account show online was
    never tested. Whether presence accepts our token's audience is `auth.md` question 4.
  - **Teams' chat list came from CSA** (`chatsAndChannelsByIds`, a read by `POST`, denied); the chat service's own
    conversation list works natively and lists channels as read-only conversations. CSA's team reads by `GET`
    (`api/v3/teams/users/me`) are "seen, not reviewed".
  - **Two live-only bugs** (a signed delivery id, a base64url endpoint id) passed every offline test: fixtures have to
    cover a field's whole captured range.
- What stays owed from M1 (`docs/backlog/`):
  - `seeded-own-rules-ignore-the-query.md` (worth: yes): Task 1.
  - `catch-up-consumes-an-unreadable-entry-of-a-pending-row.md` (worth: yes): Task 2; a daily driver keeps the core
    running for days, so the window it describes is no longer rare.
  - `ams-image-sending.md` (later): Tasks 21–22.
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
  once a capture showed it, and a new read, own-state or `PostRead` class is added only as CLAUDE.md's allowance rules
  say.
- **a hard-rule change is written into CLAUDE.md first**, in the words of the user's decision (this plan's
  "Decisions of 2026-10-21", or the user's answer at Task 26), before any code that relies on it.
- **no window, bus or handler of the user's desktop** for any lab or test process: every GUI test, offline or live,
  runs in the UI sandbox (a nested niri on a headless weston, a private D-Bus session bus, a no-op URI opener); a
  client is pointed at the sandbox's socket or bus explicitly, and a test checks the client honours it. The lab app
  refuses to open a window outside the sandbox. The lab's tokens stay in the Secret Service on the user's bus, which
  the sandboxed app never reaches: it gets them only through the credential broker (Task 4), a process outside the
  sandbox. (The personal build runs on the user's desktop; agents never run it.)
- **test-only reach stays behind `test-support`**, as in M1; the lab app and the personal app refuse to run when
  built with it.
- **no Zed code is copied** (GPL); `gpui-component` and gpui-pre's examples are Apache-2.0 and may be used.
- **captures and screenshots of the live lab stay outside the repo**: captures under the lab's `captures/`, live
  screenshots under `<data_dir>/screenshots/` (they show lab and tenant data). Fixtures are synthetic-from-capture and
  pass `cargo test -p fixture-guard`; a fixture covers every form of a field the captures show.
- live runs come from a normal build (`--target-dir target/lab`), under the live lock with an outer `timeout
  --signal=INT`, one process per lab account and one browser per lab profile at a time; nothing is left running. No
  write ever comes from a `cargo test` process.
- **a breached live browser run** is handled as CLAUDE.md says.
- "decide with Codex" means a `codex-discussion`; its outcome is recorded in the task (and in `docs/protocol/` when it
  settles a protocol question).
- protocol notes go to `docs/protocol/` in our own words; every endpoint touched gets its row in `endpoints.md`.
- every version written (a crate, a toolchain) is looked up live at the time, never taken from memory.

## Testing Strategy
- **unit / view**: `cargo test --workspace`, offline. Views tested on GPUI's test context (`gpui-pre`'s
  `test-support`, a dev-dependency) against a fake engine (the `Core` trait in `teams-ui`); models against a real
  store file.
- **whole app, offline**: the app's views run in-process in the UI sandbox against `teams-mock` (two or three accounts,
  each its own engine, fence and `fence.log`), driven by the sandbox's virtual keyboard and pointer, read back by the
  app's view trace and by screenshots checked for coarse properties (window present, regions drawn). Screenshots of
  the offline runs are test outputs under `target/ui-shots/`, never committed.
- **fence suite**: as in M1, plus: `teams-ui` and `teams-app` depend directly on no HTTP or WebSocket client, and
  `gpui-component`'s tree holds none for the host target (a manifest test over `cargo metadata --filter-platform`);
  `zbus` is used only on the bus the app is given, for notifications and the tray; GPUI's HTTP client stays the null
  one; every write in a UI test reaches the mock through `Fence::check` with its `fence.log` line; a `fence-seal`
  case for each new sealed class (`PostRead`, the own-presence write, the personal fence).
- **fixture guard**: `cargo test -p fixture-guard` on every `tests/fixtures/` and `docs/`.
- **live lab**: `--features live` tests, run only with `PROTO_LAB_LIVE=1`: an orchestrator that starts the UI sandbox,
  spawns the normally built app for a, b and c in it, drives them by keyboard (the chat switcher and the compose
  box's shortcuts, no screen coordinates), reads their view traces and `fence.log`, and keeps screenshots under the
  lab's data dir; plus `proto-lab run …` for the captures.
- **latency**: send → shown on the other account's screen, per message, p50/p95/max over N = 20.
- **personal build**: tested only on the mock (its lib, as `teams-headless`'s commands are); nothing runs it against
  the tenant but the user.

## Progress Tracking
- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

```
teams-app (lab: one account, GPUI) ─┐
teams (personal: the user's account)┴─ teams-ui (views, models, gpui-component) ── EngineHandle: actions in, events out
      │                                                                                  │
      │ spawns, reads the code                                                           ▼
teams-signin (WebKitGTK, its own process) ──▶ teams-auth          teams-core ──▶ teams-proto / teams-trouter / store
ui-sandbox (tests and lab runs only: weston + nested niri, private D-Bus, virtual input, screenshots, no-op opener)
```

- **`teams-ui`**: GPUI views and their models over an engine seam: the shell (rail, list, conversation), the chat
  list, the chat view, compose, the chat switcher, search, teams and channels, presence, settings (read receipts,
  typing, notifications, the account's status). The bridge owns the engine handle; one background task drains its
  events and updates the models (GPUI entities the views observe) on GPUI's foreground; queries are `Reply` futures.
  No view builds a request.
- **`teams-app`**: the lab binary, `teams-app --lab <alias> --secrets-fd <n>`: `Fence::open_lab`, the account's
  `Session` over the credential broker's socket, `LiveMembershipSource::for_fence` with the transport as its
  `CredentialSource`, the store at `LabPaths::store_file`, Trouter. It refuses a `test-support` build, refuses to
  open a window outside the UI sandbox, and prints no token. Notifications and the tray go to the session bus it is
  given. It writes a view trace (ids and kinds of what each view shows, never message text) for the live test.
  `teams-app secrets --lab <alias>` is its credential broker, started outside the sandbox (Task 4).
- **`teams`** (Task 27): the personal binary, on the same `teams-ui`, under the rules Task 26 settles.
- **`teams-signin`**: the sign-in window, a separate process (GTK's main loop apart from GPUI's): Microsoft's page in
  a WebKitGTK webview behind a request guard; it hands the redirect's code to its parent, and `teams-auth` does the
  rest (PKCE, exchange, the ID token's account check).
- **`ui-sandbox`**: the M0 spike's sandbox made a crate (a dev-dependency of the tests, a dependency of the live
  orchestrator), with a private bus, virtual input and a no-op URI opener; `spikes/gpui-video` moves onto it.
- **`teams-core` grows** only the actions and queries the UI needs (Task 5: the request split, the account lock, the
  outbox and horizon queries, catch-up change reports, transient writes; later: local search, names, presence), each
  tested on the mock first.

## Technical Details
- **The bridge**: the engine stays on its own thread and runtime. `EngineHandle` has one event receiver,
  `next_event` (and `try_next_event`, `blocking_next_event`) takes `&mut self`, and `shutdown(self)` consumes it, so
  it can't sit behind an `Arc`: `teams-ui`'s bridge owns it. Its request side (writes and queries, each answering a
  `Reply`) is split off as a cloneable handle (`teams-core` grows that split, which M1 doesn't have: the request
  sender cloned, the receiver and the thread kept by the owner) and shared behind a trait (`Core`), so views are
  tested on a fake. One background task drains `next_event` and applies each event to the models on GPUI's
  foreground through its async context; views observe the models; a view asks for data with a query `Reply`. One
  main window; the models are shared, so a second view of the same data (the switcher, search) sees every update.
  Shutdown ends the drain task and calls `shutdown` on the owned handle off GPUI's foreground (it blocks until the
  engine's thread is joined). A message the account sends shows at once from its outbox row (`Event::Outbox`), then
  as the message once it is sent.
- **One engine per account**: the engine's owner takes an exclusive per-account lock (`flock` with try-lock on
  `<store>-runs/engine.lock`) before it starts; a second process on the same account (another `teams-app`, a
  `teams-headless`, a second `teams`) is told and exits. Two engines would mean two Trouter sockets and doubled read
  receipts, typing and presence. The store's own run locks stay as they are.
- **What the UI reads from the engine** (Task 5): the existing queries plus `unread_count`, `read_horizon`, a
  conversation's outbox rows, the earlier runs' rows and `resend`; a catch-up reports which conversations it changed
  (`ConversationChanged` per synced conversation, and `Event::Message` for each message it newly stored, marked as
  from a catch-up), so the models and notifications see what a reconnect brought.
- **Transient writes** (typing, the account's own availability): the outbox grows a transient kind rather than a
  second path (the outbox stays the only way a write goes out). Each send is a fresh `Fence::check` with its
  `fence.log` line and the transport's rules, but no row is stored: nothing is checked for, retried after an unknown
  outcome, adopted by a later run or offered for "send again"; an unknown outcome is dropped; a newer one replaces
  one still waiting (latest wins). A durable row for `Control/Typing` would never settle (`Control/*` never settle a
  row) and would go out again at a moment the user didn't choose.
- **Credentials: the broker.** `teams-headless` reaches the lab's tokens through `KeyringSecretStore`, which is `oo7` on
  the session bus named by `DBUS_SESSION_BUS_ADDRESS` (oo7 0.6 stable API: no other connection), and `Session` reads and
  writes the store for the whole run (a renewal stores the new tokens). The sandboxed app's session bus is the
  sandbox's, and it must hold no connection to the user's bus at all (oo7's `unstable` API could take another
  connection, but handing one in would break the desktop-bus rule). Instead `teams-app secrets --lab <alias>` runs
  outside the sandbox with the user's bus, holds the one `KeyringSecretStore`, and serves `get`, `put` and `delete` of
  that account's own `teams-lab` items, nothing else, on one end of a socket pair. The app (`--secrets-fd <n>`) gets the
  other end and uses it through a `SecretStore` of its own (`BrokerSecretStore`). The broker opens no window, starts no
  desktop client, and exits when the socket closes. Neither side logs a value, only the key's account and audience. The
  app has no fallback: without `--secrets-fd` it doesn't start, and it never opens `KeyringSecretStore` or a bus from
  its environment for secrets. So the desktop-bus rule holds: the sandboxed process reaches only the sandbox's bus, and
  the user's bus is reached only by the broker, for the lab's own items.
- **GPUI's own reach**: GPUI's `App` keeps its `NullHttpClient` (no `set_http_client` anywhere; a test that an
  `img()` of a URL loads nothing and sends nothing). Pictures and images reach a view only as bytes from the engine.
  Links open through GPUI's `open_url` (the portal on the app's bus, then `xdg-open`), which the sandbox's no-op
  opener catches (Task 3).
- **The UI kit**: `gpui-component` (Apache-2.0, `gpui-pre =0.3.9`, resolved to the vendored patched copy by
  `[patch.crates-io]`): `Textarea` for compose, `MessageScroller`/`Message`/`Bubble` (or GPUI's `ListState` directly)
  for the chat view, its command palette for the chat switcher, `Avatar`, its theme (light and dark from the
  window's appearance). Not its markdown/HTML text view: message text is rendered only from `teams-proto`'s
  sanitized, capped `RichText` (Task 9), never from raw HTML. Audited first (Task 7): the kit turns on `gpui-pre`'s
  default features (`x11`, `font-kit`), so a lab app refuses to start with `DISPLAY` set (GPUI would pick X11
  without `WAYLAND_DISPLAY`), and its theme file watcher (`watch_dir`) is never called. The app installs no
  `tracing` subscriber, so the kit's `tracing` events go nowhere; `log` stays capped as M1 set it.
- **Reads the UI needs, by request class**: the store first (everything shown comes from it); new service reads are
  `GET`s added to the transport's allowlist host by host (Graph `https://graph.microsoft.com/v1.0/` with a Graph
  token from the `Session`, CSA's team reads with its `chatsvcagg` token), each probed read-only first
  (`proto-lab probe …`, statuses, key names and sizes only). Reads by `POST` go through the sealed `PostRead` list
  (Task 11): the profile lookups (Task 12) and `getpresence` (Task 17), each entry in its exact captured shape, every
  MRI a lab account at decision time in a lab build. The transport knows only `Service::{ChatService, Registrar,
  Trouter}` today (`crates/teams-proto/src/endpoint.rs`): each new host is a new `Service` variant with its host (or
  the GTM it comes from), its audience, its fixed headers and its path check (the middle tier: `middleTier` GTM,
  `/api/mt/part/<partition>/beta/users/…`, `api.spaces.skype.com`, the captured header values; Graph; CSA with
  `chatsvcagg`; presence, its audience per `auth.md` question 4).
- **Writes the UI adds**: typing (`teams_proto::Typing`, already sealed and mapped; a transient `Action::Typing`, sent
  no more often than the capture shows) and the account's own availability (transient, Tasks 18–20); nothing else
  new. Reactions offer
  only the keys a capture showed.
- **Outbox states in the UI**: sending, sent, waiting (`Retry-After`), denied by the fence, failed. "Send again" on a
  failed row and on an earlier session's row is the explicit `Store::resend`, never automatic.
- **Notifications and tray**: notifications through GPUI's `show_system_notification` if it reaches
  `org.freedesktop.Notifications` on the bus the app is given (checked first, Task 14), else our own over `zbus`; the
  tray a StatusNotifierItem over `zbus`. `zbus` in the async-io flavour (no `tokio` feature), for the reason `oo7`
  is. In tests the sandbox's private bus with a fake notification server and tray watcher.
- **Search**: SQLite FTS5 (the bundled SQLite has it) over the stored messages' plain text. The migration only
  creates the table: the store keeps service HTML and `teams-store` can't turn it into text, so `teams-core` hands
  each message's plain text (from `RichText`) on upsert, and a Rust-side backfill indexes rows stored before. User
  input is quoted before `MATCH`.
- **Sign-in**: WebKitGTK. Neither Rust binding in the registry (`webkit2gtk` 2.0.2, `webkit6` 0.6.1) binds the web
  process extension's `send-request`; `webkit6` has `UserContentFilterStore` and `decide-policy`. The route (the
  content filter plus the policy, or a hand-FFI `cdylib` extension with its `unsafe` under the workspace lints) is
  Task 24's first decision. The guard classifies each request as proto-lab's `sign_in` classifier does (`GET` reads,
  the seeded own-state rules in their exact shapes after Task 1, everything else denied), never by host alone, and
  logs every decision to `fence.log`; ephemeral website data, no service workers, no new windows; the `nativeclient`
  redirect caught in the navigation policy before it loads.
- **Keyboard**: Ctrl+K the chat switcher; Enter sends, Shift-Enter breaks the line; Up in an empty compose box edits
  the account's last message; Escape cancels a reply or an edit; shortcuts for reply and react on the selected
  message. The live test drives the app only through these.
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

- [x] tests first: the Entra token endpoint is own-state only with one of the two query key sets the captures show
      (`client-request-id` alone, or with `brk_client_id`, `brk_redirect_uri` and `client_id`), each key once; authz
      only with no query; any other key set or a repeated key is `Unknown`
- [x] `login` records its run like a scenario (fail-closed, under `captures/login/<ts>/`), so the sign-in form's
      queries can be reviewed; until a capture shows them, the sign-in form keeps its path match and the backlog item
      keeps that part (tightening it blind could break `login`, which runs only when a session has expired). Task
      24's guard reuses these exact shapes
- [x] live: `proto-lab run load --headless` and `proto-lab status a b c` still pass (every token post still own-state)
- [x] `cargo test -p proto-lab` and the gate pass; the backlog item deleted or cut to the sign-in form
- Done (2026-10-21): beyond the key sets, each value is held to its captured form too (CLAUDE.md: "every query
  parameter and its value"): the request id a lower-case GUID (`Core-` and a GUID only in the lone form), the broker's
  client ids exact, the redirect URI one of the two Teams origins'. `login`'s capture keeps no request body (the
  sign-in form's posts carry the password; `Capture::without_request_bodies`). Live: `status a b c` all ok and `run
  load` ok, every token post `entra_token`; `login` wasn't run (no session had expired), so no `captures/login/` yet.

### Task 2: An unreadable own entry in an ordinary catch-up
**Files:**
- Modify: `crates/teams-core/src/{sync.rs,outbox.rs}`, `crates/teams-store/src/{outbox.rs,schema.rs}`, a new migration
  (`0004_*`), `crates/teams-core/tests/sync.rs`, `crates/teams-store/tests/store.rs`
- Delete: `docs/backlog/catch-up-consumes-an-unreadable-entry-of-a-pending-row.md`

- [x] tests first (plain wiremock, as the backlog's regressions): an ordinary catch-up meets an unreadable entry
      carrying a pending row's `clientmessageid`, moves on, and a later check must not send the row again
- [x] the fix: the row records (a new migration) that such an entry was seen, and its check treats the row as
      possibly arrived: it is not sent again on its own (the backlog's proposal: the check fails and gives up at
      `MAX_FAILED_CHECKS`), only by an explicit resend; the row's state says "may have arrived", which the UI shows
      before it offers "send again" (Task 13); decide with Codex if the item's options leave a real choice
- [x] `cargo test -p teams-core -p teams-store` and the gate pass; delete the backlog item
- Done (2026-10-21): migration `0004` adds `outbox.unreadable_seen_at` (the first sighting's time);
  `OutboxEntry::may_have_arrived()` is that record on a row not sent, the state Task 13 shows. Every read alike (an
  ordinary catch-up, a check, `load_older`) records it through `Outbox::unreadable_seen` for a message post of the
  entry's conversation, of any run, that had an attempt, then moves on: the check no longer refuses such a page (the
  old `Reader`/`guard_page` split is gone), it fails on the record (`UnreadableSeen`) as one that couldn't sync,
  until `MAX_FAILED_CHECKS`. The recorded row is told like an arrived one (`take_arrived`, so `Event::Outbox`).
  `Store::resend` clears the record: the resend is the owner's decision after the warning. No Codex round: the plan
  and Task 13 settle each option the item left (give up at `MAX_FAILED_CHECKS`, resend sends).

### Task 3: The UI sandbox
**Files:**
- Create: `crates/ui-sandbox/{Cargo.toml,src/lib.rs}`, `crates/ui-sandbox/tests/sandbox.rs`
- Modify: `spikes/gpui-video/{Cargo.toml,src/sandbox.rs,tests/*}` (onto the crate), `Cargo.toml` (member)

- [x] move the spike's sandbox into `ui-sandbox` (`[lints] workspace = true`): headless weston with `--fake-seat`, a
      nested niri, its own `WAYLAND_DISPLAY` and `NIRI_SOCKET`, screenshots through `niri msg`; `spikes/gpui-video`'s
      tests pass on it unchanged
- [x] tests first: a private D-Bus session bus (`dbus-daemon --session` on a socket in the sandbox dir) with a fake
      notification server and a StatusNotifierItem watcher that record what they get; the user's bus is never reached
      (the test plants a canary on the inherited address and checks it stays silent)
- [x] tests first: virtual keyboard and pointer into the nested niri (`zwp_virtual_keyboard_manager_v1`,
      `zwlr_virtual_pointer_manager_v1`), checked on a test window; a no-op URI opener on the sandbox's `PATH` that
      records what it was asked to open; a GPUI window's `open_url` in the sandbox reaches it (the private bus has no
      portal, so GPUI falls back to `xdg-open`), and nothing reaches the user's desktop
- [x] GPUI's start-up portal and settings calls on the private bus fail quietly (no hang, no fallback to another bus):
      a test window opens and draws with the bus holding only the fakes
- [x] nothing inherits the user's `WAYLAND_DISPLAY`, `DISPLAY`, `DBUS_SESSION_BUS_ADDRESS`, `NIRI_SOCKET` or
      `XDG_RUNTIME_DIR` content (test); without weston 15 or niri the sandbox tests say so and skip, and fail under
      `GPUI_SMOKE_REQUIRE=1`
- [x] tests first: the sandbox's spawn passes through only the file descriptors its caller names (the credential
      broker's socket, the view trace) and closes every other one it inherits. So the lab's credentials reach a
      sandboxed process only on the broker's socket (Technical Details), never by the user's bus
- [x] `cargo test -p ui-sandbox -p gpui-video` and the gate pass
- What was built, where it differs from the lines above:
  - The bus is `dbus-daemon --config-file` with a config of our own, not `--session`: the stock session config
    activates services from the desktop's service dirs (a portal, a notification daemon), which would start them on
    the private bus. Ours lists none, so an unowned name fails at once (`ServiceUnknown`). Its socket is
    `<niri-run>/bus`, the client's `$XDG_RUNTIME_DIR/bus`, so a client that falls back from a missing
    `DBUS_SESSION_BUS_ADDRESS` lands on it too. The fakes run in the test process, each on a `zbus` connection to that
    address only (`zbus` 5.19 in the async-io flavour GPUI's ashpd already takes).
  - GPUI's `open_url` runs the launchers first (`xdg-open`, then `gio open`, `gnome-open`, `kde-open`) and asks the
    OpenURI portal only when all fail; the sandbox's opener answers all four names, exits 0 and records each call.
  - The canary test runs everything together (the probe window draws, gets typed text, Ctrl+K, Enter and a click,
    its `open_url` reaches the opener, it holds a connection to the private bus) in a child test process whose
    inherited `DBUS_SESSION_BUS_ADDRESS`, `WAYLAND_DISPLAY`, `NIRI_SOCKET` and `XDG_RUNTIME_DIR` name listening
    canaries and `DISPLAY` an X display; the child checks every client's and helper's environment, the parent that no
    canary was connected to. GPUI also subscribes to logind's sleep signals on the system bus (read-only, not the
    desktop session's); the sandbox leaves that as it is.
  - The sandbox's opt-in variable is now `UI_SANDBOX_DIR` (`ui_sandbox::window_allowed`, which the spike binary and
    the sandbox's own probe window, `ui-sandbox-probe`, check before GPUI connects); without `dbus-daemon` the
    sandbox tests skip too.
  - The spawn passes the named descriptors under their own numbers: before `exec` the child marks every other one
    from 3 up close-on-exec (`close_range` with `CLOSE_RANGE_CLOEXEC`) and clears the flag on the named ones.

### Task 4: The credential broker
**Files:**
- Create: `crates/teams-app/{Cargo.toml,src/main.rs,src/secrets.rs}` (the broker subcommand first; the app's start in
  Task 6), tests
- Modify: `Cargo.toml` (member), `crates/teams-auth/src/secrets.rs` (if `BrokerSecretStore` lives there)

- [x] tests first: `teams-app secrets --lab <alias>` serves `get`, `put` and `delete` of that account's own
      `teams-lab` keys on a socket pair and refuses any other account or application name; a framed protocol with a
      size cap; a malformed or oversized frame closes the socket
- [x] tests first: the app's `BrokerSecretStore` over it passes `Session`'s renewal tests (a renewal's new tokens
      stored through it); offline tests serve a `MemorySecretStore`
- [x] no value in either side's output, log or `Debug` (only the key's account and audience); the broker opens no
      window and no desktop client and exits when the socket closes (tests)
- [x] `[lints] workspace = true`; `cargo test -p teams-app -p teams-auth` and the gate pass
- Done (2026-10-21), where it differs from the lines above:
  - The protocol, its server loop (`serve`) and `BrokerSecretStore` live in `teams-auth`'s new `broker` module, not in
    `secrets.rs` (which gains only `SecretError::Broker`), so `Session`'s tests run on it inside `teams-auth`.
    `teams-app` (a lib beside `main.rs`: `cli`, `secrets`) is the subcommand: it takes over the socket, reads the
    alias's UPN from `lab.toml` and serves `teams-lab` for it.
  - The command line is `teams-app secrets --lab <alias> --fd <n>`: the socket's descriptor is named, 3 or above,
    checked open and a Unix stream socket before it is taken over, and made close-on-exec.
  - The wire: a 4-byte big-endian length and 1 to 64 KiB; a request is an operation byte and length-prefixed UTF-8
    fields (application, account, audience, the secret for `put`), nothing after; an answer a status byte (done,
    found, missing, refused, failed) and the secret or the store's error. A request names its application, so a
    lab broker refuses a `teams-personal` client. The account matches exactly; an empty audience is refused.
    Frames carrying a secret are zeroed on drop.
  - The client does blocking reads and writes on tokio's blocking pool, one exchange at a time, so it works from any
    runtime (the engine's own) and a Secret Service unlock prompt holds up no runtime thread. After a socket error or
    a malformed answer it drops the connection and every later call fails (`BrokerError::Closed`).
  - The broker connects the Secret Service on the first request it serves (`LazyKeyring`), so a broker that only
    refuses or only sees its socket close never reaches the bus; that is how the process tests run the real binary
    (cleared environment, only its socket passed, canary bus, Wayland and niri sockets that must stay unreached)
    without a keyring. It notes on stderr only refused or failed requests.
  - It runs in any build, `cargo test`'s included: it reaches no fence, transport or mock origin, so the
    `test-support` refusal is the app's (Task 6), not the broker's.
  - `Session`'s tests moved to `tests/session/cases.rs`, loaded twice (`tests/session/{in_memory,
    through_the_broker}.rs`, one `#[expect(clippy::duplicate_mod)]`): all 26 run on a `MemorySecretStore` and through
    the broker, and the renewal tests also check the rotated refresh token and the renewed skypetoken in the store
    behind the broker.

### Task 5: The engine grows what the UI needs
**Files:**
- Modify: `crates/teams-core/src/{engine.rs,outbox.rs,sync.rs}`, `crates/teams-store/src/*` (if a query is missing),
  `crates/teams-core/tests/*`

- [x] tests first: the request side split off `EngineHandle` as a cloneable handle (writes and queries, each a
      `Reply`); the owner keeps the event receiver, the thread and `shutdown`
- [x] tests first: the exclusive per-account engine lock (Technical Details): a second engine on the same store, in
      this process or another, is refused with a clear error, never waits and never breaks the lock; the lock goes
      with the engine's shutdown
- [x] tests first: the queries the UI needs: `unread_count`, `read_horizon`, a conversation's outbox rows, the
      earlier runs' rows; the `resend` request (the explicit `Store::resend`, only for a row of a run that has ended)
- [x] tests first: a catch-up reports what it changed: `ConversationChanged` for each conversation it synced with a
      change, and `Event::Message` for each message it newly stored, marked as brought by a catch-up (so Task 14 can
      tell it from a delivery); a replay or an unchanged row gives none
- [x] tests first: the outbox's transient kind (Technical Details): a fresh `Fence::check` and `fence.log` line per
      send; no stored row; an unknown outcome dropped, never checked for or retried; latest wins while one waits;
      nothing of it survives a restart or appears among the earlier runs' rows
- [x] `cargo test -p teams-core -p teams-store` and the gate pass
- Done (2026-10-21), where it differs from the lines above:
  - The request side is `EngineRequests` (`Clone`, `Send`): `EngineHandle` keeps the events, the thread and
    `shutdown`, hands out clones (`requests()`) and derefs to its own, so existing callers are unchanged. Dropping the
    owner now sends the engine its stop itself, since a clone left elsewhere keeps the channel open.
  - The lock is `Store::engine_lock` (`flock` try-lock on `<store>-runs/engine.lock`, never swept); `engine::start`
    takes it before anything else and answers `EngineError::AlreadyRunning { lock }`; the engine's thread drops it
    after its runtime and store. The other-process case is a store test (a child process of the test binary holds it).
  - The queries: `unread_count`, `read_horizon` (the account's own), `outbox_unsettled` (a conversation's rows that
    aren't sent, of any run: a new store query on a partial index, migration `0005`), `left_by_earlier_runs`;
    `resend(id)` is `Store::resend` with its own rules (a row of a run whose store is open, this run's waiting or
    in-flight row included, and a sent row are refused), then sent at once as a write is: a row that had an attempt
    is looked for first. It also takes a failed or denied row of this run, since Task 13 offers "send again" on one.
  - What reads changed is kept by the catch-up (`CatchUp::take_changes`: `Changes` of `StoredMessage`s and
    conversations) for every read alike, so the engine tells it after each one: `Event::Message` replaces its
    `replay: bool` with `source: Source` (`Delivery`, `Replay`, `CatchUp` for a catch-up, the sync after a write or a
    delivery's new conversation, the outbox's check; `History` for `load_older`). A read's newer version is
    `MessageChange::Other` (a read doesn't say what changed). `teams-headless` prints `source` beside `replay`.
  - The transient kind is `outbox::Transient` (`Typing { conversation }`), sent by `EngineRequests::transient`
    (refused for a conversation the store doesn't know), not an `Action` variant: `write` answers with a stored row,
    which a transient never has. A 401 gets one more attempt at once, a 429/503 (or a throttled token service) waits
    in memory and goes out from the tick (`Event::Transient` tells how), a denial, any other answer and an unknown
    outcome drop it. `endpoints.md`'s typing row says so.

### Task 6: `teams-ui` and `teams-app`: the bridge and the lab binary
**Files:**
- Create: `crates/teams-ui/{Cargo.toml,src/lib.rs,src/bridge.rs,src/models.rs,src/trace.rs}`, tests in both
- Modify: `Cargo.toml` (members, workspace deps), `crates/teams-app/src/main.rs`

- [x] tests first: `teams-app --lab <alias> --secrets-fd <n>` builds its parts as `teams-headless` does
      (`Fence::open_lab`, `Session` over the broker's socket, the region, `LiveMembershipSource::for_fence` with the
      transport's credential, the store at `LabPaths::store_file`, Trouter); refuses to start without
      `--secrets-fd` and never opens `KeyringSecretStore` itself; refuses a `test-support` build; refuses to open a
      window unless its Wayland socket is inside the directory `TEAMS_APP_SANDBOX` names (as the GPUI spike does) and
      `DISPLAY` is unset; prints no token; another engine on the account (Task 5's lock) is reported and the app exits
- [x] tests first: the bridge owns the `EngineHandle`; one background task drains `next_event` and applies each event
      to the models on GPUI's foreground (two views observing one model both see it); views share only the request
      side, behind the `Core` trait; a query answers through its `Reply`; shutdown ends the drain task and consumes
      the owned handle with `shutdown`, off the foreground, joining the engine's thread
- [x] tests first: GPUI's HTTP client is the null one: an `img()` of an `https` URL in a test window loads nothing and
      no request leaves the process (no `set_http_client` in the app crates: a source test)
- [x] a manifest test (`cargo metadata --filter-platform` for the host): `teams-ui` and `teams-app` depend directly on
      no HTTP or WebSocket client (`reqwest`, `hyper`, `tungstenite`, `ureq`, `isahc`, …): every request goes through
      `teams-core`
- [x] the view trace: one JSON line per view change (view, conversation and message ids, kinds, times; never text),
      on a file descriptor the caller passes; a test that no message text or name reaches it
- [x] the signed-out state: when the `Session` reports an invalid grant (an expired or revoked refresh token), the
      app shows "signed out" (the lab build: "run `proto-lab auth <alias>`"; the personal build reopens sign-in, Task
      27) and sends nothing more
- [x] an empty window opens in the sandbox on `teams-mock` (in-process test, `test-support`), and its screenshot shows
      it
- [x] `[lints] workspace = true`, listed in `members`; `cargo test -p teams-ui -p teams-app` and the gate pass
- Done (2026-10-21), where it differs from the lines above:
  - The sandbox opt-in is `UI_SANDBOX_DIR`, the variable Task 3's sandbox sets for every client
    (`ui_sandbox::in_sandbox`), not a `TEAMS_APP_SANDBOX` of the app's own (`teams_app::guard`, which also refuses a
    non-empty `DISPLAY`). `teams-app` takes `ui-sandbox` as a normal dependency for that check.
  - The lab engine's assembly moved out of `teams-headless` into `teams_core::lab::start` (generic over the secret
    store), so both binaries build it one way: `teams-headless` hands in the keyring, `teams-app` the broker's
    `BrokerSecretStore`; `teams_headless::lab::display_name` re-exports the core's.
  - Signed out is the session's state: `Session` latches it once Entra refuses the refresh token (`invalid_grant`) or
    none is stored, answers every later call with that error at once (a cached token included, nothing sent), and
    tells it once (`watch_signed_out`). The app watches it (`teams_app::app::watch_sign_out`); `Bridge::sign_out`
    shows the hint and shuts the engine down, so its requests answer `Stopped` (its push channel closes as on exit).
  - The bridge takes the engine's owning side as an `EventSource` (the `EngineHandle` in the app, a fake in the view
    tests), and `teams_core::Reply::ready` lets a fake core answer. The drain runs on GPUI's foreground; the blocking
    `shutdown` on GPUI's background executor. GPUI's quit mode is `Explicit`, since GPUI would otherwise quit as the
    last window closes, before the engine is shut down; the app quits once it is.
  - GPUI's test context has a fake HTTP client of its own, so the null-client check runs in a real GPUI app in the
    sandbox (`crates/teams-app/tests/window.rs`, `http` mode): an `img()` of an `https` URL on a loopback listener the
    child holds, a request through `cx.http_client()` refused ("No HttpClient available"), the listener never
    connected to. The source test also refuses `with_http_client` and any `.expose()` of a secret in the app crates
    (`crates/teams-ui/tests/reach.rs`, which holds the manifest test too: direct normal and build dependencies).
  - The window test is a `harness = false` test that runs itself again in the sandbox (GPUI on the main thread): a's
    engine on `teams-mock`, the view trace on a pipe (the window opened, the push channel connected), a screenshot
    under `target/ui-shots/` filled by the window's background, the window closed through niri, the app quit and the
    account's engine lock free afterwards.
  - The view trace takes ids only as `TraceId`: visible ASCII, all digits or with a `:` (a service id's shape);
    anything else is left out and counted (`refused`); view, change and kind names are `&'static str`.
  - `gpui-pre`'s `test-support` (a dev-dependency of `teams-ui` and `teams-app`) brings its own test crates into
    `Cargo.lock` (`proptest`, `postage`, the screen-capture crates with their X11 bindings); only test builds use them.

### Task 7: The UI kit
**Files:**
- Modify: `Cargo.toml` (`gpui-component` in `[workspace.dependencies]`, version looked up live),
  `crates/teams-ui/{Cargo.toml,src/theme.rs}`, tests, `README.md` (third-party UI kit and its license)

- [x] add `gpui-component`; it resolves to the vendored patched `gpui-pre` (one `gpui-pre` in `Cargo.lock`) and the
      workspace builds with GPUI's async-io `zbus`/`ashpd` unchanged
- [x] audit (written into this task): the kit turns on `gpui-pre`'s default features (`x11`, `font-kit`,
      `windows-manifest`) for the whole workspace; record it and decide it is acceptable (Task 6's `DISPLAY` refusal
      covers the lab app); `cargo tree` of `teams-ui` for the host target shows no HTTP or WebSocket client through
      the kit (`gpui-pre-reqwest` is wasm-only; the manifest test extended to the kit's tree); the kit starts no file
      watcher (`watch_dir` never called), thread or process in our use; its `tracing`/`log` lines carry no text we
      hand it at the levels the app keeps; its licence and its dependencies' are Apache-2.0/MIT-compatible
- [x] tests first: the kit's `Textarea` in the sandbox: typing, selection, copy, cut, paste (plain text only), undo,
      soft wrap, IME composition (through GPUI's input handler in the test context), Enter and Shift-Enter told apart
- [x] the theme: Teams-like and compact, light and dark following the window's appearance, switching live (test on
      the test context)
- [x] ⚠️ if the audit fails: fall back to our own widgets on gpui-pre's examples (`input.rs`, `view_example/`,
      `list_example.rs`), add them as ➕ tasks here, and update Technical Details (not needed: the audit passed)
- [x] `cargo test -p teams-ui` and the gate pass
- Done (2026-10-21), where it differs from the lines above:
  - The kit is `gpui-component` **0.8.0** (the latest on crates.io when added, 2026-10-19; still `gpui-pre =0.3.9`),
    not the 0.7.3 Context names, taken with no features of its own (`default-features = false`; its defaults are
    none anyway). One `gpui-pre` in `Cargo.lock`, the vendored path crate (a test checks it); `ashpd` and `zbus` stay
    on async-io (the kit's own `ashpd` asks for `async-io` and `settings`). `Cargo.lock` also lists `gpui-fast` (an
    optional GPUI variant of the kit's base crate): no build compiles it (`cargo tree --workspace --all-features
    --target all -i gpui-fast` is empty).
  - The audit passed:
    - Default features: the kit takes `gpui-pre` with its defaults, so the workspace's GPUI has `x11`, `font-kit` and
      `windows-manifest` on. In the vendored 0.3.9 `x11` only lets `GraphicalEnvironment::guess_compositor` name X11
      and keeps a few scene `cfg`s; `font-kit` is an empty feature nothing reads; `windows-manifest` runs only in a
      Windows build script. The X11 client lives in `gpui-pre-linux`, whose features stay `wayland` only (nobody
      turns its `x11` on), and the app starts `gpui_platform::linux(WindowingModes::WAYLAND)`; Task 6's `DISPLAY`
      refusal stays as a second guard. Accepted.
    - Reach: the kit's whole host tree (`cargo metadata --filter-platform`, normal and build dependencies, proc
      macros included) holds no HTTP or WebSocket client (`gpui-pre-reqwest` is under `cfg(target_family =
      "wasm")` in `gpui-kit-assets`). `crates/teams-ui/tests/reach.rs` checks it.
    - Threads, processes, files: the kit's sources spawn no thread or process (its `smol` use is channels only, its
      speech/microphone code is behind the `speech` feature, off). Its only file watcher is
      `ThemeRegistry::watch_dir` (`notify`/`inotify`), never called (a source test refuses `watch_dir(` in the app
      crates). Its assets crate (`rust-embed`) is reached only through an `AssetSource` the app doesn't install. On
      Linux its base crate reads the reduced-motion setting from the settings portal on the bus the app is given,
      as GPUI reads the colour scheme there: read only, and the sandbox's bus has no portal.
    - Logs: its `tracing`/`log` lines (warnings about theme files, Markdown/HTML nodes we never hand it, a failed
      clipboard read) go nowhere: `tracing` is built with `log`, but the app installs no `log` logger and no
      `tracing` subscriber (the source test refuses `set_logger`, `set_boxed_logger`, `set_global_default`,
      `tracing_subscriber` in the app crates), and `log` stays capped at `debug` at compile time.
    - Licences: the kit and its crates are Apache-2.0; its tree is MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib,
      Unicode-3.0, 0BSD, CC0-1.0, Unlicense, BSL-1.0, and MPL-2.0 for `option-ext` only (through `dirs` in
      `rust-embed`'s build-time macro; MPL's copyleft stops at its own unmodified files). `reach.rs` checks every
      package's SPDX expression against that list.
  - The theme (`crates/teams-ui/src/theme.rs`): our two `ThemeConfig`s ("Teams Light", "Teams Dark") built in code
    from two palettes (Teams' brand purple, its greys), 14 px text, radius 4/6, no shadows; anything not named keeps
    the kit's default for the mode. `theme::init` (once per app: `gpui_component::init`, then ours, light first),
    `theme::follow(window)` (applies the window's appearance and again on each change), `theme::apply`,
    `theme::current` (the palette now). `window::open` sets the kit up and keeps the root view following its window;
    the root view draws in the theme's colours (`view::BACKGROUND` is the light palette's background).
  - GPUI's test context can't change a window's appearance (`TestWindow` is crate-private), so the live switch is
    tested twice: on the test context through `theme::apply` (every window redraws in the new colours, state kept),
    and on the real platform in the sandbox, where a fake settings portal on the sandbox's bus
    (`org.freedesktop.portal.Settings`, the one GPUI asks) signals the colour scheme's change and the window's theme
    follows to dark and back (`crates/teams-ui/tests/kit_window.rs`).
  - The text area is tested on both: `crates/teams-ui/tests/kit.rs` on the test context (typing through GPUI's
    platform input handler, selection, copy/cut/paste on the test platform's clipboard, a paste of an image pasting
    nothing and of text beside an image the text only, undo and redo, soft wrap, IME composition marked then
    committed and given up through `EntityInputHandler`, Enter and Shift-Enter) and `kit_window.rs` in the sandbox
    (the virtual keyboard and the nested compositor's own clipboard). Two things the compose box (Task 13) must
    keep: with `submit_on_enter`, the kit's text area hands a plain Enter on (`Enter` action propagated, then
    `InputEvent::PressEnter { shift: false }`), and the platform types it as a line break unless an ancestor handles
    the `Enter` action, so the compose box takes Enter as the send with `on_action`; redo is `ctrl-y` on Linux (the
    kit binds no `ctrl-shift-z`).

### Task 8: The shell, settings and the chat list
**Files:**
- Create: `crates/teams-ui/src/{shell.rs,settings.rs,chat_list.rs,switcher.rs}`, tests
- Modify: `crates/teams-ui/src/models.rs`

- [x] tests first: the shell's three panes (rail: Chats, Teams; the list; the conversation), resizable list pane
- [x] tests first: the settings model and its file (lab: `<data_dir>/ui/<alias>.toml`; personal: the build's own data
      dir; `0600`, its dir `0700`; a missing or unreadable file gives the defaults, a bad value is reported and the
      default kept): the selected chat, send typing, send read receipts, notifications (all, mentions only, off),
      idle time, the account's manual status; a settings view to change them
- [x] tests first (view tests on a fake core; in-process on `teams-mock`): conversations by last activity, a row's
      avatar (initials until Task 12), name, last message's first line, time and unread count from the store's
      horizon; kinds (1:1 named by the other member: their last `imdisplayname` in the store, else the MRI, until
      Task 12; the group chat by its topic or members; channels left to the Teams rail; the account's own streams
      hidden); live updates from events, catch-ups included; the connection state (connected, reconnecting, offline)
      from the push channel's events
- [x] tests first: the chat switcher (Ctrl+K, the kit's command palette): fuzzy match on chat names, Enter opens,
      Escape closes; the live test's way into a chat
- [x] opens from the store at once, then catches up; nothing shown comes from anywhere but the store
- [x] `cargo test -p teams-ui` and the gate pass
- Done (2026-10-21), where it differs from the lines above:
  - The settings view is a module of its own (`settings_view.rs`); `models.rs` is unchanged: the settings are an
    entity of their own (`SettingsModel`), the chat list reads the existing `Chats` revisions.
  - The chat list needs two things the engine didn't have: who the account is (`EngineRequests::me`, `Core::me`) and
    a person's newest `imdisplayname` in the store, in any chat (`Store::display_name` over a new index, migration
    `0006`; `EngineRequests::display_name`). The lab app's settings path is `LabPaths::ui_settings_file`.
  - A row is built again only when its chat changed since it was built (`Chats::changed_at`); every row after a
    catch-up, which may bring names from other chats. A load while one runs waits and runs once after it; a stopped
    engine leaves the rows shown. The status's names are now connecting, connected, reconnecting (the push channel
    down), offline (it or the engine stopped) and signed out; the list's header shows it.
  - The switcher is the kit's command palette with its own filter off: ours matches fuzzily (the query's characters
    in order, any case, runs and word starts ranked higher), at most 50 matches. The kit's Escape first clears a typed
    query; ours closes the switcher at once (a keystroke interceptor while it is open). Ctrl+K toggles it.
  - The view trace gained a list's conversations in order with their unread counts (`chat-list shown`), and `chat
    opened`, `settings opened`, `rail`, `switcher opened`/`closed`.
  - The in-process mock test is the app's window test (`crates/teams-app/tests/window.rs`): a's chat list filled by
    the catch-up (the chats with their kinds, not the channel nor the notifications stream), the panes' colours in the
    screenshot, and the switcher driven by the sandbox's virtual keyboard (Ctrl+K, Escape, Ctrl+K and Enter opening the
    chat at its top). The view tests on the fake core are `crates/teams-ui/tests/shell.rs` and `settings.rs`.
  - Avatars show the kit's initials of a name the store gave; a chat known only by MRIs gets the kit's empty avatar.

### Task 9: The chat view
**Files:**
- Create: `crates/teams-ui/src/{chat_view.rs,rich_text.rs}`, tests

- [x] tests first: a render test per `RichText` node, our own renderer (marks, breaks, paragraphs, mentions
      highlighted (the account's own more strongly), links opened through `open_url` (the sandbox's no-op opener in
      tests), quotes, an image as a placeholder until Task 22); edited and deleted markers; reactions with counts and
      the account's own; `Control/*` never shown; no message HTML ever reaches a markdown or HTML renderer
- [x] the newest at the bottom (a bottom-aligned list that follows the tail while at the bottom); older pages when
      fewer than a page of rows is above the view (`load_older`); a sender's messages grouped within 5 minutes; day
      separators; a "New messages" divider at the account's read horizon (`read_horizon`); a "Jump to newest" pill
      when scrolled up; rendered rich text cached per message id and version
- [x] the typing indicator from `Event::Typing { typing }`, cleared by `typing: false` or a timeout (from Task 10's
      capture; 5 s until then)
- [x] hover and keyboard actions on a message: reply, react, edit and delete (the account's own only); wired in Task 13
- [x] a long chat (1 000 messages from the mock) scrolls without loading everything at once (a measured bound in the
      test)
- [x] `cargo test -p teams-ui` and the gate pass
- Done (2026-10-21), where it differs from the lines above:
  - `rich_text.rs` lays `teams-proto`'s `RichText` out as blocks (a paragraph's text with styled pieces and link
    ranges; quotes with a reply's sender) and draws them with GPUI's `StyledText`/`InteractiveText`; a mention is the
    account's own when its `properties.mentions` entry (read by `teams-proto`'s reader) names it, and an entry in an
    unseen form names nobody; an image is `[Image: alt]`, never its source; a link opens only if `http`, `https` or
    `mailto`. Tested node by node on the test context (`tests/rich_text.rs`: a click opens through the test
    platform's `open_url`) and in the sandbox (`tests/kit_window.rs`: a link clicked with the virtual pointer reaches
    the no-op opener). `tests/reach.rs` refuses the kit's `TextView`/markdown/HTML views in the app crates.
  - `chat_view.rs`: pages of 50 from the store; the service's older page (`load_older`) only once the store has none;
    after three older loads in a row that bring nothing, rows near the top stop asking until asked again, and a failed
    one shows "Try again". The "New messages" divider stays at the horizon read when the chat opened. A thread event
    is a line of its own, a type this build doesn't show a placeholder naming no content. The shell keeps one
    `ChatView` for the chat opened.
  - Typing (`models.rs`): the timeout is Task 10's capture, 25 s (the web client's indicator went off then, or at the
    sender's message; it never sent `Control/ClearTyping`): each signal has a stamp, so an older signal's timeout
    never clears a newer one, and a delivered message from the typer clears it.
  - Actions are told as `ChatViewEvent::Action`; keys in the message list's own context (`ChatMessages`, never
    while compose's text area has the focus): Up/Down select, `r` reply, `a` react, `e` edit, Delete delete, Escape.
  - The 1 000-message chat is on the fake core's store pages (`tests/chat_view.rs`), not `teams-mock`, whose chats
    hold only the fixtures' messages: opening reads at most three pages, a frame lays out at most 80 rows, every
    store read asks for one page. The in-process window test on `teams-mock` (`teams-app/tests/window.rs`) checks the
    a–b chat's messages shown with their kinds and keeps a screenshot.
  - The view trace gained `chat-view shown` (the newest 50 message ids and their kinds) and `typing shown`/`cleared`.

### Task 10: The chat shapes the UI still needs (capture)
**Files:**
- Modify: `crates/proto-lab/src/scenario/ab_chat_full.rs` (or a new scenario), `docs/protocol/chat.md`,
  `tests/fixtures/chatsvc/*` (synthetic), `docs/protocol/endpoints.md`

- [x] live, under the fence, in the a–b chat: each of the web client's default reactions (not only `like`), set and
      taken back; typing while a message is typed (how often it is sent, how long the indicator stays on the other
      side) and whether `Control/ClearTyping` goes out; a reply with its quote and a message with line breaks and a
      link, unless M1's captures already show them; a mention of b in the group chat (`abc`). Every write on the a–b
      or the recorded group chat
- [x] tests first for any mapping row the capture corrects; shapes into `chat.md` with their source, synthetic
      fixtures of every form seen
- [x] live run recorded here (a breached run as CLAUDE.md says); `cargo test -p proto-lab -p fixture-guard` and the
      gate pass

Done (2026-10-21): a new scenario, `proto-lab run chat-shapes` (`crates/proto-lab/src/scenario/chat_shapes.rs`, a and
b under the messaging classifier so b's push channel is up; the reply with its quote was M1's, `reply.json`). Three
live runs, each **exit 0, no breach**, every allowed write on the a–b chat or the recorded group chat (`fence.log`:
reactions, typing and messages on the a–b chat, one message adding b and typing on the group chat): 16:56Z (text
inserted), 17:03Z (a quiet start, a key pressed after the link), 17:10Z (keys pressed). Findings (`chat.md`, "The
shapes the UI needs"): the quick reactions are `like`, `heart`, `laugh`, `surprised`, side by side; the web client
never sent `Control/ClearTyping`, sent typing sparsely (none during 45 s of steady presses, one just after they
stopped; the bundle throttles it to one per 6 s), and b's indicator went **about 25 s** after the last typing delivery
or when a's message arrived (Task 9's typing timeout: 25 s, not 5 s); Shift+Enter makes paragraphs, a typed link an
anchor plus a `properties.links` entry; the group mention is the 1:1 form, its feed entry `mentionInChat`. The
capture corrected reading, not mapping: the service joins read-back paragraphs with CRLF, which `teams-proto`'s
reader now drops beside a paragraph or a quote (tests first). Fixtures `reactions.json`, `lines.json`,
`mention-group.json`; the link preview read (`/urlp/v1/url/info`) is in `endpoints.md` as seen, not reviewed.

### Task 11: The native read by `POST` (`PostRead`)
**Files:**
- Modify: `CLAUDE.md` (the rule text, first), `crates/teams-proto/src/{endpoint.rs,http.rs,lib.rs}` and a new
  `post_read.rs`, `crates/teams-fence/src/*` (the decision), `crates/fence-seal/tests/ui/*`,
  `docs/protocol/endpoints.md`, `docs/protocol/fence-targets.md`

- [x] CLAUDE.md first, in the words of the user's decision of 2026-10-21: "The native client sends only what its
      sealed policy allows" gains a fourth list, `teams-proto`'s sealed `PostRead`, for requests that change nothing:
      each entry added only after a review in its exact captured shape (host, method, path, every query parameter
      and its value, the headers that change what it does, the body schema); every MRI it names checked when the
      fence decides (`Fence::lab_accounts_only` in a lab build), any other id `Unknown`; its captured answer holds
      only the accounts asked for; logged to `fence.log` before it is sent and not sent if it can't be logged; no
      token to a third party; never a search or a suggestion list
- [x] tests first: a `PostRead` request goes out only with a `PostReadPermit` the fence gives for its exact MRI list;
      a body field, a query parameter or a header value the entry doesn't name makes it `Unknown`; a non-lab MRI, an
      approved service or a duplicate makes the lab fence deny it; the `fence.log` line comes first; the transport
      sends it only to its entry's host
- [x] tests first: the middle tier as a new `Service` (host from the `middleTier` GTM, the
      `/api/mt/part/<partition>/beta/users/…` path check, the `api.spaces.skype.com` audience, the captured fixed
      header values as `crates/proto-lab/src/post_read.rs` reads them), on the transport's host allowlist
- [x] its first entry: `users/fetchShortProfile` in the captured `?isMailAddress=false&…` shape (the browser fence's
      `short_profile` read by `POST` is the reference), with a wiremock test of its captured answer (synthetic
      fixture)
- [x] a `fence-seal` trybuild case: nothing outside `teams-proto`/`teams-fence` can mint a `PostReadPermit` or a
      `PostRead` entry
- [x] `cargo test -p teams-proto -p teams-fence -p fence-seal -p fixture-guard` and the gate pass
- Done (2026-10-21): CLAUDE.md gains the rule as a bullet of its own after the native-policy rule (that rule names
  the `PostRead` list; its "Both lists grow" became "The own-state and frame lists grow", same meaning). The API,
  settled with Codex: `Fence::check_post_read(kind, names)` → a `PostReadPermit` (kind + the exact ordered MRIs;
  empty, duplicate, non-lab or service MRIs denied; the decision logged as `post_read_decision` with a count, never
  the MRIs, and the permit made only once logged); `AuthorizedPostRead::new` pairs a sealed `PostRead` entry with it;
  `Transport` gains a fourth method, `post_read` (the seven test fakes delegate). The transport reads the request it
  is about to send back against the entry's captured shape (path, every query pair, every header but the token, the
  body naming exactly the permit's MRIs) before it asks for a token: anything else is `HttpError::Unknown`, proven
  through a `test-support` `ProbePostRead`. One send per permit, never retried (a 401 refreshes the token for a later
  permit); the send logged as `post_read` first. `Service::MiddleTier` comes from `HttpTransport::with_middle_tier`
  (`new`'s callers unchanged). `ShortProfileLookup::read_answer` drops an answer naming any MRI not asked for. The
  fixture `tests/fixtures/mt/short-profile.json` comes from a reference copy exported from ab-chat-full's capture of
  2026-10-20 (`reference/chat/short-profile-a.jsonl`), the first with the lookups allowed.

### Task 12: Names and pictures
**Files:**
- Create: `crates/proto-lab/src/commands/profile_probe.rs`, `docs/protocol/graph.md`
- Modify: `crates/teams-proto/src/{endpoint.rs,http.rs,read.rs}` (a Graph service, `GET` only),
  `crates/teams-core/src/*`, a store migration (names and pictures cached), `crates/teams-ui/src/*`,
  `docs/protocol/endpoints.md`

- [x] a read-only probe first (`proto-lab probe profile <alias>`): Graph `GET /v1.0/users/<id>` and
      `/users/<id>/photo/$value` for the lab accounts with our own Graph token; statuses, key names and sizes only;
      written up in `graph.md`, which records that Graph is our own probed reach, not the web client's (the web
      client's pictures come from the middle tier's `profilepicturev2` with a `cookiev2` cookie, `endpoints.md`),
      so the probe's answer is the reviewed shape
- [x] tests first (wiremock, synthetic fixtures): if Graph answers, `Service::Graph` (that host exactly, `GET` only,
      the Graph audience from the `Session`, its path check) for names and photos; the profile lookup (`PostRead`,
      Task 11) for names Graph doesn't give; then a message's `imdisplayname` and the account names in `lab.toml`;
      pictures as initials when there is no photo
- [x] names and pictures cached in the store, refreshed by age, a lookup never repeated while one is under way; the
      chat list and the chat view use them (the mention picker in Task 13); the bytes of a photo reach the view only
      from the engine
- [x] `cargo test -p teams-proto -p teams-core -p teams-store -p teams-ui -p fixture-guard` and the gate pass
- Done (2026-10-21), where it differs from the lines above:
  - The probe (`crates/proto-lab/src/commands/profile_probe.rs`) read each lab account's user record whole and in the
    native `$select=id,displayName` form, its largest picture (`photo/$value`) and its 96×96 one
    (`photos/96x96/$value`); it prints statuses, header names, content types, sizes and key names with their JSON
    types, and a Graph error's plain code. Two live runs (as a, then as b; GETs only, under the live lock): every
    user read 200, a's pictures 200 `image/jpeg` (about 230 KB whole, about 7 KB at 96×96), b's and c's 404
    `ImageNotFound`; the Graph token carries `User.ReadBasic.All`. `graph.md` has the shapes.
  - The native client reads `users/<oid>?$select=id,displayName` (the name shown and nothing else of the record) and
    `users/<oid>/photos/96x96/$value` (the avatar's size, not the largest `photo/$value` the line above names), both
    probed; `Service::Graph` refuses every other Graph endpoint and any request to Graph that isn't a read
    (`GraphUser`, `GraphPicture` in `teams-proto`'s `read.rs`; pictures kept as JPEG or PNG up to 256 KB).
  - `teams-core`'s `profiles.rs`: `EngineRequests::person(mri)` answers from the store at once and starts a lookup
    in the background (at most 4 at a time, on the engine's runtime) when the stored profile is missing or older than
    a day; a part that failed on the way keeps the last one and the person waits 5 minutes; a change stored is told as
    `Event::Profile`. The name: Graph, else the profile lookup (only with the fence's permit, and only once the lab
    engine has the middle tier's base from the GTM `middleTier`), else the newest message's `imdisplayname`, else
    `Parts::known_names` (the lab accounts' UPN local parts from `lab.toml`). Store migration `0007_profiles`.
  - `teams-ui`: `Core::display_name` became `Core::person`; the chat list shows a 1:1 chat's partner's picture, the
    chat view an avatar beside every other sender's name, a profile name over the name a message carried; rows and
    names are asked again on `Event::Profile` (`people.rs`). The Graph reads ran natively only on wiremock: the app's
    first live run is Task 25's.

### Task 13: Compose and the writes from the UI
**Files:**
- Create: `crates/teams-ui/src/compose.rs`, tests
- Modify: `crates/teams-core/src/engine.rs` (`Action::Typing`, transient), `crates/teams-ui/src/chat_view.rs`,
  `docs/protocol/endpoints.md`

- [x] tests first (in-process, two accounts on `teams-mock`): plain text with line breaks and links → `MessageText`;
      Enter sends, Shift-Enter breaks the line; a mention completed after `@` from the chat's members (names from Task
      12), only by MRI; a reply with its quote (Escape cancels); editing the account's own messages (Up in an empty
      box edits the last); deleting them (confirmed); the reactions Task 10 captured; each reaches the other account
      through the mock and leaves its `fence.log` line
- [x] typing as a transient engine action (Task 5), sent no more often than Task 10 measured and only while the
      compose box has text and focus; behind the "send typing" setting (on by default); tests that it is fenced like
      any write, never stored and never sent again after a restart or an unknown outcome
- [x] the read receipt when a chat is shown with the window focused, up to the newest message shown, debounced; behind
      the "send read receipts" setting (on by default)
- [x] each message's outbox state shown (sending, sent, waiting, denied by the fence, failed, may have arrived);
      "send again" on a failed row or an earlier session's row is the engine's explicit `resend`, after a warning for
      a row that may have arrived; a denied write (an outsider mentioned on the mock) shows as denied and nothing is
      sent
- [x] creating a chat is not offered (a native group create is denied: CLAUDE.md; starting a 1:1 needs people search)
- [x] `cargo test -p teams-core -p teams-ui` and the gate pass
- Done (2026-10-21), where it differs from the lines above:
  - The engine needed no `Action::Typing`: typing has been `EngineRequests::transient` since Task 5, and its engine
    tests (fenced per send, never stored, nothing after a restart or an unknown outcome) stand; this task's tests show
    the compose box sends it through `Core::transient` and that it reaches b with a's `fence.log` line. The engine's
    one change is `Event::Queued`: a row is told as it is made (or adopted by `resend`), before its first attempt, so
    a post shows at once as sending (a write's reply comes only after the attempt, about 1.4 s live).
    `teams-headless` prints it as `queued`.
  - The compose box (`compose.rs`) is part of the chat view (one per chat opened, under the messages);
    `ChatView::new` now takes the shell's parts and the window. A mention is the kit's atomic inline token
    (`gpui-component`'s `InlineToken`, its id the MRI), put in only by the picker, which offers the chat's work-account
    members but the account (`Core::members`, names from `Core::person`); a name typed is only text. A typed
    `http`/`https`/`mailto` URL is sent as a plain `<a href>` with `properties.links` `"[]"`, not the web client's
    anchor with its `links` entry. Each line is a paragraph. A reply's preview is the quoted message's text on one
    line, cut at 300 characters (no capture showed a long one). An edit loads the message back without its marks (the
    box has no formatting), its mentions as tokens by MRI and its quote kept as read back; a message showing an image
    can't be edited. Typing goes at most once per 6 s (leading edge), not while editing.
  - Keys: in the message list `r` and `e` move to the compose box (reply, edit), `a` opens the quick reactions (1–4
    pick: like, heart, laugh, surprised), Delete asks and Enter confirms, Escape closes what is open first; in the
    box Up and Down move in the mention picker, Enter or Tab choose, Escape closes it, then cancels a reply or an
    edit. A reaction chip clicked sets or takes back the account's own.
  - Read receipts: while the window is active and the list follows the newest, up to the newest message from someone
    else after the account's horizon (read from the store on every load) that carries a `clientmessageid`, 800 ms
    after the view settles, one at a time (a burst gives one, for the newest).
  - Outbox states (`sending.rs`): a post shows as a pending message of its own (from the row the engine told, or the
    store's unsettled rows) until its message is held; an edit, a delete or a reaction shows on its message. "Send
    again" is offered on a failed row and an earlier session's (a warning first when it may have arrived), not on a
    denied one (the fence would decide the same). A row that isn't sent stays shown: the store has no way to dismiss
    one.
  - The view trace gained `compose sent`/`replying`/`editing`/`cancelled` (the kind: message, reply, edit) and
    `outbox shown` (the rows' ids and states).
  - The two-account test (`crates/teams-ui/tests/compose_mock.rs`) runs a's chat view on GPUI's test context over a's
    engine and b's engine headless. The mock gives a new message an arrival time before its fixture's, so there Up
    edits the newest fixture message of a's (a quoted reply, whose quote the edit keeps); Up is also tested on the
    fake core. The source test `no_view_offers_creating_a_chat` (`reach.rs`) checks no app crate names `CreateChat`.

### Task 14: Notifications and the tray
**Files:**
- Create: `crates/teams-ui/src/{notify.rs,tray.rs}`, tests
- Modify: `Cargo.toml` (`zbus` in `[workspace.dependencies]`, async-io flavour, version looked up live and matching
  the one GPUI links)

- [x] first: does GPUI's `show_system_notification` reach `org.freedesktop.Notifications` on the bus the app is given
      (the sandbox's fake server sees it, with its actions)? If so it is the notification path; if not, our own over
      `zbus`
- [x] tests first, on the sandbox's private bus: a notification for a message from someone else in a chat that isn't
      shown and focused (a mention marked as such), as the "notifications" setting says; none for the account's own,
      for a replayed delivery (`replay`) or for one the store already had (`applied` unchanged); a message brought by
      a catch-up notifies only if it arrived after the push channel last went down and is unread, and more than
      three in one chat collapse into one notification; activating it opens the chat
- [x] the tray (StatusNotifierItem): the unread total, open, quit; closing the window keeps the app in the tray
- [x] the bus address is the one the app is given, and a test checks the app never falls back to another
- [x] `cargo test -p teams-ui` and the gate pass
- Done (2026-10-22), where it differs from the lines above:
  - GPUI's own path reaches the sandbox's fake server with its actions (`default`, then its own `gpui-action-<n>`
    keys) and hands the body's activation back with its tag (`crates/teams-ui/tests/gpui_notification.rs`), but it
    is not the path: its tag replaces nothing on Linux (`replaces_id` always 0) and nothing can close one
    (`dismiss_system_notification` does nothing), so more than three of one chat can't collapse into one; and each
    notification opens a connection of its own through `zbus`'s session-bus constructor (the environment's address,
    else `$XDG_RUNTIME_DIR/bus`) on a thread that waits until the server closes it. The app sends its own over
    `zbus` (`notify::BusNotifications`). The test keeps GPUI's behaviour on record.
  - `zbus` was already in `[workspace.dependencies]` at 5.19.0, async-io, the version GPUI links (Task 3); it is
    now a normal dependency of `teams-ui`. crates.io's newest is 6.0.0 (looked up 2026-10-22), which would put a
    second `zbus` in the tree, so it stays.
  - The decisions are on GPUI's test context against the fake engine and a fake notification server
    (`tests/notify.rs`, `tests/desktop.rs`); the D-Bus side on the sandbox's real private bus (`tests/desktop_bus.rs`:
    the server, the tray item, the bus given being the only one) and the whole app in the sandbox on `teams-mock`
    (`teams-app/tests/window.rs`). New modules: `bus.rs` (the address and the one connection), `desktop.rs` (the
    window, the notifier and the tray held together), besides `notify.rs` and `tray.rs`; the models gained a `Feed`
    entity (each message newly stored, with its source, and each read horizon moved).
  - Which messages: a delivery notifies with no time or read check (it is new by definition; the mock's arrival
    times are older than its fixtures', so a horizon check would drop a live one); a catch-up's only after the push
    channel last went down (before it first connects: since the app started) and after the account's read horizon.
    A replayed delivery (`replay`) the store didn't have notifies under the same rule as a catch-up's, unlike the
    line above: a delivery missed in a silent drop comes back as a replay on the reconnect; one the store had stays
    silent, as any message not newly stored does. The cutoff is when the socket last heard from the service (the
    engine tells it with the drop) less `CLOCK_SKEW_MS`, not the moment the drop was noticed: a socket gone silent
    in a suspend is noticed only later, and the service's times are compared with this machine's.
    In a team's channel only a mention notifies; the account's own streams (`48:`) and non-text messages never do.
    A mention reads "<name> mentioned you" (in "<chat>" for a group).
  - Collapsing: messages are gathered for 250 ms before they are shown, so a catch-up's come together; a chat keeps
    up to three single notifications, and the fourth closes them and shows one "N new messages" (", one or more
    mentioning you" when one did) that later ones replace by id. A chat's notifications close and its count starts
    again once it is read (its horizon moved and nothing in it is unread), opened from one, or shown in a focused
    window when the next message comes.
  - The tray item is `org.kde.StatusNotifierItem` with a `com.canonical.dbusmenu` menu ("Open Teams", "Quit") under
    `org.kde.StatusNotifierItem-<pid>-1`, registered with the watcher; its title and tooltip carry the unread total
    of the listed chats, its status `NeedsAttention` while it isn't 0, its icon named from the theme. Closing the
    window keeps the app only when the item is up (a watcher took it); without one, closing quits as before.
  - The app holds its desktop and bridge in a GPUI global for its whole life: nothing else holds the desktop once its
    window is closed, and dropping the bridge stops the engine (the window test found it).
  - The bus the app is given is its `DBUS_SESSION_BUS_ADDRESS`, read once (`bus::address_from_env`); one that is a
    list is refused, and without one there are no notifications and no tray. `reach.rs` refuses `zbus`'s session-bus
    constructors, its blocking API and GPUI's system notifications in the app crates, and any connection built
    outside `bus.rs`.
  - The view trace gained `notification shown` (the chat and the kind: message, mention, collapsed), `notification
    opened`, `notifications closed`, `tray unread` (the total), `tray open`/`quit` and `window closed`.
  - `teams-mock` gained `ChatServiceMock::post_as` (another account's post, stored and delivered as a client's), and
    the sandbox's bus `PrivateBus::invoke_action` (the fake server's `ActionInvoked`).

### Task 15: Local search
**Files:**
- Create: `crates/teams-ui/src/search.rs`, a store migration (the FTS5 table), tests
- Modify: `crates/teams-store/src/*`, `crates/teams-core/src/*` (plain text on upsert, the backfill, a search query)

- [x] tests first: the migration creates the FTS5 table only; `teams-core` hands each message's plain text (from
      `RichText`) when it stores it (edits replace, deletes remove); a backfill on the engine's start indexes rows
      stored before, in bounded batches, and resumes after a stop
- [x] tests first: a query across chats with its hits in order; user input quoted before `MATCH` (a query with `"`,
      `*`, `-`, `:`, `NEAR` and parentheses finds the literal text and never errors); a hit opens its chat at the
      message (Ctrl+Shift+F, and from the switcher)
- [x] the server's search is not used (Substrate lists other tenant users: never let through in the lab)
- [x] `cargo test -p teams-store -p teams-core -p teams-ui` and the gate pass
- Done (2026-10-22), where it differs from the lines above:
  - Migration `0008_search` creates and fills nothing, but holds two tables, not one: the FTS5 table
    (`message_search`: `unicode61` with diacritics removed, prefix indexes of 2 and 3 characters) and its bookkeeping,
    `search_entries` (one row per message indexed, at the version its text was read from, its id the text's rowid; a
    message without text has a row and no text). The backfill resumes from it and an edit replaces by it; a trigger
    drops a text with its row (a conversation deleted cascades).
  - The core indexes in the store's own write (`Store::upsert_message_indexed`, the text read by a closure the core
    hands in, `teams_core::search_text`): `Outbox::receive`, the one way the core stores a message, uses it, so a
    delivery, a catch-up, older history and our own message coming back are all indexed; a newer version replaces the
    text, a deleted one (stored without content) removes it. The text is `RichText::plain_text` (a reply's quote
    included), a `Text` message's content as it came, none for a thread event or a deleted message.
  - The backfill (`teams_core::search::Backfill`): the messages whose index row is missing or older than their
    version, in the store's order of conversation and id, `EngineOptions::index_batch` (200) a step, from the engine's
    loop as a branch that is ready while it runs (`select!` picks at random, so a request waits for one batch at
    most). Once over it is told as `Event::Indexed { messages }` (`teams-headless` prints `indexed`); a store error
    is told and gives the backfill up for the run (the next start goes on). `EngineRequests::search(query, limit)`
    answers from the store.
  - The query: each whitespace-separated term with a letter or a digit is quoted as an FTS5 string (its `"` doubled)
    and matched as a prefix, all of them needed, at most 16; a query of none finds nothing without asking SQLite. The
    hits come the newest first, each with FTS5's snippet (the matches between U+E000 and U+E001, which are removed from
    every indexed text). The store quotes; the views pass the input as typed.
  - The views (`search.rs`): the search is the kit's command palette (its own filter off), the switcher lists the same
    hits as a "Messages" group under its "Chats"; a query of at least 2 characters is asked (200 hits; those of chats
    the list doesn't show, a channel's or an own stream's, are left out; 50 shown), each with the chat's name, the
    sender (`Core::person`'s name, "You" for the account) and the matched text marked. Each change asks again and
    drops the earlier search. One of the two is open at a time; Escape closes either. A hit opens through
    `Shell::open_message`: the chat opens and `ChatView::reveal` reads the store back to the message (the window load
    reads down to it), selects it and scrolls to it, and the list stops following the newest.
  - The view trace gained `search opened`/`closed`/`shown` (the hits' conversation and message ids) and `chat-view
    revealed`; a query or a text never.
  - The server's search: `reach.rs`' `the_search_is_the_store_s_and_never_the_service_s` (no client crate names
    Substrate's host or paths, the views ask only `Core::search`, the engine answers it from the store, and no
    `Service` names Substrate). The tests: `teams-store/tests/search.rs`, `teams-core/tests/search.rs` and the engine's
    two on the mock (`tests/engine.rs`), `teams-ui/tests/search.rs` on the fake core (whose search plays the store's).
    The sandbox window test isn't extended: the keys are the view tests'.

### Task 16: Teams and channels, read-only
**Files:**
- Create: `docs/protocol/channels.md`, `tests/fixtures/csa/*` (synthetic),
  `docs/backlog/channel-writes-need-a-lab-only-team.md`, tests
- Modify: `crates/teams-proto/src/{endpoint.rs,http.rs,read.rs}` (CSA's team reads, `GET` only),
  `crates/teams-mock/src/*` (a team, its channels), `crates/teams-ui/src/*`, `docs/protocol/endpoints.md`

- [x] a read-only probe first: CSA's `GET api/v3/teams/users/me` (teams and channels) with our `chatsvcagg` token, and
      a channel's messages through the chat service's conversation reads; shapes into `channels.md`, synthetic
      fixtures
- [x] tests first: CSA as a new `Service` (its host, the `chatsvcagg` audience, its path check), `GET` only
- [x] tests first: the Teams rail lists teams with their channels; a channel's posts and reply chains shown read-only;
      no composer for a channel, and no channel write exists natively (no sealed type for it)
- [x] the backlog item: channel writes need a team of the lab accounts only (a write the tenant's directory shows,
      the user's decision) before they can be mapped and tested; the personal build stays read-only until then
- [x] `cargo test -p teams-proto -p teams-mock -p teams-ui -p fixture-guard` and the gate pass
- Done (2026-10-22), where it differs from the lines above:
  - The probe (`proto-lab probe teams <alias>`, `crates/proto-lab/src/commands/teams_probe.rs`) ran live once as a
    (GETs only, under the live lock): CSA took our `chatsvcagg` token with `Accept` alone (200, about 32 KB: 2 teams,
    10 channels, each general channel's id the team's); the chat service's list held only the 2 general channels; a
    channel's thread is `space` (general) or **`topic`** (another); a channel's page holds posts and replies together,
    every message's `conversationLink` ending `;messageid=<post>`; one post's chain read (`<channel>;messageid=<post>`)
    answered too but isn't used. It prints key paths with types, counts and two protocol enums' plain values, never a
    name, text or id. Shapes in `channels.md`; fixtures `tests/fixtures/csa/teams.json` and
    `tests/fixtures/chatsvc/channel-messages.json`.
  - `Service::Csa`: the base only from `HttpTransport::with_csa` (the GTM `chatSvcAggAfd`, an AFD host,
    `/api/csa/<region>`; `lab.rs` gives it when the region's is of that form), one endpoint (`api/v3/teams/users/me`
    with the web client's query exactly, in order: `CsaTeams`), `GET` only (`own_state`/`post_read` refuse it), the
    `chatsvcagg` token and `Accept` only. `ConversationKind::of` takes `topic` as a channel; `Message::reply_root`
    reads a channel message's post from its `conversationLink`.
  - The store (migration `0009_teams`): `teams`/`team_channels` replaced whole by each read, `teams_read`, and
    `reply_roots` (a stored channel message's post, kept as the core stores it); the migration drops the channels'
    sync states so their messages stored before get their posts on the next read. The core: `teams.rs` reads CSA in
    the background when a view asks and the list is missing or an hour old (`Event::Teams` on a change, a failed read
    waits `EngineOptions::teams_retry`), `EngineRequests::{teams, sync_conversation, reply_roots}`; a channel other
    than a team's general one is in no conversation list, so the channel view syncs it on open.
  - The UI: `teams_list.rs` (the rail's list, "Loading teams…" until a read settled it) and `channel_view.rs` (threads:
    each post with its replies, the oldest first; replies whose post isn't held under "Replies to an earlier post";
    "Load older posts"; no composer, no action, no read receipt; `Main::Channel`, the header "<team> › <channel>"). The
    trace gained `teams-list shown`, `channel opened` and `channel-view shown` (ids and kinds only). `reach.rs`'s
    `no_channel_write_exists_and_the_teams_views_write_nothing` checks no `ChannelPost` mapping in `teams-proto` and
    no write call in the two views; `teams-core/tests/teams.rs` shows a post and a read receipt to a channel denied by
    the fence with nothing but reads reaching the mock.
  - The mock gained CSA's read (`csa.rs`) and the team's other channel (`TEAM_CHANNEL`, unlisted, `topic`). The
    native CSA read ran only on the mock; the app's first live run is Task 25's. The sandbox window test isn't
    extended.

### Task 17: Others' presence
**Files:**
- Modify: `crates/proto-lab/src/post_read.rs`, `crates/proto-lab/tests/post_reads.rs`,
  `crates/teams-proto/src/{endpoint.rs,post_read.rs}`, `crates/teams-core/src/*`, `crates/teams-ui/src/*`,
  `docs/protocol/endpoints.md`, `docs/protocol/auth.md`, `docs/protocol/trouter.md`

- [x] tests first: the browser fence lets UPS's `POST v1/presence/getpresence/` out as a read by `POST` only in its
      captured request shape and only when every MRI it names is a lab account
- [x] live: the first allowed run (an `ab-chat` run) is judged by its request naming only lab MRIs; its answer is
      captured and reviewed right after, before anything native: it must name nobody but the accounts asked for (if
      it does, the allowance is taken back and this task stops with a ⚠️); answer `auth.md` question 4 (the presence
      token's audience, and whether ours is accepted)
- [x] tests first: presence (UPS) as a new `Service` (host from its GTM, its audience per question 4) and the native
      `PostRead` entry for `getpresence`, polled at a bounded interval for the lab accounts in view (no pubsub
      subscription: it changes service state, so it is no read; a backlog item if polling proves not enough)
- [x] presence dots in the chat list, the chat view's header and the mention picker; "unknown" when a read fails
- [x] `cargo test -p proto-lab -p teams-proto -p teams-core -p teams-ui -p fence-seal` and the gate pass
- Done (2026-10-23), where it differs from the lines above:
  - The browser entry (`post_read::PostReadKind::Presence`, `fence.log` `presence`) is the shape of the 5461 captured
    reads: either Teams origin, `/ups/<geo>/v1/presence/getpresence/` with its trailing slash, no query at all, only
    the captured headers (pinned: `Content-Type: application/json`, `behavioroverride: redirectAs404`,
    `x-ms-client-type: cdlworker`, `x-ms-client-user-agent: Teams-V2-Web`, an empty `x-ms-object-id`), a body of
    `{"mri"}` objects with at most `"source": "ups"`, no MRI twice. A read carrying `x-ms-endpoint-id` (2065 of the
    5461: the client's endpoint, an id the fence has no check for) stays `Unknown`. Fixture
    `tests/fixtures/ups/getpresence-requests.json`; the `presence reads` expected-deny class stays for the rest.
  - Live: `proto-lab run ab-chat --headless` (exit 0, ok) let 14 reads through, each naming only a, b or c (1–3
    MRIs), none with `x-ms-endpoint-id`, none denied; every answer was 200 and named only the MRIs asked (one item per
    MRI: `mri`, `presence` with `availability`/`activity` `Offline`, `etag`, `status` `20000`). The answer fixture
    `tests/fixtures/ups/getpresence-answer.json` comes from the reference copy `reference/presence/getpresence-ab-chat.jsonl`
    (exported from that capture's presence events only). `auth.md` question 4 answered: the audience is
    `https://presence.teams.microsoft.com/`, and our non-CAE token is taken (`teams-headless a presence`, under the
    live lock: two reads, b and c `Offline`, two `post_read` `presence` lines in `fence.log`).
  - Native: `Service::Presence` (base only from the GTM `presenceUPS` via `HttpTransport::with_presence`; a `Read`
    or own-state request to it refused), `teams-proto`'s `PresenceRead` (the body `[{"mri"}]` only, never `source`;
    the answer dropped whole if it names anyone not asked; an item's words read only with `status` `20000`).
    `teams-core`'s `presence.rs`: `EngineRequests::presence(mri)` answers the last read at once and watches the
    person for the run (at most 20, the one asked longest ago dropped); every minute (`EngineOptions::presence_poll`;
    at once for someone newly watched) one read of the watched people `Fence::lab_accounts_only` admits, under the
    fence's permit; a throttled read waits its `Retry-After`; each change told as `Event::Presence`; a failed read
    makes everyone it asked about `Availability::Unknown`. Not stored. A `teams-headless <alias> presence` command
    (not in the Files list) reads the other lab accounts' once, for the live check.
  - UI: `teams-ui`'s `presence.rs` (the `Presences` model the bridge fills from `Event::Presence`, the dot, a hollow
    ring for unknown); the chat list's 1:1 rows (on the avatar), the chat's header (the shell's, a 1:1 chat only) and
    the mention picker. `Core::presence` is asked as the rows and the picker's members are read.

### Task 18: Own presence (1/3): the rule and the research
**Files:**
- Modify: `CLAUDE.md` (the exception, first), `crates/proto-lab/src/{classify.rs,scenario/*}`,
  `docs/protocol/{trouter.md,endpoints.md}`

- [x] CLAUDE.md first, in the words of the user's decision of 2026-10-21: a lab account may set its own availability
      (UPS's presence endpoint `PUT …/v1/me/endpoints/`, the `user.activity` frame and, once captured, the manual
      status request), in their captured shapes and availability values only; never a status note, out-of-office or
      a profile field; every id it names must be the account itself or its own registered endpoint
- [x] live, under the fence, step by step: (1) what makes a show online as b sees it (b's `getpresence`, Task 17):
      `user.activity` alone, the endpoint `PUT` alone, both, each mapped in its captured shape before it is allowed;
      (2) a's status set by hand in the web client (Busy, Do not disturb, Away, Available) with that request denied,
      to capture it; then mapped in its captured shape and allowed in a later run; how a manual status is cleared;
      written up in `trouter.md`; a breached run as CLAUDE.md says
- [x] `cargo test -p proto-lab` and the gate pass
- Done (2026-10-23), where it differs from the lines above:
  - The browser fence's mapping is a module of its own, `crates/proto-lab/src/availability.rs` (not in the Files
    list), behind `Classifier::with_own_availability` (the `own-presence` scenarios only): the endpoint `PUT` and the
    manual status (`PUT …/v1/me/forceavailability/`: a set of `Busy`, `DoNotDisturb`, `Away` or `Available`, or the
    reset, the same `PUT` with no body) in their captured shapes, let through as own state `own_availability` only on
    an endpoint this browser's fence let the account register (enrolled after the registration's `Allow`), a reset
    only when Chrome's event shows no body at all (`PausedRequest::body_unread`). Settled with Codex (transcript
    `discussions/task-18-availability-mapping`). `teams-signin`'s guard gained the new classification (unclassified).
  - Live (ten runs of the `own-presence-*` scenarios, b's view sampled by `teams-headless b presence` through
    `scripts/own-presence-run.sh`, not in the Files list): **the endpoint `PUT` makes a show online to b;
    `user.activity` alone does not**. Without `user.activity` a went offline 10–25 s after its browser closed, with
    it about 6 min after. The manual status was captured denied (run 7) and let through (runs 8–10, each answered
    `200`); the reset clears it. In every run that let it through the web client sent no endpoint `PUT`, and with no
    live endpoint b read `Offline` throughout, Busy included: whether a manual status overrides an endpoint's
    `Available` as b sees it is left to Task 19's native runs, which send the endpoint `PUT` themselves. Run 8's b
    samples were lost with `/tmp` in the reboot. No run was breached. Written up in `trouter.md` ("Own presence"),
    rows in `endpoints.md`, the scenarios in the README.

### Task 19: Own presence (2/3): the fenced write
**Files:**
- Modify: `crates/teams-proto/src/{write/*,frame.rs,endpoint.rs}`, `crates/teams-fence/src/{target.rs,decision.rs}`,
  `crates/teams-trouter/src/*`, `crates/teams-core/src/*`, `crates/fence-seal/tests/ui/*`

- [x] tests first: a new `WriteKind` and `Scope` for "the account itself": `Fence::judge` permits it without a member
      read only under the exception (today it reads members for every scope but `New`), for the fence's own account
      only
- [x] tests first: the endpoint `PUT`'s `id` is checked as this account's own registered Trouter endpoint (the one
      the push channel registered this run), any other id `Unknown`; availability values only those captured
- [x] tests first: the sealed `Mutating` write(s) for availability (and the manual status, if Task 18 mapped it), sent
      through the outbox's transient kind; `user.activity` as an `OwnFrame` entry if Task 18 shows it is needed,
      logged before it is sent; a `fence-seal` case
- [x] `cargo test -p teams-proto -p teams-fence -p teams-trouter -p teams-core -p fence-seal` and the gate pass
- Done (2026-10-23), where it differs from the lines above:
  - `teams-fence`: `WriteKind::OwnAvailability` and `Scope::Account { mri }`. `Fence::check` reads no members for
    either; it permits the own availability only on `Scope::Account` naming the fence's own account, a lab account,
    with nobody added (`DenyReason::NotOwnAccount` otherwise, before any fetch), and denies any other kind on
    `Scope::Account` (`DenyReason::AccountScope`). The own account comes from `Fence::open_lab_account(alias)` (the MRI
    `lab.toml` gives it; `None` while unknown, which permits nothing); `Fence::open_lab` (the browser fence's) owns no
    account. `teams-headless` and `teams-app` now open their fence with it. Tests: `teams-fence/tests/own_availability.rs`.
  - The endpoint check is by construction, in `teams-proto`: the writes take a `RegisteredEndpoint`, which only
    `teams_proto::register` makes (no public constructor), from an `EndpointRegistration` the account's transport sent
    and the registrar answered with a success; the push channel's registrar goes through it and hands the endpoint on
    as `PushEvent::Registered`, and the engine keeps it while the channel runs (dropped when it stops). So no other id
    can be named at all, rather than one named and found `Unknown`.
  - `teams-proto`: `SetEndpointAvailability` (the endpoint `PUT`, `Available`/`Available` only: the captured
    `Busy`/`InACall` is a call's) and `ForceAvailability` (`set` of the four captured values, `ManualAvailability`;
    `reset` with no body), on UPS (`Wire` gained its service and headers), sent with the presence token, the presence
    read's header values, `x-ms-endpoint-id` and `Content-Type` (the reset too), without the caller, session ids or
    cookies (as Task 17 left them out of the read); the transport takes them only as `Authorized` writes, to the two
    `v1/me/…/` paths. Fixtures checked: `tests/fixtures/ups/endpoint-requests.json`, `manual-status-requests.json`.
  - `teams-core`: the transient kinds `Transient::EndpointAvailable` and `Transient::ManualStatus`, and
    `EngineRequests::own_availability(OwnStatus)` (`EngineError::NoEndpoint` before a registration of this run went
    through). `lab.rs`'s writes' transport gets UPS's base too (found missing by the first live run). No
    `user.activity` frame: Task 18 showed it isn't needed to show online.
  - `fence-seal` cases: `fence_own_account`, `fence_open_account_config`, `proto_registered_endpoint`,
    `proto_availability_literal`. `teams-mock` serves the two UPS `PUT`s (`ups.rs`, not in the Files list), checking the
    endpoint against its registrar's registrations.
  - Live (not in the lines above): `teams-headless <alias> availability` (not in the Files list) and
    `scripts/own-availability-native-run.sh`. Run 1 (07:15 UTC) sent nothing (the outbox's transport had no presence
    base: three permits, each refused by the transport before sending); run 2 (07:26 UTC, after the fix): b read a
    `Available` 12 s after the endpoint `PUT`, **`Busy` 6 s after the manual status beside the live endpoint**, back to
    `Available` with the reset, and `Offline` 5 min 20 s after the one `PUT` while the socket was still up: the `PUT`
    lapses and has to be sent again (Task 20's policy). `fence.log`: three `own_availability` permits a run, nothing
    denied. Written up in `trouter.md` ("The native client's own availability"), rows in `endpoints.md`.

### Task 20: Own presence (3/3): the app's policy
**Files:**
- Modify: `crates/teams-core/src/*`, `crates/teams-ui/src/*`

- [x] tests first: the app reports Available while it runs and the window was used recently, Away after the idle time
      (a setting), Offline on quit (best effort, transient); a manual status from the account menu overrides it until
      cleared; a failed report shown, never retried outside the transient kind's rules
- [x] `cargo test -p teams-core -p teams-ui` and the gate pass
- Done (2026-10-23), where it differs from the lines above (settled with Codex, transcript
  `discussions/task-20-away-offline`):
  - The policy is `teams_core::own_presence` (`OwnPresencePolicy`: inputs in, the `OwnStatus`es to send out, on the
    caller's clock), driven by `teams-ui`'s `OwnPresence` (`own_presence.rs`), which the desktop holds for the app's
    life (its refresh goes on in the tray) and hands to each window's shell; the shell's mouse and key input is the
    window's use. **Available**: the endpoint `PUT` at each registration that went through while the window was used
    within the idle time (the start counts as use) or a manual status is set, and again every 2 min
    (`AVAILABLE_REFRESH`, inside the 5 min 20 s lapse Task 19 saw); a registration while idle sends nothing, the next
    use sends it at once.
  - **Idle reports no Away**: nothing is sent and the endpoint lapses on its own, so others see `Offline` some minutes
    later. The only captured Away is the manual status, account-wide server state that overrides other devices and
    outlives the app (a suspend, a crash), so it is never sent on the user's behalf; the setting reads "Stop reporting
    me available after". **Quit sends nothing** (no captured request reports Offline): the refresh stops before the
    engine's shutdown closes the push channel. Both in `docs/backlog/own-presence-idle-and-offline-shapes.md`.
  - The account menu is the rail's status button (above Settings) with the statuses and what is reported; it and the
    settings view change the same `manual_status` setting, now Automatic and the four captured statuses only ("Be
    right back" and "Appear offline" dropped: a file naming one is reported, Automatic kept). A manual status is sent
    when chosen and once at the run's first registration (Automatic at the start sends no reset), keeps the endpoint
    refreshed while idle, and Automatic sends the reset. A failed report is shown in the menu and traced
    (`own-presence` `failed`), never sent again; a fence denial stops the reports for the run.
  - Not in the Files list: `docs/protocol/{trouter.md,endpoints.md}` (the policy written up), the backlog item, and
    `teams_core::RegistrationReport` re-exported for the feed's new `Registered`, `PushStopped` and `OwnReport` events.

### Task 21: Images (1/2): settle sending
**Files:**
- Modify: as `docs/backlog/ams-image-sending.md` lists (`crates/proto-lab/src/classify.rs`), `docs/protocol/chat.md`,
  `docs/protocol/endpoints.md`, the backlog item

- [x] the backlog item's step 1: tests first for a reviewed one-time mapping of exactly the captured object creation
      (the async gateway host only, the captured headers and body, one `permissions` key that is the a–b chat),
      judged by `Fence::check`, uploads and forwards still denied (tests that they are)
- [x] live: the object created once, then a read-only `GET` of it: does it list its readers, completely? Decide with
      Codex whether that read is authoritative; recorded in `chat.md` and the backlog item; a breached run as
      CLAUDE.md says
- [x] if it isn't: images stay deferred, shown as a placeholder with an "open in Teams on the web" link (tests), Task 22
      is skipped, and the backlog item records what this task learned
- [x] `cargo test -p proto-lab -p teams-ui` and the gate pass
- Done 2026-10-22: the creation was mapped once (`ab-image-object`, judged by `Fence::check` for the a–b chat, let
  out once per run, uploads and forwards force-denied) and created live twice (the first run breached by a system
  suspend 15 s after the creation, the second clean); `proto-lab probe object` read it: 200 to a and b, 403 to c,
  eight keys of metadata and no reader. Not authoritative (decided with Codex): images stay deferred, Task 22
  skipped, the mapping removed again in the same task (kept in its first commit), the read-only probe kept, an image
  shown as a placeholder with a link to its message in Teams on the web (`docs/protocol/chat.md`, the backlog item).

### Task 22: Images (2/2): send and show (only if Task 21 found the read authoritative)
**Files:**
- Modify: `crates/proto-lab/src/classify.rs`, `crates/teams-proto/src/{endpoint.rs,http.rs,own.rs,write/*,chat/*}`,
  `crates/teams-core/src/*`, `crates/teams-ui/src/*`, `docs/protocol/{chat.md,endpoints.md}`

- [x] (skipped: Task 21 found the read not authoritative) tests first: the upload judged by a fresh read of the object's readers (all lab accounts), the forward race
      assessed and recorded; the captured upload answer and message post mapped
- [x] (skipped: Task 21 found the read not authoritative) tests first: a native `SendImage` (object, upload, post), each step fenced; pasting an image into compose sends
      it
- [x] (skipped: Task 21 found the read not authoritative) tests first: viewing: the media service's token exchange (`/v1/skypetokenauth`) reviewed as own-state in its
      captured shape; images fetched by `GET` from the async gateway only; thumbnail inline, full size on click; the
      bytes reach the view only from the engine
- [x] (skipped: Task 21 found the read not authoritative) live: a sends an image to b natively; b's app shows it
- [x] (skipped: Task 21 found the read not authoritative) `cargo test -p proto-lab -p teams-proto -p teams-core -p teams-ui -p fence-seal` and the gate pass

### Task 23: Received files
**Files:**
- Create: `docs/backlog/file-sending.md`, tests
- Modify: `docs/protocol/{chat.md,endpoints.md}`, `crates/teams-proto/src/chat/*`, `crates/teams-ui/src/*`

- [x] capture the web client sending a file in the a–b chat with every write of it denied (as M1 Task 20 did for
      images); write up what it sends in `chat.md`; whether the file's sharing can be fenced (its readers are the
      chat's members) goes to `file-sending.md`, with what the capture showed
- [x] if the capture shows the message's file shape (a denied post's body): tests first (synthetic fixtures of that
      shape): a file in a message is parsed (name, size, type, link) and shown as a card; activating it opens its
      link through `open_url` (not taken: no post with a file went out; the web client stopped before any write of
      the file, so no file shape was captured and no fixture of one can be written)
- [x] otherwise (no post went out, as with images): a message whose content no capture showed is shown as
      "unsupported content, open in Teams on the web", never guessed (`Field::Unknown`), and the file card waits in
      `file-sending.md`
- [x] `cargo test -p teams-proto -p teams-ui -p fixture-guard` and the gate pass
- Done (2026-10-22), where it differs from the lines above:
  - A new scenario, `proto-lab run ab-file` (`crates/proto-lab/src/scenario/ab_file.rs`, a and b under `ab-chat`'s
    fence): force-denies every write to the file services (`SharePoint`, `OneDrive`, their media hosts, Graph), every
    AMS object write (`ab-image`'s rules) and every `POST`/`PUT` whose body names a lab file (the file name and the
    caption carry `lab-file-`); after the enforcement probe and a plain message, a types a caption and hands the
    compose box a made-up text file: a drop, a paste, then the "Attach files" button with the page's file pickers made
    to hand the file over without a dialog. A box that takes no file fails the run before anything is sent, noting
    the compose controls and what the page says, and the caption is taken out of the box again.
  - Live, 2026-10-22, each under the live lock, **no breach and no unexpected deny in any**: 01:37Z exit 0 (its
    attachment test took the typed caption for the file: the drop attached nothing, and the caption alone was posted
    with `files` `"[]"`, denied five times by the marker rule; reloaded, a deleted the unsent message locally, no
    request), 01:56Z exit 1 (the attach test clicked the profile button), 02:02Z and 02:16Z exit 1 (the "Attach files"
    button opened no menu and asked for no file; the page said "Sorry, you need OneDrive to share files. See your
    admin about getting a license."), 11:09Z exit 1 (a's page didn't load, Chrome's error page: nothing typed or
    sent), 11:17Z exit 1 (the same notice, read from the page, and the caption taken out of the box again). In none did a write of the file go out: the client's
    own file storage check, `POST <middle tier>/beta/me/files/availability` (no body), is denied by the fence (on
    the expected-deny list since M0), and it refuses files after that. Whether the lab accounts have a OneDrive at
    all is open. Written up in `chat.md` ("Files: sending, denied"); `file-sending.md` (worth: later) lists what would
    settle sending in order (a read-only drive check, the storage check as a reviewed read by `POST` of the account's
    own state, the upload into the account's own drive, the share judged by its recipients, the post) and keeps the
    file card.
  - Received: `teams-proto`'s `Message::files` is known only as an empty list (an entry makes it `Field::Unknown`
    with the raw value), and `Message::unshown`/`unshown_parts` name what no capture showed (`properties.files`,
    `properties.cards`, `amsreferences` with an entry), never read further. The chat view shows such a message's text
    with "This message has content that can't be shown here" and an "Open in Teams on the web" button that opens the
    chat (`web_chat_link`: `https://teams.microsoft.com/l/chat/<chat>/0`, `abc-chat`'s deep link form) through
    `open_url`; `properties` the view can't read count too. The trace names such a row's kind `unshown`. Tests:
    `teams-proto/tests/chat_read.rs`, `teams-ui/tests/chat_view.rs`, `proto-lab/tests/{ab_file.rs,scenario_dsl.rs}`
    (the scenario's page expressions on pages of their own), `abc_chat.rs` (`ab-file` force-denies the group chat's
    first message sent again).

### Task 24: The sign-in window
**Files:**
- Create: `crates/teams-signin/{Cargo.toml,src/main.rs,src/guard.rs}` (and, if chosen, its web process extension),
  tests
- Modify: `crates/teams-auth/src/*` (the parent's side), `crates/teams-app/src/main.rs`

- [x] first, with Codex: can a WebKitGTK guard cover everything the page can send, as the browser fence does for
      Chrome (CLAUDE.md: nothing runs live against the tenant before the fence is armed)? And the route: `webkit6`'s
      content filter plus `decide-policy`, or a hand-FFI web process extension (`cdylib`, its `unsafe` under the
      workspace lints). Recorded here before any code
- Decided with Codex (2026-10-21, agreed; transcript `transcript.md` under the run's
  `~/.local/state/plan-exec-revmux/20261021-native-teams-m2-gpui-daily-driver/discussions/task-24-webkit-guard/`;
  WebKit sources at the `webkitgtk-2.54.1` tag, the installed version):
  - **Route: the hand-FFI web process extension**, as the offline implementation route. Its `send-request` handler
    (`WebKitWebPage.cpp:313–325`: emitted synchronously, the request dropped when the handler says so) hands each
    request's method and URL to `teams-signin`'s UI process, which classifies it with the `sign_in` rules, appends
    the decision to `fence.log` (reads included: stricter than Chrome's unlogged reads) and only then answers; a
    timeout, a socket error or a failed append is a deny, and a page whose socket broke denies everything. The content
    filter plus `decide-policy` alone is rejected: `decide-policy` sees navigations only, a content rule list can't log
    a decision, and its `request-method` is the *initiating document's* method, not the subresource's
    (`ContentExtensionsBackend.cpp:243–273`), so it isn't a method fence at all.
  - Further layers, defence in depth (each exercised by an offline test where it can be, not an audited switch-by-switch
    guarantee): WebKit features off (`WebSocket`, which `send-request` never sees (`WebSocketChannel.cpp:149–180`),
    `BeaconAPI`, `ServiceWorkers`, `SharedWorker`, `PushAPI`, `Notifications`, `WebTransport`, `LinkPreconnect`,
    `LinkPrefetch`, `SpeculationRulesPrefetch`, `PrivateClickMeasurement`, `BackgroundFetchAPI`,
    `ProcessSwapOnCrossSiteNavigation`, which reduces process swaps but is no single-process invariant), WebRTC,
    hyperlink auditing, DNS prefetching, developer extras and the page cache off; an ephemeral network session; no new
    windows; downloads refused; the `nativeclient` redirect caught by `decide-policy` and refused by the guard as a
    request too, whichever comes first. Arming is correlated, not counted: a page's extension reports it armed (page
    and process id) after it has connected that page's handler, the first load starts only once the view's own page is
    armed, and a query from a page with no armed record is refused. A process that never arms is detected and the
    view destroyed: detection and containment, not a proof that nothing ran.
  - **Verdict: the guard is not complete**, so Task 24's live sign-in does not run. A synchronous load's redirect is
    followed inside the network process (`NetworkResourceLoader.cpp:1438–1449`), and so is a kept-alive load's after
    its page is gone (`1476–1483`), neither through a second `send-request`, a decision or a `fence.log` line;
    `NetworkLoadChecker` doesn't apply content rules to them (`NetworkLoadChecker.h:199`, `.cpp:625–628`), and no
    setting of 2.54.1 turns synchronous XHR or keepalive off. Either gap defeats "every decision logged before the
    request goes" on its own. Offline regression tests pin them (a synchronous GET → 302, an allowed POST → 307 under
    a test-only local own rule, an orphaned keepalive GET → 302), and `docs/backlog/webkit-sign-in-guard-redirects.md`
    records what would close them: a mandatory, inspecting egress guard (the only way out of every relevant
    subprocess, no direct connection or uninspected tunnel possible) with its own coverage proof, or a WebKit change.
    The lab keeps signing in through `proto-lab auth`.
- [x] tests first, offline, in the sandbox: a local fake sign-in page that redirects to the `nativeclient` URI with a
      code and the request's `state`; the redirect is caught in the navigation policy before it loads, the code is
      handed to the parent on a pipe (never logged), and `teams-auth` redeems it only for the account the ID token
      names (in the lab, the alias's account; in the personal build, the account it signs in)
- [x] tests first: the guard classifies each request as proto-lab's `sign_in` classifier does (`GET` reads, the
      seeded own-state rules in their exact shapes from Task 1, everything else denied), never by host alone; every
      decision is logged to `fence.log` (lab) before the request goes; a page's fetch, beacon, image, frame, worker
      and WebSocket outside that are refused; website data is ephemeral, service workers off, no new windows
- [x] only if Codex found the guard complete: one live sign-in of a lab account through it, under the live lock, only
      Entra's sign-in form let through, stopped by any multi-factor prompt. Otherwise the webview stays
      offline-tested and the lab keeps signing in through `proto-lab auth`; the personal build's sign-in policy is
      settled in Task 26 (not run: the guard isn't complete, see the decision above; the binary refuses any page
      that isn't on a loopback address, `teams_signin::cli::NOT_LIVE`)
- [x] `cargo test -p teams-signin -p teams-auth` and the gate pass
- Done (2026-10-21), where it differs from the lines above:
  - Three pieces: `teams-signin-extension` (the `cdylib` WebKit loads into every web process, hand FFI on
    `glib-sys`/`gobject-sys` and three `webkitgtk-6.0` functions, each `unsafe` op in its own documented block; its
    `rlib` side holds the guard's line protocol and the fail-closed client), `teams-signin` (lib + `teams-signin
    --lab` binary: the guard, the webview's setup, the refusals) and `teams_auth::signin` (the parent's side: one JSON
    request line on the window's stdin, one answer line on its stdout, `code_from_redirect` then `exchange_code`, so
    the code is redeemed only for the account the ID token names). `teams-signin` depends on the extension crate, so
    a `cargo build -p teams-signin` leaves the module in `deps/`, where the binary looks for it (or beside itself),
    checks it exports WebKit's entry point and copies it alone into a private `0700` directory with the guard's socket.
  - The answer channel is the binary's stdout, not a separate pipe descriptor: it duplicates stdout close-on-exec for
    the answer and points stdout at stderr before GTK starts, so nothing GTK, WebKit or their processes print reaches
    it (a test checks one line and nothing else). The code never reaches a log: `fence.log` keeps the redirect's URL
    without its query, as `sign_in_redirect` (a new `RequestOutcome`, beside `Read`: the window logs its reads too).
  - The window test (`crates/teams-signin/tests/window.rs`, `harness = false`, rerun inside the sandbox with a
    private `HOME`) runs the window in-process, not through the binary: its second scenario needs a test-only local
    own-state rule (`Guard::with_own`, `test-support`), which the binary refuses. It checks the redirect handed back,
    no `POST`, popup or WebSocket connection reaching the local server, WebSocket, beacons and service workers off,
    every request the server got allowed in `fence.log` first and the fetch, frame and worker posts denied there,
    no code or state in `fence.log`, no website data kept; and it pins the gaps (a synchronous `GET` → 302 and an
    allowed synchronous `POST` → 307 reach their targets undecided, the asynchronous `POST` → 307 is decided and
    denied; a kept-alive `GET` redirected after its page navigated away was followed undecided too, reported). The
    pipe to the parent is tested in `teams-auth` (`run_window` on a stand-in, the redemption on wiremock).
  - The guard classifies with proto-lab's own `Classifier::sign_in` (a dependency on `proto-lab`), not a copy, so it
    can't drift from the browser fence's rules; the personal build's sign-in policy (Task 26) may move it.
  - `teams-app` is unchanged: with the guard incomplete, no app path opens the window against the tenant; the
    personal build wires it in Task 27 under Task 26's policy.
  - `webkit6` 0.6.1 needs `gtk4`'s `v4_10` (it names `gtk::Accessible`); `webkit6`'s `v2_52` API level (2.54.1 on the
    machine). WebKitGTK 2.50 dropped the hyperlink auditing setting and 2.48 the DNS prefetching one, so neither is
    set; an `<a ping>` reaches the guard through the cached-resource loader.
  - Review fixes (round 1; settled with Codex, transcript under the run's
    `discussions/fixer-task-24-not-live-bound/`): `NOT_LIVE` now holds for the whole run, not only the first URL.
    The guard runs loopback-only (`Guard::local_only`), the navigation policy refuses anything off loopback, and the
    network session's only way off loopback is `teams_signin::egress`'s black hole (a custom proxy that logs and
    refuses; numeric `127.0.0.0/8` and `::1` bypass it, so `localhost` is no longer a local page). The window test's
    `remote` scenario pins it. Arming is checked for the view's first page only, not "a process that never arms is
    detected" as decided above: the process id is told but unused, and a later web process whose extension didn't
    load goes unnoticed (recorded in the backlog item). The run ends when the page's web process ends (`crash`
    scenario), and every end tears the view's web process and window down. The window has its Wayland app id
    (`APP_ID`, as GTK's program name), and the answer channel has a test of its own (`tests/answer_channel.rs`).

### Task 24a: The app's diagnostic log
Added on 2026-10-22 at the user's request. M2 must end with an app the user uses daily (Overview), and debugging it
needs a log. The user decided on 2026-10-22: comprehensive logging, and at `debug` full requests and responses, for
faster movement in early development. Today the app installs no logger (Technical Details, Task 7's audit); this
task replaces that with one logger of our own. `teams-headless`'s `TEAMS_HEADLESS_LOG` stays as it is.

**Files:**
- Create: `crates/teams-log/{Cargo.toml,src/lib.rs,src/redact.rs,src/file.rs,src/diagnostics.rs}`, tests
- Modify: `CLAUDE.md`, `README.md`, `Cargo.toml` (members), `crates/teams-proto/src/http.rs`,
  `crates/teams-trouter/src/{socket.rs,session.rs}`, `crates/teams-auth/src/session.rs`,
  `crates/teams-core/src/{engine.rs,outbox.rs,outbox/transient.rs,sync.rs}`, `crates/teams-store/src/schema.rs`,
  `crates/teams-fence/src/lab.rs` (the log's directory), `crates/teams-ui/src/{settings.rs,settings_view.rs,bridge.rs}`,
  `crates/teams-ui/tests/reach.rs`, `crates/teams-app/src/{main.rs,app.rs}`

- [x] CLAUDE.md first: amend the M1 pattern "No log line carries a payload": at `debug` the app's own diagnostic log
      carries full HTTP requests and responses (method, URL with its query, headers, bodies) and every Trouter frame
      both ways, by the user's decision of 2026-10-22; `info` stays payload-free. The hard rule "Tokens never reach
      logs" is unchanged: credentials are redacted by the logger itself (`Authorization`, `Cookie`/`Set-Cookie`, the
      skypetoken and every other token header, the token fields of sign-in and renewal answers, Trouter's
      `user.authenticate`), never left to call sites. A lab run's debug log is lab data like a capture (never in the
      repo, never sanitized into it); the personal build's is the user's data, which agents never read
- [x] tests first: `teams-log`, a small crate of our own (`log` only, no `tracing` subscriber), installed once by
      `teams-app` (and by `teams`, Task 27) before the engine starts: a file under the account's data dir (the lab's
      `<data_dir>/logs/<alias>/teams-app.log`, `LabPaths::app_log_dir`), `0600` in a `0700` dir, rotated by size
      (8 MiB, 4 older files kept, each `0600`); written from a thread of its own through a bounded channel, so GPUI's
      foreground never waits on the file (dropped lines are counted and the count logged); a log that can't be opened
      is shown once in the UI and the app runs without it. Level: `info` by default, the settings file's `log_level`
      (`error`, `warn`, `info`, `debug`; changed live from the settings view), `TEAMS_APP_LOG` overriding it; `trace`
      is taken as `debug`
- [x] tests first: only our own crates' records (`teams_…` targets) reach the file, as `teams-headless`'s log does: a
      third-party `log` line and a kit `tracing` event (forwarded to `log`, no subscriber) at any level never do; `log`
      stays capped at `debug` at compile time (`teams-trouter`'s `max_level_debug`; a test that
      `log::STATIC_MAX_LEVEL` is `Debug`), so `tungstenite`'s `trace!` doesn't exist in the binary: our transport and
      socket log requests and frames themselves, and the logger redacts them. `reach.rs` changes from "no logger" to
      "only this logger": `set_logger`, `set_boxed_logger`, `set_global_default`, `tracing_subscriber` stay refused
      everywhere but `teams-log`, and the app crates call only its `install`
- [x] tests first, `info`: engine actions and their outcomes (kind, conversation and message ids); the transport's
      requests (method, host, path without query, status, latency, the retry decision); Trouter connects, drops (with
      reason), reconnects and catch-ups (counts, conversations changed); outbox rows' state changes (row id, attempt,
      `clientmessageid`, the fence's decision, so a line links to its `fence.log` entry); session renewals (audience,
      expiry, outcome, as the hard rule allows) and signing out; store migrations (from and to); the UI's failures (a
      failed `Reply`, a view's error, by kind); a panic hook that writes the panic (redacted), its location, thread and
      a forced backtrace, flushes, then calls the previous hook. No body, query, header value, message text or display
      name at `info`: an offline run on `teams-mock` sends, receives, edits and renames with marker text and marker
      names and real-shaped tokens, and a grep of the file finds none of them
- [x] tests first, `debug` adds: every request and response in full (method, URL with its query, headers, bodies;
      a binary body, a picture or an image, by content type and size only) and every Trouter frame both ways. Redaction
      lives in `teams-log`: the transport and the socket hand it typed records (`teams_log::wire`) whose formatting
      redacts by header name (`Authorization`, `Cookie`, `Set-Cookie`, `X-Skypetoken`, `Authentication`, any name
      holding `token`), by JSON field (`access_token`, `refresh_token`, `id_token`, `skypetoken`, `token`), by form or
      query field of `teams-auth`'s requests and redirects (`code`, `code_verifier`, `refresh_token`) and
      `user.authenticate`'s arguments; then every line, any record's, passes a last pattern pass (JWT-shaped strings,
      `skypetoken=…`, `Bearer …`). Tests put real-shaped tokens in each of those places (a JWT, a skypetoken, a refresh
      token, a cookie, an authorization code, a `user.authenticate` frame) and in a panic message, and grep the
      rotated files for every one; a line the redactor can't parse is redacted whole, never written raw
- [x] tests first: a "copy diagnostics" action (the settings view and the command palette) puts on the clipboard the
      app's version and build profile, the log's level, the current log's last lines (capped), and the account's
      recent `fence.log` lines; when it includes any `debug` line it says so first, plainly ("contains full requests
      and responses: not safe to paste"), in the text and in the view
- [x] README: where the log lives, its levels, `TEAMS_APP_LOG`, rotation, "copy diagnostics", and that a debug log
      holds message text and must not be shared
- [x] `cargo test -p teams-log -p teams-proto -p teams-trouter -p teams-auth -p teams-core -p teams-store -p teams-ui
      -p teams-app` and the gate pass
- Done (2026-10-23), where it differs from the lines above:
  - `teams-log` takes `log` and `serde_json` (bodies and frames read as JSON to redact their fields by name), no
    `tracing` crate as a dependency (only as a dev-dependency, for the test that the kit's `tracing` events never reach
    the file). Its files: `lib.rs`, `redact.rs`, `file.rs`, `diagnostics.rs` and `wire.rs` (the typed records).
  - The wire records go under targets of their own (`teams_wire::http`, `teams_wire::trouter`), so `teams-headless`'s
    `TEAMS_HEADLESS_LOG` leaves them out by name and stays by kind and size (`teams_log::wire::is_wire`).
  - Redaction by name is wider than the list: any header, JSON or form field whose name holds `token` (`token_type`
    and `tokens` too), plus `password`, `client_secret`, `client_assertion`; the last pass also replaces any run of 100
    or more token characters (a refresh token or an authorization code met outside a named field).
  - A failed `Reply` is logged where it is read (`teams-core`'s `Reply`, by `EngineError::kind`), for every view at
    once; the bridge logs the shutdown, the sign-out and an engine that stopped on its own.
  - The log that can't be opened is shown as one of the settings view's problems (`Problem::LogUnavailable`) and
    printed once on stderr.
  - The command palette is the chat switcher (Ctrl+K): it gained a "Commands" section with "Copy diagnostics", which
    copies, closes the switcher and shows the settings, where the view says what was copied (`switcher.rs`,
    `shell.rs`, outside the file list). `fence.log` entries name no account, so "the account's recent `fence.log`
    lines" are the lab's `fence.log`'s last 40.
  - The mock has no rename: the offline `info` test changes a's display name in its stored message (as a later read
    gives it) and b catches up; the markers are message text, both accounts' display names and both JWT-shaped tokens
    (`crates/teams-core/tests/diagnostic_log.rs`). `debug` is tested on the transport too
    (`crates/teams-proto/tests/debug_log.rs`) and on every record type with tokens in each place, across rotated files
    (`crates/teams-log/tests/redact.rs`).

### Task 25: Verify the lab acceptance
**Files:**
- Create: `crates/teams-app/tests/ui_live.rs` (`required-features = ["live"]`), `docs/m2-acceptance.md`

- [ ] the live test is an orchestrator only: it starts the UI sandbox, spawns the app built by `cargo build -p
      teams-app --target-dir target/lab` (path from `TEAMS_APP_BIN`) for a, b and c, one app process each, drives them
      by keyboard through the sandbox's input, reads their view traces and `fence.log`; it opens no fence, holds no
      token and sends nothing itself
- [ ] credentials: for each account the orchestrator starts that binary's broker (`teams-app secrets --lab <alias>`)
      outside the sandbox, with the user's session bus, and hands the socket's other end to the app it spawns in the
      sandbox (`--secrets-fd`). It never reads or writes the socket itself, and the app's environment carries only the
      sandbox's bus. Every broker has exited when the run ends (checked); no token in the orchestrator's, the
      brokers' or the apps' output
- [ ] (`PROTO_LAB_LIVE=1`): a types and sends N = 20 messages to b; each is shown in b's chat view; **send → shown
      p95 under 2 s** (p50/p95/max reported); an edit, a reply and a reaction shown; a message to the group chat shown
      by b and c; b's notification raised on the sandbox's bus for a chat it isn't viewing; names and presence shown
      (b sees a online, then Busy when a sets it); a search in b finds a's message; a channel shown read-only; b's app
      closed, a sends, b's app reopened shows every message once and notifies once. Images and files only as far as
      Tasks 21–23 got
- [ ] `fence.log` for the run: allows only on the lab chats and the lab accounts' own presence, no deny of a mapped
      write, own-state, `PostRead` and frames only from the reviewed lists; screenshots of each step under
      `<data_dir>/screenshots/<time>/`, never the repo
- [ ] `proto-lab run ab-chat --headless` and `abc-chat` still exit 0; `teams-headless`'s `chat_live` still passes
- [ ] the gate passes; write `docs/m2-acceptance.md` (numbers, fence summary, flakes, any breached run next to its
      clean rerun)

### Task 26: The personal build's rules: design, then the user decides
**Files:**
- Create: `docs/personal-build.md`

- [x] write down the personal build from the direction of 2026-10-21 (Decisions above): the `teams` binary and its
      data (keyring name `teams-personal`, `~/.local/share/teams/`, no broker, no sandbox); its fence (every write a
      sealed type sent only as `Authorized<_>`, from an explicit action in the UI or a setting the user turned on, the
      target a conversation the account is a member of by a fresh member read, logged locally; the outbox's rules
      unchanged); its reads (`GET`s, own-state, `PostRead` for anyone in the account's chats, still never a search or
      suggestion list in M2); its sign-in (the guard's policy for the user's own sign-in pages: a federated identity
      provider or multi-factor prompts the lab never showed); what the lab can't show and what the personal build
      does without it (channel writes, new chats, server search: not in M2); the first run's checklist and
      Conditional Access (`auth.md` question 6)
- [x] what "agents never build a personal build" means once the code exists: the gate compiles and mock-tests it
      (allowed); producing a release or installed personal binary, or running one against the tenant, is not
- [x] how lab and personal binaries are kept apart: each refuses the other's data dir and keyring name, both refuse
      `test-support`; the personal fence lives in `teams-fence` (permits are sealed there) behind a feature only the
      `teams` crate enables, with a manifest test over `cargo metadata -p teams-app` that it is absent and `teams-app`
      refusing to run when feature unification brings it in, as it refuses `test-support`
- [x] the CLAUDE.md text it needs, as a proposal in the doc (the hard rules name the lab today)
- [x] ⚠️ stop here for the user's decision. No code that loosens the fence for a personal build is written before it;
      on approval, CLAUDE.md gets the approved text, and Task 27 follows it (blocked: awaiting the user's decision —
      docs/backlog/personal-build-rules-await-the-users-decision.md; no fence-loosening code written)
- [x] `cargo test -p fixture-guard` passes
- Done (2026-10-22): `docs/personal-build.md`, a proposal; CLAUDE.md unchanged. Where it goes past the lines above:
  the personal fence's question is membership (a fresh read listing the account), not a complete roster; a member
  form no capture showed, a meeting chat and a `48:` stream leave the write denied; the sign-in window gets a
  `--personal` mode whose egress proxy becomes a host gate (Entra's sign-in hosts and the identity provider's hosts
  the user names), the guard's two redirect gaps accepted for the user's own sign-in only if the user approves; and
  "agents never build a personal build" is read as no runnable (non-`test-support`) `teams` binary in any profile,
  debug included, since a debug build reaches the tenant as well as a release one. Each is listed for the user's
  decision in the doc's last section and in the backlog item.

### Task 27: The personal build
**Files:**
- Create: `crates/teams/{Cargo.toml,src/main.rs,src/lib.rs}`, tests, `packaging/teams.desktop`,
  `scripts/install-personal.sh`
- Modify: `crates/teams-fence/src/*` (the personal fence), `crates/teams-auth/src/*`, `crates/teams-ui/src/*`,
  `crates/teams-app/src/main.rs` (its refusal), `crates/fence-seal/tests/ui/*`, `README.md`

- [ ] only after Task 26's approval, and as the approved CLAUDE.md text says
- [ ] tests first (on `teams-mock`, the binary's lib): the personal fence permits a write to a chat with an outsider
      that the lab fence denies, and denies one to a conversation the account isn't a member of (by a fresh member
      read), an unknown target or a partial read; a `PostRead` for a non-lab MRI the account's chats name is allowed,
      a search shape is not; a `fence-seal` case that only the personal binary's start can open the personal fence
- [ ] tests first: the keep-apart rules of Task 26 (the manifest test, `teams-app`'s refusal, each binary refusing the
      other's data dir, `lab.toml` and keyring items, both refusing `test-support`)
- [ ] tests first: `teams` signs in through `teams-signin`, stores its tokens under `teams-personal` on the desktop's
      Secret Service, opens its store under `~/.local/share/teams/`; on an invalid grant it shows "signed out" and
      reopens the sign-in window, and nothing is sent until it is signed in again
- [ ] the settings that make writes happen on their own (read receipts, typing, presence) shown on the first run, with
      their defaults; nothing else goes out without a user action
- [ ] `packaging/teams.desktop` and `scripts/install-personal.sh` (a release build installed under `~/.local/`), for
      the user to run; agents don't run it
- [ ] `cargo test -p teams -p teams-fence -p teams-auth -p teams-ui -p teams-app -p fence-seal` and the gate pass

### Task 28: [Final] Update documentation
- [ ] README.md: the M2 crates, `teams-app`, `teams`, the UI sandbox, the live UI test, the UI kit and its licence
- [ ] CLAUDE.md: the patterns M2 settled (the UI sandbox, the bridge, the engine lock, the outbox's transient kind,
      the credential broker, the view trace, the UI kit's audit, `PostRead`, own presence, the diagnostic log and its
      redaction, the personal build's rules as approved)
- [ ] `docs/protocol/endpoints.md` checked against every endpoint M2 touched
- [ ] write the M3 plan (calls), informed by M2; the calendar in the roadmap
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
The personal build (`teams`, `scripts/install-personal.sh`) is never built for or run against the tenant by an agent.

## Roadmap after M2
- **M3 — calls**: first resolve `docs/backlog/fence-call-follow-ups.md` (an authoritative call-membership read), then
  the `sdp-ngc-1.0` conversion and the echo call (`docs/spikes/echo-call.md`), then 1:1 and group media.
- **M4 — meetings**: joining scheduled meetings, lobby, roster, meeting chat, content sharing; the calendar (Graph's
  calendar view, read-only, an agenda with join links), moved out of M2 on 2026-10-21.
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
  - **image sending**, if Task 21 leaves it deferred (`docs/backlog/ams-image-sending.md`), and file sending, if
    Task 23 does (`docs/backlog/file-sending.md`);
  - channel writes, if the user approves a team of the lab accounts only
    (`docs/backlog/channel-writes-need-a-lab-only-team.md`); starting new chats and the server's search, in the
    personal build only.
- **Cross-cutting**: drift detection (a scheduled bundle fetch and a diff report), the sanitizer's known gaps,
  `login`'s one-time-code prompt.

## Post-Completion
*Items requiring a human or a decision — informational only.*

**Manual steps:**
- Lab: none expected: the accounts are signed in and `proto-lab auth` keeps their tokens. If Microsoft asks a lab
  account for a one-time code, `login` stops (`docs/backlog/one-time-code-sign-in-under-the-fence.md`) and the user is
  asked.
- **The user's first run of the personal build** (after Task 27): run `scripts/install-personal.sh`, start `teams`,
  sign in through the sign-in window (Conditional Access and any MFA prompt: `auth.md` question 6), check the chat
  list and a chat read back, review the settings for read receipts, typing and presence, then send a first message to
  a self-chat or a colleague told beforehand. Report anything odd before relying on it.

**Decisions for the user:**
- **The personal build's rules** (Task 26): approve or change `docs/personal-build.md` and its CLAUDE.md text. Agents
  never build or run it against the tenant.
- **A lab-only team** (backlog): whether to create a team of a, b and c only (a group the tenant's directory shows),
  so that channel writes can be mapped and tested.
- **Native group-chat enrolment**: whether the group-chat rule may admit chats the native client creates, and on what
  binding (roadmap).
