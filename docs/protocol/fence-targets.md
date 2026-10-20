# Fence targets: the chat service's member reads and writes

Status: 2026-10-20, M0 Task 11. First written from static analysis of the web bundle `26260.1717.5229.7049` (imported
with `proto-lab bundles import`, searched with `proto-lab bundles search` and read in the chat service's request
builders), plus [auth.md](auth.md). Then checked the same day against live traffic, once the lab accounts had a Teams
license: `proto-lab run load` (account a) and `proto-lab run ab-chat` (a and b, the a–b 1:1 chat), the web client as
served then. [What the captures showed](#what-the-captures-showed) lists what they confirmed and what they corrected;
the sections above it are already corrected. M1 Task 7's `proto-lab run ab-chat-full` (the same day) exercised the
edit, the delete, the reactions and a quoted reply, and corrected the edit row ([chat.md](chat.md) has the shapes).
M1 Task 8 (2026-10-20) added a group chat: [A group chat's reads](#a-group-chats-reads).

The code: the mapping is `Classifier::mapped` (`crates/proto-lab/src/classify.rs`), the member read is
`LiveMembershipSource` (`crates/teams-fence/src/membership.rs`). Both are tested against synthetic fixtures in the
sanitizer's form (`tests/fixtures/chatsvc/`).

## The chat service

Chats live in the chat service ("chatsvc", the IC3 messaging service). Its base URL comes from authz's region GTMs:
`chatServiceAfd`, which prod prefers, or `chatService` (see auth.md). The web client recognizes the Front Door form by
host: `teams.microsoft.com`, `teams.cloud.microsoft` (commercial), `teams.live.com` (consumer), `gov.teams.microsoft.us`
and `dod.teams.microsoft.us`, with the path `/api/chatsvc/<region>/`. Every request below is relative to that base and
starts with `v1/` (endpoint registration with `v2/`). Conversation and thread ids are percent-encoded as one path
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

What the fence accepts as the chat service (the mapping and the live read share one check,
`teams_fence::chat_service_path`): `https` only, no port, no user info, and either an AFD host of the commercial cloud
(`teams.microsoft.com`, `teams.cloud.microsoft`) with `/api/chatsvc/<region>/v1/…`, the region 1–32 lowercase letters,
digits or `-`, or a host ending in `.msg.teams.microsoft.com` with `/v1/…`, each label of its name before that suffix
non-empty letters, digits or `-`. The `.msg` host form is the regional chat host: the bundle doesn't name it (the GTM
value comes at runtime), but the captures did, as the `chatService` GTM and in `Location` answers (above), with `/v1/…`
straight under the host. The web client never sent a request there, so the fence has only ever judged the AFD form
live.

### Ids

| Id | What |
|---|---|
| `19:<oid>_<oid>@unq.gbl.spaces` | a 1:1 chat; its id names both members' Entra object ids, lowercase and sorted |
| `19:<id>@thread.v2` | a group chat; `19:meeting_<id>@thread.v2` is a meeting's chat |
| `19:<id>@thread.tacv2`, `19:<id>@thread.skype` | a team's channel (the team's General channel has the team's id) |
| `<channel id>;messageid=<root id>` | a reply chain in a channel (the client joins the root message id to the conversation id) |
| `48:notes`, `48:…` | the user's own streams (notes, notifications, call logs) |
| `8:orgid:<oid>` | a work account's MRI; `28:<id>` a bot |

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

The fence's live read (`LiveMembershipSource`) makes exactly one request, `GET v1/threads/<id>?view=msnp24Equivalent`
with the token the page used (bearer or skypetoken), from Rust with its own HTTPS client (no proxy, no redirects), on
the host and region of a chat service request the armed lab page itself sent. Live, with the bearer token, it read the
a–b chat as complete (the message and typing posts to it were allowed after the read). It accepts the answer as
complete only when:

- the status is 2xx and the body (at most 1 MiB) is a JSON object;
- no key at the top level or under `_metadata` looks like pagination (`next…`, `…continuation…`, `skiptoken`,
  `syncState`);
- any member count (`memberCount`, `membercount`, `membersCount`, top level or under `properties`) equals the
  number of listed members;
- `id` is exactly the requested thread, and every member has a string `id`;
- **and, since only 1:1 chats are supported,** the members are exactly the two `8:orgid:<oid>` the chat's id names.

That last check is what makes the answer evidence of the *full* membership rather than a view filtered by the
caller's permissions: a 1:1 chat's membership is fixed by its id, and the read only confirms the thread exists with
exactly those members. Group and meeting chats, channels, teams and calls are `Partial(Unsupported)` and so denied,
answered without a read (`LiveMembershipSource::reads_in_full` tells a caller up front): the bundle shows paging
(`thread_members`) and a separate filtered view (`filtered_thread`), and for group chats a live read didn't settle it
either (below). Pagination and truncation are always `Partial`.

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

**Decision (with Codex): not enough to call a group chat's member read complete; group chats stay
`Partial(Unsupported)`.** The candidate was a paired read: the thread read and the member list unpaged and listing the
same distinct members, `totalMemberCount` equal to that list, equal `eTag`s, `threadType` `chat`, not a meeting's id.
It shows the two endpoints agree, not that what they agree on is the whole membership: both exposing, and the count
counting, one caller-visible subset fits the same answers, and a 3-member roster that every viewer sees alike can't
tell the two apart. Nor was any paging form seen, and the `eTag`s' meaning across the endpoints is unknown (the list's
`rosterVersion` is 0). What would settle it, and the open decision for the user (whether M1 exits on 1:1 chats), are in
`docs/backlog/group-chat-member-read-completeness.md`.

## Writes

Every mutating request the mapping recognizes, with its `WriteKind` and `Target` (all relative to the base; `<chat>`
is a chat id, `1:1` or group, never a channel).

| Request | Body | WriteKind | Target |
|---|---|---|---|
| `POST v1/users/ME/conversations/<chat>/messages` | `messagetype` `Text`, `RichText`, `RichText/…` | `Message` | `Conversation(<chat>)`, adding whoever `properties.mentions` names |
| same | `messagetype` `Control/Typing`, `Control/ClearTyping` | `Typing` | `Conversation(<chat>)` |
| `PUT v1/users/ME/conversations/<chat>/messages/<msg>` | the whole message again, a message `messagetype` | `Edit` | `Conversation(<chat>)`, adding whoever `properties.mentions` names |
| `DELETE v1/users/ME/conversations/<chat>/messages/<msg>?behavior=softDelete` | none | `Delete` | `Conversation(<chat>)` |
| `PUT`/`DELETE v1/users/ME/conversations/<chat>/messages/<msg>/properties?name=emotions` | `{"emotions": {"key", "value"}}` / `{"emotions": {"key"}}` | `Reaction` | `Conversation(<chat>)` |
| `PUT v1/users/ME/conversations/<chat>/properties?name=consumptionhorizon` | the read horizon | `ReadReceipt` | `Conversation(<chat>)` |
| `POST v1/threads` | `{members: [{id, role}], properties}` | `ChatCreate` | `New`, adding every member in the body |
| `PUT v1/threads/<chat>/members` | `{members: [{id, role}]}` | `MemberChange` | `Conversation(<chat>)`, adding the body's members |
| `PUT v1/threads/<chat>/members/<mri>` | the member's role or property | `MemberChange` | `Conversation(<chat>)`, adding `<mri>` |
| `DELETE v1/threads/<chat>/members/<mri>` | none | `MemberChange` | `Conversation(<chat>)` |

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
- **Mention**: not seen (the fence leaves the mention picker nobody to offer; chat.md). Its form is the bundle's.

Rules that keep the mapping fail-closed:

- The kind of a message post comes from its body. A body the browser didn't inline, one that isn't JSON, or a
  `messagetype` not in the table (`Control/…` other than typing, `Event/…`, …) is `Unknown` and denied. An edit needs
  its body too, with a message `messagetype`.
- Whoever a message or an edit mentions is an added participant of its target, so the fence judges them as it judges
  the chat's members, and a mention of an outsider is denied. The mentions are `properties.mentions`, a JSON string of
  a list whose entries each name an `mri` (the bundle's form; the captures only had the empty `"[]"`). No
  `properties` or no `mentions` is no mention; `properties` that aren't an object, `mentions` in any other form, and
  either key in another case (`Properties`, `Mentions`, which a service binding keys in any case may read) are
  `Unknown`.
- A message or an edit maps to its chat alone only in the forms the captures showed of the fields that could take it
  further: `crossPostChannels` (a post into channels too) left out or `[]`, and `properties.onbehalfof` (a message
  sent for someone else) left out or `null`. Any other value of either, or either key in another case, is `Unknown`.
- A new chat's participants are exactly the body's `members`; a body without them is `Unknown`. The fence judges a
  `New` target by those participants without a read.
- Only chat ids map: channels (`@thread.tacv2`, `@thread.skype`) and reply chains (`;messageid=`) are `Unknown`,
  because a channel's target needs its team, which the URL doesn't carry. `ChannelPost` stays unmapped until a capture
  shows how to find the team.
- Anything the table doesn't list is `Unknown`, among them (all seen in the bundle):
  - conversation properties other than `consumptionhorizon` (favourites, muting, …) and thread properties
    (`PUT v1/threads/<id>/properties?name=<n>`, the topic and such, which other members see);
  - `PUT v1/threads/<id>/members/ALL?…` (a property of every member), live messages
    (`v1/threads/<id>/livemessages`), forwarding and moving messages (`…/messages/forward`, `…/messages/move`),
    targeted messages, reply-chain metadata (`rcmetadata`, `rc2s`), file references (`v1/threads/<id>/files`), drafts
    (`v1/users/ME/drafts`);
  - bot invokes (`POST v1/agents/<bot mri>/invoke`), a `BotReply` candidate; seen live whenever an editor opened (the compose
    box at the mention picker, an edit box): `composeExtension/fetchCommands` to two message extensions' bots, denied
    (and expected by the scenarios' reviewed list, [chat.md](chat.md));
  - the user's own properties (`PUT v1/users/ME/properties?name=<n>`), endpoint registration
    (`PUT`/`DELETE v2/users/ME/endpoints/<id>`) and the active-endpoint post (`activeendpoint`). These change only the
    account's own state and are **candidates for the own-state list**, but they are not on it: nothing is added there
    without a review. Seen live: the own properties `messageDensity`, `favorites`, `firstLoginInformation`, `locale` and `readReceiptsEnabled`,
    each a one-key JSON object, which the client retries more than a hundred times a run while they are denied; no
    endpoint registration or active-endpoint post at all. Until reviewed a writing run sees them denied (`ab-chat`
    fails on them, which is the honest outcome).
  - writes on a team's threads, which the lab accounts belong to with people outside the lab: the user's own member
    entry in a team channel (`PUT v1/threads/<channel>/members/<own MRI>`, `{"expiration": <n>}`) and a conversation
    property (`PUT v1/users/ME/conversations/<channel>/properties?name=lastTimeFavorited`). They are `Unknown` because
    channels aren't mapped, and denied; they would be denied by membership too.
- Calls (`Call`, `CallAnswer`), meetings, invites and channel posts aren't mapped here: calling goes through other
  services. A run that calls (`Classifier::calling`, Task 18) maps only a call's start, as a new call of its caller and
  callees; every write on an existing call and any answer stay unknown ([calling-echo.md](calling-echo.md)). The middle tier (`/api/mt/…`, `/api/apps/…`) and CSA (`chatsvcagg`) mutating requests aren't
  mapped either; live they were all lookups sent as `POST` (below), denied as `Unknown`.

## How a run uses it

- `load`, `status` and `login` stay fail-closed: the seeded classifier maps no write, and the membership source is
  `FailClosed`.
- `ab-chat` uses `Classifier::mapped` with `LiveMembershipSource`. The browser fence hands the source the account's
  chat service token from the page's own chat service requests (`HttpFence::with_credentials`: `HttpFence::handle`
  takes it from the raw `Fetch.requestPaused` event's headers before the event becomes a `PausedRequest`, which holds
  no header). The event itself, headers included, has already been handed to the run's event watchers
  (`ArmedBrowser::watch`) and offered to the capture recorder. A write to the a–b 1:1 chat passes only when the read
  confirms its two members are lab accounts.
- `ab-chat` opens the chat by b's MRI (`l/chat/0/0?users=<mri>`): by UPN the client first resolves the user with the
  middle tier's `fetchShortProfile`, a `POST` the fence denies, and never opens the chat; by the chat's id a page that
  loaded before the chat existed says the user isn't in it.
- `abc-chat` (M1 Task 8) creates the a–b–c group chat once, from account a: it opens the web client straight at the
  new-chat deep link for b and c (a deep link reloads the whole client), sends a first message carrying the marker
  `lab fence probe abc-chat first message ` and the run's nonce, force-denied under a label of this run's own (an
  earlier creation run's first message, sent again on load, is denied under another, so it can't stand in for this
  run's), and records the chat's id (from that denied post's URL) in the lab's `chats.toml` as soon as the deny names
  it, even when the run fails after that: the chat exists by then, and a run without its id would create another one.
  The marker begins with `ab-chat`'s probe marker, so `ab-chat` and
  `abc-chat` force-deny the first message, as expected, when the web client sends it again; any other scenario on a's
  profile sees the fence deny it as a write to a group chat (`Partial(Unsupported)`), unexpected there. Once the id is recorded, a run would
  message the chat (a's probe, then a real message that b and c must show), but it refuses to start while
  `LiveMembershipSource::reads_in_full` says no for a group chat: a denied message would be resent on every later load.
- `ab-chat`'s live enforcement test forces a deny (`ForcedDeny`) on every write carrying the probe marker, from before
  the page loads to the end of the run (so an earlier run's unsent probe, sent again on load, is denied too). This
  run's probe carries a run-unique nonce and has a labelled forced deny of its own ahead of the marker's, so the
  evidence is this run's probe only: a deny under that label of a post to the a–b chat, and the same request, found by
  the `networkId` the paused event gives (the page's `Network` request id), failing in the page with
  `net::ERR_BLOCKED_BY_CLIENT`; then b's copy of the chat must hold the real message and not the probe.

## What the captures showed

`proto-lab run load` (account a, fail-closed) and `proto-lab run ab-chat` (a and b) on 2026-10-20, read locally; the
captures stay outside the repository and no fixture was exported from them.

- [x] The chat service base: `https://<teams origin>/api/chatsvc/amer/` (AFD), on `teams.cloud.microsoft` or
      `teams.microsoft.com`, as the fence's URL check expects. **Corrected**: its requests carry
      `Authorization: Bearer` with an Entra token for `https://ic3.teams.office.com`, not a skypetoken. The
      skypetoken goes, as `Authorization: skype_token <token>`, to the async gateway and the media service
      (`*.asyncgw.teams.microsoft.com`, `*.asm.skype.com`, `…/skypetokenauth`, posts the fence denies); CSA takes a
      bearer token for `chatsvcagg.teams.microsoft.com`, the middle tier one for `api.spaces.skype.com`.
      `ChatCredential` now accepts the bearer form and the live read sends it back as such.
- [x] The 1:1 thread read: the fence's `GET v1/threads/<1:1 id>?view=msnp24Equivalent` with the bearer token came back
      complete for the a–b chat (the writes it gated were allowed). The client itself reads the batch form
      (`threads?threadIds=`), whose thread object has `id`, both members in `members` and no paging or count field.
- [x] A group chat's read and `thread_members` (M1 Task 8, [A group chat's reads](#a-group-chats-reads)): the thread
      read and the member list agree and the list carries `totalMemberCount`; no paging form was seen and the filtered
      view is refused to members. Not enough to call the read complete (decided with Codex): group chats stay
      `Partial(Unsupported)` (`docs/backlog/group-chat-member-read-completeness.md`).
- [x] Writes as sent: chat create, message (`RichText/Html`), typing (`Control/Typing`) and read receipt
      (`consumptionhorizon`) match the table and the mapping (see [Writes](#writes)).
- [x] Edit, delete, reaction (added and taken back) and quoted reply, exercised by `ab-chat-full` (2026-10-20, M1
      Task 7): they match the table; each was mapped to the a–b chat and allowed after the live member read. The edit
      sends the whole message again, which the mapping now requires (with a message `messagetype`), and a message's or
      an edit's mentioned MRIs are added participants. Member changes, reply chains and a mention weren't exercised.
- [x] The 1:1 id: the two object ids, lowercase and sorted (the bundle builds it that way; the live chat's id matches).
      Federated and guest chats weren't seen.
- [x] Own state: the own properties above; no endpoint registration (the client uses the push channel `trouter` and the
      async gateway instead). Still not on the own-state list.
- [x] `RegistrationToken`: never sent; reads work without it.
- [x] Denied in every run, all `Unknown`, expected by `load` and, through the reviewed expected-deny list that keeps
      them denied (`crates/proto-lab/src/scenario/expected.rs`), by `ab-chat`, `ab-chat-full` and `echo-call`: telemetry
      (`*.events.data.microsoft.com`, `browser.pipe.aria.microsoft.com`, `admin.microsoft.com/api/instrument`), the
      middle tier's lookups by `POST` (`users/fetchShortProfile` with a JSON array of MRIs, `users/fetch`,
      `effectivePolicies`, `useraggregatesettings`, `fetchfeaturesettings`, `apps/aggregatedEntitlements`,
      `apps/eligibilities`, `apps/batchedDefinitions`, `me/files/availability`, `users/<oid>/cookiev2`,
      `mcps/initArtifactFolder`), CSA's `chatsAndChannelsByIds`, presence (`ups/<region>/v1/presence/getpresence`), the
      team push updates (`api/tps/…/pushupdates`), the async gateway's and media service's token exchanges, the editor
      service (`editor.svc.cloud.microsoft`), Substrate search suggestions, and every frame sent on the
      `augloop.office.com` and `go.trouter.teams.microsoft.com` sockets. The chat works without them, though a chat
      partner shows as "Unknown User" (no profile lookup). Two classes on the list were first seen in `ab-chat-full`'s
      runs of 2026-10-20: the bot invokes an opening editor sends (`POST v1/agents/<bot>/invoke`,
      `composeExtension/fetchCommands`) and the own activity feed's item read (`PUT
      v1/users/ME/conversations/48%3Anotifications/messages/<id>/properties?name=isread`).

The synthetic fixtures in `tests/fixtures/chatsvc/` stay until they are rewritten synthetic-from-capture: written by
hand from a local reference copy of these captures, with the same shape and made-up values. Captures never enter the
repo, not even sanitized.
