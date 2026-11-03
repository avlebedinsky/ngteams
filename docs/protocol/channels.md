# Teams and channels

Status: 2026-10-22, M2 Task 16. Written from `proto-lab probe teams a`, run live once as lab account a (its own
`chatsvcagg` and `ic3` tokens, `GET`s only, under the live lock), which printed statuses, header names, content types,
sizes, the answers' key paths with their JSON types, counts, and two protocol enums' plain values (a thread's
`threadType`, a message's `messagetype`), never a name, a text or an id, and kept nothing; and from the web client's
captures of the same reads (`ab-chat`, `abc-chat`, `load`: method, path pattern, query names and flag values, header
names, statuses and key shapes only). The lab accounts are members of two teams of the tenant that people outside the
lab are in too, so nothing of them is written here. The native client's read is in `crates/teams-proto/src/read.rs`
(`CsaTeams`), the transport's check in `crates/teams-proto/src/http.rs` (`Service::Csa`), its use in
`crates/teams-core/src/teams.rs`, and the views in `crates/teams-ui/src/{teams_list.rs,channel_view.rs}`.

## Where the web client gets them

The web client lists the account's teams and channels from CSA, the chat service aggregator, at the region's GTM
`chatSvcAggAfd` (`https://<AFD host>/api/csa/<region>`, `amer` for the lab):

- `GET api/v3/teams/users/me` with `isPrefetch=false`, `enableMembershipSummary=true`,
  `supportsAdditionalSystemGeneratedFolders=true`, `supportsSliceItems=true`, `enableEngageCommunities=false`, in
  that order, in every capture of the full read: the account's teams with their channels, its chats and more;
- `GET api/v3/teams/users/me/updates` with the same flags (and `migratePinnedToFavorites` in some), what changed since a
  sync token (`x-ms-synctoken`);
- a channel's posts through CSA too: `GET api/v1/containers/<channel>/posts` (`modality`, `pageSize`, `teamId`,
  `includeRcMetadata`, `filterSystemMessage`, `shouldIncludeSharedToL1Rc`, or `threadedPostsOnly`), answered 207 with
  `posts` (each a post with its `replies.messages` and `totalCount`), `hasMoreForward`, `hasMoreBackward`; and
  `…/teams/<team>/channels/<channel>/pins`, `…/systemmessages`.

Its requests carry many `x-ms-…` headers (`x-ms-client-type`, `x-ms-client-version`, `x-ms-partition`,
`x-ms-region`, `x-ms-session-id`, `x-ms-request-priority`, …) and the account's token for
`https://chatsvcagg.teams.microsoft.com` ([auth.md](auth.md#audience--service-table)).

## What the probe read

### The token

The native client (`1fec8e78-…`) gets a `chatsvcagg` token from the account's refresh token like any other audience:
`aud https://chatsvcagg.teams.microsoft.com`, scope `user_impersonation`. CSA took it with nothing else: the probe sent
`Authorization: Bearer <token>` and `Accept: application/json`, no `x-ms-…` header.

### CSA's teams read

`GET <chatSvcAggAfd>/api/v3/teams/users/me` with the web client's query, as above: **200**, `application/json`, about
32 KB; header names `cache-control`, `content-length`, `content-type`, `date`, `pragma`, `request-context`,
`timing-allow-origin`, `x-cache`, `x-content-type-options`, `x-msedge-ref`.

- Top level: `teams`, `chats`, `users`, `privateFeeds`, `engageCommunities` (arrays), `conversationFolders` (an object:
  folders, their order, slices metadata) and `metadata` (`syncToken`, `forwardSyncToken` `null`, `isPartialData`,
  `hasMoreChats`, `coldBootTime`).
- A team: `id` (a `19:…@thread.tacv2` id, its general channel's), `displayName`, `description`, `channels`,
  `isArchived`, `isDeleted`, `isFavorite`, `isCollapsed`, `isTenantWide`, `isGeneralChannelFavorite` and other flags,
  `memberRole`/`memberRoleString`, `membershipSummary` (counts), `rosterVersion` and `membershipVersion` (numbers),
  `threadVersion` and `threadSchemaVersion` (strings), `smtpAddress`, `tenantId`, `teamSettings`, `teamGuestSettings`,
  `teamSiteInformation`, `teamStatus`, `extensionDefinition`, … .
- A channel: `id`, `displayName`, `description`, `parentTeamId`, `threadType`, `threadSubType`, `channelType` (a
  number), `modality`, `isGeneral`, `isArchived`, `isDeleted`, `isMember`, `isFavorite`, `isPinned`, `isShared`,
  `isMuted` and other flags, `lastMessage` (an object), `version`, `threadVersion`, `membershipVersion` (numbers),
  `memberRole`, `creator`, `creationTime`, `lastJoinAt`, `tabs`, `connectorProfiles`, `defaultFileSettings`, … .
- Counts: 2 teams, 10 channels; each team's general channel is the one whose id is the team's, with `isGeneral`
  `true` (2); one team and one channel archived; none deleted.

### The channels on the chat service

A channel is a conversation of the chat service, read with the same reads as a chat:

- **The conversation list** (`v1/users/ME/conversations`) held 2 of the 10 channels: the teams' general channels
  (`threadType` `space`, [chat.md](chat.md#the-conversation-list-probe)). A team's other channels are in no list, so
  no catch-up reaches them; they are read by their id.
- **A channel's thread** (`GET v1/threads/<channel>?view=msnp24Equivalent`): 200, `threadType` `space` for a general
  channel and **`topic`** for another (no `productThreadType` in either), one entry in `members`, `rosterSummary`
  counts, and many `properties` (`spaceId`, `topic`, `topicThreadTopic`, `groupId`, `sharepointChannelDocsFolder`, …).
- **Its messages** (`GET v1/users/ME/conversations/<channel>/messages?view=msnp24Equivalent|supportsMessageProperties&
  pageSize=200`): 200, `messages`, `tenantId` and `_metadata` (`lastCompleteSegmentStartTime`,
  `lastCompleteSegmentEndTime`, `syncState`), as a chat's page. The general channel's page held 48 messages
  (`RichText/Html` 14, `Text` 4, and thread events: `ThreadActivity/AddMember` 12, `DeleteMember` 12, `RoleUpdate` 2,
  `TopicTopicUpdated` 2, `ModalityUpdate` 1, `SpaceThreadChannelAdded` 1); a standard channel's 28 (`RichText/Html` 21,
  `Text` 5, `ModalityUpdate` 1, `UpdateFavDefault` 1). A message has a chat message's keys
  ([chat.md](chat.md#a-message-as-read)) and `s2spartnername`, `fromGivenNameInToken`, `fromFamilyNameInToken`,
  `origincontextid`, and on some `rootMessageId` (a string); `properties` adds `announceViaEmail`, `atp`, `blurHash`,
  `deltaEmotions`, `pinned`, `skipfanouttobots`.
- **Reply chains**: every message's `conversationid` was the channel's, and every message's `conversationLink` ended
  in `;messageid=<post>`: the post a reply belongs to, a post's own id for a post (an event too). No message had
  `properties.parentmessageid`. The channel's page holds posts and replies together, newest first.
- **One post's chain** (`GET v1/users/ME/conversations/<channel>;messageid=<post>/messages`, the same query): 200, the
  post and its replies (24 messages for one post, 1 for an event), each with that link.

## What the native client reads

- **The teams**: `GET <CSA base>/api/v3/teams/users/me` with the web client's query exactly, the account's
  `chatsvcagg` token and `Accept: application/json`, nothing else (`Service::Csa`, `CsaTeams`). The transport takes a
  CSA base only of the GTM's form (`https://<AFD host>/api/csa/<region>`, given by `HttpTransport::with_csa`) and
  refuses any other CSA endpoint or query (the update read, the posts read, discover, pins, …) and anything but a
  `GET` before it asks for a token. Of the answer it reads only the teams: a team's and a channel's `id`,
  `displayName`, `isDeleted` (left out), `isArchived` (a team's) and `isGeneral` (or the channel whose id is the
  team's), a channel's `channelType`, and what a new channel needs of a team (`teamSiteInformation.groupId`,
  `smtpAddress`, `memberRoleString`, `teamSettings`, `teamGuestSettings`: [Creating a channel](#creating-a-channel)); an entry without a string id and name is left out; the general channel first. The answer is taken when
  `metadata.isPartialData` is `false`, as in every lab capture, or when it is `true` with `metadata.hasMoreChats`
  `true`: a personal build's account with a few hundred chats was answered so every time (12 teams, its `chats`
  264 of them, `conversationFolders` with `isPartialResponse` `false`), and the web bundle ties the flag to the chats
  alone (`partialDataStartupChatsLimit`, and `shouldSkipGroupChatSyncBasedOnIsPartialData`: the group chats are read
  from the chat service after a partial answer), so its teams and folders are whole. One that says it is partial
  otherwise, or doesn't say (no `metadata`, no flag, a flag that isn't a boolean), is refused as malformed, since the
  store replaces its list with what a read gives; the stored list and its read time stay, and the read is tried again
  after the engine's retry wait. `chats`, `users` and the rest aren't read: the chat list stays the chat service's;
  of `conversationFolders` only the favourites are ([chat.md](chat.md#favorites-empty-chats-and-the-chat-list)).
  `teams-core` reads it in the background when a view asks and the stored list is missing or older than an hour
  (`TEAMS_MAX_AGE`), keeps it in the store whole and tells a change as `Event::Teams`. It reads it again at once when
  a read or a delivery brings a team's thread no stored team names (the account was added to a team; once per thread a
  run), or a team's activity is delivered on its thread (`ThreadActivity/…`: a channel made comes to everyone on the
  team as `SpaceThreadChannelAdded`, seen 2026-11-03): without it a team the account was just added to, or its new
  channel, stayed missing for up to an hour.
- **A channel's posts**: the chat service's message reads, as a chat's (`ChatReads::messages` and its links), on demand
  when the channel is opened (`EngineRequests::sync_conversation`), older pages on "Load older posts". A channel the
  store doesn't know is read as a thread first (`topic` and `space` are both a channel, read-only). Each stored
  message's post is kept beside it from its `conversationLink` (`Message::reply_root`, only when the link names that
  channel and the post is digits); a message without one is shown as a post of its own. The store's schema 9 started
  the general channels' syncs again, so their messages stored before get their posts.
- **Not used**: one post's chain read (the channel's page holds the replies), the thread's members (a channel is
  read-only: no member is stored for it), CSA's posts read and its update read.

## Creating a channel

From `proto-lab run channel-create` (2026-11-02): account a in the web client's "Create a channel" dialog of a team of
the tenant, every write that could make a channel held back by a forced deny, so nothing was ever made; the answers below
are the web bundle's (`26260.1717.5229.7049`) reading of them, since no lab team can be written to
(`docs/backlog/channel-writes-need-a-lab-only-team.md`). The native client's writes are
`crates/teams-proto/src/write/channel.rs`, its reads `channel_read.rs`, the engine's part `teams-core`'s `channels`, the
dialog `teams-ui`'s `channel_dialog`.

**The team's "..." menu** offered a member of a team "Hide all channels", "Add channel", "Manage team", "Manage tags" and
"Copy link"; "Add channel" isn't there for an archived team. The web client decides as `CsaTeams` reads it: an owner
(`memberRoleString` `admin`) always, a member by `teamSettings.createTopic`, a guest by `teamGuestSettings.createTopic`;
a private channel for an owner, or a member with `teamSettings.createTopic` and
`teamSettings.isPrivateChannelCreationEnabled`. The channel types offered were Standard and Private
(`isSharedChannelCreationEnabled` `false`, and shared channels aren't offered here).

**The dialog's rules** (the bundle's, its words in `channel_dialog.rs`): a name of at most 50 characters, trimmed;
refused when empty, starting with `_` or `.`, ending with `.`, holding any of `~ # % & * { } / \ : < > ? + | ' "` or
`..`, one of the reserved words (`forms`, `.lock`, `con`, `prn`, `aux`, `nul`, `_vti_`, `desktop.ini`, `com1`-`com8`,
`lpt1`-`lpt9`), `General` in any case, a name the team's channels have in any case, or a link. A description of at most
1024 characters without a link. A problem shows once the name or the description was touched; Create is on when both
read, a type is chosen and something was touched. Threads is chosen at first. An owner choosing Standard also gets
"Automatically show this channel in everyone's channel list". The Layout cards name `chatModalityType`.

**A standard channel** goes on the middle tier, its token and headers its own:

```
POST <middleTier>/beta/teams/<team thread>/channels
{"displayName": …, "description": …, "groupId": <team group>, "channelType": "Standard",
 "chatModalityType": "Conversational" | "PostReply"}
```

**Seen live** (`channel-create` writing on the lab's own team, 2026-11-03), for Threads and Posts alike: 201 with
`{"value": {"objectId": <channel thread>, "createdDateTime", "displayName", "description", "groupId": <team group>,
"channelState": "Active", "isFavoriteByDefault": false, "chatModalityType"}, "type": …}`.

**A private channel** goes on the teams' provisioning service, the GTM `teamsAndChannelsProvisioningService`
(`https://<AFD host>/fabric/<region>/templates/api`), with a token for `https://templates.teams.microsoft.com` (the
native client gets one, an opaque token) and `x-anchormailbox: <the team's smtpAddress>`:

```
POST <provisioning>/teams/<team group>/channels?forceSync=false
{"spaceThreadId": <team thread>, "displayName": …, "description": …, "channelType": "Private",
 "sharedWithParentTeam": true, "sessionKey": <uuid>, "chatModalityType": …}
```

Before it, as the type is picked, the web client sent `POST <provisioning>/teams/<team group>/channelsWarmup` with no body
and the same key in `x-routingparameter-sessionkey`. The bundle attaches the key only behind a flag; the native client
sends neither. **Seen live**: 200 with `{"groupId": "OID:<channel group>@<tenant>", "displayName", "description",
"hostTeamId": <team group>, "hostTenantId", "createdTime", "chatModalityType"}`, no `threadId`: the thread can
come later, from `GET <provisioning>/teams/<team group>/channels/OID:<channel group>@<tenant>` (`threadId`), read every 4 s.

**The step after a private channel** is the add-people dialog, in the create flow with Skip: "Add members to the
<name> channel", "Start typing a name of a person you want to add to this private channel.", a box "Type a name or email"
scoped to the team, and Add. People go in by the channel's group, before its thread exists:

```
POST <provisioning>/teams/<team group>/channels/OID:<channel group>@<tenant>/users?forceSync=false
{"users": [{"id": "OID:<object id>@<tenant>", "role": 1}]}
```

**Seen live** adding b: 200 with `{"multipleStatus": [], "asyncOperations": []}`; the bundle reads `multipleStatus` as those
that failed (`id`, `statusSubCode`, `message`). No error code was seen live: every write here succeeded. After each creation
the web client sets the new channel a favourite of the account's (`PUT v1/users/ME/conversations/<channel>/properties?name=favorite&requirePermanentJoinedState=true`,
`{"favorite": "true"}`, its own list alone) and marks it read (`consumptionhorizon`, which the lab's fence leaves denied on a
channel). The box's search is a read sent as
a `POST` on the user profile service (GTM `userProfileService`, `https://<AFD host>/api/userprofilesvc/<region>`, the
middle tier's token): `api/v1.0/teams/<team thread>/users/profilesearch?top=15&includeDLs=false&includeBots=false&
enableGuest=true` with `{"keyword": …}`, answered with a list of people (`mri`, `displayName`, `email`, `jobTitle`,
`tenantId`, `objectId`, …): a mention in one of the team's channels asked it so in the capture, the bundle's add-people box
scoping its search to the team the same way. Sent once from Rust as account a (2026-11-02): it answered 200 with the
capture's `x-ms-client-type`, `x-ms-migration`, `x-ms-object-id`, `x-ms-test-user` and `x-client-ui-language`, and 400
without them.

**Shown to everyone**: `PUT <middleTier>/beta/teams/<channel thread>/setDefaultChannel`, no body (`DELETE` takes it back).

**What goes wrong**: the middle tier names an error code (`errorCode`, or `errors[0].errorCode`); the bundle's dialog
replaces the form with its words for each (`problem_words`): `ChannelNameAlreadyExist`,
`MaxPrivateChannelLimitExceeded`, `MaxChannelLimitExceeded`, `UnableToFetchTeamOwner`, `NoPermissionToCreateChannel`,
`UserNotAllowedToCreateChannel`, `CannotCreatePrivateChannelDueToUserPolicy`, `OperationNotAllowedOnArchivedTeam`,
`InvalidUsersInCreateChannelRequest`, `ThreadDescriptionLimitExceeded`. Anything else says "We ran into a problem creating
your channel" with "Try again" (the capture's forced deny showed it).

**The list**: a channel made comes from CSA's next read, which may not name it yet; the native client stores it at once
under its team, by name, and keeps it against reads that don't name it for ten minutes. A private channel's row carries
a lock (`channelType` 1; 0 is standard, 2 shared).

**The fence**: a new channel and its people reach the team, so the rows are writes on the team or the channel
(`WriteKind::ChannelChange`, `MemberChange`; `teams_fence::contacting`'s `contacting/channel.rs`), judged by the team's
members read from its thread, Teams' own apps not counted ([teams.md](teams.md#the-fence)): the lab's fence lets them through
on a team of the lab's own and denies them on any other (the dialog then says "We ran into a problem creating your
channel"). A private channel's people step names the team by its group, so it is judged once the channel's creation read
the team's thread. The personal build's logs and sends them. The provisioning service is a contacting service: everything mutating
there but these rows is unreadable.

## No other channel write

Besides a new channel, nothing the native client sends can post, reply, react, mark read or type in a channel. What keeps it so is the fence,
not a missing type: the engine takes a chat write (`Action::Send`, `Action::Read`, …) for any conversation id, a
channel's too, as a sealed chat write (`PostMessage`, a read receipt) aimed at the channel, and the fence denies it,
since its member read admits only a 1:1 chat and the lab's recorded group chat (a channel's members are never read
complete, and its thread read listed one member); `teams-core`'s `nothing_is_written_to_a_channel` holds it. No write
is mapped to `WriteKind::ChannelPost`, and the views offer no composer or message action in a channel. The web
client's channel writes stay unmapped and denied in the browser fence ([endpoints.md](endpoints.md#writes)). They need
a team of the lab accounts only before they can be mapped and tested
(`docs/backlog/channel-writes-need-a-lab-only-team.md`). The personal build's fence allows them (it denies nothing),
but the app has no channel composer yet.
