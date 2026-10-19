# Fence targets: the chat service's member reads and writes

Status: 2026-10-20, M0 Task 11. Written from static analysis of the web bundle `26260.1717.5229.7049` (imported with
`proto-lab bundles import`, searched with `proto-lab bundles search` and read in the chat service's request builders),
plus [auth.md](auth.md) for the chat service's base URL and credential. **No live traffic**: the `load` capture this
mapping should be checked against is blocked, because the lab accounts have no Teams license and the web client never
loads for them (`docs/backlog/lab-accounts-have-no-teams-license.md`). What still needs a capture is listed in
[To confirm by capture](#to-confirm-by-capture).

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

Credential: `Authentication: skypetoken=<skypetoken>` (auth.md's service table). A `RegistrationToken` appears once an
endpoint is registered; whether reads need it is open.

What the fence accepts as the chat service (both the mapping and the live read): `https` only, no port, no user info,
and either an AFD host of the commercial cloud (`teams.microsoft.com`, `teams.cloud.microsoft`) with
`/api/chatsvc/<region>/v1/…`, or a host ending in `.msg.teams.microsoft.com` with `/v1/…`. The `.msg` host form is the
historical regional chat host; the bundle doesn't name it (the GTM value comes at runtime), so it is to confirm.

### Ids

| Id | What |
|---|---|
| `19:<oid>_<oid>@unq.gbl.spaces` | a 1:1 chat; its id names both members' Entra object ids |
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

The fence's live read (`LiveMembershipSource`) makes exactly one request, `GET v1/threads/<id>?view=msnp24Equivalent`
with the skypetoken, from Rust with its own HTTPS client (no proxy, no redirects), on the host and region of a chat
service request the armed lab page itself sent. It accepts the answer as complete only when:

- the status is 2xx and the body (at most 1 MiB) is a JSON object;
- no key at the top level or under `_metadata` looks like pagination (`next…`, `…continuation…`, `skiptoken`,
  `syncState`);
- any member count (`memberCount`, `membercount`, `membersCount`, top level or under `properties`) equals the
  number of listed members;
- `id` is exactly the requested thread, and every member has a string `id`;
- **and, since only 1:1 chats are supported,** the members are exactly the two `8:orgid:<oid>` the chat's id names.

That last check is what makes the answer evidence of the *full* membership rather than a view filtered by the
caller's permissions: a 1:1 chat's membership is fixed by its id, and the read only confirms the thread exists with
exactly those members. Group and meeting chats, channels, teams and calls are `Partial(Unsupported)` and so denied:
the bundle shows paging (`thread_members`) and a separate filtered view (`filtered_thread`), and nothing static says
whether the plain thread read lists every member of a large chat. Pagination and truncation are always `Partial`.

## Writes

Every mutating request the mapping recognizes, with its `WriteKind` and `Target` (all relative to the base; `<chat>`
is a chat id, `1:1` or group, never a channel).

| Request | Body | WriteKind | Target |
|---|---|---|---|
| `POST v1/users/ME/conversations/<chat>/messages` | `messagetype` `Text`, `RichText`, `RichText/…` | `Message` | `Conversation(<chat>)` |
| same | `messagetype` `Control/Typing`, `Control/ClearTyping` | `Typing` | `Conversation(<chat>)` |
| `PUT v1/users/ME/conversations/<chat>/messages/<msg>` | the edited message | `Edit` | `Conversation(<chat>)` |
| `DELETE v1/users/ME/conversations/<chat>/messages/<msg>?behavior=softDelete` | none | `Delete` | `Conversation(<chat>)` |
| `PUT`/`DELETE v1/users/ME/conversations/<chat>/messages/<msg>/properties?name=emotions` | the reaction | `Reaction` | `Conversation(<chat>)` |
| `PUT v1/users/ME/conversations/<chat>/properties?name=consumptionhorizon` | the read horizon | `ReadReceipt` | `Conversation(<chat>)` |
| `POST v1/threads` | `{members: [{id, role}], properties}` | `ChatCreate` | `New`, adding every member in the body |
| `PUT v1/threads/<chat>/members` | `{members: [{id, role}]}` | `MemberChange` | `Conversation(<chat>)`, adding the body's members |
| `PUT v1/threads/<chat>/members/<mri>` | the member's role or property | `MemberChange` | `Conversation(<chat>)`, adding `<mri>` |
| `DELETE v1/threads/<chat>/members/<mri>` | none | `MemberChange` | `Conversation(<chat>)` |

A message post's body is the message: `content`, `messagetype`, `contenttype`, `clientmessageid`, `imdisplayname`,
`properties`. A reply in a reply chain posts to the chain's conversation id (`…;messageid=<root>`).

Rules that keep the mapping fail-closed:

- The kind of a message post comes from its body. A body the browser didn't inline, one that isn't JSON, or a
  `messagetype` not in the table (`Control/…` other than typing, `Event/…`, …) is `Unknown` and denied.
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
  - bot invokes (`POST v1/agents/<bot mri>/invoke`), a `BotReply` candidate;
  - the user's own properties (`PUT v1/users/ME/properties?name=<n>`), endpoint registration
    (`PUT`/`DELETE v2/users/ME/endpoints/<id>`) and the active-endpoint post (`activeendpoint`). These change only the
    account's own state and are **candidates for the own-state list**, but they are not on it: nothing is added there
    without a capture of what they carry and a review. Until then a writing run sees them denied (`ab-chat` fails on
    them, which is the honest outcome).
- Calls (`Call`, `CallAnswer`), meetings, invites and channel posts aren't mapped: calling goes through other services
  (Task 18's capture), and the middle tier (`/api/mt/…`) and CSA (`chatsvcagg`) writes haven't been looked at.

## How a run uses it

- `load`, `status` and `login` stay fail-closed: the seeded classifier maps no write, and the membership source is
  `FailClosed`.
- `ab-chat` uses `Classifier::mapped` with `LiveMembershipSource`. The browser fence hands the source the account's
  skypetoken from the page's own chat service requests (`HttpFence::with_credentials`, from the `Fetch.requestPaused`
  headers before anything else reads the event). A write to the a–b 1:1 chat passes only when the read confirms its two
  members are lab accounts.
- `ab-chat`'s live enforcement test forces a deny (`ForcedDeny`) on the probe message: the post must fail in the page
  with `net::ERR_BLOCKED_BY_CLIENT`, and b must never see it.

## To confirm by capture

Blocked until the lab accounts get a Teams license. `proto-lab run load` on 2026-10-20 (account a, fail-closed)
recorded only the stop: authz answered 403 with `errorCode` `UserLicenseNotPresentTrialEligible`, and the client made no
chat service request at all. Its mutating requests were the Entra token refresh and two authz posts (own state,
allowed) and three telemetry posts to `*.events.data.microsoft.com` (denied). With a license, the `load` and `ab-chat`
captures should show:

- [ ] the chat service base the client uses for our tenant (`chatServiceAfd` → `teams.microsoft.com/api/chatsvc/<region>/`,
      or a `.msg.teams.microsoft.com` host), and that its requests carry `Authentication: skypetoken=…`;
- [ ] that `GET v1/threads/<1:1 id>?view=msnp24Equivalent` answers a 1:1 chat with `id` and both members in
      `members` (`8:orgid:<oid>`), with no pagination or count fields that would make it `Partial`;
- [ ] the same read for a group chat: whether `members` is the whole list (and for how many members), and what
      `thread_members` paging looks like, before group chats are supported;
- [ ] each write in the table as the client really sends it (method, path, body fields, `messagetype` values, the
      reaction and read-horizon property names), and the reply-chain form of a conversation id;
- [ ] that the 1:1 thread id's two object ids are always the two members (federated and guest chats look different);
- [ ] what endpoint registration and the user's own properties carry, for the own-state review;
- [ ] whether reads need the `RegistrationToken` too.

Then replace the synthetic fixtures in `tests/fixtures/chatsvc/` with sanitized exports of those captures.
