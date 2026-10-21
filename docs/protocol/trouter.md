# Trouter: the push channel for messaging

Status: 2026-10-20, M1 Task 14; the native client's part (below) 2026-10-21, M1 Task 16. Written from live runs of `proto-lab run ab-trouter --headless` (accounts a and b,
Chrome for Testing 155 headless, the fence armed with the **messaging set**, below), from M0's `echo-call` runs
(`calling-echo.md`), and from the web bundle `26260.1717.5229.7049` (read locally, notes in our own words). The
captures stay outside the repository; the fixtures in `tests/fixtures/trouter/` are synthetic, shaped like them
(`session.json`: one socket's frames both ways; `registration.json`: both stacks' registrations; `deliveries.json`:
the chat deliveries and the web client's answers). Ids, tokens, host numbers and addresses below are placeholders.

The code: the scenario is `crates/proto-lab/src/scenario/ab_trouter.rs` (`ab-trouter`, and `ab-trouter-quiet`, which
also holds back `user.activity`); what the fence lets through is `crates/proto-lab/src/calling.rs`
(`messaging_request`, `trouter_frame`) behind `Classifier::messaging` and `FrameClassifier::messaging`.

## Outcome

- Chat events reach the messaging stack's own socket as `POST /v4/f/<endpoint>/messaging` deliveries, `async` with a
  15-minute lifetime: new messages, typing, the chat's update, edits, reactions, deletes. a's message showed in b's
  open chat 3.5–4 s after a's compose box let it go (typing and the client's own rendering included; the delivery
  itself came 200–250 ms after the message's `originalarrivaltime`), an edit 1.2–1.6 s after a sent it, and a
  reaction's delivery reached b's socket 0.9 s after a's reaction request went out (0.13 s after its `200`).
- **Unanswered deliveries cost a bounded replay on every reconnect, nothing more**: deliveries stay unanswered, and
  the native client builds no response path ([Answering deliveries](#answering-deliveries)).
- **`user.activity` is not needed** for deliveries; the native client doesn't send it in M1
  ([`user.activity` and presence](#useractivity-and-presence)).
- The web client never re-authenticates an open socket; a renewed token takes a reconnect
  ([Token renewal](#token-renewal-on-a-live-socket)).

## The messaging set (what the fence lets out)

A run that chats over the push channel uses `Classifier::messaging` and `FrameClassifier::messaging`:

- the write mapping of `ab-chat` (`Classifier::mapped`, members read live);
- the registrar registration of the **messaging stack's** endpoint only: `appId` exactly `TeamsCDLWebWorker` and
  `templateKey` `TeamsCDLWebWorker_<digits and dots>` (`2.6` and `2.7` were seen), in the reviewed shape
  (`calling::messaging_request`). The calling stack's (`SkypeSpacesWeb`) stays unknown: it makes the account reachable
  for calls. Its denial, and its retries, are expected in this scenario's verdict (`TROUTER_RUN_REQUESTS`);
- the Trouter session's own frames on any Trouter socket (`calling::trouter_frame`, the same frames `echo-call`
  allows): `user.authenticate`, `ping`, the client's own `user.activity` (`active`/`inactive` only; the account showing
  online is accepted, the user's decision of 2026-10-19), `trouter.processed_message_loss` and the disconnect `0::`;
- nothing else: no settings reads, no call, and **no answer to any delivery** (`3:::` frames stay unknown and denied).

## What the native client may send (M1 Task 15)

The native client's socket sends only `teams_proto::OwnFrame`s, through `teams_proto::FrameSink`, each logged to
`fence.log` (`event` `frame`: the socket's URL without its query and the frame's kind, never its text) before it goes
out; a frame that can't be logged isn't sent. The list is sealed and holds the messaging stack's own session frames in
the shapes above:

| kind | frame |
|---|---|
| `trouter_authenticate` | `5:::{"name":"user.authenticate",…}`, the headers exactly `X-Ms-Test-User: False`, `Authorization`, `X-MS-Migration: True` (the messaging stack sent all three on every socket, the first one included; the calling stack's sockets sent no `X-Ms-Test-User`), plus the seven `connectparams` when a `trouter.connected` handed some over |
| `trouter_ping` | `5:<n>+::{"name":"ping"}` |
| `trouter_message_loss_ack` | `5:<n>+::{"name":"trouter.processed_message_loss",…}`, the indicators as they came (never an empty list) |
| `trouter_disconnect` | `0::` |

The sink numbers the acknowledged events itself (`<n>`, from 1 per socket). No `user.activity` and no answer to a
delivery: neither has a type, so neither can be sent.

## The native client (M1 Task 16)

`crates/teams-trouter` is the native push channel, tested against `teams-mock`'s Trouter (`TrouterMock`, which
answers as the fixtures here do and delivers the chat mock's queued events once a registration names the endpoint):

- **Connect**: `wss://go.trouter.teams.microsoft.com/v4/c`, then the `reconnectUrl` a `trouter.connected` handed over
  (a `<name>-t` host under `.trouter.teams.microsoft.com`, port 443 dropped as the web client's connect URL dropped
  it) with its `connectparams`, until those are within an hour of `se`, three connects in a row failed (any that
  ended before its `trouter.connected`, but for a renewal) or two `dup` disconnects came within 10 s. The query is
  the messaging stack's as captured: `tc` (`{"cv":…,"ua":"TeamsCDL","hr":"","v":"1415/<build>"}`, braces and quotes
  percent-encoded, `:`, `,` and `/` left), `timeout=40`, `epid` (one GUID per client run), `cor_id` (a GUID per
  connect), `con_num` (`<ms>_<n>`); the handshake carries the web client's `Origin: https://teams.microsoft.com` and
  no credential. TLS is checked by the system's verifier, as the HTTP transport's. The host allowlist and the TLS
  are lifted only by the `test-support` mock origin (`ws` on loopback), which a `fence-seal` case keeps out of a normal
  build.
- **Frames**: `user.authenticate` at once after `1::` (never before it: a stop before then sends nothing at all),
  pings every 40 s and a reconnect when a pong is 5 s late, the message-loss acknowledgement once the owner has caught
  up, and `0::` on a stop or a renewal (once authenticated). Each goes through `teams_proto::FrameSink` and is logged to `fence.log`
  first; the socket's write half goes only to the sink. No `user.activity`, no answer to any delivery, an incoming
  call's (a `callNotification`, whatever its path) included.
- **Registration**: the messaging stack's, as `teams_proto::EndpointRegistration` (an own-state request, logged
  first): the body above with `templateKey` `TeamsCDLWebWorker_2.6` and `platformUIVersion` the `tc`'s `v`, on every
  `trouter.connected`, again 30 s before its hour ends, after a failure again 123 s later. The transport sends the
  registrar `X-MS-Migration: True` and `X-Ms-Test-User: False` with the token.
- **Reconnect**: at once for a renewal, a missing pong, an `unauthorized` disconnect (with a new token) or a `dup`,
  but never twice within 10 s; otherwise after a backoff of `1 s · 2^n` ±20 %, capped at 300 s, whose count starts
  again after a connection stayed up a minute; a throttled token service's `Retry-After` is a floor on that wait,
  never a shortcut past it. `skypetoken-deprecated` stops the client.
- **Renewal**: an open socket is never authenticated again, so the client reconnects with a new token 5 minutes
  (the session's refresh margin) before the held one expires, checking the expiry against the wall clock every 30 s,
  and at once when the session announces a newer `ic3` token. A renewal before
  the service's `1::` sends nothing on that socket.

## Connect and the regional move

- The client opens `wss://go.trouter.teams.microsoft.com/v4/c?tc=<json>&timeout=40&epid=<endpoint guid>&ccid=…&
  cor_id=<guid>&con_num=<ms>_<n>`. `tc` names the client (`"ua":"TeamsCDL"` for the messaging stack,
  `"SkypeSpaces"` for the calling stack, plus its version). `timeout=40` is the ping interval in seconds. The
  handshake carries no credential.
- The service answers `1::` (socket.io 0.9 connect). The client must authenticate at once: once, with the
  authentication held about 1.5 s for the fence's decision under load, the service closed the socket with
  `0:::{"reason":"timeout"}` 1.1 s after `1::`; the client opened a new socket and authenticated in time.
- After `trouter.connected` (below) the client keeps the regional host and reconnects straight to `reconnectUrl`
  (`wss://<name>-t.trouter.teams.microsoft.com:443/v4/c`) next time, sending the cached `connectparams` in its
  `user.authenticate`. The cache survives a page load and even a browser restart (it lives in the profile): b's
  sockets reconnected to the same two regional hosts with the same endpoint ids in runs 45 minutes apart. Per the bundle
  the cache is dropped when `se` is less than an hour away, the service URL changes, three connects in a row fail, or
  the service disconnects twice for a duplicate within 10 s.
- Each stack opens its own socket and its own endpoint (`epid`), and one page load can leave one socket per document:
  b had two `TeamsCDL` sockets (one per page load) and one `SkypeSpaces` socket.

## Authentication: the first frame

`5:::{"name":"user.authenticate","args":[{"headers":{"X-Ms-Test-User":"False","Authorization":"Bearer <ic3 token>",
"X-MS-Migration":"True"},"connectparams":{"issuer":"","scae":"1","sig":"","sr":"<endpoint id>","sp":"<regional
name>","se":"<ms>","st":"<ms>"}}]}`

- The token is the `https://ic3.teams.office.com` one (the web client's is a CAE token, `xms_cc` `CP1`, living about
  24 hours); the registrar takes the same token. The calling stack's socket sends only `Authorization` and
  `X-MS-Migration`.
- `connectparams` is what an earlier `trouter.connected` handed over, sent back as it came; a first connect has none.
- Whether the service takes **our** non-CAE `ic3` token on Trouter is not tested yet (it does on the chat service,
  `auth.md` question 8). Task 16 built the native client against the mock only: no lab binary runs it yet (a live run
  never comes from a `cargo test` process), so the first live socket is Task 19's `teams-headless watch`.

## `trouter.connected`: the endpoint

`5:1::{"name":"trouter.connected","args":[{…}]}` with:

| field | meaning |
|---|---|
| `id` | the endpoint (connection) id; the last part of the push URLs |
| `ccid` | a short form of `id` |
| `url`, `surl` | the push URLs, `https://<name>-f.trouter.teams.microsoft.com:8443/v4/f/<id>/` and the same on `:3443`; the registration names `surl` |
| `curlb` | `https://<name>-t.trouter.teams.microsoft.com:443`, a base for client-to-client links |
| `healthUrl` | `…-t…:443/v4/h` |
| `reconnectUrl` | `wss://<name>-t…:443/v4/c`, where the next connect goes |
| `registrarUrl` | a v3 registrar on another host (`…/registrar/<geo>/v3/registrations`); the web client still registers through `<teams>/registrar/prod/V2/registrations` |
| `socketio` | `https://<name>-t…:443/` |
| `ttl` | seconds the connection's parameters stay valid (about 6.4 days: `se` minus now) |
| `dur` | `"1"` |
| `connectparams` | `issuer`, `scae`, `sig`, `sr` (the endpoint id), `sp` (the regional name), `se`/`st` (expiry and start, ms) |

## Keep-alives and acknowledgements

- The client sends `5:<n>+::{"name":"ping"}` every 40 s per socket; the service answers `6:::<n>+["pong"]` within
  about 100 ms. Every event the client sends with `+` (ping, `user.activity`, `processed_message_loss`) is
  acknowledged with `6:::<n>+[…]`. Per the bundle a pong missing for 5 s makes the client reconnect at once, and a
  `user.activity` not acknowledged within 10 s does too.
- The client also pings right after every answer it sends to a delivery (seen: each denied answer was followed by a
  ping within 100 ms).
- The bundle keeps a 72 s socket.io heartbeat timeout; any packet resets it.

## Message loss

On connect the service sends `5:<n>::{"name":"trouter.message_loss","args":[{"droppedIndicators":[{"tag":"messaging",
"etag":"<time>"},…]}]}`, sometimes twice or three times. The messaging stack saw only the `messaging` tag; the calling
stack's socket also saw `""`, `messagingsync`, `pinnedchannel` and `tps`. The client acknowledges each report with
`trouter.processed_message_loss` carrying the same indicators. Per the bundle the `messaging` tag makes the chat
layer run a delta sync (it missed pushes while away), and a new connection id does too. For the native client: a
`messaging` loss report means "catch up now" (Task 17), then acknowledge it.

## Registration and its TTL

`POST <teams>/registrar/prod/V2/registrations` (both `teams.microsoft.com` and `teams.cloud.microsoft` were used),
`Authorization: Bearer <ic3>`, `X-MS-Migration: True`, `X-Ms-Test-User: False`, JSON:

```json
{"clientDescription":{"appId":"TeamsCDLWebWorker","aesKey":"","languageId":"en-US","platform":"chrome",
 "templateKey":"TeamsCDLWebWorker_2.6","platformUIVersion":"<version>"},
 "registrationId":"<guid>","nodeId":"",
 "transports":{"TROUTER":[{"context":"","path":"<surl>","ttl":3600}]}}
```

- Answer `202`, empty body. The `registrationId` is the socket's `epid`.
- The messaging stack registered on every `trouter.connected` (every page load and reconnect). Per the bundle it
  registers again 30 s before the TTL ends (3600 s by default), retries a failed registration after 123 s, and
  deletes its registration only on a sign-out-like stop (a normal stop leaves it to expire).
- The calling stack registers its own socket as `SkypeSpacesWeb` (`SkypeSpacesWeb_2.6`); denied, it retried every few
  seconds for the whole run.

## Deliveries

Chat events reach the **messaging stack's** socket as socket.io messages:

`3:::{"id":<number>,"method":"POST","url":"/v4/f/<endpoint id>/messaging","headers":{…},"body":"<json text>"}`

- Headers: `X-Trouter-Delivery-Control: async; ttl=900; flow=messaging; prio=normal` (typing adds
  `resilience=nostorenoloss`), `X-Microsoft-Skype-Message-ID` (a GUID per delivery), `trouter-request`
  (`{"id":"<guid>","src":"<address>","port":3443}`), `Trouter-Timeout` / `Trouter-TimeoutMs` (about 117 s), `MS-CV`,
  `traceparent`, `Content-Type: text/xml` (the body is JSON nonetheless), `Content-Length`, `Host`, `User-Agent`
  (`Skype-NotificationHub/…`), `X-IC3-Needle` (a server-side trace) and `__RequestTunnelId__`.
- The body is an `EventMessage`: `time`, `type`, `resourceLink` (`https://notifications.skype.net/v1/users/ME/
  conversations/<chat>/messages/<id>`), `resourceType`, `resource`, sometimes `mentiontypes`, and `isactive`.
- Resource types seen, in the order a's actions caused them:

| a's action | `resourceType` | `resource` |
|---|---|---|
| typing | `NewMessage` | a message of `messagetype` `Control/Typing`, empty content, `contenttype` `Application/Message` |
| the message | `NewMessage` | the message as the chat service stores it: `id`, `version`, `clientmessageid` (a's own), `content` (HTML), `messagetype` `RichText/Html`, `from` (a `contacts/8:orgid:<guid>` link), `imdisplayname`, `originalarrivaltime`, `composetime`, `sequenceId`, `properties` (`mentions`, `cards`, `links`, `files` as JSON text, `formatVariant`, `languageStamp`), `conversationLink`, `to` (the chat id), `threadtype` |
| (follows the message) | `ConversationUpdate` | the chat: `id`, `lastMessage` (the message again; its `mentions` as a JSON array this time, plus `ams_references`), `lastUpdatedMessageId`/`Version`, `properties` (`lastimreceivedtime`, `addedBy`, an AI reply suggestion), `threadProperties` (`uniquerosterthread`, `rosterVersion`, `lastSequenceId`, `threadType`, `tenantid`, …), `version`, `propertiesUpdated`, `memberProperties` |
| the edit | `MessageUpdate` | the same message id with a new `version`, the new `content` and `properties.edittime` |
| a reaction | `MessageUpdate` | the same message, a new `version`, and `properties.emotions`: the whole set, `[{"key":"like","users":[{"mri":"8:orgid:<guid>","time":<ms>,"value":"<ms>"}]}]` |
| a reaction removed | `MessageUpdate` | `emotions` with the key kept and its `users` empty |
| a delete | `MessageUpdate` | the same message id, a new `version`, empty `content`, `properties` only `deletetime`, `importance`, `subject` |
| a reply with a quote | `NewMessage` | `content` starting with the quote's `<blockquote itemscope itemtype="http://schema.skype.com/Reply" …>`, `properties.qtdMsgs` and `hasValidMsgReferences` |
| b reading the chat (to b's own endpoints) | `ConversationUpdate` | the chat, its `propertiesUpdated` `["consumptionhorizon"]` and `properties.consumptionhorizon` the new horizon (`<arrival id>;<ms>;<clientmessageid>`, `chat.md`) |

The reaction row was captured live in run 13 (below): a liked its edited message from the message's hover bar
(`message-actions-like`, the control `ab-chat-full` uses), its `PUT …/properties?name=emotions` was answered `200`
741 ms after it went out, and b's open socket got one live `MessageUpdate` 873 ms after it went out: the message
whole (its `properties` still holding `edittime`), `emotions` `[{"key":"like","users":[one user: `mri`, `time`,
`value`]}]`, the live delivery's headers as above (`X-Trouter-Delivery-Control: async; ttl=900; flow=messaging;
prio=normal`, `Trouter-Timeout`/`Trouter-TimeoutMs`, `__RequestTunnelId__`, no `mstore`). a's own socket got the same
event. Left unanswered, it wasn't delivered again on b's stable socket in the 16 minutes that followed. The removal,
delete and reply rows come from deliveries b's endpoints got as stored replays of an `ab-chat-full` run (Task 7, the
same a–b chat), seen again at run 13's connects. A reaction by b to a's message reaches b's own endpoints too: an
account's endpoints get its own
events.

The native client turns these into typed events (Task 16, `teams_trouter::events`): a `NewMessage` whose
`messagetype` is `Control/Typing` is a signal (typing), not a message, and so is every other `Control/…` one
(`Control/ClearTyping`, which the web client sends, is typing's end; one no capture showed is `Unknown`); a message's own `clientmessageid` matches it
to our outbox row (Task 13). A `MessageUpdate` carries the whole message, so what changed is told from it the way
every one in the captures of runs 12 and 13 (below; both accounts' sockets) could be told (read locally, the times
compared, none printed): an
edit's `edittime` equals its new `version` (6 of 6), a delete's `deletetime` does (3 of 3), and a reaction change
carries `emotions` with neither (14 of 14: an edited message keeps its old `edittime`). A `ConversationUpdate` whose
`propertiesUpdated` names `consumptionhorizon` is the account's own read horizon (4 of 4, the last row above, in
`deliveries.json` as the eighth delivery); any other one is the chat's update. Any other `resourceType` is kept as
`Unknown` by its name. No delivery of another member's read receipt and none of a new chat were seen, so the mock
doesn't deliver those (`teams_mock::TrouterMock`).

## Answering deliveries

The web client answers each chat delivery `3:::{"id":<the delivery's id>,"status":200,"headers":{"MS-CV":"<its
MS-CV>.0","trouter-request":"<echoed>","trouter-client":"{\"cd\":<ms spent>}"},"body":""}` and pings right after.
The messaging set denies every answer (unknown), so every run showed what an endpoint that never answers gets:

- **On a stable socket, nothing more.** Each live delivery came once; in 16 minutes of observation after the last one
  (two runs) none came again. The socket stayed up, every ping was answered within about 100 ms, and later deliveries
  kept coming promptly (an edit showed on b 1.2–1.3 s after a sent it, after earlier deliveries had gone unanswered).
- **On every new connection, a replay.** When an endpoint connects (and every other endpoint the account still has
  registered: registrations live an hour, and each web page load made a new one), the service delivers again, as
  stored deliveries, the account's non-typing chat events of the last 15 minutes or so that went unanswered: events
  delivered live to an open socket and left unanswered, and events from while the account was offline. A stored
  delivery's `trouter-request` gains `"mstore":true,"flow":"messaging","seq":<n>`, its `Trouter-Timeout` is `7500`,
  and it adds `Connection: keep-alive`; its `id` stays the same from one connect of an endpoint to the next, and each
  endpoint gets its own copy. One run's live, unanswered message, conversation update and edit were replayed at b's
  connects 9, 12.5 and 13.5 minutes later and not at 19 minutes, which fits `ttl=900`; typing
  (`resilience=nostorenoloss`) was never stored. The bursts seen held 3 to 14 events.
- **Under socket churn, retries.** In `ab-trouter-quiet` the web client reconnected about every 10 s (its own
  `user.activity` never got an acknowledgement, below). A live unanswered delivery was then sent again on each new
  socket with a fresh `id` for about 95–120 s, and then joined the stored set, replayed on every connect (36 times in
  about 6 minutes).
- No throttling, no socket closed by the service, no stop of deliveries, and no expected test event missing, in any
  run.

**Decision** (Claude and Codex, 2026-10-20; `discussions/task-14-unanswered-deliveries` in the plan run's state):
no operational harm was shown under these conditions. The cost of not answering is the bounded replay on reconnect,
which the native client absorbs by design: it syncs every chat after every reconnect anyway (Task 17) and stores by
id and version (Task 11), so a replay adds no new state. The answers therefore **stay unanswered** and no response
path is built (Task 15's `DeliveryResponse` is struck); M0's rule stands. What the native client must do instead
(Task 16): apply replays idempotently by conversation, message id and version (the transport's
`X-Microsoft-Skype-Message-ID` is at most a hint) so that neither a replay nor its meeting with the sync produces a
repeated user-visible effect; keep reconnects rare (one endpoint, no `user.activity` acknowledgement to wait for). How
newly found offline messages are notified is M2's.

## `user.activity` and presence

- The web client sends `5:<n>+::{"name":"user.activity","args":[{"state":"active","cv":"<cv>"}]}` right after
  `trouter.connected` (the bundle: again 10 s later, and on every change; only `active` and `inactive`), and the
  service acknowledges it. The bundle shows no idle timer in the Trouter layer: the chat layer reports active when it
  starts and inactive when it stops.
- `ab-trouter-quiet` held every `user.activity` back. The service still delivered live chat events to sockets that
  had never sent one. What changed was the web client: per the bundle it reconnects when its activity event isn't
  acknowledged within 10 s, so it reconnected about every 10 s for the whole run, with the replays above. That is the
  web client's own dependency, not the service's: a client that never sends the event has no acknowledgement to wait
  for.
- **Decision** (same discussion): the native client sends no `user.activity` in M1; it is not on the reviewed
  `OwnFrame` list (Task 15). Revisit only if a long-lived native session without it shows the service closing the
  socket or delivering less.
- Presence (for M2), not settled here: alongside `user.activity` the web client PUTs its presence endpoint,
  `PUT <teams>/ups/<geo>/v1/me/endpoints/` with `{"id":"<the Trouter epid>","availability":"Available","activity":
  "Available","activityReporting":"Transport","deviceType":"Web"}`, and subscribes to others' presence
  (`POST …/ups/<geo>/v1/pubsub/subscriptions/<epid>`). The fence denied both in every run (they are on the run's
  expected list), and other people's presence reads were denied too, so what makes an account show online (the PUT,
  the activity, or both) was not tested.

## Reconnect parameters and backoff (from the bundle)

- Backoff `1000 · 2^n` ms, ±20 % jitter, capped at 300 s for the messaging stack (30 s elsewhere); the count resets
  after a long quiet spell; a connect times out after 20 s.
- The service can disconnect with a reason (`{"reason":…,"claims":…}`): `unauthorized` (reconnect with a fresh token;
  `claims` is a CAE challenge), `skypetoken-deprecated` (stop), `dup` (reconnect at once).
- `trouter.reconnect` `{"target":"self"|"host","url":…}` asks the client to reconnect (make-before-break for `self`,
  to `url` for `host`). Not seen live.

## Token renewal on a live socket

- Not captured live: the web client's `ic3` token lives about 24 hours (CAE), so no run renews it on an open socket.
- Per the bundle the client **never re-authenticates an open socket**: `user.authenticate` is sent once per connect,
  and a renewed token is used at the next connect. An expired token surfaces as a service disconnect with reason
  `unauthorized`, after which the client reconnects with a fresh token.
- For the native client, whose `ic3` token lives about 2 hours: reconnect (a new socket, a new `user.authenticate`
  with the cached `connectparams`, a new registration) before the token expires, and treat an `unauthorized`
  disconnect the same way. Task 16 tests it with a fake clock on the mock; whether the service closes an open socket
  when its token expires is open (it may keep it, since it checked the token once).

## Live runs

All on 2026-10-20 (UTC), `proto-lab run ab-trouter[-quiet] --headless` from a normal build, accounts a and b, the
machine shared with other agents' builds (load average 15–30 for runs 1–9, 2–4 for runs 10–12, about 30 for run
13). Every write the fence allowed in these runs was on the a–b 1:1 chat (messages, typing, edits, reactions, read
receipts), both of whose members
are lab accounts; the probe was force-denied and failed in a's page in every run that got that far.

| run | start | outcome |
|---|---|---|
| 1 | 19:09 | a's browser killed to contain an iframe not armed in time (contained, no breach); failed at a's compose box; the calling stack's registration and presence denied as unexpected (now `TROUTER_RUN_REQUESTS`) |
| 2 | 19:23 | **breached**: b, an iframe whose resume got no answer within 10 s (resume sent); browser killed. Its capture and `fence.log` show nothing sent unguarded (b's 3 frames all allowed, no mutating request answered without a verdict) |
| 3 | 19:29 | failed at a's compose box (60 s; under load) |
| 4 | 19:40 | **breached**: a, two `browser_ui` targets (Chrome's own omnibox pages, not held at attach) whose auto-attach got no answer; b contained a worker. Nothing sent unguarded |
| 5 | 20:13 | message and edit delivered; b's check of the edit failed on a no-break space the editor put in; two unexpected deny classes (activity feed read state, agent invocation; now listed). No breach |
| 6 | 20:24 | failed at a's compose box (load ~30) |
| 7 | 20:28 | **breached**: b, an iframe whose resume got no answer within 10 s; browser killed. Capture and `fence.log`: nothing sent unguarded |
| 8 | 20:29 | a's browser killed to contain a worker (contained); a didn't load |
| 9 | 20:34 | message delivered; the edit's hover landed elsewhere; 16 min observed: no redelivery on the open socket |
| 10 | 20:54 | `ab-trouter-quiet`: message and edit delivered; `user.activity` denied 37 times; the web client reconnected about every 10 s; reaction control not found |
| 11 | 21:00 | message and edit delivered; the connect replayed run 10's unanswered events; 16 min observed: no redelivery |
| 12 | 21:20 | as run 11; the reaction's fallback (the "more options" menu) found no reaction control either |
| 13 | 22:14 | after the review fixes (the hover bar's `message-actions-like`; each of b's documents must get a registration answered `2xx`): **exit 0**, no breach. Message shown on b 12.7 s after a's Enter (under load), edit 1.6 s after it was sent, reaction from the hover bar answered `200` and delivered live to b 0.9 s after it went out; 16 min observed: no redelivery. Registrations: a 2 answered, 36 denied; b 2 answered, 39 denied (the calling stack's retries). `fence.log`: the only allowed writes were a message, typing, an edit and a reaction (2 permits each), all on the a–b chat, no participant added; the other allows were the two accounts' token refreshes, the messaging stack's registrations (8) and Trouter session frames (135); 25 forced denies (the probe and the client's resends of it), every other deny unclassified |

Runs 1–4 were made by an earlier agent on this task before it was cut off; their printed output was kept. Runs 1–12
didn't exit 0: each failed on a step of the web client's UI (a's compose box under load, the reaction control) or on
an arming timeout, never on an unexpected deny after run 5. Run 13, after the reaction step moved to the hover bar's
like, was the first to exit 0. The three breached runs count as breaches (the CLAUDE.md
rule: a resume sent, or a target not held, is a breach); the later clean runs are reported next to them, not in their
place.

## Open

- Whether the service takes our own non-CAE `ic3` token on Trouter and the registrar, and the native connect's
  query, `Origin` and registration as built (Task 16 tested them on the mock only; the first live run is Task 19's).
- What the service does when a token expires on an open socket.
- The v3 registrar (`registrarUrl`) the service hands over; the web client doesn't use it.
- Accepted delivery answers might reduce the reconnect replay; not tested under the current policy (every answer is
  denied). Revisit only if the replay measurably impairs the native client.
- Whether a long-lived native session without `user.activity` is treated differently (the sockets that never sent
  one here lived seconds to minutes).
- What makes an account show online ([presence](#useractivity-and-presence)).
