# Fence targets: what reaches people, and whose members the fence reads

Status: 2026-11-03, the lab rule: **lab accounts talk and call only each other, the echo service and the user's own
account (`lab.toml`'s `contacts`); apps and bots aren't people; everything else is allowed** ([lab-rules.md](../lab-rules.md)).
Rewritten for the earlier lab-only rule by Task 7 of `docs/plans/20261023-lab-fence-contacting-writes-only.md`. First written 2026-10-20 (M0 Task 11) from
static analysis of the web bundle `26260.1717.5229.7049` (imported with `proto-lab bundles import`, searched with
`proto-lab bundles search` and read in the chat service's request builders) and [auth.md](auth.md), then checked the
same day against live traffic, once the lab accounts had a Teams license: `proto-lab run load` (account a) and
`proto-lab run ab-chat` (a and b, the a–b 1:1 chat). [What the captures showed](#what-the-captures-showed) lists what
they confirmed and what they corrected. M1 Task 7's `ab-chat-full` (the same day) exercised the edit, the delete, the
reactions and a quoted reply ([chat.md](chat.md) has the shapes); on 2026-10-21, a mention. M1 Task 8 (2026-10-20)
read a group chat: [A group chat's reads](#a-group-chats-reads).

The code: the one classifier is `teams_fence::contacting` (`crates/teams-fence/src/contacting.rs`, the chat service's
rows in `contacting/chat.rs`, the call start in `contacting/call.rs`), tested row by row in
`crates/teams-fence/tests/contacting.rs` against the synthetic fixtures in `crates/teams-fence/tests/fixtures/contacting/`
and the chat service's in `tests/fixtures/chatsvc/`. The lab browser's fence asks it for every request it pauses
(`crates/proto-lab/src/classify.rs`), the native transport for every plain request it sends
(`crates/teams-proto/src/http.rs`, `Transport::send`). A mapped write's target is judged by `Fence::check`
(`crates/teams-fence/src/decision.rs`) against a fresh member read, `LiveMembershipSource`
(`crates/teams-fence/src/membership.rs`).

## The verdicts

`contacting` answers one of three:

- **`Contact::None`**: the request reaches nobody and is let through. Every request that only reads (`GET`, `HEAD` or
  `OPTIONS` with no body and no method override), every request to a host that is no contacting service, the
  carve-outs on one, and the middle tier's paths that its rule doesn't count as contacting. A mutating one is logged
  to `fence.log` as `allowed`, by its method, host and path shape (ids replaced by their kind: `<chat>`, `<message>`,
  `<mri>`, `<guid>`, `<id>`), never a query value, a header or a body; one that can't be logged isn't let through.
- **`Contact::Write(Target)`**: a mapped write, let through only with a permit from `Fence::check` for that target:
  an existing chat after a fresh, complete member read whose every member is a lab account (`lab.toml`'s MRIs), an
  approved service participant (the echo service), a contact (the user's own account) or an app or a bot (`28:`, no
  person), a new target (`Scope::New`: a chat's creation, the test call's
  start) by the participants it proposes, and any participant it adds (a mention, a member added) judged the same
  way. The decision is logged by `Fence::check`.
- **`Contact::Unreadable`**: a mutating request on a contacting service that no row maps or carves out: denied, and
  logged.

A request is **mutating** when its method isn't `GET`, `HEAD` or `OPTIONS` (in any case), when it carries a body
anyway, or when a method-override header (`X-HTTP-Method-Override`, `X-HTTP-Method`, `X-Method-Override`) names
another method. On a contacting service an overridden method is unreadable: the fence can't tell which method the
service acts on. A mutating request whose URL doesn't parse is unreadable.

`Fence::check` also takes the three kinds that reach nobody but stay in the native outbox, which orders writes on a
chat's read horizon and on presence: typing and read receipts, permitted on a chat by its id (1:1, group or meeting)
without a member read, and the account's own availability on `Scope::Account`, permitted without a member read (it reaches
nobody, unless it adds a person outside the lab). The browser never needs them: there, typing and read receipts
are carve-outs and presence is no contacting service.

## The contacting services

On these services a mutating request is contacting unless a row maps it or carves it out by name. Hosts are matched in
lower case on a dot boundary (the host or any host under it), a trailing dot dropped, any scheme and port.

| Service | Hosts | What isn't `Unreadable` there |
|---|---|---|
| Chat service | a Teams origin's `/api/chatsvc/…`; `*.msg.teams.microsoft.com` | [its rows](#the-chat-services-rows) |
| Calling | `*.flightproxy.teams.microsoft.com` (whose `ep/<host>/…` forms reach the hosts behind it), `api.fl.teams.microsoft.com`, every `*.skype.com` host but AMS's (`*.conv.skype.com`, `*.cc.skype.com`, `*.broker.skype.com` and any other: a Skype host whose purpose is unknown counts as contacting) | the web client's test call start in its captured shape (`CALL_START_URL`, no query), `Write(Call)` to `New` with the caller and the echo service, only when the policy names the echo callee (`Policy::with_echo_callee`; the browser's `Classifier::with_test_call_callee`): [calling-echo.md](calling-echo.md); a chat header's call start in its captured shape, `Write(Call)` to the chat it carries (`Conversation`) with the caller and every callee: [calling.md](calling.md#the-start); the captured operations on a call (`updateEndpointMetadata`, `add`, `leave`, `acknowledge`, `applyChannelParameters`), each in its captured method, path and body, a write on `Scope::CallOperation` that [the call registry](#the-call-registry) resolves |
| Graph | `*.graph.microsoft.com`, every version, `$batch` included | nothing |
| Outlook, OWA and Substrate | `*.outlook.office.com`, `*.outlook.office365.com`, `*.outlook.cloud.microsoft`, `*.outlook.live.com`, `*.substrate.office.com` | `POST /search/api/<version>/suggestions` and `…/query`, and `POST /searchservice/api/<version>/query` on Substrate: search reads ([search.md](search.md)); `POST /search/api/<version>/events` (with or without its trailing slash), the search's own telemetry; `POST /owa/startupdata.ashx` on `outlook.office.com`, the embedded calendar app's startup read |
| SharePoint and OneDrive | `*.sharepoint.com`, `*.sharepoint-df.com`, `*.microsoftpersonalcontent.com`, `*.onedrive.com`, `onedrive.live.com`, the media hosts `*.svc.ms` | the file browser's prewarm, `POST /personal/<user>/_layouts/15/filebrowser.aspx` on a host one label under `sharepoint.com` (the account's own token to its own personal site) |
| AMS and the async gateway | `*.asyncgw.teams.microsoft.com`, `*.asm.skype.com`, a Teams origin's `/asm/…` and `/ams/…` | the token exchanges `POST …/v1/skypetokenauth` and `POST …/v1/<id>/aadtokenauth`, which trade the account's own token for the media service's cookie. An object's creation, its content upload and its `forward` stay unreadable: the fence can't tell whom an object reaches ([chat.md](chat.md#the-object-created-once-ab-image-object)) |
| Middle tier and apps | a Teams origin's `/api/mt/…` and `/api/apps/…` | a new standard channel and a channel shown to everyone, writes on the team (`contacting/channel.rs`, [channels.md](channels.md#creating-a-channel)); a team managed, writes on the team (`contacting/team.rs`, [teams.md](teams.md)); everything but a request with a segment (in any case) `team(s)`, `chat(s)`, `channel(s)`, `conversation(s)`, `thread(s)`, `group(s)`, a segment of its Outlook REST proxy (`v2.0`, `event(s)`, `message(s)`), or a segment holding `meeting`, `invite`, `calendar` or `mail`; of those, `POST …/beta/teams/apps/aggregatedEntitlements` (the app catalogue of the account's teams, a read) is carved out. Out-of-office with its automatic replies (`…/automaticRepliesSetting`) is the account's own settings. The calendar's rows (`contacting/calendar.rs`, [calendar.md](calendar.md#what-the-fence-makes-of-them)): an event created (`Write(Meeting)` on `New`, the organizer and invitees by address), changed, cancelled or deleted (`Write(Meeting)` on `Event`, whose organizer and attendees the fence reads fresh from the middle tier, by address, against `lab.toml`'s UPNs); a channel's meeting created, changed or cancelled on its team's group calendar (`v2.0/groups/smtp:<team mailbox>/…`), `Write(Meeting)` on `TeamCalendar`, judged by the channel's team whose root thread names that mailbox ([calling.md](calling.md#a-channels-meeting)); a Teams meeting's online meeting made without a chat, and the scheduling assistant's free/busy reads, reach nobody |
| CSA | a Teams origin's `/api/csa/…`, `*.chatsvcagg.teams.microsoft.com` | `POST …/api/<version>/teams/users/me/chatsAndChannelsByIds`, a read |
| Provisioning | a Teams origin's `/fabric/…`, `*.templates.teams.microsoft.com` | a new private channel and its people, writes on its team and its channel, and a group's warm-up, which names nobody: [channels.md](channels.md#creating-a-channel) |

A Teams origin is `teams.microsoft.com`, `teams.cloud.microsoft`, `teams.live.com` and the hosts under them. On one,
the service is told by the path's first two segments, each percent-decoded on its own and compared in any case; a
later segment that doesn't decode leaves the service to its row, which refuses it. A mutating request whose first two
segments can't be read (one empty with more path after it, as `//api/chatsvc/…` or `/api//chatsvc/…`, which a server
merging slashes would read as the service; not UTF-8 once decoded; or holding `/ \ ; ? #` once decoded, as
`/api/chatsvc%2Femea/…`) is unreadable.

The middle tier's settings and profile lookups are requests on a contacting service that its rule doesn't count as
contacting (no people segment), and Substrate's search reads are a carve-out on Outlook's: both `Contact::None`.
**Everything else** is no contacting service, and every request to it is `Contact::None`: telemetry and logging,
presence (`/ups/…`, the endpoint `PUT` and the manual status included), the registrar, Entra and authz, the push
service `/api/tps/…` (whose `pushupdates` `POST` is the account's own subscription), Trouter's HTTP side, the page and
its static hosts. [endpoints.md](endpoints.md) lists every endpoint seen with its verdict.

A WebSocket's URL is read the same way (`teams_fence::on_contacting_service`, any scheme): the lab browser denies
every frame on a contacting service's socket, and on a Trouter socket judges each frame by its socket.io type (`0`,
`1`, `2`, `3` and `5` pass, `3` the answer to a delivery, a call's receipts and acknowledgements among them; `4`, `6`,
a binary frame and anything that doesn't parse are denied: [trouter.md](trouter.md#answering-deliveries),
[the lab's call rule](#the-labs-call-rule)). Frames on any other socket pass.

## The chat service

Chats live in the chat service ("chatsvc", the IC3 messaging service). Its base URL comes from authz's region GTMs:
`chatServiceAfd`, which prod prefers, or `chatService` (see auth.md). The web client recognizes the Front Door form by
host: `teams.microsoft.com`, `teams.cloud.microsoft` (commercial), `teams.live.com` (consumer), `gov.teams.microsoft.us`
and `dod.teams.microsoft.us`, with the path `/api/chatsvc/<region>/`. Every request below is relative to that base and
starts with `v1/` (endpoint registration also with `v2/`). Conversation and thread ids are percent-encoded as one path
segment.

For the lab tenant the client uses the Front Door form with the region `amer`, on whichever Teams origin the page is
on (`teams.cloud.microsoft` after the client moves there, `teams.microsoft.com` after a deep link reloads it). It sent
no request to a `.msg.teams.microsoft.com` host. That host form showed up only as data: authz's `chatService` GTM
(`https://amer.ng.msg.teams.microsoft.com`, auth.md's Region GTMs), and the `Location` header of the 201 answers to
`POST v1/threads` and to message posts, which names the new resource on that host with a `/v1/…` path.

Credential: the web client sends the chat service an **Entra bearer token**, `Authorization: Bearer <token>` whose
audience is `https://ic3.teams.office.com` (scope `Teams.AccessAsUser.All`), on every chat service request, and no
skypetoken there. The bundle also names `Authentication: skypetoken=<skypetoken>`, which the captures never showed
(auth.md's service table); the fence's live read accepts either and sends back the form the page used. No
`RegistrationToken` was sent: the client registered no endpoint.

Where a mapped write is read from (the mapping and the live member read share one check,
`teams_fence::chat_service_path`): `https` only, no port, no user info, and either an AFD host of the commercial cloud
(`teams.microsoft.com`, `teams.cloud.microsoft`) with `/api/chatsvc/<region>/v1/…`, the region 1–32 lowercase letters,
digits or `-`, or a host ending in `.msg.teams.microsoft.com` with `/v1/…`, each label of its name before that suffix
non-empty letters, digits or `-`. The `.msg` host form is the regional chat host: the bundle doesn't name it (the GTM
value comes at runtime), but the captures did, as the `chatService` GTM and in `Location` answers (above), with `/v1/…`
straight under the host. The web client never sent a request there, so the fence has only ever judged the AFD form
live. A mutating request to the chat service in any other form (another version, `teams.live.com`, a path the check
doesn't read) is unreadable unless it is a carve-out.

### Ids

| Id | What |
|---|---|
| `19:<oid>_<oid>@unq.gbl.spaces` | a 1:1 chat; its id names both members' Entra object ids, lowercase and sorted |
| `19:<id>@thread.v2` | a group chat; `19:meeting_<id>@thread.v2` is a meeting's chat |
| `19:<id>@thread.tacv2`, `19:<id>@thread.skype` | a team's channel (the team's General channel has the team's id) |
| `<channel id>;messageid=<root id>` | a reply chain in a channel (the client joins the root message id to the conversation id) |
| `48:notes`, `48:…` | the user's own streams (notes, notifications, mentions, call logs) |
| `8:orgid:<oid>` | a work account's MRI; `28:<id>` a bot |

A **chat by its id**, where typing, read receipts and `Fence::check`'s signals ask for one (`teams_fence::is_chat_id`),
is a 1:1 chat's id naming two different object ids, or a group or meeting chat's `19:<id>@thread.v2` whose `<id>` is
1–128 ASCII letters, digits, `_` or `-`.

## The chat service's rows

On the chat service a mutating request is one of these; anything else is `Unreadable`. `ME` is `ME` in any case or the
account's own MRI (`Policy::with_own_account`). Any path segment that is empty or holds `/ ; ? #` once decoded, and any
query key twice in any case, makes the request unreadable.

| Request | Verdict |
|---|---|
| `POST v1/users/ME/conversations/<chat>/messages`, `messagetype` `Text`, `RichText`, `RichText/…` | `Write(Message)` on the chat, adding whoever `properties.mentions` names (a mention of everyone in the group chat itself adds nobody) |
| same path, `messagetype` `Control/Typing` or `Control/ClearTyping`, on a chat by its id, the body only `content` (empty or absent), `contenttype` and `messagetype`; a typing body in any other form or on any other id is unreadable | `None` (typing) |
| `PUT v1/users/ME/conversations/<chat>/messages/<msg>` | `Write(Edit)`, adding whoever `properties.mentions` names (a mention of everyone in the group chat itself adds nobody) |
| `DELETE v1/users/ME/conversations/<chat>/messages/<msg>` | `Write(Delete)` |
| `PUT`/`DELETE …/messages/<msg>/properties?name=emotions` (the only query key) | `Write(Reaction)` |
| `PUT v1/users/ME/conversations/<channel>/messages/<post>/properties?name=meeting` (the only query key), `<channel>` a team's thread, `<post>` digits, the body exactly `{"meeting": {"meetingtitle": <string>}}` (a channel's Meet now titled) | `Write(Edit)` on `Team(<channel>)` |
| `PUT v1/users/ME/conversations/<chat>/properties?name=consumptionhorizon` (the only query key), on a chat by its id | `None` (read receipt) |
| the same on the account's own streams, `48:notifications`, `48:mentions` and `48:calllogs` (the call log's horizon moves when a call ends) | `None` |
| `PUT v1/users/ME/conversations/<id>/properties?name=<n>`, `<n>` one of `OWN_VIEW_PROPERTIES` (`lastTimeFavorited`, `favorite`, `isGeneralChannelFavorite`), the only query key, or `name=favorite&requirePermanentJoinedState=<boolean>` exactly (the web client on each channel it made), `<id>` a `19:` chat or channel id | `None` (the account's own view of a conversation) |
| `PUT v1/users/ME/conversations/48:notifications` or `48:mentions` `/messages/<id>/properties?name=isread`, the only query key, the body exactly `{"isread": <string>}` | `None` (the account's own feed) |
| `PUT v1/users/ME/properties?name=<n>`, the only query key | `None` (the account's own settings) |
| `PUT`, `POST`, `DELETE` on `v1` or `v2` `users/ME/endpoints`, `…/endpoints/<id>`, `…/endpoints/<id>/active`, `…/endpoints/<id>/activeendpoint`, `users/ME/activeendpoint` | `None` (the account's own session) |
| `POST v1/threads` | `Write(ChatCreate)` to `New`, adding every member in the body |
| `PUT v1/threads/<chat>/members` | `Write(MemberChange)`, adding the body's members |
| `PUT v1/threads/<chat>/members/<mri>` | `Write(MemberChange)`, adding `<mri>` |
| `DELETE v1/threads/<chat>/members/<mri>` | `Write(MemberChange)` |
| anything else: other `48:` streams and their paths (notes, `sharedcalllogs`), forwarding and moving, live and targeted messages, drafts and scheduled sends, `PUT`/`DELETE` on a conversation itself, an `8:…` or `28:…` conversation, bot invokes (`v1/agents/…`), channel and team ids, reply chains, thread properties, `members/ALL` | `Unreadable` |

A mapped write's `<chat>` is a chat: `19:…@unq.gbl.spaces` or `19:…@thread.v2`, never a channel or team
(`@thread.tacv2`, `@thread.skype`) or a reply chain (`;messageid=`). That is looser than a chat by its id on purpose:
a mapped write is judged by its chat's live member read, which refuses an id it doesn't know; the carve-outs, which
skip that read, take only a chat by its id.

## Writes

The mapped writes as the web client sends them (all relative to the base):

| Request | Body | WriteKind | Target |
|---|---|---|---|
| `POST v1/users/ME/conversations/<chat>/messages` | `messagetype` `Text`, `RichText`, `RichText/…` | `Message` | `Conversation(<chat>)`, adding whoever `properties.mentions` names (a mention of everyone in the group chat itself adds nobody) |
| `PUT v1/users/ME/conversations/<chat>/messages/<msg>` | the whole message again, a message `messagetype` | `Edit` | `Conversation(<chat>)`, adding whoever `properties.mentions` names (a mention of everyone in the group chat itself adds nobody) |
| `DELETE v1/users/ME/conversations/<chat>/messages/<msg>?behavior=softDelete` | none | `Delete` | `Conversation(<chat>)` |
| `PUT`/`DELETE v1/users/ME/conversations/<chat>/messages/<msg>/properties?name=emotions` | `{"emotions": {"key", "value"}}` / `{"emotions": {"key"}}` | `Reaction` | `Conversation(<chat>)` |
| `PUT v1/users/ME/conversations/<channel>/messages/<post>/properties?name=meeting` | `{"meeting": {"meetingtitle": <subject>}}` | `Edit` | `Team(<channel>)` |
| `POST v1/threads` | `{members: [{id, role}], properties}` | `ChatCreate` | `New`, adding every member in the body |
| `PUT v1/threads/<chat>/members` | `{members: [{id, role}]}` | `MemberChange` | `Conversation(<chat>)`, adding the body's members |
| `PUT v1/threads/<chat>/members/<mri>` | the member's role or property | `MemberChange` | `Conversation(<chat>)`, adding `<mri>` |
| `DELETE v1/threads/<chat>/members/<mri>` | none | `MemberChange` | `Conversation(<chat>)` |

The native client's own writes of the same requests carry their kind themselves (sealed `Mutating` types in
`teams-proto`), with `Typing` and `ReadReceipt` beside them: [the verdicts](#the-verdicts).

A message post's body is the message: `content`, `messagetype`, `contenttype`, `clientmessageid`, `imdisplayname`,
`properties`. A reply in a reply chain posts to the chain's conversation id (`…;messageid=<root>`).

Seen live (the web client of 2026-10-20, the a–b 1:1 chat: `ab-chat`, and `ab-chat-full` for the edit, the delete,
the reactions and the quoted reply; the rows not listed here weren't exercised). The bodies and the answers in detail,
and how each change reads back, are in [chat.md](chat.md):

- **New 1:1 chat**: the first send in a chat that doesn't exist yet is preceded by `POST v1/threads` with
  `{"members": [{"id": "8:orgid:<oid>", "role": "Admin"}, …both…], "properties": {"threadType": "chat",
  "fixedRoster": true, "uniquerosterthread": true}}`. It answers 201 with `{"threadResource": {…the thread…}}`; the
  thread's id is the `19:<oid>_<oid>@unq.gbl.spaces` the two object ids make. The fence judged it as `ChatCreate` with
  both lab accounts added, and allowed it.
- **New group chat** (M1 Task 8): opened by the deep link `l/chat/0/0?users=<mri>,<mri>`, the first send is preceded
  by `POST v1/threads` with `{"members": [{"id": "8:orgid:<oid>", "role": "Admin"}, …all three…], "properties":
  {"threadType": "chat"}}` (no `fixedRoster`, no `uniquerosterthread`), answering 201; then the message post to the new
  `19:<hex>@thread.v2`. The fence judged the create as `ChatCreate` with the three lab accounts added, and allowed it;
  the message was force-denied by the scenario (see [How a run uses it](#how-a-run-uses-it)).
- **Message**: `POST v1/users/ME/conversations/<chat>/messages`, `messagetype` `RichText/Html`, `contenttype` `Text`,
  the HTML in `content`, plus `id` `-1`, `type` `Message`, `conversationid`, `conversationLink`, `from`,
  `fromUserId`, `composetime`, `originalarrivaltime`, `clientmessageid`, `imdisplayname`, `callId`, `state`,
  `version`, `amsreferences`, `properties` (`importance`, `subject`, `cards`, `links`, `mentions`, `files`, …) and
  `crossPostChannels`. It answers 201 with `OriginalArrivalTime`. A send that failed (the fence's forced deny) stays in
  the client as unsent and is **sent again on a later load**, when the run that denied it is long over.
- **Typing**: the same post with `{"content": "", "contenttype": "Application/Message", "messagetype":
  "Control/Typing"}`.
- **Read receipt**: `PUT v1/users/ME/conversations/<chat>/properties?name=consumptionhorizon` with
  `{"consumptionhorizon": "<horizon>"}`, answering 200.
- **Edit**: `PUT …/messages/<id>` with the whole message again (its `id`, the new `content`, `version` an ISO time,
  `properties.edittime`), answering 200 with an empty body.
- **Delete**: `DELETE …/messages/<id>?behavior=softDelete`, no body, answering 200.
- **Reaction**: `PUT …/messages/<id>/properties?name=emotions` with `{"emotions": {"key": "like", "value": <ms>}}`, and
  taken back with `DELETE` on the same URL and `{"emotions": {"key": "like"}}`; both answer 200.
- **Quoted reply**: an ordinary message post (`RichText/Html`) whose content quotes the message and whose
  `properties.qtdMsgs` names it. A 1:1 chat has no reply chain.
- **Mention** (2026-10-21): an ordinary message post whose content holds the mention span and whose
  `properties.mentions` is a JSON string of a list, one entry per mention with its `mri` (`8:orgid:<oid>`),
  `mentionType` `person`, `displayName`, `itemid` and `@type`. The mapping added the mentioned lab account to the
  message's target, and the fence allowed it after the live member read.
- **Mention of everyone** (2026-10-29, `abc-everyone`, held back): the same post with an entry of `mentionType`
  `everyone` whose `mri` is the group chat's own id. The mapping adds nobody for it (the chat's members are its reach,
  read fresh for the write); one naming another chat, or posted to a chat that isn't a group chat, is unreadable.

Rules that keep the mapping fail-closed:

- Bodies are read by a strict JSON reader: one JSON value, and no object with two keys alike in any case (which a
  service binding keys in any case might read either way); anything else makes the request unreadable.
- The kind of a message post comes from its body. A body the browser didn't inline (the browser fence reads it as
  empty), one that isn't JSON, or a `messagetype` not in the table (`Control/…` other than typing, `Event/…`, …) is
  unreadable. An edit needs its body too, with a message `messagetype`.
- Whoever a message or an edit mentions is an added participant of its target, so the fence judges them as it judges
  the chat's members, and a mention of an outsider is denied. The mentions are `properties.mentions`, a JSON string of
  a list whose entries each name an `mri` (the form the mention of 2026-10-21 showed). No `properties` or no
  `mentions` is no mention; `properties` that aren't an object, `mentions` in any other form, and either key in
  another case (`Properties`, `Mentions`) are unreadable.
- A message or an edit maps to its chat alone only in the forms the captures showed of the fields that could take it
  further: `crossPostChannels` (a post into channels too) left out or `[]`, and `properties.onbehalfof` (a message
  sent for someone else) left out or `null`. Any other value of either, or either key in another case, is unreadable.
- A new chat's participants are exactly the body's `members`, a non-empty list of objects each with a string `id`; a
  body without them is unreadable. The fence judges a `New` target by those participants without a read.
- Only chats map here: channels (`@thread.tacv2`, `@thread.skype`) and reply chains (`;messageid=`) are unreadable,
  because no channel post, reply or reaction is mapped from a capture yet (a channel's members are its team's, which
  the fence reads from the team's thread: [teams.md](teams.md#the-fence)). The account's own view of a channel
  (`lastTimeFavorited`) is a carve-out; its own member entry in a team channel (`PUT
  v1/threads/<channel>/members/<own MRI>?returnThreadMetadata`, `{"expiration": <n>}`, sent as the client lists the
  account's teams) and other channel properties stay unreadable: the writing scenarios' expected-deny list names
  them, so a run isn't failed by them.
- Bot invokes (`POST v1/agents/<bot mri>/invoke`, sent whenever an editor opens, `composeExtension/fetchCommands` to
  two message extensions' bots) stay unreadable: their body hands the account's Entra token to the bot's own service
  (`docs/backlog/bot-invokes-and-message-extensions.md`).
- Media objects (AMS) aren't mapped (M1 Task 20, decided with Codex): an object's creation names its readers
  (`permissions`, one key: the chat), but its upload (`PUT v1/objects/<id>/content/<kind>`) names only the object,
  and binding it to that chat would take a registry of what the fence permitted or an authoritative read of the
  object's current readers, which no capture has shown and which `forward` could widen. So images aren't sent
  (`docs/backlog/ams-image-sending.md`, [chat.md](chat.md)).
- Calls map the test call's start, as a new call of its caller and the echo service; a chat header's call start, as
  a call in the chat it carries with the caller and its callees ([calling.md](calling.md#the-start)); and the captured
  operations on a call, each in its captured shape: a join, an answer, a decline, an `add`, the follow-ups and the
  callee's ring-time sends, judged by the lab's call rule ([below](#the-labs-call-rule)); any other write on a call
  stays unreadable ([calling-echo.md](calling-echo.md), [calling.md](calling.md)).
  The calendar's event writes (a creation, a change, a cancellation, a delete) are mapped as meetings, judged by
  whom the event reaches ([calendar.md](calendar.md#what-the-fence-makes-of-them)); invites, mail and channel posts
  aren't mapped.

## The lab's call rule

Every lab fence (`Fence::open_lab`, `Fence::open_lab_account`, `Fence::new` in tests) judges a call by its entry
(`crates/teams-fence/src/decision.rs`, `CallRule::Entry`; [lab-rules.md](../lab-rules.md)), the
user's decision after the registry's link binding kept failing the lab's own calls (a retried start shadowed by a stale
pending one, acknowledgements and receipts denied on a live call):

| Operation | Judged by |
|---|---|
| a start (`epconv`) | its callees and the chat it carries, read fresh ([above](#writes)), as before |
| a meeting's start (`epconv`) or its pre-join's subscription | the sender and the meeting's chat it names, read fresh: it must list the sender, and only the lab's people (lab accounts, the echo service, the contact; apps and bots aren't people) ([calling.md](calling.md#joining-a-meeting)) |
| a channel's Meet now (`epconv`, its subscription or its start with media) | the sender and the channel's team, read fresh from the team's thread (the team's first channel's thread is the team's own; another standard channel's thread, `threadType` `topic`, names its team's root in `spaceId`, which is read in turn and must be a team's root naming the channel's `groupId`), apps and bots not counted ([calling.md](calling.md#a-channels-meeting)) |
| a join (`POST …/conv/<id>`, roster-only or with a call invitation) | the group chat it names (`joins`), read fresh: complete, listing the joiner, only the lab's people (lab accounts, the echo service, the contact; apps and bots aren't people); no invitation need bind the conversation to it. A meeting's entry from its pre-join (its preheat too) is a join of the meeting's chat, judged the same ([calling.md](calling.md#joining-a-meeting)); a channel's meeting's entry is a join of the channel, judged by its team's members as its Meet now is ([calling.md](calling.md#a-channels-meeting)) |
| an answer (`accept`) or a decline (`reject` 603) | an invitation this fence offered (below), tied to it by the call leg its link names (`…/cc/v1/incoming/<call>/…`, the call the invitation's `attach` link names: `CallRegistry::resolve_answer`), the answer from its callee, while it is open for the operation (ringing; the same `accept` again once answered, the same `reject` once declined), its group chat read fresh; no attach answer need have bound the link |
| an `add` | its sender and every invitee the lab's people (the row maps it only naming the 1:1 chat of the sender and its one invitee, and the sender as organizer) |
| a meeting lobby's admit (`…/conv/<id>/admit`) or Deny (`…/removeParticipant`) | as an `add`: its sender and everyone it names the lab's people ([calling.md](calling.md#the-lobby)) |
| every other operation: the follow-ups (endpoint metadata and state, participant properties, a conversation's notification links, the acceptance's acknowledgement, channel parameters, renegotiation, media answers and acknowledgements, media descriptions, the keep-alive), the callee's `attach` and `progress`, `leave` | permitted with no member read and no lookup, logged (`call_follow_up`, `call_ringing`) |
| a Trouter delivery response | the browser's frame rule lets every `3:::` through on a Trouter socket, logged `allowed`; the native client's typed responses are permitted by `Fence::check_delivery_response` when they name a delivery id, logged (`delivery_response`, by the kind the registry knows them as, else `call_delivery_response`) |

The browser fence reads no call answer at the response stage and registers or binds nothing. The registry still reads
every delivery the account's calling socket receives (`Fence::observe_delivery`): it offers the account's own
invitations whose caller isn't a person outside the lab (only an offered one is answered or declined), ends an invitation's ring at the callbacks
its `attach` handed out, and, for the native client, registers its own starts from their answers and binds their
acceptances, which is how its call machine reads its deliveries (`Observed::Acceptance`, `Observed::Renegotiation`).
None of that decides a lab write. The native transport refuses a call write the registry won't pend only on a fence
that tracks calls (`Fence::tracks_calls`): a lab fence's `attach` or `accept` goes with its answer unread.

## The call registry

The registry's own rule (`CallRule::Registry`, `Fence::tracks_calls`) is no build's fence's: its tests run it on a lab
roster through `Fence::tracking_calls` (`test-support` only) to check the registry's resolution as verdicts, the
resolution the lab's call rule and the personal fence bind a call's operations by when they move the registry. The
personal fence (`crates/teams-fence/src/decision/audit.rs`, `docs/personal-build.md`) denies no call write: it offers
any invitation addressed to the account (one at a time) and keeps the registry's bookkeeping, and the call floor
keeps one call at a time across the app's accounts. What follows is the registry's rule as it was designed: a call's follow-ups reach its participants, and no source reads a call's members (the conversation's
`getAllParticipants` answered an empty list for a call that had its caller in it:
[calling-echo.md](calling-echo.md#open)).
So the fence trusts only the calls it permitted itself ([lab-rules.md](../lab-rules.md)): each fence
holds a call registry (`crates/teams-fence/src/calls.rs`), and a follow-up passes only as a captured operation sent to
exactly a link the call's own services handed out for that operation. Decided with Codex (M3 Task 3, transcript under
`~/.local/state/plan-exec-revmux/20261024-native-teams-m3-calls/discussions/task-3-call-registry/`).

**The starts it registers.** Two captured starts map (`crates/teams-fence/src/contacting/call.rs`): the echo test
call's, a new call (`Scope::New`) of the caller and the echo service; and a chat header's call (`chat_call_start`, M3
Task 5, [calling.md](calling.md#the-start)), `WriteKind::Call` on the `Scope::Conversation` of the chat it is tied to
by what it carries, the caller and every callee it names added. The chat is the group chat `groupChat.threadId` names
(a group chat's id), or, for a start whose `groupChat` is `null` and that names one callee, the 1:1 chat of the caller
and that callee by `one_to_one_id`; never one taken from the client's context. So `Fence::check` reads that chat's
members fresh and permits the start only when they and every callee are the lab's people (lab accounts, the echo service, the
contact) and so is its caller (a lab-only chat with one more
lab member than the start calls passes; a callee outside the lab is denied, however lab-only the chat). The callees
must be work accounts (`8:orgid:<guid>`), at least one, none twice, none the caller, and `groupChat` must be there.
Decided with Codex (M3 Task 5, transcript under
`~/.local/state/plan-exec-revmux/20261024-native-teams-m3-calls/discussions/task-5-start-target/`): the plan's
`Scope::Call` names a call no source reads; the chat is what the rule reads, so the start lands on the conversation.
`Fence::pend_start` re-reads the body with the row that matches the permit's scope and needs exactly the permit's
target back.

**The bindings**, each only as the echo call's capture shows it (a chat's call binds the same way: M3 Task 5's runs
registered it from its answer and bound a voicemail's acceptance, [calling.md](calling.md#the-rings-ends)):

| Step | What binds | How |
|---|---|---|
| the start | a pending call: its caller (`participants.from.id`), its participants (the permit's), the endpoint's own callbacks (`callInvitation.links`, `conversationRequest.links`) | `Fence::pend_start(&permit, start body)`: the permit must be this fence's (it carries the fence's run-unique id) and is taken once (by its serial); the body must read as the start the permit was issued for |
| the start's answer | the call is registered: of the answer's `links`, `updateEndpointMetadata`, `leave` and `addParticipantAndModality` (the `/add` the client posts to, not `addParticipant`), each only when it is `conversationController`'s path plus the operation's segment, with its query | `Fence::register_start(pending, answer)`: the native transport reads the answer of the start it sent (the browser fence, under the lab's rule, reads none) |
| the acceptance | the call controller's links: `acknowledgement` (`…/cc/v1/callAcceptance/<call>/…/acknowledge`) and `applyChannelParameters` (`…/cc/v1/mcProxy/<call>/…/applyChannelParameters`), when every link of `callAcceptance.links` names one call controller host and one call id (a redelivery naming another binds nothing) | `Fence::observe_delivery`: a Trouter delivery at exactly the path of the start's own `acceptance` callback (the endpoint, call agent and random segment), its body decoded (gzip and base64, as the call controller sends it). One that arrives before the answer waits on the pending call |
| a `conversationUpdate` | nothing | none of the captured operations needs it: the callee's side binds from the attach's and the accept's answers, a join from the invitation (below) |

Every link is `https` on the flight proxy (`api.flightproxy.teams.microsoft.com`, no port) and is compared whole:
scheme, host, path and query. A follow-up is `Scope::CallOperation { operation, url, from, callbacks }`
(`WriteKind::CallFollowUp`, or `Invite` for an `add`, its invitees added); `Fence::check` resolves it in the registry,
never by a member read: a live call holding that URL for that operation, its sender (`participants.from.id`, where the
operation's body names one) the call's caller, invitees only on an `add`, and every callback link the body hands the
call's services (an acknowledgement's `callAcceptanceAcknowledgement.links`, an `add`'s `links`) on the start's own
endpoint and call agent, as the acceptance's delivery response must be (below; a call whose start named no call agent
takes none); the call's participants and any invitee must all be the lab's people.
Otherwise
`Partial(UnregisteredCall)` (a call this fence didn't permit, whose answer or acceptance wasn't read, or another
run's), `Partial(CallEnded)` or `CallMismatch`. An `add` maps only naming its sender's own chat and meeting: a
`groupChat` must name exactly the 1:1 chat of the sender and the add's one invitee (`19:<id>_<id>@unq.gbl.spaces`,
the two ids in either order), and a `meetingInfo` must name the sender's object id as its organizer; any other chat
or organizer is unreadable. `fence.log` holds the operation and its URL, never the sender or the callbacks.

**The end.** A permitted `leave` moves the call to ending, where only that `leave` passes again (the web client resent
it); a delivery at the start's own `conversationEnd` or call `end` callback ends it, and nothing passes on it any more.
An ended call is never restored, whatever arrives after. A rejoin (a later start or join of the run, permitted on its
own, answered with the same conversation) takes the conversation's links from an ending or ended call, so the new
call's follow-ups resolve to it alone; a live call keeps its links, and a link two calls hold resolves to none
(`Partial(UnregisteredCall)`). A re-invitation to that conversation answered since holds its links too (its `attach`'s
answer binds them): an operation on them acts on the invitation's call while it is open for the operation, and on the
left call's tombstone (`Partial(CallEnded)`) only when no such invitation holds them.

**The acceptance's delivery response** is one of the two Trouter delivery responses the fence lets through (the other
is an offered invitation's receipt, below; `Fence::check_delivery_response`, logged as a `delivery_response` entry): it
answers a pending acceptance delivery of
a live call on the same connection (a closed socket's pending deliveries are dropped), once, in the captured envelope (`id` the delivery's, `status` 200, exactly the headers
`MS-CV`, `trouter-client`, and `trouter-request`, `X-Microsoft-Skype-Chain-ID` and `X-Microsoft-Skype-Message-ID`
each equal to the delivery's own), its body a `callAcceptanceAcknowledgement` whose
links are all callbacks on the start's own endpoint and call agent (same authority, same path prefix). Every other
delivery response stays denied, the empty acknowledgements of a `conversationUpdate` included.

**Offered invitations** (`crates/teams-fence/src/calls/offer.rs`, M3 Task 6; decided with Codex, transcript under
`~/.local/state/plan-exec-revmux/20261024-native-teams-m3-calls/discussions/task-6-attach-join/`): the callee's side, a
record of its own beside the registered calls. A Trouter delivery at the root of the account's calling endpoint
(`/v4/f/<endpoint>/`), `{"evt": 107 | 109, "gp": <base64 JSON>}`, is an invitation when it reads: a caller and a callee,
an `attach` link on the flight proxy of a forked call leg, a conversation (`/conv/<id>`), no call it replaces, no group
context, and the call's kind in a captured shape: a 1:1 call's (107) naming no group chat, `isMultiParty` false; a
group call's (109) naming its group chat by a group chat's id, `isMultiParty` true (its chat is the only field naming
its other participants, so a group call naming none is no invitation). `Fence::observe_delivery` offers it only when it is addressed
to the fence's own account (the browser fence is now each account's own, `Fence::open_lab_account`), a lab account,
and its caller isn't a person outside the lab; any other invitation is ignored and nothing passes for it, as
is a delivery at the root that may be an invitation but doesn't read (`Observed::Ignored`: an `evt` 107 or 109, or a
`callNotification` at its top or in a `gp` that decodes, or a body that isn't a strictly read JSON object with a
numeric `evt`; the root's `evt` 115 and 116 aren't). A
group call's invitation names only its caller, its callee and its group chat, so **every decision on a group call's
invitation reads that chat fresh** (complete, lab-only, listing the caller and the callee): a gate beside the MRIs, not
proof of the call's whole roster (Codex's qualification; in the lab every participant of a lab-only chat is a fenced
lab client, and an `add` is judged per invitee). What passes on an offered invitation, each bound to it:

| While | What | Bound to |
|---|---|---|
| it rings | its delivery response, `call_invitation_receipt` in `fence.log` | the delivery on its own connection, once per delivery: `status` 200, `trouter-request` equal to the delivery's, `trouter-client`, `MS-CV` or not (both seen), an empty body |
| it rings | the `attach` (`WriteKind::CallRinging`) | exactly the invitation's `attach` link; its one additional action a `join` that waits, to exactly the invitation's conversation; its sender the callee; every callback on one call agent of the endpoint the invitation came to. It doesn't join the call: the bundle builds it as a roster-only conversation subscription (no call invitation, no media), the call rings only after its answer, and the run's rosters showed no endpoint of the callee in the call until it accepted |
| it rings | the `progress` (`CallRinging`, status `ringing` only) | the `progress` link the attach's answer handed out |
| it rings, then on the click | the endpoint state (`CallFollowUp`), the `accept` (`CallAnswer`) or the `reject` (`CallDecline`, only code 603 `CallEndReasonLocalUserInitiated`) | the conversation subscription's `updateEndpointState` link; the attach answer's `acceptance` and `callLeg` links; the accept's callbacks on the attach's call agent |
| it was answered | the answered call's endpoint metadata and state, its own participant properties (`to` only the sender), its `leave` (then only that leave again), its `renegotiate`, media lines (`updateMediaDescriptions`), `applyChannelParameters`, a renegotiation's `answer` and an answer's `acknowledge` | the conversation subscription's links; the accept answer's `mediaRenegotiation`, `updateMediaDescriptions` and `applyChannelParameters`; the links a renegotiation or an answer delivered at the callee's own `mediaRenegotiation` / `mediaAnswer` callback hands out |

The native transport reads the `attach`'s and the `accept`'s answers (`Fence::pend_answer`, `Fence::bind_answer`), as
it does the start's. A decided `accept` or `reject` passes again (the client resends), nothing else of the ring does. A delivery at the
attach's `call/end` or `conversationEnd` callback ends the invitation for good, answered or not; it is never offered
again. So does the call machine letting it go before it rang (`Fence::let_go`, the engine's for the machine's
`Effect::LetGo`): its `attach` failed for good (no further attempt goes: not sent, denied, an unknown outcome, a
refusal no retry follows), its answer didn't read, or its media failed; the service may never have taken the callbacks
its ring would end at. An `attach` refused for a reason another attempt may clear (a 401, a short throttle) retires
nothing: the attempt after it goes on the same invitation. The invitations of one conversation (a
re-invitation after a decline or a leave) hold its links alike: an operation on such a link acts on the latest one open
for it, and one closed for it keeps the links only as a tombstone (`CallEnded` when no other holds them).

**The caller's side of the media negotiation** binds the same way: the acceptance also binds the call controller's
`mediaRenegotiation` (`…/cc/v1/active/<call>/…/renegotiate`) and `updateMediaDescriptions`; a renegotiation or an
answer delivered at a `mediaRenegotiation` or `mediaAnswer` callback on the start's own call agent binds its
`negotiations/<call>/…/answer` or `mediaAnswers/<call>/…/acknowledge` link (the acknowledgement goes with no body).

**Joins** (`CallOperation::Join`, `WriteKind::Call`, judged like a start in that chat): a `POST` to a conversation
itself. An invitation this fence offered binds its conversation (its controller, exactly the join's URL) to the group
chat it named, whatever its ring did since; the join's sender is the account the invitation was addressed to, its
callbacks on that account's endpoint, and `Fence::check` reads the chat fresh. Without a call invitation the join only
subscribes to the conversation's roster (the client does so by itself once its ring ended, and only then shows the
chat's join); with one it enters the call, and pends and registers its call as a start does (its answer, the
conversation, read by the native transport; its acceptance at its own callback). The caller's own re-subscription
after its call ended is denied under this rule (no invitation binds it); the lab's rule judges it by its chat.

## Reading a chat's members

The client's member reads (all `GET`, relative to the base):

| Request | apiName in the bundle | Notes |
|---|---|---|
| `v1/threads/<id>?view=msnp24Equivalent` | `thread` | the thread with its properties and a `members` array |
| `v1/threads/<id>/members?view=msnp24Equivalent&pageSize=<n>` | `thread_members` | the member list, paged; extra filters `selectMemberRoles`, `state`, `filterOnIsModerator`, `filterExplicitlyAdded`, `filterInterest`; the next page is a full URL the client re-bases |
| `v1/threads/<id>/filtered?view=msnp24Equivalent` | `filtered_thread` | a **filtered** view of the thread |
| `v1/threads?view=msnp24Equivalent&threadIds=<id>,<id>` | `threads` | several threads at once |
| `v1/threads/onetoone?view=msnp24Equivalent&userIds=<a>,<b>&threadType=<t>&isFederated=<b>` | `one_to_one_thread` | finds the 1:1 thread of two users |
| `v1/threads/<id>/members/<mri>` | `getMemberProperty` | one member |
| `v1/users/ME/conversations/<id>?view=msnp24Equivalent` | `myconversation` | the user's own view of a conversation (not a member list) |

Seen live: the client reads a chat's thread with the batch form, `GET v1/threads?view=msnp24Equivalent&threadIds=<id>`.
The answer is a JSON array with one thread object per id: `id`, `type` `Thread`, `properties` (`threadType` `chat`,
`fixedRoster`, `uniquerosterthread`, `creator`, `createdat`, `capabilities`, …), `members` (each with `id` (the
`8:orgid:` MRI), `type` `ThreadMember`, `role`, `userLink`, `capabilities`, `hidden`, `isFollowing`, …), `version`,
`rosterVersion`, `messages` (a link) and `eTag`. For the 1:1 chat it listed both members and carried no paging key and
no member count. `GET v1/threads/<id>/consumptionhorizons` gives the members' read horizons. The user's view of the
chat, `GET v1/users/ME/conversations/<id>`, answers 404 until the chat exists, and the client takes a 404 to mean the
chat is one the user isn't in.

The fence's live read (`LiveMembershipSource`) is made from Rust with its own HTTPS client (no proxy, no redirects),
with the token the page used (bearer or skypetoken) on the host and region of a chat service request the armed lab
page itself sent, or, for the native client, with its own credential (below). It reads at most 1 MiB of body (more is
`Truncated`), within one 10 s deadline, and a 401 asks the credential source once for a renewed token and reads again
with it. Channels, teams and calls are `Partial(Unsupported)` and so denied, answered without a read
(`LiveMembershipSource::reads_in_full` tells a caller up front). Pagination and truncation are always `Partial`.

### 1:1 chats

One request, `GET v1/threads/<id>?view=msnp24Equivalent`. Live, with the bearer token, it read the a–b chat as complete
(the message and typing posts to it were allowed after the read). The answer is complete only when:

- the status is 2xx and the body is a JSON object;
- no key at the top level or under `_metadata` looks like pagination (`next…`, `…continuation…`, `skiptoken`,
  `syncState`);
- any member count (`memberCount`, `membercount`, `membersCount`, top level or under `properties`) equals the
  number of listed members;
- `id` is exactly the requested thread, and every member has a string `id`;
- **and** the members are exactly the two `8:orgid:<oid>` the chat's id names.

That last check is what makes the answer evidence of the *full* membership rather than a view filtered by the
caller's permissions: a 1:1 chat's membership is fixed by its id, and the read only confirms the thread exists with
exactly those members.

### Group and meeting chats

Any group chat (`19:<id>@thread.v2`) and any meeting chat (`19:meeting_<id>@thread.v2`), the lab's a–b–c chat, one
the native client created and any other alike: one request, `GET v1/threads/<id>/members?view=msnp24Equivalent&
pageSize=100`, with the token and the web client's `X-MS-Migration: True` and `BehaviorOverride: redirectAs404`
(`parse_members`). The answer is complete only when:

- no key at any depth looks like pagination (else `Paginated`);
- it is an object with `isDeleted` `false` and a `members` array;
- its `totalMemberCount` is an integer equal to the members listed, and any other member count agrees (else
  `Truncated`);
- every member says `hidden` `false`, says `isDeleted` `false` or nothing about it, and has an `id` in a form the reads
  showed: a work account's `8:orgid:<guid>` or a bot's `28:<guid>`;
- nobody is listed twice, case folded.

Any other shape is malformed, and a timeout or a failed read is `Partial` too: each denies the write. The fence then
judges every listed member, and every participant the write adds, against the roster: a write passes unless one is a
person outside the lab (not a lab account, the echo service or the contact; an app or a bot is no person). Whether a chat is in the lab's `chats.toml` plays no part: that file is the
scenarios' record of the lab's own group chat, not the fence's.

**What the rule accepts.** One fresh read can't prove that the service shows every participant to a member: the
member list and its count could leave out one the service shows to nobody, and the members can change between the
read and the write. [A group chat's reads](#a-group-chats-reads) records why that was once enough to deny every group
chat; the lab rule takes a complete, unpaged read whose members are all the lab's people as the chat's participants, for group and meeting chats as for 1:1 chats.

### A group chat's reads

Seen live (M1 Task 8, 2026-10-20): the a–b–c group chat, created by a in the web client (`proto-lab run abc-chat`, its
id kept in the lab's `chats.toml`), read with each account's own `ic3` token (`proto-lab probe thread <alias>`; local
reference copies `reference/chat/thread-group-{a,b,c}.jsonl`; synthetic fixtures `tests/fixtures/chatsvc/thread-group.json`,
`thread-group-members.json`, `thread-group-filtered.json` written from them). A, b and c got the same answers:

- The id: `19:<32 lowercase hex>@thread.v2`.
- `GET v1/threads/<id>?view=msnp24Equivalent` (and the batch form, an array holding the same object): 200; `id`,
  `type` `Thread`, `members` (a, b and c, each `role` `Admin`, `hidden` false, with `capabilities`, `rangeStart`,
  `rangeEnd`, `meetingRole`, `userLink`, …), `properties` (`threadType` `chat`, `creator`, `createdat`,
  `capabilities`, `historydisclosed`, `productContext` `TEAMS`, …), `rosterVersion`, `version` and `eTag` (all three the
  creation time), `tenantId`, `messages` (a link), `fileReferences`, `encryptionType`, `copilotShareTokens`. No member
  count, no paging key, no `_metadata`.
- `GET v1/threads/<id>/members?view=msnp24Equivalent&pageSize=<n>`: 200; `members` (the same three, each with
  `interest`), `totalMemberCount` 3, `eTag` (the thread's), `rosterVersion` **0**, `isDeleted` false. `pageSize=1` got
  the same answer as `pageSize=100`: all members and no next link, so its paging form was never seen.
- `GET v1/threads/<id>/filtered?view=msnp24Equivalent`: **403**, error 209 `AclCheckFailed`: a chat's `Admin` lacks
  the `GetFilteredThread` capability.
- `GET v1/users/ME/conversations/<id>?view=msnp24Equivalent`: the account's own view, no member list:
  `memberProperties` (`role`, `interest`, `isReader`), `threadProperties` (`rosterVersion` as the thread's,
  `threadType` `chat`, `productThreadType` `Chat`, `isCreator`, …), `properties.isemptyconversation`.

Decided with Codex then: not enough to call a group chat's member read complete, so group chats stayed
`Partial(Unsupported)`. Both endpoints exposing, and the count counting, one caller-visible subset fits the same
answers, and a 3-member roster that every viewer sees alike can't tell the two apart; no paging form was seen, and
the `eTag`s' meaning across the endpoints is unknown. From 2026-10-21 the fence admitted the lab's one recorded chat
when two such reads agreed on exactly a, b and c; since 2026-10-23 the lab rule judges every group and meeting chat by
its member list alone ([above](#group-and-meeting-chats)).

## Reads by POST

Requests the web client sends as a `POST` although they only read. Under the lab rule they reach nobody, whoever they
name: the middle tier's profile lookups are on none of its people paths, UPS's presence read goes to no contacting
service, and CSA's `chatsAndChannelsByIds` and Substrate's search are carve-outs, so the lab browser and the native
client let them through ([the contacting services](#the-contacting-services)). Until 2026-10-23 the browser fence let
the first three through only in their captured shape and only when every MRI they named was a lab account; the
personal build sends those it has an entry for in those shapes, as sealed `PostRead` entries, whoever they name
([below](#the-native-clients-reads-by-post)).

| Request | Body |
|---|---|
| `POST https://<teams origin>/api/mt/part/<partition>/beta/users/fetchShortProfile?<the lookup query>` | a non-empty JSON list of MRIs |
| `POST https://<teams origin>/api/mt/part/<partition>/beta/users/fetch?<the lookup query>` | a non-empty JSON list of MRIs |
| `POST https://<teams origin>/ups/<geo>/v1/presence/getpresence/` (M2 Task 17) | a non-empty JSON list of `{"mri": …}`, each with at most `"source": "ups"` beside it, no MRI twice |
| `POST https://<teams origin>/ups/<geo>/v1/pubsub/subscriptions/<epid>` (2026-10-27) | exactly `trouterUri`, `shouldPurgePreviousSubscriptions`, `subscriptionsToAdd` and `subscriptionsToRemove`, each list of `{"mri": …, "source": "ups"}`: the account's own endpoint subscribed to others' presence ([trouter.md](trouter.md#the-subscription-and-its-deliveries)) |

The profile lookups, as 5869 captured ones sent them:

- `POST` on `https://teams.microsoft.com` or `https://teams.cloud.microsoft`, the partition a lowercase region, `-`
  and a number (`amer-03`);
- the query `isMailAddress=false`, `enableGuest=true`, `skypeTeamsInfo=true`, `canBeSmtpAddress=false`,
  `includeIBBarredUsers=true`, `includeDisabledAccounts=true` (one lookup by address had `isMailAddress=true` with
  addresses in the body);
- the headers `Accept`, `Cookie`, `Origin`, `Referer`, `User-Agent`, `authorization`, `content-type` and the client's
  `x-client-ui-language`, `x-ms-client-caller`, `x-ms-client-request-type`, `x-ms-client-type`,
  `x-ms-client-version`, `x-ms-migration`, `x-ms-object-id`, `x-ms-request-id`, `x-ms-session-id`, each with one
  value on every lookup where that matters: `Content-Type` `application/json;charset=UTF-8`, `x-ms-object-id` empty,
  `x-ms-migration` `True`, `x-ms-test-user` `False`, `x-ms-client-request-type` `0`, `x-ms-client-type` `cdlworker`;
  `x-ms-client-caller` names the client component that asked (dozens seen, `use-chat-hook`, `User.avatarUrl`);
- a body that is a non-empty list of MRIs.

The answers seen live name exactly the requested accounts: `{"type": …, "value": [one profile per requested MRI]}`,
each profile with its `mri`, `objectId`, `displayName`, `givenName`, `surname`, `email`, `userPrincipalName`,
`userType`, `tenantName`, `isShortProfile`, and for `users/fetch` also `alias`, `mail`, `smtpAddresses`,
`sipProxyAddress`, `phones`, `accountEnabled`, `skypeTeamsInfo`, `featureSettings` and such; no other account's id
or address.

### The presence read

UPS's `getpresence` (M2 Task 17), as the 5461 presence reads of the captures of 2026-10-19 to 2026-10-21 sent it:

- `POST` on either Teams origin, the path `/ups/<geo>/v1/presence/getpresence/` with its trailing slash, the geo
  lowercase letters (`noam`), and no query at all;
- the headers `Accept`, `Cookie`, `Origin`, `Referer`, `User-Agent`, `authorization`, `behavioroverride`,
  `content-type`, `x-ms-client-caller`, `x-ms-client-type`, `x-ms-client-user-agent`, `x-ms-client-version`,
  `x-ms-correlation-id`, `x-ms-object-id`, `x-ms-request-id`, `x-ms-session-id`, with `Content-Type`
  `application/json`, `behavioroverride` `redirectAs404`, `x-ms-client-type` `cdlworker`, `x-ms-client-user-agent`
  `Teams-V2-Web`, `x-ms-object-id` empty; on 2065 of them also `x-ms-endpoint-id`, the client's own endpoint;
- a body that is a non-empty list of objects, each `{"mri": <string>}` or `{"mri": <string>, "source": "ups"}` (the
  client's own MRI came with `source`), no MRI twice.

The first run that let it through (`ab-chat` of 2026-10-22) sent 14 of them, each naming only a, b or c; every answer
was 200 and named only the accounts asked for: a list of one item per MRI (`mri`, `presence` with `sourceNetwork`
(`Self` for the account's own, `SameEnterprise` for another), `calendarData` (`isOutOfOffice`), `capabilities`,
`availability`, `activity`, `lastActiveTime`, and `source` when asked with it), `etagMatch`, `etag`, and the item's
`status` `20000`) ([trouter.md](trouter.md#others-presence)).

### The native client's reads by POST

A lab binary sends three of them, each a plain `Request` through `Transport::send` (judged by `contacting`, `None`, and
logged `allowed` before it goes), whoever it names:

- `ShortProfileLookup` to the middle tier (the account's middle-tier base from the GTM `middleTier`), in the captured
  query, with `Accept: application/json`, `Content-Type: application/json;charset=UTF-8`, `x-ms-client-request-type:
  0`, `x-ms-client-type: cdlworker`, `x-ms-migration: True`, an empty `x-ms-object-id`, `x-ms-test-user: False`, the
  body a JSON list of the MRIs, and the `api.spaces.skype.com` token. `read_answer` reads the answer
  (`{"type": …, "value": [profile, …]}`, both keys required as every captured answer has them) and drops it whole if
  any profile is of an MRI the lookup didn't ask for.
- `PresenceRead` to UPS (the base from the GTM `presenceUPS`: `HttpTransport::with_presence`, an AFD host and
  `/ups/<geo>` exactly) at `v1/presence/getpresence/` with its trailing slash and no query, with `Accept:
  application/json`, `Content-Type: application/json`, `behavioroverride: redirectAs404`, `x-ms-client-type:
  cdlworker`, `x-ms-client-user-agent: Teams-V2-Web` and an empty `x-ms-object-id` (no `x-ms-endpoint-id`, no cookie,
  no caller or correlation ids), the body `[{"mri": …}, …]` (never the web client's `source`), and the
  `https://presence.teams.microsoft.com/` token. `PresenceRead::read_answer` drops the answer whole if any item is of
  an MRI not asked for, and reads an item's `availability` and `activity` only when its `status` is the captured
  `20000`.
- `PresenceSubscription` to UPS at `v1/pubsub/subscriptions/<epid>` with no trailing slash and no query (the `epid` a
  `RegisteredEndpoint`, a registration of this run), the presence read's headers (not the web client's
  `x-ms-endpoint-id`), the body above with the connection's push URL and `"source": "ups"` beside each MRI, never a
  `28:` or `48:` id, at most 8 added per request by the engine. `PresenceSubscription::read_answer` drops the answer
  whole if any status is of an MRI not asked for, and counts an MRI subscribed only for the captured `20200`.

Not sent: `users/fetch` (names come from Graph and `short_profile`).

Both went out naming an account outside the lab for the first time on 2026-10-23 (`teams-headless a presence --mri`,
`teams-headless a profile`, which name anyone by MRI): each was logged `allowed` and answered 200 (the lookup for an
account Graph answered 404 for, with no name for it).

The personal build sends the three as entries of `teams-proto`'s sealed `PostRead` trait (`short_profile`, `presence`,
`presence_subscription`, the last one's path ending in the endpoint's `epid`, a lowercase GUID), today's examples, not a
closed list ([personal-build.md](../personal-build.md#its-reads)), sent by `HttpTransport::post_read` only as an
`AuthorizedPostRead`: `Fence::check_post_read(kind, names)` logs the entry's kind and how many MRIs it names (as
`post_read_decision`, never with the MRIs) and, through a personal fence, permits any names with a `PostReadPermit`, which `AuthorizedPostRead::new` pairs with
the request only for that entry and exactly those MRIs; the transport reads the request back against the entry's
captured shape (the path, every query pair once, every header but the token once with its pinned value, the body
naming exactly the permit's MRIs), logs it by its kind and sends it once. A lab binary doesn't use that path.

## How a run uses it

- Every run's browser fence asks `contacting` the same way; only its membership source differs. `load`, `status` and
  `login` read no members: their source is `FailClosed`, so every write whose target has to be fetched is denied
  (`Partial(NoSource)`), while everything that reaches nobody passes.
- `ab-chat` and the other writing scenarios read members live (`LiveMembershipSource::for_fence`). The browser fence
  hands the source the account's chat service token from the page's own chat service requests
  (`HttpFence::with_credentials`: `HttpFence::handle` takes it from the raw `Fetch.requestPaused` event's headers). A
  `PausedRequest` keeps every header's name, and the value only of the method overrides `contacting` reads
  (`teams_fence::OVERRIDE_HEADERS`), never a token or a cookie. The event itself, headers included, has already been
  handed to the run's event watchers (`ArmedBrowser::watch`) and offered to the capture recorder. A write to the a–b
  1:1 chat passes only when the read confirms its two members are the lab's people.
- The native client sends each chat write as a sealed request whose target is the table's: the chat for every write
  on it, a message's and an edit's mentioned MRIs added, a new chat `New` with its members. Its live member read takes
  the client's own credential from the native transport on every read (`CredentialSource`: the account's `ic3` token
  as its session has it now, and the region's chat base, as `ChatCredential::bearer`); with no token the read is
  `Partial(NoCredential)` and the write denied. Its source is `LiveMembershipSource::for_fence`, so it writes to any
  chat whose members are all the lab's people, a group chat it created included.
- `ab-chat` opens the chat by b's MRI (`l/chat/0/0?users=<mri>`): by UPN the client first resolves the user with the
  middle tier's `fetchShortProfile` by address (`isMailAddress=true`), which was denied when `ab-chat` was written; by
  the chat's id a page that loaded before the chat existed says the user isn't in it.
- `abc-chat` (M1 Task 8) creates the a–b–c group chat once, from account a: it opens the web client straight at the
  new-chat deep link for b and c (a deep link reloads the whole client), sends a first message carrying the marker
  `lab fence probe abc-chat first message ` and the run's nonce, force-denied under a label of this run's own (an
  earlier creation run's first message, sent again on load, is denied under another, so it can't stand in for this
  run's), and records the chat's id (from that denied post's URL) in the lab's `chats.toml` as soon as the deny names
  it, even when the run fails after that: the chat exists by then, and a run without its id would create another one.
  The marker begins with `ab-chat`'s probe marker, so every scenario that opens a's profile with members read live
  force-denies the first message when the web client sends it again, and expects that deny: `abc-chat` under its
  earlier-first-message label, `ab-chat`, `ab-chat-full`, `ab-image`, `ab-trouter`, `ab-trouter-quiet` and
  `echo-call` under the earlier-run-probe label, all from before a's page loads (`echo-call`, which has no probe of
  its own, installs only that rule). `load` reads no members, so its fence denies the resend as every write
  (`Partial(NoSource)`). Once the id is recorded, a run messages the chat (a's probe, then a real message that b and c
  must show). It refuses to start, before any browser opens, when the lab's source says in `reads_in_full` that it
  can't read the recorded id's members at all (not a 1:1, group or meeting chat's id): a denied message would be
  resent on every later load.
- `ab-chat`'s live enforcement test forces a deny (`ForcedDeny`) on every write carrying the probe marker, from before
  the page loads to the end of the run (so an earlier run's unsent probe, sent again on load, is denied too). This
  run's probe carries a run-unique nonce and has a labelled forced deny of its own ahead of the marker's, so the
  evidence is this run's probe only: a deny under that label of a post to the a–b chat, and the same request, found by
  the `networkId` the paused event gives (the page's `Network` request id), failing in the page with
  `net::ERR_BLOCKED_BY_CLIENT`; then b's copy of the chat must hold the real message and not the probe.

## What the captures showed

`proto-lab run load` (account a, fail-closed) and `proto-lab run ab-chat` (a and b) on 2026-10-20, read locally; the
captures stay outside the repository and no fixture was exported from them. The verdicts named here are those of the
fence then (default deny, an own-state list, a reviewed expected-deny list); [the contacting
services](#the-contacting-services) give today's.

- [x] The chat service base: `https://<teams origin>/api/chatsvc/amer/` (AFD), on `teams.cloud.microsoft` or
      `teams.microsoft.com`, as the fence's URL check expects. **Corrected**: its requests carry
      `Authorization: Bearer` with an Entra token for `https://ic3.teams.office.com`, not a skypetoken. The
      skypetoken goes, as `Authorization: skype_token <token>`, to the async gateway and the media service
      (`*.asyncgw.teams.microsoft.com`, `*.asm.skype.com`, `…/skypetokenauth`); CSA takes a bearer token for
      `chatsvcagg.teams.microsoft.com`, the middle tier one for `api.spaces.skype.com`. `ChatCredential` now accepts
      the bearer form and the live read sends it back as such.
- [x] The 1:1 thread read: the fence's `GET v1/threads/<1:1 id>?view=msnp24Equivalent` with the bearer token came back
      complete for the a–b chat (the writes it gated were allowed). The client itself reads the batch form
      (`threads?threadIds=`), whose thread object has `id`, both members in `members` and no paging or count field.
- [x] A group chat's read and `thread_members` (M1 Task 8, [A group chat's reads](#a-group-chats-reads)): the thread
      read and the member list agree and the list carries `totalMemberCount`; no paging form was seen and the filtered
      view is refused to members.
- [x] Writes as sent: chat create, message (`RichText/Html`), typing (`Control/Typing`) and read receipt
      (`consumptionhorizon`) match the table and the mapping (see [Writes](#writes)).
- [x] Edit, delete, reaction (added and taken back) and quoted reply, exercised by `ab-chat-full` (2026-10-20, M1
      Task 7): they match the table; each was mapped to the a–b chat and allowed after the live member read. The edit
      sends the whole message again, which the mapping now requires (with a message `messagetype`), and a message's or
      an edit's mentioned MRIs are added participants. A mention, on 2026-10-21: as the bundle builds it, b added to
      the target and allowed. Member changes and reply chains weren't exercised.
- [x] The 1:1 id: the two object ids, lowercase and sorted (the bundle builds it that way; the live chat's id matches).
      Federated and guest chats weren't seen.
- [x] Own state: the own properties `messageDensity`, `favorites`, `firstLoginInformation`, `locale` and
      `readReceiptsEnabled`, each a one-key JSON object, which the client retried more than a hundred times a run
      while they were denied; no endpoint registration (the client uses the push channel `trouter` and the async
      gateway instead).
- [x] `RegistrationToken`: never sent; reads work without it.
- [x] Denied in every run then, all unknown: telemetry (`*.events.data.microsoft.com`,
      `browser.pipe.aria.microsoft.com`, `admin.microsoft.com/api/instrument`), the middle tier's lookups by `POST`
      (`users/fetchShortProfile` with a JSON array of MRIs and `users/fetch`, `effectivePolicies`,
      `useraggregatesettings`, `fetchfeaturesettings`, `apps/aggregatedEntitlements`, `apps/eligibilities`,
      `apps/batchedDefinitions`, `me/files/availability`, `users/<oid>/cookiev2`, `mcps/initArtifactFolder`), CSA's
      `chatsAndChannelsByIds`, presence (`ups/<region>/v1/presence/getpresence`), the team push updates
      (`api/tps/…/pushupdates`), the async gateway's and media service's token exchanges, the editor service
      (`editor.svc.cloud.microsoft`), Substrate search suggestions, and every frame sent on the `augloop.office.com`
      and `go.trouter.teams.microsoft.com` sockets. The chat worked without them; without the profile lookups a chat
      partner showed as "Unknown User". `ab-chat-full`'s runs of 2026-10-20 added the bot invokes an opening editor
      sends (`POST v1/agents/<bot>/invoke`, `composeExtension/fetchCommands`) and the own activity feed's item read
      (`PUT v1/users/ME/conversations/48%3Anotifications/messages/<id>/properties?name=isread`). Under the lab rule
      all of these pass but the bot invokes and, on the Trouter socket, every frame but socket.io types `0`, `1`, `2`
      and `5`: an answer to a delivery (`3:::`) was denied then (the lab's call rule now lets it pass).

The synthetic fixtures in `tests/fixtures/chatsvc/` stay until they are rewritten synthetic-from-capture: written by
hand from a local reference copy of these captures, with the same shape and made-up values. Captures never enter the
repo, not even sanitized.
