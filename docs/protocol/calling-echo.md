# Calling: the web client's test call to the echo service

Status: 2026-10-20, M0 Task 18 (spike 3b, part 1). Written from live runs of `proto-lab run echo-call --headless`
(account a, Chrome for Testing 155 headless, a fake microphone playing a 440 Hz WAV tone, the fence armed with the
calling classifiers), from the web client's own diagnostic log of one of those runs (Ctrl+Alt+Shift+1, read locally),
and from the web bundle `26260.1717.5229.7049`. The captures stay outside the repository; the fixtures in
`tests/fixtures/calling/echo/` are synthetic, shaped like them. Ids, tokens, hosts' numbers and addresses below are
placeholders.

The code: the scenario is `crates/proto-lab/src/scenario/echo_call.rs`; what the fence lets through for a run that
calls is `crates/proto-lab/src/calling.rs` (`Classifier::calling`, `FrameClassifier::calling`), and audio worklets
are armed by `TargetKind::Worklet` (`crates/proto-lab/src/arming.rs`).

## Outcome

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
name, only a bot's `28:<guid>`).

## What has to be up before a call

The web client starts its calling stack lazily (`enableDeferredCSI`): "Make a test call" asks for it, and it is
deferred ("Deferring calling stack Initialization. [entryPoint:startEchoBotCall]" in the client's log) until the Calls
app or an app pre-warm starts it. A headless lab run only gets it by opening the Calls app first. A Settings deep link
reloads the page and loses it again, so the scenario opens Settings from the account menu.

Before that, the calling stack needs (each one is own-state of the account in `Classifier::calling` /
`FrameClassifier::calling`, strict shapes, tests in `crates/proto-lab/tests/calling.rs`):

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
  empty body for most, the call's `callAcceptanceAcknowledgement` for the acceptance callback. **These responses stay
  unknown and denied**: a response goes back to whoever sent the delivery, and which deliveries an empty success only
  acknowledges (and which it might answer: an incoming call's) isn't established (Codex and Claude, 2026-10-20). The
  service redelivers; the calls still reached the media controller without them.
- **Endpoint registration.** `POST <teams>/registrar/prod/V2/registrations` with `clientDescription` (`appId`,
  `templateKey`, …), a `registrationId` and `transports.TROUTER: [{"context":"","path":"<push url>","ttl":3600}]`;
  `202`. The messaging stack registers as `TeamsCDLWebWorker`; the calling stack opens a second Trouter socket and
  registers it as `SkypeSpacesWeb`. This makes the account reachable: an incoming call would be delivered to the page.
  **Incoming calls are never answered**: answering is a request to the call's `acceptance` link, which the fence
  doesn't map (denied), the delivery responses above are denied too, and the scenario never clicks an answer.

Nothing else of the client's unmapped traffic (presence, the middle tier's other lookups, the async media gateway's
cookie exchanges, telemetry) was needed; it stays denied.

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
   create, `Scope::New`, judged by its members.
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
   **The fence maps it** (`calling::call_start`), and only this shape: `WriteKind::Call` on `Scope::New`, its
   participants `from` and the echo service of `lab.toml`, which the start doesn't name and is counted in, so the call
   passes only if the fence would admit the echo. The scenario must be `UserInitiatedTestCall`, `to` empty or exactly
   the echo service, and every key at every level one the capture had, its value of the captured type (`null` where it
   was `null`: `replaces`, `transferor`, `clientTransferContext`, the group, chat, meeting and content sharing). Any
   other call (a named callee, another scenario, an unreviewed key anywhere) or a body that doesn't read strictly is
   unknown.
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
6. **Follow-ups the client sends next** (all denied today, see [Open](#open)):
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
server-reflexive candidate to the controller's relay candidate.

## Open

1. **Fencing a call's follow-ups.** `updateEndpointMetadata`, `applyChannelParameters`, `acknowledge`, `add` and
   `leave` land on a call that exists. The fence requires its members read fresh and complete; neither the chat
   service read nor anything else gives that for a call yet. Candidates: the conversation's `getAllParticipants` link
   (method and answer not captured), or protocol evidence that a test call's membership can only change through the
   caller's own fenced `add` (Codex and Claude, 2026-10-20: the evidence so far doesn't establish it). Backlog:
   `docs/backlog/fence-call-follow-ups.md`.

   `getAllParticipants`, tried on 2026-10-20 (one live run, account a, fence armed, writes still denied). The web
   bundle's client sends it as a `POST` to the conversation's link with `{"from": <the caller's own participant>,
   "scope": "call" | "lobby", "scenario": 0}`, keeps only `participants` from the answer, and uses it for the large
   meeting lobby panel. Sent from inside the fence for the test call's `call` scope, it answered `200` and
   `{"participants":[]}` at every follow-up, although a was in the call. So in this call it doesn't list the
   caller, and it can't serve as the call's membership. The fence denied the follow-ups as partial. Also seen: the
   call controller's call id (`callAcceptance/<id>`, `mcProxy/<id>`) is tied to the conversation only in a delivered
   `conversationUpdate` (`activeModalities.call.links.participants`, `…/cc/v1/callHub/<same id>/…/participantLegs`).
2. **Whether the echo answers.** With `add` allowed the echo service should join, record and play back; its timing
   (spike 3b question 6) is still unseen.
3. **Headless Chrome.** The runs set a normal Chrome `User-Agent`; whether `HeadlessChrome` alone would keep the
   calling stack off wasn't isolated.
