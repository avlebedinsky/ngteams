# Calling: lab-to-lab calls in the web client

Status: 2026-10-24, M3 Tasks 5 and 6; [The SDP dialect](#the-sdp-dialect) 2026-10-25, M3 Task 7. Written from live runs of `proto-lab run ab-call` (a calls b from their 1:1 chat) and
`proto-lab run abc-call` (a calls the lab's a–b–c group chat), each step of `--call` (below), Chrome for Testing 155
headless, muted, with fake devices, the fence armed with the lab rule. The captures stay outside the repository; the
fixtures in `tests/fixtures/calling/chat/` are synthetic, shaped like them. Ids, hosts' numbers, tokens and addresses
below are placeholders. The echo test call is in [calling-echo.md](calling-echo.md); the fence's side of a call is in
[fence-targets.md](fence-targets.md#the-labs-call-rule): a call is judged by its entry (its start, a join, an answer
or a decline, an `add`), and what it sends once it was let in passes.

The code: the scenarios are `crates/proto-lab/src/scenario/ab_call.rs` and `abc_call.rs`; the start's row is
`chat_call_start`, the callee's and the call's operations `follow_up`, in `crates/teams-fence/src/contacting/call.rs`;
offered invitations are `crates/teams-fence/src/calls/offer.rs`. The native client's call requests are
`crates/teams-proto/src/write/call.rs` (each built in the shape below and classified by the same rows), its typed
delivery responses `crates/teams-proto/src/frame.rs`.

## The runs

| `--call` | Browsers | What is held back (labelled forced denies) | What happens |
|---|---|---|---|
| `start-held` | a | every `POST epconv` (`the call start, held back`) | a clicks the call button(s); nothing rings |
| `answer` | a, each callee | nothing: the fence judges the answer, and the rest of the call passes | each callee answers by the run's click (only on the run's own lab-only invitation), the call connects, every page turns its noise suppression off and its microphone over, the audio is judged both ways by the WebRTC statistics (the tone noted when heard), then each callee hangs up |
| `decline` | a, each callee | as `answer` | each callee declines by the run's click, its rejection let through by the fence; a hangs up when its call is still up |
| `video` | a, each callee | as `answer` | as `answer`, and once the audio was judged every page turns its camera on; each must decode video |
| `dropped` | a, each callee | as `answer` | as `answer`, and the last callee's page goes off the network for 10 s with its calling socket closed by the run (it must close; none closed fails the run), then comes back; 25 s later every page must still show the call |
| `join` | a, b and c (`abc-call`) | as `answer` | b answers; once c's ring ended c opens the chat and, only on the run's own call with the run's chat shown and one join, clicks the header's join of the running call, which the fence judges by the chat it names; c must then show the call screen, and its capture the join let through |
| `cancel` | a, each callee | as `answer` | a hangs up 8 s after it placed the call |
| `timeout` | a, each callee | as `cancel` | nobody acts for 50 s, then a hangs up if its call is still up |

Before a calls, every page's calling socket must be up (a socket naming the calling stack's client, `SkypeSpaces`, that
received `trouter.connected`): a call placed or rung before it reaches nobody (seen: a callee whose calling socket
wasn't up got no invitation; a caller whose socket reconnected mid-call missed its acceptance and posted
`notificationLinks` and its acknowledgement by request, which no row maps).

Each callee opens the Calls app first: the client starts its calling stack only then (as for the test call,
[calling-echo.md](calling-echo.md#what-has-to-be-up-before-a-call)). a loads at the chat, opens the Calls app, and goes
back to the chat with the Chat app's button (in the same document: a deep link would reload the page and lose the
stack).

## Placing a call from a chat

A 1:1 chat's header shows one call button, the kind placed last (`default-chat-call-audio-button` or
`default-chat-call-video-button`), and the other in its options (`dropdown-calling-toggle-more-options-btn`, then
`chat-call-audio-button` / `chat-call-video-button`). A group chat's header shows **Meet now** (`audio-drop-in-button`,
which starts a meeting: M4) and its options (`audio-drop-in-more-options-button`) hold one item, **Audio call**
(`audio-drop-in-dropdown-menu-item-audio-button`); a group chat offers no video call to start. The group call asks for
a confirmation (`group-start-call-confirmation-btn`) before it rings everyone.

## The start

`POST https://api.flightproxy.teams.microsoft.com/api/v2/epconv` with no query, as the test call's
([calling-echo.md](calling-echo.md#the-call-step-by-step)), with these differences:

- `conversationRequest` has **no `scenario`** (the test call's is `UserInitiatedTestCall`);
- `participants.to` names the callees: `[{id: "8:orgid:<guid>", participantId: "<guid>"}]`, one for a 1:1 call, every
  other member for the group call (b and c);
- `groupChat` is `null` for a 1:1 call (it names only its callee) and `{threadId: "19:<id>@thread.v2", messageId:
  null}` for the group call; `groupContext`, `meetingInfo`, `meetingData`, `contentSharing`, `capabilities` stay
  `null`;
- `callInvitation.callModalities` is `["Audio"]` for a 1:1 call and `["Audio", "ScreenViewer"]` for the group call;
- `callInvitation.mediaContent` carries `requiredFeatures: "nonByPass"` in a 1:1 call and `mediaDescriptions`
  (`descriptions: [{mid, direction}]`, `requestId`) in the group call, besides the test call's `blob`, `contentType`
  (`application/sdp-ngc-1.0`), `clientLocation`, `applyChannelParameters` and `mediaLegId`.

**Video.** The header's video call posts the same start as its audio call: `callModalities` stays `["Audio"]`, and the
offer is the same (an audio line `sendrecv`, the main video line `recvonly`, an application sharing video line
`inactive`, a data line `sendrecv`). The camera comes later, on the running call ([Video](#video)). The group
call's offer has one audio line and eleven video lines (six `sendrecv` main video lines, four `recvonly`, one
application sharing line) and a data line; its `mediaDescriptions` lists the `recvonly` ones.

**A failed start** (held back by the forced deny) is tried twice more by the client, about a second apart, with the
same body; then it shows a retry screen (`calling-retry-screen`, `calling-retry-cancelbutton`).

**The fence** maps it as a call in the chat it carries (`WriteKind::Call` on `Scope::Conversation`, the caller and
every callee added): the group chat `groupChat.threadId` names, or for a 1:1 start the 1:1 chat of the caller and its
one callee as the chat service names it. Only the captured shape maps; anything else on that URL is unreadable
([fence-targets.md](fence-targets.md#the-labs-call-rule)). The start's answer is the conversation, as for the test
call.

## The invitation

The callee's calling stack's endpoint receives the invitation over Trouter: a delivery at the endpoint's root
(`POST /v4/f/<endpoint>/`), its body `{"evt": <n>, "gp": "<base64 JSON>"}`, `evt` 107 for a 1:1 call and 109 for a
group call. Decoded, `gp` holds:

- `callNotification`: `from` (the caller: its MRI, display name, endpoint, participant id, tenant, region), `to` (the
  callee, endpoint all zeros), `links` (`attach`, `progress` and `reject`, each
  `https://api.flightproxy.teams.microsoft.com/api/v2/ep/<cc host>/cc/v1/forked/<call>/<n>/i1/<n>/<op>?i=…`;
  `mediaAnswer: "cc://ma"`; `udpTransport: "udp://<relay>:3478/"`), `mediaContent` (the caller's offer in a 1:1
  call, `sdp-ngc-1.0`, with `requiredFeatures`; in a group call the call's mixer's, `sdp-ngc-0.5`, `fromMixer`,
  `originator: "mcGvc"`, `skipIceReinvite`), `udpKey` (`sessionKey`, `ticket`), the caller's capabilities;
  in a group call also `fromMixer`, `conversationType: "groupCall"`, `isDialOut: true`;
- `conversationInvitation`: the `conversationController` (the conversation's URL on the flight proxy),
  `isMultiParty`, `isBroadcast`; in a group call also `groupCallInitiator` (the caller's MRI) and `isHostless`;
- in a group call `groupChat` (`threadId`, `messageId: "0"`) and `meetingProperties`;
- `debugContent` (the call id, the call controller's instance, the start time).

The invitation names only the caller and the one callee it is addressed to; a group call's other callees appear in no
field of it (only the group chat's id does).

## What the callee sends while it rings

At once (within a fraction of a second) the callee's page answers the delivery and posts the invitation's `attach`
link; when the attach's answer comes it posts `progress` and subscribes to the call's broker (`GET …/broker…/api/v1/
subscribe/<id>/<n>`, a read), and only then shows the incoming call.

- **The delivery response**: `3:::{"id": <the delivery's>, "status": 200, "headers": {"MS-CV"?, "trouter-request": <the
  delivery's>, "trouter-client": "{\"cd\":<n>}"}, "body": ""}` (with `MS-CV` or without: both seen). Held back, the
  service delivers the invitation again three times, about 2 s apart, and the page answers each again `404` with a
  `Trouter-Responder: ClientLib` header.
- **The `attach`**: `{"attach": {"requireMediaContent": false, "links": {"end": <a callback of its own>},
  "locationContent"/"networkContent"/"areaContent": null}, "capabilities": null, "endpointCapabilities": <n>,
  "additionalActions": [{"name": "join", "url": <the conversationController>, "waitForResponse": true, "input":
  {"conversationRequest": {"roster": {"type": "Delta", "rosterUpdate": <callback>}, "links": {<six conversation
  callbacks>}}, "endpointMetadata": {}, "participants": {"from": <the callee>}, …}}], "debugContent": {"ecsEtag"}}`,
  one shape in the 1:1 and the group runs. **It doesn't join the call**: the web client builds it as the leg's attach
  plus a roster-only subscription to the call's conversation (no `callInvitation`, no media; the same kind of join the
  client makes to watch a chat's running call), and moves the call to ringing only once the answer came. The run's
  rosters listed no endpoint of the callee in the call until it accepted (only its voicemail's once that took an
  unanswered call). Its answer hands out the call leg's links (`callInvitation.links`: `progress`, `acceptance`
  (`…/cc/v1/incoming/<call>/<n>/t/<n>/accept`), `callLeg` (`…/reject`), `redirection`, `newOffer`, `mediaAnswer`,
  `subscribe`) and, in `additionalActionResponses[0].output`, the conversation: its roster, state and `links` (`leave`,
  `updateEndpointState`, `updateEndpointMetadata`, `updateParticipantProperties`, `addParticipant`, …). Held back, the
  page retries it twice, 4 s apart.
- **The `progress`**: `{"callProgress": {"sender": <the callee>, "status": "ringing", "phrase": "ringing"}}` to the
  answer's `progress` link; a group call's callee sends it again every few seconds while it rings. Held back, the call
  controller drops the callee after about 30 s with "Call timed out without any signaling after attach from callee
  endpoints" (`408`) at the attach's `call/end` callback, and the call goes to voicemail.

**The incoming call** shows as a calling notification in the page's in-app notifications area
(`[data-testid="notification-wrapper"]`, labelled by `cn-calling-main-content-…`, "<caller> is calling you"), with three
buttons named only by their labels: **Accept with video**, **Accept with audio**, **Decline call**. No incoming call
showed while the delivery response and the `attach` were held back (1:1 and group runs alike).

## The answer

On **Accept with audio** the callee's page posts, in order:

- its endpoint state to the conversation subscription's `updateEndpointState` link: `{"from": <the callee>,
  "endpointState": {"state": {"isMuted": false}, "endpointProperties": {…}, "endpointStateSequenceNumber": 1}}`;
- its acceptance to the attach answer's `acceptance` link: `{"callAcceptance": {"acceptedBy": <the callee>,
  "acceptedCallModalities": ["Audio"] (a group call's `["Audio", "ScreenViewer"]`), "capabilities": null,
  "endpointCapabilities", "clientEndpointCapabilities", "links": {<its callbacks: mediaRenegotiation, transfer,
  replacement, balanceUpdate, retargetCompletion, controlVideoStreaming, updateMediaDescriptions>},
  "clientContentForMediaController": {"controlVideoStreaming", "csrcInfo"}, "mediaContent": {<its answer>},
  "pstnContent", "callKeepAliveInterval": null}}`. Its answer is a `callAcceptanceAcknowledgement` inline (the callee
  gets no acceptance delivery): the call leg's `links` (`callLeg` `…/cc/v1/active/<call>/…`, `mediaRenegotiation`
  `…/renegotiate`, `updateMediaDescriptions`, `updateCallState`, `hold`, `monitor`, `transfer`, `replacement`,
  `retargetCompletion`, `startOutgoingNegotiation`, and in a group call `applyChannelParameters` on `mcProxy`) and
  `callKeepAliveInterval` (2700 s): what goes on it, at the `callLeg` link, is in [The keep-alive](#the-keep-alive).

Once the call is up it puts its endpoint metadata (`PUT …/updateEndpointMetadata`, as the caller does) and posts its own
participant properties (`updateParticipantProperties`: `{"from", "to": [<itself, its aiVoiceConsent>], "links":
{"updateParticipantPropertiesStatus"}, "operationId"}`), both to the conversation subscription's links. When it hangs
up it posts the conversation's `leave`, in the caller's `leave` shape. With the accept held back, the page tried it
three times, 4 s apart, then gave the leg up with `DELETE …/reject` and `callEnd` code `490`
`CallEndReasonFetchCorsOrNetwork` (not a decline: the fence maps only the user's).

**How the media goes** (for Task 7): in a 1:1 call the invitation carries the caller's offer (`sdp-ngc-1.0`,
`requiredFeatures: "nonByPass"`) and the callee's `accept` carries its answer (`mediaContent`: `blob`, `contentType`
`application/sdp-ngc-1.0`, `clientLocation`, `applyChannelParameters`, `mediaLegId`; the web client's own host
candidates, `a=setup:active`, the audio line `sendrecv`, the main video line `inactive`, a data line); `attach` asks no
media (`requireMediaContent: false`). In a group call the invitation carries the call's mixer's offer (`sdp-ngc-0.5`,
`fromMixer`, `originator: "mcGvc"`), the callee answers it in its `accept`, and the mixer then renegotiates with each
participant: a `mediaNegotiation` delivered at the participant's `mediaRenegotiation` callback (the mixer's offer, its
`links.mediaAnswer` `…/cc/v1/negotiations/<call>/…/answer` and `rejection`), answered by a `POST` of `{"mediaAnswer":
{"callModalities", "sender", "links": {"mediaAcknowledgement"}, "clientContentForMediaController", "mediaContent"}}`.

## The decline

On **Decline call** the callee's page sends `DELETE` to the attach answer's `callLeg` link (`…/cc/v1/incoming/<call>/…/
reject`) with `{"callEnd": {"code": 603, "subCode": 0, "phrase": "CallEndReasonLocalUserInitiated", "resultCategories":
["Success"]}}`; held back, it tried three times, 4 s apart. Let through, the callee's ring ends at once. In a 1:1 call
the call controller forwards the declined call to the callee's voicemail (a receives `callProgress` `forwarded`,
`forwardingDestinationType: "voicemail"`, and the voicemail's acceptance, as for an unanswered call): the caller's
call connects to the voicemail. In a group call the caller receives `addParticipantFailure` for that callee (`code
580`, subcode 5208, "… since the user declined …"), and once nobody else is left the call's `end`.

## Video

In a 1:1 call the camera comes on the running call by a renegotiation: the page that turns its camera on posts
`{"mediaNegotiation": {"callModalities": ["Audio", "Video"], "sender", "links": {"mediaAnswer", "rejection": <its
callbacks>}, "mediaContent": {"blob", "contentType", "requiredFeatures", "clientLocation", "negotiationTag":
"<id>;v_1", "applyChannelParameters", "mediaLegId"}}}` to the call controller's `mediaRenegotiation` link (the
caller's from its acceptance, the callee's from its acceptance's acknowledgement); the peer receives it at its own
`mediaRenegotiation` callback and answers with `POST …/negotiations/<call>/…/answer` (as above); the answer comes back
at the renegotiating page's `mediaAnswer` callback, carrying a `links.mediaAcknowledgement`
(`…/cc/v1/mediaAnswers/<call>/…/acknowledge`), which the callee acknowledged with an empty `POST`. Both pages then
decoded video. In a group call the camera goes on with `POST …/cc/v1/active/<call>/…/updateMediaDescriptions`
(`{"UpdateMediaDescriptions": {"mediaDescriptions": {"descriptions": [{"mid": "1", "direction": "sendrecv", "label":
"main-video"}, {"mid": "2", "direction": "recvonly"}, …], "negotiationTag", "requestId"}}}`, the main video line
`sendrecv`, the others `recvonly`) and `applyChannelParameters` on `mcProxy`; every page then decoded the others'
video. **Which participant's video a page receives** is named in no request or delivery of the signaling: the
updates name only the lines' directions, and the group call's remote description carries a data channel
(`m=application … webrtc-datachannel`, `a=sctp-port:5000`; the `m=x-data` line in `sdp-ngc`); the source selection
travels over that channel ("Group video" below).

### The H.264 codec

M3 Task 18's decision (with Codex, 2026-10-25): the calls' H.264 is OpenH264 (Cisco's, BSD-2-Clause, through the
`openh264` 0.9.9 binding), and which library a build uses is fixed when it is built:

- **A normal build (the lab binaries, the personal build) has no H.264 code of its own.** It loads Cisco's prebuilt
  library at run time (`libopenh264-2.6.0-linux64.8.so`, from Cisco's own download, `scripts/fetch-openh264.sh` for
  the lab), and only when its SHA-256 is the one the binding lists for it; a missing or different file means the
  call has no video and its audio goes on. Never the system's library (a distribution's rebuild is not Cisco's binary).
- **Only a `test-support` build compiles the bundled source**, so the tests run offline; such a build refuses to run
  against the tenant.
- **Copyright and patents are separate questions.** The binding's and OpenH264's BSD-2-Clause licences fit this
  project; that settles copying, not patents. Cisco's arrangement with the H.264 patent pool (MPEG LA's AVC licence)
  covers Cisco's own binary, on conditions: it is downloaded to the user's machine apart from the program, the user
  can turn its use off and on again, the program shows "OpenH264 Video Codec provided by Cisco Systems, Inc." where
  that control is, and Cisco's licence text is reproduced where licensing information is shown. It covers no build
  from source and no other copy. Nothing here claims a patent licence for the source the tests build, and neither
  private use nor this project being unpublished is taken to settle the question.
- **The lab meets those conditions now**: the script downloads the library from Cisco and saves its licence
  (`BINARY_LICENSE.txt`) beside it; `teams-headless` loads it only with `--video` (without it the library isn't
  loaded) and prints the attribution when it does; the lab app's settings show the switch "Video in calls: OpenH264
  Video Codec provided by Cisco Systems, Inc." (on by default; off, a call starts no video). The personal build's
  provisioning, switch, attribution and licence text are owed before its calls carry video (M3 Task 22), and whether
  the user's work calls should rely on Cisco's patent cover is the user's own call
  (`docs/backlog/openh264-patents-and-the-personal-build.md`).
- Hardware encoding (VA-API) stays out unless Task 21's CPU numbers call for it.

### Native video

A 1:1 call's video in the native client (M3 Task 18), as the web client's camera-on flow above shows it:

- **The camera is off at every start, join and answer**, and only the account's action turns it on
  (`CallAction::Camera`). Turned on, the native client offers its video line again (`renegotiate` to the call
  controller's link, `callModalities` `["Audio", "Video"]`), the far end answers at its `mediaAnswer` callback, and
  the native client acknowledges; turned off, the line is offered `recvonly`.
- **What the far end offers is answered by the call's video lines rule**: a 1:1 call takes the main video line (the
  first video line that isn't the sharing line, then that line for the call's life) and answers it `recvonly` while
  the camera is off, `sendrecv` while it is on; the sharing line and every other video line are answered rejected
  (`m=video 0 RTP/SAVP 34`). A group call's video lines (a start's in a group chat, a join's, a group call
  answered) follow "Group video" below; the test call's media takes no video line.
- **H.264** is offered as constrained baseline (`profile-level-id=42e01f`, `packetization-mode=1`) on the payload
  type a group call's mixer gives it, `107`, with its retransmissions on `99` (the web client offers `108` and `109`;
  the mixer answers any offer with its own numbers and takes only those, "Group video" below); an offer we answer
  keeps its own numbers. The encoder
  makes 20-30 fps camera pictures up to 1280 by 720 at a target bitrate that follows `str0m`'s bandwidth estimate
  (transport-wide congestion control) less what the audio takes, between 100 kb/s and 1.5 Mb/s; an IDR comes first,
  every 4 s, and whenever the far end asks (PLI or FIR); a receiver that loses a frame asks for one (PLI) and shows
  nothing until it comes.
- **A line the far end's offer added names its stream when it begins to send**: our offers name no line-naming
  header extension (the dialect renames our lines' mids, Task 16), so when a line we only received on turns
  `sendrecv` (our camera on) the native client declares its sending stream before it offers, and the offer names its
  SSRCs (and the retransmissions', `ssrc-group:FID`); without them the far end could tell the stream by nothing.
- **Every negotiation delivery gets its receipt.** The web client answers the renegotiation, the answer and the
  answer's acknowledgement (delivered at its `mediaAcknowledgement` callback) each with an empty `200` delivery
  response, as it answers an invitation. Native runs without them showed the call controller holding the call: the
  acknowledgement came about 22 s after the answer, and a renegotiation in that window was rejected; with the receipts
  it came in under a second. So the native client sends each receipt (`InvitationReceipt`, decided by
  `Fence::check_delivery_response` like its other delivery responses: permitted and logged under the lab's call rule).
- **A renegotiation rejected with `491`** ("Negotiation is in progress": both ends turned their cameras on at once,
  or the far end's previous negotiation isn't acknowledged yet) goes again with the same offer 3 s later, at most 5
  times, and no other renegotiation of ours begins meanwhile (the camera's button is refused: the offer waiting to
  go again is still the media's). A renegotiation of the far end's delivered while ours is being made or waits for
  its answer is held until ours settles: after our answer is applied it is answered; when ours is then turned away
  (the controller took the far end's first, as when both cameras went on together) ours gives way to it, and so it
  does to one delivered while ours waits to go again: the far end's is answered with our camera's line only
  receiving, then our camera's line is offered again as the camera goes. Any other rejection fails the call. The
  media's SDP steps (an offer, an answer, an accepted answer) run one at a time, in the order they were asked.

### Group video

A group call's video goes through the call's mixer (M3 Task 19). What follows comes from the web client's code (read
for its shapes, written here in our own words), `abc-call --call video`'s signaling captures, the native lab runs
that made it work, and `abc-call-mixed`'s captures, where the lab browser's data channel hooks (`rtc_hooks.js`:
`dc:open`, `dc:send`, `dc:message`) read the web callees' channel.

- **The lines.** The mixer's offer to a callee (and its answer to a caller's or a joiner's group offer) has audio,
  the main video line, a set of receiving video lines (nine seen), the sharing line and the data line (`m=x-data`).
  The native client takes the main line and up to nine receiving lines and rejects the sharing line; a caller's or a
  joiner's own offer has the same lines in that order. Every video line is answered (and offered) `sendrecv`, as the
  web client does (an answer `recvonly` on a receiving line is refused with `410` `SdpParsingFailure` or
  `InvalidFinalAnswerMediaDescriptions`), and each carries `a=rtcp-rsize`, `a=rtcp-fb:* …` for every payload type,
  a stream of its own (`a=x-ssrc-range`; a line we send nothing on names stream `1`, no media stream) and the
  largest picture it takes in its H.264 `fmtp` (`max-fs=3600;max-mbps=108000;max-fps=3000`). Which way a line
  really goes is told apart from the SDP, by the media descriptions. The mixer numbers H.264 `107` and its
  retransmissions `99` whatever the offer said, sends with the offer's numbers and takes only its own: a caller's
  video offered as `108` reached nobody, so the native client offers the mixer's numbers.
- **The media descriptions.** A start's, a join's and an `accept`'s `mediaContent` carries `mediaDescriptions`
  (`{"descriptions": [{"mid", "direction", "label"?}], "negotiationTag", "requestId"}`: the main line `recvonly`
  with the camera off, `sendrecv` with it on, labelled `main-video`; every receiving line `recvonly`) and
  `applyChannelParameters` seeding the mixer's send rate (`{"mids": ["*"], "mediaParameter": "{\"sendSideBWSeed\":
  {\"seedValueBitsPerSec\": …}}"}`; the native client seeds 4 Mb/s). **Without the seed the mixer sends no video
  on the receiving lines** (native runs: source requests answered `ok`, audio flowing, no video RTP; with it, video
  came within seconds). The mixer renegotiates the media now and then (`fromMixer`); the answer to each carries the
  descriptions again (`requestId` counting on) and the seed, as the web client's do: native callees whose answers
  lacked them stopped receiving video after the first renegotiation. The camera turns on and off with
  `updateMediaDescriptions` (the main line `sendrecv` or `recvonly`, the request numbered on) and, turning on,
  `applyChannelParameters` with the main line's send capabilities (`maxVideoSendCapabilities`, one stream); no SDP
  changes. A group call's own video is sent at most 640 by 360 at 600 kb/s.
- **The data channel and the control plane.** The client opens a channel labelled `main-channel` on the data line.
  Every message on it is a packet: an 11-byte header (the format version in the high nibble and the header's length,
  a start flag with the data id, a 16-bit sequence number, how many packets follow, the sender's 32-bit source id,
  how many recipients), the recipients (4 bytes each) and the payload. The control plane is data id `18` addressed
  to the mixer (`0xFFFFFFFC`), the sender the data line's own stream id; its payload is a version byte `1` and a
  JSON array of messages. The client opens it with `syn` naming what it takes (the web client `["dsh", "bwe",
  "sr", "ssbwe", "leave", "vdclc", "heartbeat"]`, the native client only `["dsh", "sr"]`), the mixer answers `ack`
  (it sends `bwe`, `dsh`, `sr_res`, `heartbeat`, `vdclc`; it takes `sr`, `ssbwe`, `leave`, `heartbeat`); then the
  client sends source requests (`sr`, `{"controlVideoStreaming": {"sequenceNumber", "controlInfo": {"sourceId",
  "streamMsid", "fmtParams": [{"max-fs", "max-mbps", "max-fps", "profile-level-id"}]}}}`): per receiving line its
  stream id (`streamMsid`, the line's `a=x-source-streamid`), the participant's video `sourceId` (from the roster;
  `-1` to stop a line) and the largest picture it takes (`max-fs` in macroblocks, `max-fps` in hundredths of a
  frame per second; the web client asks up to 1920 by 1080, `max-fs` 8160, the native client 1280 by 720); the
  mixer answers each `sr_res` `ok`. The mixer tells who spoke last as `dsh` (the dominant speakers' audio source
  ids); the web client also trades `heartbeat`s with it every second, gets its estimate of the bandwidth (`bwe`)
  and the sending constraints for its own video (`vdclc`, its `fmtParamsList`, e.g. `max-fs=2040`, `max-br=482`),
  and tells the bandwidth it takes per line (`ssbwe`); the native client does none of these and runs calls of
  minutes without them. A request goes only when what a line should show changes, and names a participant's source only while its
  camera is on (the roster says so), `-1` once it goes off. In one native run a caller that asked for both callees
  as they connected, before their cameras were on, got `ok` and no video at all, while a callee that asked for the
  other before its camera came on did get it; why is not settled, and asking again when the camera comes on gave
  every participant the others' video in the runs after.
- **The roster.** Who is in the call, and their sources, come only in `rosterUpdate` deliveries at the
  conversation's callback (`{"participants": {<MRI>: {"version", "state", "details": {"displayName"}, "endpoints":
  {<id>: {"call": {"mediaStreams": [{"type", "label", "sourceId", "direction"}]}}}}}, "type": "Delta",
  "sequenceNumber"}`): a participant is in the call while its `state` is `active` with an endpoint in the call, the
  `main-video` stream's `sourceId` is what a source request names and its direction says whether its camera is on,
  the `main-audio` stream's `sourceId` is what `dsh` names; a newer `version` replaces an older one. An update
  lists only who changed, so a client knows the others only from what it was told before: a callee's `attach`
  subscribes to the roster while it rings, and the service goes on delivering updates at that subscription's
  callback after the ring ended; a later join gets only the changes from then on, at both its own callback and the
  `attach`'s (the web joiner's captures), so the native joiner starts from the roster its invitation's `attach` was
  told and takes the updates at both. The native
  client reads it only to show the call (who is in it, whose video is on which line, who speaks); it sends nothing
  for it, and the fence reads no roster.

## Joining a running call

When a group call's callee lets its ring end unanswered, its page at once subscribes to the call's conversation (the
invitation's `conversationController`) with a roster-only join: `POST …/conv/<id>` with `{"conversationRequest":
{"subject": null, "roster", "properties", "links"}, "groupContext": null, "groupChat": {"threadId", "messageId": "0"},
"participants": {"from"}, "capabilities": null, "endpointCapabilities", "clientEndpointCapabilities",
"endpointMetadata", "meetingInfo": {"organizerId", "tenantId", "isBroadcast"}, "endpointState", "debugContent":
{"ecsEtag", "causeId"}}` (no `callInvitation`). Its answer is the conversation (roster, state, links,
`activeModalities`, `subscriptionDetails`). Only with it does the chat's header show the running call's join (a
control whose `data-tid` names a join in `chat-calling-meeting-buttons`); with it held back the header showed only
**Meet now**. Clicking join posts the same request with a `callInvitation` (its callbacks, its offer with
`mediaDescriptions`, `pstnContent`, `callModalities: ["Audio", "ScreenViewer"]`), the subject a string and the
`participantPropertyBag`; let through, its answer is the conversation, an acceptance comes at its own `acceptance`
callback, and it puts its metadata and acknowledges by request, as a caller does. The caller's own roster join after
its call ended has the same shape. A start in a group chat whose call still runs (one left behind by a broken run)
ends at once with `conversationEnd` `409`, `subCode` `5704`, "Conversation cannot be created due to a failure to
resolve ownership", naming the running conversation (`conversationUrl`) and the chat (`threadId`); the captures show no
join of that one: the client posts a roster-only join to its own new conversation four times and sends no `leave`
(seen 2026-10-26). A native start ended so ends its call at once (`End::Remote`, printed by `teams-headless` as
`remote 409 chat has a running call <conversation>`). The service refused a chat's call for about a minute after its
last call ended, too (seen 2026-10-26, starts ~60 s after a cleanly left call; ones 78 s and later went):
`scripts/native-call-run.sh` leaves 90 s between calls in a chat. A failed `ab-call` or `abc-call` run hangs up every
page that shows its call before it closes, so it leaves none running.

## A call held past a dropped socket

`dropped`: with the callee's page off the network (DevTools' emulation fails new requests; open sockets stay, so the run
closes the calling socket itself) its calling socket closed; while offline nothing reconnects. Back online the page
allocates a new Trouter connection (`POST …trouter…/v4/a`), registers its endpoint again (`POST …/registrar/prod/V2/
registrations`, a plain request the fence lets through), and its new connection ran over socket.io XHR polling
(`POST …/socket.io/1/xhr-polling/<id>`). It sent nothing to the call: no `notificationLinks`, no link of the call
changed in the 25 s after; the call's media went on, the call stayed up on both pages, and the callee's `leave`
afterwards passed on the same links.

## The keep-alive

`ab-call --call keep-alive` (2026-10-25) held an answered a → b call 47 minutes with every page's requests recorded.
Two keep-alives came, from each page:

- **The call's** (`callKeepAliveInterval`, 2700 s in every acceptance and acknowledgement): 2430 s into the call (90 %
  of the interval, as the bundle's `scheduleKeepAlives` sets its timer), each page posted `{"callParticipantUpdate":
  {}}` to its call leg's own `callLeg` link: the caller's from its acceptance (`…/cc/v1/active/<call>/<n>/<leg>/<n>
  ?i&e`), the callee's from its accept's answer (the same form, a leg of its own). Denied (the caller's as unreadable,
  the callee's held back), each page tried it again twice, 4 s apart, then waited for the next interval. Its answer
  isn't captured. Mapped since: `CallOperation::KeepAlive` to the call leg's own link, exactly that body, passing once
  the call was let in ([endpoints.md](endpoints.md#calling-teamsmicrosoftcomtrap-apiflightproxyteamsmicrosoftcomapiv2));
  the native client sends it the same way (`teams-call`, every 90 % of the interval while the call is up, a keep-alive
  not taken ending nothing). Live, a native a → b call held 42 minutes: both ends sent it 2430 s into the call, each
  answered `202`, every write of the call permitted.
- **The conversation's** (`sendConversationKeepAlive`, its period from the client's configuration): about 2200 s into
  the call each page posted its conversation subscription again (`participants.from`, `roster`, the conversation's
  callbacks) to the conversation's `notificationLinks` link, tried twice more 4 s apart. Not mapped: no native
  request is made for it.

With both denied throughout, the call stayed up on both pages past 2820 s, its media going on, until b hung up: neither
keep-alive missing ended it within the run.

## Native lab calls

M3 Task 16, release lab builds of `teams-headless` (`scripts/native-call-run.sh`, `proto-lab run ab-call-native`), each
writing through its own fence. Setup is from the caller's placing to its media connected, the callee's answer by
`watch --answer-lab-calls` in it; latency is a keyed tone's onset at the sender's microphone to its onset in the far
end's recording, the two processes' files lined up by when their devices opened (one clock, one machine):

| Call | Runs | Setup | Latency p50 (per direction and run) |
|---|---|---|---|
| a → b, native both ends | 5 | 4.3–6.2 s | 147–211 ms (p95 up to 230 ms, one outlier block at 2 s) |
| a → a–b–c group chat, b and c native | 2 | 3.2–3.7 s | 281–322 ms (through the call's mixer; p95 up to 393 ms) |
| a → group, b answering, c joining once its ring ran out | 1 | a 3.6 s; c's join 1.9 s | 239–290 ms |

The web client and the native client, both ways (`ab-call-native`): a's native call to b's page connected and the
page heard a's tone and sent and received audio by its statistics, a heard b's; b's page's call to a's native client,
answered by `watch --answer-lab-calls`, the same both ways; and with b's native client up beside b's page, a's call
answered on the page ended the native client's ring (below). Every write of the native clients was permitted on its
call; a native callee whose call ended while its endpoint metadata was on its way had that write denied
(`call_ended`) and nothing sent.

## The ring's ends

- **The caller cancels** (`cancel`): a's `leave` on the conversation, its `callTransactionEnd` with `code 487`,
  `phrase: "CallEndReasonLocalUserInitiated"` and `callQualityDiagnosticsInformation: {cancelationDuration: <s>}`
  (mapped as the call's `leave`); a receives its call's `conversationEnd` and call `end` deliveries. A callee whose
  `attach` went receives the call's end at its attach's `call/end` callback (which ends its offered invitation); one
  whose `attach` never went receives nothing of the call after it: a missed call appears in its activity feed
  (`48:notifications`, `activityType: call`, `activitySubtype: missedCall`) and its call log (`48:calllogs`).
- **Nobody acts, 1:1** (`timeout`): about 22 s after the start a receives a `callProgress` at its `progress` callback,
  `status: "forwarded"`, `forwardingDestinationType: "voicemail"`, then an acceptance at its `acceptance` callback whose
  `acceptedBy` is the callee's MRI with `endpointType: "voicemail"`: the callee's own voicemail takes the call, and
  a's call connects to it (its acknowledgement and a's follow-ups pass). The
  roster then lists the callee with a voicemail endpoint (`GraphCommunicationsClient-Voicemail`). a's tone is
  recorded into the callee's voicemail, which reaches only the callee's own mailbox.
- **Nobody acts, group call** (`timeout`): the call's mixer accepts a's leg at once (about 3 s after the start, an
  acceptance at a's callback, before anyone answered); at about 25 s each callee's `addParticipantFailure` arrives,
  then the call's `end` and `conversationEnd`; the group call ends without voicemail.
- **The caller's join after it left** (group calls): after its call ended or it hung up, a's page posts a roster-only
  join to the conversation controller itself ([Joining a running call](#joining-a-running-call)), and retries it twice,
  4 s apart. The fence judges it by the group chat it names, lab-only, and lets it through.
- **Answered, declined**: [The answer](#the-answer), [The decline](#the-decline).
- **Answered elsewhere** (`ab-call-native`'s third phase: b's web client and b's native client up together, a's call
  answered on b's page): b's native client's ring ends 1.2–1.6 s after the page's click with a delivery at its
  attach's `call/end` callback, `code 487`, `phrase: "Call cancelled as it was accepted by another fork."`; it sends
  nothing for it.
- **A group callee's ring that runs out** (`teams-headless c join abc`, nobody acting on c's endpoint): about 55 s
  after the invitation, `code 487`, `phrase: "Call cancelled on timeout."`, at the attach's `call/end` callback; the
  call runs on, and the invitation's conversation stays joinable.

## The SDP dialect

Every offer and answer in the signaling is a `mediaContent` object: the SDP as `blob` and `contentType`
`application/sdp-ngc-1.0`, in every start, acceptance, `accept`, `mediaNegotiation` and `mediaAnswer` of the runs,
whoever wrote it. The one exception is a group call's invitation: the mixer's offer there is
`application/sdp-ngc-0.5` (below). The web client turns each into standard SDP before its `RTCPeerConnection` takes it,
and its own standard SDP into the dialect before it sends it; both forms were captured side by side (the WebRTC hooks
record what the page hands its peer connection). `teams-media`'s `sdp` module does the same conversion for `str0m`
(fixture `crates/teams-media/tests/fixtures/sdp/captured.json`, synthetic).

**From standard SDP to the dialect** (what the web client sends):

- the session: `o=- <id> <version> IN IP4 127.0.0.1`, `s=-`, `b=CT:4000`, `t=0 0`, `a=extmap-allow-mixed`,
  `a=msid-semantic: WMS *`, the bundle group with the lines' mids (`0`, `1`, …, the line's place in the offer);
- each line on `RTP/SAVP` instead of `UDP/TLS/RTP/SAVPF`, its port and `c=` those of the default candidate (in the
  web client's offer its relayed one) instead of the placeholder `9` and `0.0.0.0`; the gathered candidates inline
  in the first line only (the bundled lines carry none); an offer adds `a=rtcp:<port>`, an answer doesn't;
- Teams' own lines: `a=x-signaling-fb:* x-message app recv:dsh` on audio and `… send:src recv:src,vc` on video,
  `a=x-ssrc-range:<n>-<n>` naming the line's first SSRC, and `a=label` last (`main-audio`, `main-video`,
  `applicationsharing-video`, `data`);
- two extension URIs written with backslashes for slashes (`abs-send-time` and the transport-wide congestion
  control one); the others go as they are;
- the data channel's `m=application … UDP/DTLS/SCTP webrtc-datachannel` as `m=x-data <port> RTP/SAVP 127 126` with
  `a=x-data-protocol:sctp`, an `a=x-ssrc-range` of its own, `a=rtpmap:127 x-data/90000`, `a=rtpmap:126 rtx/90000`,
  `a=fmtp:126 apt=127`, `a=rtcp-mux`, then `a=sctp-port` and `a=max-message-size`;
- a rejected line as `m=<kind> 0 RTP/SAVP <payload type>` with only its label, left out of the bundle.

**From the dialect to standard SDP** (what the web client applies): the transports back; Teams' own lines, the
labels, `a=rtcp`, `b=CT` and every line its peer connection has no use for dropped; `a=rtcp-fb:*` written out per
payload type; the backslashes back to slashes; `a=sctp-port:5000` added to a data line that names none (the media
controller's); a stream that a line names only by `a=x-ssrc-range` or `a=ssrc-group` (the media controller's and the
mixer's lines) given its own `a=ssrc` line with a cname; a line with no direction is `sendrecv`; a rejected line
gets its offer's mid and `a=inactive`. Teams' candidate forms are rewritten (`UDP` in capitals, `tcp-pass` and
`tcp-act` for the TCP ones, a trailing `MTURNID <n>`).

**Who answers whom, and in which form.** In a 1:1 call both ends are clients: the callee's `accept` answers the
caller's offer directly (the callee's host candidates, `a=setup:active`). The media controller (the echo test call)
answers with its relay candidates on port 3478 over UDP and TCP, IPv4 and IPv6, `a=setup:passive`, no `ice-lite`, and
an answer line for every offered line, the data line included. Every answerer in the runs took the DTLS client's part
(`active`) except the media controller, which takes the server's.

**The mixer's dialect, `sdp-ngc-0.5`**: `RTP/AVP` lines with SDES keys (`a=crypto`, `a=cryptoscale`) beside a DTLS
fingerprint, RTCP on a second component (candidates for components 1 and 2, `a=rtcp:<port+1>`), no `a=setup`, ICE
credentials on every line but candidates only on the first two lines and on the sharing and data lines (each of those
two on ports of its own), the bundle's mids out of line order, and codecs no browser offers (Teams' own audio and video codecs beside Opus,
G.722, H.264 and AV1). The web client reads it as DTLS-SRTP: the SDES keys and the second component dropped,
`a=setup:actpass`, and only the codecs it knows; it answers in `sdp-ngc-1.0`, as it answers the mixer's later renegotiations (which come in `sdp-ngc-1.0`,
`a=setup:passive`).

**What the native client does differently**: it writes only its relayed candidate, with `raddr 0.0.0.0 rport 0`
(no local address leaves it) and a numbered foundation, refusing to write an SDP with any other candidate; it reads
only the UDP candidates of component 1 (its media goes through its UDP relay allocation, which a TCP candidate is out
of reach of); and it reads an offer's `actpass` (or the mixer's missing setup) as `passive`, so `str0m`, which would
take the server's part for `actpass`, takes the client's, as the web client does. `str0m` names its lines with mids of
its own; the conversion keeps both namings for the call's life, so a renegotiation offered by either side names the
same lines. A line it adds is numbered one past the highest mid of the call (the mixer's numbering starts at `1` and
runs out of line order, so neither the count of lines nor a line's place is free), and a rejected line that names no
mid is the call's line at its place. It labels its lines as the web client does: the last video line of an offer with
more than one, while the call has no sharing line, is `applicationsharing-video`, the others `main-video`. It offers
Opus as `102`, the media controller's own number for it: the media controller answered an Opus-only offer's `111`
with `102` and took nothing sent as `111`, while `str0m` keeps one number per codec for both directions
([calling-echo.md](calling-echo.md#the-native-echo-call)); an answer that still numbers an offered codec otherwise is
read back with the offer's number.

Three things the native lab calls (M3 Task 16) showed live, each now the native client's way:

- **No video yet: every video line is answered rejected.** Answering a group call's mixer with `str0m`'s default
  codecs put twelve `sendrecv` H.264 lines in the `accept`; the call controller ended the leg at once with `410
  SdpParsingFailure`. With audio alone configured, `str0m` answers each video line rejected, and the dialect writes
  it as the web client writes a rejected line, `m=video 0 RTP/SAVP 34` with its label (`str0m`'s own form names no
  format at all, which no SDP may do); the mixer takes that answer. A caller's 1:1 offer's video lines are answered
  the same way, and a web client takes it.
- **A rejected line may name any profile.** The mixer's renegotiation that follows (within a fraction of a second of
  the call being up) names the lines we rejected on a profile other than `RTP/SAVP`; a line on port 0 is read as
  rejected whatever it names, and only a line in use must be on Teams' profile (the error names its kind, port and
  profile, nothing else).
- **Our offers name no line-naming header extension** (`sdes:mid`, `sdes:rtp-stream-id`,
  `sdes:repaired-rtp-stream-id`). `str0m` names its lines with mids of its own, which the dialect renames: a web client
  that answered an offer naming the mid extension took every packet of ours, carrying `str0m`'s mid, for a line it
  doesn't know and dropped it (its candidate pair counted them, no inbound stream was made); the page heard nothing.
  Without them in the answer `str0m` sends none. Our answers keep the offer's: they answer the offer's own mids.

**Opus as the lines ask for it**: every captured Opus line carries `a=ptime:20` and `a=fmtp:111
minptime=10;useinbandfec=1` (the media controller's answer adds `usedtx=1`). The native client sends 20 ms packets
of mono Opus with in-band FEC (each packet also carries a coarse copy of the one before it), turns DTX on only when
the far end's line asks for it (silence then goes out as a keep-alive now and then, and the media time keeps counting
through the gap), and on receipt plays packets in media-time order from a jitter buffer kept near its depth, decoding
a lost packet from the next one's FEC when it has it and concealing it otherwise.

## Seen beside the call

- The caller's own call log's read horizon: `PUT …/v1/users/ME/conversations/48:calllogs/properties?name=consumptionhorizon`
  when a call ends; it reaches nobody, but no row carves it out, so it is denied and on the expected-deny list.
- Deliveries at an endpoint's root with `evt` 115 and 116 and a gzip'd `cp`: 115 when a participant joins a call of the
  account's (`conversationController`, `userJoinTime`, `participants`), 116 when one leaves. Read by nothing.
