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

### The media engine

The native client's media is libwebrtc's, the engine the web client's browser runs, through LiveKit's Rust bindings
(`libwebrtc` and `webrtc-sys`, Apache-2.0; their prebuilt static libwebrtc is downloaded by the build):
`teams_media::rtc`. libwebrtc does ICE, DTLS-SRTP, Opus and H.264, the jitter buffer, FEC and retransmissions,
bandwidth estimation, pacing and A/V sync; the native client keeps only what is Teams' own (the SDP dialect below,
the mixer's control plane, the media descriptions and the bandwidth seed) and the device seam.

- **Codecs**: every line is restricted to what the web client offers, Opus on audio and H.264 in packetization mode 1
  (with its retransmissions) on video, by libwebrtc's codec preferences; libwebrtc numbers them itself and sends
  with the answer's numbers, as the browser does.
- **Hardware**: libwebrtc encodes and decodes H.264 in software. NVIDIA's NVENC and NVDEC are left out of the build
  (`CUDA_HOME` unset): with a decoder alive at exit, NVIDIA's decoder library hangs the process in its exit handler. Its encoder follows its own bandwidth estimate within the call's bounds
  (`EncoderSettings`: up to 1.5 Mb/s and 30 fps; a group call's 600 kb/s and 640 by 360).
- **The audio devices stay ours**: libwebrtc runs with no audio device of its own (LiveKit's synthetic one paces its
  playout every 10 ms); the session hands it the microphone's frames after libwebrtc's own audio processing module
  (AEC3, noise suppression, AGC2) and plays its audio on the device seam's speaker, each played frame handed to the
  module as the far end.
- **The video switch**: the settings' "Video in calls" (on by default; off, a call starts no video), and
  `teams-headless` takes video with `--video`.

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
  answered) follow "Group video" below; the test call's media takes a 1:1 call's main video line, its camera turned
  on by a renegotiation the media controller answers ([calling-echo.md](calling-echo.md#the-native-echo-call)).
- **H.264** is what every video line offers (libwebrtc's own payload numbers; it sends with the answer's, the mixer's
  `107` among them), encoded by libwebrtc at the rate its bandwidth estimate allows, a keyframe whenever the far end
  asks; the camera's track stays on the main video line from its first negotiation and sends only while the camera is
  on (its encoding active), and while it is off a 1:1 call's offer writes that line `recvonly`.
- **A line the far end's offer added names its stream when it begins to send**: our offers name no line-naming
  header extension (Task 16), so the offer that turns a line we only received on `sendrecv` (our camera on) names its
  SSRCs (and the retransmissions', `ssrc-group:FID`), as libwebrtc writes them; without them the far end could tell
  the stream by nothing.
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
  a stream of its own (`a=x-ssrc-range` and its `a=ssrc`, drawn once per line for the call: nothing is sent on a
  receiving line, but one going both ways that names the placeholder stream `1` is refused with `410`
  `SdpParsingFailure`; only a line that really goes receive-only names `1`, no media stream) and the
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
  "sequenceNumber"}`): a participant is in the call while its `state` is `active` with an endpoint in the call (one
  with a `call`; a 1:1 call's endpoints list no `mediaStreams`, their `call` only `serverMuteVersion`, as its media
  goes through no mixer; both ends of a 1:1 call get the same updates, the first listing both once the callee
  accepted), the
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
`scripts/native-call-run.sh` leaves 90 s between calls in a chat (keyed by the chat, however it is named). A failed
`ab-call`, `abc-call`, `ab-call-native` or `abc-call-mixed` run hangs up every page that shows its call (each waiting
up to 10 s for its `leave`) before it stops its native clients and closes, so it leaves none running.

## Joining a meeting

Written 2026-11-02 from `ab-meeting-join` (a's Teams meeting with b, made from the scheduling form; each of a's and
b's web clients on its pre-join, neither in a call) and the native client's live runs. A meeting's Join works the same
whether its call runs yet or not, and at any time: nobody rings, and the service keeps one conversation per meeting.

The join link (`https://teams.microsoft.com/l/meetup-join/<thread, percent-encoded>/0?context={"Tid":<tenant>,
"Oid":<organizer's object id>}`) opens the web client's pre-join: the meeting's title, the camera's preview and switch,
background filters, computer audio (the microphone's switch and device, the speaker's device), phone, room or no audio,
Cancel and Join now. While it shows, the page has already posted a **roster-only subscription** to `POST
…/api/v2/epconv`: `conversationRequest` (`subject: null`, the roster and conversation callbacks, as placeholders on
`http://broker.invalid/…` until its Trouter socket is up), `groupContext: null`, `groupChat: {threadId: <the meeting's
chat>, messageId: "0"}`, `participants.from`, the endpoint's capabilities and state, `meetingInfo: {tenantId,
organizerId}`, no `callInvitation`. It answers `201` with the meeting's conversation (`conversationController`, `state`:
`isMultiParty`, `isHostless`, `conversationType: "scheduledMeeting"`, `isMeetingActivated`, `groupCallInitiator`; its
`links`, `admit` and `admitAll` among them; `meetingInfo`; `roster` with its `participantCounts`), and that answer
creates the conversation when nobody had: the first to subscribe is its `groupCallInitiator` and `originalGroupCallInitiator`.

Then the page posts its real callbacks (`POST …/conv/<id>/notificationLinks`) and on Join now its entry at the
conversation (`POST …/conv/<id>` with `callInvitation`: a join's shape, with `suppressDialout: true`, the subject `""`
and `participants.to: []`). Both are mapped since 2026-11-03 (`meeting-steps`, a's web client into its own meeting):
the entry is a join of the meeting's chat, judged by that chat read fresh as a group chat's join is (only in its captured
shape: `messageId` `"0"`, the organizer and tenant GUIDs, nobody to ring), and the callbacks
(`CallOperation::NotificationLinks`: the sender and its callbacks by their captured names, `admitParticipantSuccess`,
`removeParticipantFailure`, `disablePreheatAsyncStatus` and the rest) pass as a follow-up. The web client may also enter
while its pre-join still shows (a **preheat**: the same entry with `endpointState.endpointProperties.preheatProperties:
1`, its media lines receive-only, and `meetingData: {meetingCode, passcode}` beside `groupChat` and `meetingInfo`); Join
now then ends the preheat with `POST …/conv/<id>/updateEndpointState` (`endpointProperties.preheatProperties: 0`), and
the client is in the meeting. Both are mapped the same way. `ab-meeting-join` still holds them back by name.

With the subscription or the entry not going through, the client instead sends the whole of it as one **start with
media**, `POST …/api/v2/epconv`: `conversationRequest` (`conversationType: null`, `subject: null`, `suppressDialout:
true`, the callbacks), `contentSharing: null`, `participants: {from, to: []}`, the capabilities, `groupContext: null`,
`groupChat: {threadId, messageId: "0"}`, `meetingInfo: {tenantId, organizerId, meetingType: 1}` (the client's `Chat`:
a meeting with its own chat, once or a series'; a channel's meeting is its other value), `meetingData: null`,
`endpointState`, `callInvitation` (`callModalities: ["Audio", "ScreenViewer"]`, a group call's offer with its
`mediaDescriptions`), `debugContent`, `participantPropertyBag`. **That is the native client's**
(`StartCall::in_meeting`, `CallAction::JoinMeeting`, `teams-headless <alias> join-meeting <link>`): the conversation's
answer as any start's, an acceptance from the conference's mixer at its `acceptance` callback, then as a join's. Seen
live 2026-11-02 (`teams-headless a` then `b join-meeting`, each of a single meeting and a series' occurrence): a started
a call nobody had (`status` placing, joining, connecting, connected, about 5 s), b's same start 25 s later joined it,
and each heard the other's tone; the fence read the meeting's chat fresh both times (the invitee is a member of it from
the meeting's creation). The start's answer lists who is in the call already (`roster.participants`, as an update's);
the updates after it tell only changes.

A link with no thread or organizer (`https://teams.microsoft.com/meet/<code>?p=<passcode>`) resolves differently: the
page read `GET …/api/mt/part/<partition>/beta/meetings/identifiers?meetingId=&passcode=&link=` (it answered
`{"tenantId": ""}`) and subscribed and started by `meetingData: {meetingCode, passcode, meetingUrl}` and
`meetingPreferences: {shouldResurrect: "resurrect"}`, `groupChat` and `meetingInfo` both `null` (its start with media
the same, with a `callInvitation`). The fence can't tie that to anyone, so no row maps it. The native client doesn't
send it: a calendar event with such a link names its chat (`skypeTeamsData`'s `cid`), and the meeting's chat's thread
names its organizer and tenant (`GET chatsvc v1/threads/<chat>?view=msnp24Equivalent`, `properties.meeting`: a JSON
object as text with `organizerId`, `tenantId`, `subject`, `meetingJoinUrl`, `meetingType`, `startTime`; seen
2026-11-02, `meeting-steps`, and in `ab-meeting-join`'s web client, which reads the same batch form), so it joins by
the chat as above (`teams_proto::MeetingThread`). The meeting chat's header does the same for its Join, as Teams'
header has it whether the meeting started or not. "Join with an ID" finds the meeting among the calendar's events by
the ID and passcode their short link carries (`MeetingCode`) and joins it so; an ID the calendar doesn't have is looked
for among the account's meeting chats (the newest 40 the chat list holds), each one's thread read for its join link
(`MeetingThread`'s `meetingJoinUrl`), and the meeting whose link carries the ID is joined by its chat as above, judged by
that chat read fresh. Joining by `meetingData` alone stays unmapped, because nothing the lab can read ties an ID to the
people in its meeting before a write: the identifiers read (`GET …/beta/meetings/identifiers?meetingId&passcode&link`)
answers only `{"tenantId": ""}` for a valid ID, the meeting's chat (its members) can be read only by its members, and the
meeting's conversation, which names its chat, comes only in the answer to a subscription or a start, both writes. So a
meeting whose chat the account isn't in (an ID someone passed on) is refused. Seen 2026-11-03 (`teams-headless a meetings
--read`): the threads of meetings made from the scheduling form name their long `meetup-join` link, not the short one,
so only a meeting whose thread names a short link is found by its ID this way.

**A channel's meeting**: see [A channel's meeting](#a-channels-meeting).

**The pre-join's audio** (seen 2026-11-02, `meeting-steps`, a lab account): Computer audio (the microphone's switch
and device, the speaker's device), Phone audio drawn greyed out (the account has no dial-in), Room audio (a "Search for
a room" picker, read by `POST outlook.office.com/SchedulingB2/api/v1.0/me/findmeetinglocations`, which the lab tenant
answers with no room: "Unable to find any rooms", Join now disabled until one is picked) and Don't use audio ("Join
muted to avoid causing audio disruption."). Don't use audio changes nothing of the entry but its `endpointState`:
`{"endpointStateSequenceNumber": 1, "state": {"isMuted": true}}`, the same `callModalities` (`Audio`, `ScreenViewer`)
and offer. The web client also enters the meeting's conversation while its pre-join shows, before Join now
("preheat": `endpointState.endpointProperties.preheatProperties: 1`, the entry carrying the meeting's `meetingData`
code beside `groupChat` and `meetingInfo`); mapped since 2026-11-03 as the entry is (above). The native client offers
Computer audio, Phone audio greyed out and Don't use audio (it joins muted, as its mute after the media is up); Room
audio isn't offered. Three things block it: the lab's tenant has no room, so neither a search's answer with one nor
what joining a room's audio sends (the room device is told to join, a write to it no capture shows) can be seen; the
search is Outlook's (`outlook.office.com/SchedulingB2`), for which the native client has no token (its sign-in asks for
Teams' audiences only); and a write to a room reaches the room's device, which the lab rule can't judge without a room
of the lab's own.

## A channel's meeting

Seen 2026-11-03 (`meeting-steps`, a team of the lab's own made for it with a and b and deleted after, its General
channel). The channel header's **Meet now** opens a pre-join as a meeting's does, its subscription at `epconv` a
meeting start's keys with the channel in `groupChat` (`messageId` `null`) and no `meetingInfo`; answered `201` with a new
conversation, `conversationType` `adHocMeeting`. Join now enters it at the conversation (the preheat while the pre-join
shows, `preheatProperties: 1`, then `updateEndpointState`), the entry a meeting's with `groupChat` the channel and
`meetingInfo` `{organizerId, tenantId}`: the account that started it organizes it. With the subscription denied, the
client sends the whole of it as one start with media at `epconv` (the same keys, a call invitation, `meetingInfo`
`null`). The service posts the meeting to the channel ("Meeting in "General"", its card's Join) and CSA's teams read
names it under the channel: `activeMeetups: [{"messageId": <the post>, "conversationUrl", "conversationId",
"groupCallInitiator", "organizerId", "tenantId", "callMeetingType": -1, "conversationType": "adHocMeeting", "status":
"Active", "expiration", …}]`. Its card's Join (b's web client) goes to that conversation: a roster-only join, then the
entry with its call invitation, `groupChat` the channel and the post's id, `meetingInfo` the organizer and tenant (and
`meetingType` `-1` on the roster-only one), `meetingData` the meeting's code and passcode. When everyone left, the post
reads "Meeting in "General" ended".

The fence maps the Meet now (its subscription and its start with media) as a call on the channel's team
(`Scope::Team`, `contacting::call::channel_call_start`) and every entry naming a channel as a join of that channel
(`CHANNEL_JOIN`), each judged by the team's members read fresh from the team's thread, Teams' own apps not counted, as
any write on a team is (`docs/lab-rules.md`, "A team and its channels"). The team's first channel's thread is the team's
root; another standard channel's thread read (`GET v1/threads/<channel>?view=msnp24Equivalent`, seen 2026-11-02 in
`channel-create`) has `threadType` `topic`, `spaceId` (the team's root thread) and the team's `groupId`, so the fence reads
the channel's thread, then the root it names, which must be a team's root naming the same group, then the root's members
(`teams_fence::parse_channel_thread`). A private channel's thread is a `space` of its own (`productThreadType`
`TeamsPrivateChannel`, its own `groupId` the team's), read with its own members. The native client joins a running channel meeting:
the channel's header shows **Join** while CSA names a meeting running in it (read when the channel opens and every 30 s
while it shows, `Core::channel_meetup`), which opens the pre-join, and Join now reads the meeting again and enters it at
its conversation in the web client's entry shape (`Calls::join_channel_meeting`). While none runs, the header has
**Meet now** with its caret's menu, as Teams' has: it opens the pre-join titled `Meeting in "<channel>"`, and Join now
goes as the web client's does (`CallAction::MeetNow`, `Placement::Channel`): first the subscription at `epconv`
(`StartCall::meet_now`: the conversation's callbacks, `enableGroupCallMeetupGeneration` `true`, `groupChat` the channel
with `messageId` `null`, `meetingInfo` `null`, no call invitation; it pends nothing in the call registry), then, from its
answer's `conversationController`, the entry with media at that conversation (`JoinCall::enter`: `messageId` `null`,
`meetingInfo` the account itself and its tenant, `enableGroupCallMeetupGeneration` `true`). Seen live 2026-11-03 (a team of
the lab's own made for it, a and b, in a channel other than General, deleted after): the subscription's answer is the
new `adHocMeeting` conversation, the entry connects, the service posts the meeting (`Event/Call`, `callEventType`
`callStarted`, its content naming the `conversationUrl`; the web client draws it as the card "Meeting in "<channel>"")
and a's CSA read names it under the channel's `activeMeetups` within seconds, so the header shows Join once the account
left while it still runs. The web client's start with media instead (what it sends when its subscription fails) made a
`groupCall` conversation, which the native client no longer sends.

**b joining it** (seen live 2026-11-03, a team of the lab's own with a and b, a channel other than General made after b
was added): b's own CSA read lists the channel but names no meeting under it (no `activeMeetups`) while a's Meet now
runs, and keeps not naming it: CSA tells a channel's running meeting only to some of the team (b had never opened the
channel). The channel's posts tell it to everyone: the call's `Event/Call` post (`<callEventType>callStarted`, its
`<conversationUrl>` and `<callId>`, `properties.meeting` naming the organizer and tenant), which b's web client joins
from by its card. So the native client looks at the channel's newest posts when CSA names none
(`ChannelMeetup::of_posts`: the newest start no newer `callEnded` of the same `callId` follows), and b's native Join
went in (the join judged by the channel's team, a in the call). What looked like a stale CSA answer in the first run was
b's stored teams list, read only when it is an hour old: the engine now reads the teams again when a team's thread it
doesn't name comes, or a team's activity (`ThreadActivity/…`, a channel made: `SpaceThreadChannelAdded`) is delivered
([channels.md](channels.md#what-the-native-client-reads)).
`teams-headless <alias> channel-meeting <channel> [--join] [--subject <text>]` does the same from the command line.

**The Meet now's subject** (seen 2026-11-03, `meeting-steps`, a team of the lab's own with a alone, deleted after): the
pre-join shows "Microsoft Teams meeting" over a text box holding `Meeting in "<channel>"`, which the user may change;
changing it sends nothing. Once Join now went in and the service posted the meeting (its `Event/Call` `callStarted`
post), the web client titles that post: `PUT …/chatsvc/<region>/v1/users/ME/conversations/<channel>/messages/<post>/properties?name=meeting`
with `{"meeting": {"meetingtitle": <subject>}}`, edited or not (earlier runs' captures show it with the unedited
`Meeting in "<channel>"`). Until then the post's `properties.meeting` names the meeting (`ScheduledMeetingCreated`, its
`organizerId`, its join link) with `meetingtitle` `null`; the web client's card then reads "<subject>", and "<subject>
ended" once a `callEnded` post of the same `callId` came. The fence maps it as an `Edit` on the channel's team
(`Scope::Team`, `contacting/chat.rs`): everyone on the team sees the post. The native pre-join of a Meet now shows the
same heading and box (`Calls::set_prejoin_title`), and once Join now asked the meeting, the engine looks at the
channel's newest posts every 3 s for up to 2 minutes for a meeting the account organizes, posted since a minute before
the ask, and titles it (`SetMeetingTitle`, `EngineRequests::title_meet_now`). Seen live 2026-11-03: the native title
went through (the fence's `edit` on the team), and the web client showed the post as "<subject> ended" after the
call. The channel view shows such a post's title the same way.

**Schedule a meeting** (the caret's other item, seen 2026-11-03 with the event's creation held back): the scheduling form
opens with the channel as a row of its own ("<team> > <channel>", and a "Send personal invites" switch); Send creates
the online meeting as any form does, then posts the event to the team's group calendar: `POST
…/api/mt/part/<partition>/v2.0/groups/smtp:<team mailbox>/calendars/default/events?isOnlineMeeting=true&shouldDecryptData=true`
with the creation's event keys at the top level (no `calendarEvent` wrapper), `attendees` the organizer and the team's
mailbox (`{"address": <team mailbox>, "role": "User", "type": "Required"}`), `mailboxAddress` the team's mailbox, and
`skypeTeamsData` naming the channel (`{"cid": <channel>, "uid": <organizer MRI>, "type": 0}`), and with no text of its own
`emptyBodyPlaceholder: "Scheduled a meeting"`; no `schedulingServiceEvent` (the online meeting the form made is left
unused, the service makes the event's own). The row is a combobox ("Add channel") showing the team's tile, "<team> >
<channel>"; its **Send personal invites** switch (off at first, its info "Meeting invitations will be added to the
personal calendars of all channel members.") adds `inviteAllMembers: true` (seen held back 2026-11-03). Answered `201`
with the event (its `skypeTeamsData.cid` the channel, its chat the channel itself); within seconds the service posts it
to the channel: a `RichText/Html` "Scheduled a meeting" whose `properties.meeting` (JSON as text) is
`{"@type": "http://schema.skype.com/ScheduledMeetingCreated", "meetingtitle", "scheduledmeetinginfo": {"startTime",
"endTime", "location", "exchangeId", "iCalUid", "eventType", …}, "organizerId", "tenantId", "meetingJoinUrl"}`, which the
web client draws as a card (the day's tile, the title, "<weekday>, <start> - <end> | <location>", "<team> > <channel>",
Edit for the organizer); a Meet now's post carries the same `@type` with no title and no times. The organizer's own
calendar holds the meeting a few seconds later (as an attendee's copy, `isOrganizer` `false`).

Its **Cancel meeting** (the card's Edit, then the details' Cancel meeting and its note): `PUT
…/v2.0/groups/smtp:<team mailbox>/calendars/events/<iCalUid>/cancel` with `{"conversationId": <channel>, "messageId":
<post>, "rootMessageId": <post>, "cancellationMessageFormat": "The meeting \"{0}\" is cancelled", "fetchEventContract":
{"eventType", "eventTimeZone": "UTC", "startTime": <start, UTC, ms>}, "cancellationMessage"?: <note>}`. The service then
replies under the post: a `Text` "The meeting "<title>" is cancelled" whose `properties.meeting` is the same meeting as
`ScheduledMeetingCancelled` (`scheduledmeetinginfo.isCancelled` `true`); the web client strikes the card's title
through and drops its tile.

Its **details** (the card's Edit) read the event from the group's calendar: `GET
…/v2.0/groups/smtp:<team mailbox>/calendars/events/iCalUId/<iCalUid>?eventStartDateTime=&shouldDecryptData=true`,
answered with the event itself: `objectId`, `iCalUID`, `cleanGlobalObjectId`, the times and zone, `subject`,
`location`, `attendees` (the team's mailbox as `Organizer`, who scheduled it as an attendee who accepted, anyone
invited), `organizerAddress` and `organizerName` the team, `isOrganizer` `true` and `myResponseType` `Organizer` (the
group's view, whoever reads it), the body (the join block), `skypeTeamsData` (`cid` the channel, `rid` and `mid` its
post, `uid` who scheduled it), the join links, `schedulingServiceUpdateUrl`, `inviteAllMembers`. The organizer's own
calendar holds an attendee's copy of it (`isOrganizer` `false`, `myResponseType` `Accepted`, the team its organizer,
the same `skypeTeamsData` and `iCalUID`): who scheduled it is `skypeTeamsData`'s `uid`, and Teams offers that account
the organizer's Edit and Cancel meeting from its calendar too. Its **change** (a new title, Send update, seen held back
2026-11-03): `PUT …/v2.0/groups/smtp:<team mailbox>/calendars/events/<iCalUid>?isOnlineMeeting=true` with
`{"updateMessageFormat": "The meeting has been updated", "updatedCalendarEvent": <the event as read, the form's fields
over it: its read-only parts (`lastModifiedTime`, `bodyPreview`, `meetingLocations`, `conflictingMeetings`,
`skypeTeamsDataObject`, …) left out, `attendees` the organizer (`Admin`, `Organizer`, with its MRI) and the team's
mailbox (`Required`), `organizerAddress` and `organizerName` the organizer's, `mailboxAddress` the team's, a new
`clientOptimisticEventId`>, "fetchEventContract": {"eventType", "startTime": <its start as read, UTC, ms>,
"eventTimeZone": "UTC"}}`. Seen live natively the same day (`teams-headless … channel-schedule --rename`): answered
2xx, and the service edits the channel's post in place (its `properties.meeting` gets the new `meetingtitle`, the post
a new version); no reply is posted under it.

The fence maps the creation, the change and the cancellation as a `Meeting` on the team's calendar
(`Scope::TeamCalendar`: the channel the body names and the mailbox the path names; a change's event must name the
`iCalUID` its path does), judged by the channel's team read fresh, Teams' own apps not
counted, as any write on a team: the root thread the channel names must be the team's own (no `productThreadType`) and
name the mailbox as its `teamSmtpAddress` (or `aadTeamSmtpAddress`), so a write naming a lab channel can't reach another
group's calendar. Personal invites reach the channel's members, the same team. The native client: the channel header's
Meet now menu offers **Schedule a meeting** for a standard channel of a team whose mailbox CSA named; it opens the
scheduling form with the channel's row (and Send personal invites) in place of the Teams meeting switch, and its Send
creates the meeting there (`EventWrite::CreateInChannel`, `CreateChannelEvent`). The channel view draws the post as the
meeting card (cancelled once a cancellation's reply names it), with **Edit** for the account that scheduled it; the
calendar's card of that meeting offers Edit and Cancel meeting too (a double click opens it), though its copy reads
`isOrganizer` `false`. Edit reads the meeting from the team's calendar (`ChannelEventRead`,
`EngineRequests::channel_event`) and opens the scheduling form on it with the channel's row: its Send update changes it
there (`EventWrite::UpdateInChannel`, `UpdateChannelEvent`), its Cancel meeting cancels it with the note
(`EventWrite::CancelInChannel`). Seen live 2026-11-03 (a team of the lab's own, a and b, a channel other than General,
deleted after): the native creation answered `201`, the post came, a's calendar held it, and the native cancellation
(`teams-headless a channel-schedule <channel> <title> --cancel`) went through; again the same day (a team with a alone,
deleted after) with the change (`--rename`) between them, its post renamed in place.

## The lobby

Who waits in a meeting's lobby is the meeting's option `AutoAdmittedUsers` (`Everyone`,
`EveryoneInSameAndFederatedCompany`, `EveryoneInCompany` (the lab tenant's default: every lab account bypasses it),
`EveryoneInCompanyExcludingGuests`, `InvitedUsers`, `OrganizerOnly`), set from the meeting options page
(`https://teams.microsoft.com/meetingOptions/?organizerId&tenantId&threadId=<chat, its colon an underscore>&messageId=0`):
its Save posts every option to `POST …/api/mt/part/<partition>/beta/meetings/v1/options/<tenant>/<organizer>/<chat>/0/?monarch=true`
(answered `200`; the fence lets it through while it picks nobody). `UsersCanAdmitFromLobby` says who admits. A meeting's
conversation lists `admit` and `admitAll` among its `links`, `activeModalities.lobby` (`null` outside it) and the
roster's `participantCounts.lobbyParticipants`. What a joiner held in the lobby is told, and what admitting or denying
one sends, wasn't captured: in the 2026-11-02 run the web client's entry at the conversation (`POST …/conv/<id>` with
its `callInvitation`, unmapped and denied) failed its join before the lobby ("Try joining again, or dial in"), and the
native client, which starts by the mapped start with media, couldn't run (the lab's keyring was gone).

Seen 2026-11-03 (`meeting-steps` with a's web client in its own meeting, its options saved with `OrganizerOnly`, and b's
native client joining by the link): b's start with media is answered as any (`200`/`201`, the conversation and its
acceptance), but b waits in the lobby. The organizer's roster tells it: b's entry has an endpoint with a `lobby` object
(`{"mediaStreams": [{"type": "audio", "label": "main-audio", "direction": "sendrecv", "serverMuted": true}]}`) where a
participant in the call has a `call` one, its `modalityJoined` `"Lobby"`, and `participantCounts.lobbyParticipants`
counts it. The page shows "Waiting in the lobby", the person, Deny and Admit (and a message to the lobby). The person's
own roster (b's callbacks) lists b the same way: that is how a joiner knows it waits. Once it leaves the lobby (it left,
or it was turned away) its entry goes `inactive` with no endpoints.

- **Admit**: `POST …/conv/<id>/admit` (the conversation's `admit` link) with `{"participants": {"from", "to": [{"id":
  <MRI>}]}, "links": {"admitSuccess", "admitFailure"}, "debugContent": {"causeId"}}`, answered `202`; the web client
  tried it three times, 4 s apart, while it was denied. The admitted person's entry moves from `lobby` to `call`.
- **Deny**: `POST …/conv/<id>/removeParticipant` (its `removeParticipant` link) with `{"participants": {"from", "to":
  [{"id"}]}, "links": {"removeParticipantSuccess", "removeParticipantFailure"}}`; the card goes from the page at once.

Both are mapped as an `add` is judged: the sender and everyone named lab accounts, approved services or call guests
(`CallOperation::Admit`, a `WriteKind::Invite`, and `CallOperation::RemoveParticipant`). The native client: the roster's
lobby is read apart from who is in the call (`teams-call`'s `CallEvent::Lobby` for whoever waits,
`CallEvent::InLobby` for the account itself); the call shows `CallStatus::Lobby` ("Someone in the meeting should let you
in soon") while the account waits, and the call window shows whoever may admit Teams' "Waiting in the lobby" card with
Deny and Admit (`CallAction::Lobby`, `DecideLobby` with the web client's callbacks). Seen live 2026-11-03
(`teams-headless a join-meeting --admit` and `b join-meeting`): b told `lobby`, a told b waiting, a's admit answered
`202`, a told nobody waiting and b saw a in its call.

**What the admitted joiner gets** (seen 2026-11-03, `meeting-steps`: b's web client let into a's meeting, every one of
its deliveries recorded): the conversation's update and a roster that still lists it in the lobby, then the mixer's first
offer at its `mediaRenegotiation` callback (`mediaContent.newOffer: true`, `fromMixer`, `contentType`
`application/sdp-ngc-0.5`: a session of its own, every line and RTP header extension numbered anew), its answer
link on the call's call id. The client answers it (`202`), and the acknowledgement of that answer, at the answer's own
`mediaAcknowledgement` callback, moves the call: `{"mediaAcknowledgement": {"code": 0, "subCode": 10109, "phrase":
"Participant retarget was successful.", "resultCategories": ["Success"]}, "links": {…}}`, its `links` the call's own
(`callLeg`, `mediaRenegotiation`, `transfer`, `retargetCompletion`, `hold`, `monitor`, `updateCallState`,
`controlVideoStreaming`, `applyChannelParameters`, `updateMediaDescriptions`) all on **another call id** of the same
call controller, `callLeg` named twice (`null`, then the new leg: read as the web client's `JSON.parse` keeps the last).
The roster then moves the joiner from `lobby` to `call` (`modalityJoined` `"Lobby,Call"`), and about 5 s later the mixer
renegotiates the new session (`newOffer: false`, `sdp-ngc-1.0`), its answer link on the new call id; answered `202`, its
acknowledgement hands out no call leg. The web client posts nothing to `retargetCompletion`. The native client does the
same: its media set up again for the mixer's first offer (`Calling::rebuild`, the roster's participants kept), the
retarget's acknowledgement followed (the call registry moves the call to the new call id and binds its links as an
acceptance's, `teams-fence`'s `Call::follow_retarget`; the call machine takes the new `mediaRenegotiation` and call
leg, `Calls::retargeted`), and the renegotiations after it answered on the media set up for the mixer (a call's offers
after its rebuild are its own renegotiations, not another session). Seen live 2026-11-03: b's native client let in by
a's web client stayed in the meeting 40 and 60 s and hung up itself, every write permitted, its audio frames sent and
received.

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

## How a call's end reads

Seen in the personal build's own calls (2026-10-30). An end delivered at the call's `call/end` callback is
`{"callEnd": {"reason", "sender", "code", "subCode", "phrase", "resultCategories"}, "debugContent"}`:

- **The far end hung up**: `sender` is the far end (its MRI and `displayName`), `reason` `noError`, `code` 0, `phrase`
  `CallEndReasonLocalUserInitiated`, `resultCategories` `["Success"]`. The phrase is from the sender's side: "local"
  means the sender's own user, so the same phrase comes back with the account as `sender` after its own `leave`. Who
  ended a call is told by `sender`, never by the phrase.
- **The service gave the call up**: `sender` null, `reason` `clientError`, and `resultCategories` naming the failure
  (`JoinFailure`, `MediaNegotiationFailure`; `phrase` `SdpParsingFailure`, `subCode` 301000 for a group join whose
  answer the mixer didn't take).
- **A conversation's end** at the `conversationEnd` callback: `sender` null, its `phrase` a sentence of the service's
  own and its `subCode` the cause: 5002 when everyone left the audio-video modality (with
  `callControllerTransactionEnd.sender` naming who left last, its phrase again `CallEndReasonLocalUserInitiated`),
  5013 when no one else joined a group call. The call `end` and the conversation's end both come, either first, each
  delivered again 5–13 s later (the machine ends the call on the first).

The app names the cause in plain words from these (`teams_ui::calls::Ended::reason`) and shows no code or phrase.

## The SDP dialect

Every offer and answer in the signaling is a `mediaContent` object: the SDP as `blob` and `contentType`
`application/sdp-ngc-1.0`, in every start, acceptance, `accept`, `mediaNegotiation` and `mediaAnswer` of the runs,
whoever wrote it. The one exception is a group call's invitation: the mixer's offer there is
`application/sdp-ngc-0.5` (below). The web client turns each into standard SDP before its `RTCPeerConnection` takes it,
and its own standard SDP into the dialect before it sends it; both forms were captured side by side (the WebRTC hooks
record what the page hands its peer connection). `teams-media`'s `sdp` module does the same conversion for libwebrtc
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

**What the native client does differently**: it writes only its relayed candidates, with `raddr 0.0.0.0 rport 0`
(no local address leaves it) and numbered foundations, refusing to write an SDP with any other candidate, and only
once libwebrtc's gathering ended (Teams takes no trickled candidate); it reads only the UDP candidates of component 1
(its media reaches the far end through the relay, whose relayed addresses are UDP); and it reads the mixer's missing
setup as `actpass`, so libwebrtc takes the client's part, as the web client does, and keeps the call's role in a
renegotiation.
The conversion keeps both namings of every line (libwebrtc's mid, Teams' mid) for the call's life, so a
renegotiation offered by either side names the same lines. A line it adds is numbered one past the highest mid of
the call (the mixer's numbering starts at `1` and runs out of line order, so neither the count of lines nor a line's
place is free), and a rejected line that names no mid is the call's line at its place. It labels its lines as the
web client does: the last video line of an offer with more than one, while the call has no sharing line, is
`applicationsharing-video`, the others `main-video`. A codec the answer numbers otherwise than our offer did (the
media controller answers Opus offered as `111` with `102`, [calling-echo.md](calling-echo.md#the-native-echo-call))
is left as answered: libwebrtc sends with the answer's number and takes the far end's media with our offer's, as the
browser does. A group call's main and receiving lines are answered `sendrecv` whichever way libwebrtc answered them
(it answers a line it has no track for receive-only, which the mixer refuses), a rejected line is written as the web
client writes one (`m=video 0 RTP/SAVP 34`, `m=audio 0 RTP/SAVP 0`).

Three things the native lab calls (M3 Task 16) showed live, each now the native client's way:

- **A call without video answers every video line rejected.** Twelve `sendrecv` H.264 lines in an `accept` that
  described none of them made the call controller end the leg at once with `410 SdpParsingFailure`; answered
  rejected, written as the web client writes a rejected line (`m=video 0 RTP/SAVP 34` with its label), the mixer takes
  the answer. A caller's 1:1 offer's video lines are answered the same way when the call takes no video, and a web
  client takes it.
- **A rejected line may name any profile.** The mixer's renegotiation that follows (within a fraction of a second of
  the call being up) names the lines we rejected on a profile other than `RTP/SAVP`; a line on port 0 is read as
  rejected whatever it names, and only a line in use must be on Teams' profile (the error names its kind, port and
  profile, nothing else).
- **Our offers name no line-naming header extension** (`sdes:mid`, `sdes:rtp-stream-id`,
  `sdes:repaired-rtp-stream-id`). The dialect may name our lines otherwise than our peer does: a web client that
  answered an offer naming the mid extension took every packet of ours, carrying our peer's own mid, for a line it
  doesn't know and dropped it (its candidate pair counted them, no inbound stream was made); the page heard nothing.
  Without them in the answer libwebrtc sends none. Our answers keep the offer's: they answer the offer's own mids.

**Opus as the lines ask for it**: every captured Opus line carries `a=ptime:20` and `a=fmtp:111
minptime=10;useinbandfec=1` (the media controller's answer adds `usedtx=1`). libwebrtc encodes and decodes Opus as
the lines negotiate it (in-band FEC, DTX when the far end's line asks for it) and plays what arrives through its own
jitter buffer (NetEq), which recovers or conceals what was lost.

## Seen beside the call

- The caller's own call log's read horizon: `PUT …/v1/users/ME/conversations/48:calllogs/properties?name=consumptionhorizon`
  when a call ends; it reaches nobody, but no row carves it out, so it is denied and on the expected-deny list.
- Deliveries at an endpoint's root with `evt` 115 and 116 and a gzip'd `cp`: 115 when a participant joins a call of the
  account's (`conversationController`, `userJoinTime`, `participants`), 116 when one leaves. Read by nothing.
