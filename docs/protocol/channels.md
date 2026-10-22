# Teams and channels (read-only)

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
  team's); an entry without a string id and name is left out; the general channel first. `chats`, `users`,
  `conversationFolders` and the rest aren't read: the chat list stays the chat service's.
  `teams-core` reads it in the background when a view asks and the stored list is missing or older than an hour
  (`TEAMS_MAX_AGE`), keeps it in the store whole and tells a change as `Event::Teams`.
- **A channel's posts**: the chat service's message reads, as a chat's (`ChatReads::messages` and its links), on demand
  when the channel is opened (`EngineRequests::sync_conversation`), older pages on "Load older posts". A channel the
  store doesn't know is read as a thread first (`topic` and `space` are both a channel, read-only). Each stored
  message's post is kept beside it from its `conversationLink` (`Message::reply_root`, only when the link names that
  channel and the post is digits); a message without one is shown as a post of its own. The store's schema 9 started
  the general channels' syncs again, so their messages stored before get their posts.
- **Not used**: one post's chain read (the channel's page holds the replies), the thread's members (a channel is
  read-only: no member is stored for it), CSA's posts read and its update read.

## No channel write

Nothing the native client sends can post, reply, react, mark read or type in a channel: `teams-proto` has no sealed
write for a channel (`WriteKind::ChannelPost` is mapped nowhere), the views offer no composer or message action there,
and a chat write aimed at a channel would be denied by the fence besides (its member read admits only a 1:1 chat and
the lab's recorded group chat; a channel's members are never read complete, and its thread read listed one member). The web client's channel writes stay unmapped and denied in the browser
fence ([endpoints.md](endpoints.md#writes)). They need a team of the lab accounts only before they can be mapped and
tested (`docs/backlog/channel-writes-need-a-lab-only-team.md`); the personal build stays read-only until then.
