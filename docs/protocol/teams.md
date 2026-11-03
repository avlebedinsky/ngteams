# Managing a team

Status: 2026-11-02. From `proto-lab run team-manage`: lab account a in the web client's "Your teams and channels" and Manage
team pages under the armed fence, driven step by step. What the fence let through is marked **seen live**: a private team
made by a (the lab's only team of its own) and its channels hidden. Everything else went only as far as the fence: the page
sent it, the capture holds its method, path and body, the fence denied it, so its shape is captured and **its answer is the
web bundle's reading (`26260.1717.5229.7049`) until a write gets through**. See [The fence](#the-fence) for why. The native
client's writes are `crates/teams-proto/src/write/team.rs`, its reads `team_read.rs`, the engine's part `teams-core`'s
`team_admin`, the pages `teams-ui`'s `teams_page`, `manage_team`, `join_team`, `team_dialogs` and `team_menu`.

**2026-11-03**: with the lab's fence reading a team's members ([The fence](#the-fence)), `team-manage` with
`PROTO_LAB_TEAM_WRITES=1` wrote on the lab's own team, each write permitted and answered: Team details' Edit (`PATCH`, 200),
Add member b (`bulkUpdateRoledMembers`, 200), b promoted (`PUT …/members`, `Promoted`, 200), the team code made and taken away
(`PUT` 200, `DELETE` 200), a member-permissions box and back (`spaceAdminSettings`, 200), the tags' box (`tagSettings`, 200), a
tag made with b and deleted (`POST …/tags` 200, `DELETE …/tags/<tag>` 204), archived and restored (`archive` and `unarchive`,
202 each), and the hidden team's General channel shown again (200). These are **seen live** now.

**2026-11-03, later**: three more `team-manage` runs with `PROTO_LAB_TEAM_WRITES=1`, on a lab team made for them and deleted
after: the Apps tab, Change picture, the Giphy rating, a channel's analytics and "More create team options" (below). Seen
live: the Giphy rating (`spaceAdminSettings`, 200), the picture set and taken back (`profilepicture`, 200 each), an app removed
(200) and a tab app added (201), a team copied from it (`clone`, 200) and that copy deleted (200). A team from a template went
only as far as the fence, by choice: its apps (Workflows, Approvals) would put bots on the team, and the lab's fence then
denies every write on it, its deletion too.

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
  an invitation's keys are the bundle's and untested. Nothing answers one here: an invitation is a shared channel shared with a
  team the account owns, and the lab's tenant offers no shared channel ("Create a channel" lists Standard and Private only,
  `isSharedChannelCreationEnabled` `false`), so none can reach a lab account to capture what answering sends.
- `GET` middle tier `beta/analytics/me/teams/summary-timeseries?metric=all&timeperiod=last-seven-days&includeTimeseries=activeUsers&pagesize=40&nextCursor=`:
  `{"teams": [{"id", "displayName", "privacy", "metrics": {"activeUsers": {"value", "timeSeries": [{"date", "value"}]},
  "guests", "postMessages", "replyMessages", "meetingsOrganized", "reactions", "mentions", ...}}]}`, seen live.

## Creating a team

"Create a team": Team name (at most 256, required), Description (at most 1024), Team type (a step of its own, "What kind of
team will this be?", Private or Public), First channel name (at most 50, required, its "..." sets it to General). Create is off
until both required fields read.

"More create team options" is a page across both panes: Back, "Create team", and From template, From another team, From
group.

- **From template**: "Create a team from a template", a "From scratch" card (the dialog above) and a card per template,
  `GET` provisioning `teamtemplates/v1.0/en-us`: `{"value": [{"id", "name", "shortDescription", "description", "iconUri",
  "channelCount", "appCount", "categories", "locale", "scope", "visibility"}]}`. A card shows the template's channels and
  apps, `GET teamtemplates/v1.0/<id>/Public/en-US`: `{"value": {"templateId", "teamDefinition": {"channels": [{"id",
  "displayName", "description", "isFavoriteByDefault", "tabs"}], "apps": [{"id"}], "memberSettings", "funSettings", …}}}`;
  "Use this template" opens a form of name, description, team type and "Customize channels". Create sends
  `POST` provisioning `team` with `{"apps": [{"id"}], "channels": [{"displayName", "id"}], "description", "displayName",
  "isTenantWide": false, "locale": "en-US", "sensitivityLabelId": null, "templateId", "visibility": "Private" | "Public"}`
  (captured; its answer unseen). The native page leaves the channels' names as the template has them.
- **From another team**: "Which team do you want to use?", a card per team the account owns (none: "You need to own a team
  to create another team from it."). One picked: "You are using "<name>" as a template for a new team", the name filled as
  `<name> [copy]` and the description from it, Privacy, and "Choose what you'd like to include from the original team":
  Channels and Apps ticked and fixed, Tabs and Team settings ticked, Members (N people) not. Create sends `POST` provisioning
  `groups/<group>/clone` with `{"displayName", "description", "ownerObjectId", "teamGroupId", "visibility", "sensitivityLabelId":
  "", "includeRoster", "includeChannels", "includeAppEntitlements", "includeSettings", "includeTabs"}`, **seen live**: 200 with
  `{"groupId", "threadId", "workflowId"}`; the page then polls `GET` provisioning `workflow/status/<workflowId>`
  (`{"status": "InProgress" | "Completed", "details"}`) behind "Creating the team...". The native page lets the teams' reread
  bring the copy.
- **From group**: "Which Microsoft 365 group do you want to use?", `GET beta/teams/usergroups?teamType=undefined` (the literal
  `undefined` as sent), seen only as `[]`: "You need to own a group with fewer than 10000 people to create a team from that
  group. Please create a group first." A group's entry and making a team of it (`migrateGroupToTeam` in the bundle) aren't
  captured: the lab's accounts own no group.

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

Tabs Channels, Members, Pending requests and Settings (an owner's), Analytics, Apps, Tags.

- **Apps**: "Get more apps", "Upload an app", Sort, "Find by name", and a row per app: its icon, name and maker, and a "..."
  of Copy link and Remove. The list is `POST beta/teams/apps/aggregatedEntitlements?appbarview=userpinned` with `[<team>]`, a
  read: `{"value": {"teamsEntitlements": {<team>: [{"id", "state", "isNewGlobalPreInstalledApp"?}]}, "definitions": {<app
  id>: {"name", "developerName", "shortDescription", "accentColor", "smallImageUrl", "largeImageUrl", "bots", "staticTabs",
  …}}, "teamEntitlementsHash", "teamsAsyncOperationInProgress"}}`; the tab lists the entitlements `Installed` (a new team's:
  Forms, Lists, OneNote, Planner, Power BI, Praise, SharePoint, SharePoint Pages, Updates), the rest being Teams' built-in
  ones (`InstalledAndPermanent`, `InstalledAndDeprecated`) and ones offered (`NotInstalled`, `PreConsented`). A new team's
  three member apps are these apps' bots (Updates' is one). Remove asks "Remove <app>" / "Are you sure you want to remove
  <app>?", then `DELETE beta/teams/<team>/apps/entitlements/<app id>`, no body, **seen live**, 200. "Get more apps" opens the
  whole app store: `POST beta/users/apps/store` (each category's apps by id, a hundred at most, with ratings), names from
  `POST beta/users/apps/batchedDefinitions?includeblockedapps=true` with the ids (a list of definitions); an app's Add
  first adds it for the account (`POST beta/users/apps/entitlements` with its definition, 201, the account's own), then
  "Choose a place to use the app" with the team's channel picked, and Go sends `POST beta/teams/<team>/apps/entitlements`
  with the app's definition as the body: **seen live** for SharePoint Pages (no bot), 201 with `{"value": {"id", "state":
  "Installed", "installedVersion", "installedBy", …}}`; a tab's own setup follows, which the native client leaves out. The
  native "Get more apps" is a dialog of the store's popular and Microsoft's apps, searched by name, Add on each.

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
  there is none. Fun stuff's Giphy box, while on, has "Filter out inappropriate content using one of the settings below:"
  with G, PG or PG-13 "(based on the GIPHY Content Rating)": `userSettings.giphyRating` 0, 1 or 2 (a new team's is 1), written
  as every box, **seen live**. Change picture opens "Change team picture": the picture, Upload (`.png,.jpg,.jpeg`), Remove,
  Close and Save. Save sends `POST beta/teams/<group>/profilepicture?threadId=<team>` with the image's bytes as the body (under
  `Content-Type: application/json`, as sent), **seen live**, 200, answered with the picture. Remove shows the picture Teams
  makes from the name, `GET beta/teams/defaultProfilePicture?displayName=<name>&highResolution=true` (a JPEG), and Save then
  sends those bytes the same way, seen live.
- **Analytics**: `GET beta/analytics/teams/<group>/summary-timeseries?metric=all&timeperiod=…&includeTimeseries=…`, the same
  metrics as above in `{"teams": {...}}`, seen live. "Select a channel" (All Channels, then each channel) reads
  `GET beta/analytics/teams/<group>/channels/<channel>/summary-timeseries?metric=all&timeperiod=…&includeTimeseries=activeUsers,postMessages,replyMessages,reactions,mentions,meetingsOrganized`,
  answered as a team's under `channels`, seen live (empty for a new team: `"metrics": {}, "errorCode": 2`). The web client
  opens the channel's own page (Settings, Analytics) for it; the native tab shows the channel's numbers in place.
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
as a new appointment does; a public or org-wide one has nobody to judge: `OpenTeamCreate`, which the lab's fence denies. A team
from a template is judged as a new team by its `visibility`. A copy of a team with its people (`includeRoster`) reaches
everyone on the team it copies: `MemberChange` on that team's group; without them it is a new team. Anything else on a team is
`TeamChange` or `MemberChange` on `Scope::Team`, the people it adds named on it: an app added names its bots (`bots[].id`,
`inputExtensions[].botId`) as `28:` members, so an app with a bot is denied in the lab (a team app is never someone a write
may add). The own-view writes above reach nobody. The personal build logs and sends them all.

The lab's fence reads a team's members from its thread (`LiveMembershipSource`, the user's sign-off of 2026-11-03,
[lab-rules.md](../lab-rules.md)): `GET v1/threads/<thread>?view=msnp24Equivalent` first, which must be that thread with
`properties.threadType` `space` (a team's root) and a `properties.groupId`; a standard channel's thread (`topic`) named
as the team is read as the team its `spaceId` names, whose root thread is read in turn and must name the channel's
`groupId` (a private channel's thread is a `space` of its own, `productThreadType` `TeamsPrivateChannel`, read with its
own members); then `GET v1/threads/<thread>/members?view=msnp24Equivalent&pageSize=100`, complete as a group chat's is.
A write that names the team by its group (archive, the provisioning service's channel paths) is read only once a read of the
thread in the same run named that group; before, it is denied. Teams' own apps every new team holds (`28:<guid>`, role
`User`, no `tenantId`; the same three on the lab's team and on a tenant team of 87 members), like any other app or bot a
team holds, aren't people and aren't counted.
