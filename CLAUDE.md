# ms-teams

A native Microsoft Teams client for Linux in Rust + GPUI, written against Teams' internal APIs with the web client as
the reference. The next work is M4, meetings and the calendar (`docs/plans/20261026-native-teams-m4-meetings.md`);
finished plans are in `docs/plans/completed/`. Commands and layout are in [README.md](README.md).

## Hard rules

These hold for every agent and every change, with no exceptions. **Lab accounts contact only each other; everything
else is allowed.** The lab browser's fence and the native client (its HTTP transport and its Trouter frames) follow
this rule. The rules about writes, calls and the browser fence govern the lab and every lab binary; the personal build
(`teams`) has a fence of its own and the three rules about it below. The rules on the native client's sealed types,
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
- **Calls go only to the lab, both ways.** A call is a contacting write whose participants are its caller, its callees
  and whoever it adds; every call write is mapped only from a capture, in its captured shape. The fence judges a call's
  entry, each a fresh decision. A start is permitted when every callee it names is a lab account or the echo service
  and the chat it is tied to is lab-only by a fresh, complete member read; a start is tied to its chat by what it
  carries (the chat it names, or for a 1:1 start naming only its callee, the two accounts' 1:1 chat as the chat service
  names it), never by the client's context, and the test call's start, which names no chat, passes only in its captured
  shape. A running call is joined only in the chat the join names, when that chat's fresh, complete member read lists
  the joiner and only lab accounts. An `add` is judged per invitee like a start. An incoming call's invitation is
  **offered** only when it is readable, addressed to the account, and names a caller and participants that are all lab
  accounts or the echo service; only an offered invitation is answered or declined (a decline rejects the call as the
  web client does), tied to it by the call leg the answer's link names, and only until its ring ends (answered,
  declined, cancelled, answered elsewhere, timed out). Every other incoming call (the user's own account's included) is
  ignored: no answer, decline or join for it, and no scenario click. Everything a call sends once its start, join or
  answer was let in goes without a further decision, each send logged: its follow-ups (media and endpoint updates,
  renegotiation and its answers and acknowledgements, keep-alive, channel parameters, the acceptance's
  acknowledgement), its `leave` and end exchange, what the callee's client sends by itself while it rings, and its
  Trouter delivery responses; no call registry or link binding stands between a call and these. The native client
  still sends each as a sealed type in its captured shape, and a delivery response only as a typed `OwnFrame` for the
  delivery it answers. No transfer, forwarding or replacement is mapped. The roster isn't watched. Media (TURN, ICE,
  DTLS-SRTP) isn't classified: it carries a call the signaling fence permitted.
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
  (messages, edits, deletes, reactions, typing, read receipts, and the calls of the next rule); only to a 1:1 or group
  chat whose thread, read fresh when the fence decides, unpaged and in a shape a capture showed (every member in a
  captured MRI form), lists the account as a member (never a channel, a meeting chat, a `48:` stream, a new chat, a
  paged answer or an unknown shape), or the account's own availability on its own registered endpoint in a captured
  value, or a call write as the next rule allows; only from an explicit action in the UI or a setting the user turned on
  (typing, read receipts and presence only after the first run's screen is confirmed), save the call's own sends the
  next rule names; logged to its `fence.log` before it goes. Its reads are `GET`s and the sealed lists: `OwnState`, and
  `PostRead` naming only the account or people the account's stored chats list, never a search or a suggestion list; it
  sends no plain request, and logs its Trouter frames. Its sign-in window reaches only Entra's sign-in hosts and the
  identity provider's hosts `account.toml` names, through its egress gate. The rules that hold for both builds hold for
  it; nothing in it changes a lab rule: every lab binary keeps the lab fence, refuses the personal data dir, keyring
  items and accounts, and refuses to run with `personal` or `test-support` built in; `teams` refuses the lab's.
- **The personal build's calls are the user's, in the account's own chats.** Only 1:1 and group-chat calls (no meetings,
  screen or content sharing, transfer, forwarding, replacement, call park or PSTN), every call write a sealed type in a
  captured shape sent as `Authorized<_>` through the outbox, a fresh `Fence::check` logged to its `fence.log` before it
  goes, never resent after an unknown outcome or by a later run. A start or a join goes out only on the user's action in
  that chat: an existing 1:1 or group chat whose one fresh, unpaged thread read in a captured shape lists the account
  and every callee the start names; the chat is the one the write carries, as the lab's rule ties it, never the client's
  context, and a join also needs a captured answer or delivery binding the running call to that chat. Any readable
  invitation addressed to the account may be offered, whoever it is from; it is answered or declined only on the user's
  explicit action on that still-offered invitation (its banner or notification), never by a delivery, a timer, a setting
  or a reconnect, and a decline is the captured rejection, not a dismissed banner. An invitation stops being offered
  when its ring ends (answered, declined, cancelled, answered elsewhere, timed out). One call or ringing invitation at a
  time: another invitation isn't offered and nothing is sent for it. Every follow-up on an entered call, whether
  requested by the user (mute, camera, hang up) or generated by the call's machinery, needs this run's registration of a
  call the user started, joined or answered, bound to its conversation, call and service as the captures bind them
  (never by a bare id), and only a captured operation, until the call ends (its captured end exchange included); a
  reconnect keeps a live call's registration and never restores an ended call or invitation. Without a further user
  action only these go: that call's captured setup, keep-alive and end writes (acknowledgements, channel parameters,
  renegotiation, `leave` on a failure), and the captured sends the client makes by itself while an offered invitation
  rings, bound to it, never one that answers, declines, joins or adds. No `add` and no new chat but the test call's, a
  named exception for the echo service alone, each from the user's "Test call": the 1:1 chat of exactly the account and
  the bot created in its captured shape, the start in the test call's captured shape, and the `add` of exactly that bot
  to that call (its MRI a constant in `teams-proto` from the web bundle's `teamsEchoBotMri`, never taken from the
  network or the UI). The calling registration is an `OwnState` entry and the calling socket's frames are logged; a call
  delivery response goes only as a captured, typed `OwnFrame` bound to the delivery it answers, under the same rules as
  the write it stands for. The call roster isn't read or watched. Media isn't classified: it carries only a call the
  signaling fence permitted, through relayed candidates only, with no local address in an offer. The personal build's
  PipeWire is the desktop's, fixed when it is built. The microphone opens only after the user's start, join or answer
  passed the fence; the camera is off at every start, join and answer, and only the user turns it on, through the
  desktop portal.
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
  socket (any host under `trouter.teams.microsoft.com`) only socket.io 0.9 types `0`, `1`, `2`, `3` and `5` pass, so a
  delivery response (`3`, a call's receipts and acknowledgements among them) passes, and a JSON message, an
  acknowledgement, a binary frame and anything unparsable are denied; on a contacting service's socket
  (`teams_fence::on_contacting_service`) every frame is denied; anywhere else a frame passes. A frame let through is
  logged `allowed` (method `SEND`). The page's received Trouter frames (`Network.webSocketFrameReceived`, observed in
  event order, before any send they cause) go to the call registry on their connection (the socket's session and
  `Network` id), which offers the account's own invitations from lab callers: only an offered invitation is answered
  or declined. No response is paused for a call: a call is judged at the request stage alone, and nothing is
  registered or bound for its later requests.
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
  native client or browser session `Fence::open_lab_account(alias)`, the same with that account as its own (whose
  invitations it offers); its membership source
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
  keys make it `Unknown`), and a call start (the echo test call's, or a chat's) is matched in its captured shape.
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
  created, judged by its added participants alone), an operation on a call (`Scope::CallOperation`, decided by the
  fence's call rule, below), typing and read receipts on a chat, and the account's own availability (which keeps
  its own-account check). Typing and read receipts reach nobody: on a chat
  id `is_chat_id` takes (1:1, group or meeting, the rule `contacting` carves them out by) they are permitted without a
  member read, judged only by the participants they name, and logged like every decision; anywhere else (a channel, a
  chat id in another form) they are judged like any write. So a caller hands it a `Typing` or `ReadReceipt` target
  only for the content-free signal: `contacting` carves typing out only in its captured body (`content` empty or
  absent, no key but `content`, `contenttype`, `messagetype`) and a read receipt only with `name` its one query key.
- **A call start** maps in two captured shapes (`contacting/call.rs`): the echo test call's (`WriteKind::Call` on
  `Scope::New`, the caller and the echo service added), and a chat header's call (`chat_call_start`): `WriteKind::Call`
  on the `Scope::Conversation` it is tied to by what it carries (`groupChat.threadId`, a group chat's id, or for a
  start whose `groupChat` is `null` and that names one callee, the 1:1 chat of the caller and that callee by
  `one_to_one_id`), the caller and every callee of `participants.to` added; so `Fence::check` reads that chat fresh
  and admits the start only when its members and every callee are lab accounts. The callees are work accounts
  (`8:orgid:<guid>`), at least one, none twice, none the caller.
- **The lab's call rule** (`decision.rs`'s `CallRule::Entry`, every lab fence's): a call is judged by its entry, every
  other operation on it passes. A start as above. A join (`POST` to a conversation controller, with a call invitation
  or roster-only) is `WriteKind::Call` on `Scope::CallOperation` naming the group chat it joins (`joins`); the fence
  reads that chat fresh and permits the join only when the read is complete, lists the joiner and is lab-only. An
  answer (`accept`, `CallAnswer`) or a decline (`reject` `603`, `CallDecline`) passes only on an invitation this fence
  offered (below), tied to it by the call leg its link names (`…/cc/v1/incoming/<call>/…`, the call the invitation's
  `attach` link names: `CallRegistry::resolve_answer`), the answer from the invitation's callee, while the invitation
  is open for it (ringing; the same `accept` again once answered, the same `reject` once declined), its group chat
  read fresh. An `add` passes when its sender and every invitee are lab accounts or approved services (its row maps
  it only naming the 1:1 chat of the sender and its one invitee and the sender as organizer). Every other call
  operation (`CallFollowUp`, `CallRinging`: the follow-ups, the callee's ring-time sends, `leave`) is permitted with no
  member read and no registry lookup, logged like every decision; `Fence::check_delivery_response` permits every
  delivery response that names a delivery id, logged. The registry still reads the deliveries and what passes (an
  acceptance, a call's end, an `attach`'s ring-end callbacks), so the native client's call machine sees the same
  deliveries, but it decides nothing for a lab fence.
- **The call registry** (`crates/teams-fence/src/calls.rs`, one per fence) is the personal build's call rule
  (`CallRule::Registry`, `Fence::tracks_calls`; tested on a lab roster through `Fence::tracking_calls`, `test-support`
  only). Under it no source reads a call's members, so a
  follow-up passes only on a call this fence permitted, to exactly a link the call's own services handed out. A call
  start's permit (this fence's, taken once: `WritePermit` carries the fence's run-unique id and a serial) and the
  start's body make a pending call (`Fence::pend_start`); a readable answer registers it (`Fence::register_start`:
  `conversationController` and the answer's `updateEndpointMetadata`, `leave` and `addParticipantAndModality` links,
  each the controller's path plus its segment with its query); a Trouter delivery at exactly the start's own
  `acceptance` callback path binds the call controller's `acknowledgement` and `applyChannelParameters` links, all of
  one call (`Fence::observe_delivery`). A follow-up is `Scope::CallOperation { operation, url, from, callbacks }`
  (`WriteKind::CallFollowUp`, `Invite` for an `add`), resolved by the registry with no member read: a live call
  holding that exact URL for that operation, its sender the call's caller, invitees only on an `add`, judged per
  invitee, and every callback link it hands out (an acknowledgement's, an `add`'s) on the start's own endpoint and
  call agent; an `add` maps only when its `groupChat` is the 1:1 chat of the sender and its one invitee and its
  `meetingInfo` names the sender as organizer; otherwise `Partial(UnregisteredCall)`, `Partial(CallEnded)` or `CallMismatch`. A permitted `leave` leaves
  only that `leave` passing; a delivery at the start's `conversationEnd` or call `end` callback ends the call for good.
  `fence.log` never holds the sender. A link held by a live call acts on that call; a call the account left keeps its
  links as a tombstone, which yields to a re-invitation to that conversation answered since and to a rejoin (a later
  start or join answered with the same conversation takes the links from an ending or ended call); a link two live
  calls hold resolves to neither. An invitation the call machine dropped before it rang is retired for good
  (`Fence::let_go`). The native transport feeds the registry around a call write's send (below, "The call writes").
  A `conversationUpdate` binds nothing.
- **The personal fence's calls** (`crates/teams-fence/src/decision/personal_calls.rs`, `docs/personal-build.md`,
  "Calls"): no roster. A start is judged by its chat's one fresh thread read (`Listed`) listing the account and every
  participant it names, the account among them (`CalleesNotInChat`, `NotAMember`); the test call's start and its
  `add` only with exactly the account and `TEAMS_ECHO_BOT_MRI` (the web bundle's, a constant in `teams-fence`
  re-exported by `teams-proto`; `NotTheTestCall`); a join by the registry's binding and its chat's read listing the
  account; everything else as the registry resolves it, with no read. `observe_delivery` offers any readable
  invitation addressed to the account whose caller isn't the account, one at a time (none while a registered call is
  live, an offered one answered, or one rings with its end bound); an offered invitation's receipt reads no chat;
  `pend_start` takes only the account's own start or join. The personal build creates no chat for the test call.
- **Offered invitations** (`crates/teams-fence/src/calls/offer.rs`): an invitation delivery (Trouter `evt` 107 or
  109, `callNotification`) to the fence's own account (`Fence::open_lab_account`) from a lab caller is offered by
  `observe_delivery` (a personal fence offers one from any caller but the account itself, one at a time: above);
  under the lab's rule a group call's invitation names only its caller, callee and group chat, so every decision on
  it also reads that chat fresh. Every other invitation is ignored, and nothing answers or declines it under either
  rule.
  Under the lab's call rule only an offered invitation's answer and decline are judged (above). Under the registry's
  rule, on it pass, each to exactly a link the invitation or an answer bound
  (`Scope::CallOperation`, `joins` naming the conversation an `attach` subscribes to): its receipt, the `attach`
  (`CallRinging`: its one roster-only conversation join joins no call), `progress` (`ringing`), the endpoint state,
  the answer (`accept`, `CallAnswer`) and the decline (`reject` `603`, `CallDecline`) while it rings; once answered,
  the answered call's follow-ups until the callee leaves. A permitted `accept`, `reject` or `leave` passes again (the
  browser decides a request on two layers). A join (`POST` to a conversation controller with a call invitation) is a
  `Call` start on the group chat the conversation's invitation bound it to, from that invitation's callee, and its
  answer registers the call; a roster-only conversation join passes on the same binding. A renegotiation or its answer
  delivered at a call's own `mediaRenegotiation` or `mediaAnswer` callback binds the `answer` or `acknowledge` link it
  names, and one at its `mediaAcknowledgement` callback binds nothing; each of them may get its receipt, once (without
  one the call controller holds the call's next negotiation); the caller's `renegotiate` and `updateMediaDescriptions`
  come from the acceptance.
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
- **Call writes** (`teams-core/src/calling.rs`, `outbox/call.rs`) go through the outbox as a kind of their own, no
  row and no transient (`Outbox::try_call`): each attempt a fresh `Fence::check`, its answer (any status, with its
  body) back to the `teams-call` machine, which decides what a failed step means (the call ends, with a `leave` when
  the account is in it); one more attempt only after a 401 or a throttle of at most `MAX_TRANSIENT_WAIT` (once each,
  never after an unknown outcome: a call controller acting twice is worse than a failed step) and only if the
  machine still wants the write once the wait is over (`Calls::may_send_again`, asked again then: a hang-up during the
  wait fails it unsent). One call write is in flight at a time: beside it the engine takes only the machine's events
  and delivery responses, so its later writes stay in the machine's queue and a call that ends drops them. A start, a
  join or an answer goes only once its media is set up, the session started without its devices; the microphone and
  the speaker open only once that start, join or answer passed the fence and was taken (the call ringing, joining or
  connected). An own renegotiation begins in the machine (`Calls::begin_renegotiation`) before the session makes its
  offer, which replaces the one it waits on; one turned away while another negotiation is in progress (`491`) goes
  again with its offer a few seconds later (no other own one begins meanwhile), and the far end's renegotiation goes
  first: one delivered while ours is made or waits for its answer is held until ours settles, and ours gives way to
  it when turned away (`RenegotiationGaveWay`: the far end's answered with our camera's line only receiving, that
  line then offered again). The media's SDP
  steps (an answer applied, a renegotiation answered, an own offer, the camera turned) run one at a time, in the order
  the signaling asked for them. Every media step that blocks runs on tokio's blocking pool, never on the engine's
  thread, and the calling runs as a task of its own on the engine's thread (a `LocalSet`'s), so a chat's work the
  engine awaits doesn't hold up a call's write, deliveries or responses. Nothing answers or declines an invitation but
  its `CallAction`, and nothing turns the camera on but `CallAction::Camera` (off at every start, join and answer);
  the engine stopping hangs the call up and lets an invitation that rings go unanswered.
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
  frame, no answer to a delivery but a typed call delivery response (`AcceptanceAcknowledgement`,
  `InvitationReceipt`, `DeliveryResponse`, sealed and no `OwnFrame` itself) sent only by `FrameSink::respond`, which
  makes its JSON once, has `Fence::check_delivery_response` decide exactly that JSON for the connection the sink was
  given (`FrameSink::with_call_responses`, the key its deliveries are observed under) and sends it once as a private
  typed frame, or in those two halves, so the socket reads on while the fence decides (`FrameSink::decider`'s
  `ResponseDecider::decide`, then a sealed `PermittedResponse` sent only by that connection's
  `FrameSink::send_permitted`), and no retry the transport didn't decide.
- **The call writes** (`crates/teams-proto/src/write/call.rs`): each call request a sealed `Mutating` type built whole
  when it is made: its method, its exact URL (`CALL_START_URL`, or a `CallLink`, a link the call's services handed out
  under `https://api.flightproxy.teams.microsoft.com/api/v2/ep/<host>/…`, kept byte for byte, its routing query never
  decoded and built again), its headers (`CallContext`: the call's chain id, a fresh message id, the region and
  partition) and its body. The constructor classifies that request with `teams_fence::contacting` and keeps the
  target it gives, so the native target is the browser fence's for the same bytes; a request it doesn't map, or maps
  as another write, isn't made (`CallWriteError`). Only call writes go to `Service::Calling` (a read or a plain
  request there is refused), with the account's skypetoken (`Credentials::skypetoken`; none is
  `Refusal::NoSkypetoken`), never resent. The transport pends a start or a join with media in the call registry
  before it goes (`Fence::pend_start`, exactly the body sent; `Refusal::NotPended` when the registry won't take the
  permit) and registers the call from a successful answer; an `attach` or an `accept` pends its answer and binds its
  links, and on a lab fence one the registry won't pend goes with its answer unread (only a fence that tracks calls
  refuses it, `NotPended`); one answered without success binds and retires nothing (a 401 or a short throttle is
  followed by another attempt), and only the call machine, once it drops an invitation before it rang (no further
  attempt at its `attach` goes), retires it (`Effect::LetGo`, `Fence::let_go`). The registry's bookkeeping is what
  the call machine reads its deliveries by (`Observed`); under the lab's rule it decides nothing. The TURN credentials
  are a read (`RelayTokens` on `Service::Relay`).
- **A group call's video** (`teams-media/src/mcp.rs`, `session/control.rs`, `teams-core/src/calling/gallery.rs`;
  `docs/protocol/calling.md`, "Group video") goes through the call's mixer: every video line answered and offered
  `sendrecv` as the web client does, which way each really goes told by the media descriptions; the start, join,
  `accept` and every answer to the mixer's renegotiations carry the descriptions and the bandwidth seed (without
  them the mixer sends no video on the receiving lines); our offers number H.264 as the mixer does (`107`, its
  retransmissions `99`: it takes only its own numbers); whose video each receiving line shows is a source request
  on the data channel's control plane, which is media (never classified, never a write). Who is in a call, 1:1 or
  group, comes from the `rosterUpdate` deliveries (a participant counts when it is `active` with an endpoint holding a
  `call`: `teams-call`'s `read::roster`), read only to show it: nothing is sent for them and the fence reads no roster.
- **A call's media reach** (`teams-media`): the peer gathers and writes only relayed candidates, through Teams' fixed
  relay host (`TEAMS_RELAY_HOST`, resolved when the call's media is set up: every address over UDP 3478, then TCP 443,
  a `300 Try Alternate` followed once; `RelayTokens` gives only the credentials and their expiry), never a host
  candidate or a local address in an offer; its
  sockets are `teams-media`'s own, never the HTTP transport's. Media and the data channel's control plane aren't
  classified: they carry a call the signaling fence permitted. SDP goes to and from the call controller only in
  Teams' dialect (`application/sdp-ngc-1.0`; a browser's SDP is refused where the dialect is required). H.264 is
  Cisco's prebuilt OpenH264, loaded at run time only when its SHA-256 is Cisco's (the lab's from its data dir's
  `openh264/`, the personal build's from the user's own copy); only a `test-support` build compiles the bundled
  source. Without the library a call carries audio only.
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
- **Deliveries are never answered** but a call's, by a typed response through `FrameSink::respond` (above); on a
  reconnect the service replays up to ~15 min of unanswered events, which the catch-up and the versioned store
  absorb. On the messaging socket an incoming call is ignored. The calling stack's socket
  (`teams_trouter::Trouter::for_calls`, its own endpoint registered as `SkypeSpacesWeb`) hands every delivery to the
  fence first (`Fence::observe_delivery`): an invitation it doesn't offer or can't read is ignored and nothing is sent
  for it, the rest go to their consumer, answered only through the delivery's own connection by `FrameSink::respond`
  (or its two halves, above); the personal build opens its calling socket only when its devices opened
  (`teams::calling::open`), its registration an own-state entry and its frames logged. The native client sends no
  `user.activity`; what makes an account show online is UPS's endpoint
  `PUT`, not `user.activity` (`docs/protocol/trouter.md`, "Own presence"). Its own availability goes out only as the sealed writes
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
- **Calls in the app** (`teams-ui/src/calls.rs`, `call_alert.rs`, `call_view.rs`, `ringtone.rs`, `video.rs`): a
  build shows calls only when its models were made with them (`Models::with_calls`: the lab app, and the personal
  build once its calling opened); without them it shows, rings for and asks for none, whatever the engine tells. Every
  ask is one `CallAction` through `Core::call`, which the engine judges again and sends under the fence, and each is
  the user's: a button, a key, a switcher command or the incoming call's notification (Answer and Decline on the bus
  the app is given). Nothing in the views answers, declines or turns the camera on by itself. The ringtone plays
  through the device seam on the PipeWire the build allows; the video is drawn from the media's `VideoFeed` through
  GPUI's external surfaces, and the settings' video switch turns the codec on or off for the calls from then on.
  Calls are traced by number and kind (`call asked`, `ringtone started`, `video-tile presented`), never with a name.
- **The calls' devices** (`teams-media/src/devices/pipewire*`, `devices/catalog.rs`, `devices/portal.rs`): the
  settings list PipeWire's microphones, speakers and cameras and keep a choice by node name, "follow the system" (no
  `target.object`) by default; a choice moves every open stream of its kind by making its replacement and swapping it
  in once linked (make before break), so a call's device never says it closed. Which PipeWire a build opens is fixed
  when it is built, never by an argument (`devices::pipewire::THIS_BUILD`): the UI sandbox's private one for the lab
  and for every `test-support` build (even with `personal` on, as `--all-features` builds it), the desktop's only for
  `personal` alone, which only `crates/teams` enables; `teams-headless` opens none (its media is files). The lab app
  refuses a PipeWire outside the sandbox as it refuses a window outside it. The personal build's camera comes
  through the desktop's camera portal; a sandbox build asks only a portal on a bus inside the sandbox dir, where the
  sandbox's fake answers. WirePlumber moves a stream whose `target.object` names the current default to every later
  default (PulseAudio's rule), so a chosen device's stream sets `node.dont-move` (it stays on its node and falls back to
  the default when the node goes; `node.dont-reconnect` would destroy it with its node), and the
  sandbox's recorder sets `node.dont-reconnect`.
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
  invokes, the account's own channel entry and channel properties, its call log's read horizon, the file browser's
  prewarm; no frame class, as a Trouter delivery response passes). The list judges the run only, never a verdict:
  everything on it stays denied and logged, and it holds only what the lab rule denies. A deny on the scenario's own
  chat, one for a partial or refused membership, one whose `fence.log` entry was lost, a deny of what the lab rule
  lets through, and any class off the list fail the run, the failure naming the deny's contacting service; a new class
  goes onto the list with its reason, never waved through. A call's requests pass once its entry was let in, so none
  of them is on it.
- Every run uses the one classifier (`classify::Classifier`, the lab rule; `echo-call` adds the echo service's MRI
  for its test call start), so a scenario picks none. What a scenario holds back on purpose is a labelled forced deny,
  of requests (`ForcedDeny`) or of frames (`ForcedFrameDeny`: `ab-trouter-quiet` and `own-presence-endpoint` hold
  back `user.activity`, the other `own-presence` runs the endpoint `PUT` or the manual status; `ab-call` and
  `abc-call` a's call starts in `--call start-held`), expected by its label. Nothing a call's page sends is held back
  otherwise: the fence judges its entry and lets the rest of the call through. A call run that fails hangs up every
  page that shows its call (a's first) and waits up to 10 s for each one's `leave` before its browsers close
  (`ab_call::hang_up_all`), so no call is left running in the chat; a start the service ends at once because the chat
  already has a running call (`conversationEnd` 409, sub-code 5704) fails the step then, naming that call
  (`ab_call::running_call_conflict`, `native_call_conflict`).
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
- **Nothing is played aloud.** Every lab browser runs muted with Chrome's fake audio manager (`--mute-audio`,
  `--disable-audio-output`, `browser::BASE_ARGS`), which opens no audio device or server of the desktop; a call
  scenario reads the audio it receives in the page (the WebRTC hooks' audio tap: WebAudio and a muted element of its
  own, a Goertzel check at the tone), never from a speaker.
- The personal build is never a live run of an agent's; the live lock is the lab's.
- Every live command, a lab browser's or a native client's, holds the live lock (`flock
  ~/.local/share/teams-lab/live.lock`) and runs under an outer `timeout --signal=INT`. Every page call and navigation
  is bounded (a `beforeunload` dialog holding a navigation is accepted after 5 s), and SIGINT or SIGTERM kills the
  run's browsers and lets the fence's decisions still running land in `fence.log` (`crates/proto-lab/src/stop.rs`).
- **Only the lab's own calls are answered or declined.** A scenario clicks answer, decline or join only in a
  scenario that exists to do it (`ab-call`, `abc-call`: the lab's accounts calling each other), only on the run's own
  call (every invitation the page received is that run's call from the caller to that callee in the run's chat, and
  the page shows one incoming call, or for a join the run's chat with one join), and the fence decides what the click
  sends again on its own: an answer or a decline passes only on the account's own offered invitation from a lab
  caller (a group call's chat read fresh), a join only in a chat whose fresh, complete read is lab-only; what the
  call sends after that passes. Every other incoming call is ignored and never clicked. The native Trouter client
  answers no invitation by itself: its calling socket only hands deliveries on, an invitation the fence doesn't offer
  to no one, and only a `CallAction` answers, declines or joins a call.
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
