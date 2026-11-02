# Managing a team

Status: 2026-11-02. From `proto-lab run team-manage`: lab account a in the web client's "Your teams and channels" and Manage
team pages under the armed fence, driven step by step. What the fence let through is marked **seen live**: a private team
made by a (the lab's only team of its own) and its channels hidden. Everything else went only as far as the fence: the page
sent it, the capture holds its method, path and body, the fence denied it, so its shape is captured and **its answer is the
web bundle's reading (`26260.1717.5229.7049`) until a write gets through**. See [The fence](#the-fence) for why. The native
client's writes are `crates/teams-proto/src/write/team.rs`, its reads `team_read.rs`, the engine's part `teams-core`'s
`team_admin`, the pages `teams-ui`'s `teams_page`, `manage_team`, `join_team`, `team_dialogs` and `team_menu`.

## What a team's entry holds

CSA's teams read ([channels.md](channels.md#what-the-native-client-reads)) has it all for the page:
`accessType` (1 private), `description`, `membershipSummary` (`totalMemberCount`, `botCount`: a new team's three apps are
members too, so the page's "N people" leaves the bots out), `memberRoleString`, `isArchived`, `dynamicMembership`,
`isTenantWide`, `channelOnlyMember`, `pictureETag`, `tenantId`. A team's `isFavorite` `false` is a team whose channels are
hidden for the account (the page's eye, and no line in the sidebar); a general channel is in the account's list by the team's
`isGeneralChannelFavorite`, another channel by its own `isFavorite`, and a team with channels out of the list has a
"See all channels" under it.

## The team's "..." menu

The same in the sidebar and on the page. In the bundle's terms, for the account's role:

| Entry | Offered |
|---|---|
| Hide all channels; Show on a hidden team | always |
| Add member | an owner; otherwise a member of a private team (who sends a request) or of a public team that isn't org-wide; never a guest, a channel-only member, a dynamic team or an archived one |
| Add channel | [channels.md](channels.md#creating-a-channel) |
| Manage team, Manage tags, Copy link | always |
| Archive, or Restore on an archived team | an owner, not channel-only |
| Leave team | not a dynamic team or a channel-only member; an org-wide team only for an owner |
| Delete team | an owner, not channel-only |

Copy link puts the team's link on the clipboard (`/l/team/<thread>/<name>?groupId=<group>&tenantId=<tenant>`) and says so in a
toast. Archive and Leave and Delete and Hide ask first, in Teams' words (`team_dialogs.rs`); Delete only after its tick.

## "Your teams and channels"

Tabs Teams, Pending invites, Analytics. The filters: All, Teams you own, and under "3 more" Teams with channels hidden, Teams
with channels shown, Archived Teams. Sort: A-Z, Z-A, Membership Type, Team type, Visibility Type. A row clicked goes to Manage
team. Create team's dropdown holds Create team and Join Team.

- `GET` provisioning `users/me/invitations`: `{"invitations": [...]}`, read for the Pending invites tab; empty for the lab, so
  an invitation's keys are the bundle's and untested. Nothing answers one here.
- `GET` middle tier `beta/analytics/me/teams/summary-timeseries?metric=all&timeperiod=last-seven-days&includeTimeseries=activeUsers&pagesize=40&nextCursor=`:
  `{"teams": [{"id", "displayName", "privacy", "metrics": {"activeUsers": {"value", "timeSeries": [{"date", "value"}]},
  "guests", "postMessages", "replyMessages", "meetingsOrganized", "reactions", "mentions", ...}}]}`, seen live.

## Creating a team

"Create a team": Team name (at most 256, required), Description (at most 1024), Team type (a step of its own, "What kind of
team will this be?", Private or Public), First channel name (at most 50, required, its "..." sets it to General). Create is off
until both required fields read. "More create team options" (from a group or another team) leads to other requests,
`PUT beta/teams/migrateGroup/<group>` and `POST beta/teams/<team>/clone`, which aren't captured or built.

```
POST <middleTier>/beta/teams/create
{"accessType": 1 | 3, "description": …, "displayName": …, "displayNamePrefix": "", "displayNameSuffix": "",
 "isTenantWide": false, "sensitivityLabelId": null, "topicThreadTopic": <first channel>, "validationRequired": false}
```

**Seen live**: 201 with `{"value": {"objectId": <thread>, "displayName", "accessType": "Private", "skypeThreadId": <thread>,
"description", "siteInfo": {"groupId": <group>}}, "type": …}`. The team holds its creator as owner and three of Teams' own apps
as members (`28:<guid>`, role `User`). The Add members step follows, with Skip.

## Adding people

The box finds people on `POST <middleTier>/beta/users/searchV2?includeDLs=true&enableGuest=true&includeBots=true&skypeTeamsInfo=true&source=addToTeam`
with the text as the body, a read, seen live. Each person picked has a role, Member or Owner.

```
PUT <middleTier>/beta/teams/<thread>/bulkUpdateRoledMembers?allowAsyncAddition=true
{"users": [{"mri": …, "role": 0 (member) | 1 (owner)}], "groupId": <group>}
```

What goes wrong says "N of M members were added" with Try again. A member of a private team instead sends a request:
`POST beta/teams/sponsorJoinRequest/<group>` with the people's MRIs, from the bundle.

## Manage team

Tabs Channels, Members, Pending requests and Settings (an owner's), Analytics, Apps, Tags. The Apps tab isn't built: the web
client's list of a team's apps came from no read this capture could name.

- **Channels**: `GET` provisioning `groups/<group>/channels?mode=IncludeIncomingChannels`: `{"channels": [{"threadId",
  "displayName", "description", "channelType", "channelState", "archiveState", "lastActivity", ...}]}`, seen live.
  **Show and Hide are the account's own**: `PUT v1/threads/<channel>/members/<own mri>` with `{"interest": "Interested"}`
  (`"NotInterested"` hiding, the bundle's), and `PUT v1/users/ME/conversations/<channel>/properties?name=isGeneralChannelFavorite`,
  `favorite` for a channel that isn't general. Hide all channels is `PUT` provisioning `team/<thread>/hide`, no body, **202 seen
  live**; a hidden team's Show is `PUT` provisioning `conversations/show` with `{"teamIds", "channelIds"}`, the bundle's.
- **Members**: `GET v1/threads/<thread>/members?view=msnp24Equivalent&pageSize=100&selectMemberRoles=Admin`, then
  `…=User|Guest`, seen live; names from `fetchShortProfile`. Bots aren't listed. A role changes, or someone goes, by
  `PUT <middleTier>/beta/teams/<thread>/members` with `{"userMri", "updateType", "groupId"}`: `Promoted`, `Demoted`, `Removed`,
  and for the account itself `Left`, or `DemotedAndLeft` for an owner (captured).
- **Pending requests**: `POST beta/teams/findJoinRequests` with `{"GroupIdList": [<group>]}`, a read, and
  `POST beta/teams/joinRequestAdminAction?silentAction=false` with `{"GroupId", "UserMri", "ActionType"}`, from the bundle.
- **Settings**: Team details' Edit is `PATCH <middleTier>/beta/teams/<thread>` with `{"description", "displayNamePrefix": "",
  "displayNameSuffix": "", "validationRequired": false}` and `displayName` and `accessType` when they change (captured). The
  boxes of Member permissions, Guest permissions, @mentions and Fun stuff write the team's whole settings on the chat service:
  `PUT v1/threads/<thread>/properties?name=spaceAdminSettings` with `{"spaceAdminSettings": "<JSON as text>"}`, holding
  `userSettings` and `guestSettings`, read from `GET v1/threads/<thread>?view=msnp24Equivalent` (`properties.spaceAdminSettings`).
  The tags' boxes are `PUT <CSA>/api/v1/teams/<thread>/tagSettings` with `{"onlyOwnersCanUpdate", "disallowedSources",
  "enableAutomaticTags"}`. Team code: `GET`, `PUT` (make or reset), `DELETE` `beta/teams/<thread>/joinCode`; the read is 204 while
  there is none. Change picture isn't built (`POST beta/teams/<group>/profilepicture?threadId=<thread>` with the image).
- **Analytics**: `GET beta/analytics/teams/<group>/summary-timeseries?metric=all&timeperiod=…&includeTimeseries=…`, the same
  metrics as above in `{"teams": {...}}`, seen live. Its "Select a channel" isn't built.
- **Tags**: `GET <CSA>/api/v1/teams/<thread>/tagCards?tagType=any`: `{"tagCards": [...], "teamTagSettings": {...}}`, seen live
  and empty, so a card's keys are the bundle's. `POST <CSA>/api/v1/teams/<thread>/tags` with `{"tagName", "tagDescription",
  "memberIds"}` (captured); `PATCH` `…/tags/<tag>` with `if-match: <version>` and `DELETE` `…/tags/<tag>` (the bundle's).

## Archive, Restore, Delete, Leave

- Archive: `POST` provisioning `groups/<group>/archive`, with `?setSpoSiteReadOnlyForMembers=true` when ticked; Restore:
  `POST` provisioning `groups/<group>/unarchive`.
- Delete: `DELETE <middleTier>/beta/teams/<thread>/delete`, no body (captured).
- Leave: `updateType` above.

## Join a team

Create team's dropdown leads there: `GET beta/teams/suggested?accessType=Public&enableFallback=true` and
`GET beta/teams/find?keywords=…&accessType=Public&enableKeywordSearch=false&top=100&skipToken=`; `POST beta/teams/joinRequest/<group>`
to join; `POST beta/teams/joinByCode?restrictJoinToPublicTeam=false` with the code as a JSON string. All from the bundle: the
page showed live and nothing was joined.

## The fence

`teams_fence::contacting`'s `contacting/team.rs`. A new private team names nobody: `TeamCreate` on `Scope::New`, which passes,
as a new appointment does; a public or org-wide one has nobody to judge: `OpenTeamCreate`, which the lab's fence denies. Anything
else on a team is `TeamChange` or `MemberChange` on `Scope::Team`, the people it adds named on it. The own-view writes above reach
nobody. The fence doesn't read a team's members (`LiveMembershipSource` answers `Unsupported` for `Scope::Team`, as before), so
**the lab's fence denies every write on every team, its own team too**. Reading them from the team's thread wouldn't change that:
its member list (`GET v1/threads/<thread>/members`, seen live) holds a new team's three apps (`28:<guid>`), which aren't lab
accounts or the echo service. Either step changes the rule's test and needs the user's sign-off. The personal build logs and
sends them all.
