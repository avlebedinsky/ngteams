# The chat service: conversations, messages and what happens to them

Status: 2026-10-20, M1 Task 7. Written from what the lab saw, in our own words, plus the web bundle where nothing was
captured (marked so). The chat service's base, credential, ids, member reads and the fence's mapping of writes are in
[fence-targets.md](fence-targets.md); its token in [auth.md](auth.md).

Sources (all outside the repository, under the lab's data dir; fixtures are written from them by hand, synthetic):

- **probe**: Task 3's read-only `proto-lab probe-chat` of accounts a and b (`reference/chat/probe-conversations-a.jsonl`,
  `…-b.jsonl`): the conversation list.
- **ab-chat-full**: `proto-lab run ab-chat-full` of 2026-10-20 (accounts a and b, the a–b 1:1 chat, captures
  `captures/ab-chat-full/20261020T194153Z` for a and `…194156Z` for b, reference copies
  `reference/chat/ab-chat-full-a.jsonl`, `…-b.jsonl`), and the confirming run recorded in the M1 plan's Task 7
  (`captures/ab-chat-full/20261020T205155Z` and `…205156Z`, read locally): a sent, edited, reacted and took the
  reaction back, replied with a quote, reacted to the reply, sent and deleted a message; b opened the chat (reading
  its history) and reacted.
- **bundle**: the web client `26260.1717.5229.7049`, for what no run showed.

Every path below is relative to the chat service base (`…/api/chatsvc/<region>/`), and every id in a path is
percent-encoded as one segment.

## The conversation list (probe)

`GET v1/users/ME/conversations?view=msnp24Equivalent&pageSize=200&startTime=1&targetType=All` answers 200 with
`{"conversations": [...], "_metadata": {...}}`. The web client never sent it: it reads its chat list from CSA
(`chatsAndChannelsByIds`, a `POST` the fence denies). Fixture: `tests/fixtures/chatsvc/conversations.json`.

- `_metadata`: `totalCount` (5 for a lab account), `forwardLink` (empty: one page held everything) and `syncState`, a
  full URL for the next incremental read: `v1/users/<the account's MRI>/conversations?view=msnp24Equivalent&syncState=
  <opaque>&pageSize=200`. The path names the account's MRI, not `ME`. A non-empty `forwardLink` (the next page) was
  never seen; the client follows it as a full URL.
- Each entry: `id`, `type` `Conversation`, `version` (ms), `targetLink` (the thread, `v1/threads/<id>`), `messages`
  (the message list's URL), `lastUpdatedMessageId` and `lastUpdatedMessageVersion` (numbers; 0 for an empty chat),
  `lastMessage` (a message as below, without `properties`; `{}` for an empty chat), `memberProperties` (the account's
  own `role`, `interest`, `isReader`, `memberExpirationTime`, `relationshipState`), `properties` (`addedBy`,
  `addedByTenantId`, `consumptionhorizon` when the account has read anything, `isemptyconversation` as the strings
  `True`/`False`) and `threadProperties`.
- What kind of conversation it is comes from `threadProperties`: `threadType` `chat` with `productThreadType`
  `OneToOneChat` (a 1:1 chat, `uniquerosterthread` `True`), `threadType` `space` with `TeamsTeam` (a team; its
  channels in `topics`, a JSON string), `streamofnotifications` (`48:notifications`). The rest: `createdat`,
  `isCreator`, `lastjoinat`, `lastSequenceId`, `rosterVersion` (a number), `version` (a string),
  `originalThreadId`, `gapDetectionEnabled`. Booleans in `properties` and `threadProperties` are mostly strings.
  A group chat in the list wasn't seen (the lab had none when the probe ran).

## Message pages (ab-chat-full, b)

Opening the chat, b's client made two reads. Fixture: `tests/fixtures/chatsvc/messages.json`.

1. **Sync**: `GET v1/users/ME/conversations/<chat>/messages?view=msnp24Equivalent|supportsMessageProperties&
   pageSize=200&syncState=<opaque>`, from a sync state the client had kept from an earlier session. It answered with
   the messages changed since, newest first, and `_metadata` `{lastCompleteSegmentStartTime, lastCompleteSegmentEndTime,
   syncState}`, where `syncState` is the full URL of the next sync read (`…/messages?startTime=<ms>&syncState=
   <opaque>&pageSize=200&view=msnp24Equivalent`). The top level also has `tenantId`.
2. **Around a time**: `GET v1/users/ME/conversations/<chat>/messages/epochTimeStamp/<ms>?pageSize=40&direction=
   BIDIRECTIONAL&includeMetadata=true&locationType=Primary`, the time being the thread's `lastjoinat`. It answered with
   every message of the (small) chat, newest first, `_metadata` with both segment times 0 and no link.

Further pages: the bundle pages a message list through `_metadata.backwardLink` (older) and `forwardLink` (newer), full
URLs. Neither was seen: the lab chat fits in one page. A first read without a kept sync state wasn't seen either.

## A message as read

Every message (`type` `Message`) has `id` (its arrival time in ms, as a string), `sequenceId` (1, 2, … in the chat),
`version` (ms, a string), `conversationid`, `conversationLink` (a URL), `messagetype`, `content`, `from` (a URL ending
`…/contacts/<MRI>`: take the sender's MRI from its last segment), `composetime` and `originalarrivaltime` (ISO times
with seven fraction digits). A user's message adds `clientmessageid` (the sender's, kept), `contenttype` `Text`,
`imdisplayname` (the name the sender's client gave: "Unknown User" in the lab, since it couldn't resolve profiles),
`fromDisplayNameInToken` (the name from the sender's token), `amsreferences` (`[]`) and `properties`:
`mentions`, `cards`, `links` and `files` as JSON strings (`"[]"`), `formatVariant` `TEAMS`, `languageStamp`.

- **The chat's first event**: `messagetype` `ThreadActivity/AddMember`, `from` the thread's own contact URL, no
  `clientmessageid`, and an XML `content`: `<addmember>` with `eventtime`, `initiator`, `rosterVersion` and per member a
  `target` and `detailedtargetinfo`.
- **Edited** (fixture `edit.json`): same `id` and `clientmessageid`, the new `content`, `version` raised and
  `properties.edittime` (ms, set by the service: it differs from the one the edit sent). No `skypeeditedid` on an
  edited message. The edit was a `PUT v1/users/ME/conversations/<chat>/messages/<id>` of the whole message again (the
  post's fields with `id` the message's, `from` `worker/<MRI>`, `conversationLink` `conversation/<chat>;messageid=<id>`,
  `version` the edit's ISO time and `properties.edittime`), answered 200 with an empty body.
- **Deleted** (fixture `delete.json`): `DELETE v1/users/ME/conversations/<chat>/messages/<id>?behavior=softDelete`, no
  body, answered 200 with the text `null`. Read back: same `id` and `clientmessageid`, `content` empty, `properties`
  only `{deletetime}`, `version` the delete's time, no `amsreferences`, and **`skypeeditedid` set to the message's
  `clientmessageid`**: the one place that key showed up.
- **Reactions** (fixture `reaction.json`): `PUT v1/users/ME/conversations/<chat>/messages/<id>/properties?name=emotions`
  with `{"emotions": {"key": "like", "value": <ms>}}`, answered 200 with the text `200`; taken back with `DELETE` on the
  same URL and `{"emotions": {"key": "like"}}`, answered 200 with the text `null`. Read back, `properties.emotions` is
  a list of `{key, users}`, one per reaction key; a reaction taken back leaves its key with `users: []`. A reaction
  change raises the message's `version`, and a message that had one carries `annotationsSummary`: `{}` once none is
  left, `{"emotions": {"like": 1}}` (a count per key) while one stands. A standing reaction lists each user as
  `{"mri", "time", "value"}`: `time` the service's (ms), `value` the one the client sent, as a string.
- **Quoted reply** (fixture `reply.json`): an ordinary post whose `content` starts with
  `<blockquote itemscope itemtype="http://schema.skype.com/Reply" itemid="<quoted id>">`, holding
  `<strong itemprop="mri" itemid="<sender MRI>">name</strong>`, `<span itemprop="time" itemid="<quoted id>">` and
  `<p itemprop="preview">` with the quoted text, followed by the reply's own paragraphs. `properties.qtdMsgs` names the
  quoted message: sent as a JSON string `[{"messageId": "<id>", "sender": "<MRI>", "time": <ms>}]`, read back as a list
  of objects (`messageId` a number, plus `message` null, `validationResult` `Valid`, `sharedRefId`, `replyChainId`) and
  `hasValidMsgReferences` true. A 1:1 chat has no reply chains: a quote is all a "reply" is.
- **Mentions** (fixture `mention.json`): **not captured** (below).
- **Received attachments**: not seen; every message had `amsreferences` `[]` and `files` `"[]"`. Task 20 decides what
  to do about attachments; their shapes need a capture with one.

## A message as sent (ab-chat-full, a)

`POST v1/users/ME/conversations/<chat>/messages` as in fence-targets.md: the message's fields with `id` `-1`,
`version` `"0"`, `clientmessageid` a random numeric string (18 and 19 digits seen), `properties` with the empty JSON-string lists and
`formatVariant`. It answers 201 with `{"OriginalArrivalTime": <ms>}`, and the new message's `id` is that time. Typing is
the same post with `messagetype` `Control/Typing`, also answered with an arrival time.

## Mentions (bundle only, not captured)

No mention could be made: under the fence the mention picker offered nobody but a message extension, since the profile
lookups behind it are denied (`docs/backlog/mention-not-captured-under-the-fence.md`). What the bundle builds and reads:

- in `content`, the mentioned name in `<span itemtype="http://schema.skype.com/Mention" itemscope itemid="<n>">`, `<n>`
  the mention's index in the message (the client's reader accepts `itemid` before or after `itemtype`);
- `properties.mentions`, a JSON string of a list with one entry per mention: `{"@type":
  "http://schema.skype.com/Mention", "itemid": "<n>", "mri": "<MRI>", "mentionType": "person", "displayName": "<name>"}`,
  with `appId` for a bot. Other `mentionType`s the bundle names: `bot`, `everyone`, `tag`, `channel`, `team`,
  `community`, `nearby`, `share-contact`. Reading, the client takes `mentions` as a string or a list.

The fence's mapping takes the `mri` of each entry as an added participant of the message's (or edit's) target, so a
mention of anyone outside the lab is denied like adding them to the chat; any other form of `mentions` is unknown.

## Read horizons

- The account's own horizon is the conversation property `consumptionhorizon`, `<arrival id>;<ms>;<clientmessageid>`:
  the last message read (its id and its `clientmessageid`) and when the horizon was set. a's client set it with `PUT
  v1/users/ME/conversations/<chat>/properties?name=consumptionhorizon` (`{"consumptionhorizon": "…"}`, answered 200
  with an empty body) after its own posts; the conversation list carries it in `properties`.
- `GET v1/threads/<chat>/consumptionhorizons` answers `{id, version, consumptionhorizons: []}`: for the lab chat the
  list was empty both times it was read (the members' horizons that others may see, when read receipts are on).
- A page sends its receipt only while it is in use. In the first run b's page sent none while it showed a's messages;
  in the confirming run it sent none within 30 s of showing them and focusing its compose box, and sent one during its
  reaction, once the pointer was over a message. a's page sent its receipts as it typed and posted.

## Seen beside it

- Own properties the client puts and the fence denies (unknown, not on the own-state list): `messageDensity` (hundreds
  of retries a run), `favorites`, `locale`, `firstLoginInformation`, and `readReceiptsEnabled`, new in this run.
- Whenever an editor opens (the compose box at the mention picker, an edit box), the client asks its message
  extensions' bots for their commands: `POST v1/agents/28:<id>/invoke` with `name` `composeExtension/fetchCommands`, to
  two bots, denied as unknown (a bot invoke reaches the bot's own service). The reviewed expected-deny list
  (`crates/proto-lab/src/scenario/expected.rs`) now expects them, so a run isn't failed by them; they stay denied.
- Once b's reaction reached a's activity feed, a's client marked the feed item read: `PUT
  v1/users/ME/conversations/48%3Anotifications/messages/<id>/properties?name=isread` with `{"isread": "true"}`, retried
  nine times while denied. Expected by the same list (the account's own feed), still denied.
- `GET v1/users/ME/properties` answers 404, and so does `v1/users/ME/conversations/48:calllogs` for the lab accounts.
