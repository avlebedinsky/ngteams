# Endpoints: every request the project has seen or uses

Status: 2026-10-21, M1 (after Tasks 16 and 20). One row per endpoint the lab has seen the web client send, a lab
command sent itself, the native client sends, or the bundle names and a note relies on. The notes in this directory
explain the endpoints; this file only lists them, so that "which endpoints do we know, and what happens to each" has
one answer. Built from the protocol notes, the code (`proto-lab`'s classifiers, its reviewed expected-deny list and
read-by-`POST` list, `teams-proto`'s reads, writes and frames, `teams-trouter`, `teams-auth`) and a local count of the
captures under the lab's data dir (method, host and path pattern only, ids normalised; nothing of a capture is copied
here).

**Kept current by every task that touches an endpoint.** A task that captures, maps, allowlists, expects or uses a
new endpoint, or changes how one is treated, adds or updates its row here in the same change. To add a row: put it
in its service's table; write the path with placeholders only (`<region>`, `<partition>`, `<chat>`, `<message>`,
`<mri>`, `<guid>`, `<id>`, `<n>`), never a real id, host number, site or tenant name; describe it in our own words;
fill the two treatment columns from the code as it stands; link the note that describes it and name the task; say
where the evidence comes from. An endpoint first seen in a capture and not yet reviewed goes in as **seen, not
reviewed**, and keeps that until a note describes it.

## How to read a row

- **Path** is relative to the section's host or base; `?a&b` lists the query parameters that matter (names only).
- **Browser fence** is what the lab browser's fence does with the web client's request (`crates/proto-lab`):
  - `read`: `GET`, `HEAD` and `OPTIONS` change nothing others see and pass in every classifier;
  - `mapped <WriteKind> → <Target>`: a write `Classifier::mapped` (and so `calling`, `messaging`) maps, allowed only
    with a permit from `Fence::check`: an existing target after a fresh member read, a new one (`Scope::New`: a chat's
    creation, the test call's start) by its proposed participants, with no member read;
  - `own-state <kind>`: on the reviewed own-state list (`OwnKind`), with the classifiers that have it;
  - `read-by-POST <entry>`: on `post_read::POST_READS`, let through only when every MRI it names is a lab account;
  - `expected deny: <class>`: `Unknown` and denied, and on the writing scenarios' reviewed expected-deny list
    (`scenario/expected.rs`; `ab-trouter`'s own classes in `TROUTER_RUN_REQUESTS`), so it doesn't fail a run;
  - `denied Unknown`: denied and on no list, so a writing run that meets it fails until it is reviewed;
  - `forced deny`: a scenario's `ForcedDeny` (its probe, `ab-image`'s media writes), denied before classification.
  A frame on a socket is judged by `FrameClassifier` the same way (`own-state trouter_session` or `Unknown`).
- **Native** is what the native client does with it (`crates/teams-proto`, `crates/teams-auth`): `Read`, `OwnState`,
  `Authorized<Mutating>` (a sealed write sent only with its permit), `OwnFrame` (a sealed frame logged to `fence.log`
  first), `teams-auth` (its own HTTP client, not the transport), `teams-trouter` (the push socket, its own WSS
  connection to the Trouter hosts only, sending nothing but `OwnFrame`s through its `FrameSink`), or **not on the
  allowlist** (the transport reaches only the chat service, the registrar and Trouter's HTTP side, so it can't send it
  at all). The transport's own-state list (`OwnState`) has one entry, the messaging stack's `EndpointRegistration`.
  The native push channel (Task 16) is tested on `teams-mock` only; its first live run is Task 19's.
- **Notes · task**: the note that describes it, and the task that captured, mapped or built it (M0 or M1 task number).
- **Evidence**: `seen` (in the lab's captures, with the scenarios), `probe` (a `proto-lab` command's own live request),
  `bundle only` (named in the web bundle `26260.1717.5229.7049`, never on the wire), or `seen, not reviewed` (in the
  captures, described nowhere but here).

## Entra (`login.microsoftonline.com`)

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `GET` | `/organizations/oauth2/v2.0/authorize` `?client_id&response_type&redirect_uri&scope&login_hint&code_challenge` | the sign-in page; the native client's code comes back on the `nativeclient` redirect | read | `teams-auth` (PKCE, native client) | [auth.md](auth.md#flow) · M0 T12, T14 | probe (`proto-lab auth`) |
| `GET` | `/common/oauth2/v2.0/authorize`, `/<tenant>/oauth2/v2.0/authorize` `?prompt&response_mode&client_info` | the web client's MSAL sign-in (SPA client) | read | not used | [auth.md](auth.md#flow) · M0 T11 | seen (all scenarios) |
| `GET` | `/common/oauth2/nativeclient` | the native client's redirect page; the code is taken from the navigation | read (the code is caught before the page loads) | `teams-auth` | [auth.md](auth.md#client-and-redirect) · M0 T14 | probe |
| `POST` | `/<tenant or organizations or common>/oauth2/v2.0/token` | code redemption and every refresh grant, one per audience | own-state `entra_token` (every classifier) | `teams-auth` | [auth.md](auth.md#flow) · M0 T11, T14 | seen (all), probe |
| `POST` | `/<tenant>/GetCredentialType`, `/<tenant>/login`, `/kmsi`, `/<tenant>/kmsi` | the sign-in form: username lookup, password, "stay signed in" | own-state `entra_sign_in` (`Classifier::sign_in`, `proto-lab login` only) | not used | `classify.rs` (`SIGN_IN`) · M0 T8 | probe (`proto-lab login`) |

## Teams auth service (`teams.microsoft.com/api/authsvc/v1.0/`)

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `POST` | `authz` | an `api.spaces.skype.com` token for the skypetoken, the region and the region GTMs | own-state `authz` | `teams-auth` (`AUTHZ_ENDPOINT`) | [auth.md](auth.md#authz-the-skypetoken-exchange) · M0 T11, T14 | seen (`load`, `ab-chat`, `ab-chat-full`, `ab-trouter`), probe |
| `POST` | `authzcore`; also `authsvc.teams.microsoft.com/v1.0/authz` and `…/authzcore` | the AuthZ v2 flow (no skypetoken) and the old host | own-state `authz` | not used | [auth.md](auth.md#authz-vs-authzcore) · M0 T12 | bundle only |
| `POST` | `authz/signuptrial`, `authzcore/signuptrial` | starts a Teams trial: a licensing change | denied Unknown (the own-state rule matches whole paths) | not used (`teams-auth` refuses any URL not ending in `/authz`) | [auth.md](auth.md#authz-vs-authzcore) · M0 T12 | bundle only |
| `GET` | `lfts/TeamsPremium/eligibility/`, `lfts/TeamsPremium/policies/selfserve` | Teams Premium license checks | read | not on the allowlist | [auth.md](auth.md#audience--service-table) · M0 T11 | seen (all) |

## Chat service (`https://<teams origin>/api/chatsvc/<region>/`)

The base is authz's `chatServiceAfd` GTM; `<teams origin>` is `teams.microsoft.com` or `teams.cloud.microsoft`, and the
lab's region is `amer`. The regional form `https://<region>.ng.msg.teams.microsoft.com/` (paths from `v1/` on) was
seen only in answers (`chatService` GTM, `Location` headers); the fence and the transport accept both
([fence-targets.md](fence-targets.md#the-chat-service)). Ids are one percent-encoded segment.

### Reads

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `GET` | `v1/users/ME/conversations` `?view&pageSize&startTime&targetType` | the conversation list, one page, with a `syncState` link | read | `Read` (`ChatReads::conversations`, and its links) | [chat.md](chat.md#the-conversation-list-probe) · M1 T3, T10 | probe (the web client never sent it) |
| `GET` | `v1/users/ME/conversations/<chat>/messages` `?view&pageSize&syncState&startTime` | a page of a chat's messages: sync from a kept state, or newest first | read | `Read` (`ChatReads::messages`, and its links) | [chat.md](chat.md#message-pages-ab-chat-full-b) · M1 T7, T10 | seen (all) |
| `GET` | `v1/users/ME/conversations/<chat>/messages/epochTimeStamp/<ms>` `?pageSize&direction&includeMetadata&locationType` | the page around a time, both ways | read | `Read` (`ChatReads::messages_around`) | [chat.md](chat.md#message-pages-ab-chat-full-b) · M1 T7, T10 | seen (`ab-chat`, `ab-chat-full`) |
| `GET` | `v1/users/ME/conversations/<chat>` `?view` | the account's own view of a chat (404 until it exists) | read | not used | [fence-targets.md](fence-targets.md#a-group-chats-reads) · M0 T11, M1 T8 | seen (`ab-chat`, `echo-call`), probe |
| `GET` | `v1/threads/<chat>` `?view` | the thread with its `members`: the fence's live member read | read | `Read` (`ChatReads::thread`); `teams-fence`'s `LiveMembershipSource` sends it from Rust | [fence-targets.md](fence-targets.md#reading-a-chats-members) · M0 T11, M1 T3, T8 | probe (`proto-lab probe thread`, the fence's own read) |
| `GET` | `v1/threads` `?view&threadIds` | several threads at once (the web client's form) | read | not used | [fence-targets.md](fence-targets.md#reading-a-chats-members) · M0 T11 | seen (`ab-chat`), probe |
| `GET` | `v1/threads/<chat>/members` `?view&pageSize` | the member list with `totalMemberCount` (no paging seen) | read | `Read` (`ChatReads::members`) | [fence-targets.md](fence-targets.md#a-group-chats-reads) · M1 T8, T10 | probe |
| `GET` | `v1/threads/<chat>/filtered` `?view` | a filtered thread view (403 `AclCheckFailed` for a member) | read | not used | [fence-targets.md](fence-targets.md#a-group-chats-reads) · M1 T8 | probe |
| `GET` | `v1/threads/onetoone` `?view&userIds&threadType&isFederated`, `v1/threads/<chat>/members/<mri>` | finds two users' 1:1 thread; one member | read | not used | [fence-targets.md](fence-targets.md#reading-a-chats-members) · M0 T11 | bundle only |
| `GET` | `v1/threads/<chat>/consumptionhorizons` | the members' read horizons others may see | read | not used | [chat.md](chat.md#read-horizons) · M1 T7 | seen (all) |
| `GET` | `v1/users/ME/conversations/48:mentions/messages` `?view&pageSize&startTime` | the account's own mentions stream | read | not used | [chat.md](chat.md#mentions-mention) · M1 T7 | seen (`ab-chat-full`) |
| `GET` | `v1/users/ME/conversations/48:notifications/messages` `?view&pageSize&startTime&syncState`, `v1/users/ME/conversations/48:notifications` | the account's own activity feed | read | not used | [chat.md](chat.md#mentions-mention) · M1 T7 | seen (all but `abc-chat`, `echo-call`) |
| `GET` | `v1/users/ME/conversations/48:calllogs` `?view` | the call log stream (404 or 200 by run) | read | not used | [chat.md](chat.md#seen-beside-it) · M0 T11 | seen (all) |
| `GET` | `v1/users/ME/conversations/48:notes` `?view` | the account's own notes stream | read | not used | [fence-targets.md](fence-targets.md#ids) (the id only) · M0 T11 | seen, not reviewed (`ab-chat`) |
| `GET` | `v1/users/ME/conversations/48:sharedcalllogs` `?view` | a shared call log stream | read | not used | this file | seen, not reviewed (`echo-call`) |
| `GET` | `v1/users/ME/properties` | the account's own properties (404 for the lab) | read | not used | [chat.md](chat.md#seen-beside-it) · M0 T11 | seen (all) |

### Writes

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `POST` | `v1/users/ME/conversations/<chat>/messages` (`messagetype` `RichText/Html`, `Text`, `RichText/…`) | posts a message (quotes and mentions included) | mapped `Message` → `Conversation(<chat>)` + mentioned MRIs; the scenarios' probe is a forced deny | `Authorized<Mutating>` (`SendMessage`), sent by `teams-core`'s outbox for the engine's `Action::Send` | [chat.md](chat.md#a-message-as-sent-ab-chat-full-a), [fence-targets.md](fence-targets.md#writes) · M0 T11, M1 T7, T12, T13, T18 | seen (all but `echo-call`, `load`) |
| `POST` | same, `messagetype` `Control/Typing` | typing | mapped `Typing` → `Conversation(<chat>)` | `Authorized<Mutating>` (`Typing`); the engine doesn't send it | [fence-targets.md](fence-targets.md#writes) · M0 T11, M1 T12 | seen |
| `POST` | same, `messagetype` `Control/ClearTyping` | typing stopped | mapped `Typing` → `Conversation(<chat>)` | not used (not captured) | [fence-targets.md](fence-targets.md#writes) · M0 T11 | bundle only |
| `PUT` | `v1/users/ME/conversations/<chat>/messages/<message>` | edits a message (the whole message again) | mapped `Edit` → `Conversation(<chat>)` + mentioned MRIs | `Authorized<Mutating>` (`EditMessage`), sent by the outbox for `Action::Edit` (the account's own standing message only) | [chat.md](chat.md#a-message-as-read) · M1 T7, T12, T18 | seen (`ab-chat-full`, `ab-trouter`) |
| `DELETE` | `v1/users/ME/conversations/<chat>/messages/<message>` `?behavior=softDelete` | soft-deletes a message | mapped `Delete` → `Conversation(<chat>)` | `Authorized<Mutating>` (`DeleteMessage`), sent by the outbox for `Action::Delete` (the account's own standing message only) | [chat.md](chat.md#a-message-as-read) · M1 T7, T12, T18 | seen (`ab-chat-full`) |
| `PUT`, `DELETE` | `v1/users/ME/conversations/<chat>/messages/<message>/properties` `?name=emotions` | adds or takes back a reaction | mapped `Reaction` → `Conversation(<chat>)` | `Authorized<Mutating>` (`SetReaction`, `RemoveReaction`), sent by the outbox for `Action::React`, `Action::Unreact` | [chat.md](chat.md#a-message-as-read) · M1 T7, T12, T18 | seen (`ab-chat-full`, `ab-trouter`) |
| `PUT` | `v1/users/ME/conversations/<chat>/properties` `?name=consumptionhorizon` | the read receipt (the account's read horizon) | mapped `ReadReceipt` → `Conversation(<chat>)` | `Authorized<Mutating>` (`ReadReceipt`), sent by the outbox for `Action::Read` | [chat.md](chat.md#read-horizons) · M0 T11, M1 T12, T18 | seen (`ab-chat`, `ab-chat-full`) |
| `POST` | `v1/threads` | creates a chat (1:1 with `fixedRoster`/`uniquerosterthread`, or a group) | mapped `ChatCreate` → `New` + the body's members | `Authorized<Mutating>` (`CreateChat`), sent by the outbox for `Action::CreateChat` | [fence-targets.md](fence-targets.md#writes) · M0 T11, M1 T8, T12, T18 | seen (`ab-chat`, `abc-chat`, `echo-call`) |
| `PUT` | `v1/threads/<chat>/members`, `v1/threads/<chat>/members/<mri>` | adds members or changes one member's role | mapped `MemberChange` → `Conversation(<chat>)` + the added MRIs | not used | [fence-targets.md](fence-targets.md#writes) · M0 T11 | bundle only |
| `DELETE` | `v1/threads/<chat>/members/<mri>` | removes a member | mapped `MemberChange` → `Conversation(<chat>)` | not used | [fence-targets.md](fence-targets.md#writes) · M0 T11 | bundle only |

### Unmapped (denied)

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `PUT` | `v1/users/ME/properties` `?name` | the account's own settings (`messageDensity`, `favorites`, `locale`, `firstLoginInformation`, `readReceiptsEnabled`) | expected deny: own chat properties | not used | [chat.md](chat.md#seen-beside-it) · M0 T11 | seen (all; tens of thousands of retries) |
| `PUT` | `v1/users/ME/conversations/48%3Anotifications/messages/<message>/properties` `?name=isread` | marks an activity feed item read | expected deny: own activity feed item read | not used | [chat.md](chat.md#seen-beside-it) · M1 T7 | seen (`ab-chat-full`, `ab-image`, `ab-trouter`) |
| `PUT` | `v1/users/ME/conversations/48%3Amentions/messages/<message>/properties` `?name=isread` | marks a mention read | expected deny: own mention read | not used | [chat.md](chat.md#mentions-mention) · M1 T7, T20 | seen (`ab-chat-full`, `ab-image`) |
| `POST` | `v1/agents/<bot>/invoke` | a bot invoke (`composeExtension/fetchCommands` when an editor opens) | expected deny: bot invokes | not used | [chat.md](chat.md#seen-beside-it), `docs/backlog/bot-invokes-and-message-extensions.md` · M1 T7 | seen (`ab-chat-full`, `ab-trouter`) |
| `PUT` | `v1/threads/<channel>/members/<mri>` `?returnThreadMetadata` | the account's own member entry in a team channel | denied Unknown (channels aren't mapped) | not used | [fence-targets.md](fence-targets.md#writes) · M0 T11 | seen (`ab-chat`) |
| `PUT` | `v1/users/ME/conversations/<channel>/properties` `?name=lastTimeFavorited` | a channel's conversation property | denied Unknown | not used | [fence-targets.md](fence-targets.md#writes) · M0 T11 | seen (`ab-chat`) |
| `POST`, `PUT` | `v1/users/ME/conversations/<channel or chain>/messages…` | posts and edits in a channel or reply chain | denied Unknown (`ChannelPost` unmapped) | not used | [fence-targets.md](fence-targets.md#writes) · M0 T11 | bundle only |
| `PUT` | `v1/users/ME/conversations/<chat>/properties` `?name=<other>` | other conversation properties (favourite, mute) | denied Unknown | not used | [fence-targets.md](fence-targets.md#writes) · M0 T11 | bundle only |
| `PUT` | `v1/threads/<chat>/properties` `?name`, `v1/threads/<chat>/members/ALL` | thread properties (topic) and a property of every member | denied Unknown | not used | [fence-targets.md](fence-targets.md#writes) · M0 T11 | bundle only |
| various | `v1/threads/<chat>/livemessages`, `…/messages/forward`, `…/messages/move`, `rcmetadata`, `rc2s`, `v1/threads/<chat>/files`, `v1/users/ME/drafts` | live and targeted messages, forwarding and moving, reply-chain metadata, file references, drafts | denied Unknown | not used | [fence-targets.md](fence-targets.md#writes) · M0 T11 | bundle only |
| `PUT`, `DELETE` | `v2/users/ME/endpoints/<id>`, `…/activeendpoint` | chat endpoint registration and the active-endpoint post (the client uses Trouter instead) | denied Unknown (a candidate for own-state, not reviewed) | not used | [fence-targets.md](fence-targets.md#writes) · M0 T11 | bundle only |

## CSA and the push service (`https://<teams origin>/api/csa/<region>/`, `…/api/tps/`)

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `POST` | `api/csa/<region>/api/v1/teams/users/me/chatsAndChannelsByIds` | the web client's chat list, read by `POST` | expected deny: chat list by POST (CSA) | not on the allowlist | [chat.md](chat.md#the-conversation-list-probe) · M0 T11 | seen (`ab-chat`) |
| `GET` | `api/csa/<region>/api/v3/teams/users/me/updates` `?isPrefetch&enableMembershipSummary&…` | teams, chats and their latest messages since a point | read | not on the allowlist | [chat.md](chat.md#mentions-mention) · M1 T7 | seen (all) |
| `GET` | `api/csa/<region>/api/v3/teams/users/me` `?isPrefetch&…` | the account's teams and chats, in full | read | not on the allowlist | this file | seen, not reviewed (`ab-chat`, `load`) |
| `GET` | `api/csa/<region>/api/v1/teams/users/me/discover` `?pageSize&continuationToken`, `…/pinnedChannels`, `…/teams/tagCards` `?tagType&pageSize`, `…/tenantTagSettings` | discoverable teams, pinned channels, tags and tag settings | read | not on the allowlist | this file | seen, not reviewed (`discover`, `pinnedChannels`: all; the tags: `ab-chat`) |
| `GET` | `api/csa/<region>/api/v1/containers/<channel>/posts` `?pageSize&…`, `…/teams/<team>/channels/<channel>/pins` `?pageSize`, `…/systemmessages` `?pageSize` | a channel's posts, pinned posts and system messages | read | not on the allowlist | this file | seen, not reviewed (`ab-chat`, `load`) |
| `GET` | `api/csa/<region>/api/v1/chats/48:calllogs/messages` `?messagePageSize` | the call log through CSA | read | not on the allowlist | this file | seen, not reviewed (`echo-call`) |
| `GET` | `api/csa/<region>/api/v1/translator/languages` `?locale` | the translation languages offered | read | not on the allowlist | this file | seen, not reviewed (`ab-chat`, `load`) |
| `POST` | `api/tps/api/v1/teams/users/me/metadata/pushupdates` | team metadata updates (push service) | expected deny: push updates metadata | not on the allowlist | [auth.md](auth.md#audience--service-table) · M0 T11 | seen (all writing scenarios) |

## Middle tier and apps (`https://<teams origin>/api/mt/part/<partition>/`, `…/api/apps/part/<partition>/`)

`<partition>` is a region, `-` and a number (`amer-03` for the lab). Every `POST` here is a lookup the web client
sends as a `POST`; every `GET` passes as a read.

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `POST` | `beta/users/fetchShortProfile` `?isMailAddress=false&enableGuest&skypeTeamsInfo&canBeSmtpAddress&includeIBBarredUsers&includeDisabledAccounts` | short profiles of a list of MRIs | read-by-POST `short_profile` when every MRI is a lab account; otherwise expected deny: short profiles beyond the lab | not on the allowlist | [fence-targets.md](fence-targets.md#reads-by-post) · M0 T11, M1 T7 | seen (`ab-chat`, `ab-chat-full`, `ab-trouter`, `abc-chat`, `echo-call`) |
| `POST` | `beta/users/fetchShortProfile` `?isMailAddress=true` | the same lookup by address | expected deny: short profiles beyond the lab | not on the allowlist | [fence-targets.md](fence-targets.md#how-a-run-uses-it) · M0 T11 | seen (`ab-chat`) |
| `POST` | `beta/users/fetch` `?<the lookup query>` | full profiles of a list of MRIs | read-by-POST `user_fetch` (lab accounts only); otherwise expected deny: user fetch beyond the lab | not on the allowlist | [fence-targets.md](fence-targets.md#reads-by-post) · M1 T7 | seen (`ab-chat`, `ab-chat-full`, `ab-trouter`, `abc-chat`) |
| `POST` | `beta/users/useraggregatesettings` | the account's setting groups (the calling policy among them) | own-state `settings_read` in `Classifier::calling` (reviewed shape); elsewhere expected deny: aggregate settings | not on the allowlist | [calling-echo.md](calling-echo.md#what-has-to-be-up-before-a-call) · M0 T18 | seen (all writing scenarios) |
| `POST` | `beta/users/effectivePolicies` | the account's effective policies by name | own-state `settings_read` in `Classifier::calling`; elsewhere expected deny: effective policies | not on the allowlist | [calling-echo.md](calling-echo.md#what-has-to-be-up-before-a-call) · M0 T18 | seen |
| `POST` | `beta/users/fetchfeaturesettings` | feature settings | expected deny: feature settings | not on the allowlist | [fence-targets.md](fence-targets.md#what-the-captures-showed) · M0 T11 | seen |
| `POST` | `beta/users/<oid>/cookiev2` | a cookie for profile pictures and embedded content | expected deny: cookie exchange | not on the allowlist | [auth.md](auth.md#audience--service-table) · M0 T11 | seen |
| `POST` | `beta/me/files/availability` | whether the account's file storage is ready | expected deny: files availability | not on the allowlist | [fence-targets.md](fence-targets.md#what-the-captures-showed) · M0 T11 | seen |
| `POST` | `v1/mcps/initArtifactFolder` | sets up a folder for shared artifacts | expected deny: artifact folder | not on the allowlist | [fence-targets.md](fence-targets.md#what-the-captures-showed) · M0 T11 | seen |
| `POST` | `v1/sharetoteams/installApp` | installs the share-to-Teams app | expected deny: share-to-Teams app install | not on the allowlist | `scenario/expected.rs` · M1 T1 | seen |
| `PUT` | `beta/me/engagementSurfaces/actions` | records which of the client's prompts the account saw | expected deny: engagement surfaces | not on the allowlist | `scenario/expected.rs` · M1 T1 | seen |
| `POST` | `beta/users/apps/aggregatedEntitlements` `?appbarview`, `beta/teams/apps/aggregatedEntitlements`, `beta/users/apps/batchedDefinitions`, `beta/users/apps/eligibilities` (each under `api/mt/…` and `api/apps/…`) | the app catalogue and entitlements | expected deny: the matching app class | not on the allowlist | [fence-targets.md](fence-targets.md#what-the-captures-showed) · M0 T11 | seen |
| `GET` | `beta/users/<oid>/profilepicturev2/<mri>` `?displayname&size&ETag`, `…/profilepicturev2/teams/<guid>`, `…/mergedProfilePicturev2` `?usersInfo&size` | profile and team pictures (cookie auth) | read | not on the allowlist | [auth.md](auth.md#audience--service-table) (the first) · M0 T11 | seen; the team and merged forms seen, not reviewed |
| `GET` | `v2.0/places/findPlaces` `?filterQuery`, `v2.0/users/licenseDetails`, `beta/sharepoint/sites/validSites`, `beta/tenant/verifiedDomains` | the calls that still carry `x-skypetoken` | read | not on the allowlist | [auth.md](auth.md#audience--service-table) · M0 T11 | seen |
| `GET` | `beta/users/tenantsv2`, `beta/users/<id>` `?applicableTo`, `beta/users/sensitivityLabels`, `beta/users/apps/usage`, `beta/users/apps/definitionsv2`, `beta/me/engagementSurfaces`, `beta/me/engagementSurfaces/iris/batch`, `beta/me/settings/meetingConfiguration`, `beta/me/settings/appPolicy`, `beta/me/settings/canManageLobApps` | tenants, the account's user record, sensitivity labels, app usage and definitions, prompts, meeting and app settings | read | not on the allowlist | this file | seen, not reviewed |
| `GET` | `beta/userSettings/breakthroughlist`, `beta/userSettings/blocklist`, `beta/usersettings/useEndToEndEncryption`, `beta/usersettings/blockedCallers`, `beta/users/automaticRepliesSetting` | the account's own user settings | read | not on the allowlist | this file | seen, not reviewed |
| `GET` | `beta/contactsv3/` `?pageSize`, `beta/teams/suggested`, `beta/atpsafelinks/getpolicy/`, `beta/tenant/privacyProfile`, `v2.0/me/calendars/default/calendarView` `?StartDate&EndDate`, `beta/users/<id>/tenant/<guid>/user/<guid>` | contacts, suggested teams, the safe-links policy, the tenant's privacy profile, the calendar, a cross-tenant user | read | not on the allowlist | this file | seen, not reviewed |
| `GET` | `beta/channels/<channel>/tabs`, `beta/chats/<chat>/apps/chatentitlements` (under `api/mt/…` and `api/apps/…`) | a channel's tabs and a chat's apps | read | not on the allowlist | this file | seen, not reviewed (`ab-chat`) |

## Registrar (`https://<teams origin>/registrar/prod/V2/`)

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `POST` | `registrations` (`appId` `TeamsCDLWebWorker`) | registers the messaging stack's Trouter endpoint, so chat events reach it | own-state `endpoint_registration` in `Classifier::messaging` (reviewed shape) and `Classifier::calling`; denied Unknown elsewhere | `OwnState` (`EndpointRegistration`, the messaging stack only, `templateKey` `TeamsCDLWebWorker_2.6`), sent by `teams-trouter` through the transport on every `trouter.connected`, renewed 30 s before its 3600 s TTL ends, retried 123 s after a failure | [trouter.md](trouter.md#registration-and-its-ttl), [the native client](trouter.md#the-native-client-m1-task-16) · M0 T18, M1 T14, T16 | seen (`ab-trouter`, `ab-trouter-quiet`, `echo-call`) |
| `POST` | `registrations` (`appId` `SkypeSpacesWeb`) | registers the calling stack's endpoint: makes the account reachable for calls | own-state `endpoint_registration` in `Classifier::calling` only; expected deny (`ab-trouter`'s calling-stack registration) | not used (`teams-trouter` never makes it: it would make the account reachable for calls) | [calling-echo.md](calling-echo.md#what-has-to-be-up-before-a-call) · M0 T18, M1 T14 | seen |
| `POST` | `<geo>/v3/registrations` on the host `trouter.connected` names | a v3 registrar the service hands over; the web client doesn't use it | denied Unknown | not used | [trouter.md](trouter.md#trouterconnected-the-endpoint) · M1 T14 | seen as data only |

## Trouter (`go.trouter.teams.microsoft.com`, `<name>-t.trouter.teams.microsoft.com`)

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `GET` | `https://go.trouter.teams.microsoft.com/` `?check&epid&tc&cor_id` | a reachability probe before connecting | read | not used (`Service::Trouter` is routed, nothing reads it; `teams-trouter` connects without the probe) | [auth.md](auth.md#audience--service-table) · M0 T11 | seen (all) |
| `WS` | `wss://go.trouter.teams.microsoft.com/v4/c` (also `/v4/c/`) `?tc&timeout&epid&ccid&cor_id&con_num` | the push socket (socket.io 0.9), one per stack | socket allowed; frames judged per frame (next rows) | `teams-trouter`'s socket (`/v4/c`, `?tc&timeout&epid&cor_id&con_num` as the messaging stack's, no `ccid`; the web client's `Origin`, no credential in the handshake; the system's TLS verifier); frames only through `FrameSink` | [trouter.md](trouter.md#connect-and-the-regional-move), [the native client](trouter.md#the-native-client-m1-task-16) · M0 T18, M1 T14, T16 | seen (all) |
| `WS` | `wss://<name>-t.trouter.teams.microsoft.com/v4/c` | the regional socket after `trouter.connected` (`reconnectUrl`) | as above | `teams-trouter`'s socket, to a `<name>-t` host under `.trouter.teams.microsoft.com` only (a fixed allowlist, port 443), with the `connectparams`, until they near their expiry or connects keep failing | [trouter.md](trouter.md#connect-and-the-regional-move), [the native client](trouter.md#the-native-client-m1-task-16) · M1 T14, T16 | seen |
| frame | `5:::{"name":"user.authenticate",…}` | authenticates the socket with the `ic3` token and the cached `connectparams` | own-state `trouter_session` (`FrameClassifier::calling`, `messaging`); expected deny: Trouter frames elsewhere | `OwnFrame` `trouter_authenticate`, sent by `teams-trouter` once per connect, right after `1::` (never again on an open socket) | [trouter.md](trouter.md#authentication-the-first-frame) · M0 T18, M1 T14, T15, T16 | seen |
| frame | `5:<n>+::{"name":"ping"}`, `…trouter.processed_message_loss…`, `0::` | keep-alive, message-loss acknowledgement, disconnect | own-state `trouter_session` (as above) | `OwnFrame` `trouter_ping` (every 40 s), `trouter_message_loss_ack` (once the owner caught up), `trouter_disconnect` (on a stop or a renewal), sent by `teams-trouter` | [trouter.md](trouter.md#what-the-native-client-may-send-m1-task-15), [the native client](trouter.md#the-native-client-m1-task-16) · M1 T14, T15, T16 | seen |
| frame | `5:<n>+::{"name":"user.activity",…}` (`active`, `inactive`) | reports the client active (the account shows online) | own-state `trouter_session`; held back in `ab-trouter-quiet` (Unknown and denied there; expected deny: Trouter frames, so the run doesn't fail on it) | not used (no `OwnFrame`; `teams-trouter` doesn't send it) | [trouter.md](trouter.md#useractivity-and-presence) · M1 T14 | seen |
| frame | `3:::{"id":…,"status":200,…}` | the answer to a delivery | Unknown and denied in every scenario; expected deny: Trouter frames, so a run doesn't fail on it | not used: `teams-trouter` leaves every delivery unanswered, an incoming call's included (no response path) | [trouter.md](trouter.md#answering-deliveries) · M0 T18, M1 T14, T16 | seen |
| inbound | `POST /v4/f/<endpoint>/messaging` (a `3:::` delivery) | a chat event pushed to the endpoint (`NewMessage`, `MessageUpdate`, `ConversationUpdate`) | received, not judged | received by `teams-trouter`, never answered, turned into typed chat events (`teams_trouter::ChatEvent`: message, typing, edit, delete, reaction, read horizon, the chat's update; anything else `Unknown`, logged by kind only); a `callNotification` or a delivery at another path is ignored | [trouter.md](trouter.md#deliveries), [the native client](trouter.md#the-native-client-m1-task-16) · M1 T14, T16 | seen |
| inbound | `1::`, `5:::{"name":"trouter.connected",…}`, `5:::{"name":"trouter.message_loss",…}`, `6:::<n>+` | the socket is open; the endpoint, its push URL, the regional `reconnectUrl` and `connectparams`; pushes were lost; a ping's answer | received | `teams-trouter`: authenticates after `1::`, registers and moves to the regional host on `trouter.connected`, hands a loss report on for a catch-up, a pong missing 5 s means a reconnect | [trouter.md](trouter.md#trouterconnected-the-endpoint), [message loss](trouter.md#message-loss), [keep-alives](trouter.md#keep-alives-and-acknowledgements) · M1 T14, T16 | seen |
| inbound | `trouter.reconnect`, a disconnect with a reason | the service asks for a reconnect or closes | received | `teams-trouter`: a disconnect's reason decides (`unauthorized`: reconnect with a new token; `dup`: reconnect at once, never twice within 10 s; `skypetoken-deprecated`: stop; else a backoff); `trouter.reconnect` is left alone (not seen live) | [trouter.md](trouter.md#reconnect-parameters-and-backoff-from-the-bundle), [the native client](trouter.md#the-native-client-m1-task-16) · M1 T14, T16 | bundle only |

## AMS and the async gateway (`us-prod.asyncgw.teams.microsoft.com`, `us-api.asm.skype.com`)

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `POST` | `/v1/skypetokenauth` (both hosts) | trades the skypetoken for the media service's cookie | expected deny: async gateway / AMS skypetoken exchange | not on the allowlist | [auth.md](auth.md#audience--service-table) · M0 T11 | seen (all) |
| `POST` | `/v1/<guid>/aadtokenauth` (async gateway) | the same with the `ic3` Entra token | expected deny: async gateway AAD token exchange | not on the allowlist | [auth.md](auth.md#audience--service-table) · M0 T11 | seen (all) |
| `POST` | `/v1/objects/` (async gateway) | creates an image object readable by one conversation | forced deny in `ab-image`; denied Unknown (AMS objects aren't mapped) | not on the allowlist (images deferred out of M1) | [chat.md](chat.md#images-sending-denied-ab-image) · M1 T20 | seen (`ab-image`) |
| `PUT` | `/v1/objects/<id>/content/imgpsh` | uploads the image's content | denied Unknown | not on the allowlist | [chat.md](chat.md#images-sending-denied-ab-image), `docs/backlog/ams-image-sending.md` · M1 T20 | bundle only |
| `POST` | `/v1/objects/<id>/forward` | lets another conversation read an object | denied Unknown | not on the allowlist | [chat.md](chat.md#images-sending-denied-ab-image) · M1 T20 | bundle only |
| `GET` | `/v1/objects/<id>/`, `/v1/objects/<id>/views/<name>`, `…/views/<name>/status` | reads an object and its rendered views | read | not on the allowlist | [chat.md](chat.md#images-sending-denied-ab-image) · M1 T20 | bundle only |

## Presence (`https://<teams origin>/ups/<geo>/`)

`<geo>` is `noam` for the lab (GTM `presenceUPS`).

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `POST` | `v1/presence/getpresence/` | other people's presence | expected deny: presence reads | not on the allowlist | [auth.md](auth.md#audience--service-table) · M0 T11 | seen (all writing scenarios) |
| `PUT` | `v1/me/endpoints/` | creates the account's presence endpoint (availability) | expected deny in `ab-trouter` (presence endpoint); denied Unknown elsewhere | not on the allowlist | [trouter.md](trouter.md#useractivity-and-presence) · M1 T14 | seen (`ab-trouter`, `ab-trouter-quiet`, `echo-call`) |
| `POST` | `v1/pubsub/subscriptions/<epid>` | subscribes the endpoint to others' presence | expected deny in `ab-trouter` (presence subscriptions); denied Unknown elsewhere | not on the allowlist | [trouter.md](trouter.md#useractivity-and-presence) · M1 T14 | seen |

## Calling (`teams.microsoft.com/trap/`, `api.flightproxy.teams.microsoft.com/api/v2/`)

Services behind the flight proxy are addressed as `ep/<service host>/…`, each URL handed out by the service.

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `GET` | `https://teams.microsoft.com/trap/tokens` | TURN relay credentials, valid 7 days | read | not on `teams-proto`'s allowlist; the `spikes/echo-call` spike reads its answer (`TrapTokens`) | [calling-echo.md](calling-echo.md#the-turn-credentials), [echo-call.md](../spikes/echo-call.md) · M0 T18, T19 | seen (all) |
| `POST` | `epconv` | starts a call (the test call: `UserInitiatedTestCall`, no callee) | mapped `Call` → `New` + caller and the echo service (`Classifier::calling`, reviewed shape); denied Unknown elsewhere | `Authorized<Mutating>` (`StartCall`), no route in the transport (`wire` is `None`); the `spikes/echo-call` spike builds its body from an `Authorized<StartCall>` and sends nothing | [calling-echo.md](calling-echo.md#the-call-step-by-step), [echo-call.md](../spikes/echo-call.md) · M0 T18, T19 | seen (`echo-call`) |
| `GET` | `ep/<broker>/api/v1/subscribe/<id>/<n>` `?i` | long-polls the call's events | read | not on the allowlist | [calling-echo.md](calling-echo.md#the-call-step-by-step) · M0 T18 | seen (`echo-call`) |
| `POST` | `ep/<conv>/conv/<conversation>/add` `?i&e` | invites the echo service into the call | denied Unknown (call follow-ups aren't mapped) | `Authorized<Mutating>` (`AddToCall`), which the fence denies as partial today (no call member read); no native route | [calling-echo.md](calling-echo.md#the-call-step-by-step), `docs/backlog/fence-call-follow-ups.md` · M0 T18 | seen (`echo-call`) |
| `PUT` | `ep/<conv>/conv/<conversation>/updateEndpointMetadata` `?i&e` | updates the caller's endpoint metadata | denied Unknown | not used | [calling-echo.md](calling-echo.md#the-call-step-by-step) · M0 T18 | seen (`echo-call`) |
| `POST` | `ep/<conv>/conv/<conversation>/leave` `?i&e` | hangs up | denied Unknown | not used | [calling-echo.md](calling-echo.md#the-call-step-by-step) · M0 T18 | seen (`echo-call`) |
| `POST` | `ep/<cc>/cc/v1/mcProxy/<call>/…/applyChannelParameters` `?i&e` | a media line's send capabilities | denied Unknown | not used | [calling-echo.md](calling-echo.md#the-call-step-by-step) · M0 T18 | seen (`echo-call`) |
| `POST` | `ep/<cc>/cc/v1/callAcceptance/<call>/…/acknowledge` `?i` | acknowledges the call controller's acceptance | denied Unknown | not used | [calling-echo.md](calling-echo.md#the-call-step-by-step) · M0 T18 | seen (`echo-call`) |
| `POST` | `ep/<conv>/conv/<conversation>/getAllParticipants` `?i&e` | lists a call's or lobby's participants (empty for the test call) | denied Unknown (no allowance) | not used | [calling-echo.md](calling-echo.md#open) · M0 T18 | probe (sent once from inside the fence, 2026-10-20) |
| `POST` | the call's `acceptance` link | answers an incoming call | denied Unknown, never mapped (`CallAnswer`); no scenario clicks answer | not used (no request type) | [calling-echo.md](calling-echo.md#what-has-to-be-up-before-a-call) · M0 T18 | bundle only |
| `GET` | `https://api.interfaces.records.teams.microsoft.com/Teams.VoiceApps/obo-accounts/<guid>` | voice app accounts acting for the user | read | not on the allowlist | this file | seen, not reviewed (`echo-call`) |

## Microsoft 365 around Teams

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `POST` | `https://substrate.office.com/search/api/v1/suggestions` `?scenario` | search suggestions (people, files) | expected deny: search suggestions (never allowed: it lists other people) | not on the allowlist | [fence-targets.md](fence-targets.md#reads-by-post) · M0 T11 | seen (all) |
| `GET` | `https://substrate.office.com/search/api/v2/init` `?scenario&cvid` | initialises a search session | read | not on the allowlist | this file | seen, not reviewed (`ab-chat`, `ab-chat-full`, `echo-call`) |
| `GET` | `https://substrate.office.com/userknowledgebase/v1.0/<id>/teams/affinityscore` | people affinity scores (the Outlook token) | read | not on the allowlist | [auth.md](auth.md#audience--service-table) · M0 T11 | seen (all) |
| `GET` | `https://outlook.office.com/api/v2.0/me/messages` `?$filter&$select&$top&$expand` | the account's mail | read | not on the allowlist | this file | seen, not reviewed (`echo-call`) |
| `GET` | `https://graph.microsoft.com/v1.0/domains/` `?$select` | the tenant's domains | read | not on the allowlist | [auth.md](auth.md#audience--service-table) (Graph only) · M0 T11 | seen (`ab-chat`, `ab-image`, `ab-trouter`) |
| `GET` | `https://<tenant>.sharepoint.com/<id>/v2.0/shares` | resolves shared files | read | not on the allowlist | this file | seen, not reviewed (`ab-chat`, `ab-image`, `echo-call`) |
| `POST` | `https://<tenant>.sharepoint.com/sites/<site>/…/filebrowser.aspx` `?app&scenario&…` | the embedded file browser | denied Unknown | not on the allowlist | this file | seen, not reviewed (`ab-image`) |
| `GET` | `https://loki.delve.office.com/api/v1/livepersonacard/configuration` `?cultureName` | the live persona card's configuration | read | not on the allowlist | [auth.md](auth.md#audience--service-table) (the host) · M0 T11 | seen (all) |
| `GET` | `https://webshell.suite.office.com/api/shell/navbardata` | the Office shell's nav bar | read | not on the allowlist | [auth.md](auth.md#audience--service-table) (the host) · M0 T11 | seen (all) |
| `GET` | `https://admin.microsoft.com/admin/api/uxversion` `?bldVer` | the admin center's UX version | read | not on the allowlist | this file | seen, not reviewed (all) |

## Client services: config, telemetry, editor, AI

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `GET` | `https://config.teams.microsoft.com/config/v1/MicrosoftTeams/<id>`, `…/config/v1/Skype/<id>` (many client query parameters) | the client's remote configuration and flights (ECS) | read | not on the allowlist | this file | seen, not reviewed (all) |
| `GET` | `https://config.edge.skype.com/config/v1/CHILL/<id>` | another remote configuration | read | not on the allowlist | this file | seen, not reviewed |
| `POST` | `https://<name>.events.data.microsoft.com/OneCollector/<version>/` (`teams.`, `browser.`, `ic<n>.`) | telemetry | expected deny: telemetry | not on the allowlist | [fence-targets.md](fence-targets.md#what-the-captures-showed) · M0 T11 | seen (all) |
| `POST` | `https://browser.pipe.aria.microsoft.com/Collector/<version>/` | logging | expected deny: Aria logging | not on the allowlist | [fence-targets.md](fence-targets.md#what-the-captures-showed) · M0 T11 | seen |
| `POST` | `https://admin.microsoft.com/api/instrument/logclient` | the admin center's client log | expected deny: admin center client log | not on the allowlist | [fence-targets.md](fence-targets.md#what-the-captures-showed) · M0 T11 | seen (all) |
| `POST` | `https://editor.svc.cloud.microsoft/NLEditor/Config/V2`, `…/NLEditor/api/V1/LanguageInfo` | the compose box's spelling and grammar service | expected deny: editor service config, editor service language | not on the allowlist | [fence-targets.md](fence-targets.md#what-the-captures-showed) · M0 T11 | seen |
| `POST`, `WS` | `https://augloop.office.com/`, `wss://augloop.office.com/` | AI and suggestion features | expected deny: AugLoop posts, AugLoop frames | not on the allowlist | [fence-targets.md](fence-targets.md#what-the-captures-showed) · M0 T11 | seen (all) |

## The Teams page itself (`https://<teams origin>/`)

| Method | Path | What it does | Browser fence | Native | Notes · task | Evidence |
|---|---|---|---|---|---|---|
| `GET` | `/`, `/v2`, `/v2/` `?tenantId` | the web client's page | read | not used | [auth.md](auth.md#region-gtms) · M0 T11 | seen (all) |
| `GET` | `/v2/authv2` | the web client's sign-in redirect | read | not used | [auth.md](auth.md#client-and-redirect) · M0 T11 | seen |
| `GET` | `/l/chat/0/0` `?users`, `/l/chat/<chat>/0` `?tenantId` | deep links: a new chat with the users named, or a chat by id | read | not used | [fence-targets.md](fence-targets.md#how-a-run-uses-it) · M0 T11, M1 T8 | seen (all but `echo-call`, `load`) |
| `GET` | `/l/settings/devices` | the device settings deep link (reloads the page) | read | not used | [calling-echo.md](calling-echo.md#what-has-to-be-up-before-a-call) · M0 T18 | seen (`echo-call`) |
| `HEAD` | `/diagnostics/network` `?cb` | a connectivity check | read | not used | this file | seen, not reviewed |
| `GET` | static assets on `teams.public.onecdn.static.microsoft`, `res.cdn.office.net`, `statics.teams.cdn.office.net`, `aadcdn.msftauth.net`, `login.live.com` | the client's scripts, styles, images, sounds and sign-in pages | read | not used | this file | seen, not reviewed |
