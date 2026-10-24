# Native Teams for Linux — M3: Calls

## Overview
- **Goal (the user's, 2026-10-24): a Teams client with audio and video calls.** M3 ends with the personal build
  (`teams`) making, answering, declining and joining real 1:1 and group-chat calls with the user's colleagues, audio
  and video, under rules the user approves. Everything is proven on the lab first: M3 maps the call writes under the
  lab rule (a call is a contacting write whose participants are its caller, its callees and whoever it adds),
  completes the echo call the fence stopped in M0, then builds native calling on M2's app: a call started from a chat,
  an incoming call that rings and is answered or declined by an explicit action, a running group-chat call joined,
  the in-call view (mute, camera, hang up, devices), 1:1 and group audio with echo cancellation, and video. Each step
  runs on the web client in the armed browser, then the native client under the fence (against itself and against the
  web client, both ways), then the app in the UI sandbox.
- **Exit:**
  - **The lab acceptance** (Task 21): the echo call returns its tone natively; a native a → b call and an a–b–c group
    call carry audio and video among all of them, live, from the app in the UI sandbox, with a mixed case (the app and
    the web client in one group call); the app and the web client call each other both ways with audio and video; a
    declined call and a late join behave; `fence.log` shows writes only on calls the fence itself permitted; no call
    outside the lab is ever answered.
  - **The personal build's calls** under the rule the user approved (Task 2), implemented and tested on the mock
    (Task 22).
  - **The user's real-call checklist** (Task 23), which the user runs (agents never run the personal build): the test
    call, 1:1 audio and video with a colleague both ways, a group-chat call, a late join, a decline, a device switch
    mid-call, an incoming call from outside the user's chats rung and answered, a call answered on another of the
    user's devices.
- **The user's decisions of 2026-10-24** (this plan's brainstorm and its review):
  - **Calls only**: 1:1 and group-chat calls. Meetings (joining by link, the lobby, the meeting roster, the meeting
    chat, a chat's meeting card) stay in M4.
  - **The personal build's rule**: a call is started only in an existing chat whose fresh thread read lists the
    account, every callee in that read; a running call is joined under the same rule; any incoming call addressed to
    the account may be answered or declined, only by the user's click; no `add` but the echo service's.
  - **The test call in the personal build**: a named exception for the echo service only (its MRI from the web
    bundle's `teamsEchoBotMri`): the 1:1 chat with the bot, the start in the test call's shape, the `add` of exactly
    that bot.
  - **Decline is a real decline**: it rejects the call as the web client does (captured and mapped in the lab), so the
    ringing stops everywhere and the caller sees it declined.
  - **The user's build may call the lab accounts for testing**: the user creates a 1:1 chat with each lab account
    once from their own client, so the personal rule (a call in a chat that lists the account) covers these calls with
    no exception of its own. The lab rule doesn't change: the lab never answers, declines or places a call to the
    user's account, no lab config names it, and the lab fence denies every contacting write in those chats; such a
    call rings on the lab account and is ignored, unless the user answers it on a device of their own signed in as
    that account. The user places it when no lab live run holds the live lock.
  - **One pass**: audio and video are built through the lab, then the personal build; the user's real-call check is
    at the end (Task 23), with no audio-only checkpoint before it.
- **What the hard rules already settle for the lab** (CLAUDE.md, "Calls go only to the lab, both ways", and
  `docs/backlog/fence-call-follow-ups.md`, the user's decisions of 2026-10-23):
  - **Starting**: allowed when every callee is a lab account or the echo service, in the shape the call start's
    capture shows. The calling stack's own Trouter endpoint (`SkypeSpacesWeb`) is registered freely (the registrar is
    no contacting service).
  - **Answering**: allowed when the invitation names a caller and participants that are all lab accounts or the echo
    service: the `acceptance` request and, where the flow uses one, a typed delivery response for that call. Every
    other incoming call is ignored, never answered.
  - **Follow-ups** (`updateEndpointMetadata`, `applyChannelParameters`, `acknowledge`, `mediaRenegotiation`, hold,
    `updateCallState`, `controlVideoStreaming`, `leave`): allowed on a call in the fence's **call registry**, the calls
    this run's fence permitted a start or an answer for, keyed by the conversation and call ids the start's answer and
    the acceptance bind together. `add` is judged per invitee like a start. A call not in the registry is `Partial` and
    denied.
  - **No call roster is read and the roster isn't watched.** **Media** (TURN, ICE, DTLS-SRTP) isn't classified: it
    carries a call the signaling fence permitted.
  - **Not settled yet, so Task 2 proposes it**: a decline (judged like an answer), a join of a running call (judged
    like a start in that chat), and what the callee's client sends by itself while a call rings (judged like an
    answer, on an invitation the fence would let it answer) in the lab.
- **Every write goes through the fence** as M1 and M2 built it: the UI calls the engine, the engine the outbox, the
  outbox `Fence::check`. A call write is a sealed `Mutating` type in `teams-proto`, sent only as `Authorized<_>`; a
  frame (a call's delivery response) only as a typed `OwnFrame`. No view builds a request or opens a socket.
- **Out of scope**: meetings (M4), screen and content sharing (M4), `add` in the personal build (but the echo
  service's), a second call while in one (call waiting: a second invitation isn't offered and rings on the user's
  other clients), hardware video encoding (unless Task 21's CPU numbers call for it), PSTN, transfer, call park,
  voicemail, recording and captions (M5), calls with anyone outside the lab in the lab build, the calendar (M4).

## Context (from M2)
- M2 is done (`docs/plans/completed/20261021-native-teams-m2-gpui-daily-driver.md`, `docs/m2-acceptance.md`). What
  exists and M3 builds on:
  - **The app** (`teams-app`, lab; `teams`, personal) on `teams-ui`: the bridge (the engine owned by the bridge, its
    events drained into GPUI models, views on the `Core` trait), the shell, chat list, chat view, compose, the chat
    switcher, notifications and the tray over `zbus` on the bus the app is given, settings, the view trace, the
    diagnostic log (`teams-log`), own presence as the app's policy.
  - **The UI sandbox** (`ui-sandbox`): a nested niri on a headless weston, a private D-Bus bus with a fake notification
    server and tray watcher, virtual keyboard and pointer, a no-op opener. Every GUI test and lab run happens there; a
    live test runs one sandbox per app.
  - **The credential broker**: a lab app reaches its tokens only through `teams-app secrets` outside the sandbox.
  - **The engine**: one per account (the engine lock), writes through the outbox (durable rows for chat writes, the
    transient kind for typing and the account's own availability), the push channel (`teams-trouter`: the messaging
    stack's socket and `TeamsCDLWebWorker` registration, deliveries never answered, an incoming call ignored; a token
    renewal reconnects the socket before expiry).
  - **The lab fence** (`teams_fence::contacting`, shared by the browser fence and the native transport): the calling
    family has one row, the test call's start in its captured shape (`contacting/call.rs`, mapped only with the echo
    service's MRI); every other mutating request on a calling host is `Unreadable`. `Scope::Call`, `WriteKind::Call`,
    `WriteKind::CallAnswer` and `WriteKind::Invite` exist; `AddToCall` is `Partial` (no source reads a call's members).
    The frame rule denies every Trouter delivery response (socket.io type `3`).
  - **The personal build** (`teams`, `docs/personal-build.md`): its fence permits only messages, edits, deletes,
    reactions, typing, read receipts and the account's own availability; its transport refuses every plain request
    (`Refusal::Personal`), sends its registration as an `OwnState` entry (`Transport::send_own`) and logs its Trouter
    frames (`FrameSink::logging`); incoming calls are ignored there, and the user's other clients still ring. Agents
    compile and mock-test the `teams` crate only, never a runnable personal binary.
- **From M0** (`docs/protocol/calling-echo.md`, `docs/spikes/echo-call.md`): the web client's test call captured (the
  1:1 chat with the bot, the start on the flight proxy with `X-Skypetoken`, the conversation answer, the acceptance
  with the media controller's `application/sdp-ngc-1.0` answer, the follow-ups, the `add` of the echo bot, the
  `leave`); the TURN credentials from `GET https://teams.microsoft.com/trap/tokens` and the one relay host;
  `spikes/media-loop` (two `str0m` peers, Opus, a Goertzel check); `spikes/echo-call` (a TURN client tested against
  coturn, `str0m` through the relay, the signaling built from the capture: the call start from an
  `Authorized<StartCall>`, the callback links, the acceptance, the `add`; `call::start` sends nothing);
  `spikes/gpui-video` (9 × 720p at 30 fps only with the external-surfaces patch, which the vendored GPUI carries).
- **What M2 learned that shapes M3**:
  - **Drive the app by the keyboard and read it by its trace**, in a sandbox per app, never by screen coordinates. The
    debug build takes keystrokes in more slowly than the virtual keyboard types them
    (`docs/backlog/debug-build-keystroke-lag.md`): a live step waits for the app's own trace line, not for a timer.
  - **Measure from the traces' clock** (send → shown p95 1.85 s, of which the post is most). Call setup and media
    timing get the same treatment: one clock on one machine, p50/p95/max over a run.
  - **The machine is loaded** (load average 17–75): timing claims carry headroom, a live step that misses its bound is
    reported with the load, and a bound is never loosened to pass. Media timing runs from a release build.
  - **Fixtures cover a field's whole captured range** (M1's two live-only bugs). A call's ids, SDP and candidates
    vary between runs and stacks; the fixture of a shape covers every form the captures show.
  - **A held-back request is captured first, mapped second** (M2's own-presence research): a scenario holds the write
    back with a labelled forced deny, the capture shows its shape, a row maps exactly that shape, and a later run lets
    it through.
  - **New dependencies pass the reach audit** (`crates/teams-ui/tests/reach.rs`): no HTTP or WebSocket client in an app
    crate's host tree, licences an Apache-2.0/MIT project can take, no logger but `teams-log`, and a crate that logs at
    `debug` or below is checked against the compile-time cap.
- **Owed into M3** (`docs/backlog/`): `fence-call-follow-ups.md` (worth: yes, M3: Tasks 3–6). Unchanged and outside
  M3: `ams-image-sending.md`, `file-sending.md`, `channel-writes-need-a-lab-only-team.md`,
  `bot-invokes-and-message-extensions.md` (M5), `own-presence-idle-and-offline-shapes.md`,
  `one-time-code-sign-in-under-the-fence.md`, `sanitizer-defence-in-depth-known-gaps.md`,
  `webkit-sign-in-guard-redirects.md`, `seeded-own-rules-ignore-the-query.md`,
  `containment-kill-cannot-order-an-unread-eof.md` (accepted), `debug-build-keystroke-lag.md`.
- Lab: a, b, c licensed and signed in, the echo service's MRI in `lab.toml`, the a–b–c group chat in `chats.toml`.

## Development Approach
- **testing approach**: TDD (tests first)
- complete each task fully before moving to the next; small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task, success and error paths
- **CRITICAL: all tests must pass before starting the next task**
- **CRITICAL: every task ends with the full gate** (`cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  --all-features -- -D warnings`, `scripts/lint-report.sh`, `cargo test --workspace`); "the gate passes" means all four
- **CRITICAL: update this plan file when scope changes during implementation**
- **a call write is mapped only from a capture**, in exactly its captured shape, with a case in
  `crates/teams-fence/tests/contacting.rs` or its fixtures; a write the fence can't tie to a target with certainty is
  denied. A response or delivery shape is handled only once a capture showed it.
- **a hard-rule change is written into CLAUDE.md first**, in the words of the user's decision, before any code that
  relies on it: the lab's decline and join, and the personal build's calls, their wording approved by the user in
  Task 2. CLAUDE.md gets current rules only (no dates, no task numbers).
- **no call is answered or declined by a test or a scenario unless its invitation names only lab accounts or the echo
  service**, and only in the scenario or live test that exists to do it. Until Task 6 maps answering, every answer
  stays denied, as today.
- **no window, bus, audio server, camera or opener of the user's desktop** for any lab or test process:
  - every GUI and media test of the app runs in the UI sandbox, which grows a private PipeWire with a tone source, a
    recorded sink and a fake camera (Task 8); the lab app refuses any PipeWire but the sandbox's as it refuses a
    window outside it;
  - `teams-headless` opens no audio server at all: its media goes through the device seam's file backend (a tone or
    WAV in, a recording out; Task 8);
  - the armed browser plays nothing aloud: a call scenario runs Chrome with its output muted (`--mute-audio`) and its
    fake devices, and reads received audio in the page (WebAudio and `getStats()`; Task 4);
  - which PipeWire a build may open is fixed when it is built (the lab and test-support builds: the sandbox's only;
    the personal build: the desktop's), never by an argument: `test-support` always wins (the sandbox's only, even
    with `personal` on, as `cargo test --workspace` and `clippy --all-features` build it), and `teams-media`'s
    `personal` feature is enabled only by `crates/teams` (the manifest test extended); `cargo test` of any crate opens
    no PipeWire but the sandbox's and no portal.
- **the personal build is the user's**: agents compile and mock-test the `teams` crate only (`docs/personal-build.md`);
  its real calls are the user's checklist (Task 23). Until Task 22 it keeps calling off (no calling socket, no
  registration, no call UI), pinned by a `teams` mock test, so the user's daily driver never shows a call it can't
  make while M3's shared crates land.
- **test-only reach stays behind `test-support`**; the lab and personal binaries refuse to run with it. No write ever
  comes from a `cargo test` process: a live test spawns normally built lab binaries (`teams-app`, `teams-headless`,
  and `proto-lab run <scenario>` for a web client's side).
- **captures and recordings stay outside the repo**: captures under the lab's `captures/`, live screenshots and audio
  recordings under `<data_dir>/screenshots/` and `<data_dir>/recordings/` (`0700`). Fixtures are synthetic-from-capture
  and pass `cargo test -p fixture-guard`.
- live runs come from a normal build (`--target-dir target/lab`; `--release` for media timing), under the live lock
  with an outer `timeout --signal=INT`, one native engine per lab account's store and one browser per lab profile at
  a time (an account's web client and its native client may be up together, as two endpoints); nothing is left
  running. A breached browser run is handled as CLAUDE.md says.
- "decide with Codex" means a `codex-discussion`; its outcome is recorded in the task (and in `docs/protocol/` when it
  settles a protocol question).
- protocol notes go to `docs/protocol/` in our own words; every endpoint touched gets its row in `endpoints.md`.
- every version written (a crate, a toolchain) is looked up live at the time, never taken from memory.

## Testing Strategy
- **unit**: `cargo test --workspace`, offline. Signaling on synthetic fixtures shaped like the captures; SDP conversion
  both ways on the captured offer and answer's shapes; the media path on loopback (two native peers through our TURN
  test server, a Goertzel check of a tone, frames counted); echo cancellation on a loop of the sink back into the
  source (the echo suppressed by a set margin, a near-end tone kept while both talk).
- **fence suite**: the calling rows (`crates/teams-fence/tests/contacting.rs`), the registry (a follow-up permitted only
  on a registered call, its keys bound only as the capture binds them, a call of another run or conversation host
  `Partial`), answering and declining (a lab-only invitation permitted, one naming anyone else denied and never
  answered), joining (in a lab-only chat), `add` per invitee; the personal fence's call rule (Task 22); `fence-seal`
  cases for each new sealed write and frame; the browser fence's frame rule letting through only a typed delivery
  response of a registered or offered call as the captures show it.
- **mock**: `teams-mock` grows a calling service (the start, the conversation answer, the acceptance delivered over
  Trouter to the calling stack's endpoint, the follow-ups, an incoming call's invitation, a lab-only one and one naming
  an outsider, the ring's end: declined, cancelled by the caller, answered elsewhere, timed out; a running call to
  join) and a loopback media peer (a `str0m` peer that answers our offer and offers to us, at a call's start and in a
  renegotiation, in `sdp-ngc-1.0`).
- **whole app, offline**: the app in the UI sandbox on the mock, two or three accounts, each with its own engine, fence
  and `fence.log`, the sandbox's private PipeWire playing a tone into each app, recording what each plays and showing
  each a fake camera; driven by the keyboard and the notification's actions, read by the trace and the recordings.
- **live lab**: `proto-lab run echo-call`, `ab-call`, `abc-call` (the web client on every end); `proto-lab run
  ab-call-native` (b's web client and a's native client, both directions) and `abc-call-mixed` (a native, b and c
  web); `teams-headless … call` (native on every end); `crates/teams-app/tests/call_live.rs` (`--features live`,
  `PROTO_LAB_LIVE=1`): the app for a, b and c in sandboxes of their own, as `ui_live` does, and the app against b's
  web client run by a spawned `proto-lab run ab-call-app`.
- **measurements**: call setup (the user's action → the media connected), the echo's round trip, a → b tone latency
  (the tone's onset at a's source → its onset in b's recording, one clock), video frames shown per second, CPU per
  process in a 1:1 video call, each p50/p95/max, with the load average beside them.
- **personal build**: tested only on the mock; nothing runs it against the tenant but the user, whose checklist
  (Task 23) is its live proof.

## Progress Tracking
- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

```
teams-app / teams ── teams-ui (call and join, ringing, in-call view, devices) ── EngineHandle
                                                                                    │
teams-core (engine: call actions and events; the call session) ──▶ teams-call (signaling state machine)
      │ outbox: every call write a fresh Fence::check             │ media session on its own threads ▼
      ▼                                                           teams-media (TURN, str0m, Opus, echo cancellation,
teams-proto (sealed call writes, Service::Calling, trap tokens)      H.264, devices: PipeWire or files)
teams-trouter (the calling stack's socket + SkypeSpacesWeb registration, typed delivery responses)
teams-fence (calling rows + the call registry, shared with proto-lab's browser fence; the personal call rule)
ui-sandbox (+ a private PipeWire: a tone source, a recorded sink, a fake camera)
```

- **`teams-fence`**: the calling family's rows and the **call registry** (`CallRegistry`), owned by the fence of a run:
  a start, a join or an answer the fence permitted registers its call; the keys are bound only as the captures bind
  them (the start's answer gives the conversation controller URL; the acceptance or a delivered `conversationUpdate`
  ties the call controller's call id to it). A follow-up names its registered call by those keys or is `Partial`. An
  invitation the fence would let the account answer is **offered**; a decline is judged on an offered invitation. The
  browser fence feeds it from what it observes (the start's answer, the invitation, the acceptance and
  `conversationUpdate` deliveries on the page's Trouter sockets); the native client feeds it from its own signaling.
  The personal fence gets the call rule the user approves (Task 2) on the same registry.
- **`teams-call`** (new, from `spikes/echo-call`'s signaling): the call's state machine (outgoing, incoming, ringing,
  connecting, connected, ended; the ring's ends: declined, cancelled, answered elsewhere, timed out), the callback
  links, the start, the join and the conversation answer, the acceptance and its acknowledgement, the decline, `add`,
  the follow-ups, keep-alives (`callKeepAliveInterval`), `leave`; it builds sealed writes and hands them to the
  engine; it never sends a request itself.
- **`teams-media`** (new, from `spikes/media-loop` and `spikes/echo-call`): the TURN client, `str0m` through the relay,
  the `sdp-ngc-1.0` conversion, Opus with a jitter buffer and loss concealment, echo cancellation, noise suppression
  and gain control (`webrtc-audio-processing`), the device seam (PipeWire for audio and the camera; a file backend
  for `teams-headless` and tests), H.264 encode and decode, frames handed to GPUI's external surfaces. Its media runs
  on threads of its own, never on the engine's runtime.
- **`teams-core`**: the call session joins them: on the caller's side `str0m`'s offer into the start's or the join's
  media content and the answer applied; on the callee's side whatever the captured invitation and acceptance carry
  (the caller's offer read, our answer written, or as Tasks 5–6 show); a renegotiation offered by either side; the
  TURN credentials fetched into the peer; the native signaling fed into the fence's `CallRegistry`; the session's
  state turned into `Event::Call`.
- **`teams-proto`** grows the sealed call writes (`StartCall` with callees, `JoinCall`, `AcceptCall`, `DeclineCall`,
  `AcknowledgeAcceptance`, `ApplyChannelParameters`, `UpdateEndpointMetadata`, `AddToCall`, `LeaveCall`, and what the
  captures add), a `Service::Calling` (the flight proxy's base and the `ep/<host>` URLs the service hands out, each
  checked against it), the TURN credentials read, the calling registration's own-state entry for the personal build,
  and the typed call delivery responses (`OwnFrame`).
- **`teams-trouter`** grows the calling stack's own socket and `SkypeSpacesWeb` registration (in the lab a plain
  request, it reaches nobody; in the personal build its `OwnState` entry through `Transport::send_own`), deliveries at
  the call agent's links handed to `teams-call`, an incoming call's invitation handed on for the fence's judgement, a
  reconnect during a call that keeps the call's links, and still no answer to any delivery but the typed ones the
  captures show.
- **`teams-ui`**: a call button in a chat's header and the switcher ("Call", "Test call"), "Join" on a chat with a
  running call, the incoming call (a ringtone, a notification with Answer and Decline on the bus the app is given, and
  a banner in the window, the caller's name as the invitation gives it), the in-call view (participants for display,
  mute, camera, hang up, duration, devices, video tiles, who is speaking), the devices in the settings, the call's
  trace lines.

## Technical Details
- **The registry's keys** are the open design point (Task 3, decided with Codex): the start's answer carries the
  conversation controller URL; the call controller's call id (`callAcceptance/<id>`, `mcProxy/<id>`) is tied to the
  conversation only by the acceptance callback or a delivered `conversationUpdate`
  (`activeModalities.call.links.participants`). A follow-up is permitted only when the URL it goes to names a
  registered conversation or a call id bound to one. The browser fence sees responses and received frames only if it
  asks: the start's answer through a response-stage pause for that one URL pattern, the deliveries through the page's
  received Trouter frames; an observation it can't read binds nothing, so the follow-ups stay denied.
- **Answering and declining**: the invitation (a `callNotification` delivered to the calling stack's endpoint) names
  the caller and the participants; the fence judges `WriteKind::CallAnswer` on `Scope::Call` by those MRIs alone (no
  roster read), and a decline the same way. The native client asks the fence before it offers Answer and Decline: an
  invitation the fence wouldn't permit is ignored (logged by kind, never shown, nothing sent for it). The engine never
  answers or declines by itself: each goes out only on an explicit `Action::Answer` or `Action::Decline` for an
  invitation the engine offered (`Event::IncomingCall`), and each one is a fresh `Fence::check`. In the app
  (`teams-app`, `teams`) that action is only the user's (the banner or the notification). The one other sender is a lab
  binary's opt-in flag, `teams-headless <alias> watch --answer-lab-calls`, which sends that same `Action::Answer` for
  an invitation the engine offered; the flag lives in `teams-headless` alone (no path to it from `teams-core`,
  `teams-call`, `teams-ui`, `teams-app` or `teams`), and the fence still judges every answer it sends, so nothing
  outside the lab is answered. What the callee's client sends by itself while a call rings (as Task 5 captures it) is
  judged like an answer and sent only for an offered invitation, with no click (both builds, once Task 2's wording is
  approved). One call at a time: an invitation arriving during a call isn't offered. A ring ends on
  the delivery that ends it (cancelled, answered elsewhere, timed out) as the captures show it.
- **Joining a running call**: a chat's running call (as its thread or a delivery shows it, captured in Task 6) is
  joined by a join
  write judged like a start in that chat: in the lab, a chat whose fresh member read lists only lab accounts; in the
  personal build, a chat whose fresh read lists the account. A permitted join registers its call.
- **A start is tied to its chat by what it carries**: the chat the start names (`groupChat.threadId` or what Task 5's
  captures show) or, for a 1:1 start that names only `to[]`, the two accounts' 1:1 chat as the chat service names it;
  the fence reads that chat, never one taken from the engine's context.
- **The personal build's calls** (the user's decisions of 2026-10-24; the wording approved in Task 2): a start or a
  join only from the user's action in an existing chat whose one fresh unpaged thread read lists the account, every
  callee the start names in that read; an answer or a decline of any invitation addressed to the account, only on the
  user's click; follow-ups on the registry as the lab's, sent by the call's own machinery (keep-alives,
  acknowledgements, channel parameters, renegotiation, `leave` on a failure) only on a call the user started, joined
  or answered, and what the client sends by itself while an offered invitation rings; no `add` but the echo
  service's. The test call: the 1:1 chat of the account and the echo service
  created, the start in the test call's shape, the `add` of exactly that bot (its MRI a constant in `teams-proto`,
  from the bundle's `teamsEchoBotMri`). The calling registration as an `OwnState` entry; the calling socket's frames
  logged (`FrameSink::logging`); media unclassified as in the lab. The microphone opens only once the user started,
  joined or answered a call; the camera is off when a call starts and the user turns it on; the camera comes through
  the desktop portal (its permission asked once by the desktop). Every call write logged to `fence.log` as chat writes
  are. A caller from outside the account's chats is shown by the name the invitation gives (no profile lookup for
  them).
- **Call writes through the outbox**: each one a fresh `Fence::check` and its `fence.log` line, and never resent after
  an unknown outcome (a call controller acting twice is worse than a failed step). Whether they are the transient kind
  (no row) or a kind of their own is decided in Task 14 with Codex; either way the state machine decides what a failed
  step means (usually: end the call locally and `leave`).
- **A call outlives a reconnect**: the calling socket's token renewal or a dropped socket reconnects without ending the
  call; whether the endpoint re-registers and whether the call's links change is read from Task 6's capture of a
  call held past a dropped socket; the socket's side is tested in Task 12, the call surviving in Task 14, and a long
  call across a token renewal is on the user's checklist (Task 23).
- **Media reach**: the native peer gathers only relayed candidates through the relay host the TURN credentials name
  (no host candidates: no local addresses in an offer), and sends media only through that relay to the candidates the
  media controller's answer names. TURN over TCP 443 when UDP fails; TURN over TLS only if a capture shows it is
  needed. Media isn't classified (the hard rules); its sockets are `teams-media`'s own, never the HTTP transport's.
- **Interop with Microsoft's stack**: the native client takes the web client's place in a call (its endpoint type,
  its SDP shapes), so a colleague's client sees what it sees from a web client. The lab proves it against the web
  client in the armed browser, both directions, audio and video, 1:1 and in a mixed group (Tasks 16, 18, 19, 21);
  native ↔ native alone could hide a bug both ends share. The desktop clients' own stacks the lab can't run; the
  user's checklist (Task 23) covers them.
- **SDP**: `str0m` speaks standard SDP; the call controller and media controller speak `application/sdp-ngc-1.0`.
  `teams-media` converts both ways (`RTP/SAVP` with inline candidates and real ports, `a=rtcp`, `b=CT`,
  `a=x-ssrc-range`, `a=x-signaling-fb`, `a=label`, the answer's `m=x-data` dropped unless Task 6 shows group video
  needs it) from the captured shapes, audio first, then the video lines the captures show.
- **Devices: PipeWire, or files.** One backend for audio and the camera (the camera portal hands over a PipeWire node,
  so one server serves both): the app uses the PipeWire its build allows (the sandbox's for the lab app and tests, the
  desktop's for the personal build). The sandbox runs a private PipeWire (its own runtime dir, nothing of the
  desktop's) with a source playing a given tone, a sink recorded to a file and a fake camera showing a moving test
  pattern, and a loopback that can feed a sink back into a source (the echo-cancellation check). `teams-headless`
  uses the file backend (a tone or WAV in, a recording out; a generated test pattern as its camera, received frames
  counted and summarised on its output) and opens no audio server. The
  crate (`pipewire`, `cpal` the fallback) is confirmed in Task 8 (versions looked up live, its licence, threads and
  logging checked as the reach audit does).
- **Audio processing**: `webrtc-audio-processing` (AEC3, noise suppression, gain control) on 10 ms frames at 48 kHz,
  the played signal fed to it as the far end; Opus as the capture configures it (in-band FEC), a jitter buffer and
  Opus's own loss concealment. The ringtone plays on the default output (Task 17).
- **Devices in use** (Task 20): the settings list microphones, speakers and cameras, "follow the system" by default; a
  device switched mid-call without dropping the call; a headset plugged in or out moves the call when following the
  system.
- **Video**: H.264 as the media controller's answer selects it (the capture's), through `openh264` (software), at the
  resolution and bitrate the answer allows, the bitrate following `str0m`'s bandwidth estimate. Its licence and the
  patent question (Cisco's patent cover is for its prebuilt binary) are settled with Codex in Task 18 and written
  down. Hardware encoding (VA-API) waits for Task 21's CPU numbers. Frames go to GPUI through the external-surfaces
  patch (`spikes/gpui-video`).
- **⚠️ Group video sources**: how a group call's participant chooses whose video it receives (the answer's `m=x-data`
  and a data channel, or the SDP and signaling alone) isn't captured yet; Task 6's `abc-call` with video on captures
  it, and Task 19 builds what it shows.
- **The incoming-call policy stays fail-closed**: anything not a readable invitation the fence permits answering is
  ignored; a delivery is answered only with a typed response the captures show for a registered or offered call; the
  browser frame rule lets through only those.
- **Fixtures of calls hold made-up network data**: documentation addresses (`192.0.2.0/24`, `198.51.100.0/24`,
  `203.0.113.0/24`, `2001:db8::/32`), made-up ICE ufrag and password, DTLS fingerprints and TURN credentials; the
  fixture guard learns to flag real ones first (Task 1).

## What Goes Where
- **Implementation Steps**: code, tests and docs in this repo, plus lab commands the agent runs on its own.
- **Post-Completion**: steps that need a human or a decision.

## Implementation Steps

### Task 1: The fixture guard knows calls
**Files:**
- Modify: `crates/fixture-guard/src/*`, `crates/fixture-guard/tests/*`

- [x] tests first: the guard flags a public IPv4 or IPv6 address outside the documentation ranges, an ICE
      `a=ice-ufrag`/`a=ice-pwd` pair, a DTLS `a=fingerprint`, a TURN `username`/`password` pair and a `trap/tokens`
      answer's credential in any `tests/fixtures/` or `docs/`, and passes the documentation ranges, placeholders and
      Rust paths (`teams_fence::contacting`, `a::b`) that look like IPv6
- [x] the existing calling fixtures (`tests/fixtures/calling/echo/`, `spikes/echo-call/tests/fixtures/`) checked and
      rewritten where they need it (checked: documentation addresses, loopback and placeholders only; none needed a
      rewrite)
- [x] `cargo test -p fixture-guard` and the gate pass

### Task 2: The call rules' wording: the proposal, then the user approves it
**Files:**
- Modify: `docs/personal-build.md`, `docs/backlog/fence-call-follow-ups.md` (the lab's proposal), CLAUDE.md (on
  approval)

- [ ] the lab's additions, proposed as CLAUDE.md text in `docs/backlog/fence-call-follow-ups.md`: a decline judged
      like an answer (an invitation naming only lab accounts or the echo service); a join of a running call judged
      like a start in that chat (a chat whose fresh member read lists only lab accounts); what the callee's client
      sends by itself while a call rings, judged like an answer, on an offered invitation, with no click
- [ ] the personal build's calls written into `docs/personal-build.md` as the user decided on 2026-10-24 (Technical
      Details, "The personal build's calls"): the writes (start, join, answer, decline, follow-ups; `add` only of the
      echo service); to whom (a start or join in a chat whose fresh thread read lists the account, every callee in it;
      an answer or decline of any invitation addressed to the account); how a start is tied to its chat (Technical
      Details); the test call's exception (the chat with the bot, the start's shape, the bot's `add`, the MRI's source);
      the follow-ups the call sends by itself on a call the user started, joined or answered, and what it sends by
      itself while an offered invitation rings, as exceptions to "only from an explicit action"; the calling
      registration as an `OwnState` entry and its frames logged; media unclassified; one call at a time; the microphone
      and the camera (off at start, the desktop portal); the `fence.log` lines; what the lab can't show (colleagues'
      desktop clients, callers outside the tenant) and how Task 23 covers it; the doc's "calls" row
- [ ] the CLAUDE.md text the personal build's calls need, as a proposal in the doc (current rules only: no dates, no
      task numbers)
- [ ] ⚠️ ask the user to approve the wording. The run doesn't wait: Tasks 3–21 go on while it is pending, captures
      included (a held-back write sends nothing). What waits for it: Task 6's mapping and let-through of the decline,
      the join and the ring-time sends; the live steps that need them (Task 16's late join, Task 19's late join,
      Task 21's decline and rejoin), run once it is in; and all of Task 22. On approval CLAUDE.md gets the approved
      text
- [ ] `cargo test -p fixture-guard` passes

### Task 3: The call registry and the echo call's follow-ups
**Files:**
- Modify: `crates/teams-fence/src/{contacting/call.rs,decision.rs,…}`, `crates/teams-fence/tests/*`,
  `crates/proto-lab/src/{classify.rs,fence_http.rs,fence_ws.rs}`, `crates/fence-seal/tests/ui/*`,
  `docs/protocol/{calling-echo.md,fence-targets.md,endpoints.md}`, CLAUDE.md

- [ ] decide with Codex: the registry's keys and how each is bound (the start's answer, the acceptance, a
      `conversationUpdate`), how the browser fence observes them (a response-stage pause for the start's URL, the
      page's received Trouter frames), offered invitations, and what a registered call's end does
- [ ] tests first: `CallRegistry` in `teams-fence`: a permitted start registers its conversation once its answer is
      read; a call id is bound only as the capture binds it; a follow-up on a registered call is permitted, on another
      conversation, host or run `Partial`; an observation that doesn't read binds nothing
- [ ] tests first: the follow-ups' rows from the M0 capture (`updateEndpointMetadata`, `applyChannelParameters`,
      `acknowledge`, `leave`) and `add` judged per invitee (the echo service permitted, anyone else denied); the
      acknowledgement's delivery response over Trouter as a typed frame for a registered call only (the browser's frame
      rule lets exactly that through, every other type `3` stays denied)
- [ ] the browser fence feeds the registry; `fence-seal` cases for the new types
- [ ] CLAUDE.md's patterns ("The sealed fence API", the frame rule) updated with the code; `endpoints.md` rows
- [ ] `cargo test -p teams-fence -p proto-lab -p fence-seal` and the gate pass

### Task 4: The echo call completes in the browser (live)
**Files:**
- Modify: `crates/proto-lab/src/{browser.rs,scenario/echo_call.rs}`, `docs/protocol/calling-echo.md`,
  `tests/fixtures/calling/echo/*`

- [ ] tests first: a call scenario's Chrome runs with its output muted (`--mute-audio`) besides its fake devices, and
      the scenario reads received audio in the page (WebAudio taps and `getStats()`), never from a speaker
- [ ] `proto-lab run echo-call --headless`: the start permitted, the acceptance delivery observed and binding the call
      id, the `add`, the acknowledgement and the `leave` permitted on the registered call; the echo joins and the tone
      comes back (`getStats()` and a Goertzel check of the received audio in the page); the run exits 0 with only its
      expected denies
- [ ] the capture exported to a reference copy; the shapes the M0 capture lacked (the echo joining, its roster update,
      `mediaRenegotiation`, the call's end) written into `docs/protocol/calling-echo.md` and as synthetic fixtures
- [ ] `ab-chat` and `abc-chat` still exit 0; the gate passes

### Task 5: Lab-to-lab calls in the browser (capture the starts, map them, capture the ring)
**Files:**
- Create: `crates/proto-lab/src/scenario/{ab_call.rs,abc_call.rs}`
- Modify: `crates/proto-lab/src/scenario/expected.rs`, `crates/teams-fence/src/contacting/call.rs`,
  `crates/teams-fence/tests/*`, `docs/protocol/calling.md` (new), `docs/protocol/fence-targets.md`, `endpoints.md`,
  CLAUDE.md

The fence maps only the echo call's start today, so a start to b or c is denied before anyone is invited: the starts
are captured denied first, mapped, and only then can a rerun reach the invitations, the ring and the answers.

- [ ] capture the starts: `ab-call` (a calls b from their 1:1 chat, audio, then video) and `abc-call` (a calls the
      recorded group chat, audio, then video), each start held back by a labelled forced deny (nothing reaches b or
      c); the start to a lab callee (`to[]`) and the group start's shape captured, and what each names of its chat
      (`groupChat.threadId` or only `to[]`)
- [ ] tests first: the start to lab callees and in a lab-only group chat mapped from those captures (`Scope::Call` with
      its `to[]`, the chat it names read fresh, Technical Details "A start is tied to its chat"), denied with any other
      callee; a permitted start registers its call as the echo call's does
- [ ] CLAUDE.md's "Live runs" line on answering rewritten before the scenarios click answer or decline, for what the
      code then does: a scenario clicks answer or decline only on an invitation naming only lab accounts or the echo
      service, and only in a scenario that exists to do it; the fence doesn't map `acceptance` or a decline and denies
      Trouter delivery responses, so such a click is held back by a labelled forced deny; every other incoming call is
      ignored, never clicked; the native Trouter client still has no path to answer any delivery
- [ ] capture the ring: `ab-call` rerun with the start let through; b's page receives the invitation; everything b's
      page sends between the invitation and an answer (delivery responses, progress posts) held back by labelled
      forced denies and captured; b clicks answer and the answer (the `acceptance` and its delivery response) is held
      back, so the call rings and ends unanswered; again with b clicking decline (held back); again with a hanging up
      while b rings (the caller's cancel as b receives it); again with nobody acting (the ring's timeout); `abc-call`
      the same with b's and c's answers held back
- [ ] the shapes written up in `docs/protocol/calling.md` (our own words): the start with callees and in a group, the
      invitation as delivered, what the callee sends while ringing, the answer, the decline, the ring's ends; fixtures
      synthetic, covering every form the runs show
- [ ] the gate passes

### Task 6: Answering, declining and joining lab-to-lab calls (map, then live)
**Files:**
- Modify: `crates/proto-lab/src/scenario/{ab_call.rs,abc_call.rs,expected.rs}`, `crates/teams-fence/src/*`,
  `crates/teams-fence/tests/*`, `docs/protocol/{calling.md,fence-targets.md,endpoints.md}`, CLAUDE.md

- [ ] tests first: answering judged by the invitation's caller and participants, permitted only when all are lab
      accounts or the echo service; the typed delivery responses for that call; a registered answered call's
      follow-ups; how the invitation and the acceptance carry media (the caller's offer, our answer, or as captured)
      written into `docs/protocol/calling.md` for Task 7
- [ ] `ab-call` and `abc-call` let the answers through: b (and c) answer by the scenario's click, the call connects,
      audio flows both ways (`getStats()`), every participant leaves; exit 0
- [ ] both reruns with video on: the video follow-ups (`controlVideoStreaming`, `mediaRenegotiation` each way)
      captured, then mapped on registered calls, tests first (cases in `contacting.rs`); in `abc-call`, how a
      participant chooses whose video it receives (the `m=x-data` line, a data channel's messages, or signaling alone)
      captured and written into `docs/protocol/calling.md` (Technical Details, "Group video sources")
- [ ] capture the join (no approval needed: nothing is sent): `abc-call` with a and b connected; how c's page learns
      the group chat has a running call (a thread property, a delivery, a call event message) captured; c clicks Join
      and the join is held back by a labelled forced deny; both written into `docs/protocol/calling.md`
- [ ] capture a call held past a dropped socket: `ab-call` connected, the scenario closes b's calling Trouter socket
      (and, if the run lasts, lets a token renewal pass); what b's page re-sends, whether it re-registers and whether
      the call's links change, written into `docs/protocol/calling.md`
- [ ] once Task 2's lab wording is approved and in CLAUDE.md, tests first, then live: the callee's ring-time sends
      (Task 5's capture) mapped on an offered lab-only invitation; the decline mapped on an offered lab-only
      invitation (denied for any other) and `ab-call`'s decline run let through (a sees it declined, b's ring ends);
      the join mapped as a start in that chat and let through, a permitted join registering its call
- [ ] CLAUDE.md's "Live runs" line rewritten for what the code now does (still: nothing outside the lab is answered or
      declined, no scenario clicks either on any other call); `endpoints.md`, `fence-targets.md`
- [ ] the gate passes

### Task 7: `sdp-ngc-1.0` both ways
**Files:**
- Create: `crates/teams-media/{Cargo.toml,src/sdp.rs,…}` (from `spikes/echo-call`, `spikes/media-loop`)

- [ ] tests first, the caller's side: `str0m`'s offer (audio only, one relayed candidate) written as `sdp-ngc-1.0`
      matches the captured offer's shape line for line where it matters; the media controller's answer read back as
      standard SDP that `str0m` accepts
- [ ] tests first, the callee's side, in the shapes Tasks 5–6 captured: the media an invitation or acceptance carries
      read into `str0m` (a remote offer), our answer written back as `sdp-ngc-1.0`; a mid-call renegotiation offered
      by us and offered to us
- [ ] tests first: a browser SDP refused where `sdp-ngc-1.0` is required; the video lines the captures show, and
      `m=x-data` as Task 6 found it
- [ ] the spikes' TURN client and relay moved into `teams-media` with their tests (coturn's external test kept)
- [ ] the gate passes

### Task 8: The device seam and the sandbox's PipeWire
**Files:**
- Modify: `crates/teams-media/src/*`, `crates/ui-sandbox/src/*`

- [ ] confirm the PipeWire crate (`pipewire`; `cpal` the fallback) (versions looked up live; licence, threads and
      logging checked as the reach audit does); decide with Codex if the two are close
- [ ] tests first: the device seam (`Devices`): a PipeWire backend and a file backend (a tone or WAV in, a recording
      out, a generated test pattern as the camera, received frames counted; no audio server); which PipeWire a build
      may open fixed at build time (Development Approach: `test-support` wins; `teams-media`'s `personal` feature only
      from `crates/teams`, the manifest test extended); a build with both `personal` and `test-support` on refuses the
      desktop's PipeWire and Pulse sockets; a lab build refuses any PipeWire but the sandbox's
- [ ] tests first: the sandbox runs a private PipeWire (its own runtime dir, nothing of the desktop's) with a source
      playing a given tone, a sink recorded to a file, a fake camera (a moving test pattern) and a loopback feeding a
      sink back into a source with a set delay and attenuation; a canary on the
      session's own PipeWire and Pulse sockets proves no sandboxed process reaches them
- [ ] the gate passes

### Task 9: Opus and the relay on loopback
**Files:**
- Modify: `crates/teams-media/src/*`

- [ ] tests first: Opus encode and decode as the capture configures it (in-band FEC) with a jitter buffer and loss
      concealment; two native peers on loopback through the TURN test server carry a tone both ways (Goertzel) over
      the file backend, with loss and reordering injected
- [ ] the media session runs on threads of its own (never the engine's runtime), with a bounded handoff to and from the
      device seam
- [ ] the gate passes

### Task 10: Echo cancellation, noise suppression and gain control
**Files:**
- Modify: `crates/teams-media/src/*`

- [ ] confirm `webrtc-audio-processing` (version looked up live; licence, its C++ build needs and logging checked as
      the reach audit does)
- [ ] tests first: the sink looped back into the source with a delay and attenuation, the echo suppressed by a set
      margin (dB, written in the test); a near-end tone kept while both talk; noise under a tone reduced; the played
      signal fed as the far end on the same 10 ms frames
- [ ] the gate passes

### Task 11: The sealed call writes and the calling service
**Files:**
- Modify: `crates/teams-proto/src/*`, `crates/teams-proto/tests/*`, `crates/fence-seal/tests/ui/*`

- [ ] tests first: the sealed writes from the captures (`StartCall`, `JoinCall`, `AcceptCall`, `DeclineCall`,
      `AcknowledgeAcceptance`, `ApplyChannelParameters`, `UpdateEndpointMetadata`, `AddToCall`, `LeaveCall`, the video
      follow-ups), each only as `Authorized<_>`; `Service::Calling` and every `ep/<host>` URL checked against its base;
      the trap tokens read; the typed delivery responses (`OwnFrame`) the captures show
- [ ] `fence-seal` cases for each new type
- [ ] the gate passes

### Task 12: The calling stack's Trouter socket
**Files:**
- Modify: `crates/teams-trouter/src/*`, `crates/teams-trouter/tests/*`, `crates/teams-mock/src/*`

- [ ] tests first: `teams-trouter` opens the calling stack's socket and registers `SkypeSpacesWeb` (a plain request in
      the lab; the personal build's `OwnState` entry through `Transport::send_own`, its frames through
      `FrameSink::logging`); the call agent's deliveries reach their consumer; an invitation goes to the fence's
      judgement; no delivery is answered but with a typed response for a registered or offered call
- [ ] tests first on the mock: a token renewal or a dropped socket reconnects the calling socket; the re-registration
      and the call's links as Task 6's capture shows them
- [ ] tests first (`teams`' lib on the mock): the personal build opens no calling socket and sends no calling
      registration until Task 22 turns calling on
- [ ] the gate passes

### Task 13: `teams-call` on the mock
**Files:**
- Create: `crates/teams-call/*`
- Modify: `crates/teams-mock/src/*` (the calling service and the loopback media peer)

- [ ] tests first on the mock: an outgoing call through start → acceptance → acknowledgement → connected → leave; a
      join of a running call; an incoming lab-only call answered by an action, declined by an action, one naming an
      outsider ignored and never answered (nothing sent for it, no request or frame); an offered invitation nobody acts
      on stays unanswered (no answer without `Action::Answer`); the ring's ends (cancelled, answered elsewhere, timed
      out) end the ringing; an invitation during a call not offered; keep-alives on `callKeepAliveInterval`
- [ ] `teams-call` builds sealed writes and hands them on; it sends nothing itself
- [ ] the gate passes

### Task 14: The call session in the engine
**Files:**
- Modify: `crates/teams-core/src/*`, `crates/teams-core/tests/*`, `spikes/echo-call/*`, README.md

- [ ] decide with Codex: call writes as the outbox's transient kind or a kind of their own; what a failed step does
- [ ] tests first on the mock and its loopback media peer: the call session joins `teams-call` and `teams-media`
      (outgoing: `str0m`'s offer into the start's or the join's media content, the answer applied; incoming: the
      invitation's and acceptance's media as Task 7 reads and writes them; a renegotiation either way; the TURN
      credentials fetched into the peer); a call connects and a tone crosses on the file backend, both directions;
      every write with its `fence.log` line
- [ ] tests first: the native signaling feeds the fence's `CallRegistry`: a native start's answer registers its call;
      the acceptance or a `conversationUpdate` delivery binds the call id as the capture binds it; a follow-up on an
      unbound id is refused before it is sent; an answer or delivery that doesn't read binds nothing
- [ ] tests first: a call survives the calling socket's reconnect (Task 12) and keeps its media
- [ ] engine: `Action::{Call, Join, Answer, Decline, Hangup, Mute, Camera, …}`, `Event::{IncomingCall, RingEnded,
      Call(state)}`; one call at a time
- [ ] `spikes/echo-call` reduced to what isn't moved (or removed, its README row and docs updated)
- [ ] the gate passes

### Task 15: The native echo call (live)
**Files:**
- Modify: `crates/teams-headless/src/*`, `crates/teams-headless/tests/*`, `docs/protocol/calling-echo.md`

- [ ] tests first (on the mock): `teams-headless <alias> call <target>` (`echo`, a lab alias or a recorded chat) places
      the call through the engine on the file backend (`--tone`, `--record`, `--seconds`), and ends it
- [ ] `teams-headless a call echo --seconds 20` (release lab build): the tone goes out, comes back, and is checked
      (Goertzel); the round trip measured (p50/p95/max; how the echo re-times playback written up)
- [ ] `fence.log`: the start, the `add`, the follow-ups, all on the registered call; nothing denied; no answer to any
      delivery but the acknowledgement's typed response
- [ ] the open questions of `docs/spikes/echo-call.md` answered in `docs/protocol/calling-echo.md` (what the start
      needs, TCP framing, the relay transport used, re-timing)
- [ ] the gate passes

### Task 16: Native lab-to-lab audio, against itself and the web client (live)
**Files:**
- Create: `crates/proto-lab/src/scenario/ab_call_native.rs`
- Modify: `crates/teams-headless/src/*`, `crates/teams-headless/tests/*`

- [ ] tests first (`teams-headless`, on the mock): `watch --answer-lab-calls` sends `Action::Answer` for an
      invitation the engine offers and nothing for one it ignores (an outsider's: no request, no frame); without the
      flag nothing is answered; the flag exists in `teams-headless` alone (Technical Details, "Answering and
      declining")
- [ ] `teams-headless a call b` (release lab build), with `teams-headless b watch --answer-lab-calls` answering only
      a lab-only invitation the engine offered: the call connects; a tone each way is recorded and checked on the
      other side; setup time and tone latency measured; both leave
- [ ] the same for the a–b–c group chat (`teams-headless a call abc`, b and c answering), and, once Task 2's wording
      is in, c joining late (`teams-headless c join abc`)
- [ ] `proto-lab run ab-call-native`: b's web client in the armed browser and a's native client (the release lab
      `teams-headless`, spawned by the scenario), both directions: a calls b and b's page answers by the scenario's
      click; b's page calls a and `watch --answer-lab-calls` answers; the tone checked on both sides (in the page,
      and in the native recording); exit 0
- [ ] answered elsewhere: b's web client and b's native client up together (two endpoints of b); a calls b, b's page
      answers, b's native client's ring ends on the delivery
- [ ] `fence.log` as Task 15's; the gate passes

### Task 17: Calls in the app
**Files:**
- Modify: `crates/teams-ui/src/*`, `crates/teams-app/src/*`, `crates/teams-ui/tests/*`, `crates/teams-app/tests/*`

- [ ] tests first (fake `Core`): a call button in a chat's header and the switcher's "Call" and "Test call"; "Join" on
      a chat with a running call; the incoming call's ringtone (on the default output), banner and notification
      (Answer, Decline) only for an invitation the engine offers, the caller's name as the invitation gives it, the
      ring ending on `Event::RingEnded`; the in-call view (participants for display, mute, hang up, duration, the
      call's state and failures); trace lines for each (ids and kinds, never names); the personal build shows no call
      UI until Task 22 (a `teams` mock test)
- [ ] keyboard paths for each (the live test drives them)
- [ ] the window test on the mock in the sandbox: a calls b, b's ringtone is in b's recording, b answers from the
      notification's action, the tone crosses both ways in the sandbox's recordings, a hangs up; b declines a second
      call and a sees it declined
- [ ] the gate passes

### Task 18: Video, 1:1
**Files:**
- Modify: `crates/teams-media/src/*`, `crates/teams-ui/src/*`, `crates/teams-ui/tests/*`, `docs/protocol/calling.md`

- [ ] confirm `openh264` for H.264 as the answers select it (versions looked up live); decide with Codex its licence
      and the patent question (Cisco's cover is for its prebuilt binary) and write the outcome down; hardware encoding
      stays out unless Task 21's numbers call for it
- [ ] tests first: the sandbox's fake camera; encode, send, receive and decode on loopback through the relay, the
      bitrate following `str0m`'s bandwidth estimate; frames reach GPUI's external surface (the trace counts them)
- [ ] the in-call view's video tiles and camera toggle (off when a call starts); the video follow-ups on registered
      calls as Task 6 mapped them
- [ ] live: `proto-lab run ab-call-native` with video, both directions (frames decoded in the page's `getStats()`;
      the page's test pattern received by `teams-headless`, counted on its output)
- [ ] the gate passes

### Task 19: Group calls and group video
**Files:**
- Create: `crates/proto-lab/src/scenario/abc_call_mixed.rs`
- Modify: `crates/teams-media/src/*`, `crates/teams-core/src/*`, `crates/teams-ui/src/*`, `crates/teams-ui/tests/*`

- [ ] tests first: group video sources as Task 6 captured them (the data channel and its messages if that is how,
      otherwise the signaling); up to 9 tiles as the M0 spike measured
- [ ] tests first on the mock: a call in the a–b–c group chat; b and c answer; a participant who leaves; c joining
      late; the in-call view's participant grid and who is speaking (from the media's audio levels)
- [ ] live: `teams-headless` a, b and c in the group chat with video, each receiving the others' frames (counted on
      its output); `proto-lab run abc-call-mixed` (a native, b and c web) with audio and video among all three; once
      Task 2's wording is in, c leaving and joining late
- [ ] the gate passes

### Task 20: Real devices
**Files:**
- Modify: `crates/teams-media/src/*`, `crates/teams-ui/src/*`, `crates/ui-sandbox/src/*`

- [ ] tests first: the settings list microphones, speakers and cameras from PipeWire, "follow the system" by default;
      a device switched mid-call without dropping the call (the tone continues in the recording); a device added or
      removed while a call runs (the sandbox adds and removes a node), the call moving when following the system
- [ ] the camera through the desktop portal behind a seam (the sandbox's fake camera in lab runs; the portal's node in
      the personal build), tested on a fake portal on the sandbox's bus
- [ ] the gate passes

### Task 21: Verify the lab acceptance
**Files:**
- Create: `crates/teams-app/tests/call_live.rs` (`required-features = ["live"]`),
  `crates/proto-lab/src/scenario/ab_call_app.rs`, `docs/m3-acceptance.md`

- [ ] the live test is an orchestrator only, as `ui_live` is: a sandbox per app with its private PipeWire and fake
      camera, each app's broker outside, the normally built (release) app for a, b and c, keyboard only, traces,
      recordings and `fence.log`; b's web side, where a case needs it, is a spawned normally built `proto-lab run
      ab-call-app` (wait and answer, or call), coordinated by the app's trace lines; no browser and no write in the
      `cargo test` process
- [ ] (`PROTO_LAB_LIVE=1`): a's test call returns its tone; a calls b, b's ringtone plays, b answers from the
      notification, the tone crosses both ways, video frames shown both ways, a switches its microphone mid-call, a
      hangs up; b declines a call and a sees it declined; a calls the group chat, b and c answer, audio and video among
      all three, c leaves and joins again, a ends it; the app on a and b's web client call each other both ways with
      audio and video
- [ ] the echo-cancellation check through the whole app: b's sink looped into b's source by the sandbox's loopback
      (Task 8), a hears no echo of itself beyond the margin
- [ ] numbers: setup time, the echo's round trip, a → b tone latency, video frames per second, CPU per process in a
      1:1 video call (p50/p95/max, load average beside them)
- [ ] `fence.log`: writes only on calls the fence registered or offered and the lab chats; no deny of a mapped write;
      no answer or decline outside the lab; screenshots and recordings under the lab's data dir, never the repo
- [ ] `ui_live`, `chat_live`, `ab-chat` and `abc-chat` still pass; the gate passes; `docs/m3-acceptance.md` (numbers,
      the fence's picture, flakes, any breached run next to its clean rerun)

### Task 22: The personal build's calls (after Task 2's approval)
**Files:**
- Modify: `crates/teams/src/*`, `crates/teams/tests/*`, `crates/teams-fence/src/*`, `crates/teams-fence/tests/*`,
  `crates/teams-proto/src/*`, `crates/teams-trouter/src/*`, `crates/teams-core/src/*`, `crates/teams-media/src/*`,
  `crates/teams-call/src/*`, `docs/personal-build.md`

- [ ] CLAUDE.md holds Task 2's approved text before this task's code
- [ ] every plain request a lab call sends (the `allowed` lines in Tasks 15, 16 and 21's `fence.log`, such as the
      calling policy's read) listed, and each given a personal route (a `GET`, an `OwnState` entry or a `PostRead`) or
      shown unneeded; tests first: a personal call on the mock completes with no `Refusal::Personal`
- [ ] tests first on the mock (`teams`' lib): a start or a join in a chat whose fresh thread read lists the account
      permitted; a start naming a callee outside that read denied; a start in a chat whose read doesn't list the
      account denied; the test call (the bot's chat, the start, the bot's `add`) permitted and an `add` of anyone else
      denied; an outsider's invitation offered, answered or declined only on the action, never without it, shown by the
      invitation's name; the call's own follow-ups only on a call the user started, joined or answered; an invitation
      during a call not offered; the calling registration as an `OwnState` entry and no plain request; the calling
      socket's frames logged; the camera off when a call starts; the microphone opened only after a start, a join or an
      answer; every call write with its `fence.log` line
- [ ] the personal build on the desktop's PipeWire and the camera portal (fixed at build time, Task 8); `cargo test -p
      teams` opens neither (the seam's fake); agents build no runnable personal binary
- [ ] `docs/personal-build.md`'s "calls" row and the first-run screen's wording, if it lists what the build sends
- [ ] `cargo test -p teams -p teams-fence -p teams-call -p teams-ui -p fence-seal` and the gate pass

### Task 23: The user's real-call checklist
**Files:**
- Modify: `docs/m3-acceptance.md`

- [ ] the checklist written into `docs/m3-acceptance.md`, each step with what the user should see and what to send
      back if it fails (the `fence.log` line; the diagnostic log at `debug`, which the user reads, never an agent):
  - the test call;
  - a call to a lab account through its 1:1 chat, when no lab live run holds the lock (it rings there and the lab
    ignores it; the user may answer it on a device of their own signed in as that account);
  - a 1:1 audio call with a colleague, each way; video on both sides, the camera off at start and turned on;
  - a group-chat call; joining a running group-chat call;
  - a colleague's call declined in the app (the colleague sees it declined);
  - a microphone or headset switched mid-call;
  - an incoming call from someone outside the user's chats, rung and answered;
  - a call answered on the user's phone (the app stops ringing);
  - a call longer than an hour, across a token renewal, that stays up;
  - echo heard by the colleague or not
- [ ] ⚠️ stop here for the user's run; each reported problem fixed (tests first, on the mock or in the lab) and the
      step rerun by the user before Task 24
- [ ] the outcome recorded in `docs/m3-acceptance.md` (the user's report, in their words where they give them)

### Task 24: [Final] Update documentation
- [ ] README.md: the M3 crates, the calling scenarios, `teams-headless … call`, `join` and `watch --answer-lab-calls`,
      the live call test, the sandbox's PipeWire and camera, calls in the personal build
- [ ] CLAUDE.md: the patterns M3 settled (the call registry, the calling rows, answering, declining and joining, the
      media's reach, the device seam and which PipeWire each build opens, the personal build's calls), current rules
      only
- [ ] `docs/protocol/endpoints.md` checked against every endpoint M3 touched
- [ ] write the M4 plan (meetings and the calendar), informed by M3
- [ ] `cargo test -p fixture-guard` and the gate pass
- [ ] move this plan to `docs/plans/completed/`

## Validation commands

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/lint-report.sh
PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1 cargo test --workspace
cargo test -p fixture-guard
cargo build -p proto-lab -p teams-headless -p teams-app --target-dir target/lab
cargo build --release -p teams-headless -p teams-app --target-dir target/lab
target/lab/debug/proto-lab run echo-call --headless
target/lab/debug/proto-lab run ab-call --headless
target/lab/debug/proto-lab run abc-call --headless
target/lab/debug/proto-lab run ab-call-native --headless
target/lab/debug/proto-lab run abc-call-mixed --headless
target/lab/release/teams-headless a call echo --seconds 20
TEAMS_APP_BIN=$PWD/target/lab/release/teams-app PROTO_LAB_LIVE=1 \
  cargo test -p teams-app --features live --test call_live
```

Every live command above runs under `flock ~/.local/share/teams-lab/live.lock` with an outer `timeout --signal=INT`.
The personal build is never built for or run against the tenant by an agent.

## Roadmap after M3
- **M4 — meetings**: joining scheduled meetings by their link, the lobby, the meeting roster, the meeting chat, a
  chat's meeting card, screen and content sharing (viewing first, then sharing through the desktop portal); **the
  calendar** (Graph's calendar view, read-only: an agenda with join links), moved out of M2 for the daily driver being
  about chats.
- **M5 — long tail**: breakout rooms, live captions, the recording indicator, background blur (ONNX), PSTN,
  `teams-webhost` (WebKitGTK + a JS SDK shim: tabs, Loop, Whiteboard), and bots and message extensions with Adaptive
  Cards and their actions (`docs/backlog/bot-invokes-and-message-extensions.md`: the invokes carry the account's token
  to the bot's backend and stay denied until a fence rule decides the target and the token).
- **Work that waits for evidence or the user** (any milestone that needs it): image and file sending
  (`docs/backlog/ams-image-sending.md`, `file-sending.md`); channel writes if the user approves a team of the lab
  accounts only (`docs/backlog/channel-writes-need-a-lab-only-team.md`); starting new chats and the server's search,
  in the personal build only; `add` and call waiting in the personal build.
- **Cross-cutting**: drift detection (a scheduled bundle fetch and a diff report; it also watches `teamsEchoBotMri`),
  the sanitizer's known gaps, `login`'s one-time-code prompt, the debug build's keystroke lag, hardware video encoding
  (VA-API) if Task 21's CPU numbers call for it.

## Post-Completion
*Items requiring a human or a decision — informational only.*

**Manual steps:**
- Lab: none expected. If Microsoft asks a lab account for a one-time code, `login` stops and the user is asked.
- The user's real calls are Task 23, inside the plan.
- Before Task 23, if the user wants to call the lab accounts: a 1:1 chat with each (a, b, c) created once from the
  user's own client.

**Decisions for the user:**
- **The call rules' wording** (Task 2): the lab's decline and join, and the personal build's calls.
- **A lab-only team** (backlog): still open; meetings in a channel (M4) would need it too.
- **Hardware video encoding**, if Task 21's CPU numbers make software H.264 too heavy for daily calls.
