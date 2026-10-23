# Trouter: the push channel for messaging

Status: 2026-10-20, M1 Task 14; the native client's part (below) 2026-10-21, M1 Task 16; others' and own presence 2026-10-23, M2 Tasks 17–18. Written from live runs of `proto-lab run ab-trouter --headless` (accounts a and b,
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
  registrar `X-MS-Migration: True` and `X-Ms-Test-User: False` with the token. The push URL's endpoint id is
  base64url; until M1 Task 21 the registration refused one with a `-` or `_` (11 of the 28 ids in the captures), so such an
  endpoint went unregistered and got no deliveries.
- **Reconnect**: at once for a renewal, a missing pong, an `unauthorized` disconnect (with a new token) or a `dup`,
  but never twice within 10 s; otherwise after a backoff of `1 s · 2^n` ±20 %, capped at 300 s, whose count starts
  again after a connection stayed up a minute; a throttled token service's `Retry-After` is a floor on that wait,
  never a shortcut past it. `skypetoken-deprecated` stops the client.
- **Renewal**: an open socket is never authenticated again, so the client reconnects with a new token 5 minutes
  (the session's refresh margin) before the held one expires, checking the expiry against the wall clock every 30 s,
  and at once when the session announces another `ic3` token (told by its value, not its expiry). A renewal before
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
- The service takes **our** non-CAE `ic3` token on Trouter, as on the chat service (`auth.md` question 8): Task 16
  built the native client against the mock, and its first live socket, M1 Task 19's `teams-headless watch`
  (2026-10-21), authenticated, registered and got the delivery; so did every socket of Task 21's acceptance runs.

## `trouter.connected`: the endpoint

`5:1::{"name":"trouter.connected","args":[{…}]}` with:

| field | meaning |
|---|---|
| `id` | the endpoint (connection) id, 22 base64url characters (letters, digits, `-`, `_`); the last part of the push URLs |
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

- `id` is a signed 32-bit number: of the 2515 deliveries in the captures 267 had a negative one. Until M1 Task 21 the
  native client read it as unsigned and silently dropped those deliveries (3 to 18 of 50 messages in a live run); it
  reads it signed now.
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
(`Control/ClearTyping`, known from the bundle only, is typing's end when one is delivered: the web client sent none in
three `chat-shapes` runs, [chat.md](chat.md#typing-chat-shapes); one no capture showed is `Unknown`); a message's own `clientmessageid` matches it
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
  the activity, or both) was not tested. Others' presence is read since M2 Task 17 ([below](#others-presence)); the
  account's own was measured in M2 Task 18 ([Own presence](#own-presence-m2-task-18)): the endpoint `PUT` is what
  makes it show online, `user.activity` alone does nothing others see.

## Others' presence

M2 Task 17. Others' presence is read from UPS (GTM `presenceUPS`, `<AFD host>/ups/<geo>`) by `POST
v1/presence/getpresence/`, a read the web client sends as a `POST`.

- **The request** (5461 of them in the captures of 2026-10-19 to 2026-10-21, all denied then): a JSON list of
  `{"mri": …}`, up to 8 seen, the account's own sometimes with `"source": "ups"` beside it; no query; the trailing
  slash; `behavioroverride: redirectAs404`, `x-ms-client-type: cdlworker`, `x-ms-client-user-agent: Teams-V2-Web`, an
  empty `x-ms-object-id`, `Content-Type: application/json`, the client's caller and correlation ids, and on 2065 of
  them `x-ms-endpoint-id` (the client's endpoint). The client asks with a caller per component
  (`presence-sync-strategy:onInterval` about every four minutes, 218–258 s apart; others on a subscription event, on
  Trouter's state, for its own "me" queries). Its token: `https://presence.teams.microsoft.com/`, CAE.
- **The answer** (`ab-chat` of 2026-10-22, the first run that let lab-only reads through: 14 reads, all 200): one item
  per MRI asked and no other, each `mri`, `presence` (`sourceNetwork`: `Self` for the account's own, `SameEnterprise`
  for another; `calendarData.isOutOfOffice`; `capabilities`, empty; `availability`; `activity`; `lastActiveTime`; and
  `source` when asked with it), `etagMatch`, `etag` and the item's `status`, `20000`. Every lab account was
  `Offline`/`Offline` then (nothing sets their presence yet), `lastActiveTime` `0001-01-01T00:00:00Z`.
- **The browser fence** lets it through as the read-by-`POST` entry `presence` only in that shape, without
  `x-ms-endpoint-id`, and only when every MRI is a lab account ([fence-targets.md](fence-targets.md#the-presence-read)).
- **The native client** sends it as the sealed `PostRead` entry `PresenceRead`, by MRI alone (`[{"mri": …}]`, never
  `source`), with the captured header values and its own non-CAE presence token, which UPS took
  (`teams-headless a presence`, 2026-10-23: two reads, `Offline` for b and c; [auth.md](auth.md#open-questions)
  question 4). The engine polls it every minute for the people the views asked about, in one read of those the fence
  lets a read name (`teams-core` `presence.rs`): at most 20 watched, the one asked about longest ago dropped (and told
  unknown); someone newly asked about brings the read forward, never within 10 s of the last; a 429 or 503 holds every
  read until its `Retry-After`, counted from the answer, is over. The views show a dot, "unknown" before the first read,
  after a failed one and for someone no longer watched.
- **No subscription**: the web client also subscribes its endpoint to others' presence (`POST
  v1/pubsub/subscriptions/<epid>`), which changes UPS's state, so it is no read and stays denied; the native client's
  presence is only as fresh as its last poll. If a minute proves too stale, that is a backlog item, not a subscription
  under the read rules.

## Own presence (M2 Task 18)

What makes a lab account show online to another, and what a manual status sends, measured under the user's
exception of 2026-10-21 (CLAUDE.md: a lab account may set its own availability, in its captured shapes, availability
values only, every id it names the account itself or its own registered endpoint). The scenarios are
`crates/proto-lab/src/scenario/own_presence.rs` (`own-presence-activity`, `-endpoint`, `-both`, `-manual-held`,
`-manual`): only a's browser runs, under the messaging set ([above](#the-messaging-set-what-the-fence-lets-out)), and
opens the a–b chat (the probe force-denied first, as `ab-chat`). b's view is b's native client reading a's presence,
`teams-headless b presence` (the read of [Others' presence](#others-presence), under b's own fence), about every
16 s, from before a's browser starts until minutes after it closed (`scripts/own-presence-run.sh`, under the live
lock). The fence mapping is `crates/proto-lab/src/availability.rs`, behind `Classifier::with_own_availability` (the
endpoint `PUT` and the manual status: `own-presence-manual`) or `Classifier::with_own_endpoint_put` (the endpoint `PUT`
alone, the manual status denied: `-endpoint`, `-both`, `-manual-held`).

### What the web client sends

- **The endpoint `PUT`**: `PUT <teams origin>/ups/<geo>/v1/me/endpoints/`, no query, a body of exactly
  `{"id","availability","activity","activityReporting":"Transport","deviceType":"Web"}`. `id` is the client's Trouter
  endpoint id: the messaging stack's registration's `registrationId`, its socket's `epid`, and the request's
  `x-ms-endpoint-id`, a lower-case GUID. The pairs seen (403 in the captures of 2026-10-20 to 2026-10-21):
  `Available`/`Available` while the client runs (388) and `Busy`/`InACall` in a call (15, `echo-call`). Its callers
  (`x-ms-client-caller`): `presence-sync-strategy:TrouterStateChanged` (the push channel's state changed),
  `presence-sync-strategy-client` and the two `set-callstate-presence-worker-resolver:setCallStatePresence…`.
  Answered `201` with no body when let through. The web client sends it a few seconds after a Trouter connect, after
  its presence subscriptions (denied) were tried: 11–60 s after the page began in runs 1, 2, 4 and 5 below, and not
  at all in runs 3 (within the 79 s its page was open) and 6–10, the same client version (`1415/26100212011`)
  throughout; what decides it is not known.
- **The manual status** (the account menu's status list, captured with it denied in run 7): `PUT
  <teams origin>/ups/<geo>/v1/me/forceavailability/`, no query, the same headers as the endpoint `PUT` (its
  `x-ms-endpoint-id` the current messaging endpoint), the caller `TeamsClient:setPresenceAvailability` and a body of
  exactly `{"availability": <Busy | DoNotDisturb | Away | Available>}` (the menu's "Busy", "Do not disturb", "Appear
  away", "Available"; three tries each when denied). **"Reset status"** clears it with the same `PUT` and **no body
  at all** (no `postData`, no `hasPostData` in Chrome's event), the caller `TeamsClient:setPresenceAvailability-reset`,
  `Content-Type: application/json` still sent. Each was answered `200` with no body when let through (runs 8–10).
  Choosing "Available" by hand is a set like the others (it forces Available); only the reset clears. In runs 9 and
  10 the client sent nothing for the choices after Busy (Do not disturb, Away, Available) within 20 s of each, while
  run 8 sent all five; its own presence reads, denied (they carry `x-ms-endpoint-id`, [above](#others-presence)),
  may be why. Not driven, so not captured: "Be right back", "Appear offline", a duration
  (`set-availability-duration`), and a status note.
- **`user.activity`** (`active`) on each Trouter socket, as [above](#useractivity-and-presence); the browser fence's
  messaging set already allows it (the user's decision of 2026-10-19).
- When the account menu opens, the client also sends `PUT …/api/mt/part/<partition>/beta/me/engagementSurfaces/
  displayed` (which of its prompts it showed); it stays denied (the runs' `OWN_PRESENCE_REQUESTS`).

### What b saw

All `proto-lab run <scenario> --headless` from a normal lab build, under the live lock, 2026-10-22 23:31 to
2026-10-23 05:28 UTC, the machine otherwise idle. Every run's probe was force-denied and failed in a's page; the only
writes the fence let through were a's typing on the a–b chat (2 permits a run) and the own availability below; no
target failed to arm, **no run was breached**.

| run | start (UTC) | scenario | what reported a | b's reads of a |
|---|---|---|---|---|
| 1 | 23:31 | `own-presence-activity` (ok) | `user.activity` (2 sent); the endpoint `PUT` denied (3) | `Offline` throughout: before, during the 5 min the page was open, after |
| 2 | 23:48 | `own-presence-endpoint` (ok) | the endpoint `PUT` (2 answered `201`, two endpoints after a reconnect); every `user.activity` held back (the client reconnected about every 10 s, as in `ab-trouter-quiet`) | `Available` from 4 s after the `PUT` until the page closed; `Offline` 10–25 s after the browser closed |
| 3 | 00:09 | `own-presence-both` (failed: no endpoint `PUT` within the 60 s it then waited) | `user.activity` (2) | `Offline` throughout |
| 4 | 00:25 | `own-presence-both` (ok) | both (`user.activity` 2, the `PUT` 1 answered `201`) | `Available` from 7 s after the `PUT`; still `Available` 5 min 37 s after the browser closed, `Offline` by 5 min 55 s |
| 5 | 00:44 | `own-presence-manual-held` (failed: the status entries' `data-tid`s had changed, no status chosen) | both (the `PUT` answered `201`) | `Available`, then `Offline` about 5 min 50 s after the browser closed |
| 6 | 01:04 | `own-presence-manual-held` (failed: no endpoint `PUT` within 180 s) | `user.activity` | `Offline` throughout |
| 7 | 01:26 | `own-presence-manual-held` (failed on one unexpected deny, `engagementSurfaces/displayed`, since listed) | `user.activity`; no endpoint `PUT` sent; the five manual requests denied (15 with retries: the capture of their shape) | `Offline` throughout |
| 8 | 02:01 | `own-presence-manual` (its exit not kept) | `user.activity`; no endpoint `PUT` sent; the five manual requests let through, each answered `200` | not kept: the run's printed report and b's samples were in `/tmp`, lost in the machine's reboot of 2026-10-23; `fence.log` holds b's 92 reads, let through, without their answers |
| 9 | 05:04 | `own-presence-manual` (ok) | `user.activity`; no endpoint `PUT`; Busy let through (`200`), nothing sent for the next three choices, the reset let through (`200`) 6 min after Busy | `Offline` throughout, Busy included |
| 10 | 05:28 | `own-presence-manual` (ok) | as run 9 | `Offline` throughout, Busy included |

So:

- **The endpoint `PUT` makes an account show online; `user.activity` alone does not** (run 1). One `PUT` held it
  `Available` for the whole 5 minutes the page stayed open (runs 2, 4): no refresh was needed within that time.
- **Going offline** follows the browser's close: within 25 s when no `user.activity` was sent (run 2), about 6 minutes
  when it was (runs 4, 5). Run 2's socket also reconnected every 10 s, so which of the two (the frame or a steady
  socket) holds the endpoint open after its socket closes is not separated. No explicit "offline" request was seen:
  the web client sent none when its page closed.
- **A manual status shows nothing while the account has no live endpoint**: in runs 9 and 10 UPS took Busy (`200`)
  and, 6 minutes later, its reset, and b read `Offline` before, during and after. Whether a manual status overrides
  an endpoint's `Available` as b sees it (what the menu promises) was not observed: the web client sent no endpoint
  `PUT` in any run that let the manual status through. The native client sends its endpoint `PUT` itself (Task 19),
  so its first live runs settle it: Busy set beside a live endpoint, then the reset, each read by b.
- **A manual status is cleared** by the reset (the bodiless `PUT`, answered `200`); no other request clears it.
- For the native client (Tasks 19–20): the endpoint `PUT` is needed and enough to show online; `user.activity` isn't
  needed for it, and without it the account went offline within seconds of the client's end rather than minutes.

### The browser fence's mapping

`availability.rs`, asked only by a classifier built with `Classifier::with_own_availability` (both requests:
`own-presence-manual`) or `Classifier::with_own_endpoint_put` (the endpoint `PUT` alone: `own-presence-endpoint`,
`-both`, `-manual-held`, so the held run's manual status stays denied); every other run denies these requests as
before.

- `endpoint_put` and `manual_status` recognize exactly the shapes above: either Teams origin, `/ups/<lowercase
  geo>/v1/me/endpoints/` or `…/forceavailability/` with the trailing slash, no query, no fragment, no user info;
  only the captured header names, each pinned header present with its one captured value (`content-type`,
  `behavioroverride`, `x-ms-client-type`, `x-ms-client-user-agent`, an empty `x-ms-object-id` and `x-ms-request-id`),
  `Origin` the request's own, `x-ms-endpoint-id` present and a lower-case GUID, the caller one of the captured ones
  for that request; a body read strictly (case-folded duplicate keys make it unknown). The endpoint `PUT`'s `id` must
  equal its `x-ms-endpoint-id`, with one of the two captured pairs; a manual set only with its caller and one of the
  four values; a reset only with its caller and no body. A caller of one with the body of the other is unknown.
- Each is `Classification::OwnAvailability { endpoint }`; the session's HTTP fence (`HttpFence::decide`) makes it own
  state (`own_availability` in `fence.log`) only when that endpoint is one **this browser's fence let this account
  register in this run**: a messaging-stack registration (`calling::messaging_request`) whose verdict was `Allow`
  enrols its `registrationId` (`OwnEndpoints::enrol`, after the verdict: a registration the log refused enrols
  nothing). It says the fence let the registration out, not that the registrar took it. Each lab session has its own
  set, so b's endpoint never counts for a; the calling stack's registration never enrols.
- A reset is recognized only when Chrome's event says nothing of a body: `PausedRequest::body_unread` is set when
  `postData` isn't a string while any of `postData`, `hasPostData`, `postDataEntries` is there (of any value, `null`
  and `false` included), and then the request is unknown. An empty, blank, `{}` or `null` body is no reset.
- Anything else (another endpoint, another value, "Be right back", "Appear offline", a duration, a status note
  (`publishnote`), a work location, out-of-office, the presence subscriptions) stays unknown and denied. The design
  was settled with Codex (2026-10-23): enrolment only after the registration's `Allow`, every pinned header required,
  the callers pinned, the reset's body positively absent.

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

- ~~Whether the service takes our own non-CAE `ic3` token on Trouter and the registrar, and the native connect's
  query, `Origin` and registration as built.~~ **Answered (live, M1 Task 19, 2026-10-21): yes**, all of them as built;
  right after each connect the service sent three message-loss reports, each acknowledged after a catch-up that found
  nothing.
- What the service does when a token expires on an open socket.
- The v3 registrar (`registrarUrl`) the service hands over; the web client doesn't use it.
- Accepted delivery answers might reduce the reconnect replay; not tested under the current policy (every answer is
  denied). Revisit only if the replay measurably impairs the native client.
- Whether a long-lived native session without `user.activity` is treated differently (the sockets that never sent
  one here lived seconds to minutes).
- ~~What makes an account show online ([presence](#useractivity-and-presence)).~~ **Answered (live, M2 Task 18,
  2026-10-23)**: UPS's presence endpoint `PUT`; `user.activity` alone doesn't
  ([Own presence](#own-presence-m2-task-18)).
- Why a closed browser that had sent `user.activity` stayed online about 6 minutes, and one that hadn't only seconds
  ([Own presence](#own-presence-m2-task-18)); and why the web client sent no endpoint `PUT` at all in some runs.
