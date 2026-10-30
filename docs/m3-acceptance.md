# M3 acceptance check (the lab)

Date: 2026-10-26. M3 Task 21 (`docs/plans/20261024-native-teams-m3-calls.md`), on branch `native-teams-m3-calls`. Every
live command ran from a **normal release build** (`cargo build --release -p teams-app --target-dir target/lab`, the same
for `proto-lab` and `teams-headless`: no `test-support`, no `personal`), headless, under the lab's live lock (`flock
~/.local/share/teams-lab/live.lock`) with an outer `timeout --signal=INT`; one app (and one broker) per lab account, each
app in a UI sandbox of its own with its private PipeWire, one browser per lab profile in the `proto-lab` runs; nothing
was left running after any run (checked after each). Writes went only to the lab's a–b chat, the recorded a–b–c group
chat, the calls the fence registered or offered there, the echo service's test call and the lab accounts' own
availability. No call outside the lab was answered or declined. The machine was quiet (load average 0.7–1.8 during
the measured calls).

**Open: the user's real-call run.** The personal build's calls are tested on the mock only; their live proof is the
user's run of [the checklist below](#the-users-real-call-checklist-the-personal-build), whose
[outcome](#outcome) is still empty. Each problem the user reports is fixed in a follow-up and that step run again.

| # | criterion | verdict |
|---|---|---|
| 1 | a's test call returns its tone | **met** (round trip p50 16.8 s, as Task 15's native echo call) |
| 2 | a calls b; b's ringtone plays; b answers from the notification; the tone crosses both ways | **met** |
| 3 | video frames shown both ways in the 1:1 call | **met** (15 fps each way: the fake camera's rate) |
| 4 | a switches its microphone mid-call | **met** (a headset plugged into a's sandbox, followed in 1.6 s, back in 1.6 s; the call never ended) |
| 5 | a hangs up; b's call ends with it | **met** |
| 6 | b declines a call and a sees it declined | **met** (a's call ended by the service 0.4 s after b's decline; see §4) |
| 7 | a calls the group chat, b and c answer, audio and video among all three | **met** (each hears both others; each window presents the other two on two lines) |
| 8 | c leaves and joins again | **met**, slowly seen: a's grid dropped c 56 s after c's `leave` (§5, `docs/backlog/native-group-leave-seen-a-minute-late.md`); c's join connected in 1.7 s and a saw c's video again |
| 9 | a ends it | **met**: a hung up; b's and c's calls went on (the service keeps a group call up while two are in it), and each hung up after 15 s |
| 10 | the app on a and b's web client call each other both ways with audio and video | **met**, with one gap of the harness: the page's fake microphone was silent in the call it placed (§2) |
| 11 | the echo-cancellation check through the whole app | **met**: a heard nothing of itself above what the recording resolves, 45.7 dB or more under what b played (b's speaker, the loopback's 6 dB included; §3) |
| 12 | numbers: setup, the echo's round trip, a → b latency, video fps, CPU per process with the load average | **met** (§6) |
| 13 | `fence.log`: writes only on calls the fence registered or offered and in the lab chats; no deny of a mapped write; no answer or decline outside the lab | **met** (§7) |
| 14 | screenshots and recordings under the lab's data dir, never the repo | **met** (`<data_dir>/call-live/<time>/`, `<data_dir>/screenshots/<time>/`) |
| 15 | `ui_live`, `chat_live`, `ab-chat` and `abc-chat` still pass | **met** (§8; `abc-chat` passed on its third run, after a call left running in the group chat by a failed run had ended) |
| 16 | the gate | **met** (§9) |

## The live test

`crates/teams-app/tests/call_live.rs` (`--features live`, and only with `PROTO_LAB_LIVE=1`) is an orchestrator, as
`ui_live` is: it opens no fence, holds no token and sends nothing itself. The acceptance run is run 23 (09:16Z, 410 s,
every case in one process: `CALL_LIVE_PHASES` unset).

- **A UI sandbox per account** with its private PipeWire (`ui-sandbox`): its own microphone, its speaker recorded, its
  fake camera (a moving test pattern at 15 fps), and microphones plugged in and out as the cases need. a's own
  microphone plays 440 Hz steady (the tone b's page's audio tap listens for); b's and c's are silent, and b's speaker
  is looped back into b's own microphone (100 ms, 6 dB down).
- **Voices**: before the app-to-app cases each account plugs in a microphone above its sandbox's own playing its tone
  keyed (a 440, b 660, c 550 Hz, on and off in 200 ms slots of a degree-9 shift register's pattern). The call's noise
  suppression lowers a steady tone within seconds (a's 440 Hz reached b at 0.0002 after ten seconds in run 10), so a
  steady tone can't tell whether a voice still crosses.
- **The one clock**: the sandbox's recorder marks when each buffer of the speaker's recording was heard, and a looped
  microphone marks when each buffer it played went out (`ui_sandbox::pipewire::timeline`). Lining a keyed tone's
  slots up with a recording, each 2 s of it, on this one clock gives a delay per block: the echo's round trip
  (`test-call`) and a → b's latency (`one-to-one`).
- **b's web client**: `proto-lab run ab-call-app --headless --call answer|place` (the new scenario,
  `crates/proto-lab/src/scenario/ab_call_app.rs`), a normally built binary the test spawns, which arms its own browser
  fence and says how far it got on stdout (`ab-call-app: ready|in-call|heard|mic|saw|ended at <ms>`); the test keeps
  a's side in step by a's trace.
- **The keyboard only**: Ctrl+K and "Test call", Ctrl+Shift+C (call), Ctrl+Shift+S (answer), Ctrl+Shift+J (join),
  Ctrl+Shift+O (camera), Ctrl+Shift+H (hang up); b's answer and decline go through the call notification's actions on
  b's sandbox's bus, as a desktop's would.
- **What it reads**: each app's view trace (`in-call`, `call-banner`, `ringtone`, `call-notification`,
  `video-tile presented`, `call-participant`), the recordings, `/proc` for CPU, and `fence.log` from the byte the run
  began at. Outputs: traces, the apps', brokers' and page's output, the recordings and `summary.json` under
  `<data_dir>/call-live/<time>/`; no text file there holds a token (checked by the run).

## 1. The test call

a plugs in the keyed headset (990 Hz) and runs "Test call" from the switcher; connected 5.9 s after the switcher was
opened for it. That figure includes the test's own wait before Enter (the query typed, then 3 s for the switcher to
take it in): run 23 took the time before the switcher, and the test now measures from the app's `call asked` line. The
call is held 48 s; the echo service plays back what it recorded, and 8 blocks of a's recording lined up with what the
headset played: round trip p50 16 760 ms, min 16 730, p95/max 21 800 (one block matched a later slot of the pattern's
playback). Task 15's native echo call measured 16.7–16.9 s. Run 23 counted every block that lined up; the test now
takes only the delay at least three blocks agree on (within 50 ms of each other), so a block lined up elsewhere, as
that 21 800 ms one, is left out of the round trip.

## 2. The app and b's web client

- **a calls, b's page answers** (`--call answer`): a connected 5.6 s after Ctrl+Shift+C (the page's ring and its
  scripted click included); a turned its camera on and presented the page's video; the page heard a's tone 3.4 s
  after it was in the call, decoded a's video, and its own microphone played its tone (`mic`); a heard it (0.48);
  a hung up; the page's run exited 0.
- **b's page calls, a answers** (`--call place`): a's banner named b, its ringtone played (0.18 in a's recording);
  Ctrl+Shift+S connected a 1.3 s later; a presented the page's video; the page heard a and decoded a's video; the
  page hung up after 40 s and a's call ended `remote`; the page's run exited 0.
- **The gap**: in every run where the page placed the call (runs 2–7, 9, 23) and in one where it answered (run 7), the
  page's own microphone sent silence: its WebRTC audio source's level stayed 0 for the whole call, however often it
  was switched to another microphone (three more switches, its microphone button "Mute mic", i.e. unmuted), while
  Chrome's fake input played the tone file and the page heard and saw a throughout. The page then sent DTX packets
  only (5–12 a second), and a's speaker played silence. So the page → app audio is checked only when the page says
  `mic` (it did in the answering direction of run 23); the app's own reception is checked in every app-to-app case.
  The earlier `ab-call-native` runs had the same gap unseen: a native callee answering the page heard its tone only
  4–9 s of 20 (`tone hz=440 heard=true seconds=…`).

## 3. 1:1 between the apps

- **The ring**: a's Ctrl+Shift+C; b's banner named a, b's ringtone started and was in b's recording (0.19 at 784 Hz),
  b's notification came; after 3 s b answered from it; b connected 1.3 s after the answer, a 1.3 s after.
- **The voices**: a heard b's (0.14 at 660 Hz), b heard a's (0.07 at 440 Hz).
- **Echo cancellation**: b's voice unplugged (b silent, its microphone only the loopback of its speaker), a talked (a
  speech-like signal: low-passed noise in 250 ms syllables, every fourth a pause, as `teams_media::test_signal`'s far
  end); after 8 s b played a's speech at 0.0019 (its speech-band level), and a heard back 0.0000001, under the floor
  measured while a was quiet (0.000013): 45.7 dB under what b played, the margin capped at what a 16-bit recording
  resolves. The earlier measure with steady tones (runs 9–10) read 19.3 and 11.6 dB: a steady tone is noise to the
  suppression and a period of it is no echo to the canceller, and b's own steady tone was constant double talk.
  Run 23's margin was taken against b's speaker, so the loopback's 6 dB count in it; the test now takes it against
  the echo at b's microphone (what b played, 6 dB down), as Task 10's test does, and also requires b's microphone to
  have heard something before its processing at both ends of the window (b's trace, `call-participant self`
  `mic-sound`), so a dead microphone can't pass for a canceller. Neither was in run 23.
- **The microphone switched**: a headset (the keyed 990 Hz) plugged into a's sandbox above the system's; a's app
  follows the system, and b heard the headset alone 1.6 s after it was plugged in; over 20 s 9 blocks gave a → b's
  latency: p50 230 ms, min 190, p95/max 230. Unplugged, b heard a's voice alone again 1.6 s later. a's call never
  ended. (The settings' own choice of a device is checked on the mock: `call_window`, M3 Task 20.)
- **Video**: both cameras on; each window presented the other's video and its own self view, 15.0 fps each (p50; min
  14.9, max 15.1 over 20 s): the fake camera's 15 fps.
- **The end**: a hung up; a's call ended `hung-up`, b's `remote`.

## 4. The decline

a called b again; b declined from the notification 3 s into the ring; b's call ended `declined` 0.15 s later. The
service then ended a's call 0.43 s after the decline, as `remote` with code 0/5002 ("all participants left the
audio-video modality"). When the decline was captured (M3 Task 6, Task 17) the service took a declined call to b's
voicemail instead (a's bar says "Declined or not answered: voicemail"); in all of today's runs it ended the call. The
test takes either as a seeing the decline.

## 5. The group call

- a called the group chat from its header (connected 2.4 s after Ctrl+Shift+C); b answered from its notification
  (connected 2.1 s after), c by Ctrl+Shift+S (2.1 s).
- Each heard both others' voices (0.11–0.92), every camera went on, and each window presented the other two, each on
  a line of its own (`line-1`, `line-2`), 30 frames or more each.
- **c leaves**: c's Ctrl+Shift+H; c's call ended `hung-up` at once, its `leave` answered 204. a's grid dropped c 56 s
  later (34–71 s over four runs), when the service's roster first marked c inactive: the roster kept c active with its
  call's media until then. c's web client leaving the same kind of call is seen within 7 s (`abc-call-mixed`), with
  the same `leave` body. Filed: `docs/backlog/native-group-leave-seen-a-minute-late.md`.
- **c joins again**: c's chat offered Join (`call-button shown join`); Ctrl+Shift+J connected 1.7 s later; a's grid
  listed c again, c heard a and b, b heard c, c turned its camera on and saw both, and a presented c's video on
  `line-2` again (30 more frames).
- **a ends it**: a hung up; b's and c's calls stayed up (two are still in it), and each hung up 15 s later.

## 6. The numbers

Run 23, one machine, one clock (Unix ms of the traces and the sandbox's marks), load average 0.7–0.8.

| | n | min | p50 | p95 | max |
|---|---|---|---|---|---|
| setup: the ask that brings a call up → the asker's media connected, ms (every call of the run; the test call's includes the test's 3 s wait, §1) | 9 | 1 270 | 2 103 | 5 878 | 5 878 |
| the echo's round trip, ms (2 s blocks, every block that lined up, §1) | 8 | 16 730 | 16 760 | 21 800 | 21 800 |
| a → b tone latency, ms (2 s blocks) | 9 | 190 | 230 | 230 | 230 |
| a's window presenting b's video, fps | 9 | 15.0 | 15.0 | 15.1 | 15.1 |
| b's window presenting a's video, fps | 9 | 14.9 | 15.0 | 15.1 | 15.1 |

Setup by call: the test call 5 878 ms (from before the switcher's typing and the test's 3 s wait before Enter, §1, to
connected: its start, the echo service's answer and media), a calling
b's page 5 573 ms (the page's ring and click included), b's page calling a 1 296 ms (from a's answer), 1:1 b's
answer → b 1 335 / → a 1 270, group a 2 355 / b 2 130 / c 2 103, c's join 1 693 ms.

CPU per process in the 1:1 video call (both cameras on, 20 one-second samples, % of one CPU):

| process | p50 | p95 | max |
|---|---|---|---|
| a's app | 14 | 15 | 16 |
| b's app | 15 | 18 | 18 |
| a's niri / weston / PipeWire / WirePlumber | 2 / 0 / 0 / 0 | 3 / 2 / 1 / 0 | 3 / 2 / 1 / 0 |
| b's niri / weston / PipeWire / WirePlumber | 2 / 1 / 0 / 0 | 2 / 1 / 1 / 0 | 3 / 2 / 1 / 0 |
| the orchestrator (the sandboxes' cameras and recorders) | 2 | 2 | 3 |

An app holding a 1:1 video call (OpenH264 at 640×360, 15 fps each way, Opus, the audio processing) takes about 15 %
of one CPU on this machine: no case for hardware video encoding yet.

## 7. `fence.log` (run 23, before the lab's call rule changed)

Run 23 ran under the call registry's rule (every follow-up resolved to a call the fence registered); §10 has the runs
after the lab's call rule changed to judging a call's entry. Run 23, from the byte the run began: 519 whole lines, no breach, no deny of a write; every permitted write is one the
run made:

| writes permitted | count |
|---|---|
| call starts: the test call (`new`, the caller and the echo service) / in the a–b chat / in the group chat | 1 / 5 / 1 |
| the join of the running group call | 1 |
| answers (`accept`) / the decline (`reject`) / the ring's `attach` and `progress` | 6 / 1 / 7 and 7 |
| follow-ups on registered calls: endpoint metadata 13, endpoint state 6, renegotiate 10, media answer 12, media acknowledge 8, channel parameters 4, media descriptions 4, participant properties 2, leave 8; the echo's `add` 1 | 68 |
| the lab accounts' own availability | 8 |
| typed delivery responses: acceptance acknowledgements / invitation receipts / negotiation receipts | 6 / 6 / 19 |

Every call write but the starts is on `Scope::CallOperation`, so the call registry resolved it: a call this run's
fence started, joined or answered, or an invitation it offered. Every answer and the decline were on an invitation the
fence offered (a lab caller's, naming only lab accounts). The denies (113) are all browser denies inside b's page's two
runs (95 of the reviewed expected-deny list, 18 of the runs' labelled forced holds), which the scenario judged itself
(both runs exited 0). Requests let through because they reach nobody (`allowed`): the registrations, presence reads
and subscriptions, the endpoints' availability `PUT`, telemetry and the page's own token and settings requests.

Over every run of the task (06:31–09:26Z, runs 1–23 and the regressions): 4 070 allows and 1 046 browser denies, no
deny of a write and no breach, contained or arming-failure line.

## 8. The other live checks

- `proto-lab run ab-chat --headless` (08:02Z): exit 0, `ok`.
- `teams-headless`'s `chat_live` (08:05Z): passed. `teams-app`'s `ui_live` (08:08Z): passed.
- `proto-lab run abc-chat --headless`: failed at 08:04Z and 08:13Z (b's page showed the group chat at Saturday's
  messages and never the new one, with no jump to the newest offered) while a call a failed run had left running
  in the group chat was still up (§ The failed runs, run 13); passed at 09:24Z (exit 0, `ok`) once it had ended.
- The first attempt at all four (07:53Z) lost the network: `teams.microsoft.com` didn't answer for about 10 minutes.

## The failed runs

None was a breach. Each failed run's output is kept under `<data_dir>/call-live/`.

- **Runs 2–4** (web): a heard nothing from the page in the call the page placed. Read: the page's microphone was
  silent (§2), not the app. `ab-call-app` now switches its microphone again while its own audio source stays silent,
  and says `mic` when it plays.
- **Runs 4–6** (web): a's call the page hung up ended `failed media` instead of `remote` (the open item Task 16 left: b
  "failed media about 90 ms after a hung up"). Two defects, fixed with tests: media the far end closed counted as
  media that never connected once the connect deadline had passed (`teams-core`, `calling/media.rs`, `Deadlines`), so
  the call failed at once instead of giving the call's own end its grace; and when that end never comes (none did in
  runs 5–6: the media closed and no `call/end` was delivered), the call now ends as the far end's (`End::Remote`, nothing
  read into it) with a `leave`, not as a failure (`Calls::media_closed`).
- **Run 8**: the test call's start got no answer within 10 s (outcome unknown, not sent again): environment.
- **Runs 9–11**: the echo check measured with steady tones and b talking throughout (§3), then a floor measured too
  soon after b fell silent; fixed in the test.
- **Run 12**: a was told of b's decline by the call ending, not by voicemail (§4); the test takes both now.
- **Runs 13, 18, 20, 21**: a's in-call bar never dropped c after c left: the bar's people are its callees, not the roster;
  the test reads the grid (`call-participant shown`) now, which drops c once the service does (§5).
- **Runs 14–16**: the group call's start answered 409 ("Conversation cannot be created due to a failure to resolve
  ownership") or timed out for 40 minutes: run 13's failure killed a's and b's apps inside the group call, which the
  service kept up. The test now asks an app to quit from its tray when a run fails (its engine hangs up), and kills it
  only after 15 s.
- **Runs 17, 19**: Entra's token endpoint didn't answer (08:34–08:41Z, 08:49–09:02Z): environment.
- **Runs 1, 22**: passed their phases (the test call; the group call).

## 9. The gate

On the final tree: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`scripts/lint-report.sh` (two reasoned `expect`s, unchanged), `cargo test -p fixture-guard` and
`PROTO_LAB_REQUIRE_CHROME=1 GPUI_SMOKE_REQUIRE=1 cargo test --workspace` all passed.

## 10. After the lab's call rule changed

The lab's fence now judges a call by its entry only (CLAUDE.md, "Calls go only to the lab, both ways";
`docs/protocol/fence-targets.md`, "The lab's call rule"): a start, a join by the chat it names, an answer or a decline of
the account's own offered invitation, an `add` by its invitees, each read fresh; the follow-ups, the ring-time sends,
the `leave` and every Trouter delivery response pass, logged, with no registry lookup. The browser fence reads no call
answer at the response stage, and the scenarios hold nothing of a call's back but `start-held`'s starts. The runs of
2026-10-26 11:47–12:29Z, from normal release builds (`target/lab`), headless, under the live lock and an outer
`timeout --signal=INT`, one browser per lab profile, nothing left running after any run:

| run | result |
|---|---|
| `proto-lab run echo-call --headless` (11:47Z) | **ok**: the tone came back; start, `add`, `updateEndpointMetadata`, channel parameters and `leave` permitted, the acceptance acknowledged over Trouter (20 delivery responses let through) |
| `proto-lab run ab-call-native --headless` | **ok on the third attempt** (11:58Z). 11:49Z: phase 1's call ended `remote 429 Too many media renegotiations` after b's page renegotiated again after every native answer for 22 s (a loop the capture of 03:26Z, before this change, shows too: `docs/backlog/web-renegotiates-against-a-native-answer-in-a-loop.md`); 11:53Z: b's page's WebRTC statistics showed 4 packets sent in phase 1, though each side heard the other's tone |
| `proto-lab run abc-call --call video --headless` (12:02Z) | **ok**: a's start let through, b and c answered, every page decoded video |
| `proto-lab run abc-call-mixed --headless` (12:08Z, 90 s after the last group call) | **ok**: a's native client called, b and c answered, c left and joined again (2 joins let through), video on two lines |
| `call_live`, every case (12:15Z, 12:20Z) | **failed twice at the same check**: `one-to-one`'s echo check reads b's microphone from b's `call-participant self` trace line, which b's 1:1 call never has (its grid shows only once the roster names a participant, and a 1:1 callee's never does: run 23's b had none either); the echo margin itself measured 38.3 and 39.4 dB. The test call (round trip p50 16.9 and 16.7 s) and both `web` directions passed in both runs (the cause, a 1:1 call's roster read as nobody in the call, was fixed after: below) |
| `call_live`, `CALL_LIVE_PHASES=decline,group` (12:26Z) | **passed**: b declined and a was told (ended, 2.7 s); the group call (setup a 2245, b 1953, c 1856 ms; each hears the other two; two lines on every window), c left (seen by a after 70.6 s, as before) and joined again (2294 ms) |

**`fence.log`** over these runs (11:47:32–12:28:59Z, 3 720 whole lines): no breach, no arming failure, no deny of a
write. Writes permitted: call starts 29 (test call 4, in a chat 25, each a browser's two layers or a native client's
one), joins 5, answers 33 and a decline 1, all on the account's own offered invitation from a lab caller; the echo's
`add` 4; and with no lookup the ring-time sends (`attach` 37, `progress` 37) and the follow-ups (renegotiate 197, media
answer 100, media acknowledge 145, endpoint metadata 70, endpoint state 33, participant properties 24, channel
parameters 19, media descriptions 17, `leave` 37). The native clients' typed delivery responses: 15 acceptance
acknowledgements, 13 invitation receipts, 148 negotiation receipts, all permitted; the pages' delivery responses passed
the frame rule, logged `allowed`. The denies (186) are all browser denies on the expected-deny list or forced: the call
log's read horizon (168), the file browser's prewarm (13), an earlier run's probe in the group chat (5). No answer or
decline went outside the lab: every `accept` and `reject` was decided on an invitation the fence had offered (a lab
caller's, to the account's own MRI).

The gate on the tree of this change passed (`cargo fmt --all --check`, `cargo clippy --workspace --all-targets
--all-features -- -D warnings`, `scripts/lint-report.sh`, `cargo test -p fixture-guard`, `PROTO_LAB_REQUIRE_CHROME=1
GPUI_SMOKE_REQUIRE=1 cargo test --workspace`), with `teams-headless`'s echo test failing once on its tone's timing at a
load average of 44 (the lab build running beside it) and passing twice alone.

**A 1:1 call's roster.** Neither end of a native 1:1 call ever had a participant (`call N: 0 participant(s)`), so
its participant grid never showed and `one-to-one`'s echo check found no `call-participant self` line on b. The 1:1
call's `rosterUpdate` deliveries did come, at the start's and the `attach`'s callbacks alike (the captures of
`ab-call`, `ab-call-native` and `ab-call-app` show both ends getting the same updates, the first listing both once the
callee accepted), but a 1:1 call's endpoints list no `mediaStreams` (its media goes through no mixer: their `call`
holds only `serverMuteVersion`), and the native client took a participant as in the call only with media streams
listed. It now takes an `active` participant with an endpoint holding a `call` (`teams-call`'s `read::roster`); the
mock delivers a 1:1 roster in that shape too, and the echo check fails by name when b's grid never showed. The rerun:

| run | result |
|---|---|
| `call_live`, every case (13:22Z, 431 s; `teams-app`, `proto-lab` release builds from `target/lab`) | **passed, first attempt**: the test call (setup 2427 ms, round trip p50 16.8 s over 7 agreeing blocks); a calls b's page (6139 ms) and b's page calls a (968 ms), each heard and seen; 1:1 between the apps (b 1405, a 1336 ms), each window's grid showing the other (b's 1:1 call traced its `call-participant` lines for the first time), each hears the other, b's echo 39.0 dB under the echo at its microphone, b's microphone hearing at both ends of the window, a's microphone switch 1369 / 1053 ms, a → b latency p50 210 ms, video 15 fps both ways; b declined (a told after 366 ms); the group call (a 2408, b 1895, c 2054 ms; each hears the other two; two lines on every window), c's leave seen by a after 70.5 s, its join 1742 ms |

Its `fence.log` (512 lines): no breach, no arming failure, no deny of a write. Starts 8 (1 test call, 5 in the a–b
chat, 1 in the group chat, 1 join), answers 6 and a decline 1 on offered invitations, the echo's `add` 1; with no
lookup `attach` 7, `progress` 7 and 107 follow-ups (renegotiate 28, media answer 18, media acknowledge 24, endpoint
metadata 13, endpoint state 6, channel parameters 4, media descriptions 4, participant properties 2, `leave` 8); the
native clients' delivery responses (5 acceptance acknowledgements, 5 invitation receipts, 31 negotiation receipts) all
permitted. The 26 denies are b's page's, on the expected-deny list: the call log's read horizon (24) and the file
browser's prewarm (2).

# The user's real-call checklist (the personal build)

M3 Task 23. The steps above ran in the lab; these are the user's, on their own desktop with the personal build
(`teams`), their own account, their colleagues and their own devices. Agents never build, install or run it and never
read anything under `~/.local/share/teams/` (`docs/personal-build.md`), so every step here is the user's, and a problem
comes back by its reason: what the window said, the kind and reason of the `fence.log` line, and what the diagnostic
log showed in the user's own words, never a chat id, a name, a token or an excerpt of a log.

## Before the first call (once)

1. **Install the build with calls.** Quit the running `teams` (the tray item's "Quit"; closing the window only hides it
   while the tray has the item), then from a checkout that holds M3's calls (the branch `native-teams-m3-calls`, or
   `main` once it is merged) run `scripts/install-personal.sh` (a release build of `teams` and
   `teams-signin-personal`, installed under `~/.local/lib/teams/`, `teams` linked from `~/.local/bin/`) and start
   `teams`. The same two steps update it after a fix. Its first-run screen isn't shown again to an account that
   confirmed it; for calls it says: "Calls: when someone calls you, the call is told it rings here, as any Teams app
   does. Answering, declining, calling and joining go out only when you click; your microphone opens only then, and
   your camera only when you turn it on." What the build sends for calls without a click: the ring-time sends of an
   invitation it rings for (its receipt, `attach`, `progress`), and once the user's call is up that call's own setup,
   keep-alive and end writes.
2. **Video.** The calls' media is libwebrtc's, built into the app (H.264 on the GPU through VA-API where it
   can), so nothing is downloaded at run time: the settings show the
   switch "Video in calls" and the in-call bar a "Camera on" button. With the switch off (it is on by default, and
   read at each call's start) calls are audio only, but the bar keeps its "Camera on" button and
   answers it with "Couldn't turn the camera on or off: this call carries no video". Keep the switch on for step 4.
3. **Devices.** Settings (the rail) → **Devices** lists Microphone, Speaker and Camera, each on "Follow the system
   (<the current default>)" until another is chosen. The build opens the desktop's PipeWire; if a chat's header never
   shows **Call**, start `teams` from a terminal: `teams: no calls: <why>` on its stderr says why the calls are off.
4. **The camera portal.** The camera comes through the desktop's camera portal, which asks once, at the first "Camera
   on". `busctl --user introspect org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop
   org.freedesktop.portal.Camera` should list `AccessCamera`; without it the camera can't be turned on (the bar says
   "Couldn't turn the camera on or off: …") and the calls are still audio.
5. **The logs.** `~/.local/share/teams/fence.log` has a line for every call write, every call delivery response and
   every frame of the calling socket. The diagnostic log is `~/.local/share/teams/logs/teams-app.log` (rotated at
   8 MiB to `.1` … `.4`): at `info` (the default) it names each call write and its outcome (`call write accept (3):
   fence allowed, HTTP 200`), the calling socket's drops and reconnects, and an invitation the fence didn't offer; at
   `debug` it has every request, answer and frame in full. To read a failure in full, set Settings → **Diagnostic
   log** to `debug` (or start `TEAMS_APP_LOG=debug teams`), do the step again, read the log, and set it back to
   `info`. A debug log holds colleagues' names and messages: it, and what "Copy diagnostics" copies, are for the
   user's own reading only.
6. **Where to click.** A 1:1 or group chat's header has **Call** once the calling endpoint is registered (**Join** where
   an invitation named a running call in that group chat). An incoming call rings with a ringtone, a notification
   ("<caller> is calling", "Incoming call" or "Incoming group call", with Answer and Decline) and a banner over the
   conversation pane ("<caller> is calling", or "… is calling in <chat>"; Answer, Decline). The in-call bar shows who
   the call is with, its state ("Calling…", "Ringing…", "Answering…", "Joining…", "Connecting…", "Connected · m:ss"),
   Mute/Unmute, Camera on/off and Hang up; once the call's roster names who is in it, a cell per participant under it
   (their video when they send one, a border while they speak) and "You". After a call the bar says how it ended
   ("Call ended", "Call declined", "Call ended by the other side (…)", "Call failed (<step>)", "Call ended (it went to
   voicemail)") until Dismiss. Keys: Ctrl+Shift+C calls the chat shown, Ctrl+Shift+J joins, Ctrl+Shift+S answers,
   Ctrl+Shift+D declines, Ctrl+Shift+M mutes, Ctrl+Shift+O turns the camera on or off, Ctrl+Shift+H hangs up; Ctrl+K
   offers the same as commands ("Call", "Test call", "Join call", "Answer call", "Decline call", "Hang up", "Mute or
   unmute", "Camera on or off").

## What to send back when a step fails

- The step's number, what was done, and which client the other side used (Teams on Windows or macOS, the web client in
  a browser, the phone app).
- What the window said, word for word: the bar's end text ("Call failed (accept)", "Call ended by the other side
  (…)"), a "Couldn't …: …" line, or what didn't appear (no ring, no banner, no picture). Leave out a name or a chat if
  one is in it.
- The step's `fence.log` lines, only their kind, call operation and verdict: `jq -c '{verdict, kind, op: ((.scope |
  objects | .call_operation.operation?) // null), event, response, own, reason, message}'
  ~/.local/share/teams/fence.log | tail -n 30` prints them without the `scope`'s ids and the `url` fields, which hold
  chat and call ids; `op` is the operation's name (`leave`, `attach`, `renegotiate`, …) on a call's follow-up and
  `null` elsewhere. A `deny` line's `reason` and `message` are what is needed most.
- What the diagnostic log showed, in the user's own words: which call write failed and how (`accept`, "fence allowed,
  HTTP 403"; of a "fence denied: …" only the words after its last colon, the rest names the chat), the calling socket
  going down, "an incoming call the fence doesn't offer", and about when in the call (the bar's duration).

## The steps

Wait about 90 s between two calls in the same chat (see "Known gaps"). Every `fence.log` line a step names is an
`allow`; a `deny` is a failure to send back even when the call worked.

### 1. The test call

- **Do**: Ctrl+K, type "Test call", Enter. Talk when the echo service asks, then listen; Hang up.
- **See**: the bar "Test call", "Calling…", then "Connected"; the service's prompt, then what was said played back
  (the lab's round trip was about 17 s from speaking to hearing it back); "Call ended" after Hang up.
- **`fence.log`**: `call` (the test call's start, in no chat), `invite` (the echo service added), `call_follow_up`
  lines and `delivery_response` lines (`call_acceptance_acknowledgement`, `call_negotiation_receipt`), and the `leave`
  (a `call_follow_up`). No chat is created for it.

### 2. A lab account, through its 1:1 chat

- **Before**: once, from the user's own Teams client, start a 1:1 chat with a lab account (a, b or c, as `lab.toml`
  names them); it then shows in `teams`' chat list. No lab live run may hold the lab's live lock: in a second terminal
  hold it for the length of the step, `flock -n ~/.local/share/teams-lab/live.lock -c 'echo "lab lock held: place the
  call, then press Enter"; read _'`. If that exits at once, a lab run holds the lock: wait for it to end and try again
  (a lab run started meanwhile waits for the lock).
- **Do**: open that chat, Call (or Ctrl+Shift+C). Optionally answer it on a device of the user's own signed in as that
  lab account, talk, hang up there.
- **See**: "Calling…", "Ringing…". Nothing of the lab answers or declines it (the lab never calls, answers or declines
  the user's account). Unanswered, it ends when the service gives up: voicemail ("Declined or not answered:
  voicemail"; hang up) or "Call ended by the other side (…)". Answered on the user's device: "Connected", audio both
  ways.
- **`fence.log`**: `call` (the start, after the chat's fresh read listed the account and the callee), then the call's
  follow-ups.

### 3. 1:1 audio with a colleague, each way

- **Do**: (a) call a colleague from the 1:1 chat with them; talk for a minute; the colleague hangs up. (b) The
  colleague calls; answer once from the banner's Answer, and on another call from the notification's Answer; talk;
  hang up from `teams`.
- **See**: (a) "Calling…", "Ringing…", "Connected · 0:01" once they answer; each hears the other; "Call ended by the
  other side" when they hang up. (b) The ringtone, the notification and the banner naming the colleague; on Answer
  all three go and the bar shows "Answering…", "Connecting…", "Connected"; each hears the other; Hang up gives "Call
  ended" here and ends the call for the colleague.
- **`fence.log`**: (a) `call`; (b) while it rings `call_ringing` lines (`attach`, `progress`) and a
  `delivery_response` `call_invitation_receipt`; on Answer `call_answer`; then `call_follow_up` lines and the
  `leave`. Before Answer nothing is sent for the ring but those ring-time sends.

### 4. Video, 1:1, both sides

Needs the video setup above, with its switch on.

- **Do**: in a 1:1 call (either way), check that the bar reads "Camera on" (the camera is off at every start, answer
  and join); click it (or Ctrl+Shift+O); the first time, allow the desktop's camera dialog. Ask the colleague to turn
  their camera on. Later "Camera off", then on again.
- **See**: your own picture appears, and the colleague sees you, smoothly (the lab's test camera's 15 fps came
  through whole); their video appears once they turn it on. "Camera off": they see your video stop, and nothing more of your camera is sent.
- **`fence.log`**: `call_follow_up` lines (`renegotiate`, `media_answer`, `media_acknowledge`) as the cameras come
  and go.

### 5. A group-chat call

- **Do**: open a group chat with two or more colleagues told beforehand; Call. Let two of them answer; turn cameras on;
  talk; one of them leaves; hang up.
- **See**: "Calling…", then "Connected" once the first answers; the bar's title is the chat's name; a cell per
  participant as they come in, their videos in their cells, a border on whoever speaks. Everyone hears everyone. A
  colleague who leaves goes from the grid (up to a minute late, see "Known gaps"). Hang up ends the call for the user
  only: the others go on while two are in it.
- **`fence.log`**: `call` (the start, after the group chat's fresh read listed the account and every callee), then
  the follow-ups (`update_media_descriptions` and `apply_channel_parameters` among them as cameras turn on).

### 6. Joining a running group-chat call

- **Do**: with `teams` running and no call up, a colleague starts a call in a group chat the user is in. It rings
  here: let it ring until it stops (don't Decline). Then open that chat: its header shows **Join**; click it (or
  Ctrl+Shift+J).
- **See**: "Joining…", then "Connected"; the participants' cells; the user hears and is heard. Answering the ring
  itself (the banner's Answer) is also a way in, worth one run.
- **`fence.log`**: `call` (the join, on the group chat the invitation named, its fresh read listing the account), then
  the follow-ups.
- **Note**: Join is offered only for a call the app was rung for. A call that began while `teams` wasn't running, or
  while another call was up, shows **Call**, and a Call there ends at once (see "Known gaps"): join that one from
  another client. Once its ring has stopped here the app hears no more of that call, so it doesn't know when the call
  ends: a Join after the call has ended still goes out and ends as "Call failed (join)" (or "Call ended by the other
  side (…)"), and the header still shows Join. That is expected, not something to send back. A second Join then says
  "Couldn't join: the call there has ended", and the header goes back to Call.

### 7. A colleague's call declined

- **Do**: the colleague calls; Decline from the banner, from the notification, or with Ctrl+Shift+D.
- **See**: the ringtone, the banner and the notification go; "Call declined" here; the user's other clients stop
  ringing; the colleague's client shows the call declined, or puts them through to the user's voicemail (the service
  has done both).
- **`fence.log`**: the ring-time sends, then `call_decline`.

### 8. A microphone or headset switched mid-call

- **Do**: in a 1:1 call with a colleague: (a) Settings → Devices → Microphone, pick another (a headset's); after a
  minute, back to "Follow the system". (b) With "Follow the system", plug in (or connect over Bluetooth) a headset
  the desktop makes its default, then unplug it. The same for Speaker.
- **See**: the colleague hears the user through the new microphone within about 2 s (the lab: 1.1–1.6 s), and the
  user hears them on the new speaker; the call never ends or reconnects. A device chosen and then unplugged shows
  "(not connected)" in the settings, and its stream falls back to the system's default. A Bluetooth headset's
  microphone appears only once the desktop has switched the headset to its headset profile.
- **`fence.log`**: nothing new; a device switch sends no write.

### 9. An incoming call from someone outside the user's chats

- **Do**: a colleague of the same tenant with whom the user has no chat in `teams` calls; answer.
- **See**: the ring, the notification and the banner naming them as the call's invitation names them ("Someone"
  without a name); Answer connects as in step 3; the bar's title is their name.
- **`fence.log`**: as step 3 (b).
- **Note**: a caller of another tenant or a personal Microsoft account isn't offered here until a lab capture shows
  that kind of caller: it doesn't ring in `teams` (it rings on the other clients), and the info log says "an incoming
  call the fence doesn't offer: ignored". For a caller of the same tenant that line is a failure to send back.

### 10. A call answered on the user's phone

- **Do**: a colleague calls while `teams` and the phone app are both signed in; answer on the phone.
- **See**: `teams` stops ringing within a couple of seconds: the ringtone, the banner and the notification go, and no
  call bar appears. The call goes on on the phone.
- **`fence.log`**: the ring-time sends only; no `call_answer`.

### 11. A call longer than an hour

- **Do**: keep a 1:1 call with a colleague (audio is enough) up for at least 75 minutes, past a token renewal (the
  tokens last 60–90 minutes and are renewed 5 minutes before they expire). Talk now and then, and at the end. Before
  hanging up, check that the info log has a `session: the skypetoken renewed` line whose time (UTC) falls within the
  call; if none does yet, keep the call up until one does.
- **See**: the duration passes "1:00:00" and the call stays "Connected"; each still hears the other at the end; Hang
  up ends it normally.
- **If it drops**: the bar's end text, the duration at which it happened, and from the info log around that time the
  call write that failed and how, a "calling socket down" or "stopped" line, or a "can't be renewed" line.

### 12. Echo heard by the colleague, or not

- **Do**: in a 1:1 call, use the laptop's own speakers and microphone (the hardest case) at a normal volume, and talk
  both ways for two minutes; then the same on a headset. Ask the colleague whether they hear their own voice back.
- **See**: no echo of their voice after the first few seconds of the call (the canceller adapts within seconds; the
  lab measured the echo 39–46 dB under what was played).
- **Send back**: echo or none, on which devices, from when, how loud, and whether it went away.

## Known gaps a step may hit

- **A call in a chat soon after its last one, or in a chat whose call still runs**: the service refuses a new call in
  a chat for about a minute after its last call ended, and in a chat that has a running call, with "Call ended by the
  other side (Conversation cannot be created due to a failure to resolve ownership)" (`409`, sub-code `5704`;
  `docs/protocol/calling.md`). Wait 90 s and call again; for a running group call, Join (step 6).
- **The web client and the camera**: with a colleague on the Teams web client in a browser, their camera turned on in
  a 1:1 call made the page renegotiate after every answer of the native client until the service ended the call
  ("Call ended by the other side (Too many media renegotiations have occurred on this call leg.)", `429`), in one lab
  run of three (`docs/backlog/web-renegotiates-against-a-native-answer-in-a-loop.md`). Whether the desktop clients do
  it is what step 4 shows; say which client it was.
- **A leave seen late**: when the user hangs up a group call, the others may see the user in it for up to a minute
  more, and a colleague who leaves goes from the user's grid as late
  (`docs/backlog/native-group-leave-seen-a-minute-late.md`). Nothing to send back unless it takes longer.
- **One call or ringing invitation at a time**: a call coming in during a call doesn't ring in `teams` (it rings on the
  other clients), and nothing is sent for it.
- **No call write is sent twice**: a call write whose answer didn't come ends the call ("Call failed (<step>)")
  instead of going again. That is by design, but which step failed is worth sending back.
- **Not part of M3**: meetings, screen sharing, transfer, forwarding, hold, call park, phone numbers (PSTN), adding
  someone to a call (only the test call adds its echo service).
- **The view trace** marks a 1:1 call's other participant `camera-off` whatever their camera; it is the live test's
  record only, and the picture shows as it should.

## Outcome

Pending: the user's run. Each problem reported is fixed (tests first, on the mock or in the lab) and the step run again
by the user; the outcome goes here, in the user's words where they give them.

| step | result | the user's notes |
|---|---|---|
| 1. the test call | | |
| 2. a lab account through its 1:1 chat | | |
| 3. 1:1 audio with a colleague, each way | | |
| 4. video, 1:1, both sides | | |
| 5. a group-chat call | | |
| 6. joining a running group-chat call | | |
| 7. a colleague's call declined | | |
| 8. a microphone or headset switched mid-call | | |
| 9. an incoming call from outside the user's chats | | |
| 10. a call answered on the phone | | |
| 11. a call longer than an hour | | |
| 12. echo heard by the colleague or not | | |
