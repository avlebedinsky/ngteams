# Calling: the web client's test call to the echo service

Status: 2026-10-20, M0 Task 18 (spike 3b, part 1), 2026-10-24, M3 Task 4 (the call completed under the call
registry: [What the M3 run added](#what-the-m3-run-added)), and 2026-10-25, M3 Task 15 (the native client's own test
call: [The native echo call](#the-native-echo-call)). Written from live runs of `proto-lab run echo-call --headless`
(account a, Chrome for Testing 155 headless, a fake microphone playing a 440 Hz WAV tone, the fence armed with what
were then its calling classifiers), from the web client's own diagnostic log of one of those runs (Ctrl+Alt+Shift+1, read locally),
and from the web bundle `26260.1717.5229.7049`. The captures stay outside the repository; the fixtures in
`tests/fixtures/calling/echo/` are synthetic, shaped like them. Ids, tokens, hosts' numbers and addresses below are
placeholders.

Current code: `proto-lab/src/scenario/echo_call.rs` records the web flow, and the native client builds typed
requests in `teams-proto/src/write/call.rs`, tracked by `teams-call::track`. Audio worklets use the capture target
setup in `proto-lab/src/arming.rs`. No browser or native write fence remains.

For current native tests, run `scripts/run-lab.sh a --pipewire`, choose Make a test call in `ngteams`' sandbox UI,
and hang up before `scripts/run-lab.sh a --stop`. The agent must verify that the run reaches only the lab account
and the echo service under the [lab rule](../lab-rules.md). Dated old probe commands below are historical evidence.

## Outcome

Since M3 Task 4 the recorded flow completes: the echo joins, records and plays the tone back, and the page's audio tap reads it
([What the M3 run added](#what-the-m3-run-added)). The M0 state:

- The fence allowed the call start: a new call of a and the echo service, both in `lab.toml`.
- The call reached Teams' media controller: ICE and DTLS connected over UDP (a server-reflexive candidate of ours paired
  with the controller's relay candidate, round trip about 90 ms), Opus went out.
- **The echo service never joined, and no tone came back.** The client adds the echo participant with a request on
  the call (`add`, below), and every request on an existing call is still unknown to the fence, so it was denied, as
  were the call's acknowledgements and the `leave`. The fence can't judge a write on a call until it can read the
  call's members fresh and in full (the conversation offers a `getAllParticipants` link; its shape is not captured
  yet). See [Open](#open).

## The echo service

The echo ("test call") service is a bot, `28:<echo guid>`. The same MRI showed in three places, all read-only, before
any call was placed:

1. the bundle's prod config, `teamsEchoBotMri` (other clouds have their own, prefixed `28:<cloud>-global:`);
2. the live web client's runtime settings in its diagnostic log, `teamsEchoBotMri` from its deployed config (no ECS
   override);
3. the live client's own test-call flow: before calling, it creates a 1:1 chat with the bot (`POST v1/threads`,
   members a and the bot, `fixedRoster`, `uniquerosterthread`), which the fence denied while `lab.toml` had no echo
   MRI.

It was then recorded in `lab.toml` with `proto-lab service-mri echo <mri>` (only for a service the file approves by
name).

## What has to be up before a call

The web client starts its calling stack lazily (`enableDeferredCSI`): "Make a test call" asks for it, and it is
deferred ("Deferring calling stack Initialization. [entryPoint:startEchoBotCall]" in the client's log) until the Calls
app or an app pre-warm starts it. A headless lab run only gets it by opening the Calls app first. A Settings deep link
reloads the page and loses it again, so the scenario opens Settings from the account menu.

Before that, the calling stack needs (each reaches nobody, so the lab rule lets it through; the runs here used the
fence of the time, which allowed each as the account's own state in its captured shape):

- **The account's own settings and policies.** Calling stays off until the client knows the calling policy
  (`callingPolicy.allowPrivateCalling` from `POST <teams>/api/mt/part/<region>-<n>/beta/users/useraggregatesettings`,
  whose body is an object of setting-group flags; `…/effectivePolicies` takes a list of policy names). These are reads
  sent as `POST`.
- **The Trouter session** (the push channel). The client opens `wss://go.trouter.teams.microsoft.com/v4/c/?…` and the
  service moves it to a regional `wss://<name>-t.trouter.teams.microsoft.com/v4/c?…`. The client's frames are
  socket.io 0.9: `5:::{"name":"user.authenticate","args":[{"headers":{"Authorization":"Bearer <token>",…},
  "connectparams":{…}}]}` (the reconnect parameters the service handed over, sent back as they came), keep-alives
  `5:<n>+::{"name":"ping"}`, `5:<n>+::{"name":"user.activity","args":[{"state":"active","cv":"…"}]}`, the
  acknowledgement of the service's message-loss report `5:<n>+::{"name":"trouter.processed_message_loss",…}` and the
  disconnect `0::`. An event sent with `+` waits for an acknowledgement; without one the client closes and reopens the
  socket. The service answers `1::` (connected), then `5:<n>::{"name":"trouter.connected","args":[{"id":…,"url":…,
  "surl":…,…}]}` naming the endpoint's push URLs (`https://<name>-f.trouter.teams.microsoft.com:8443|3443/v4/f/<id>/`).
  Requests the service delivers come as `3:::{"id":…,"method":…,"url":…,"headers":…,"body":…}` and the client answers
  each with `3:::{"id":…,"status":200,"headers":{"MS-CV":…,"trouter-request":…,"trouter-client":…},"body":…}`: an
  empty body for most, the call's `callAcceptanceAcknowledgement` for the acceptance callback. These responses were
  then unknown and denied: a response goes back to whoever sent the delivery, and which deliveries an empty success
  only acknowledges (and which it might answer: an incoming call's) wasn't established (Codex and Claude, 2026-10-20).
  The service redelivered; the calls still reached the media controller without them. **Under the lab's call rule
  every one of them now passes the frame rule** ([chat-service.md](chat-service.md#current-call-tracking)). The
  acceptance's acknowledgement also
  echoes the delivery's `X-Microsoft-Skype-Chain-ID` and `X-Microsoft-Skype-Message-ID` (five headers in all).
- **Endpoint registration.** `POST <teams>/registrar/prod/V2/registrations` with `clientDescription` (`appId`,
  `templateKey`, …), a `registrationId` and `transports.TROUTER: [{"context":"","path":"<push url>","ttl":3600}]`;
  `202`. The messaging stack registers as `TeamsCDLWebWorker`; the calling stack opens a second Trouter socket and
  registers it as `SkypeSpacesWeb`. This makes the account reachable: an incoming call would be delivered to the page.
  **Incoming calls are not answered by this scenario**: it never clicks an answer. The native tracker offers invitations addressed to the account; answering is a user action ([calling.md](calling.md#the-answer)).

Nothing else of the client's unmapped traffic (presence, the middle tier's other lookups, the async media gateway's
cookie exchanges, telemetry) was needed; it was denied then, and passes under the lab rule.

## The TURN credentials

`GET https://teams.microsoft.com/trap/tokens` (a read, with the account's token) answers
`{"tokens":[{"realm":"\"rtcmedia\"","username":"<user>","password":"<password>"}],"expires":604800}`: one credential,
valid 7 days (`expires` in seconds), the realm with its quotes in the string. The client puts it on one ICE server with
three URLs: `turn:<relay host>:3478?transport=udp`, `turn:<relay host>:443?transport=tcp` and
`turns:<relay host>:443`. The relay host is `gateway-world.az.relay.teams.cloud.microsoft`. So TURN over TLS on 443 is
offered as well as plain TCP 443.

## The call, step by step

All call control goes to `https://api.flightproxy.teams.microsoft.com/api/v2/…` with `X-Skypetoken: <skypetoken>`
(not the bearer token the chat service takes), plus `X-Microsoft-Skype-Chain-ID` / `-Message-ID` (GUIDs per call and
request) and `MS-Teams-Region` / `-Partition`. Services behind the proxy are addressed as
`…/api/v2/ep/<service host>/…` (`conv-<region>-…-prod-aks.conv.skype.com`, `cc-…cc.skype.com`, `broker-…broker.skype.com`),
each URL handed out by the service before it is used.

1. **Chat with the bot.** `POST <teams>/api/chatsvc/amer/v1/threads` with members a and the bot (see above): a chat
   creation naming the caller and the bot.
2. **Peer connection and offer.** `RTCPeerConnection` with `bundlePolicy: max-bundle`, `rtcpMuxPolicy: require`, the
   TURN server above, `iceTransportPolicy: all`; `createOffer` → `setLocalDescription`; host, server-reflexive and relay
   candidates trickle (UDP and TCP, IPv4 and IPv6).
3. **Call start.** `POST …/api/v2/epconv`. The body:
   - `participants.from`: the caller's MRI, display name, `endpointId`, a fresh `participantId`, `languageId`;
     `participants.to`: the callees, **empty for the test call**;
   - `conversationRequest`: `scenario: "UserInitiatedTestCall"`, `roster.rosterUpdate` and `links` (conversation
     callbacks: `conversationEnd`, `conversationUpdate`, `localParticipantUpdate`, `addParticipantSuccess|Failure`,
     `addModalitySuccess|Failure`, `confirmUnmute`, `receiveMessage`), `properties`;
   - `callInvitation`: `callModalities` (`Audio`, `Video`), `links` (call callbacks: `progress`, `mediaAnswer`,
     `acceptance`, `redirection`, `end`), `clientContentForMediaController` (more callbacks), `pstnContent`
     (`platformName` describes the client), `mediaContent` (`blob`: the offer, `contentType:
     "application/sdp-ngc-1.0"`, `mediaDescriptions`, `applyChannelParameters`, `mediaLegId`);
   - `groupContext`, `groupChat`, `meetingInfo`, `meetingData` and `contentSharing`: `null` for a new call;
   - capabilities, `endpointMetadata`, `endpointState`, `debugContent`, `participantPropertyBag`.

   Every callback link is the endpoint's Trouter push URL plus `/callAgent/<agent id>/<random>/<kind>/<name>/`.
   The answer is the conversation: `conversationController` (its URL on the conversation service), `links` (`leave`,
   `addParticipant`, `removeParticipant`, `updateEndpointMetadata`, `getAllParticipants`, `searchParticipants`,
   `subscribe`, `brokerPublish`, …), `state`, `scenario`, `tenantId`, `region`, `conversationStartTime`.
   `StartCall::echo_test` builds the test-call shape with `UserInitiatedTestCall`, the account's sender and callbacks.
   The echo participant is added through the answer's `addParticipantAndModality` link below. The native tracker binds
   the start, acceptance and media links; it does not classify lab participants.


4. **Events.** `GET …/ep/broker-…/api/v1/subscribe/<id>/<n>` long-polls (reads): each answer has `message` (gzip,
   base64: a delivered HTTP request) and `nextSubscribeUrl`. The same callbacks also come over Trouter.
5. **Acceptance.** The call controller calls back `…/call/acceptance/` with `callAcceptance.links`: the call leg
   (`…/ep/cc-…/cc/v1/active/<call id>/…`) and its operations (`acknowledgement`, `mediaRenegotiation`, `transfer`,
   `hold`, `updateCallState`, `controlVideoStreaming`, `applyChannelParameters`, …), and the media answer in
   `callAcceptance.mediaContent` (`blob` in the same `application/sdp-ngc-1.0` dialect as the offer, `RTP/SAVP` with
   an `m=x-data` line at the end, plus `mediaLegId` and flags such as `newOffer` and `fromMixer`), and
   `callKeepAliveInterval`. The page turns it into standard SDP (below), `setRemoteDescription` applies it and ICE
   connects (the remote candidates are the media controller's, `typ relay` on port 3478, UDP and TCP passive, IPv4
   and IPv6).
6. **Follow-ups the client sends next** (denied in M0; each passes once the call's start was let in, the `add` judged
   by its invitee: [chat-service.md](chat-service.md#current-call-tracking)):
   - `PUT …/conv/<conversation id>/updateEndpointMetadata` (the caller's `participants.from` and `endpointMetadata`);
   - `POST …/cc/v1/mcProxy/<call id>/…/applyChannelParameters` (send capabilities of a media line);
   - `POST …/cc/v1/callAcceptance/<call id>/…/acknowledge` (`callAcceptanceAcknowledgement.links`: more callbacks:
     `mediaRenegotiation`, `transfer`, `replacement`, `balanceUpdate`, `retargetCompletion`, …);
   - **`POST …/conv/<conversation id>/add`**: `participants.from` (the caller) and `to: [{"id":"28:<echo guid>",
     "participantId":…}]`, `groupChat.threadId` (the 1:1 chat of step 1), `participantInvitationData` (the caller as
     organizer), `links.addParticipantSuccess|Failure`. **This is where the echo service joins**: the client invites it
     into its own conversation (the start's answer names the operation `addParticipant` at `…/addParticipant`; the
     client posted to `…/add`);
   - on hang-up, `POST …/conv/<conversation id>/leave` with `conversationTransactionEnd` and `callTransactionEnd`
     (`CallEndReasonLocalUserInitiated`).

## SDP

Two dialects travel:

- **Between the page and its `RTCPeerConnection`**: standard WebRTC. The offer has one BUNDLE group of 13 m-lines
  (`a=group:BUNDLE 0 … 12`): audio (`mid 0`), many video lines (sending and receiving slots for the main video, screen
  sharing and the gallery), and an `application` line (`UDP/DTLS/SCTP webrtc-datachannel`); `UDP/TLS/RTP/SAVPF`,
  `a=rtcp-mux` on every line, `a=rtcp-rsize` on video, `a=setup:actpass`, `a=ice-options:trickle`,
  `a=extmap-allow-mixed`. Audio offers CN/48000, Opus/48000/2 (`minptime=10;useinbandfec=1`, `transport-cc`), RED
  (`111/111`), G.722, PCMU, PCMA, CN/8000 and telephone-event (48000 and 8000). Audio header extensions:
  `ssrc-audio-level`, `abs-send-time`, transport-wide CC, `sdes:mid`. Video offers H.264 (several profiles, both
  packetization modes), AV1 and their RTX, with `goog-remb`, `transport-cc`, `ccm fir`, `nack`, `nack pli`, and the
  extensions `toffset`, `abs-send-time`, `video-orientation`, transport-wide CC, `video-content-type`, `video-timing`,
  `color-space`, `sdes:mid`, `rtp-stream-id`, `repaired-rtp-stream-id`, the AV1 dependency descriptor and
  `video-layers-allocation00`.
- **In the call start's `mediaContent.blob`** (`application/sdp-ngc-1.0`): the same offer rewritten for Teams:
  `RTP/SAVP` instead of `UDP/TLS/RTP/SAVPF`, the gathered candidates inline with a real port instead of `9`,
  `a=rtcp:<port>`, extension URIs with `\\` separators, and Teams attributes: `a=x-signaling-fb:* x-message app …`,
  `a=x-ssrc-range:<first>-<last>`, `a=label:main-audio|main-video|…`, `b=CT:4000` at the session.

The answer the page applies (from the media controller) keeps BUNDLE and rtcp-mux, answers `a=setup:passive` with its
own fingerprint, has no `a=ice-lite`, and selects Opus 111 with `usedtx=1`, RED, G.722, PCMU, PCMA, CN and
telephone-event for audio (`a=ptime:20`, `transport-cc`, `abs-send-time` and transport-wide CC), H.264 for video. On
the wire (WebRTC statistics) the audio codec in use was Opus 111 `usedtx=1`; the selected pair was our UDP
server-reflexive candidate to the controller's relay candidate. The conversion between the two dialects, line by
line, is in [calling.md](calling.md#the-sdp-dialect).

## What the M3 run added

From live runs of 2026-10-24 (M3 Task 4: `proto-lab run echo-call --headless`, account a, Chrome for Testing 155
muted with its fake audio manager, the call registry of
[chat-service.md](chat-service.md#current-call-tracking)); the shapes are in
`tests/fixtures/calling/echo/deliveries.json`, synthetic.

- **The client's noise suppression removes the tone.** With it on (the default) the sent track's level stayed at about
  -84 dBFS for the whole call, in the M0 runs too, while the same fake microphone in a plain page sends the tone
  through Chrome's own processing. The echo then records silence, says so, and leaves (its roster entry turns
  `inactive` about 10 s after it joined), and the client leaves the call by itself. The call screen's audio options
  (`#audio-button-configure`) hold a "Noise suppression" switch (`data-tid="background-suppression-switch"`); the
  scenario turns it off as soon as the call screen shows, and then the tone goes out and comes back. The Devices page
  of Settings has no such switch in the web client.
- **The acceptance is acknowledged once, one way.** The page answers the acceptance delivery with its delivery
  response (the `callAcceptanceAcknowledgement` in the body); only when that response fails does it send
  `POST …/callAcceptance/<call>/…/acknowledge`. With the response let through no `acknowledge` request goes.
- **The echo joining.** After the `add` the conversation sends `addParticipantSuccess` (the bot's MRI in `participants`
  and `participantInfos`) and a `rosterUpdate`: `type: "Delta"`, a `sequenceNumber` counting from 1 per conversation,
  `participantCounts` (`totalParticipants` 2 with the bot), and per participant MRI its `version`, `state` (`active`,
  later `inactive`), `details` (`participantType` `inTenant` for the account, `none` for the bot, no display name for
  the bot), `endpoints` (per endpoint its `call.mediaStreams`: the bot only `audio` `sendrecv`, the account `audio`,
  `video` and a receive-only stream; `callLinks.replacement`; `modalityJoined: "Call"`) and its meeting roles. The
  first roster updates list only the caller.
- **The conversation update.** `conversationUpdate` repeats the conversation (its controller, `links`, `state`:
  `conversationType` `adHocMeeting`, `groupCallInitiator` the caller, `scenario`) and its `activeModalities`: `call`
  with a `participants` link on the call controller's `callHub/<call id>`, and, from the update after the `add`, a
  `groupChat` with the 1:1 chat with the bot as `threadId`. That is a delivered binding of the call to its chat; the
  registry binds nothing from it yet (joining is Task 6's).
- **No renegotiation.** The bot joins through the conversation, and no `mediaRenegotiation` was delivered in any run;
  the acknowledgement only hands its link out.
- **The end.** On hanging up the page sends `leave` (`CallEndReasonLocalUserInitiated`) and the call controller then
  delivers `call/end` at the start's own end callback: `callEnd` with `code` 0, `phrase`, `reason: "noError"`,
  `resultCategories`, and the `sender` (the caller's participant). No `conversationEnd` came.
- **Redeliveries.** Every delivery whose response was denied in these runs (the conversation and roster updates,
  `call/end`) was delivered again about 13 s later (`Trouter-Timeout`); the redeliveries carried the same bodies.
- **Timing.** The tone came back about 32 s after the call screen showed (the echo greets, beeps, records, beeps and
  plays the recording back), on one run.

## The native echo call

From live runs of 2026-10-25 (M3 Task 15: the retired native probe, the release lab build, account
a, the lab fence). Its media is files: the microphone plays a 1 kHz tone keyed on and off in 200 ms slots of a fixed
pseudo-random pattern, the speaker is recorded, and the recording is searched for the pattern afterwards
(`teams_headless::echo`). The recordings stay under the lab's data dir. It answers the open questions of the M0 spike
([echo-call.md](../spikes/echo-call.md#open-questions-for-m3)).

- **What the start needs.** The start as `teams-proto` builds it from the capture was taken (`201`): the captured
  shape with the web client's capability numbers (`endpointCapabilities`, `clientEndpointCapabilities`) and a
  platform name in the web client's form, sent with `X-Skypetoken`, the call's chain id, a fresh message id, and the
  region and partition authz named. The `MS-Teams-Ring` header and the client-version header the web client adds
  weren't needed. Then, as in the web client's run: the conversation answer, the acceptance delivered at the calling
  endpoint and acknowledged by its delivery response, `updateEndpointMetadata` (`200`), the echo's `add` (`202`) and,
  on hanging up, `leave` (`204`). Every write was permitted by the fence on the call it registered; nothing was
  denied, and no delivery was answered but the acceptance.
- **The relay.** Over UDP 3478 the relay answers the Allocate with `300 Try Alternate`, naming another relay address
  on the same port. A TURN client follows it once, on the same transport, and allocates there (libwebrtc's does, as
  the browser's). Before it followed the redirect it fell back to TCP 443, which also carried the call: the relay
  speaks TURN's own framing over TCP (`ChannelData` padded to 4 bytes), not RFC 4571's length prefix. TURN over TLS
  wasn't needed, and the credentials worked as given (ASCII).
- **Opus's payload type.** Answering an Opus-only offer that numbered Opus `111` (the web client's number), the media
  controller answered `m=audio … 102` with `a=rtpmap:102 OPUS/48000/2`, its own number, then sent its audio numbered
  `111`, the number the offer gave it (a sender uses the receiver's numbers), and played back nothing of what we sent
  as `111`. A peer that keeps one number per codec for both directions (the native client's first one) took `102`
  from the answer and dropped every packet that came in as `111`. (Its answer to the web client's offer of nine audio
  codecs kept Opus at `111` and gave comfort noise and telephone events numbers of its own.) libwebrtc, as the web
  client's browser, sends with the answer's number and takes what comes in with the offer's, so the native client
  offers Opus with libwebrtc's own number and reads the answer as it is.
- **The media path.** Our side offers its relayed candidate only; the media controller answers with relay candidates
  of its own on port 3478, and ICE and DTLS connect through the relay. From the action to the media connected took
  2.3–2.9 s.
- **What comes back, and when**, counted from the call's devices opening (when the start is answered, about 1 s
  before the media connects): silence for about 3 s, the echo's greeting (about 7 s of speech), about 15 s of silence
  while it records, about 2 s of speech, the recording played back (about 14 s), then speech again; the call stays up
  until the caller leaves. The media controller sends nothing while its side is silent (`usedtx=1`). The echo records
  what we send from about 10 s to 24 s and **plays the recording back as a whole**: every 2 s stretch of the keyed
  tone comes back at one offset, 16.7–16.9 s later depending on the run (within a run the onsets agree to 50 ms), at
  the level it was sent at (0.495 for 0.5). It doesn't re-time packet by packet, so a frame's "round trip" through
  it is the recording's delay, not the network's.
- **Holding the call.** The playback ends about 41 s after the media connected: `--seconds 45` hears it, a 20 s call
  ends before the echo plays anything back.
- **The end.** In one run the call controller delivered `call/end` (`CallEndReasonLocalUserInitiated`) for our `leave`
  before the `leave`'s own `204`; the native client ends a call it is leaving as hung up whichever comes first.
- **The echo leaves by itself** (one live run of 2026-10-29: the retired native probe, the lab build,
  account a). Its roster entry came about 2 s after the media connected and went (the participants empty) about 50 s
  after, once it had played the recording back and spoken again; nothing else came: no `call/end`, no
  `conversationEnd`, and the call stayed up with the account alone in it until our `leave` at 150 s. The web client
  leaves the test call once the echo has left (the M0 runs saw it leave by itself after the echo left a silent
  recording), and so does the native client now: the echo's entry gone from a connected test call's roster sends the
  `leave`, and the call ends as the far end's.
- **Its camera, as a 1:1 call's** (one live run of 2026-10-28: the retired native probe, the lab build, account a). The start offers a 1:1 call's main video line, receiving; the acceptance
  hands out `mediaRenegotiation` but no `updateMediaDescriptions`, so the camera goes on as in a 1:1 call: a
  `renegotiate` with the line `sendrecv`, which the media controller answered at the renegotiation's `mediaAnswer`
  callback, then the answer's receipt and `acknowledge`. The camera's frames went out (628 sent), the echo sent no
  video, and it still played the tone back (round trip p50 16.7 s). Its roster entry (`active`, an endpoint in the
  call, no display name) came once the `add` went. Nothing was denied.

The three runs that heard the tone back (load average 2–4), each onset of the keyed tone in the stretch the echo
recorded, from going into the microphone to coming out of the speaker, on one clock:

| run | setup (action → media connected) | stretch recorded (of what was sent) | onsets timed | p50 | p95 | max |
|---|---|---|---|---|---|---|
| 1 | 2343 ms | 10 s – 24 s | 17 | 16 681 ms | 16 681 ms | 16 681 ms |
| 2 | 2897 ms | 12 s – 26 s | 17 | 16 711 ms | 16 731 ms | 16 731 ms |
| 3 | 2390 ms | 10 s – 24 s | 16 | 16 871 ms | 16 871 ms | 16 871 ms |

## Open

1. **Reading call participants.** The 2026-10-20 native/browser probe of `getAllParticipants` sent
   `{"from": <caller participant>, "scope": "call" | "lobby", "scenario": 0}` to the conversation's link. It answered
   `200` with `{"participants":[]}` even while the caller was present. It therefore does not establish complete call
   membership. The web client uses this result for its large-meeting lobby panel. Current call tracking binds links
   and state; it does not derive a lab participant policy from this read.


2. **Whether the echo answers.** Settled in M3 Task 4: with `add` allowed it joins, records and plays back
   ([What the M3 run added](#what-the-m3-run-added)), once the client's noise suppression is off.
3. **Headless Chrome.** The runs set a normal Chrome `User-Agent`; whether `HeadlessChrome` alone would keep the
   calling stack off wasn't isolated.
