# Native Teams for Linux — M3: Calls

## Overview
- **Goal: audio and video calls in the app, between the lab accounts first.** M3 maps the call writes under the lab
  rule (a call is a contacting write whose participants are its caller, its callees and whoever it adds), completes
  the echo call the fence stopped in M0, then builds native calling on M2's app: a call started from a chat, an
  incoming call answered by an explicit action, the in-call view (mute, hang up, devices), 1:1 and group audio, and
  video. Everything is proven on the lab (the web client in the armed browser, then the native client under the
  fence, then the app in the UI sandbox); the personal build gets calls only under rules the user approves (Tasks
  15–16).
- **Exit:** the echo call returns its tone natively; a native a → b call and an a–b–c group call carry audio both ways
  and video, live, from the app in the UI sandbox (Task 14); `fence.log` shows writes only on calls the fence itself
  permitted; no call outside the lab is ever answered. The personal build's call rules are approved or left out by the
  user (Task 15).
- **What the hard rules already settle** (CLAUDE.md, "Calls go only to the lab, both ways", and
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
- **Every write goes through the fence** as M1 and M2 built it: the UI calls the engine, the engine the outbox, the
  outbox `Fence::check`. A call write is a sealed `Mutating` type in `teams-proto`, sent only as `Authorized<_>`; a
  frame (a call's delivery response) only as a typed `OwnFrame`. No view builds a request or opens a socket.
- **Out of scope**: meetings (joining scheduled meetings, lobby, the meeting roster, the meeting chat: M4), screen and
  content sharing (M4), PSTN, transfer, call park, voicemail, recording and captions (M5), calls with anyone outside
  the lab in the lab build, the calendar (M4).

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
    stack's socket and `TeamsCDLWebWorker` registration, deliveries never answered, an incoming call ignored).
  - **The lab fence** (`teams_fence::contacting`, shared by the browser fence and the native transport): the calling
    family has one row, the test call's start in its captured shape (`contacting/call.rs`, mapped only with the echo
    service's MRI); every other mutating request on a calling host is `Unreadable`. `Scope::Call`, `WriteKind::Call`,
    `WriteKind::CallAnswer` and `WriteKind::Invite` exist; `AddToCall` is `Partial` (no source reads a call's members).
    The frame rule denies every Trouter delivery response (socket.io type `3`).
  - **The personal build** (`teams`, `docs/personal-build.md`): its fence permits only messages, edits, deletes,
    reactions, typing, read receipts and the account's own availability; incoming calls are ignored there, and the
    user's other clients still ring.
- **From M0** (`docs/protocol/calling-echo.md`, `docs/spikes/echo-call.md`): the web client's test call captured (the
  start on the flight proxy with `X-Skypetoken`, the conversation answer, the acceptance with the media controller's
  `application/sdp-ngc-1.0` answer, the follow-ups, the `add` of the echo bot, the `leave`); the TURN credentials from
  `GET https://teams.microsoft.com/trap/tokens` and the one relay host; `spikes/media-loop` (two `str0m` peers, Opus,
  a Goertzel check); `spikes/echo-call` (a TURN client tested against coturn, `str0m` through the relay, the signaling
  built from the capture: the call start from an `Authorized<StartCall>`, the callback links, the acceptance, the
  `add`; `call::start` sends nothing); `spikes/gpui-video` (9 × 720p at 30 fps only with the external-surfaces patch,
  which the vendored GPUI carries).
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
- **Owed into M3** (`docs/backlog/`): `fence-call-follow-ups.md` (worth: yes, M3: Tasks 2–5). Unchanged and outside
  M3: `ams-image-sending.md`, `file-sending.md`, `channel-writes-need-a-lab-only-team.md`,
  `bot-invokes-and-message-extensions.md` (M5), `own-presence-idle-and-offline-shapes.md`,
  `one-time-code-sign-in-under-the-fence.md`, `sanitizer-defence-in-depth-known-gaps.md`,
  `webkit-sign-in-guard-redirects.md`, `containment-kill-cannot-order-an-unread-eof.md` (accepted),
  `debug-build-keystroke-lag.md`.
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
  relies on it. M3 needs none for the lab (the rules above already say it); the personal build's calls need one
  (Task 15).
- **no call is answered by a test or a scenario unless its invitation names only lab accounts or the echo service**,
  and only in the scenario or live test that exists to answer it. Until Task 5 maps answering, every answer stays
  denied, as today.
- **no window, bus, audio server, camera or opener of the user's desktop** for any lab or test process: every GUI and
  media test runs in the UI sandbox, which grows a private audio server (Task 7); the lab app refuses to open audio
  devices outside it as it refuses a window outside it.
- **test-only reach stays behind `test-support`**; the lab and personal binaries refuse to run with it. No write ever
  comes from a `cargo test` process: a live test spawns normally built lab binaries.
- **captures and recordings stay outside the repo**: captures under the lab's `captures/`, live screenshots and audio
  recordings under `<data_dir>/screenshots/` and `<data_dir>/recordings/` (`0700`). Fixtures are synthetic-from-capture
  and pass `cargo test -p fixture-guard`.
- live runs come from a normal build (`--target-dir target/lab`; `--release` for media timing), under the live lock
  with an outer `timeout --signal=INT`, one process per lab account and one browser per lab profile at a time; nothing
  is left running. A breached browser run is handled as CLAUDE.md says.
- "decide with Codex" means a `codex-discussion`; its outcome is recorded in the task (and in `docs/protocol/` when it
  settles a protocol question).
- protocol notes go to `docs/protocol/` in our own words; every endpoint touched gets its row in `endpoints.md`.
- every version written (a crate, a toolchain) is looked up live at the time, never taken from memory.

## Testing Strategy
- **unit**: `cargo test --workspace`, offline. Signaling on synthetic fixtures shaped like the captures; SDP conversion
  both ways on the captured offer and answer's shapes; the media path on loopback (two native peers through our TURN
  test server, a Goertzel check of a tone, frames counted).
- **fence suite**: the calling rows (`crates/teams-fence/tests/contacting.rs`), the registry (a follow-up permitted only
  on a registered call, its keys bound only as the capture binds them, a call of another run or conversation host
  `Partial`), answering (a lab-only invitation permitted, one naming anyone else denied and never answered), `add` per
  invitee; `fence-seal` cases for each new sealed write and frame; the browser fence's frame rule letting through only
  a typed delivery response of a registered call.
- **mock**: `teams-mock` grows a calling service (the start, the conversation answer, the acceptance delivered over
  Trouter to the calling stack's endpoint, the follow-ups, an incoming call's invitation, a lab-only one and one naming
  an outsider) and a loopback media controller (a `str0m` peer answering in `sdp-ngc-1.0`).
- **whole app, offline**: the app in the UI sandbox on the mock, two or three accounts, each with its own engine, fence
  and `fence.log`, the sandbox's private audio server playing a tone into each app and recording what each plays;
  driven by the keyboard and the notification's actions, read by the trace and the recordings.
- **live lab**: `proto-lab run echo-call`, `ab-call`, `abc-call` (the web client); `teams-headless … call` (native);
  `crates/teams-app/tests/call_live.rs` (`--features live`, `PROTO_LAB_LIVE=1`): the app for a, b and c in sandboxes
  of their own, as `ui_live` does.
- **measurements**: call setup (the user's action → the media connected), the echo's round trip, a → b tone latency
  (the tone's onset at a's source → its onset in b's recording, one clock), video frames shown per second, each
  p50/p95/max.
- **personal build**: tested only on the mock; nothing runs it against the tenant but the user.

## Progress Tracking
- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

```
teams-app / teams ── teams-ui (call button, incoming call, in-call view, devices) ── EngineHandle
                                                                                        │
teams-core (engine: call actions and events) ──▶ teams-call (signaling state machine) ──┤
      │ outbox: every call write a fresh Fence::check                                    │
      ▼                                                                                  ▼
teams-proto (sealed call writes, Service::Calling, trap tokens)   teams-media (TURN, str0m, Opus, video, devices)
teams-trouter (the calling stack's socket + SkypeSpacesWeb registration, typed delivery response)
teams-fence (calling rows + the call registry, shared with proto-lab's browser fence)
ui-sandbox (+ a private audio server and a fake camera for tests and lab runs)
```

- **`teams-fence`**: the calling family's rows and the **call registry** (`CallRegistry`), owned by the fence of a run:
  a start or an answer the fence permitted registers its call; the keys are bound only as the captures bind them (the
  start's answer gives the conversation controller URL; the acceptance or a delivered `conversationUpdate` ties the
  call controller's call id to it). A follow-up names its registered call by those keys or is `Partial`. The browser
  fence feeds it from what it observes (the start's answer, the acceptance and `conversationUpdate` deliveries on the
  page's Trouter sockets); the native client feeds it from its own signaling.
- **`teams-call`** (new, from `spikes/echo-call`'s signaling): the call's state machine (outgoing, incoming, ringing,
  connecting, connected, ended), the callback links, the start and conversation answer, the acceptance and its
  acknowledgement, `add`, the follow-ups, keep-alives (`callKeepAliveInterval`), `leave`; it builds sealed writes and
  hands them to the engine; it never sends a request itself.
- **`teams-media`** (new, from `spikes/media-loop` and `spikes/echo-call`): the TURN client, `str0m` through the relay,
  the `sdp-ngc-1.0` conversion, Opus with a jitter buffer and loss concealment, the audio device seam (a real backend
  on the audio server the process is given; the sandbox's private one in tests and lab runs), video capture, encode,
  decode and frames handed to GPUI's external surfaces.
- **`teams-proto`** grows the sealed call writes (`StartCall` with callees, `AcceptCall`, `AcknowledgeAcceptance`,
  `ApplyChannelParameters`, `UpdateEndpointMetadata`, `AddToCall`, `LeaveCall`, and what the captures add), a
  `Service::Calling` (the flight proxy's base and the `ep/<host>` URLs the service hands out, each checked against it),
  the TURN credentials read, and the typed call delivery response (`OwnFrame`).
- **`teams-trouter`** grows the calling stack's own socket and `SkypeSpacesWeb` registration (a plain request, it
  reaches nobody), deliveries at the call agent's links handed to `teams-call`, an incoming call's invitation handed on
  for the fence's judgement, and still no answer to any delivery but the typed one for a registered call.
- **`teams-ui`**: a call button in a chat's header and the switcher ("Call", "Test call"), the incoming call (a
  notification with Answer and Decline on the bus the app is given, and a banner in the window), the in-call view
  (participants for display, mute, camera, hang up, duration, devices), the call's trace lines.

## Technical Details
- **The registry's keys** are the open design point (Task 2, decided with Codex): the start's answer carries the
  conversation controller URL; the call controller's call id (`callAcceptance/<id>`, `mcProxy/<id>`) is tied to the
  conversation only by the acceptance callback or a delivered `conversationUpdate`
  (`activeModalities.call.links.participants`). A follow-up is permitted only when the URL it goes to names a
  registered conversation or a call id bound to one. The browser fence sees responses and received frames only if it
  asks: the start's answer through a response-stage pause for that one URL pattern, the deliveries through the page's
  received Trouter frames; an observation it can't read binds nothing, so the follow-ups stay denied.
- **Answering**: the invitation (a `callNotification` delivered to the calling stack's endpoint) names the caller and
  the participants; the fence judges `WriteKind::CallAnswer` on `Scope::Call` by those MRIs alone (no roster read).
  The native client asks the fence before it offers Answer: an invitation the fence wouldn't permit is ignored
  (logged by kind, never shown as answerable), and Answer is only ever a user's action, never automatic.
- **Call writes through the outbox**: each one a fresh `Fence::check` and its `fence.log` line, and never resent after
  an unknown outcome (a call controller acting twice is worse than a failed step). Whether they are the transient kind
  (no row) or a kind of their own is decided in Task 8 with Codex; either way the state machine decides what a failed
  step means (usually: end the call locally and `leave`).
- **Media reach**: the native peer gathers only relayed candidates through the relay host the TURN credentials name
  (no host candidates: no local addresses in an offer), and sends media only through that relay to the candidates the
  media controller's answer names. TURN over TCP 443 when UDP fails; TURN over TLS only if a capture shows it is
  needed. Media isn't classified (the hard rules); its sockets are `teams-media`'s own, never the HTTP transport's.
- **SDP**: `str0m` speaks standard SDP; the call controller and media controller speak `application/sdp-ngc-1.0`.
  `teams-media` converts both ways (`RTP/SAVP` with inline candidates and real ports, `a=rtcp`, `b=CT`,
  `a=x-ssrc-range`, `a=x-signaling-fb`, `a=label`, the answer's `m=x-data` dropped) from the captured shapes, audio
  first, then the video lines the captures show.
- **Audio devices**: a seam (`AudioDevices`) with one real backend on the audio server named by the process's
  environment; the sandbox runs a private one (a null sink recorded to a file, a source playing a given tone) on a
  socket in its own runtime dir, and a lab app refuses to open audio outside the sandbox. The crate is chosen in Task 7
  (versions looked up live, its licence and its logging checked as the reach audit does).
- **Video**: the codec and its crate are chosen in Task 12 from what the media controller's answer selects (H.264 in
  the capture), with the licence checked; frames go to GPUI through the external-surfaces patch (`spikes/gpui-video`).
  In the sandbox the camera is a fake source (a moving test pattern); the personal build's camera goes through the
  desktop portal, decided with Task 15.
- **The incoming-call policy stays fail-closed**: anything not a readable lab-only invitation is ignored; a delivery is
  answered only with the typed response for a registered call; the browser frame rule lets through only that one.
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

- [ ] tests first: the guard flags a public IPv4 or IPv6 address outside the documentation ranges, an ICE
      `a=ice-ufrag`/`a=ice-pwd` pair, a DTLS `a=fingerprint`, a TURN `username`/`password` pair and a `trap/tokens`
      answer's credential in any `tests/fixtures/` or `docs/`, and passes the documentation ranges and placeholders
- [ ] the existing calling fixtures (`tests/fixtures/calling/echo/`, `spikes/echo-call/tests/fixtures/`) checked and
      rewritten where they need it
- [ ] `cargo test -p fixture-guard` and the gate pass

### Task 2: The call registry and the echo call's follow-ups
**Files:**
- Modify: `crates/teams-fence/src/{contacting/call.rs,decision.rs,…}`, `crates/teams-fence/tests/*`,
  `crates/proto-lab/src/{classify.rs,fence_http.rs,fence_ws.rs}`, `crates/fence-seal/tests/ui/*`,
  `docs/protocol/{calling-echo.md,fence-targets.md,endpoints.md}`, CLAUDE.md

- [ ] decide with Codex: the registry's keys and how each is bound (the start's answer, the acceptance, a
      `conversationUpdate`), how the browser fence observes them (a response-stage pause for the start's URL, the
      page's received Trouter frames), and what a registered call's end does
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

### Task 3: The echo call completes in the browser (live)
- [ ] `proto-lab run echo-call --headless`: the start, the acceptance, the `add`, the acknowledgement and the `leave`
      permitted on the registered call; the echo joins and the tone comes back (the page's `getStats()` and a Goertzel
      check of the received audio); the run exits 0 with only its expected denies
- [ ] the capture exported to a reference copy; the shapes the M0 capture lacked (the echo joining, its roster update,
      `mediaRenegotiation`, the call's end) written into `docs/protocol/calling-echo.md` and as synthetic fixtures
- [ ] `ab-chat` and `abc-chat` still exit 0; the gate passes

### Task 4: Lab-to-lab calls in the browser (capture)
**Files:**
- Create: `crates/proto-lab/src/scenario/{ab_call.rs,abc_call.rs}`
- Modify: `crates/proto-lab/src/scenario/expected.rs`, `docs/protocol/calling.md` (new), `endpoints.md`

- [ ] `ab-call`: a calls b from their 1:1 chat (audio, then video); b's page receives the invitation; b's answer (the
      `acceptance` and its delivery response) is held back by a labelled forced deny, so the call rings and ends
      unanswered; the start to a lab callee (`to[]`) captured
- [ ] `abc-call`: a calls the recorded group chat; the group start's shape captured; b's and c's answers held back
- [ ] the shapes written up in `docs/protocol/calling.md` (our own words): the start with callees and in a group, the
      invitation as delivered, the answer, what the callee sends after; fixtures synthetic, covering every form the
      runs show
- [ ] the gate passes

### Task 5: Starting and answering lab-to-lab calls (map, then live)
- [ ] tests first: the start to lab callees and in a lab-only group chat (`Scope::Call` with its `to[]`, the group's
      fresh member read where the start names a chat), denied with any other callee; answering judged by the
      invitation's caller and participants, permitted only when all are lab accounts or the echo service; the typed
      delivery response for that call; a registered answered call's follow-ups
- [ ] `ab-call` and `abc-call` let the answers through: b (and c) answer by the scenario's click, the call connects,
      audio flows both ways (`getStats()`), every participant leaves; exit 0
- [ ] CLAUDE.md's "Live runs" line on answering rewritten for what the code now does (still: nothing outside the lab
      is answered, no scenario clicks answer on any other call); `endpoints.md`, `fence-targets.md`
- [ ] the gate passes

### Task 6: `sdp-ngc-1.0` both ways
**Files:**
- Create: `crates/teams-media/{Cargo.toml,src/sdp.rs,…}` (from `spikes/echo-call`, `spikes/media-loop`)

- [ ] tests first: `str0m`'s offer (audio only, one relayed candidate) written as `sdp-ngc-1.0` matches the captured
      offer's shape line for line where it matters; the media controller's answer read back as standard SDP that
      `str0m` accepts; a browser SDP refused where `sdp-ngc-1.0` is required; the video lines the captures show
- [ ] the spikes' TURN client and relay moved into `teams-media` with their tests (coturn's external test kept)
- [ ] the gate passes

### Task 7: Media: audio, devices and the sandbox's audio server
**Files:**
- Modify: `crates/teams-media/src/*`, `crates/ui-sandbox/src/*`

- [ ] choose the audio backend crate (versions looked up live; licence, threads and logging checked as the reach audit
      does); decide with Codex if two are close
- [ ] tests first: the sandbox runs a private audio server (its own runtime dir, nothing of the desktop's), with a
      source playing a given tone and a sink recorded to a file; a canary on the session's own audio socket proves no
      sandboxed process reaches it; a lab binary refuses audio outside the sandbox
- [ ] tests first: Opus encode and decode with a jitter buffer and loss concealment; two native peers on loopback
      through the TURN test server carry a tone both ways (Goertzel), with loss and reordering injected
- [ ] the gate passes

### Task 8: Native signaling (`teams-call`) on the mock
**Files:**
- Create: `crates/teams-call/*`
- Modify: `crates/teams-proto/src/*` (sealed call writes, `Service::Calling`, trap tokens, the typed delivery
  response), `crates/teams-trouter/src/*` (the calling stack's socket and registration), `crates/teams-core/src/*`
  (call actions and events), `crates/teams-mock/src/*` (the calling service and media controller),
  `crates/fence-seal/tests/ui/*`

- [ ] decide with Codex: call writes as the outbox's transient kind or a kind of their own; what a failed step does
- [ ] tests first: the sealed writes from the captures, each only as `Authorized<_>`; every `ep/<host>` URL checked
      against the calling service's base; the trap tokens read
- [ ] tests first: `teams-trouter` opens the calling stack's socket and registers `SkypeSpacesWeb`; the call agent's
      deliveries reach `teams-call`; an invitation goes to the fence's judgement; no delivery is answered but with the
      typed response for a registered call
- [ ] tests first on the mock: an outgoing call through start → acceptance → acknowledgement → connected → leave; an
      incoming lab-only call answered by an action, one naming an outsider ignored and never answered (nothing sent for
      it, no request or frame); every write with its `fence.log` line
- [ ] engine: `Action::{Call, Answer, Decline, Hangup, Mute, …}`, `Event::{IncomingCall, Call(state)}`; one call at a
      time
- [ ] `spikes/echo-call` reduced to what isn't moved (or removed, its README row and docs updated)
- [ ] the gate passes

### Task 9: The native echo call (live)
- [ ] `teams-headless a call echo [--seconds 20]` (release lab build): the tone goes out, comes back, and is checked
      (Goertzel); the round trip measured (p50/p95/max; how the echo re-times playback written up)
- [ ] `fence.log`: the start, the `add`, the follow-ups, all on the registered call; nothing denied; no answer to any
      delivery but the acknowledgement's typed response
- [ ] the open questions of `docs/spikes/echo-call.md` answered in `docs/protocol/calling-echo.md` (what the start
      needs, TCP framing, the relay transport used, re-timing)
- [ ] the gate passes

### Task 10: Native lab-to-lab audio (live)
- [ ] `teams-headless a call b`, with `teams-headless b watch --answer-lab-calls` answering only a lab-only
      invitation: the call connects; a tone each way is recorded and checked on the other side; setup time and tone
      latency measured; both leave
- [ ] the same for the a–b–c group chat (`teams-headless a call abc`, b and c answering)
- [ ] `fence.log` as Task 9's; the gate passes

### Task 11: Calls in the app
**Files:**
- Modify: `crates/teams-ui/src/*`, `crates/teams-app/src/*`, `crates/teams-ui/tests/*`, `crates/teams-app/tests/*`

- [ ] tests first (fake `Core`): a call button in a chat's header and the switcher's "Call" and "Test call"; the
      incoming call's banner and notification (Answer, Decline) only for an invitation the engine offers; the in-call
      view (participants for display, mute, hang up, duration, the call's state and failures); devices in the
      settings; trace lines for each (ids and kinds, never names)
- [ ] keyboard paths for each (the live test drives them)
- [ ] the window test on the mock in the sandbox: a calls b, b answers from the notification's action, the tone
      crosses both ways in the sandbox's recordings, a hangs up
- [ ] the gate passes

### Task 12: Video
- [ ] choose the codec crate from what the answers select (licence checked; versions looked up live); decide with
      Codex if it touches patents or system libraries
- [ ] tests first: a fake camera source in the sandbox; encode, send, receive and decode on loopback through the relay;
      frames reach GPUI's external surface (the trace counts them); up to 9 tiles in a group call as the M0 spike
      measured
- [ ] the in-call view's video tiles and camera switch; `controlVideoStreaming` and the video follow-ups mapped from
      Task 4's capture on registered calls
- [ ] the gate passes

### Task 13: Group calls in the app
- [ ] tests first on the mock: a call in the a–b–c group chat; b and c answer; a participant who leaves; `add` of a lab
      account to a running call (per invitee); an `add` of anyone else denied
- [ ] the in-call view's participant grid and who is speaking (from the media's audio levels)
- [ ] the gate passes

### Task 14: Verify the lab acceptance
**Files:**
- Create: `crates/teams-app/tests/call_live.rs` (`required-features = ["live"]`), `docs/m3-acceptance.md`

- [ ] the live test is an orchestrator only, as `ui_live` is: a sandbox per app with its private audio server and fake
      camera, each app's broker outside, the normally built (release) app for a, b and c, keyboard only, traces,
      recordings and `fence.log`
- [ ] (`PROTO_LAB_LIVE=1`): a's test call returns its tone; a calls b, b answers from the notification, the tone
      crosses both ways, video frames shown both ways, a hangs up; a calls the group chat, b and c answer, audio among
      all three, c leaves, a ends it; an invitation b declines leaves no call
- [ ] numbers: setup time, the echo's round trip, a → b tone latency, video frames per second (p50/p95/max)
- [ ] `fence.log`: writes only on calls the fence registered and the lab chats; no deny of a mapped write; no answer
      outside the lab; screenshots and recordings under the lab's data dir, never the repo
- [ ] `ui_live`, `chat_live`, `ab-chat` and `abc-chat` still pass; the gate passes; `docs/m3-acceptance.md` (numbers,
      the fence's picture, flakes, any breached run next to its clean rerun)

### Task 15: The personal build's calls: design, then the user decides
**Files:**
- Modify: `docs/personal-build.md`

- [ ] write down calls in the personal build: which writes (start, answer, follow-ups, `add`), to whom (a call in a
      chat whose thread lists the account, by a fresh read; answering an invitation naming the account), only from an
      explicit action; the registry as the lab's; the camera and microphone through the desktop portal; what the lab
      can't show (meetings, outsiders' clients, PSTN) and what the personal build does without it
- [ ] the CLAUDE.md text it needs, as a proposal in the doc
- [ ] ⚠️ stop here for the user's decision; no code that lets the personal fence permit a call write is written before
      it; on approval CLAUDE.md gets the approved text
- [ ] `cargo test -p fixture-guard` passes

### Task 16: The personal build's calls (only after Task 15's approval)
- [ ] as the approved text says, tested only on the mock (`teams`' lib); agents build no runnable personal binary
- [ ] `cargo test -p teams -p teams-fence -p teams-call -p teams-ui -p fence-seal` and the gate pass

### Task 17: [Final] Update documentation
- [ ] README.md: the M3 crates, the calling scenarios, `teams-headless … call`, the live call test, the sandbox's audio
      server and camera
- [ ] CLAUDE.md: the patterns M3 settled (the call registry, the calling rows, answering, the media's reach, the
      sandbox's audio), current rules only
- [ ] `docs/protocol/endpoints.md` checked against every endpoint M3 touched
- [ ] write the M4 plan (meetings and the calendar), informed by M3
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
target/lab/release/teams-headless a call echo
TEAMS_APP_BIN=$PWD/target/lab/release/teams-app PROTO_LAB_LIVE=1 \
  cargo test -p teams-app --features live --test call_live
```

Every live command above runs under `flock ~/.local/share/teams-lab/live.lock` with an outer `timeout --signal=INT`.
The personal build is never built for or run against the tenant by an agent.

## Roadmap after M3
- **M4 — meetings**: joining scheduled meetings by their link, the lobby, the meeting roster, the meeting chat, screen
  and content sharing (viewing first, then sharing through the desktop portal); **the calendar** (Graph's calendar
  view, read-only: an agenda with join links), moved out of M2 for the daily driver being about chats.
- **M5 — long tail**: breakout rooms, live captions, the recording indicator, background blur (ONNX), PSTN,
  `teams-webhost` (WebKitGTK + a JS SDK shim: tabs, Loop, Whiteboard), and bots and message extensions with Adaptive
  Cards and their actions (`docs/backlog/bot-invokes-and-message-extensions.md`: the invokes carry the account's token
  to the bot's backend and stay denied until a fence rule decides the target and the token).
- **Work that waits for evidence or the user** (any milestone that needs it): image and file sending
  (`docs/backlog/ams-image-sending.md`, `file-sending.md`); channel writes if the user approves a team of the lab
  accounts only (`docs/backlog/channel-writes-need-a-lab-only-team.md`); starting new chats and the server's search,
  in the personal build only.
- **Cross-cutting**: drift detection (a scheduled bundle fetch and a diff report), the sanitizer's known gaps,
  `login`'s one-time-code prompt, the debug build's keystroke lag.

## Post-Completion
*Items requiring a human or a decision — informational only.*

**Manual steps:**
- Lab: none expected. If Microsoft asks a lab account for a one-time code, `login` stops and the user is asked.
- **The user's first call in the personal build** (after Task 16, if approved): a test call from the app, then a call
  with a colleague told beforehand. Report anything odd by its reason before relying on it.

**Decisions for the user:**
- **The personal build's calls** (Task 15): approve or change the proposal in `docs/personal-build.md` and its
  CLAUDE.md text.
- **A lab-only team** (backlog): still open; meetings in a channel (M4) would need it too.
