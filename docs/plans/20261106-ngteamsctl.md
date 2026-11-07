# ngteamsctl: scripting the running ngteams from the command line

## Overview
- `ngteamsctl` is a command-line client that drives the **running** `ngteams` over a control socket. It covers reads
  (accounts, chats, messages, unread, search, calendar), navigation (window, chats, channels, calendar, settings,
  any registered action), writes (send, edit, delete, react, mark read, new chat, own status), calls (start, answer,
  decline, join, hang up, mute, camera) and events.
- **Events are central**: `ngteamsctl events --follow` and `wait` stream what happens (messages, reactions, typing,
  unread, presence, own status, outbox, calls, meetings starting, connection, notifications, UI), so scripts can react
  to Teams instead of polling it.
- Purpose: the user's own scripting on the installed build, such as status-bar unread and presence, launcher "open chat",
  call controls on global keys, notifications piped elsewhere, and scripted sends.
- **No gates**: it's a feature of the user's Teams client, like a keyboard shortcut. Every command works on the
  installed build without opt-in settings or confirmations. Same-UID trust: the socket is `0600` in a `0700` dir.
- A second `ngteams` launch, or a Teams meeting link, hands off to the running instance over the same socket instead
  of failing with "already running".
- It fits the existing design: the server is one more caller of `teams_ui::Core`, `Desktop`/`Shell`, the settings and
  `teams_ui::calls::ask`. Nothing in it builds a request, the outbox stays the only write path, and the store is
  opened only by the engine.

## Context (from discovery and plan review)
- `crates/ngteams/src/app.rs`: `Launch` (what the app starts with), `launch` → `Launched { window, desktop }`, and the
  private `Running` (`(mri, upn, Entity<Bridge>)` per account, `Desktop`, settings, runner) held in the `Held` global.
  `Launch` is built in `src/main.rs` (×2), `tests/app.rs`, `tests/support/app.rs`, `tests/window.rs`,
  `tests/accounts_window.rs`, `tests/call_window.rs` and `tests/ui_tour.rs`.
- `crates/ngteams/src/main.rs`: `Cli::parse`, the refusals, then the `run(&cli, Reach::Desktop)` branch for the
  personal build. The engine lock is taken later (`start_accounts` → `Runner::start`).
- `crates/teams-ui/src/bridge.rs`: the `Core` trait (`conversations`, `messages`, `members`, `search`, `calendar`,
  `calendar_event`, `reactions`, `write(Action)`, `resend`, `sync`, …) and `drain` (bridge.rs ~1008), where engine
  `Event`s (not `Clone`) reach `models.apply`. `Bridge::sign_out` → `Models::signed_out`.
- `crates/teams-ui/src/models.rs`: `feed()` treats a message as new only for `MessageChange::New` +
  `Applied::Inserted`. `crates/teams-ui/src/notify.rs` holds catch-up and replay messages back by a cutoff.
- `crates/teams-ui/src/tray.rs`: each account's `Tray` keeps the unread counts the badge shows (a per-chat `counted`
  map, `total()`, recounted through `Core::unread_count`). It's reachable as `Accounts::tile(key)`.
- `crates/teams-ui/src/desktop.rs`: `show`, `open_window`, `switch(key)`, `open_chat_of`, `open_channel_of`,
  `set_in_tray`, `in_tray`, `own_presence()` (first account only) and the private `in_shell`. `closed` quits the app
  when there's no tray and no call window.
- `crates/teams-ui/src/shell.rs`: `show_calendar`, `open_settings_page`, `focus_search`, `show_search_results`,
  `open_prejoin` and `open_channel_id`. `choose_status` is the status menu's path: `own.choose(status.choice())` plus
  `settings.set_manual_status(account, status)`. `OwnPresence::settings_changed` re-applies the saved manual status, so
  both halves are needed.
- `ManualStatus` (settings.rs) is `Automatic | Available | Busy | DoNotDisturb | Away`. There is no BRB, no Appear
  offline and no duration.
- `crates/teams-ui/src/calls.rs`:
  - `calls::ask(calls, core, trace, Ask, cx)` returns `()`. It does nothing when `!shows()`.
  - Local refusals land in `Calls::refused()`; the engine's answer comes later.
  - `Ask` covers `Call`, `Join`, `JoinMeeting`, `Answer`, `Decline`, `HangUp`, `ToggleMute` and `ToggleCamera`.
  - `ActiveCall.muted` and `.camera` hold the current state.
  - `CallFloor` holds one call across the accounts.
- `crates/teams-ui/src/rich_text.rs`: `RichText::of_message(msg, me)` and `mentions_me()`.
- `crates/teams-ui/src/compose.rs`: `message_of(doc, quotes)` builds `MessageText` from the editor's `Doc`.
- `crates/teams-proto/src/calendar.rs`: `join_url_chat` / `join_url_meeting` already parse meeting join links. No chat
  or channel link shape is documented.
- `crates/ui-sandbox/src/lib.rs`: a sandboxed app's environment is cleared to `PASSED_ENV`, and its `XDG_RUNTIME_DIR`
  is the sandbox's own temp dir. So lab and read-only sockets must be handed an explicit path by their runners
  (`scripts/run-lab.sh` / `ui-sandbox-run`, `scripts/run-personal-read-only.sh`).
- Vendored GPUI: `App::build_action`, `App::all_action_names` and `Window::dispatch_action` (deferred). The app's
  actions are named `teams_ui::…`.
- `scripts/install-personal.sh:44`: its `sed` rewrites only a bare `Exec=ngteams` line of the `.desktop` entry.
- Workspace deps: `serde`, `serde_json`, `clap` and `async-channel` exist (ngteams doesn't take `async-channel` yet).
  `clap_complete` is not there yet.
- `coverage.toml`: every runtime crate needs a baseline, with a target of 90%. `crates/ngteams/tests/apart.rs`:
  default-members = members minus `crates/ngteams`.

## Development Approach
- **testing approach**: Regular (code first, then tests in the same task)
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
- maintain backward compatibility

## Testing Strategy
- **unit tests**: required for every task.
  - Protocol, codec, client and CLI live in `crates/teams-ctl/tests` (no GPUI). The CLI is tested by running the
    `ngteamsctl` binary (`CARGO_BIN_EXE_ngteamsctl`) against an in-test fake server.
- **server tests**: `crates/ngteams/tests/ctl*.rs`.
  - They follow `tests/app.rs`: `TestAppContext::single()`, the app launched over the mock engine (`teams-mock`)
    with `Launch.ctl = Some(<temp socket>)`, and the test thread pumping (`run_until_parked` + a short sleep).
  - A `teams_ctl::client` on its own thread talks to the socket. This is the round trip through the real protocol
    and server, so no separate sandboxed binary e2e is needed: a `cargo test`-built `ngteams` binary refuses to
    run anyway.
- **runner wiring**: `scripts/test_run_lab.py` (the socket path handed to the lab app).
- No live tenant run is needed for this feature; no `cargo test` process makes live writes.

## Progress Tracking
- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview
- **Transport**: a Unix socket speaking JSON lines. Each request is one line `{"id":N,"cmd":…,"account":…?,…}` and
  each response one line `{"id":N,"ok":…}` or `{"id":N,"error":{"code":…,"message":…}}`. A `subscribe` turns the
  connection into an event stream of `{"event":…}` lines. The first exchange is `hello` (protocol version and app
  version), so an old `ngteamsctl` against a newer app fails clearly.
- **Socket paths** (`teams_ctl::socket_path(Mode)`, which is also what the app and runners use):
  - personal: `$XDG_RUNTIME_DIR/ngteams/ctl.sock`;
  - lab alias: `ctl.sock` in that alias's runner state dir;
  - read-only: `ctl.sock` in the read-only runner's state dir;
  - `--socket <path>` / `$NGTEAMS_CTL_SOCKET` override any of them.

  The app takes the path it serves on from `--ctl-socket <path>`, defaulting to the personal path. The runners pass
  it, because the sandbox clears the environment and has its own runtime dir. In the read-only mode, writes are
  refused by its transport, and ctl write commands return that refusal.
- **Server**: started inside `app::launch` from `Launch.ctl: Option<PathBuf>` (`None` turns it off), holding a
  `WeakEntity<Running>`.
  - A listener thread accepts connections, and a thread per connection reads lines.
  - Requests cross to GPUI's foreground through an `async_channel`, where one `cx.spawn` loop executes them. Replies go
    back through a `oneshot`. `Core` `Reply`s are awaited on the foreground executor as the views await them.
- **Two layers**:
  - *engine commands*:
    - Reads go through the addressed account's `Bridge::core()`.
    - Writes go through `Core::write(Action)`, so the outbox applies its retries and unknown-outcome handling as for
      the UI.
    - Own status takes the status menu's path: save the manual status in the settings, plus `choose` on that
      account's `OwnPresence`.
    - Calls go through `teams_ui::calls::ask`, the call buttons' path, so the busy rule, the call window and the devices
      behave as when clicked.
  - *UI commands*: through `Desktop`/`Shell` (new per-account public wrappers where `in_shell` is private today), plus
    `action <name>`, which dispatches a registered `teams_ui::` GPUI action by name to the main window. This reaches
    every button that has an action without a command per button.
- **Account addressing**: `--account <upn|mri|index>`.
  - By default, the account shown in the window.
  - Call commands default to the account that is ringing or in the call (`CallFloor`).
  - Chats are addressed by id, or by title derived as the chat list derives it (topic, otherwise the other members'
    names). An ambiguous title is an error listing the candidates.
- **Events**: one `CtlEvent` stream, tagged with the account, fed from four sources:
  - a **generic tap on `Bridge`** (`Bridge::set_tap(Rc<dyn Fn(&Event, &mut App)>)`, run in `drain` before
    `models.apply`). `teams-ui` stays free of the wire protocol; the mapping to `CtlEvent` lives in
    `crates/ngteams/src/ctl/events.rs`.
  - **entity observers**: each account's `Tray` (unread), `OwnPresence` (own status), `Models` status (signed out),
    `Desktop` and the shell (UI, call window), and the notifier (notifications shown).
  - **the app**: accounts added and removed (`Running`).
  - **a calendar ticker** for meetings about to start, running only while a subscriber asks for that kind.

  A slow subscriber gets a final `lagged` line and is dropped, never waited on. With no subscriber the sources skip
  building events.
- **Hand-off**: on the personal build's desktop branch of `main`, after parsing and before any lock, `ngteams` with
  no flags (or with a link argument) connects to the personal socket with short connect and read timeouts. If an
  instance answers, it sends `activate` (or `open-link <url>`) and exits 0. `--forget` and `--account` keep today's
  behaviour. When the server binds, a stale socket file with nobody behind it is removed and replaced.
- **Crate split**: a new `crates/teams-ctl` (default member) holds the protocol types, socket paths, line codec,
  client and the `ngteamsctl` binary. It has no GPUI and doesn't parse links. The server lives in
  `crates/ngteams/src/ctl/`.

## Technical Details
- `teams_ctl::Request`: a `#[serde(tag = "cmd")]` enum.
  - Reads and UI: `Hello`, `Status`, `Accounts`, `Chats { unread_only, limit }`, `Messages { chat, limit, before }`,
    `Unread`, `Search { query, limit }`, `Members { chat }`, `Window { op }`, `Open { target }` (chat, channel,
    calendar, settings page, search), `SwitchAccount { to }`, `UiState`, `Action { name }`, `ActionList`,
    `Subscribe { filter }`, `Activate`.
  - Writes: `Send { chat, text }`, `Edit`, `Delete`, `React`, `Unreact`, `MarkRead`, `NewChat { people }`, `Resend`,
    `GetStatus`, `SetStatus { status }`, `Sync`.
  - Calls and calendar: `Calls`, `Call { chat }`, `Answer`, `Decline`, `HangUp`, `Mute { op }`, `Camera { op }`,
    `Join { chat }`, `Calendar { from, to }`, `CalendarEvent { event }` (not `id`, which the
    line's own request id already names), `JoinMeeting { event, now }`.
  - Links: `OpenLink { url }`.
- `teams_ctl::Response` payloads are plain serde structs (`ChatRow`, `MessageRow` with plain text, `AccountRow`,
  `UiState`, `OutboxRow`, `CallRow`, `EventRow`, …). They are mapped from the store and engine types in the server,
  never serializing internal types, so the wire format doesn't follow internal renames.
- Error codes: `unknown_account`, `unknown_chat`, `ambiguous_chat`, `busy`, `no_calling`, `no_call`, `not_ringing`,
  `no_tray`, `no_window`, `unknown_action`, `refused_read_only`, `stopped`, `timeout`, `bad_request`,
  `version_mismatch`, `engine` (with the engine error's message).
- **Own status values**: `available | busy | dnd | away | reset`. `reset` means `ManualStatus::Automatic`, so the
  app's idle policy decides again. These are exactly the menu's choices.
- **Event catalogue**: `{"event":"<kind>","account":"<upn>","at":<ms>,…}`. Each kind carries only what its source
  holds:
  - *messages*:
    - `message`: only `MessageChange::New` + `Applied::Inserted`, never `Source::History`; catch-up and replay
      messages older than the notifier's cutoff are skipped. It carries chat, message id, sender, plain text,
      `from_me`, `mentions_me` (`RichText::mentions_me`) and `source` (push, catch-up, replay).
    - `message_edited`, `message_deleted`: chat and message id.
    - `reaction`: chat and message id, as the engine tells only that the reactions changed. With `--detail`, the
      server adds the current set read through `Core::reactions`.
    - `typing`: chat, who, on or off.
    - `read`: the account's read horizon in a chat moved.
  - *chats and teams*: `chat_changed` (chat id), `teams_changed`.
  - *unread*: `unread` (a chat's count and the account's total from its `Tray`, sent only when either changes).
  - *people*: `presence` (MRI, availability), `own_status` (the account's reported status changed, from the UI, ctl or
    the idle policy).
  - *outbox*: `outbox` (row id, write kind, chat, state: queued, sent, failed, unknown).
  - *calls*:
    - `incoming_call`: call id, caller, chat.
    - `ring_ended`: call id and how it ended.
    - `call`: call id and the `CallStatus` state, with the end reason.
    - `call_participants`: joined and left, from a diff against the previous view held in `models.calls`, taken
      before the apply.
    - `lobby`: the people waiting.
    - `voicemail`.
    - `call_window`: opened or closed.
  - *calendar*: `meeting_starting` (event id, subject, start, minutes to start, whether it has a join). It fires at the
    subscriber's `--lead` (default 5 and 0 minutes).
  - *app*:
    - `connection`: the push channel connected, disconnected or stopped.
    - `caught_up` / `catch_up_failed`.
    - `engine_failed`: what failed and the error's message.
    - `signed_out`, `account_added`, `account_removed`.
    - `notification`: a desktop notification the app showed, with its title and the chat it opens.
    - `ui`: the shown account, view, chat or window visibility changed.
  - Filters on `subscribe`: `kind` (several, with `message*`-style prefixes), `account`, `chat`, `from`,
    `mentions_me`.
- `ngteamsctl` exits 0 on success, 1 on an error response, 2 when no app is running or on a usage error, and 3 when
  `wait` times out. Output is human-readable by default and `--json` gives the raw responses.
- **Call commands' replies**:
  - `!shows()` gives `no_calling`.
  - After `ask`, a local refusal in `Calls::refused()` maps to `busy`, `no_call`, `not_ringing` or `no_video`.
  - Otherwise the server waits, with a bound, for the next `Calls` change, which is the engine's answer. That gives
    `ok` with the call row, an `engine` error, or `timeout`.
  - `mute`/`camera on|off` read `ActiveCall.muted`/`.camera` and ask `ToggleMute`/`ToggleCamera` only when they differ.
- **Sending**: `text` is plain text with line breaks. It is turned into the editor's `Doc` (paragraphs) and goes
  through `compose::message_of(&doc, &[])`, so sanitizing and length limits stay where they are. Markdown and mentions
  are follow-ups. `send --wait` follows the outbox row to its final state. Without it, the reply is the row's state as
  its first send left it.

## What Goes Where
- **Implementation Steps** (`[ ]` checkboxes): code, tests, docs in this repo.
- **Post-Completion** (no checkboxes): the user's own scripts, status-bar config, key bindings, installing, captures
  for later link shapes.

## Implementation Steps

### Task 1: `teams-ctl` crate: protocol, socket paths, codec, client

**Files:**
- Create: `crates/teams-ctl/Cargo.toml`, `crates/teams-ctl/src/lib.rs`, `src/protocol.rs`, `src/path.rs`,
  `src/codec.rs`, `src/client.rs`
- Create: `crates/teams-ctl/tests/protocol.rs`, `tests/path.rs`, `tests/codec.rs`, `tests/client.rs`
- Modify: `Cargo.toml` (members, default-members), `coverage.toml` (baseline)

- [x] create the crate (`[lints] workspace = true`, workspace deps only)
- [x] `Request`/`Response`/`ErrorCode`/`CtlEvent`/`EventFilter` types with serde, and `PROTOCOL_VERSION`
- [x] `socket_path(Mode)` for personal, lab alias and read-only, the `NGTEAMS_CTL_SOCKET` override, and `prepare_dir`
      (`0700`)
- [x] JSON-lines codec (one line, a size cap, id pairing) and the client (connect with timeouts, `hello`,
      request/response, a streaming `subscribe`)
- [x] tests: round trip of every request, response and event kind, unknown `cmd` → `bad_request`, path per mode and
      override, oversize line, a client against an in-test server, connect timeout
- [x] run `cargo test -p teams-ctl`, which must pass before the next task

### Task 2: the server in the app: `Launch.ctl`, listener, foreground dispatch, `hello`/`status`

**Files:**
- Create: `crates/ngteams/src/ctl/mod.rs`, `ctl/listener.rs`, `ctl/dispatch.rs`
- Modify: `crates/ngteams/src/app.rs` (`Launch.ctl`, start in `launch`, keep `Launch.trace` in `Running`, stop on
  quit), `crates/ngteams/src/cli.rs` (`--ctl-socket`), `crates/ngteams/src/main.rs`, `crates/ngteams/Cargo.toml`
  (`teams-ctl`, `async-channel`)
- Modify: the `Launch {` literals in `tests/app.rs`, `tests/support/app.rs`, `tests/window.rs`,
  `tests/accounts_window.rs`, `tests/call_window.rs`, `tests/ui_tour.rs`
- Create: `crates/ngteams/tests/ctl.rs`

- [x] listener thread: prepare the dir and bind `0600`. If the path exists and a connect succeeds, refuse ("already
      running"). If the connect is refused, remove the stale file and bind.
- [x] a thread per connection, requests to the foreground through `async_channel`, replies through a `oneshot`. A
      malformed line gets `bad_request`.
- [x] foreground dispatch over `WeakEntity<Running>`: `hello`, and `status` (accounts with their engine state,
      the window shown or in the tray, the call up)
- [x] remove the socket file on quit
- [x] tests (client thread + pumping test thread): hello and version mismatch, status, stale socket replaced, a
      second server refused, two clients at once, quit closes connections and removes the file
- [x] run `cargo test -p ngteams --test ctl`, which must pass before the next task

### Task 3: lab and read-only sockets through their runners

**Files:**
- Modify: `crates/ui-sandbox/src/bin/ui-sandbox-run.rs` (or the place it builds the app's args),
  `scripts/run-lab.sh`, `scripts/run-personal-read-only.sh`, `crates/ngteams/src/main.rs` (lab and read-only take
  `--ctl-socket`)
- Modify: `scripts/test_run_lab.py`, `crates/ngteams/tests/lab_mode.rs` / `tests/read_only.rs`

- [x] the runners pass `--ctl-socket <state dir>/ctl.sock`, the same path `teams_ctl::socket_path(Lab(alias))` /
      `socket_path(ReadOnly)` resolves
- [x] lab and read-only apps serve on the path given and never on the personal path
- [x] tests: the path the lab runner passes (`test_run_lab.py`), the read-only and lab modes binding where told
- [x] run `python3 -B -m unittest discover -s scripts -p test_run_lab.py` and the touched ngteams tests, which must
      pass before the next task

### Task 4: account addressing and read commands

**Files:**
- Create: `crates/ngteams/src/ctl/reads.rs`, `ctl/address.rs`
- Modify: `crates/ngteams/src/ctl/dispatch.rs`, `crates/teams-ui/src/tray.rs` (a per-chat unread accessor)
- Create: `crates/ngteams/tests/ctl_reads.rs`
- ➕ Create: `crates/ngteams/tests/ctl_support/mod.rs`, the server tests' shared harness (launch over a seeded store,
  pumped client, `ask`); `tests/ctl.rs` moved onto it

- [x] `--account` by UPN, MRI or index, defaulting to the shown account; `unknown_account` otherwise
- [x] chat addressing by id or by title derived as `chat_list` derives it (topic, otherwise the other members' names
      via `Core::members`/`person`), with `ambiguous_chat` listing the candidates
- [x] `accounts`, `chats [--unread] [--limit]`, `messages <chat> [--limit] [--before]`, `search <q>`, `members <chat>`
      through `Core`, mapped to wire rows
- [x] `unread` (per chat and total) from the account's `Tray`, the badge's own counts
- [x] tests: each command over seeded mock data, a 1:1 chat found by the other person's name, addressing errors, an
      engine that stopped → `stopped`
- [x] run `cargo test -p ngteams --test ctl_reads`, which must pass before the next task

### Task 5: events: the bridge tap, engine kinds and subscriptions

**Files:**
- Modify: `crates/teams-ui/src/bridge.rs` (`Bridge::set_tap`, run in `drain` before `models.apply`)
- Create: `crates/ngteams/src/ctl/events.rs` (the hub, `Event` → `CtlEvent`)
- Modify: `crates/ngteams/src/ctl/dispatch.rs` (`subscribe`), `crates/ngteams/src/app.rs` (set the tap per bridge,
  for accounts added later too)
- Modify: `crates/teams-ui/tests/bridge.rs`; create `crates/ngteams/tests/ctl_events.rs`
- ➕ Modify: `crates/ngteams/src/ctl/listener.rs` (a `subscribe`'s answer carries its stream, which the connection's
  thread writes), `crates/ngteams/tests/ctl_support/mod.rs` (more accounts, each store seeded). `Bridge::tell` (an
  event handled as the drain handles it) and `app::bridges` let the tests stand in for an account's engine.

- [x] `Bridge::set_tap`: an `Rc<RefCell<Option<…>>>` the drain closure captures, called with `&Event` before the
      apply; `Bridge::start`'s callers unchanged
- [x] the hub: a bounded broadcast; a subscriber that falls behind gets `lagged` and is dropped; with no subscriber
      the tap returns at once
- [x] the engine kinds of the catalogue, from the tap: message (New + Inserted only, no History, the notifier's
      catch-up cutoff, `source`, `from_me`, `mentions_me`), edited, deleted, reaction, typing, read, chat_changed,
      teams_changed, presence, outbox, incoming_call, ring_ended, call, call_participants (diff against
      `models.calls` before the apply), lobby, voicemail, connection, caught_up, catch_up_failed, engine_failed
- [x] `subscribe { filter }` with the kind / account / chat / from / mentions_me filters, and `--detail` for
      reactions
- [x] tests: mapping per kind, a replayed or history message not sent, `mentions_me` and `from_me`, a slow subscriber
      dropped with `lagged` while the drain keeps going, each filter, two subscribers with different filters, the
      tap of an account added after start
- [x] run `cargo test -p teams-ui --test bridge` and `cargo test -p ngteams --test ctl_events`, which must pass
      before the next task

### Task 6: events: unread, own status, UI, app and calendar kinds

**Files:**
- Modify: `crates/ngteams/src/ctl/events.rs` (observers), `crates/ngteams/src/app.rs` (account_added /
  account_removed)
- Modify: `crates/teams-ui/src/notify.rs` (tell an observer about a notification shown, if there's no reachable
  record of it yet), `crates/teams-ui/src/desktop.rs` (a `ui_state()` read and call-window open/closed)
- Create: `crates/ngteams/src/ctl/ticker.rs`
- Modify: `crates/ngteams/tests/ctl_events.rs`
- ➕ Create: `crates/ngteams/src/ctl/watch.rs` (the observers, beside `events.rs`'s hub and tap). Modify:
  `crates/teams-ui/src/tray.rs` (observers told when a chat's count changes, not only the total),
  `crates/teams-ui/src/call_window.rs` (observers told when the call's window opens or closes),
  `crates/teams-core/src/calendar.rs` (`instant` public, for an event's start), `crates/teams-ui/tests/{notify,desktop}.rs`,
  `crates/ngteams/tests/support/app.rs` (account added / removed)

- [x] `unread`: observe each account's `Tray`, sent when a chat's count or the total changes
- [x] `own_status`: observe each account's `OwnPresence`, comparing what it reports
- [x] `signed_out` from `Models` status; `notification`, `ui`, `call_window`, `account_added`, `account_removed`
      from where they change
- [x] `meeting_starting`: a per-account ticker over the stored calendar (`Core::calendar` refreshed every few
      minutes), skipping cancelled and all-day events. It runs only while a subscriber asks for the kind and fires
      once per event per lead. (One ticker task reads every account in turn, each refreshed on its own schedule.)
- [x] tests: unread up, down and total; own status from the menu and from the idle policy; a notification shown;
      navigation → `ui`; sign-out; the ticker firing once at each lead (advanced clock, `teams-mock` calendar) and not
      at all without a subscriber (the ticker's test uses a core of its own answering the calendar: the harness's
      engine has no token, so its calendar read would sign the account out)
- [x] run `cargo test -p ngteams --test ctl_events`, which must pass before the next task

### Task 7: UI commands and action dispatch

**Files:**
- Create: `crates/ngteams/src/ctl/ui.rs`
- Modify: `crates/teams-ui/src/desktop.rs` (per-account public wrappers over `in_shell` for calendar, settings and
  search), `crates/ngteams/src/ctl/dispatch.rs`
- Create: `crates/ngteams/tests/ctl_ui.rs`
- ➕ Modify: `crates/teams-ui/src/shell.rs` (`Shell::search_for`: a query typed and entered), `crates/ngteams/src/ctl/
  address.rs` (`address::known`: an account named whether or not its engine runs, for the UI commands), and
  `crates/ngteams/src/ctl/mod.rs`. `activate` is served too, as `window show`.

- [x] `window show|hide|toggle|focus`. `hide` sends the window to the tray, and is refused with `no_tray` when
      there's no tray item, since closing would quit the app.
- [x] `open chat|channel <id|title>`, `open calendar`, `open settings [page]`, `open search <q>`,
      `account switch <account>`
- [x] `ui state`: shown account, current view (chat/channel/calendar/settings/teams/search), open chat id and title,
      window shown, call window open
- [x] `action list` (the `teams_ui::` actions) and `action <name>` (full or unique short name) dispatched to the main
      window. The reply says it was dispatched (GPUI defers it). `no_window` while the window is closed, `unknown_action`
      for an unknown name.
- [x] tests: each open moves `ui state`, hide and show with a tray, hide refused without one, an unknown action, a
      short name, `ShowCalendar` changes the view
- [x] run `cargo test -p ngteams --test ctl_ui`, which must pass before the next task

### Task 8: the `ngteamsctl` binary: reads, UI and events

**Files:**
- Create: `crates/teams-ctl/src/bin/ngteamsctl.rs`, `src/cli.rs`, `src/render.rs`
- Create: `crates/teams-ctl/tests/cli.rs` (the binary against an in-test fake server on a temp socket)
- ➕ Create: `crates/teams-ctl/tests/render.rs` (the human output of every answer and event kind, including the
  answers of Tasks 9–12, which `render` already covers)

- [x] clap subcommands for Tasks 2–7, `--account`, `--lab <alias>`, `--read-only`, `--socket`, `--json`
- [x] human output (tables for chats and messages, one line per event) and the exit codes
- [x] `events [--follow] [--kind …] [--account …] [--chat …] [--from …] [--mentions] [--detail] [--lead <min>…]`,
      `events kinds`. Without `--follow`, `events` prints the first event and exits; `--account` is the global one.
- [x] `wait <kind> [same filters] [--match <regex>] [--timeout]`: prints the first matching event and exits 0
      (`--match` is tried on the event's JSON line; `--timeout` in seconds, fractions allowed)
- [x] tests: each subcommand against the fake server, `--json` passthrough, `--lab`/`--read-only` socket choice, no
      app → exit 2, `wait` timeout → exit 3, `wait` matching
- [x] run `cargo test -p teams-ctl`, which must pass before the next task

### Task 9: message writes

**Files:**
- Create: `crates/ngteams/src/ctl/writes.rs`
- Modify: `crates/teams-ui/src/compose.rs` (a `Doc` from plain text, if the editor has no such constructor),
  `crates/ngteams/src/ctl/dispatch.rs`, `crates/teams-ctl/src/cli.rs`
- Create: `crates/ngteams/tests/ctl_writes.rs`; modify `crates/teams-ui/tests/compose.rs`
- ➕ Modify: `crates/teams-ctl/src/protocol.rs` (each write request's `wait` flag and `WRITE_WAIT`),
  `crates/ngteams/src/ctl/address.rs` (the account's `Chats`, where a wait watches its row)

- [x] plain text (with line breaks) → `Doc` → `compose::message_of(&doc, &[])` (`compose::plain_doc`; an edit keeps
      the message's quote through `compose::quotes_of`)
- [x] `send <chat> <text|->` (stdin with `-`), `edit <chat> <msg> <text>`, `delete <chat> <msg>`,
      `react|unreact <chat> <msg> <reaction>`, `read <chat>` (up to the latest message), `new-chat <people…>`, all
      through `Core::write(Action)`
- [x] `--wait` follows the outbox row to its final state; `resend <row>` through `Core::resend`
- [x] tests: each write produces the right `Action` and outbox row on the mock engine, read-only →
      `refused_read_only`, too long text → the compose error, empty text refused, line breaks kept
- [x] run `cargo test -p ngteams --test ctl_writes` and `cargo test -p teams-ui --test compose`, which must pass
      before the next task

### Task 10: own status and sync

**Files:**
- Modify: `crates/ngteams/src/ctl/writes.rs`, `crates/teams-ui/src/desktop.rs` (`own_presence_of(key)`),
  `crates/teams-ctl/src/cli.rs`
- Modify: `crates/ngteams/tests/ctl_writes.rs`
- ➕ Modify: `crates/ngteams/src/ctl/dispatch.rs`, `crates/teams-ctl/src/protocol.rs` (`SYNC_ANSWER`, the app's bound
  on a `sync`), `crates/teams-ctl/tests/cli.rs`, `crates/teams-ui/tests/desktop.rs`

- [x] `status get` (the reported status and the manual choice) and `status set available|busy|dnd|away|reset`, done
      as `Shell::choose_status` does: `set_manual_status` through `SettingsModel::change` plus `choose` on the
      addressed account's `OwnPresence`
- [x] `sync` through `Core::sync`, printing the catch-up report
- [x] tests: each status, `reset` back to automatic, a ctl-set status surviving an unrelated settings change, the
      second account's status set without touching the first, the `sync` report
- [x] run `cargo test -p ngteams --test ctl_writes`, which must pass before the next task

### Task 11: calls

**Files:**
- Create: `crates/ngteams/src/ctl/calls.rs`
- Modify: `crates/ngteams/src/ctl/dispatch.rs`, `crates/teams-ctl/src/cli.rs`
- Create: `crates/ngteams/tests/ctl_calls.rs`

- [ ] `calls` (the ringing invitation and the call up: id, account, chat, state, muted, camera)
- [ ] `call <chat>`, `answer`, `decline`, `hangup`, `join <chat>` through `teams_ui::calls::ask`, defaulting to the
      account that is ringing or in the call, with replies as in Technical Details (`no_calling`, the local
      refusals, a bounded wait for the engine's answer)
- [ ] `mute on|off|toggle` and `camera on|off|toggle` through `Ask::ToggleMute` / `Ask::ToggleCamera`, acting only
      when the state differs
- [ ] tests over the mock engine with calling: answer and decline an incoming call, start and hang up, busy, no
      calling, not ringing, mute and camera on/off idempotent, the call window closed while the call runs
- [ ] run `cargo test -p ngteams --test ctl_calls`, which must pass before the next task

### Task 12: calendar and joining meetings

**Files:**
- Create: `crates/ngteams/src/ctl/calendar.rs`
- Modify: `crates/ngteams/src/ctl/dispatch.rs`, `crates/teams-ctl/src/cli.rs`
- Create: `crates/ngteams/tests/ctl_calendar.rs`

- [ ] `calendar [--today | --from <date> --to <date>]` and `calendar show <id>` through `Core::calendar` /
      `calendar_event`
- [ ] `join meeting <event id>`: opens the prejoin as the calendar's Join does. `--now` joins through
      `Ask::JoinMeeting` with the saved devices, the join chat and meeting taken from the event.
- [ ] tests: listing a range, an event's details, join through the prejoin and `--now`, an event without a meeting
- [ ] run `cargo test -p ngteams --test ctl_calendar`, which must pass before the next task

### Task 13: hand-off of a second launch and of meeting links

**Files:**
- Create: `crates/ngteams/src/handoff.rs`, `crates/ngteams/src/ctl/links.rs`
- Modify: `crates/ngteams/src/main.rs`, `crates/ngteams/src/cli.rs` (an optional positional `URL`),
  `packaging/com.nickalie.ngteams.desktop` (`Exec=ngteams %u`, `MimeType=x-scheme-handler/msteams;`),
  `scripts/install-personal.sh` (its `Exec` rewrite matches the new line; `update-desktop-database` if present)
- Create: `crates/ngteams/tests/handoff.rs`

- [ ] in `main`, on the personal desktop branch only, after parsing and before any lock: connect to the personal
      socket with connect and read timeouts. If an instance answers, send `activate` or `open-link <url>` and exit 0.
      `--forget` and `--account` keep today's behaviour.
- [ ] `open-link` in the server: meeting join links (`msteams:` and `https://teams.microsoft.com/l/meetup-join/…`)
      parsed with `teams_proto::calendar::join_url_chat` / `join_url_meeting` and opened in the prejoin. Any other link
      shows the window and is logged as unrecognised (only its shape, no payload).
- [ ] tests: second launch hands off and exits 0, a hung instance times out and the second launch says so, a stale
      socket → normal start, `--forget` not handed off, a meetup-join link in both forms, an unknown link
- [ ] run `cargo test -p ngteams --test handoff`, which must pass before the next task

### Task 14: installing `ngteamsctl` and shell completions

**Files:**
- Modify: `Cargo.toml` (`clap_complete` in `[workspace.dependencies]`), `crates/teams-ctl/Cargo.toml`,
  `crates/teams-ctl/src/cli.rs`, `scripts/install-personal.sh`
- Modify: `crates/teams-ctl/tests/cli.rs`

- [ ] `ngteamsctl completions bash|fish|zsh` (`clap_complete`, checked against the log cap)
- [ ] the installer builds `ngteamsctl` and installs it beside `ngteams`, linked from `~/.local/bin/`
- [ ] tests: completions output per shell names the subcommands
- [ ] run `cargo test -p teams-ctl`, which must pass before the next task

### Task 15: Verify acceptance criteria
- [ ] every command in Overview works on the personal build's code path with no setting or prompt in the way
- [ ] the outbox is the only write path, no ctl code builds a request, calls go through `calls::ask`, and status goes
      through the settings and `OwnPresence`
- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `scripts/lint-report.sh`
- [ ] `cargo test -p teams-ctl`, `cargo test -p ngteams`, `cargo test -p teams-ui`, `cargo test -p fixture-guard`,
      `python3 -B -m unittest discover -s scripts -p test_run_lab.py`
- [ ] `scripts/coverage.sh --require-target` for `teams-ctl`, `ngteams` and `teams-ui`, with the new baseline in
      `coverage.toml`

### Task 16: [Final] Update documentation
- [ ] README: `teams-ctl` in Layout, and an `ngteamsctl` section under `ngteams` covering the sockets, commands,
      `--json`, the event catalogue, exit codes and hand-off
- [ ] `docs/personal-build.md`: ngteamsctl on the installed build. In "What agents may do": agents use `ngteamsctl`
      only against lab aliases (`--lab`, within the lab rule) and the read-only runner's socket (`--read-only`), never
      against the installed instance's personal socket.
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion
*Items requiring manual intervention or external systems - no checkboxes, informational only*

**Manual verification**:
- the user installs with `scripts/install-personal.sh` (on request) and tries it on their own account: status-bar
  unread and presence, `open chat` from a launcher, call keys, a meeting link clicked in the browser
- a second launch from the desktop entry focuses the running window

**Possible follow-ups**:
- chat and channel links: capture the shapes the web client produces with `proto-lab` (with the lab-contact check),
  document them in `docs/protocol/`, then route them in `open-link`
- `send --markdown` and mentions (`@name` resolved against the chat's members)
- an `ngteams` skill for agents, like the `ngterm` skill for `ngtermctl`
