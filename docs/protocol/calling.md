# Calling: lab-to-lab calls in the web client

Status: 2026-10-24, M3 Tasks 5 and 6. Written from live runs of `proto-lab run ab-call` (a calls b from their 1:1 chat) and
`proto-lab run abc-call` (a calls the lab's a–b–c group chat), each step of `--call` (below), Chrome for Testing 155
headless, muted, with fake devices, the fence armed with the lab rule. The captures stay outside the repository; the
fixtures in `tests/fixtures/calling/chat/` are synthetic, shaped like them. Ids, hosts' numbers, tokens and addresses
below are placeholders. The echo test call is in [calling-echo.md](calling-echo.md); the fence's side of a call is in
[fence-targets.md](fence-targets.md#the-call-registry).

The code: the scenarios are `crates/proto-lab/src/scenario/ab_call.rs` and `abc_call.rs`; the start's row is
`chat_call_start`, the callee's and the call's operations `follow_up`, in `crates/teams-fence/src/contacting/call.rs`;
offered invitations are `crates/teams-fence/src/calls/offer.rs`.

## The runs

| `--call` | Browsers | What is held back (labelled forced denies) | What happens |
|---|---|---|---|
| `start-held` | a | every `POST epconv` (`the call start, held back`) | a clicks the call button(s); nothing rings |
| `answer` | a, each callee | while it rings, everything each callee's page sends to the calling services but its `attach` and `progress`, which the fence maps on the offered invitation (`a callee's sends while ringing, held back`); its delivery responses go to the fence; after its click everything but the answered call's mapped operations (`a callee's answer, held back`) | each callee answers by the run's click (only on the run's own lab-only invitation), the call connects, every page turns its noise suppression off and its microphone over, the audio is judged both ways by the WebRTC statistics (the tone noted when heard), then each callee hangs up |
| `decline` | a, each callee | as `answer`; after the click everything but the decline's `DELETE` (`a callee's decline, held back`) | each callee declines by the run's click, its rejection let through by the fence; a hangs up when its call is still up |
| `video` | a, each callee | as `answer` | as `answer`, and once the audio was judged every page turns its camera on; each must decode video |
| `dropped` | a, each callee | as `answer` | as `answer`, and the last callee's page goes off the network for 10 s with its calling socket closed by the run (it must close; none closed fails the run), then comes back; 25 s later every page must still show the call |
| `join` | a, b and c (`abc-call`) | as `answer` for b; c's ring ends unanswered, then everything c sends but its joins and its call's mapped operations (`a callee's join of the running call, held back`) | b answers; once c's ring ended c opens the chat and, only on the run's own call with the run's chat shown and one join, clicks the header's join of the running call; c must then show the call screen, and its capture the join's answer registering the call |
| `cancel` | a, each callee | the callees' sends while ringing as above; a's own join of the running call (`the caller's join of the running call, held back`) | a hangs up 8 s after it placed the call |
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
([fence-targets.md](fence-targets.md#the-call-registry)). The start's answer is the conversation, as for the test
call, and registers the call.

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
  `callKeepAliveInterval` (2700 s).

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
travels over that channel, which the hooks don't read (Task 19 reads it).

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
ends at once with `conversationEnd` `409` "Conversation cannot be created due to a failure to resolve ownership",
naming the running conversation (`conversationUrl`) and the chat (`threadId`); the client then joins that one.

## A call held past a dropped socket

`dropped`: with the callee's page off the network (DevTools' emulation fails new requests; open sockets stay, so the run
closes the calling socket itself) its calling socket closed; while offline nothing reconnects. Back online the page
allocates a new Trouter connection (`POST …trouter…/v4/a`), registers its endpoint again (`POST …/registrar/prod/V2/
registrations`, a plain request the fence lets through), and its new connection ran over socket.io XHR polling
(`POST …/socket.io/1/xhr-polling/<id>`). It sent nothing to the call: no `notificationLinks`, no link of the call
changed in the 25 s after; the call's media went on, the call stayed up on both pages, and the callee's `leave`
afterwards passed on the same links.

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
  a's call connects to it (the registry binds the acceptance, its acknowledgement passes, a's follow-ups pass). The
  roster then lists the callee with a voicemail endpoint (`GraphCommunicationsClient-Voicemail`). a's tone is
  recorded into the callee's voicemail, which reaches only the callee's own mailbox.
- **Nobody acts, group call** (`timeout`): the call's mixer accepts a's leg at once (about 3 s after the start, an
  acceptance at a's callback, before anyone answered); at about 25 s each callee's `addParticipantFailure` arrives,
  then the call's `end` and `conversationEnd`; the group call ends without voicemail.
- **The caller's join after it left** (group calls): after its call ended or it hung up, a's page posts a roster-only
  join to the conversation controller itself ([Joining a running call](#joining-a-running-call)), and retries it twice,
  4 s apart. No invitation binds it on a's side; the scenario holds it back by name.
- **Answered, declined**: [The answer](#the-answer), [The decline](#the-decline). **Answered elsewhere**: not captured
  (the lab runs one endpoint per account); Task 16 captures it.

## Seen beside the call

- The caller's own call log's read horizon: `PUT …/v1/users/ME/conversations/48:calllogs/properties?name=consumptionhorizon`
  when a call ends; it reaches nobody, but no row carves it out, so it is denied and on the expected-deny list.
- Deliveries at an endpoint's root with `evt` 115 and 116 and a gzip'd `cp`: 115 when a participant joins a call of the
  account's (`conversationController`, `userJoinTime`, `participants`), 116 when one leaves. Read by nothing.
