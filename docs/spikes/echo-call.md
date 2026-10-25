# Spike 3b: a native echo call

Date: 2026-10-20. M0 Task 19 (part 2 of spike 3), brought in line with Task 18's capture of the web client's test
call ([calling-echo.md](../protocol/calling-echo.md)) the same day. Code: `spikes/echo-call` (library plus the
`echo-call` binary), on top of `spikes/media-loop` (Task 17) and `crates/teams-auth` (Tasks 13–14).

Since M3 the TURN client, its relay link and their tests live in `crates/teams-media` (`stun`, `turn`, `relay`,
`tests/turn.rs`, `tests/relay.rs`, the loopback TURN server `test_turn`), and the SDP conversion this write-up calls
unbuilt is `teams_media::sdp` (`docs/protocol/calling.md`, "The SDP dialect"). The spike crate itself is gone (M3 Task
14): its signaling is `crates/teams-call`'s, the call flow through the fence and its media the engine's
(`teams_core::calling`), and the echo's tone check `teams_media::tone` and the call session's tests; the paths below
are the spike's as it was.

## Question

Can our own Rust code place the Teams echo ("test call") call from lab account A, carry its audio over `str0m` and
our own TURN client (TCP 443 when UDP fails), and hear our tone come back?

## Answer: not yet. The signaling is known and built; the call is blocked by the fence

No native call was placed. The first blockers are gone: the lab accounts are licensed (each gets a skypetoken), and
Task 18 captured the web client's own test call, so the echo service's MRI is in `lab.toml` and the signaling below
follows that capture. What blocks now is the fence, and it blocks the web client too:

- **The echo joins only through writes on a call that exists.** The test call starts with nobody else in it; the
  caller then adds the echo bot (`POST …/conv/<id>/add`), acknowledges the call controller's acceptance and, at the
  end, leaves. The fence permits a write on an existing call only after a fresh, complete read of that call's
  members, and nothing gives one: the conversation's `getAllParticipants` answered an empty list while account a was
  in the call (`docs/backlog/fence-call-follow-ups.md`). So in Task 18 the web client's leg reached Teams' media
  controller, the echo never joined and no tone came back, and a native start would end the same way, in a call it
  couldn't even leave. `call::start` therefore sends nothing.

So the round-trip delay through Teams' echo service and whether it re-times its playback are still **unknown**.
Nothing in this task sent traffic to Microsoft, ran a browser, or answered a call.

## What works (offline, tested)

### TURN client (RFC 8656)

`src/stun.rs`, `src/turn.rs`, `src/relay.rs`.

- A STUN codec with the TURN methods and attributes, `MESSAGE-INTEGRITY` (HMAC-SHA1), `FINGERPRINT` and XOR
  addresses. It decodes and verifies all four RFC 5769 test vectors (short-term request, IPv4 and IPv6 responses, the
  long-term request with the MD5 key), and its XOR-MAPPED-ADDRESS bytes match the RFC's.
- A sans-IO client for one allocation: Allocate (unauthenticated first, then with the realm and nonce from the 401),
  Refresh before expiry, CreatePermission and ChannelBind with their refreshes (4 of 5 and 9 of 10 minutes), Send/Data
  indications and `ChannelData`, 438 stale-nonce renewal, retransmission over UDP per RFC 8489 (over TCP a request is
  sent once and waits up to 39.5 s), release with lifetime 0 (a 437 to it counts as released). Responses to
  authenticated requests are acted on only with a valid `MESSAGE-INTEGRITY`, the 401/438 challenge errors excepted:
  over UDP a forged or missing one is dropped and the request retransmitted; over TCP, where nothing is retransmitted,
  it ends the request (`TurnFailure::Unverified`, with the code it claimed).
- Only a failed Allocate or Refresh (401 twice, any other 4xx/5xx, 300 Try-Alternate, no answer) ends the allocation.
  A refused or unanswered CreatePermission or ChannelBind is reported as `TurnEvent::RequestRefused` and the
  allocation stays: a refused one isn't asked again for that IP or peer and data goes by Send indication; an
  unanswered one is asked again on the next send, a ChannelBind under the same channel number (the server may hold
  the binding already). A peer keeps its channel number for the allocation's life.
- On real sockets: UDP and TCP. On TCP the messages are framed natively (RFC 8656 12.5, `ChannelData` padded to 4
  bytes; what coturn and browsers speak) or with the RFC 4571 2-byte length prefix (`Framing::Rfc4571`), which TURN
  over TCP doesn't normally use; which one Teams' relays expect is an open question below. `allocate_any` tries the
  servers in order (`relay::teams_relay` gives Teams' relay over UDP 3478 first, then over TCP 443), so a UDP relay
  that stays silent falls back to TCP after the allocation timeout. TURN over TLS (`turns:`, 443) isn't implemented.
- Tested on synthetic exchanges with a scripted server (`tests/turn.rs`), on loopback against a minimal
  TURN server of our own (`tests/relay.rs`), and **against coturn 4.6.3** (the `coturn/coturn` container, on loopback
  only, with `--allow-loopback-peers`, started and stopped by hand for this run): allocation, permissions, channels and
  the full echo below passed over UDP and over TCP. Run it again with `ECHO_CALL_TURN_SERVER=127.0.0.1:<port>`,
  `ECHO_CALL_TURN_USER`, `ECHO_CALL_TURN_PASSWORD` and `cargo test -p echo-call --test relay external`.

### str0m through the relay

`Peer::with_candidate` (`media_loop::peer` then, `teams_media::peer` since M3) takes a relayed candidate
(`Candidate::relayed(<allocation>, <socket to the server>, "udp")`); `RelayedPeer` (this spike's `relay` then,
`teams_media::peer` since M3) carries what str0m sends from the allocation through the TURN client and hands relayed
data back with the allocation as its destination. The echo probe (`src/echo.rs`) connects (ICE, DTLS, SRTP),
sends a 440 Hz tone as Opus in real time and decodes what comes back.

In `tests/relay.rs` our peer sits behind the relay with the relayed candidate as its only one, and an echo stand-in (a
plain UDP `str0m` peer that sends every frame straight back) answers its SDP. Measured on loopback (debug build, 50
frames of 20 ms, round trip = encode start → decode end of the echo):

| relay | transport | frames back | tone back | connect | RTT p50 | RTT p95 |
|---|---|---|---|---|---|---|
| ours | UDP | 50/50 | 440 Hz | 30 ms | 9.1 ms | 18.9 ms |
| ours | TCP | 50/50 | 440 Hz | 20 ms | 8.6 ms | 19.1 ms |
| coturn 4.6.3 | UDP | 50/50 | 440 Hz | 12 ms | 4.1 ms | 6.5 ms |
| coturn 4.6.3 | TCP | 50/50 | 440 Hz | 26 ms | 6.3 ms | 18.0 ms |

(Single runs on a loaded machine; our test server naps 0.2 ms when idle, coturn doesn't. These numbers say the path
works, not what Teams' relays would add.)


### Signaling (from the Task 18 capture)

`src/signaling.rs`, tested in `tests/signaling.rs` on the Task 18 fixtures (`tests/fixtures/calling/echo/*.json`,
synthetic, shaped like the capture, each marked `_synthetic`); what the capture didn't have (a progress, an end, an
incoming call) on the hand-written fixtures in `spikes/echo-call/tests/fixtures/synthetic/`. The fixture guard passes
on both.

- **TURN credentials**: `TrapTokens` reads the answer of `GET https://teams.microsoft.com/trap/tokens`,
  `{"tokens":[{realm, username, password}], "expires": <seconds>}` (one credential, 7 days; the realm `"rtcmedia"`
  with its quotes inside the string, which come off). The relay is one fixed host,
  `gateway-world.az.relay.teams.cloud.microsoft` (`turn::TEAMS_RELAY_HOST`): UDP 3478, TCP 443 and TLS 443. There is
  no relay descriptor; the bundle-shaped `RelayDescriptor` is gone.
- **The call start**: `POST https://api.flightproxy.teams.microsoft.com/api/v2/epconv` with `X-Skypetoken`.
  `call_start_body`, built only from an `Authorized<StartCall>`, sends `participants.from` (our MRI, display name,
  endpoint id, participant id, language), an **empty `to`**, `conversationRequest` with the scenario
  `UserInitiatedTestCall`, the roster and conversation callbacks, and `callInvitation` with `Audio`, the five call
  callbacks and `mediaContent` (`application/sdp-ngc-1.0`). The test checks it against the captured start: the same
  participants and links, every key one the client sent, and the browser fence's own mapping
  (`teams_fence::contacting`) reads it as exactly the `StartCall`'s target. The rest of the client's body
  (capabilities, endpoint state, client details) is left out; whether the service needs any of it is unconfirmed.
- **Callback links**: `CallbackLinks` builds each link as the web client does, `<Trouter push URL>callAgent/<agent
  id>/<8 hex digits of its own>/<call|conversation>/<name>/`, and `OutgoingCall` acts only on deliveries at its own
  links' paths: another call's delivery changes nothing.
- **The answer**: `Conversation` reads the start's answer: its controller URL (`teams_proto::ConversationUrl`: https,
  a host, a non-empty conversation id as the last path segment; anything else is no conversation) and its operation
  links (`leave`, `getAllParticipants`, …). The client posts the `add` to the controller plus `/add`, query kept, not
  to the answer's `addParticipant` link.
- **The acceptance**: the call controller calls back at the `acceptance` link with `callAcceptance.mediaContent`, the
  media controller's SDP answer, and `callAcceptance.links` (`acknowledgement`, `callLeg`, `mediaRenegotiation`, …).
  `Inbound::Accepted` carries the answer and the two links; `OutgoingCall` asks to apply and acknowledge it once (the
  service redelivers it while it isn't acknowledged). No `mediaAnswer` delivery was seen; one at that link changes
  nothing.
- **The SDP dialect**: both the offer and the answer are Teams' `application/sdp-ngc-1.0`, not the browser's SDP:
  `RTP/SAVP` on real ports with the candidates inline, `a=rtcp:<port>`, `b=CT`, `a=x-ssrc-range`, `a=x-signaling-fb`,
  `a=label`, and in the answer an `m=x-data` line. `NgcSdp` only tells it apart from a browser's (or str0m's) SDP and
  refuses the latter; converting between the two is not built.
- **The `add`**: `add_request`, built only from an `Authorized<AddToCall>` (its URL is `AddToCall::url`, its
  invitee the request's), is posted where the client posts it and matches the captured `add` (the invitee with
  a fresh participant id, the 1:1 chat with the bot, the caller as organizer, the `add`'s callbacks). Tested with a
  stand-in membership source, because no real one can permit it.
- **The incoming-call policy**: a payload with a `callNotification` is an incoming call whatever its path, and
  `OutgoingCall::on_delivery` answers it with `Action::Nothing` in every state; there is no action that could accept
  or reject it. `teams-proto` has no request that answers a call either. (The capture had no incoming call, so its
  shape is still the bundle's.)

### The call through the fence

`teams_proto::StartCall::test_call(caller, echo)` is the call start: target `WriteKind::Call` on `Scope::New` with
the caller and the echo. The start doesn't name the echo; it is counted in so that the fence lets a test call start
only when it would admit the echo too, the same target the browser fence gives the web client's start.
`teams_proto::AddToCall` is the `add`: `WriteKind::Invite` on `Scope::Call { id: <controller URL> }` with the
invitee added. The id is the whole controller URL (host, path, query), and the `add` is posted to that same URL plus
`/add`, so a permit for one call never carries an `add` to another, not even one with the same conversation id on
another conversation host. The fence must read the call's members, which no source can (`FailClosed` reads nothing,
`LiveMembershipSource` reads chats only): it is denied as partial (`tests/call.rs`,
`crates/teams-proto/tests/fence_types.rs`).

`src/call.rs` runs the flow: A's MRI and the echo's MRI from `lab.toml` → `Fence::check` + `Authorized::new` → A's
skypetoken from the Secret Service → the call start, which `call::start` refuses (`CallError::FollowUpsUnfenced`). The
fence comes first, so no token is read for a call that isn't allowed. The echo is always `lab.toml`'s `echo` service
(there is no flag for another: the start doesn't name the echo, so the target would describe a call the body doesn't
place), the `add` invites the echo of the permitted start, and a test call for an echo the roster doesn't approve is
denied (`Outsiders`, logged to `fence.log`).

## Blocked, and what unblocks it

| Item | Blocked by | Unblocked when |
|---|---|---|
| the `add`, the acceptance's acknowledgement, the `leave` (and so the echo joining) | the fence can't read a call's members fresh and in full: `getAllParticipants` answered an empty list mid-call | an authoritative read of a call's members, or evidence that a test call's membership can change only through the caller's fenced `add` (`docs/backlog/fence-call-follow-ups.md`) |
| sending the call start | the item above: a start whose follow-ups the fence denies leaves a call that can't be completed or left | the item above; then `call::start` sends the start, the `add` and the acknowledgement through the fence |
| str0m talking to Teams' media controller | the SDP dialect: str0m's offer has to be written as `sdp-ngc-1.0` and the answer read back | the conversion is built and checked against the captured offer and answer |
| the live echo: RTT, re-timing, transport used | all of the above | `echo-call` runs end to end |

## Next step for M3

1. Resolve the fence's call membership (the backlog item above): without it neither the web client nor ours gets an
   echo, and the native call shouldn't be placed.
2. Write the SDP conversion both ways: str0m's offer (audio only to begin with) into `sdp-ngc-1.0` with the relayed
   candidate inline, and the media controller's answer into standard SDP for str0m (drop `m=x-data` and Teams'
   attributes, `RTP/SAVP` back to `UDP/TLS/RTP/SAVPF`). Check it offline on the captured offer and answer.
3. A native Trouter session and endpoint registration (M1's `teams-trouter`): the callbacks come over it, and the
   acceptance's response over Trouter (`callAcceptanceAcknowledgement`) is itself a write on the call.
4. Then run `echo-call` live: TURN credentials from `trap/tokens`, the start, the acceptance, the `add`, the tone, the
   `leave`, and measure the round trip through the echo service.

## Open questions for M3

Answered by the native client's own test call (M3 Task 15):
[calling-echo.md, "The native echo call"](../protocol/calling-echo.md#the-native-echo-call).

1. **Which parts of the call start the service needs.** The native body is a subset of the client's keys; the
   capabilities, endpoint state and client details may be required.
2. **Trouter and registration.** The capture shows the client's Trouter session and its registration with the
   registrar (`docs/protocol/calling-echo.md`); registering makes the account reachable by coworkers, so the
   incoming-call policy above must be in place first.
3. **`application/sdp-ngc-1.0`.** Which of Teams' attributes the media controller requires in an offer, and whether
   it takes an audio-only offer (the client offers 13 media lines: audio, video slots and data).
4. **TCP framing of Teams' relays.** Native TURN framing (what browsers speak, and what coturn accepts) or the
   RFC 4571 prefix? Both are implemented; a TCP allocation tells. TURN over TLS on 443 isn't implemented, and the
   client offers it.
5. **Does the echo service re-time its playback?** It records and then plays back, so per-frame RTT matching by RTP
   time may find nothing; the probe then only checks the frequency, and the RTT has to be measured from the first
   echoed frame instead.
6. **ICE details.** The web client gathers host, server-reflexive and relay candidates (`iceTransportPolicy: all`),
   and the media controller answers with relay candidates and no `ice-lite`. Our peer offers one relayed candidate;
   whether host candidates should be added is a privacy and connectivity choice for M3.
7. **Credentials preparation.** The client takes TURN credentials as given (no `OpaqueString`/SASLprep), which is
   fine if Teams' relay credentials are ASCII (not checked); MESSAGE-INTEGRITY-SHA256 and `PASSWORD-ALGORITHMS`
   (RFC 8489) are not implemented.
