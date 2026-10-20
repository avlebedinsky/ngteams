# Spike 3b: a native echo call

Date: 2026-10-20. M0 Task 19 (part 2 of spike 3). Code: `spikes/echo-call` (library plus the `echo-call` binary), on
top of `spikes/media-loop` (Task 17) and `crates/teams-auth` (Tasks 13–14).

## Question

Can our own Rust code place the Teams echo ("test call") call from lab account A, carry its audio over `str0m` and
our own TURN client (TCP 443 when UDP fails), and hear our tone come back?

## Answer: not yet. The media side works offline; the call itself is blocked

No live call was placed, and none could be:

- **No skypetoken.** The lab accounts have no Teams license: authz answers 403
  `UserLicenseNotPresentTrialEligible` for a, b and c (`docs/spikes/auth.md`,
  `docs/backlog/lab-accounts-have-no-teams-license.md`). Calling needs Teams' own tokens and endpoints, which come
  from that exchange.
- **No capture of a real echo call** (Task 18). It needs the same license, so there are no signaling fixtures, no
  confirmed endpoint for the call start, no confirmed SDP dialect, no confirmed TURN token flow, and no MRI for the
  echo service in `lab.toml`.

So the round-trip delay through Teams' echo service and the transport Teams would pick are **unknown**. Everything
that can be built and tested without Microsoft is built and tested; nothing in this task sent traffic to Microsoft,
ran a browser, or answered a call.

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
  servers in order (a relay descriptor gives every address over UDP first, then over TCP), so a UDP relay that stays
  silent falls back to TCP after the allocation timeout.
- Tested on synthetic exchanges with a scripted server (`tests/turn.rs`), on loopback against a minimal
  TURN server of our own (`tests/relay.rs`), and **against coturn 4.6.3** (the `coturn/coturn` container, on loopback
  only, with `--allow-loopback-peers`, started and stopped by hand for this run): allocation, permissions, channels and
  the full echo below passed over UDP and over TCP. Run it again with `ECHO_CALL_TURN_SERVER=127.0.0.1:<port>`,
  `ECHO_CALL_TURN_USER`, `ECHO_CALL_TURN_PASSWORD` and `cargo test -p echo-call --test relay external`.

### str0m through the relay

`media_loop::peer::Peer::with_candidate` takes a relayed candidate (`Candidate::relayed(<allocation>, <socket to the
server>, "udp")`); `relay::RelayedPeer` carries what str0m sends from the allocation through the TURN client and hands
relayed data back with the allocation as its destination. The echo probe (`src/echo.rs`) connects (ICE, DTLS, SRTP),
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

### Signaling (unconfirmed — needs the Task 18 capture)

`src/signaling.rs`, shaped only from the web client's bundle (version 26260.1717.5229.7049, read through
`proto-lab bundles search` and the calling chunk), in our own words, with hand-written fixtures in
`spikes/echo-call/tests/fixtures/synthetic/`, each marked `_synthetic` (none of them is a capture; the fixture guard
passes on them):

- the call start's payload (`participants.from/to`, `conversationRequest.links`, `callInvitation` with
  `callModalities`, the five call callback links and `mediaContent {contentType: application/sdp-ngc-1.0, blob: <SDP
  offer>}`), built only from an `Authorized<StartCall>`, so its participants are the permitted ones;
- the callbacks by path (`/call/progress/`, `/call/mediaAnswer/` with the remote SDP and its acknowledgement link,
  `/call/acceptance/`, `/call/redirection/`, `/call/end/`) and a small state machine for our one outgoing call;
- **the incoming-call policy**: a payload with a `callNotification` is an incoming call whatever its path, and
  `OutgoingCall::on_inbound` answers it with `Action::Nothing` in every state; there is no action that could accept
  or reject it (`tests/signaling.rs`). `teams-proto` has no request that answers a call either;
- the relay descriptor (`addresses`, `fqdns`, `udpPort`, `tcpPort`) and relay token (`username`, `password`, `realm`,
  `expires`) the bundle checks, turned into TURN servers (UDP first, then TCP) and credentials.

### The call through the fence

`teams_proto::StartCall` is a new mutating request: target `WriteKind::Call` on `Scope::New`, its participants the
caller and the callees, so the fence judges both. `src/call.rs` runs the flow: A's MRI and the echo service's MRI from
`lab.toml` → `Fence::check` + `Authorized::new` → A's skypetoken from the Secret Service → the call start. The fence
comes first, so no token is read for a call that isn't allowed, and the keyring isn't opened before the fence allowed
the call. Today the binary stops at the first step:

```
$ cargo build -p echo-call && target/debug/echo-call
echo-call: the echo service has no MRI in lab.toml yet: Task 18's capture fills it in (blocked: the lab accounts have no Teams license)
```

With `--callee <mri>` the fence decides, and denies any callee `lab.toml` doesn't approve (tested: `Outsiders`, logged
to `fence.log`). With an approved echo MRI the next stop is the missing skypetoken; with a token, `call::start`
refuses on purpose: the signaling is unconfirmed, and nothing unconfirmed is sent to Microsoft (`tests/call.rs`).

## Blocked, and what unblocks it

| Item | Blocked by | Unblocked when |
|---|---|---|
| skypetoken for A | no Teams license (403 at authz) | the lab accounts get a license or the Exploratory trial (backlog item) |
| echo service MRI in `lab.toml` | Task 18's capture | the capture shows the callee of the web client's test call; the bundle's prod config has a `28:<guid>` value for `teamsEchoBotMri`, a candidate to confirm, not to copy into `lab.toml` unseen |
| call start, callbacks, TURN tokens | Task 18's capture | `docs/protocol/calling-echo.md` and `tests/fixtures/calling/echo/*.json` exist; then `signaling.rs` is checked against them and `call::start` sends |
| the live echo: RTT, transport used | all of the above | `echo-call` runs end to end |

## Task 18's capture (2026-10-20): what it settled for this spike

The lab accounts got a license, and `proto-lab run echo-call` captured the web client's own test call
([calling-echo.md](../protocol/calling-echo.md)). For this spike:

- **Echo MRI**: established and recorded in `lab.toml` (`proto-lab service-mri echo <mri>`), so `call.rs`'s first
  stop is gone; the bundle's `28:<guid>` candidate was the one the live client uses.
- **Confirmed**: the call start's payload shape and its callback links, callbacks through Trouter at `…/call/<name>/`,
  Opus with `useinbandfec`, BUNDLE and rtcp-mux on both sides (question 3 in part), TURN over UDP 3478, TCP 443 and
  TLS 443 offered (question 5: TLS on 443 is in use, so TURNS matters).
- **Corrected, for a Task 19 follow-up**: the endpoint is `POST https://api.flightproxy.teams.microsoft.com/api/v2/epconv`
  with `X-Skypetoken` (question 1); the test call starts a conversation with nobody else in it
  (`scenario: "UserInitiatedTestCall"`, `to: []`) and adds the echo bot with `…/conv/<id>/add`, where
  `call_start_body` puts the bot in `to`; the blob is a Teams SDP dialect, not the browser's SDP as is (question 3);
  the remote SDP and the call leg's links come with the `call/acceptance/` callback and are acknowledged by a `POST`
  (no `call/mediaAnswer/` was seen); TURN credentials come from `GET https://teams.microsoft.com/trap/tokens`,
  `{"tokens":[{realm, username, password}],"expires":<seconds>}`, 7 days (question 4), and no relay descriptor
  appears: the relay is a fixed host. The code is unchanged; `call::start` still refuses to send.
- **Still open**: the echo never joined in the capture, because the fence denies every write on an existing call (the
  `add` among them) until it can read a call's members; so whether the echo re-times its playback (question 6) and
  the round-trip delay through it are still unknown. The same holds for the native client: its follow-ups on the call
  need that membership read too (`docs/backlog/fence-call-follow-ups.md`).

## Open questions for M3

1. **Where the call start goes and how it is authenticated.** The bundle builds the payload but the endpoint is
   resolved at runtime (a regional GTM or the call controller URL from config?), and so is its auth (skypetoken,
   or an IC3 Entra token in the token-migration mode `docs/protocol/auth.md` describes).
2. **Callbacks need Trouter.** The service calls us back on the links we send, through our Trouter endpoint. That is
   M1's `teams-trouter`; how a native client registers a calling endpoint (the registrar `v2`/`v3` registrations in
   the bundle) is unknown, and registering makes the account reachable by coworkers: the incoming-call policy above
   must be in place first.
3. **`application/sdp-ngc-1.0`.** Is it plain SDP (str0m's offer as is), SDP with Teams-only attributes, or a
   multi-part body? Does the echo service accept str0m's codecs and extensions (Opus with `useinbandfec`, BUNDLE,
   rtcp-mux, which header extensions)?
4. **TURN token flow.** The relay token service (`edge.skype.com/trap/tokens` in the bundle): request, auth, the
   unit of `expires`, how many relays it returns, and their realm.
5. **TCP framing of Teams' relays.** Native TURN framing (what browsers speak, and what coturn accepts) or the
   RFC 4571 prefix? Both are implemented; the capture's relay candidates (`relayProtocol`) and a TCP allocation tell.
   TLS on 443 (TURNS) isn't implemented yet; the descriptor's `tcpPort` may well mean TLS.
6. **Does the echo service re-time its playback?** It records and then plays back, so per-frame RTT matching by RTP
   time may find nothing; the probe then only checks the frequency, and the RTT has to be measured from the first
   echoed frame instead.
7. **ICE details.** The web client may send relay-only candidates (`iceTransportPolicy` appears in the bundle), and
   Teams' media relays may be ICE-lite. Our peer offers one relayed candidate; whether host candidates should be
   added is a privacy and connectivity choice for M3.
8. **Credentials preparation.** The client takes TURN credentials as given (no `OpaqueString`/SASLprep), which is
   fine if Teams' relay credentials are ASCII, unconfirmed until the Task 18 capture (no Teams relay credentials have
   been seen yet); MESSAGE-INTEGRITY-SHA256 and `PASSWORD-ALGORITHMS` (RFC 8489) are not implemented.
