# ms-teams

A native Microsoft Teams client for Linux in Rust + GPUI, written against Teams' internal APIs with the web client as
the reference. The work in progress is M3, calls (`docs/plans/20261024-native-teams-m3-calls.md`); finished plans
are in `docs/plans/completed/`. Commands and layout are in [README.md](README.md).

## Hard rules

These hold for every agent and every change, with no exceptions. **Lab accounts contact only each other; everything
else is allowed.** The lab browser's fence and the native client (its HTTP transport and its Trouter frames) follow
this rule. The rules about writes, calls and the browser fence govern the lab and every lab binary; the personal build
(`teams`) has a fence of its own and the two rules about it below. The rules on the native client's sealed types,
captures and tokens, agents and protocol notes hold for both builds.

- **A contacting write goes only to the lab.** A contacting write is one that reaches other people: a message (a
  post, reply, edit, delete, forward, channel post, a scheduled or drafted send), a reaction, a call (starting,
  joining or answering one), a chat created, a member added or removed, an invite, a meeting, a mail, a share. It is
  allowed only when every participant of its target, read fresh and complete (no paging key), is one of the 3 lab
  accounts (`lab.toml`'s MRIs) or an approved service participant (the echo service, by its MRI). A target whose
  members can't be read in full, or a write the fence can't tie to a target with certainty, is denied.
- **Everything else is allowed**, in the lab browser and in the native client, with no capture or review needed:
  reads by any method (a `POST` search or people lookup too, whoever its results list), the account's own session,
  tokens and settings (out-of-office with automatic replies included), presence, a status note, typing and read
  receipts (others may see these; that is fine).
- **When unsure whether a request reaches people, treat it as contacting.** On a service whose purpose is reaching
  people (the chat service, calling, Graph, Outlook, SharePoint, AMS, the middle tier's team, meeting and invite
  paths, CSA), a mutating request is contacting unless it is known not to be (typing, read receipts, the account's
  own feed, settings and endpoints); one the fence can't read is denied, never let through.
- **No token or credential goes to a third party** (a bot, an app's backend): a bot invoke stays denied. Sign-in and
  token renewal stay in `teams-auth`'s own clients and touch only the account's own credentials.
- **Calls go only to the lab, both ways.** A call may be started when every callee is a lab account or the echo
  service, and answered when its invitation names only lab accounts or the echo service; every other incoming call
  is ignored, never answered (no `acceptance`, no delivery response for it, no scenario click). A write on an
  existing call is allowed only on a call the fence itself permitted a start or an answer for in this run; an `add`
  is judged per invitee. The roster isn't watched. Media (TURN, ICE, DTLS-SRTP) isn't classified: it carries a call
  the signaling fence permitted.
- **Nothing runs live against the tenant before the browser fence is armed**: HTTP interception on every host and
  target, the WebSocket guard, the service worker neutralized.
- **The native client sends a contacting write only as `Authorized<_>`** with a `WritePermit` from `Fence::check`
  for its exact target, logged to `fence.log` by `Fence::check`; a sealed `Mutating` type in `teams-proto` is the
  only way to build one, and the transport refuses any other request `teams_fence::contacting` doesn't call `None`.
  The transport is `https` only, with no proxy and no redirects, to its services' bases, and hands each token only to
  its own service's host.
  A write is never resent by the transport (the HTTP client's own retries stay off: an HTTP/2 retry could post
  twice). A mock origin (HTTP or socket) exists only under a `test-support` feature, which a lab binary refuses to
  run with. No write ever comes from a `cargo test` process: a live test spawns a normally built lab binary (built
  with `--target-dir target/lab`) and reads its output and `fence.log`. This holds for the personal build too; only
  its fence's policy differs.
- **Captures and tokens never enter the repo, not even sanitized.** Captures live under
  `~/.local/share/teams-lab/captures/`. `proto-lab export` only writes a sanitized local reference copy under
  `~/.local/share/teams-lab/reference/` (it refuses any destination inside a git work tree). Committed fixtures are
  written synthetic-from-capture: read the reference copy locally, write a fixture of the same shape with made-up
  values, and mark it synthetic (a top-level `"_synthetic"` note in JSON, on the first line of JSONL, a
  `_synthetic:` first-line header in other text). The fixture guard (`crates/fixture-guard`) must pass on every
  `tests/fixtures/` (top-level, crates', spikes') and `docs/`, and every fixture in them must carry the marker, apart
  from the non-Teams test inputs listed in `fixture_guard::synthetic::TEST_INPUTS`. The sanitizer is
  defence in depth, not permission to commit its output. Tokens never reach logs or stdout: print only aud, exp,
  scopes and tenant. This holds for the personal build too; it makes no captures.
- **Lab binaries get a `WritePermit` only from `teams-fence`'s lab policy**: their fence comes from `Fence::open_lab`
  or `Fence::open_lab_account` (roster from `lab.toml`, the lab's `fence.log`), and `teams-fence`'s `test-support`
  feature (any roster, any log, fake membership sources) is enabled only under `[dev-dependencies]`, never as a
  normal dependency.
- **The personal build (`teams`) has a fence of its own, and only it.** It is the user's own account on the user's
  desktop: the `teams` and `teams-signin-personal` binaries, its data under `~/.local/share/teams/` and
  `~/.config/teams/`, its Secret Service items under `teams-personal`, no broker, no sandbox. Its fence is
  `teams-fence`'s personal policy, behind the `personal` feature only `crates/teams` enables, opened only by
  `Fence::open_personal`. A write goes out only as a sealed `Mutating` type sent as `Authorized<_>` through the outbox,
  with a `WritePermit` from `Fence::check` for its exact target; only the kinds `docs/personal-build.md` lists
  (messages, edits, deletes, reactions, typing, read receipts); only to a 1:1 or group chat whose thread, read fresh
  when the fence decides, unpaged and in a shape a capture showed (every member in a captured MRI form), lists the
  account as a member (never a channel, a meeting chat, a `48:` stream, a new chat, a paged answer or an unknown
  shape), or, the one exception, the account's own availability on its own registered endpoint in a captured value;
  only from an explicit action in the UI or a setting the user turned on (typing, read receipts and presence only
  after the first run's screen is confirmed); logged to its `fence.log` before it goes. Its reads are `GET`s and the
  sealed lists: `OwnState`, and `PostRead` naming only the account or people the account's stored chats list, never
  a search or a suggestion list; it sends no plain request, and logs its Trouter frames. Its sign-in window reaches
  only Entra's sign-in hosts and the identity provider's hosts `account.toml` names, through its egress gate. The
  rules that hold for both builds hold for it; nothing in it changes a lab rule: every lab binary keeps the lab
  fence, refuses the personal data dir, keyring items and accounts, and refuses to run with `personal` or
  `test-support` built in; `teams` refuses the lab's.
- **Agents never produce, install or run a runnable personal binary, and never touch its data.** Agents may compile
  the `teams` crate in the gate and test it on `teams-mock`; any `teams` or `teams-signin-personal` binary they leave
  is a `test-support` one, which refuses to run. They never build either without `test-support` (debug or release, a
  plain `cargo build -p teams` included; the root's `default-members` leave it out), never set the install marker,
  never run `scripts/install-personal.sh` or either binary, and never read or write `~/.local/share/teams/` (its
  diagnostic log included), `~/.config/teams/` or the `teams-personal` items. They never receive the user's tokens,
  chat ids or logs; the user reports a problem by its reason.
- **Protocol notes are written in our own words** in `docs/protocol/`. Microsoft JS is never copied into the repo.

## Patterns

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
  is only an audit: a frame no allow covers is a breach. The frame rule (`fence_ws::classify_frame`): on a Trouter
  socket (any host under `trouter.teams.microsoft.com`) only socket.io 0.9 types `0`, `1`, `2` and `5` pass, so a
  delivery response (`3`), a JSON message, an acknowledgement, a binary frame and anything unparsable are denied; on a
  contacting service's socket (`teams_fence::on_contacting_service`) every frame is denied; anywhere else a frame
  passes. A frame let through is logged `allowed` (method `SEND`).
- **Service workers only through the browser.** Target-level auto-attach filters them out: one attached twice while
  paused answers nothing until it runs. At run start they are unregistered and their caches cleared
  (`Storage.clearDataForOrigin` works only on a page session), and pages bypass them (`Network.setBypassServiceWorker`
  needs `Network.enable` on the same session).
- **Worklets** (the calling stack's audio) have no network API and no `Fetch`: armed with `Resume` only.
- **A target that can't be armed is closed, never resumed.** Every step times out after 10 s, the close too. A held
  target sends nothing until its parent's auto-attach ends or the connection holding it drops (`tests/browser.rs`),
  and `Target.getTargets` doesn't list a held out-of-process iframe, so a close that isn't confirmed is never
  re-checked: the fence `SIGKILL`s the browser's process group while its connection still holds the target, then reaps
  it. A held, never-resumed target closed or killed that way is contained (a warning; a killed browser fails the run).
  A target whose resume was sent, that wasn't held at attach, or that could be neither closed nor killed is a breach.
  Each one is an `ArmingRecord` in `fence.log` and an `arming_failure` in the capture. The kill can't order an EOF
  Chrome sent just before it (`docs/backlog/containment-kill-cannot-order-an-unread-eof.md`, accepted).
- The browser lives and dies with the fence: `ArmedBrowser` owns it, a lost CDP connection kills it, and `close` waits
  for decisions still running (a live membership read) before the run's denies are read. Reporting and NEL are
  disabled by flag (their uploads bypass `Fetch`). The CDP client stays our own: `chromiumoxide` resumes held targets
  by itself.

### The sealed fence API (`crates/teams-fence`, `crates/fence-seal`)

- A lab binary's fence is `Fence::open_lab()` (roster from `lab.toml`, the lab's `fence.log`), or for one account's
  native client `Fence::open_lab_account(alias)`, the same with that account as its own; its membership source
  is `FailClosed` (every fetched scope `Partial(NoSource)`) or `LiveMembershipSource::for_fence(&fence)` (1:1 chats,
  complete only when the members are exactly the two the chat id names; group and meeting chats, any of them, from one
  fresh member-list read, complete only in the shape `parse_members` takes: no paging key, the count equal to the
  members listed, nobody hidden, deleted or listed twice, every MRI `8:orgid:<guid>` or `28:<guid>`;
  `LiveMembershipSource::new` reads the same). The native client's
  member reads take their credential from a `CredentialSource` asked on every read (the session's current `ic3`
  token), renewed once on a 401; without one every write is `Partial(NoCredential)`. `MembershipSource` and `Mutating`
  are sealed: a new source lives in `teams-fence`, a new write type in `teams-proto`, and `fence-seal`'s trybuild
  cases prove nothing else can mint a permit. A binary built with `test-support` (`cargo test`, `--all-targets`,
  `--all-features` unify it in) refuses to run, so live runs use `cargo build -p <crate>`.
- One permit covers one write to one exact `Target` (kind, scope, added participants); `Authorized::new` checks it. A
  request the fence can't read with certainty is `Unknown` and denied: bodies are read strictly (case-folded duplicate
  keys make it `Unknown`), and the echo call start is matched in its captured shape.
- `teams_fence::contacting(&Request, &Policy)` is the classifier of the lab rule, for both sides: `None` (reaches
  nobody), `Write(Target)` (judged by `Fence::check`) or `Unreadable` (denied). Only a mutating request (a method
  other than `GET`/`HEAD`/`OPTIONS`, a body, or a method override) on a contacting service (`Service`, told by host
  and, on a Teams origin, by path) is looked at further; there it is unreadable unless a row of its
  table (`crates/teams-fence/src/contacting.rs`, `contacting/chat.rs`, `contacting/call.rs`) maps or carves it out. A
  row is code plus a case in `crates/teams-fence/tests/contacting.rs` or its fixtures under
  `crates/teams-fence/tests/fixtures/contacting/`. A mutating request let through as `None` is logged as `allowed`
  (`FenceLog::record_allowed`: method, host and `path_shape`, every id replaced by its kind, never a query, header or
  body). The browser fence asks it for every paused request (`proto-lab`'s `Classifier`: a read passes unlogged,
  `None` passes logged, `Write` goes to `Fence::check`, `Unreadable` is denied), and so does the native transport for
  every plain request (`Transport::send`: `None` is sent, logged; `Write` and `Unreadable` are refused before anything
  is sent, `Refusal::Write` and `Refusal::Unreadable`).
- `Fence::check` reads the target's members for every write except a new target (`Scope::New`, a chat or call
  created, judged by its added participants alone), typing and read receipts on a chat, and the account's own
  availability (which keeps its own-account check). Typing and read receipts reach nobody: on a chat
  id `is_chat_id` takes (1:1, group or meeting, the rule `contacting` carves them out by) they are permitted without a
  member read, judged only by the participants they name, and logged like every decision; anywhere else (a channel, a
  chat id in another form) they are judged like any write. So a caller hands it a `Typing` or `ReadReceipt` target
  only for the content-free signal: `contacting` carves typing out only in its captured body (`content` empty or
  absent, no key but `content`, `contenttype`, `messagetype`) and a read receipt only with `name` its one query key.
- Writes on an existing call are denied: the follow-ups aren't mapped, and the native `add` (`AddToCall`, an `Invite`
  on `Scope::Call`) is `Partial`, since no membership source reads a call's members
  (`docs/backlog/fence-call-follow-ups.md`).
- A group or meeting chat is judged like a 1:1 chat, by its fresh member list: a lab-only one is writable, a group
  chat the native client creates included. `chats.toml` is lab data for the scenarios and clients (`abc-chat` records
  the lab's a–b–c chat in it, once); the fence doesn't read it.
- A `PausedRequest` keeps header values only for the method overrides `contacting` reads (`OVERRIDE_HEADERS`), never a
  token or cookie, and a body Chrome didn't inline is classified as an empty one: a row that reads the body takes
  nothing, so such a request is never judged by a guess.

### The native client (`crates/teams-proto`, `teams-trouter`, `teams-core`, `teams-store`)

- **The outbox** (`teams-core/src/outbox.rs`) is the only way a write goes out. A command is a row with a random
  numeric `clientmessageid` kept across attempts; every attempt is a fresh `Fence::check`. After an outcome that isn't
  known (a timeout, a network error, a 5xx other than 503) the chat is synced and searched for the row's
  `clientmessageid` before it goes again with the same id, and every attempt after the first starts with that check;
  only a post can be looked for, so any other write with an unknown outcome is failed, never resent. Writes on one
  message (or on one chat's read horizon) go out in the order asked. Our message coming back is matched to its row by
  `clientmessageid`, never stored twice. **No resend across runs**: each open store is a run holding a lock; rows of
  an earlier run go out only after an explicit `Store::resend`, never a row of a run still open (the web client
  resends a failed send on a later load; this rules that out).
- **Transient writes** (typing, the account's own availability) go through the outbox too, as no row (`Transient`,
  `Outbox::send_transient`): each send is a fresh `Fence::check` with its `fence.log` line, and nothing of it is
  stored, checked for, retried after an unknown outcome or sent by a later run; a throttled one waits in memory once
  and briefly (`MAX_TRANSIENT_WAIT`), the newest of the same takes its place, and a post to its conversation drops it.
  A durable row for `Control/Typing` would never settle (`Control/*` never settles a row) and would go out at a moment
  the user didn't choose.
- **What the native client sends** (`teams-proto`): a read (`Read`: `GET`, no body), a plain request (`Request`, an
  open trait: any method and body, sent by `Transport::send` only when `teams_fence::contacting` calls it `None`,
  logged `allowed` before it goes: the registration, profile lookups and presence reads whoever they name, the
  account's own settings), or a sealed `Mutating` write as `Authorized<_>`; a Trouter frame only as a typed
  `OwnFrame`, through `FrameSink`, which writes nothing to `fence.log` (a frame isn't a contacting write). `OwnFrame`
  is sealed only to keep raw text out: no other crate makes frame text, and a new frame type in `teams-proto` (say
  `user.activity`) is an ordinary change. A request that can't be logged isn't sent. Each service has one base
  (`Service`; a new one is a code change: its base and its token's audience), and every URL is built on it and
  checked against it. The personal build's sealed `OwnState` and `PostRead` lists stay beside them: through a
  personal fence (`Fence::is_personal`) the transport refuses every plain request (`Refusal::Personal`), the
  registration goes as its own-state entry (`Transport::send_own`), a read by `POST` only through `Client::lookup`
  under a `Fence::check_post_read_among` permit against the people of the account's chats (`ChatPeople`, which the
  engine refreshes from its store), and its sink logs each frame (`FrameSink::logging`, `teams_fence::FrameRecord`);
  a lab binary sends their entries as plain requests and its sink logs no frame. There is no raw
  frame, no answer to a delivery (one may come only as a typed response for a call the fence permitted), and no retry
  the transport didn't decide.
- **The retry table, by layer**: the transport retries a read or a plain request once (after a 401 the session
  refreshes the refused token; after a 429/503 it waits `Retry-After`, capped at 30 s, 1 s without one) and never
  resends a write; `reqwest`'s own retries are off (`retry::never()`), since an HTTP/2 retry could replay a write. The
  session returns throttling as `TokenError::Throttled` with its wait, shared by every caller already waiting, with
  no hidden loop. The outbox: a 401 gets one more attempt, a 429/503 a new attempt after `Retry-After`, any other 4xx
  fails, an unknown outcome is checked as above, at most `MAX_ATTEMPTS` (5) since the row was made or resent; a check
  that can't sync backs off and gives up after `MAX_FAILED_CHECKS`.
- **A value in a form no capture showed is kept, not guessed** (`teams_proto::chat::Field::Unknown(raw)`): never read
  as absent, as `0` or as newer. A message whose version can't be read isn't stored (an own one still settles its
  row); upserts go by version, so an older one never overwrites a newer one; reactions are per user, the latest
  change wins, and a take-back stays as a tombstone. `Control/*` messages (typing) are signals, never stored, and
  never settle a row.
- **Deliveries are never answered** (on a reconnect the service replays up to ~15 min of unanswered events, which the
  catch-up and the versioned store absorb). An incoming call is ignored, and the native client sends no
  `user.activity`; what makes an account show online is UPS's endpoint `PUT`, not `user.activity`
  (`docs/protocol/trouter.md`, "Own presence"). Its own availability goes out only as the sealed writes
  `SetEndpointAvailability` and `ForceAvailability`, transient writes of the outbox on `Scope::Account`, which the
  fence permits without a member read only for its own account (`Fence::open_lab_account`), each naming only a
  `RegisteredEndpoint` (only `teams_proto::register` makes one, from a registration of this run the registrar took,
  sent over the native `HttpTransport`: `register` and the push channel take only an `EndpointRegistrar`, sealed,
  which a fake transport is only under `test-support`). It records the account it was registered for, the transport's
  fence's own (`Transport::own_account`), and a write of any other account's availability can't be built from it (the
  constructors give `None`; the outbox drops it unsent). A waiting availability is dropped when the push channel
  stops. A message-loss report means: catch up, then acknowledge it. A token renewal reconnects the socket (5 min
  before expiry, and on the session's renewal); an open socket is never re-authenticated.
- **Read a field's whole captured range, not the mock's.** A fixture holding only a field's common form passes offline
  and loses messages live: a delivery's socket.io `id` is a signed 32-bit number (about one in ten negative), and the
  push URL's endpoint id is base64url (`-` and `_`). A fixture of a field's shape covers every form the captures show.
- **No log line carries a payload, except the app's debug log.** `teams-trouter` caps `log` at `debug` at compile time
  (`max_level_debug`, for any binary that links it): `tungstenite`'s `trace!` prints every frame it sends, the
  `user.authenticate` with its bearer token included. A new dependency that logs at `debug` or below is checked the
  same way. `TEAMS_HEADLESS_LOG=debug` logs our own crates only, by kind and size. The one exception: at `debug` the
  app's own diagnostic log (`teams-log`) carries full HTTP requests and responses (method, URL with its query,
  headers, bodies) and every Trouter frame both ways; at `info` it stays payload-free. Tokens still never reach logs:
  credentials are redacted by the logger itself (`Authorization`, `Cookie`/`Set-Cookie`, the skypetoken and every
  other token header, the token fields of sign-in and renewal answers, sign-in's code and verifier wherever they are
  written, Trouter's `user.authenticate` and the `connectparams` signature (`sig`) of `trouter.connected`, a
  credential's `name=` value anywhere on a line in any escaped form: a SAS link's `sig` in a message's HTML as
  `&amp;sig=`, `%26sig%3D`, `\u0026sig=`), never left to call sites. A lab run's debug log is lab data like a capture
  (never in the repo, never sanitized into it); the personal build's is the user's data, which agents never read.
- **The store** (`teams-store`): one SQLite file per account (`0600`, its dir `0700`), WAL, migrations by
  `user_version`, each in an `IMMEDIATE` transaction; a released migration is never edited, a new one is added. A new
  file is made and switched to WAL under `<store>-runs/open.lock`: SQLite answers two connections switching at once
  with `SQLITE_BUSY` without asking its busy handler. Nothing else in the process opens the store file: closing any
  descriptor of it drops the process's SQLite locks on it.
- **The engine** (`teams-core/src/engine.rs`) is one loop on a current-thread runtime of its own: actions in through a
  channel, each answered by a `Reply`; events out (`next_event`). A write answers with its row (`Ok`) even when the row
  waits or failed, and every change of a row is told once as `Event::Outbox`, whichever read settled it. A catch-up
  runs on the push channel's first connect, on every connect after a drop and after a message-loss report (and on
  demand without a push channel); a failed one is retried from the tick. **One engine per account**: `engine::start`
  takes `Store::engine_lock` (`flock` on `<store>-runs/engine.lock`) without waiting and is refused
  (`EngineError::AlreadyRunning`) while another engine holds it, in any process (`teams-app`, `teams-headless`,
  `teams`): two would mean two Trouter sockets and doubled typing, read receipts and presence. The store's run locks
  are apart from it.

### The app (`crates/teams-ui`, `teams-app`, `ui-sandbox`, `teams-signin`)

- **No window, bus or opener of the user's desktop for a lab or test process.** Every GUI test and every lab run of
  the app is in the UI sandbox (`ui-sandbox`: a niri nested in a headless weston, a private D-Bus session bus without
  service activation holding a fake notification server and tray watcher, a no-op URI opener first on `PATH`, a
  virtual keyboard and pointer); a child's environment is built from scratch (`clean_command`) and inherits only the
  descriptors its caller names (`spawn`). A sandboxed binary refuses to open a window unless `WAYLAND_DISPLAY` is an
  absolute socket inside the dir `UI_SANDBOX_DIR` names (`ui_sandbox::in_sandbox`), and `teams-app` refuses a set
  `DISPLAY` too (the UI kit's GPUI features would let it fall back to X11). Notifications and the tray go over `zbus`
  only to the session bus the app is given (`DBUS_SESSION_BUS_ADDRESS`), never `$XDG_RUNTIME_DIR/bus`; links open only
  through GPUI's `open_url`. The personal build runs on the user's desktop, and agents never run it.
- **The credential broker** (`teams_auth::broker`, `teams-app secrets --lab <alias> --fd <n>`). The sandboxed app holds
  no connection to the user's bus, so the lab's Secret Service items reach it only through a broker started outside
  the sandbox on one end of a socket pair; the app (`--secrets-fd`) uses the other end as its `BrokerSecretStore` and
  has no fallback (no `KeyringSecretStore`, no bus from its environment for secrets). The broker serves `get`, `put`
  and `delete` of that one account's `teams-lab` items, answers anything else "refused" without reaching a store,
  names only the operation, account and audience, and exits when the socket closes; the starter keeps no copy of its
  end.
- **The bridge** (`teams-ui/src/bridge.rs`): the engine stays on its own thread and runtime. The `Bridge` owns the
  `EngineHandle` (the one event receiver and the shutdown), and one task on GPUI's foreground drains its events into
  the `Models`; views reach the engine only through the `Core` trait (the request side, `EngineRequests`; a fake in
  view tests) and await each `Reply`. Shutdown and sign-out join the engine on GPUI's background executor, never the
  foreground, and every request after them answers `Stopped`. **No view builds a request**: the app crates depend on
  no HTTP or WebSocket client, GPUI keeps its `NullHttpClient` (no `set_http_client`), and pictures reach a view only
  as bytes from the engine.
- **The view trace** (`teams-ui/src/trace.rs`, `--trace-fd`): one JSON line per view change, for the live test to read
  what each view shows, never text: view and change names are fixed in the code, and an id passes only as a `TraceId`
  in a service id shape the captures showed (anything else is left out and counted). The live test drives the apps
  only by the keyboard (no screen coordinates) and reads them through their traces, the sandbox's bus and `fence.log`.
- **Own presence is the app's policy** (`teams-ui/src/own_presence.rs` over `teams-core`'s `OwnPresencePolicy`):
  Available while the window was used within the idle time or a manual status is chosen, refreshed on a timer; nothing
  once idle and nothing on quit (no captured request reports Away or Offline for one endpoint); a manual status when
  chosen and its reset when Automatic is chosen again. A failed report is shown and never sent again on its own.
- **The UI kit** is `gpui-component` (Apache-2.0), held to its audit by `crates/teams-ui/tests/reach.rs`: its host tree
  has no HTTP or WebSocket client and only licences an Apache-2.0/MIT project can take, its default GPUI features give
  Linux no X11 backend, the only logger is `teams-log` (installed by the app), and its theme watcher
  (`ThemeRegistry::watch_dir`) is never called. Message text is drawn only from `teams-proto`'s sanitized `RichText`
  by our own renderer, never by the kit's Markdown or HTML view. A kit upgrade reruns the audit. Zed is a design
  reference only: its UI crates are GPL-3.0, and no Zed code is copied.
- **The sign-in window** (`teams-signin`): Microsoft's page in a `WebKitGTK` webview in a process of its own, behind a
  guard that `teams-signin-extension` asks about every request a page sends (classified by `teams_signin::classify`,
  logged to `fence.log` before it is answered; the `nativeclient` redirect caught, never loaded). It isn't a complete
  fence: a synchronous load's redirects, and a kept-alive load's after its page is gone, never reach the guard
  (`docs/backlog/webkit-sign-in-guard-redirects.md`). So the lab's window opens only a loopback page and stays on
  loopback (`cli::NOT_LIVE`; off loopback the guard and the navigation policy refuse everything and the network
  session's only way out is `egress::BlackHole`), and the lab signs in through `proto-lab auth`. The personal window
  (`personal`) is bound to Entra's hosts and the identity provider's instead, its only way out `egress::HostGate`.

### Captures, reference copies, fixtures

- Captures (`captures/<scenario>/<ts>/events.jsonl`) and `proto-lab export`'s reference copies (`reference/<name>`)
  live under the lab's data dir. Export refuses any destination inside a git work tree and warns about what its checks
  find (`--strict` refuses instead). Neither is ever copied into the repo.
- A committed fixture is written by hand from a reference copy: the same shape, made-up values, a `_synthetic` marker.
  Run `cargo test -p fixture-guard` after touching any `tests/fixtures/` or `docs/`: it checks the marker and scans
  for tokens, signatures, emails, MRIs, GUIDs (except `PUBLIC_GUIDS`), meeting links and passcodes, and a call's network
  data: public IP addresses (a call's fixture uses the documentation ranges), ICE ufrag and password, DTLS fingerprints
  and TURN credentials (placeholders, or values that say they are made up: `fixture_guard::network`).
- `fence.log` keeps scope ids verbatim (a 1:1 chat id names both object ids): it is lab data, not sanitized material.
- **The endpoint catalog**: `docs/protocol/endpoints.md` lists every endpoint seen or used, with how the browser
  fence and the native client treat it. A task that captures, maps, allowlists or uses a new endpoint, or changes how
  one is treated, adds or updates its row in the same change; one seen in a capture and described nowhere goes in as
  "seen, not reviewed".

### Scenarios (`crates/proto-lab/src/scenario/`)

- Steps use the DSL `Driver` (`dsl.rs`): `data-tid`/CSS waits, request and failure waits; every page call is bounded
  (`CALL_TIMEOUT`), and a timeout names its step.
- A writing scenario proves enforcement live first: a `ForcedDeny` (method + URL regex + body marker, denied before
  classification, logged `forced`) on a probe that must fail in the page (`net::ERR_BLOCKED_BY_CLIENT`, matched by
  `networkId`) and never reach the other account. The web client resends a failed send on a later load, so the forced
  deny covers the marker for the whole run, and this run's probe carries a label of its own. Every scenario that reads
  members live on a lab profile force-denies an earlier run's probe (`ab_chat::probe_rules`), `echo-call` included,
  and with it the group chat's first message, which begins with the probe's marker.
- A scenario's `DenyPolicy` says which denies it expects: `load` all of them; the writing scenarios only their own
  forced denies, by label, and the reviewed **expected-deny list** (`scenario/expected.rs`: host + method +
  `/`-segment path pattern, frames by socket host, each class with the reason the scenario doesn't need it: bot
  invokes, the account's own channel entry and channel properties, the file browser's prewarm, Trouter delivery
  responses). The list judges the run only, never a verdict: everything on it stays denied and logged, and it holds
  only what the lab rule denies. A deny on the scenario's own chat, one for a partial or refused membership, one whose
  `fence.log` entry was lost, a deny of what the lab rule lets through, and any class off the list fail the run, the
  failure naming the deny's contacting service; a new class goes onto the list with its reason, never waved through.
  `echo-call` still exits 1 on the call's follow-ups: they are needed, so they aren't on it.
- Every run uses the one classifier (`classify::Classifier`, the lab rule; `echo-call` adds the echo service's MRI
  for its test call start), so a scenario picks none. What a scenario holds back on purpose is a labelled forced deny,
  of requests (`ForcedDeny`) or of frames (`ForcedFrameDeny`: `ab-trouter-quiet` and `own-presence-endpoint` hold
  back `user.activity`, the other `own-presence` runs the endpoint `PUT` or the manual status), expected by its label.
- The sign-in window (`teams-signin`) keeps default-deny rules of its own (`teams_signin::classify`): reads, the
  token endpoint and authz in their captured queries, and Entra's sign-in form, which still takes any query
  (`docs/backlog/seeded-own-rules-ignore-the-query.md`). An own-state rule (`OwnRule`) matches the method, the
  origin, the path and the query (none unless given its `QueryRule`), never the body or headers.
- After native runs the web client opens a chat at its "last read" mark (a native read receipt moves the stored
  horizon but not that mark), so a scenario waiting for a new message clicks "Jump to newest" or the "New messages"
  pill (`ab_chat::wait_for_newest`).

### Live runs

- From a normal build, headless (`--headless`), under the armed fence, **one browser per lab profile at a time**;
  no browser is left running afterwards. Writes reach only the 3 lab accounts and the approved echo service.
- The personal build is never a live run of an agent's; the live lock is the lab's.
- Every live command, a lab browser's or a native client's, holds the live lock (`flock
  ~/.local/share/teams-lab/live.lock`) and runs under an outer `timeout --signal=INT`. Every page call and navigation
  is bounded (a `beforeunload` dialog holding a navigation is accepted after 5 s), and SIGINT or SIGTERM kills the
  run's browsers and lets the fence's decisions still running land in `fence.log` (`crates/proto-lab/src/stop.rs`).
- **No call is answered.** No scenario clicks answer, the fence doesn't map `acceptance`, and Trouter delivery
  responses are denied. The native Trouter client ignores incoming calls the same way and has no path to answer any
  delivery.
- Live tests are built only with `--features live` and run only with `PROTO_LAB_LIVE=1`.
- Tokens: our client is the native `1fec8e78-…` with the `nativeclient` redirect. The chat service takes
  `Authorization: Bearer` with an `https://ic3.teams.office.com` token, not a skypetoken, and a skypetoken expires with
  the Entra token it was exchanged from (`docs/protocol/auth.md`).

### Chrome under load

The machine is often loaded by other agents' builds (load average 30–75), and then
`browser::a_browser_that_exits_early_is_reported` is known to flake on timing. Rerun that one test before calling it
a failure; never raise a timeout or skip an arming step to make it pass.

- A test reads a page only after it has loaded (`tests/common/page.rs`'s `navigate`): `Page.navigate` answers at
  commit, before the page's scripts have run. A read that throws after that is a real error, never retried.
- Under load Chrome can take over 15 s to start, failing a test with `Timeout(15s)` at `Browser::launch`; that is
  load, not the test.

**A fence breach is never a flake.** Under load an arming step can go unanswered for 10 s; the target is then closed
or the browser killed (contained: "Arming the lab browser" above), and the run is breached when the target's resume
was sent, it wasn't held at attach, or it could be neither closed nor killed. Such a breach is real: it counts as
one. Don't rerun until a run comes out clean and record that as a pass. Keep the breached run's
printed output (the failed step survives there and in the capture's `arming_failure`; `fence.log` keeps the
`ArmingRecord` without ids), check it and `fence.log` for anything sent unguarded, report the run as breached, and
report any later clean run next to it, never in its place.

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
