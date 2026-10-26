# Native Teams for Linux — M4: Meetings and the calendar

## Overview
- **Goal: meetings and the calendar on top of M3's calls.** M4 ends with the personal build (`teams`) showing the
  account's calendar (an agenda with join links) and joining the user's scheduled meetings: from the calendar, from a
  chat's meeting card or from a meeting link, through the lobby when the meeting has one, with the meeting's roster,
  its chat beside the call, and screen sharing (seeing a shared screen first, then sharing the user's own through the
  desktop's portal). Everything is proven on the lab first, as M3 did: the web client in the armed browser is captured,
  each new write is mapped from its capture, then the native client and the app run it live in the UI sandbox.
- **Exit:**
  - **The lab acceptance** (Task 17): the app for a, b and c in the UI sandbox sees a lab meeting in its calendar,
    joins it from there and from its link, carries audio and video among all three with a mixed case (the app and the
    web client in one meeting), waits in the lobby and is admitted, posts in the meeting chat during the meeting,
    sees a screen the web client shares and shares its own (the sandbox's fake screen); `fence.log` shows writes only
    as the lab rule lets them.
  - **The personal build's meetings** under rules the user approves (Task 1), implemented and tested on the mock
    (Task 18).
  - **The user's real-meeting checklist** (Task 19), which the user runs (agents never run the personal build).
- **Scope** (the roadmap after M3): joining scheduled meetings by their link, the lobby, the meeting roster, the
  meeting chat, a chat's meeting card, screen sharing (viewing first, then sharing through the desktop portal), and the
  calendar, read-only (an agenda with join links).
- **Out of scope**: creating, editing or cancelling meetings in the native client (the calendar is read-only; the lab
  meetings are made once by the web client, Task 3), meetings in a channel (they need a team of the lab accounts only:
  `docs/backlog/channel-writes-need-a-lab-only-team.md`), breakout rooms, live captions, the recording indicator,
  background blur, PSTN dial-in, webinars and town halls, whiteboard and other in-meeting apps (M5), content sharing
  of files or PowerPoint Live, giving or taking control of a shared screen, meeting options and roles beyond what
  joining, the lobby and admitting need.

## Context (from M3)
- M3 is done in the lab (`docs/plans/completed/20261024-native-teams-m3-calls.md`, `docs/m3-acceptance.md`). What
  exists and M4 builds on:
  - **Calls, natively**: `teams-call` (the signaling machine, sans-IO: starts, joins of a running group call, answers,
    declines, renegotiation either way, keep-alive, `leave`), `teams-media` (Teams' SDP dialect both ways, TURN with
    relayed candidates only, Opus with echo cancellation, H.264 on Cisco's OpenH264, the group call's mixer and its
    data channel's control plane, PipeWire devices), the engine's `calling` (one call at a time, every call write
    through the outbox's call kind), the app's calls (Call, Join, the ringing banner and notification, the in-call
    bar, the participant grid, the devices in the settings).
  - **The lab fence judges a call by its entry** (`CallRule::Entry`): a start by its chat's fresh member read, a join
    by the chat it names, an answer or a decline of the account's own offered invitation, an `add` by its invitees;
    what the call sends after that passes, logged. The call registry is the personal build's rule
    (`CallRule::Registry`): a follow-up passes only to a link of a call that fence registered.
  - **A group call's join** is already mapped (`docs/protocol/calling.md`, "Joining a running call"): a `POST` to the
    conversation controller with a call invitation, judged by the group chat its `groupChat.threadId` names. Its body
    carries `meetingInfo` (`organizerId`, `tenantId`, `isBroadcast`); the group chat's header shows **Meet now**
    (`audio-drop-in-button`), which starts a meeting, unmapped.
  - **Meeting chats** (`19:meeting_<id>@thread.v2`) are read and shown as chats; the lab fence reads their members as
    a group chat's (`docs/protocol/fence-targets.md`, "Group and meeting chats"); none exists in the lab yet, and the
    personal fence denies every write to one.
  - **The calendar** is read nowhere yet: the web client's `v2.0/me/calendars/default/calendarView` on the middle
    tier is in `docs/protocol/endpoints.md` as seen, not reviewed; Graph's calendar view is the other candidate.
  - **The sharing line**: a group call's offers already carry an `applicationsharing-video` line (`calling.md`,
    "Teams' SDP dialect"), `callModalities` `["Audio", "ScreenViewer"]`; nothing reads or sends on it.
  - **The UI sandbox** runs a private PipeWire with a tone microphone, a recorded speaker, a fake camera and a fake
    camera portal; it has no screen-cast portal yet.
- **What M3 learned that shapes M4**:
  - **Captures first, then the map.** Every call write was captured with the web client in the armed browser, held
    back by a labelled forced deny when its shape wasn't known, mapped exactly as captured, then let through. A
    meeting's writes (its creation by the web client, its join, the lobby's admit, a share's start) go the same way.
  - **The lab fence stays the entry rule.** A meeting is a call with a meeting chat: its join is judged once, by that
    chat's fresh read, and what the call sends after that passes. M4 adds no follow-up layer to the lab fence; the
    personal build's registry learns the meeting's links as it learned the call's.
  - **Live acceptance from the app in the sandbox**, driven by the keyboard and the notifications' actions, read by
    the view traces, the recordings and `fence.log`; b's web client, where a case needs it, from a spawned
    `proto-lab run` scenario keeping in step by its progress lines (`call_live`, `ab-call-app`).
  - **Known gaps that a meeting run can hit**: a start in a chat with a running call ends at once (`409`, `5704`);
    the web client renegotiates against a native answer in a loop
    (`docs/backlog/web-renegotiates-against-a-native-answer-in-a-loop.md`); a native group leaver is seen a minute
    late (`docs/backlog/native-group-leave-seen-a-minute-late.md`). A failed call run hangs every page up before it
    closes.
  - **The user's real-call run comes at the end**, from a checklist with what to see and what to send back when a step
    fails; agents never run the personal build.
- **Open from M3**: the user's real-call run (`docs/m3-acceptance.md`, "The user's real-call checklist") is pending.
  Problems it reports are fixed in a follow-up before Task 18 builds on the personal build's calls.
- **The OpenH264 question** (`docs/backlog/openh264-patents-and-the-personal-build.md`) stays the user's; a shared
  screen arrives as H.264 too, so the personal build shows one only with the user's own copy of Cisco's library.

## Development Approach
- **testing approach**: TDD (tests first)
- complete each task fully before moving to the next; small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task, success and error paths
- **CRITICAL: all tests must pass before starting the next task**
- **CRITICAL: every task ends with the full gate** (`cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  --all-features -- -D warnings`, `scripts/lint-report.sh`, `cargo test --workspace`); "the gate passes" means all four
- **CRITICAL: update this plan file when scope changes during implementation**
- **a meeting write is mapped only from a capture**, in exactly its captured shape, with a case in
  `crates/teams-fence/tests/contacting.rs` or its fixtures; a write the fence can't tie to a target with certainty is
  denied. A response or delivery shape is handled only once a capture showed it.
- **a hard-rule change is written into CLAUDE.md first**, in the words the user approved (Task 1), before any code
  that relies on it. CLAUDE.md gets current rules only (no dates, no task numbers).
- **the fence's work is the mapping the feature needs**, nothing more: a row for each captured meeting write, judged
  by the lab rule as it stands (the entry rule for a call, a fresh member read for a chat, the participants of a new
  target). No new guard layer, no tightening a realistic sequence never reaches.
- **no window, bus, audio server, camera, screen or opener of the user's desktop** for any lab or test process: the
  app's GUI and media tests run in the UI sandbox, which grows a fake screen-cast portal (Task 2); `teams-headless`
  shares a generated test pattern; the armed browser shares Chrome's fake screen and plays nothing aloud.
- **the personal build is the user's**: agents compile and mock-test the `teams` crate only; its real meetings are the
  user's checklist (Task 19). Until Task 18 the personal build's calendar may be shown (a read), and a meeting stays
  unjoinable there.
- **no write from a `cargo test` process**; live runs from a normal build under the live lock with an outer
  `timeout --signal=INT`, one browser per lab profile and one engine per lab account at a time, nothing left running.
- **captures and recordings stay outside the repo**; fixtures are synthetic-from-capture and pass `cargo test -p
  fixture-guard` (a meeting's join link, id and passcode are already flagged).
- "decide with Codex" means a `codex-discussion`; its outcome is recorded in the task (and in `docs/protocol/` when it
  settles a protocol question). Protocol notes go to `docs/protocol/` in our own words; every endpoint touched gets its
  row in `endpoints.md`.
- every version written (a crate, a toolchain) is looked up live at the time, never taken from memory.

## Testing Strategy
- **unit**: `cargo test --workspace`, offline. The calendar's read and its paging on synthetic fixtures; a meeting
  link's parsing (every form the captures show: the `meetup-join` link, a `launcher` link, a link in a meeting card);
  the meeting join's body; the lobby's deliveries; the sharing line's SDP both ways; the share's video decoded.
- **fence suite**: the meeting rows (`contacting.rs`): the lab meeting's creation by its attendees (and the `lobby`
  meeting's options write by its meeting chat's fresh read, if it is a write of its own), a meeting join by
  its meeting chat's fresh read, the lobby's admit by its admitted participant, a share's start as an operation on a
  permitted call; the personal fence's meeting rule (Task 18); `fence-seal` cases for each new sealed write.
- **mock**: `teams-mock` grows a calendar (a week of events, a lab meeting among them), a meeting (its conversation,
  its meeting chat, its roster, a lobby that holds a joiner until the organizer admits or denies it), and a sharer (a
  party of the mock's sending a test pattern on the sharing line).
- **whole app, offline**: the app in the UI sandbox on the mock, two or three accounts: the agenda, a join from it, the
  lobby, the meeting chat beside the call, a shared screen shown, the account's own screen shared from the sandbox's
  fake portal.
- **live lab**: `proto-lab run lab-meeting` (once: the lab meetings `lab` and `lobby` made and recorded),
  `meeting-join`, `meeting-lobby`, `meeting-share` (the web client on every end), `meeting-native` (a's native client
  with b's and c's web clients), and `crates/teams-app/tests/meeting_live.rs` (`--features live`, `PROTO_LAB_LIVE=1`):
  the app for a, b and c in sandboxes of their own, with b's web client from a spawned `proto-lab run meeting-app`.
- **measurements**: join time (the user's action → media connected, and → admitted from the lobby), a shared screen's
  frames per second and its delay, CPU per process while a screen is shared, each p50/p95/max with the load average.
- **personal build**: tested only on the mock; its live proof is the user's checklist (Task 19).

## Progress Tracking
- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

```
teams-app / teams ── teams-ui (agenda, meeting card, pre-join and lobby, roster panel, meeting chat beside the call,
                               shared screen view, Share)
                                                                 │ EngineHandle
teams-core (engine: the calendar's sync; meeting actions: join by link or event, wait in the lobby, admit, share)
      │ outbox: every write a fresh Fence::check                 ▼
teams-call (the meeting join, the lobby, admit, the share's start and stop)    teams-media (the sharing line: receive,
teams-proto (calendar read, meeting links, sealed meeting writes)                  decode, and send a screen's frames)
teams-fence (the meeting rows; the personal rule's meetings)     ui-sandbox (+ a fake screen-cast portal)
```

- **The calendar** (`teams-proto`, `teams-core`, `teams-store`): the account's calendar view for a window around
  today, read as the web client reads it (Task 4 picks the endpoint from the capture), kept in the store, shown as an
  agenda; an event's join link parsed into a meeting to join. A read in both builds.
- **The meeting join** (`teams-call`): a meeting is a call in its meeting chat; the join is the captured meeting join
  (Task 5), its media as a group call's (the mixer, its lines, the data channel). The meeting's roster comes from its
  `rosterUpdate` deliveries as a group call's does.
- **The lobby** (`teams-call`): a joiner the meeting holds in the lobby waits until it is admitted (the call goes on)
  or denied or the wait times out (the call ends, told); an account in the meeting sees who waits and admits them.
- **Screen sharing** (`teams-media`, `teams-call`): viewing receives the `applicationsharing-video` line, decodes it
  and shows it as the meeting's main view; sharing sends a screen's frames on that line from the screen-cast portal
  (the sandbox's fake, the desktop's for the personal build), started and stopped by the captured writes.

## Technical Details
- **The lab meetings**: two meetings a schedules once from the web client's calendar, each recorded by name in the
  lab's data dir (`meetings.toml`, `0600`: per name its meeting chat's id, its join link, its organizer), as `abc-chat`
  records the group chat. `lab` has the attendees b and c (the meeting every account joins); `lobby` has the attendee b
  only, its lobby holding people who weren't invited, so c, joining it by its link, is the lab's lobby case (Task 7).
  Each creation is a contacting write whose participants are its organizer and its attendees, all lab accounts: mapped
  from its capture as `WriteKind::Meeting` on `Scope::New`, judged by those participants. The lab accounts share a
  tenant, whose people bypass the lobby by default, so `lobby` needs its meeting options set; unless the creation
  carries that option, it is a write of its own on the meeting a created, captured after the creation is mapped and
  judged by the meeting chat's fresh member read (Task 3). The native client creates no meeting and sets no option.
- **A meeting join is a call join in its meeting chat**: the captured join names the meeting chat
  (`groupChat.threadId` or what Task 5's capture shows), and the lab fence judges it as any join: the chat's fresh,
  complete member read lists the joiner and only lab accounts. A join from a link resolves the link to that chat by
  what the link carries (its thread id), never by the client's context.
- **The lobby**: a lab account the meeting chat doesn't list (c, not invited to `lobby`) joins by the link and waits;
  what its join sends and how the organizer's admit names it come from Task 7's capture. The wording of how the lab
  rule judges both is Task 1's (an admit judged per admitted participant like an `add`; the waiting join judged by the
  meeting's organizer and chat being lab-only). No outsider ever reaches a lab meeting's lobby, so the rule is written
  for the lab accounts only.
- **The meeting chat** during and after the meeting is a chat like any other: read, shown beside the call, and written
  under the lab rule by its fresh member read. The meeting card (a chat message announcing a meeting or its start and
  end) is read and shown, with Join when it names a meeting the account may join.
- **Screen sharing, viewing**: the sharing line's direction, its codec and how the mixer names whose screen it carries
  come from Task 11's capture of the web client sharing Chrome's fake screen; viewing changes nothing but the media the
  call receives (a source request on the data channel where the capture shows one, which is media).
- **Screen sharing, sending**: the share's start and stop are call writes mapped from the capture, sent only on the
  user's action; the frames come from the screen-cast portal as a PipeWire node, encoded as H.264 at what the mixer's
  answer allows. The portal asks the user (the personal build); the sandbox's fake portal answers with a test pattern.
- **Interop**: the native client takes the web client's place in a meeting as in a call; the lab proves it against the
  web client in the same meeting, both ways (Tasks 10, 12, 14, 17).

## What Goes Where
- **Implementation Steps**: code, tests and docs in this repo, plus lab commands the agent runs on its own.
- **Post-Completion**: steps that need a human or a decision.

## Implementation Steps

### Task 1: The meeting rules' wording: the proposal, then the user approves it
**Files:**
- Modify: `CLAUDE.md` (after approval), `docs/personal-build.md` (after approval)

- [ ] a proposal of the hard rules' wording, decided with Codex first: the lab (the lab meetings' creation by the web
      client, their lobby option included; a meeting join judged as a join in its meeting chat; the lobby: a lab
      account's join that waits there, and an admit judged by the admitted participant; a share as part of a call the
      fence let in) and the personal build (joining a meeting the account's calendar or a chat lists, on the user's
      action; the lobby; the meeting chat's writes as a group chat's, by its fresh read; viewing a shared screen;
      sharing the user's own screen only on the user's action through the desktop portal, never on a join)
- [ ] ⚠️ stop for the user's approval of the wording (the user may delegate it, as M3's call rules were)
- [ ] the approved wording written into CLAUDE.md's hard rules and `docs/personal-build.md` before any code relies on
      it
- [ ] the gate passes

### Task 2: The sandbox's screen and the browser's fake screen
**Files:**
- Modify: `crates/ui-sandbox/src/*`, `crates/ui-sandbox/tests/*`, `crates/proto-lab/src/browser.rs`

- [ ] tests first: the UI sandbox serves a fake screen-cast portal on its own bus (`org.freedesktop.portal.ScreenCast`:
      a session, a source picked without a dialog, a PipeWire node of the sandbox's showing a moving test pattern);
      nothing reaches the desktop's portal or compositor
- [ ] the lab browser shares Chrome's fake screen without a picker (the flags looked up for the pinned Chrome), with
      no screen of the desktop's
- [ ] the gate passes

### Task 3: The lab meeting (capture, map, live)
**Files:**
- Create: `crates/proto-lab/src/scenario/lab_meeting.rs`
- Modify: `crates/teams-fence/src/contacting*`, `crates/teams-fence/tests/*`, `docs/protocol/meetings.md` (new),
  `docs/protocol/endpoints.md`

- [ ] `proto-lab run lab-meeting --create-held`: a schedules the `lab` meeting from the web client's calendar with b
      and c as attendees, every write of it held back by a labelled forced deny; the capture shows the creation's
      shape (and whether the scheduling form can carry the lobby option in the creation itself)
- [ ] tests first: the creation's row (`WriteKind::Meeting` on `Scope::New`, its organizer and attendees added), a
      synthetic fixture of its shape; an attendee outside the lab denied
- [ ] `proto-lab run lab-meeting --options-held`: a creates the `lobby` meeting (attendee b only) with its creation
      let through, records it in `meetings.toml`, then sets its meeting options so people who weren't invited wait in
      the lobby, that write held back by a labelled forced deny; the capture shows the options write's shape. When
      the creation carries the lobby option (the first capture shows it), `lobby` is created with it chosen in the
      form, this step and the next fall away, and the creation's row covers it
- [ ] tests first: the options write's row (an operation on the meeting a created, judged by its meeting chat's
      fresh, complete member read: organizer and attendees lab accounts), a synthetic fixture of its shape
- [ ] `proto-lab run lab-meeting`: each creation let through once, two meetings recorded by name in `meetings.toml`
      (each with its meeting chat's id, its join link, its organizer): `lab` (organizer a, attendees b and c) and
      `lobby` (organizer a, attendee b only), and `lobby`'s options write let through so its lobby holds people who
      weren't invited; a later run finds both recorded, creates only one that is missing, and sets `lobby`'s option
      when its options read shows it unset (as the `--options-held` run leaves it)
- [ ] `docs/protocol/meetings.md` (the creation, the lobby option's write if it is one, the meeting's chat and its
      thread shape) and the endpoints' rows
- [ ] the gate passes

### Task 4: The calendar, read
**Files:**
- Modify: `crates/teams-proto/src/*`, `crates/teams-store/src/*`, `crates/teams-core/src/*`,
  `crates/teams-headless/src/*`, `crates/teams-mock/src/*`, `docs/protocol/meetings.md`

- [ ] capture the web client's calendar (a week's view and the lab meeting in it); pick the read it uses (the middle
      tier's `calendarView` or Graph's) and describe it: its window, paging, an event's fields, its join link's forms
- [ ] tests first: the read on synthetic fixtures (every captured form of an event and its link, recurring ones
      included); the store keeps a window around today and replaces it on each sync; the mock serves a week
- [ ] the engine syncs the calendar on start and on a timer, told as `Event::Calendar`; `teams-headless <alias>
      calendar` prints the window by kind and time, never a subject or a name
- [ ] the gate passes

### Task 5: Joining the lab meeting in the browser (capture, map, live)
**Files:**
- Create: `crates/proto-lab/src/scenario/meeting_join.rs`
- Modify: `crates/teams-fence/src/contacting/call.rs`, `crates/teams-fence/tests/*`, `docs/protocol/meetings.md`

- [ ] `proto-lab run meeting-join --join-held`: a, b and c open the recorded `lab` meeting from the calendar and from
      its link, each join held back by a labelled forced deny; the capture shows the pre-join screen's requests and the
      join's shape
- [ ] tests first: the meeting join's row (a join judged by the meeting chat its body names, read fresh), a fixture of
      its shape; a join naming a chat outside the lab denied
- [ ] `proto-lab run meeting-join`: a joins first, then b and c; audio and video among all three (the hooks' audio
      tap, `getStats()`); the roster's deliveries and the meeting chat's events captured; everyone leaves
- [ ] the gate passes

### Task 6: The meeting chat and the meeting card
**Files:**
- Modify: `crates/teams-proto/src/chat*`, `crates/teams-ui/src/*`, `crates/teams-fence/tests/*`,
  `crates/proto-lab/src/scenario/meeting_join.rs`

- [ ] from Task 5's capture: the meeting chat's thread and member shapes, the messages a meeting leaves in its chat
      and in the invitees' chats (the meeting card: scheduled, started, ended), described in `meetings.md`
- [ ] tests first: the fence's member read of a meeting chat in its captured shape (complete, lab-only, writable);
      the card parsed into a typed message (subject, time, join link) and kept as an unknown form when it doesn't read
- [ ] the chat view shows a meeting card with Join (a join is Task 9's)
- [ ] `meeting-join` grows a post: a's lab message in the meeting chat during the meeting (after the scenario's
      forced-deny probe) reaches b's and c's pages
- [ ] the gate passes

### Task 7: The lobby in the browser (capture, map, live)
**Files:**
- Create: `crates/proto-lab/src/scenario/meeting_lobby.rs`
- Modify: `crates/teams-fence/src/contacting/call.rs`, `crates/teams-fence/src/decision.rs`,
  `crates/teams-fence/tests/*`, `docs/protocol/meetings.md`

- [ ] `proto-lab run meeting-lobby --join-held`: a in the `lobby` meeting (its lobby option set by Task 3); c, not
      invited to it, opens its link and its join is held back by a labelled forced deny; the capture shows the
      waiting join's shape
- [ ] tests first: the waiting join's row as Task 1's wording judges it (the meeting's organizer and chat lab-only,
      the joiner a lab account the chat doesn't list yet), a fixture of its shape; then the row and its judgment in
      `decision.rs`
- [ ] `proto-lab run meeting-lobby --admit-held`: c's join let through and c waits in the lobby; a's admit held back
      by a labelled forced deny; the capture shows the lobby's deliveries and the admit's shape
- [ ] tests first: the admit's row (judged per admitted participant), a fixture of its shape
- [ ] `proto-lab run meeting-lobby`: c waits, a admits, c is in the call with audio both ways; a second run where a
      denies and c's page is told
- [ ] the gate passes

### Task 8: Meetings on the mock
**Files:**
- Modify: `crates/teams-mock/src/*`, `tests/fixtures/calling/*`

- [ ] tests first: the mock serves a meeting from the shapes of Tasks 5–7 (its conversation, its meeting chat, its
      roster, a lobby holding a joiner until the organizer admits or denies it, a meeting card in a chat), and a
      party of its own in the meeting with its media (as M3's mixer)
- [ ] the gate passes

### Task 9: The native meeting join
**Files:**
- Modify: `crates/teams-proto/src/write/call.rs`, `crates/teams-call/src/*`, `crates/teams-core/src/calling*`,
  `crates/teams-headless/src/*`

- [ ] tests first (on the mock): a meeting joined by its link and by a calendar event, as a sealed join built whole
      and classified when made; its media as a group call's; its roster shown; the meeting chat's id the join names
      taken from the link, never from the client's context
- [ ] the lobby on the native side: a join held in the lobby waits (told), goes on when admitted, ends when denied or
      when its wait runs out; an account in the meeting is told who waits and admits on `CallAction::Admit`
- [ ] `teams-headless <alias> meeting join <meeting>` and `meeting admit` (`<meeting>` a name `meetings.toml`
      records, `lab` or `lobby`, or a link)
- [ ] the gate passes

### Task 10: Native meetings live
**Files:**
- Create: `crates/proto-lab/src/scenario/meeting_native.rs`

- [ ] `proto-lab run meeting-native`: a's native client joins the `lab` meeting with b's and c's web clients; each
      hears the others and decodes video; c leaves and joins again
- [ ] the native lobby live: c's native client waits in the `lobby` meeting's lobby and a's web client admits it;
      a's native client admits c's web client
- [ ] the gate passes

### Task 11: Screen sharing (1/3): the web client's share (capture)
**Files:**
- Create: `crates/proto-lab/src/scenario/meeting_share.rs`
- Modify: `docs/protocol/meetings.md`, `crates/teams-fence/src/contacting/call.rs`, `crates/teams-fence/tests/*`

- [ ] `proto-lab run meeting-share`: in the lab meeting b shares Chrome's fake screen, a and c see it; the capture
      shows the share's start and stop, the renegotiation that adds or opens the sharing line, its codec and
      resolution, and how the mixer names the sharer; a native group call's 1:1 and group shapes too (Task 12 needs
      both)
- [ ] tests first: the share's writes mapped as operations on a call the fence let in, fixtures of their shapes
- [ ] the gate passes

### Task 12: Screen sharing (2/3): seeing a shared screen natively
**Files:**
- Modify: `crates/teams-media/src/*`, `crates/teams-core/src/calling*`, `crates/teams-ui/src/video.rs`,
  `crates/teams-ui/src/call_view.rs`, `crates/proto-lab/src/scenario/meeting_native.rs`

- [ ] tests first (on the mock): the sharing line received and decoded, its frames on a feed of their own, the sharer
      named from the roster; a share that stops is told
- [ ] the app shows a shared screen as the call's main view, the participants beside it; `teams-headless` counts its
      frames
- [ ] live (`proto-lab run meeting-native --web-share`): b's web client shares in the `lab` meeting, a's native client
      counts its frames (frames per second and delay measured); the app seeing it live is Tasks 16 and 17's
- [ ] the gate passes

### Task 13: Screen sharing (3/3): sharing the account's own screen
**Files:**
- Modify: `crates/teams-media/src/devices/*`, `crates/teams-proto/src/write/call.rs`, `crates/fence-seal/tests/*`,
  `crates/teams-call/src/*`, `crates/teams-core/src/calling*`, `crates/teams-ui/src/*`, `crates/teams-headless/src/*`,
  `crates/proto-lab/src/scenario/meeting_native.rs`

- [ ] tests first: the screen through the screen-cast portal (the sandbox's fake in tests, the desktop's only for the
      personal build, fixed when built as the camera's is), encoded on the sharing line at what the answer allows;
      the share's start and stop as sealed writes, only on `CallAction::Share`; a call never starts sharing by itself
- [ ] `teams-headless … --share-after <s>` shares a test pattern
- [ ] live (`proto-lab run meeting-native --native-share`): a's native client shares, b's and c's web clients see it
- [ ] the gate passes

### Task 14: Meetings in the app
**Files:**
- Modify: `crates/teams-ui/src/*`, `crates/teams-app/src/*`, `crates/teams-app/tests/*`

- [ ] tests first (in the UI sandbox on the mock): the agenda (the rail's Calendar: today and the week, an event's
      time, subject and Join); a join from the agenda, a meeting card and a pasted link; the pre-join screen
      (microphone and camera, the camera off); the lobby's wait screen and its end; the roster panel; the meeting chat
      beside the call; who waits in the lobby with Admit; a shared screen as the main view; Share and Stop sharing;
      keys and switcher commands for each
- [ ] traces for each view (kinds and ids, never a subject or a name)
- [ ] the gate passes

### Task 15: Meetings' notifications and the calendar's reminders
**Files:**
- Modify: `crates/teams-ui/src/*`

- [ ] tests first: a meeting starting soon notifies with Join (the agenda's setting: on by default, off); a meeting
      that started in a chat the account is in shows Join on the chat; nothing joins by itself
- [ ] the gate passes

### Task 16: The app against the web client in a meeting (live)
**Files:**
- Create: `crates/proto-lab/src/scenario/meeting_app.rs`

- [ ] `proto-lab run meeting-app` (`--role join|share|admit`): b's web client in the lab meeting with a's app, keeping
      in step by its progress lines, as `ab-call-app` does
- [ ] the gate passes

### Task 17: Verify the lab acceptance
**Files:**
- Create: `crates/teams-app/tests/meeting_live.rs` (`required-features = ["live"]`), `docs/m4-acceptance.md`

- [ ] the live test is an orchestrator only, as `call_live` is: a sandbox per app with its private PipeWire, fake
      camera and fake screen, the normally built (release) app for a, b and c with each broker outside, keyboard only,
      traces, recordings and `fence.log`; b's web side from a spawned `proto-lab run meeting-app`
- [ ] (`PROTO_LAB_LIVE=1`): a's agenda shows the `lab` meeting; a joins from it, b from the meeting card, c from the
      link; audio and video among all three; b's web client in the same meeting (the mixed case); a message in the
      meeting chat during the meeting; b's web client shares and a and c see it; a shares and b and c see it; everyone
      leaves
- [ ] the lobby phase, after it: a's app joins the `lobby` meeting, c's app joins it by its link and waits in its
      lobby, a admits it, audio both ways; both leave
- [ ] numbers: join time, admit time, a shared screen's frames per second and delay, CPU per process while sharing,
      with the load average
- [ ] `fence.log`: writes only as the lab rule lets them, in the lab meetings and chats; no deny of a mapped write;
      screenshots and recordings under the lab's data dir
- [ ] `call_live`, `ui_live`, `chat_live`, `ab-chat` and `abc-chat` still pass; the gate passes;
      `docs/m4-acceptance.md` (numbers, the fence's picture, flakes, any breached run next to its clean rerun)

### Task 18: The personal build's meetings (after Task 1's approval)
**Files:**
- Modify: `crates/teams/src/*`, `crates/teams/tests/*`, `crates/teams-fence/src/decision/personal_calls.rs`,
  `crates/teams-fence/tests/*`, `crates/teams-media/src/devices/*`, `docs/personal-build.md`

- [ ] ⚠️ the user's real-call run (M3) reported and its problems fixed first
- [ ] tests first on the mock (`teams`' lib): the calendar shown; a meeting the calendar or a chat lists joined on the
      user's action, its meeting chat read fresh as the approved rule says; the lobby; the meeting chat's writes;
      a shared screen seen; the account's screen shared only on the user's action through the portal; every meeting
      write with its `fence.log` line; the registry binds a meeting's links as it binds a call's
- [ ] the personal build's screen through the desktop's screen-cast portal (fixed when built); `cargo test -p teams`
      opens no portal of the desktop's; agents build no runnable personal binary
- [ ] `docs/personal-build.md`'s meetings row and the first run's note, if it lists what the build sends
- [ ] `cargo test -p teams -p teams-fence -p teams-call -p teams-ui -p fence-seal` and the gate pass

### Task 19: The user's real-meeting checklist
**Files:**
- Modify: `docs/m4-acceptance.md`

- [ ] the checklist, each step with what the user should see and what to send back if it fails: the calendar; a
      meeting joined from the calendar, from a chat's card and from a link; a meeting with a lobby (waiting, and
      admitting a colleague when the user organizes); the meeting chat; a colleague's shared screen; the user's own
      screen shared; a long meeting across a token renewal
- [ ] ⚠️ stop here for the user's run; each reported problem fixed (tests first, on the mock or in the lab) and the
      step rerun by the user before Task 20
- [ ] the outcome recorded in `docs/m4-acceptance.md` (the user's report, in their words where they give them)

### Task 20: [Final] Update documentation
- [ ] README.md: the calendar, meetings, the lobby, screen sharing, the new scenarios and `teams-headless` commands,
      the live meeting test, the sandbox's screen
- [ ] CLAUDE.md: the patterns M4 settled (the meeting rows, the lobby, the sharing line, the screen's portal and
      which one each build opens), current rules only
- [ ] `docs/protocol/endpoints.md` checked against every endpoint M4 touched
- [ ] write the M5 plan (the long tail), informed by M4
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
cargo build --release -p proto-lab -p teams-headless -p teams-app --target-dir target/lab
target/lab/debug/proto-lab run lab-meeting --headless
target/lab/debug/proto-lab run meeting-join --headless
target/lab/debug/proto-lab run meeting-lobby --headless
target/lab/debug/proto-lab run meeting-share --headless
target/lab/debug/proto-lab run meeting-native --headless
target/lab/release/teams-headless a calendar
target/lab/release/teams-headless a meeting join lab --seconds 45
TEAMS_APP_BIN=$PWD/target/lab/release/teams-app PROTO_LAB_BIN=$PWD/target/lab/release/proto-lab PROTO_LAB_LIVE=1 \
  cargo test -p teams-app --features live --test meeting_live
```

Every live command above runs under `flock ~/.local/share/teams-lab/live.lock` with an outer `timeout --signal=INT`.
The personal build is never built for or run against the tenant by an agent.

## Roadmap after M4
- **M5 — long tail**: breakout rooms, live captions, the recording indicator, background blur (ONNX), PSTN,
  `teams-webhost` (WebKitGTK + a JS SDK shim: tabs, Loop, Whiteboard), and bots and message extensions with Adaptive
  Cards and their actions (`docs/backlog/bot-invokes-and-message-extensions.md`).
- **Work that waits for evidence or the user** (any milestone that needs it): image and file sending
  (`docs/backlog/ams-image-sending.md`, `file-sending.md`); channel writes and channel meetings if the user approves a
  team of the lab accounts only (`docs/backlog/channel-writes-need-a-lab-only-team.md`); creating meetings natively;
  starting new chats and the server's search, in the personal build only; `add` and call waiting in the personal
  build.
- **Cross-cutting**: drift detection (a scheduled bundle fetch and a diff report; it also watches `teamsEchoBotMri`),
  the sanitizer's known gaps, `login`'s one-time-code prompt, the debug build's keystroke lag, hardware video encoding
  (VA-API) if Task 17's CPU numbers for a shared screen call for it.

## Post-Completion
*Items requiring a human or a decision — informational only.*

**Manual steps:**
- Lab: none expected. If Microsoft asks a lab account for a one-time code, `login` stops and the user is asked.
- The user's real meetings are Task 19, inside the plan; the user's real-call run (M3) comes before Task 18.

**Decisions for the user:**
- **The meeting rules' wording** (Task 1): the lab's lobby and admit, and the personal build's meetings and screen
  sharing.
- **A lab-only team** (backlog): still open; channel meetings would need it.
- **OpenH264 in the personal build** (backlog): a shared screen is H.264 too.
- **Hardware video encoding**, if sharing a screen in software is too heavy for daily meetings.
