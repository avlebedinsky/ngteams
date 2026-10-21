# The chat service: conversations, messages and what happens to them

Status: 2026-10-21, M1 Task 7; images (sending, denied) 2026-10-21, M1 Task 20; the shapes the UI needs (every quick
reaction, typing, lines and links, a group mention) 2026-10-21, M2 Task 10. Written from what the lab saw, in our
own words, plus the web bundle where nothing was captured (marked so). The chat service's base, credential, ids, member
reads and the fence's mapping of writes are in [fence-targets.md](fence-targets.md); its token in [auth.md](auth.md).

Sources (all outside the repository, under the lab's data dir; fixtures are written from them by hand, synthetic):

- **probe**: Task 3's read-only `proto-lab probe-chat` of accounts a and b (`reference/chat/probe-conversations-a.jsonl`,
  `…-b.jsonl`): the conversation list.
- **ab-chat-full**: `proto-lab run ab-chat-full` of 2026-10-20 (accounts a and b, the a–b 1:1 chat, captures
  `captures/ab-chat-full/20261020T194153Z` for a and `…194156Z` for b, reference copies
  `reference/chat/ab-chat-full-a.jsonl`, `…-b.jsonl`), and the confirming run recorded in the M1 plan's Task 7
  (`captures/ab-chat-full/20261020T205155Z` and `…205156Z`, read locally): a sent, edited, reacted and took the
  reaction back, replied with a quote, reacted to the reply, sent and deleted a message; b opened the chat (reading
  its history) and reacted.
- **mention**: `proto-lab run ab-chat-full` of 2026-10-21, once the fence let the mention picker's profile lookups of
  lab accounts through (fence-targets.md, "Reads by POST"): a mentioned b, picked from the picker (captures
  `captures/ab-chat-full/20261020T223250Z` for a and `…223251Z` for b, reference copies
  `reference/chat/ab-chat-full-mention-a.jsonl`, `…-b.jsonl`).
- **ab-image**: `proto-lab run ab-image` of 2026-10-21 (M1 Task 20; accounts a and b, the a–b chat, captures
  `captures/ab-image/20261020T225251Z` and `20261021T010819Z` for a, read locally, nothing exported): a pasted a
  made-up image and sent it with every write to an AMS object denied ([Images](#images-sending-denied-ab-image)).
- **chat-shapes**: three `proto-lab run chat-shapes` runs of 2026-10-21 (M2 Task 10; accounts a and b under the
  messaging classifier, so b's push channel was up; captures `captures/chat-shapes/20261021T165600Z`, `…170305Z` and
  `…171041Z` for a, `…165601Z`, `…170308Z` and `…171041Z-2` for b, read locally, nothing exported): a set and took back
  each quick reaction, typed while b's open chat recorded its typing indicator, sent a message of three lines with a
  link, and mentioned b in the lab's group chat ([The shapes the UI needs](#the-shapes-the-ui-needs-chat-shapes)).
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
`imdisplayname` (the name the sender's client gave: "Unknown User" in the lab's runs of 2026-10-20, when the fence denied
every profile lookup; the account's display name since the lookups of lab accounts pass),
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
- **Mentions** (fixture `mention.json`): [below](#mentions-mention).
- **Received attachments**: not seen; every message had `amsreferences` `[]` and `files` `"[]"`. Sending images is
  deferred out of M1 ([Images](#images-sending-denied-ab-image)), so no message with one has been read either.

## A message as sent (ab-chat-full, a)

`POST v1/users/ME/conversations/<chat>/messages` as in fence-targets.md: the message's fields with `id` `-1`,
`version` `"0"`, `clientmessageid` a random numeric string (18 and 19 digits seen), `properties` with the empty JSON-string lists and
`formatVariant`. It answers 201 with `{"OriginalArrivalTime": <ms>}`, and the new message's `id` is that time. Typing is
the same post with `messagetype` `Control/Typing`, also answered with an arrival time.

## Images: sending, denied (ab-image)

What the web client does to send an image, with every write to an AMS object force-denied by the scenario (M1 Task 20).
a typed a caption, pasted a made-up 48×48 PNG into the compose box of the a–b chat (a clipboard paste the editor took;
it showed the image inline) and pressed Enter.

- **The object's creation** went to the **async gateway**, not to the GTM `ams` host (`us-api.asm.skype.com`, which only
  saw its token exchange): `POST https://us-prod.asyncgw.teams.microsoft.com/v1/objects/`, after an `OPTIONS`
  preflight to the same URL. Its credential is the chat service's: `Authorization: Bearer` with the
  `https://ic3.teams.office.com` token and `X-MS-Migration: True`; also `x-ams-post-sharing-mode: Inline`,
  `x-ms-client-version`, `ms-cv` and `content-type: application/json`. The body, exactly:
  `{"type": "pish/image", "permissions": {"<the a–b chat id>": ["read"]}, "sharingMode": "Inline", "filename":
  "MicrosoftTeams-image.png"}`: the conversation that may read the object is the one key of `permissions`, and the
  client renamed the pasted file.
- **Denied**, the client tried the creation again with a growing pause (seven times over about five minutes in the
  run of 22:52Z), then kept the message in the chat as being sent: a bubble with the image and a pending mark. It
  **sent no upload** (that needs the creation's answer) and **no message post**, neither the caption nor
  `amsreferences`. While the image was pending the page asked before being left (a `beforeunload` dialog), which in a headless browser shows to nobody and
  holds any navigation; the scenario's navigations now accept it. After the page was left and loaded again the image was
  gone and its creation wasn't tried again (the run of 01:08Z, 30 s watched).
- **Bundle only** (never seen): the creation's answer is read as `{id}`; the content is then put with
  `PUT v1/objects/<id>/content/imgpsh` (`application/octet-stream`; a ranged, resumable form with `Content-Range` too),
  which names the object alone; `POST v1/objects/<id>/forward` with `{"permissions": {"<another conversation>":
  ["read"]}}` lets another conversation read an existing object; `GET v1/objects/<id>/` reads the object (its answer's
  shape unknown); the views are `v1/objects/<id>/views/<name>` (`imgo`, `imgpsh_fullsize_anim`, …) and their
  `…/status`. The creation's `permissions` may carry further keys (`@:composer`), and its `type` is `sharing/file` or
  `sharing/videototranscode` for other media.

Why sending stays out of M1 (decided with Codex, `docs/backlog/ams-image-sending.md`): the creation's target can be read
from its body, but the upload names only the object, so the fence could bind it to a conversation only through a
registry of what it permitted (which the fence must not stand in) or an authoritative read of the object's current
readers, which no capture has shown, and which `forward` could change between the read and the upload. Every answer past
the creation is uncaptured too. The fence maps none of it: AMS objects are `Unknown` and denied
([fence-targets.md](fence-targets.md)).

## Mentions (mention)

Typing `@` in the compose box opens the mention picker (`AutocompletePopup-Mentions`); in the 1:1 chat its highlighted
suggestion was the chat's other member, by name, once the client could read that member's profile (the middle tier's
`users/fetchShortProfile` and `users/fetch`; Substrate's search stayed denied and wasn't needed). Typing the name after
`@` kept it highlighted, Enter put the mention in, and the post was an ordinary message post. Fixture:
`tests/fixtures/chatsvc/mention.json`.

- **As sent**: `content` holds the mention as `<readonly class="skipProofing" itemtype="http://schema.skype.com/Mention"
  spellcheck="false"><span itemtype="http://schema.skype.com/Mention" itemscope itemid="0">name</span></readonly>`
  followed by `&nbsp;` and the text, all in one `<p>`; `itemid` is the mention's index in the message.
  `properties.mentions` is a JSON string of a list with one entry per mention: `{"@type":
  "http://schema.skype.com/Mention", "itemid": "0", "mri": "8:orgid:<oid>", "mentionType": "person", "displayName":
  "<name>"}`, `itemid` a string. The rest of the post is as any message (above), answered 201.
- **As read from the chat** (b's message page): the `<readonly>` wrapper is gone, the span is
  `<span itemtype="http://schema.skype.com/Mention" itemscope="" itemid="0">name</span>`, and `properties.mentions` is
  still a JSON string, its entries with the same keys but `itemid` a number (`0`).
- **The mentioned account's own streams**: b's `48:mentions` stream (`GET v1/users/ME/conversations/48%3Amentions/
  messages?view=msnp24Equivalent|supportsMessageProperties&pageSize=200&startTime=1`) got an entry of its own: a message
  with its own `id`, `from` b's own contact URL, `imdisplayname` empty, `clumpId` the chat's id, `secondaryReferenceId`
  `T_<chat id>_M_<message id>`, the content without the `<p>` and with a plain space, `properties.mentions` without
  `@type`, and `properties.activity` (`activityType` `mentions`, `activitySubtype` `mention`, the source chat, message
  and sender, the target user, a text preview, `activityContext`), `views` (a JSON string naming the `ActivityFeed` view
  on `48:notifications` and the `Mentions` view on `48:mentions`), `messageUpdatePolicyValue`, `issaved`,
  `s2spartnername` `skypespaces`. The same activity reached b's `48:notifications` stream. b's client then marked the
  `48:mentions` item read with `PUT …/48%3Amentions/messages/<id>/properties?name=isread` and `{"isread":"true"}`, as it
  does for its activity feed (below); the fence denied it.
- CSA's update read (`GET api/csa/<region>/api/v3/teams/users/me/updates`) carried the message too, in its own
  camel-cased form (`messageType`, `imDisplayName`, `containerId`), with the span but no `properties`.

The bundle names more `mentionType`s than `person` (`bot` with an `appId`, `everyone`, `tag`, `channel`, `team`,
`community`, `nearby`, `share-contact`) and a reader that accepts `mentions` as a string or a list; none of those was
seen.

The fence's mapping takes the `mri` of each entry as an added participant of the message's (or edit's) target, so a
mention of anyone outside the lab is denied like adding them to the chat; any other form of `mentions`, an entry
without a string `mri` (an `everyone` or `tag` mention among them), or the key in another case, is unknown. The
captured form is the one it reads.

## The native client's writes (M1 Task 12)

`teams_proto` sends each chat write in the form above, key for key (its tests compare the bodies with `mention.json`,
`reply.json`, `edit.json`, `delete.json`, `reaction.json` and the `writes.json` cases): a post with every key the web
client sent (`id` `-1`, `version` `"0"`, the `conversationLink` `blah/<chat>` it sends, the empty JSON-string lists,
`onbehalfof` and `policyViolation` null, `crossPostChannels` `[]`), the edit as the whole message again, the soft
delete, the reaction and its take-back, the read horizon, typing (`Control/Typing` only: the web client never sent
typing stopped, [Typing](#typing-chat-shapes)) and a new chat (`fixedRoster` and `uniquerosterthread` for a 1:1 chat, neither for a group). A message's
text is a typed rich-text value with the MRI of each mention, from which `content`, `properties.mentions` and the
write's target are all built; raw HTML can't be sent. Times go out as the web client writes them
(`2026-01-01T00:00:00.000Z`), an edit's `version` and `properties.edittime` being one instant.

**A mention is sent in the captured form** ([above](#mentions-mention)): the span inside the `<readonly>` element,
built in one place only, `mention_span` and `mention_entry` in `crates/teams-proto/src/chat/compose.rs` (written from
the bundle in Task 12, brought to the capture when the capture reached the M1 branch), and a no-break space written
`&nbsp;` as the web client writes the one after a mention. Its tests compare the post with `mention.json` and read the
mention back from both the post and the chat's copy.

## The shapes the UI needs (chat-shapes)

What M2's chat view and compose box need beyond M1's captures. Every write went to the a–b chat or the lab's group
chat, mapped and judged by the fence with members read live; the three runs exited 0 with no breach.

### Reactions (chat-shapes)

Fixture: `tests/fixtures/chatsvc/reactions.json`.

- The hover bar a message shows under the pointer offered four quick reactions, the same in every run: **`like`,
  `heart`, `laugh`, `surprised`** (`data-tid` `message-actions-<key>`, marked `data-inp="message-hover-reactions"`).
  The bundle takes the list from the account's settings or the service's config, with that list as its fallback, and
  turns `like` into `yes-tone<n>` for an account that chose a skin tone (bundle only; the lab accounts chose none).
- Each is set and taken back exactly as the like (`PUT`/`DELETE …/messages/<id>/properties?name=emotions`, the key in
  the body, answered 200 with the texts `200` and `null`). The keys stand side by side: setting one never took another
  back, so one account can hold all four on one message.
- Read back (b's `MessageUpdate` deliveries, each carrying the whole message), `properties.emotions` lists the keys in
  the order they were first set, each with its users; a key taken back stays with `users: []`, and
  `annotationsSummary.emotions` counts only the keys that still have a user (`{}` once none has). Each change raised the
  message's `version`.

### Typing (chat-shapes)

- **What a sends**: the post M1 saw and the native client sends, key for key (`{"content": "", "contenttype":
  "Application/Message", "messagetype": "Control/Typing"}`), answered 201. **No `Control/ClearTyping`** went out in
  any run: not when a stopped typing, not when it emptied its compose box (15–18 s watched after), not when it sent.
- **When**: sparse. With a pressing a key twice a second for 45 s (keys pressed in one run, text inserted in
  another), no typing post went out while it typed; one went 0.2–1.6 s after the presses stopped, and one or two
  within a few seconds of a's sending a message (3.2 s and 7.2 s after it in one run). The closest two posts were
  4.0 s apart, the next 16.9 s. The bundle sends it through a throttle on the editor's changes, leading edge, at most one
  per `typingIndicator.notificationFrequency` (6 000 ms unless the service's config says otherwise), only in a chat of
  2 to 20 members (`maxRosterSizeInGroupChatChatServiceLimit`, default 20), never while a message is being edited and
  never in a reply chain (bundle only). Why the steady presses of the headless run brought none is not known.
- **What b shows**: b's indicator (`data-tid="typing-indicator"`) showed within 0.3 s of each typing delivery and went
  **about 25 s after the last one** (24.7–24.9 s after its delivery; 25.6–26.3 s after a's post) while a's text was still
  in the box, in all three runs; it went at once when a message from a arrived (1.0 and 1.2 s after a's post, as it was
  delivered). A typing delivery is a `NewMessage` of `messagetype` `Control/Typing` with
  `X-Trouter-Delivery-Control: …; resilience=nostorenoloss` ([trouter.md](trouter.md)).
- For the native client: send `Control/Typing` on input at most once every 6 s and nothing when typing stops; show
  another's typing for 25 s after their last `Control/Typing`, or until a message from them arrives.

### Lines and links (chat-shapes)

Fixture: `tests/fixtures/chatsvc/lines.json`.

- Shift+Enter in the compose box starts a **new paragraph**: three lines went out as `<p>…</p><p>…</p><p>…</p>`, no
  `<br>`. Read back, the service had joined the paragraphs with CRLF (`</p>\r\n<p>`); that is markup, not text, and
  `teams-proto`'s reader drops whitespace beside a paragraph or a quote (the quoted reply's CRLF after its quote too).
- A link typed and followed by a space became an anchor: `<a href="<url>" rel="noreferrer noopener" title="<url>"
  target="_blank"><url></a>`, and `properties.links` (a JSON string) held `[{"@type":
  "http://schema.skype.com/HyperLink", "itemid": "0", "url": "<url>", "previewenabled": true, "preview":
  {"previewurl": ""}}]`. Inserted with its text in one go, the link stayed plain text and `links` stayed `"[]"`.
- With a link in the compose box the client asked the async gateway for its preview, `GET
  /urlp/v1/url/info?url=<url>` (a read: the gateway fetched the URL and answered its title, status and favicon;
  [endpoints.md](endpoints.md)). The native client doesn't ask for previews.

### A mention in the group chat (chat-shapes)

Fixture: `tests/fixtures/chatsvc/mention-group.json`.

- The post is the 1:1 chat's ([Mentions](#mentions-mention)) to the group chat, key for key; the fence took b as its
  added participant and let it out, b being a lab account. The picker offered b by name in the group chat as it does in
  the 1:1 chat.
- b's delivery of the message carries `mentiontypes` `["person"]` beside the resource (`[]` on other messages); the
  message reads back as the 1:1 mention does (no `<readonly>`, `itemid` a number).
- b's `48:notifications` stream got an entry of `activityType` **`mentionInChat`**, subtype `person` (the activity feed's
  form for a group chat), and its `48:mentions` stream one of `activityType` `mentions`, reason and subtype `mention`,
  each pointing at the message (`T_<chat>_M_<message>`) as in the 1:1 chat.

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
- Whenever an editor opens (the compose box, the mention picker, an edit box), the client asks its message
  extensions' bots for their commands: `POST v1/agents/28:<id>/invoke` with `name` `composeExtension/fetchCommands`, to
  two bots, denied as unknown (a bot invoke reaches the bot's own service). The reviewed expected-deny list
  (`crates/proto-lab/src/scenario/expected.rs`) now expects them, so a run isn't failed by them; they stay denied.
- Once b's reaction reached a's activity feed, a's client marked the feed item read: `PUT
  v1/users/ME/conversations/48%3Anotifications/messages/<id>/properties?name=isread` with `{"isread": "true"}`, retried
  nine times while denied. Expected by the same list (the account's own feed), still denied.
- `GET v1/users/ME/properties` answers 404, and so does `v1/users/ME/conversations/48:calllogs` for the lab accounts.
