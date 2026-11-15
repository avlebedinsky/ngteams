# The own server (`ngteams-server`)

`crates/ngteams-server` is a Teams-compatible server ngteams can sign in to instead of Microsoft's. It speaks the
wire the client already speaks to Microsoft (the shapes in `docs/protocol/`), with every service under one HTTPS
origin. The client sends a request to it only for an account signed in to that server.

What it serves today: sign-in (the token endpoint), authz, the chat service, teams and channels (CSA, the middle
tier's and the provisioning service's team routes), pictures in messages, files sent in chats, link previews, the
calendar and meetings' scheduling (the middle tier's calendar routes), account administration (the admin API and the
first-admin setup), the registrar and the Trouter push socket, and 1:1 calls (the call controller behind
`/flightproxy/api/v2/` and the TURN credentials). The registrar, Trouter and the calling surface have no GTM (the
client finds them on the origin). Other services are added one at a time. Authz names only the services the server
serves, so the client keeps every other service off.

The chat service serves chat creation, conversation sync, thread/member reads, message posting, edits, soft deletion,
reactions, transient typing, account preferences and message pages (newest, backward, incremental and around a
requested time) under `/api/chatsvc/<region>/v1/`. These routes require an IC3 bearer token and current membership;
disabled users are refused even while their token is live. A repeated 1:1 roster returns the existing chat, and
group rosters and names persist in SQLite or PostgreSQL. Requests are limited to 64 KiB. Authz advertises
`chatServiceAfd`. It also serves the account's own streams (notes, the activity feed, the mentions stream) and a
group chat's people (below). Mentions and quotes are stored as Teams gives them back
([Rich messages](#rich-messages)).

## Stack

| Need | Library |
|---|---|
| HTTP | axum 0.8 on hyper 1 and tower; tower-http for request timeouts, body limits and traces |
| Push socket | tokio-tungstenite on hyper's upgrade (the tungstenite the client links; axum's `ws` would add a second one) |
| TLS | rustls (aws-lc-rs) through axum-server, or plain HTTP behind a reverse proxy |
| Database | Diesel 2.4: Postgres through diesel-async (tokio-postgres, no libpq), SQLite through Diesel's own connection on the blocking pool; bb8 pools; embedded migrations per backend |
| Passwords | argon2 (Argon2id, PHC strings) |
| Tokens | jsonwebtoken (EdDSA, Ed25519 keys from aws-lc-rs, `kid` per key); opaque refresh tokens kept as SHA-256 |
| Fan-out, replay, counters | redis-rs 1.8: a connection manager with RESP3 pushes (a broken subscription ends its subscribers); Redis Streams for the replay journal (Redis 6.2 or later, or Valkey) |
| Files | object_store: any S3-compatible service, a local directory, or memory |
| Pictures, thumbnails | image (PNG, JPEG, GIF, WebP), decoded within size and memory limits |
| Link previews | reqwest behind an address guard, scraper for the page's metadata |
| Rich messages | scraper (html5ever, attributes in source order) writes a mention's message out again; indexmap keeps a mention entry's keys in order |
| Download ranges, media types | http-range-header, mime_guess |
| Configuration, CLI | figment (TOML plus environment), clap |
| Logs | tracing, tracing-subscriber |

SQLite comes from the `libsqlite3-sys` the workspace already links (rusqlite pins it); sqlx can't share that version,
so Diesel serves both backends. fred (the planned Redis client) can't resolve beside PipeWire's `libspa`: its
`redis-protocol` pins `cookie-factory =0.3.2`.

## Running it

```sh
ngteams-server --config server.toml migrate
printf '%s\n' "$PASSWORD" | ngteams-server --config server.toml user add ada@example.com --name "Ada Lovelace"
ngteams-server --config server.toml serve
```

Commands: `serve`, `migrate`, `user add|list|disable|enable|reset-password|promote|demote|delete`,
`media list|delete`, `setup-code`, `keys rotate`. A password is read from the first line of standard input, never from the command line. Disabling or
deleting an account, or resetting its password, revokes its refresh tokens and closes its push sockets at once: a
disabled or deleted account is refused everywhere from then on, and after a new password the sessions end when
their access tokens run out (one hour by default). `user add --admin` and `user promote` make an admin; `user list`
ends each line with `admin` or `user`. The command line keeps the same guards as the admin API (the last enabled
admin stays) and its actions are audited as `cli`.

`media list <upn>` prints what an account keeps in the store, a line each: `file` or `picture`, id, bytes, when
it was stored, name (tab-separated). `media delete <upn> <id>…` deletes them by id: the bytes leave the store and
the account's quota, a file's shares and links end, and a message that showed a deleted picture shows none. It is
the operator's way to free an account that is at its quota ([Freeing storage](#freeing-storage)).

With `redis_url` set, `user disable`, `user delete` and `user reset-password` make the change in the database first
and then tell the account's open push sockets through Redis. When Redis is down the change stays: the command prints
a warning on standard error that says the account is disabled or deleted (the password is replaced) and exits 0.
Nothing more is pushed to the account, as each of its open sockets, on every node, ends at its next delivery check;
the command need not be run again. A command that changed nothing (no such account, a short password, no database)
exits 1 with the error. The admin API's same changes (and an account's own password change) end the sockets the
same way; when Redis is down they answer as made and the node logs a warning. Errors and warnings carry colour codes
only when standard error is a terminal.

`NGTEAMS_SERVER_LOG` controls logging (default `info`); dependency debug and trace events are always suppressed
because database drivers can include secret bind values.

`GET /health/live` answers 204 while the process runs. `GET /health/ready` answers 204 while the database and the
fan-out answer, else 503.

## Configuration

Defaults, then the TOML file (`--config` or `NGTEAMS_SERVER_CONFIG`), then `NGTEAMS_SERVER_*` variables. A nested key
takes a double underscore: `NGTEAMS_SERVER_TLS__CERT`, `NGTEAMS_SERVER_TOKENS__ACCESS_LIFETIME`. Logs follow
`NGTEAMS_SERVER_LOG` (an `EnvFilter`, default `info`).

| Key | Default | Meaning |
|---|---|---|
| `listen` | `127.0.0.1:8080` | address to listen on |
| `public_origin` | `http://127.0.0.1:8080` | the origin clients use; every base authz hands out is under it. `https` (plain `http` only on loopback) |
| `database_url` | `sqlite://ngteams-server.sqlite` | `postgres://…` or `sqlite://<path>` |
| `database_pool` | `16` | Postgres connections per node (SQLite: one) |
| `redis_url` | none | `redis://…` or `rediss://…`; without it, fan-out and login counters stay in the node |
| `redis_namespace` | the tenant id in the database | the deployment's part of that Redis: every key and channel is `ngts:<namespace>:…`. 1 to 64 letters, digits, `.`, `_` or `-`. Set it only where two deployments on one Redis hold the same tenant id (a database restored from another's); every node of a deployment takes the same value |
| `storage_url`, `storage_options` | `memory:///`, none | where pictures and files live: `s3://<bucket>[/<prefix>]` on any S3-compatible service, or `file:///<dir>` on one node ([Storage](#storage)). `memory:///` forgets everything at exit |
| `storage.connect_timeout`, `storage.request_timeout` | `3`, `30` | seconds a node waits for a connection to an S3 store, and for its answer or for a download that stands still ([Storage](#storage)) |
| `storage.max_retries`, `storage.retry_timeout` | `3`, `8` | how often a request the store didn't answer is sent again, and the seconds after its first attempt past which it isn't |
| `media.max_picture_bytes` | `25165824` | the largest picture pasted into a message (24 MiB, the most the client reads back) |
| `media.max_file_bytes` | `262144000` | the largest file sent in a chat (250 MiB, the client's own limit) |
| `media.user_quota_bytes` | `5368709120` | what one account may store, pictures and files together (5 GiB); `0` is no limit |
| `media.link_lifetime` | `3600` | seconds a file's download or thumbnail link works |
| `media.link_previews` | `true` | whether the server fetches pages and pictures for link previews |
| `media.preview_timeout` | `5` | seconds one preview fetch may take |
| `media.preview_max_page_bytes`, `media.preview_max_picture_bytes` | `1048576`, `5242880` | the most of a page read for its preview; the largest picture fetched for one |
| `media.preview_allow_private_addresses` | `false` | lets previews reach loopback and private addresses. For tests only |
| `tls.cert`, `tls.key` | none | PEM chain and key; without them the server speaks plain HTTP for a proxy in front |
| `region`, `partition` | `own`, `own01` | the names in authz's answer and the regional bases |
| `audiences` | the client's resources | the resources a token may be asked for |
| `tokens.access_lifetime` | `3600` | seconds an access token (and the skypetoken exchanged for it) lives |
| `tokens.refresh_lifetime` | `7776000` | seconds a refresh token lives (90 days) |
| `tokens.key_rotation` | `2592000` | seconds a signing key signs before a new one replaces it (30 days) |
| `tokens.reuse_grace` | `60` | seconds a spent refresh token may still be redeemed; after that it revokes its sign-in |
| `login.max_failures`, `login.window` | `10`, `900` | password attempts allowed in the window; success clears the count |
| `setup.require_code` | `true` | whether creating the first admin takes the setup code; turn it off only for an install nobody else reaches before its admin exists |
| `setup.max_attempts`, `setup.window` | `5`, `300` | first-admin setup attempts a client address may make in the window (seconds) |
| `trouter.auth_timeout` | `10` | seconds a push socket has to authenticate after it opened |
| `calling.ring_timeout` | `30` | seconds a call rings before it ends unanswered |
| `calling.turn.urls` | none | the TURN relay's URLs (`turn:<host>:3478?transport=udp`, `turns:<host>:5349`), handed out with the credentials |
| `calling.turn.secret` | none | coturn's `static-auth-secret`: credentials are made from it per account and run out by themselves |
| `calling.turn.username`, `calling.turn.password` | none | one long-term user of the relay (coturn's `user=`), used when no secret is set |
| `calling.turn.realm`, `calling.turn.ttl` | `rtcmedia`, `86400` | coturn's `realm`; seconds a credential made from the secret holds |
| `[[rooms]]` | none | the meeting rooms the room finder lists, each `name`, `address` (its mailbox) and optionally `list` (its building), `capacity`, `floor`, `street`, `audio_device`, `video_device`, `display_device`, `wheelchair` |

## Sign-in and tokens

- `POST /oauth2/v2.0/token` takes Entra's form: `grant_type=password` (`username`, `password`) for a sign-in,
  `grant_type=refresh_token` for every later token, each with `client_id` and one resource's `scope`
  (`<resource>/.default`, beside `openid profile offline_access`). The token's `aud` is that resource. The answer
  and the errors have Entra's shapes (`invalid_grant` with `AADSTS50126` for a wrong password, `50057` for a
  disabled account, `70000` for a refresh token that is not valid), so the client's `TokenClient` and `Session` run
  unchanged with the server as authority.
- Refresh tokens are single use. Each grant returns the next one of the same sign-in. One presented again after
  `tokens.reuse_grace` revokes that whole sign-in.
- Password attempts are reserved per account before verification (in Redis when configured). Success clears the
  count. After `login.max_failures` the
  account's password sign-ins get 429 with `Retry-After` until the window ends.
- An account whose password must be changed first (an admin's reset with `must_change_password`) gets
  `invalid_grant` with `AADSTS50055` and `suberror: user_password_expired` for its right password, as Entra answers
  an expired one. It gets tokens again after `POST /api/account/v1/password`.
- An admin's access and ID tokens carry `roles: ["admin"]`; other accounts' tokens have no `roles` claim. The claim
  says what the account was when the token was minted (up to `tokens.access_lifetime` ago).
  `GET /api/account/v1/me` says what it is now.
- `POST /api/authsvc/v1.0/authz` takes an access token for `https://api.spaces.skype.com` and answers with a
  skypetoken, `region`, `partition` and `regionGtms`, as Teams' authz does. The skypetoken runs out when the
  exchanged token does. A disabled account gets 403 `UserNotFoundInSkypeTeam`.
- Access tokens, ID tokens and skypetokens are EdDSA JWTs with the issuer `<origin>/<tenant>/v2.0`. Each node checks
  them against the shared keys and reloads the keys when it meets a `kid` it doesn't hold. The tenant is one GUID per
  server, made by the first node and kept in the database. Accounts are `8:orgid:<oid>`.

## Accounts and administration

An account is an admin or not (`users.is_admin`). Admins manage accounts from ngteams through the admin API; the
operator has the same actions on the command line. All bodies are JSON.

### The first admin

- `GET /api/admin/v1/setup` (no credentials) answers `{"needs_admin": bool, "code_required": bool}`. `needs_admin`
  is true while the server has no enabled admin. The client asks this before sign-in and offers to create the admin.
- `POST /api/admin/v1/setup` with `{"code", "upn", "display_name", "password"}` creates the first admin (201, the
  account). It answers 403 `SetupCode` for a missing or wrong code, 409 `AdminExists` once an admin exists, and
  429 `TooManyAttempts` with `Retry-After` after `setup.max_attempts` attempts from one client address.
- The setup code: a node that starts without an admin issues a code and logs it once, at `warn`
  (`… setup code: XXXXX-XXXXX-XXXXX-XXXXX-XXXXX`). `ngteams-server setup-code` issues a new code, which replaces
  the pending one, and prints it. The database keeps only the code's SHA-256, so any node checks it. Creating the
  first admin spends it. The code is read in any case, with or without the dashes.
- `setup.require_code = false` drops the code: whoever reaches the server first creates the admin.
- An admin made on the command line (`user add --admin`, `user promote`) also ends the setup.

### Admin API

Every request carries `Authorization: Bearer <access token for https://api.spaces.skype.com>`. Without a valid
token the answer is 401. The server looks the account up on every request: an account that is not an enabled admin
now gets 403 `NotAdmin` (or `AccountDisabled`), whatever its token's `roles` say.

| Request | Does | Answers |
|---|---|---|
| `GET /api/admin/v1/users?q=&after=&limit=` | lists accounts by UPN. `q` matches a part of the UPN or name, any case (on Postgres a name's case folds as the database's locale does). `limit` is 1–200 (default 50). `after` is the previous page's `next` | 200 `{"users": [account], "next": upn or null}` |
| `POST /api/admin/v1/users` | adds an account: `upn`, `password`, optional `display_name` (default: the UPN), `admin`, `must_change_password` | 201 account |
| `GET /api/admin/v1/users/{id}` | reads an account | 200 account |
| `PATCH /api/admin/v1/users/{id}` | changes any of `display_name`, `upn`, `enabled`, `admin` (assign or remove an admin). `enabled: false` signs the account out everywhere | 200 account |
| `DELETE /api/admin/v1/users/{id}` | deletes an account | 204 |
| `POST /api/admin/v1/users/{id}/password` | sets `password`, signs the account out everywhere; with `must_change_password: true` the account must replace it before it signs in | 204 |
| `GET /api/admin/v1/audit?before=&limit=` | lists admin actions, newest first. `before` is the previous page's `next` | 200 `{"entries": [entry], "next": id or null}` |

An account is `{"id", "mri", "upn", "display_name", "enabled", "admin", "must_change_password", "created_at",
"updated_at"}` (times in Unix seconds). A refusal is `{"error": {"code", "message"}}`:

| Status | `code` | When |
|---|---|---|
| 400 | `InvalidUpn`, `InvalidName`, `WeakPassword`, `SamePassword` | a UPN that isn't `name@domain`, a name over 256 characters, a password under 8 characters, a new password equal to the current one |
| 403 | `NotAdmin`, `AccountDisabled`, `SetupCode`, `InvalidCredentials` | |
| 404 | `NotFound` | no such account (a deleted one too) |
| 409 | `UpnTaken`, `LastAdmin`, `AdminExists` | |
| 429 | `TooManyAttempts` | with `Retry-After` |

A body that isn't the expected JSON gets axum's 400, 415 or 422.

Guards: a change that would leave the server without an enabled admin is refused with 409 `LastAdmin`. That covers
removing the role from, disabling or deleting the last enabled admin, by itself or by the command line. Each guarded
change runs in one transaction under a database lock, so two nodes can't each remove "the other" admin.

Deleting an account removes its sign-in: the UPN, password, role and refresh tokens go, and the UPN is free for a
new account (a new id). The row stays as a disabled account named "Deleted user" with the same id, so messages it
wrote keep an author. A deleted account's access tokens open nothing: authz and the admin API refuse it.

Audit: every change (`setup.first_admin`, `user.add`, `user.update`, `user.delete`, `user.password_reset`,
`user.password_change`) writes a row to `admin_audit` in the same transaction, and a `tracing` event at `info`. An
entry is `{"id", "at", "actor_id", "actor", "action", "target_id", "target", "detail"}`. `actor` is the admin's
UPN, `cli` or `setup`. `detail` holds the changed values, never a password. A refused change writes nothing.

### The account's own endpoints

- `GET /api/account/v1/me` (the same bearer token, any enabled account) answers the account as the database has it
  now. The client reads `admin` here to show its admin features.
- `POST /api/account/v1/password` with `{"upn", "current_password", "new_password"}` replaces the account's own
  password and signs it out everywhere (204). It takes no token, because an account that must change its password
  can't get one. The current password is the credential: a wrong one, an unknown account and a disabled account all
  get 403 `InvalidCredentials`. Attempts count toward `login.max_failures`, as sign-ins do (429 when held).

## Teams and channels

The client's team pages and channel dialogs find their routes at four GTMs: `chatSvcAggAfd` (CSA), `middleTier`,
`teamsAndChannelsProvisioningService` and `userProfileService`. The shapes are those of `docs/protocol/channels.md` and `teams.md`; each
route takes the access token of its own service's audience and looks the account up, so a disabled account's live
token opens nothing.

- **A team** has a thread id (`19:<id>@thread.tacv2`, its general channel's) and a group id (a GUID). Its creator
  is its owner. **A channel is a thread of the chat service**: a standard channel's members are the team's people,
  a private channel's the people its owners put in it (people of the team only). So a channel's messages, edits,
  reactions and read state are the chat service's, behind its membership check. Of a team's channels only the
  general one is in the conversation list; the others are read by their id.
- **Posts and replies.** A message posted to a channel is a post: its `conversationLink` ends in
  `;messageid=<its own id>`. A reply is posted to `<channel>;messageid=<post>` and is kept with the channel's
  messages under that post; the chain's own page (`…/conversations/<channel>;messageid=<post>/messages`) holds the
  post and its replies. A reply needs a post of that channel that isn't deleted. An archived channel, or a channel
  of an archived team, takes no new message.
- **CSA**: `GET api/v3/teams/users/me` lists the account's teams with the channels it may see, its role, the
  team's settings as `teamSettings`, and the account's `conversationFolders` (its channel sections). The sections'
  writes (`POST …/conversationFolders`, `PATCH …/batchUpdateExpandedState`) name the hierarchy version they were made
  against; another version answers 409 and changes nothing. A team's tags: `tagCards`, `tagSettings`, `tags`
  (`PATCH` with `if-match: <version>`, 412 for another version).
- **Middle tier** (`beta/…`): `teams/create`, `PATCH teams/<team>`, `DELETE teams/<team>/delete`, `joinCode`,
  `bulkUpdateRoledMembers`, `members` (`Promoted`, `Demoted`, `Removed`, `Left`, `DemotedAndLeft`),
  `sponsorJoinRequest`, `findJoinRequests`, `joinRequestAdminAction`, `joinRequest`, `joinByCode`, `suggested`,
  `find`, standard channels (`teams/<team>/channels[/<channel>]`, `setDefaultChannel`), the team's picture (PNG or
  JPEG, at most 4 MiB, kept in the file store) and `analytics/…/summary-timeseries` (counted from the channels'
  messages: posts, replies, reactions, mentions and the people who wrote, by day).
- **Provisioning**: private channels (`teams/<group>/channels`, their `users`, archive and restore), a channel's
  moderators (`channels/<channel>/members`), `groups/<group>/archive|unarchive|channels|clone`, the account's own
  `team/<team>/hide` and `conversations/show`, and `users/me/invitations` and `teamtemplates/v1.0/en-us`, both
  empty.
- **Chat service**: `PUT v1/threads/<thread>/properties?name=spaceAdminSettings` (a team's settings, on its general
  channel) and `?name=channelSettings` (a standard channel's moderation), from a team's owner;
  `PUT v1/threads/<channel>/members/<own mri>` (the account's own interest); and the account's own channel
  properties (`favorite`, `isGeneralChannelFavorite`, `channelNotificationSettings`, `followAllRc`, `notifyAllRc`,
  `alerts`) on `v1/users/ME/conversations/<channel>/properties`.
- **People pickers** (reads by `POST`, the middle tier's token): `beta/users/searchV2` with the text as a JSON
  string lists up to 15 accounts that can sign in whose name or address holds it (Add member's box);
  `<userProfileService>/api/v1.0/teams/<team>/users/profilesearch` with `{"keyword"}` lists the team's people the
  same way, for the team's people (a private channel's box).
- **Graph**: `GET /graph/v1.0/me` (the token's account), which the client reads before each account-own write.

Who may do what, checked against the database on every request:

| Who | May |
|---|---|
| a team's owner | edit, archive, restore and delete it; add, promote, demote and remove people; read, make and take away its code; answer join requests; change its settings, tag settings and picture; archive, restore, show by default and moderate standard channels; copy it |
| a member | what the team's settings allow (`createTopic`, `updateTopic`, `deleteTopic`, `createPrivateSpace`); add members to a public team; ask a private team's owners to let people in; leave; tags when the tag settings allow it |
| a private channel's owner | rename, archive, restore and delete it; add, change and remove its people |
| anyone signed in | make a team; find and join public teams; join by a code; ask to join a private team by its group id |

A team and a private channel always keep an owner (403 `LastOwnerCannotBeRemoved`). When a private channel's only
owner leaves the team or is removed from it, the channel's longest-standing other member becomes its owner; a
channel with nobody else in it is deleted. Someone outside a team gets 404 `NotFound` for every CSA, middle-tier
and provisioning route that names it, and someone outside a private channel for every such route that names the
channel; the chat service's thread routes answer their own 403 to anyone not in the thread. A refusal is `{"errorCode", "message"}`; the codes the client's dialogs word are Teams' own
(`ChannelNameAlreadyExist`, `OperationNotAllowedOnArchivedTeam`, `NoPermissionToCreateChannel`,
`CannotCreatePrivateChannelDueToUserPolicy`).

These changes are published to the team's or channel's people as a `ThreadUpdate` on `push:<mri>`: a team made,
edited, archived, restored or deleted (the thread as it last was, with `isDeleted` true), its people and their
roles, its settings, and a channel made, renamed, archived, restored, deleted, moderated or given other people. A
channel's show-by-default, the team's picture, codes and tags reach other clients only with their next teams read.

Limits: 200 standard and 30 private channels a team (`MaxChannelLimitExceeded`,
`MaxPrivateChannelLimitExceeded`), 200 tags a team, 200 sections an account and 1000 channels a section; a team's
code is 16 characters.

Not served yet: a channel's email address, team apps and templates, the default team picture, channel moderation
enforced on posts (the settings are kept and read back), and system
messages (`ThreadActivity/…`) for a team's changes.

## The calendar and meetings

The client's calendar, scheduling form and channel meetings find their routes on the middle tier, with the
account's token for `https://api.spaces.skype.com`; the shapes are those of `docs/protocol/calendar.md` and of
"A channel's meeting" in `calling.md`. Scheduling is served here; a meeting's call and media are not.

- **Events**: `GET v2.0/me/calendars/default/calendarView` (a window of at most 100 days; the client asks for 62
  at most), `POST v2.0/me/calendars/default/events`, and `GET`, `PUT`, `DELETE v2.0/me/calendars/events/<id>` with
  `…/cancel` and `…/respond`. Times are kept as instants and answered in UTC; an event keeps the zone it names
  (`eventTimeZone`, by the middle tier's names: `TurkeySt`) and `beta/me/calendarEvents/timeZoneSettingsWithOffset`
  names an IANA zone that way. What is kept of a sent event is a fixed list of the form's own fields; who
  organizes it, who answered what and the meeting's links are the server's. A calendar request's body is at most
  1 MiB (an event's text can be long); a larger one is refused (400 `BadRequest`).
- **A series** (`eventRecurrencePattern`, `eventRecurrenceRange`) is one row, read as its occurrences: the rule
  runs on the clock of the series' own zone (the `rrule` crate), so an occurrence keeps its time of day across a
  daylight-time change. An occurrence's id is the series' id, a dot and the occurrence's own start. One occurrence
  changed, answered, cancelled or taken off a calendar is kept as a row of its own under the series; a change of the
  series' rule or times forgets those rows. An answer or a removal of an occurrence that meets a change or a
  cancellation of its series is checked again against the series as it then is; one that keeps meeting changes is
  refused (409 `Conflict`).
- **Whom an event invites** are accounts of this server, named by MRI or by address (the sign-in name); another
  address is refused (400 `UnknownAttendee`), since the server sends no mail. Each of them has the event on its
  own calendar with its own answer (`respond`), reads whom it invites, and may take it off (`DELETE`). The
  organizer's change reaches everyone it now invites (a new time, of the event or of one occurrence, asks each of
  them again; who is taken off no longer has it), and its cancellation leaves the event cancelled (`isCancelled`, `Canceled: …`) on their
  calendars until they take it off. An event nobody else has is deleted outright.
- **A Teams meeting** is a chat-service thread, `19:meeting_<id>@thread.v2`, made by
  `POST beta/me/calendarEvents/privateMeeting/schedulingService/create` (`Scheduled` for the event the form then
  saves with it, or `MeetNow`, which has no event). Its join link is
  `<origin>/l/meetup-join/<chat>/0?context={"Tid","Oid"}` and its short link `<origin>/meet/<id>?p=<passcode>`;
  the thread's `properties.meeting` names its organizer, tenant, title and link. An event takes only a meeting its
  own organizer made and no other event uses, checked as the event is written; an event changed to a Teams meeting
  gets one, and so does a series one of whose occurrences is. The people the event
  invites are put in the chat, those one occurrence of a series invites too; who no longer has an invitation to
  the event or to any occurrence of it is taken out. The chat holds the invitation: a message whose `properties.meeting` names the
  meeting (`ScheduledMeetingCreated`), changed in place when the meeting is, with a cancellation
  (`ScheduledMeetingCancelled`) after it.
- **A channel's meeting**: `POST v2.0/groups/smtp:<team mailbox>/calendars/default/events`, and for its `iCalUID`
  `GET …/calendars/events/iCalUId/<uid>`, `PUT …/calendars/events/<uid>` and `…/<uid>/cancel`. The team's mailbox
  is `<group id>@<the origin's host>`. The meeting is posted to its channel (its link names the channel and that
  post), its change edits the post in place and its cancellation replies under it. Who scheduled it has an
  accepted copy on its own calendar, as has anyone it names and, with `inviteAllMembers`, everyone in the channel.
  A change or cancellation of one occurrence names it by `fetchEventContract`: its kind and its start, which for
  an `Exception` is the start it was moved to. An `Exception` no live changed occurrence starts at
  answers 404: a cancelled one is not found again, and neither is the series' own occurrence at that time.
  A change that would put an occurrence at the start another live occurrence of its series is at, moved there or
  there by the series' rule, answers 409 `Conflict`: the start alone would not tell the two apart. An occurrence
  cancelled or moved away leaves its start free.
- **The Scheduler**: `PUT v2.0/me/calendars/schedules` (each person's busy times; what an event is about only
  for the caller's own) and `findMeetingTimes` (the half hours, 08:00 to 18:00 counted from the midnight the
  read starts at, at which everyone asked about is free).
- **Meeting options**: `GET` and `POST beta/meetings/v1/options/<tenant>/<organizer>/<chat>/0/0`, kept per chat
  (a meeting's, or a group chat's for its calls): `AutoAdmittedUsers`, `PresenterOption`, `AllowMeetingChat`,
  `AttendeeRestrictions`.
- **Rooms**: `GET v2.0/places/roomLists`, `places/rooms` and `places/<list address>/roomList/rooms` list the
  `[[rooms]]` of the configuration. A room is a place, not an account: an event that names one invites nobody by
  it.

Who may do what, checked against the database on every request:

| Who | May |
|---|---|
| an event's organizer | read, change, cancel and delete it |
| an account it invites | read it with whom it invites, answer it, take it off its own calendar |
| who scheduled a channel's meeting | change and cancel it on the team's calendar |
| anyone in a meeting's channel (for a standard channel, anyone on the team) | read the channel's meeting from the team's calendar; schedule one there, while neither the team nor the channel is archived |
| the people in a meeting's or a group chat | read its meeting options; the organizer the path names saves them (a meeting's own organizer) |
| anyone signed in | make an event or a meeting of its own; ask for any account's busy times; list the rooms |

Anyone else gets 404 `NotFound` for an event, a meeting or a team's calendar: it learns nothing of it. A private
channel's meeting is hidden that way from the team's people outside the channel. An invited account that tries to
change or cancel gets 403 `Forbidden`.

What reaches the invited accounts beside their calendars, on `push:<mri>`: the meeting chat's `ThreadUpdate` and
the invitation, change and cancellation as that chat's (or the channel's) messages, and for each a `NewMessage` of
the account's `48:notifications` stream: an activity item (`msGraph`, `privateMeetingCreated`,
`privateMeetingUpdated`, `privateMeetingCanceled`, or `channelMeeting…`) naming who did it, the meeting's time and
the event it opens. The item is kept in the account's activity feed as a mention's is (see "The account's own
streams"), so a client that was away syncs it, and the account marks it read or removes it. An invitation is
always written; a change or a cancellation is not written for an account that muted the meeting's chat or channel
(`alerts` `false`), and nothing is written for a disabled account.

Not served: mail of any kind (invitations, answers and their notes reach nobody by mail; an answer's note is not
kept), the form's delete of an unused online meeting, `masterCategories`, an event by its join link, forwarding,
reminders, and the options' enforcement in a call (the calls' part).

## Prefix table

Every service lives under the public origin with a fixed prefix. `Served` means the server answers it now, and authz
hands out its GTM where it has one.

| Service | GTM | Path under the origin | Served |
|---|---|---|---|
| Token endpoint | (the authority) | `/oauth2/v2.0/token` | yes |
| authz | `authService` (base `/api/authsvc`) | `/api/authsvc/v1.0/authz` | yes |
| Admin API, first-admin setup | (the origin) | `/api/admin/v1/` | yes |
| The account's own endpoints | (the origin) | `/api/account/v1/` | yes |
| Chat service | `chatServiceAfd` | `/api/chatsvc/<region>/` | yes |
| Middle tier | `middleTier` | `/api/mt/part/<partition>/` (the partition as `<letters>-<digits>`: `own-01`) | the team and calendar routes |
| Chat service aggregator | `chatSvcAggAfd` | `/api/csa/<region>/` | yes |
| Provisioning | `teamsAndChannelsProvisioningService` | `/fabric/<region>/templates/api/` (the client takes a region of letters only) | yes |
| Presence | `presenceUPS` | `/ups/<geo>/` | no |
| Registrar | (the origin) | `/registrar/prod/V2/` | yes |
| Trouter | (the origin) | `wss://<origin>/trouter/v4/c` | yes |
| AMS and link previews | `amsV2` | `/ams/` | yes |
| Graph | (the origin) | `/graph/v1.0/` | `me`, `me/drive`, `users/<id>`, a shared file's item and thumbnails |
| User profiles | `userProfileService` | `/api/userprofilesvc/<region>/` | a team's people search |
| Files | (the origin) | `/sp/` | yes |
| Loki | (the origin) | `/loki/` | no |
| Search | (the origin) | `/search/` | no |
| Flight proxy | (the origin) | `/flightproxy/api/v2/` | 1:1 calls |
| TURN credentials | (the origin) | `/trap/tokens` | yes |

## Push: the registrar and Trouter

The wire is Teams' (`docs/protocol/trouter.md`): socket.io 0.9 over a WebSocket at `/trouter/v4/c`, with the
client's query (`tc`, `timeout`, `epid`). The handshake carries no credential; one with another `Origin` than the
public origin is refused.

- **Authentication.** The server sends `1::`. The first frame must be `user.authenticate` with the account's `ic3`
  token within `trouter.auth_timeout`, else the socket gets `0:::{"reason":"timeout"}`. A token that isn't valid, or
  a disabled account, gets `0:::{"reason":"unauthorized"}` and no `trouter.connected`. A token is run out at its
  `exp`, with no clock skew, at the connect as on an open socket: when it runs out the socket gets `unauthorized`,
  and the client comes back with a new token (it normally reconnects before that).
- **An account's end.** `user disable`, `user delete` and `user reset-password`, the admin API's same changes and
  an account's own password change close the account's open sockets at once with `unauthorized`, on every node:
  each publishes the disconnect on Redis, after its database change, and warns when it can't (the change stays, and
  the read below ends the sockets). Each socket also reads its account
  from the database once it listens, before each delivery and, on a node without Redis (which the command can't
  reach), every second, also while a replay to a slow reader is under way; it ends when the account is gone,
  disabled or has another password than its token was issued under. An access token names its account's password
  version (`pwd_ver`, a count of the resets), so a token from before a reset opens no socket and no picture, file
  or preview route either, though it is good elsewhere until it runs out. So nothing posted after the command returned is pushed to the account. The
  client then asks for a new token, which it doesn't get, and is signed out.
- **Endpoint.** `trouter.connected` names the endpoint, its push URL (`<origin>/trouter/v4/f/<endpoint>/`), the
  `reconnectUrl` on the same origin and `connectparams` valid for seven days. The endpoint id is derived from the
  account, the client's `epid` and its stack, so the same client gets the same endpoint on every connect and on
  every node, and `connectparams` carry no secret. Of an endpoint's sockets the one that claimed it last stays: a
  socket announces its claim with its time once it listens, an older socket closes, and a newer one that hears an
  older claim announces its own again. An account holds at most 32 sockets on a node.
- **Registrar.** `POST /registrar/prod/V2/registrations` with the `ic3` token takes either stack's registration
  (`TeamsCDLWebWorker`, `SkypeSpacesWeb`) and answers `202`. The push URL must be the caller's own endpoint for that
  `registrationId` and stack (else `403`). The TTL is at most 3600 seconds. A registration is a row in
  `push_registrations`; an account holds at most 64 live ones (`429` beyond). The account's sockets are told at once.
  A registration ends with its TTL. Nothing removes it sooner: Teams' removal at a sign-out is known only from the
  bundle (no capture shows its request) and the client sends none, and the socket's goodbye (`0::`) is the same on
  a stop that Teams leaves the registration through. A registration without a socket gets no delivery.
- **Deliveries.** A messaging endpoint with a live registration gets the account's `push:<mri>` events as `3:::`
  deliveries at `/trouter/v4/f/<endpoint>/messaging`, each body an `EventMessage` around the fan-out's resource:
  `NewMessage`, `MessageUpdate`, `ConversationUpdate`, `CustomUserProperties` and `ThreadUpdate` (a new chat, a
  team's or a channel's change; the client reads it as an unknown kind). A channel's messages come as a chat's do,
  to the people in the channel's thread: a team's people for a standard channel, its own for a private one, and
  nobody once they are out of it. A reply's `to` and links name its post's chain (`<channel>;messageid=<post>`); a
  channel's message names no `threadtype`. `ReadReceipt` stays internal: Teams delivers no peer read receipt. The
  calling stack's socket gets its endpoint's call deliveries ([Calls](#calls)) and no chat events. The client's
  answers (`3:::` with a status) are accepted; a calling client's answer to an acceptance carries its later
  callbacks, which the call controller keeps.
- **Keep-alives.** `ping` is answered `["pong"]`, any other acknowledged event `[]`. A socket silent for two ping
  intervals and 20 seconds is closed.
- **Replay.** Every delivered event but typing (and other `Control/…` messages) is also kept in the account's
  journal for 15 minutes, at most 512 entries: a Redis Stream `journal:push:<mri>` (in the deployment's
  [keyspace](#scaling)), or memory on a single node. Each
  registered endpoint keeps a cursor, the last entry written to its socket, for 24 hours: a field of the account's
  hash `cursors:push:<mri>`, which holds the 64 cursors that moved last. A socket delivers from its endpoint's
  cursor, so after a reconnect, on any node, the endpoint gets what it missed as stored deliveries (`"mstore":true`,
  the same `id` as the live one would have had), in order, each written to a socket once. The cursor is kept after
  each batch and when the socket ends; only a node that dies mid-batch replays that batch. A new endpoint starts at
  the journal's end.
- **Message loss.** The socket gets `trouter.message_loss` with the `messaging` tag, and the client runs its
  conversation sync and acknowledges, when entries after the cursor may be gone:
  - the journal overflowed past the cursor;
  - the endpoint was away, or unregistered while an event came, and its cursor is older than 15 minutes with
    nothing left to prove otherwise (a socket that followed its account is not told after a quiet spell);
  - a reconnecting endpoint has no cursor;
  - the journal couldn't keep an event (Redis refused it): the account's sockets are told on the control topic;
  - the socket missed control words.
- **A broken Redis subscription.** What is published while a node's Redis subscription is down reaches none of its
  sockets, and nothing says what it was. So the break closes every socket of the node with `0::`. Each client
  reconnects after its backoff, is replayed what the journal kept from its cursor, and syncs its conversations as
  on any connect, which covers an event that reached neither the journal nor the control topic.
- **A slow reader.** A replay ends, like the socket, when the token runs out, the node stops or the control topic
  says so, however slowly the client reads.
- **Shutdown.** A node closes its sockets with `0::` when it stops; clients reconnect to another node after their
  backoff. `ngteams_server::trouter::disconnect` closes an account's sockets on every node.

## Storage

Every stored blob (a pasted picture, a sent file, a file's thumbnails) goes through `object_store`. Only its own
S3 calls are used, so any S3-compatible service works by configuration alone. Several nodes share one bucket. The
database says who may read a blob; the bucket needs no public access and clients never reach it.

AWS S3:

```toml
storage_url = "s3://my-bucket/ngteams"

[storage_options]
aws_region = "eu-central-1"
aws_access_key_id = "…"
aws_secret_access_key = "…"
# aws_session_token = "…"        # temporary credentials
```

Any other S3-compatible service (RustFS, MinIO, Ceph, Garage, R2):

```toml
storage_url = "s3://my-bucket/ngteams"

[storage_options]
aws_endpoint = "https://s3.example.com"
aws_region = "us-east-1"                      # the name the service signs requests with
aws_access_key_id = "…"
aws_secret_access_key = "…"
aws_virtual_hosted_style_request = "false"    # path-style addressing; "true" for <bucket>.<host>
# aws_allow_http = "true"                     # only for a plain-HTTP endpoint on a trusted network
```

The option names are `object_store`'s. In the environment they are `NGTEAMS_SERVER_STORAGE_OPTIONS__AWS_REGION`
and so on. Without keys, `object_store` takes the instance's or the container's role credentials. The bucket must
exist. Uploads go as multipart uploads, so the store's rule for unfinished multipart uploads decides how long an
aborted one stays.

A store out of reach answers the client 503 within seconds: a node waits `storage.connect_timeout` for a
connection and sends a request again at most `storage.max_retries` times, none later than `storage.retry_timeout`
after the first attempt. `storage.request_timeout` is the wait for the store's answer to one request (one picture,
or one 5 MiB part of a file, sent whole first) and the longest a download may then stand still, the store sending
nothing or the client taking nothing. A file of any size is sent in parts and a download has no total time, so a
slow client doesn't count against it; raise it only when the link between the server and its store is slow.
`read_timeout` and `connect_timeout` in `storage_options` take the place of the two timeouts, and `timeout` there
limits the whole of each request, a download too: none is set by default. These keys are for S3 stores only.

Keys in the bucket: `ams/<object>/<version>` (pictures: each upload of an object's bytes gets a key of its own, and
the database names the one the object reads from), `drive/<item>` (files), `drive-thumbnails/<item>/…` (thumbnails).

## Pictures, files and link previews

### Pictures in messages (AMS, `/ams/`)

Requests carry the chat service's token (`https://ic3.teams.office.com`).

| Request | Does | Answers |
|---|---|---|
| `POST /ams/v1/objects/` | makes an object for a pasted picture: `{"type": "pish/image", "permissions": {"<chat>": ["read"]}, "filename"}`. The caller must be a member of every chat named | 201 `{"id"}`; 403 for a chat the caller isn't in |
| `PUT /ams/v1/objects/<id>/content/imgpsh` | stores the picture's bytes. Only the object's maker; only PNG, JPEG, GIF or WebP, by their signature | 201; 403; 413 over `media.max_picture_bytes`; 415; 507 over the quota |
| `GET /ams/v1/objects/<id>/views/<view>` | the picture (`imgo`, `imgpsh_fullsize`, `imgpsh_fullsize_anim`: all the bytes as sent) | 200; 403; 404 while the object is empty |
| `GET /ams/v1/objects/<id>` | the object's description, in Teams' eight keys | 200; 403 |
| `DELETE /ams/v1/objects/<id>` | deletes the picture: its bytes leave the store and its maker's quota. Only the object's maker. Not seen in a capture of Teams: the route is this server's own | 204; 403; 404 |

An object is read by its maker and by whoever is a member of one of its chats when the read arrives: someone who
left the chat no longer reads it. `forward` (sharing an object with another chat) and other object types are not
served.

### Files sent in chats (`/sp/`, `/graph/v1.0/`)

The client sends a file the way it sends one to OneDrive, and the server keeps one drive per account:

1. `GET /graph/v1.0/me/drive` names the drive at `<origin>/sp/personal/<account id without dashes>/`.
2. Under `/sp/personal/<account>/_api/v2.0/`, with a token whose audience is the server's own origin (the token
   endpoint mints it beside the configured audiences), and only for the drive's owner:
   - `GET drive`: the drive and its owner.
   - `GET drive/root:/Microsoft Teams Chat Files` (404 until the first upload) and its `:/permissions`, or
     `drive/root/permissions`: the owner's grant alone.
   - `PUT drive/root:/Microsoft Teams Chat Files/<name>:/content`: stores the body, streamed to the store, never
     held whole. 201 with the item (`id`, `name`, `size`, `webUrl`, `parentReference.driveId`,
     `sharepointIds.listItemUniqueId`). A name the drive already has gets a number (`notes 1.txt`). 413 over
     `media.max_file_bytes`, 507 over the quota. A new file is readable by its owner alone.
   - `POST sites/root/items/<item>/driveItem/invite`: shares the file with named accounts (`recipients` by
     `email`, which is the UPN). Only the direct form the client sends is taken: `requireSignIn: true` and
     `sendInvitation: false`. There are no links anyone could open. 400 for an address that is no enabled account.
   - `GET sites/root/items/<item>/driveItem/permissions`: who the file is shared with: the owner
     (`roles: ["owner"]`) and each other account (`["write"]`).
   - `DELETE sites/root/items/<item>/driveItem/permissions/<account id>`: ends one account's access.
   - `GET drive/root:/Microsoft Teams Chat Files:/children`: the drive's files (`value`: the items).
   - `DELETE drive/items/<item>` (the item's `id` or its `sharepointIds.listItemUniqueId`): deletes the file, as
     Graph and SharePoint delete a drive item. 204; 403 on another account's drive; 404 for a file the drive
     doesn't have. Its bytes and thumbnails leave the store and the owner's quota, and its shares and links end.
3. A reader reaches the file through Graph, by the sharing link the message carries (the file's `webUrl`):
   `GET /graph/v1.0/shares/u!<link, base64url>/driveItem` (the item with `@microsoft.graph.downloadUrl`) and
   `…/driveItem/thumbnails` (Graph's `small`, `medium` and `large`, 96, 176 and 800 pixels). 403 for an account
   the file isn't shared with.

Download and thumbnail links are `<origin>/sp/download/<item>?tempauth=…` and
`<origin>/sp/thumbnail/<item>/<size>?tempauth=…`. The `tempauth` is a JWT the server signs for that one item and
purpose, valid for `media.link_lifetime` seconds. Any node checks it with the shared keys. It names the account
it was handed to and that account's password version, and each use reads the account: a link of an account that
is disabled, deleted or has had its password reset since answers 401, and a link to a file no longer shared with
that account answers 403. The link takes no
token of the account, and the server streams the bytes from the store, so the store's credentials and the
account's tokens never meet. S3's own presigned URLs aren't handed out: the client takes a file link only on the
server's origin and follows no redirect.

A download serves `Range` (one range: 206 with `Content-Range`; an unsatisfiable one: 416; several ranges: the
whole file). Files always download as attachments (`Content-Disposition`, `nosniff`), so nothing uploaded is
rendered on the server's origin.

Thumbnails are made the first time they are asked for, from PNG, JPEG, GIF and WebP files up to 32 MiB, as JPEGs,
and kept in the store. Other files have none (`value: []`), and the client draws its placeholder.

The file's `webUrl` itself answers nothing: there is no web page to open a file in.

Sharing needs two people reads the people service will own: `POST <middle tier>/beta/users/fetchShortProfile`
(MRIs to names and addresses) and `GET /graph/v1.0/users/<id>`. Both are served in the smallest form file sharing
needs. Authz hands out `middleTier` and `amsV2`, so a client signed in through authz pastes pictures and sends
files.

### Freeing storage

An account at `media.user_quota_bytes` gets 507 for each new picture or file until some of what it stored is
deleted:

- **A picture** is deleted with the last message that shows it: when its sender deletes a message, each picture
  of the sender's that the message showed (`amsreferences`), and that no other message in its chats still names,
  is deleted. `DELETE /ams/v1/objects/<id>` deletes one whatever shows it. A picture taken out of a message by
  an edit, or pasted and never sent, stays until its object is deleted.
- **A file** stays in its sender's drive when its message is deleted, as a file stays in the sender's OneDrive in
  Teams. Its owner deletes it with `DELETE drive/items/<item>`. ngteams has no command for that yet, so on a
  server of ngteams clients the operator frees files with `ngteams-server media list|delete`.

Each delete takes the bytes off the account's count in the transaction that removes the row, so uploads and
deletes at once leave the count right, on several nodes too. The bytes are cleared from the store after that; if
the store fails then, the node logs their key and the bytes stay in the bucket, read by nothing. A node that
stops waits up to 10 seconds for the clears still running, and logs the keys it leaves.

### Link previews (`/ams/urlp/v1/url/`)

- `GET info?url=<page>` answers `{"url", "status", "title", "favicon", "preview": {"title", "description",
  "previewurl"}}`, from the page's Open Graph tags, else its `<title>` and description. `preview` is the object a
  message's `properties.links` entry carries. Teams' own answer was never captured, so this shape is ours.
- `GET image/Thumbnail?url=<picture>` answers the picture as a JPEG at most 640 pixels on its longer side. This is
  the read the client makes for a message's `previewurl`, so a reader's client never reaches the page's site.

The account chooses the address, so the server guards every fetch against request forgery:

- only `http` and `https` on their default ports, no user info;
- every address of the host must be public: loopback, private, link-local (the cloud metadata address too),
  shared, reserved, multicast and documentation ranges are refused, in IPv4, IPv6 and the IPv6 forms that carry an
  IPv4 address. A refused address gets 403 before anything is sent;
- the connection is made through a resolver that hands out public addresses only, so a name can't change its
  answer between the check and the connection;
- at most three redirects, each checked the same way; no proxy;
- `media.preview_timeout` for the whole exchange, size caps for the page and the picture, and at most 16 fetches
  per node at once (503 with `Retry-After` beyond).

`media.link_previews = false` removes both routes.

## Calls

The server does a call's signaling and hands out the TURN relay's credentials. A 1:1 call's media goes between the
two clients through the relay (their ICE is relay-only): the caller's offer travels in the callee's invitation, the
callee's answer in the caller's acceptance. `ngteams` doesn't take the relay from the server yet
([the gap](#the-turn-relay-coturn)), so its calls on an own server ring and connect without media. The wire is Teams' (`docs/protocol/calling.md`), every link under
`<origin>/flightproxy/api/v2/ep/`; requests carry the account's skypetoken.

- **The start** (`POST /flightproxy/api/v2/epconv`) from one account to one other. The answer is the conversation
  with its links (`leave`, `updateEndpointState`, `updateEndpointMetadata`, `updateParticipantProperties`,
  `notificationLinks`). The callee's registered calling endpoints (the `SkypeSpaces` stack's Trouter sockets, on
  any node) get the invitation at their root: `{"evt": 107, "gp": <base64 JSON>}` with the caller, the leg's
  `attach`, `progress` and `reject` links and the caller's offer. A group call's, a meeting's and the test call's
  start is answered `501`: they need the call's mixer.
- **While it rings**: `attach` (answered with the leg's `progress`, `acceptance` and `callLeg` links and the
  conversation's), `progress`, `accept` (answered with the call leg's links and the keep-alive interval), `reject`.
  An answer delivers the acceptance, with the callee's answer, at the caller's `acceptance` callback; the callee's
  other endpoints get the call's end (`487`, "accepted by another fork"). A decline ends the call for the caller
  (`603`, the callee as its sender): there is no voicemail. A ring nobody answers in `calling.ring_timeout` ends at
  both ends (`487` "Call cancelled on timeout." at the callee, `480` at the caller); the caller's `leave` while it
  rings cancels it.
- **While it is up**: the call's keep-alive at the call leg's own link (`202`); `leave`, which ends the call for
  the other party; `updateEndpointState`, whose mute both rosters then list; a renegotiation (`renegotiate`),
  delivered at the far end's `mediaRenegotiation` callback with where its answer (`negotiations/…/answer`) or its
  rejection goes, the answer delivered back with where it is acknowledged (`mediaAnswers/…/acknowledge`), and the
  acknowledgement told to the answerer; a renegotiation while another waits for its answer is turned away with
  `491` at its `rejection` callback, and one unanswered for 30 seconds gives way to the next: what then comes for
  it at its own links is answered `410`. `updateMediaDescriptions` and `applyChannelParameters` are taken and change
  nothing. The caller's `mediaRenegotiation` callback is read from its answer to the acceptance's delivery (its
  `callAcceptanceAcknowledgement`), as Teams' call controller reads it.
- **Hold and resume**: `hold` is answered `202` with the `resume` link; both rosters gain Teams' music-on-hold
  app for as long as the hold lasts, which is how the held client knows, and the holder is told at its
  `holdCompletion` and `resumeCompletion` callbacks. No music plays and the media isn't moved: that takes the
  mixer.
- **Deliveries** go to the callbacks each party handed out (`…/trouter/v4/f/<endpoint>/callAgent/…`), gzip'd in
  base64 as Teams' call controller sends them. A request's callbacks are taken only when all are on one calling
  endpoint the account itself registered (`403` otherwise), so no request makes the server deliver to someone
  else's socket.
- **Who may do what**: the token's account must be one of the call's two parties; anyone else gets `404` for every
  link of the call. Only the callee attaches, answers and declines. A call controller operation on a call that is over gets
  `410`; the conversation's own (`leave`, the endpoint's state and metadata) are answered `200` and change nothing,
  as a write that was on its way when the far end hung up must not read as a failure.
- **The chat** of the two parties, when they have one, gets the call's `Event/Call` messages from the caller:
  its start, and its end listing who was in it and for how long. An end with the caller alone is a missed call
  (unanswered, declined or cancelled).

Not served yet: group calls and meetings, the test call, transfer, voicemail, adding someone to a call, the call
log stream (`48:calllogs`) and a missed call's activity item.

### The TURN relay (coturn)

`GET /trap/tokens` (the account's `ic3` token) answers Teams' shape, `{"tokens": [{"realm", "username",
"password"}], "expires"}`, and beside it `urls`, the relay's URLs from `calling.turn.urls`. With
`calling.turn.secret` the credential is the TURN REST API's: the user name is `<expiry in Unix seconds>:<account
id>` and the password the base64 HMAC-SHA1 of the user name under the secret, so coturn checks it without asking
the server and it runs out after `calling.turn.ttl`. Without a secret, `calling.turn.username` and
`calling.turn.password` name one long-term user. With neither the answer is `503` and calls have no media.

**Not in the client yet.** `ngteams` reads the credential and not `urls`: every account's relay is still Teams'
own host, which refuses an own server's credential. So a call between two `ngteams` clients on an own server has
no audio or video, whatever relay is configured, until the client takes an own-server account's relay from `urls`
(`docs/backlog/own-server-calls-use-the-servers-relay.md`). The server's tests name their relay to the client
themselves.

A coturn for it:

```
listening-port=3478
tls-listening-port=5349
realm=rtcmedia
use-auth-secret
static-auth-secret=<the same secret as calling.turn.secret>
cert=/etc/coturn/fullchain.pem
pkey=/etc/coturn/privkey.pem
no-multicast-peers
denied-peer-ip=10.0.0.0-10.255.255.255
denied-peer-ip=172.16.0.0-172.31.255.255
denied-peer-ip=192.168.0.0-192.168.255.255
```

Clients relay everything, so the relay needs its UDP range (`min-port`, `max-port`) open and enough bandwidth for
every call's media twice. Several coturn instances with one secret serve any number of nodes.

## Chat events

Chat creation publishes to every member, including the acting account's other endpoints, on `push:<mri>`.
The internal JSON envelope is
`{"resourceType":"ThreadUpdate","resource":<thread>}`; the resource has the thread-read shape. Trouter delivery
wraps this internal event separately. A message post publishes `NewMessage` with the stored message as `resource`;
an edit, soft deletion or reaction publishes `MessageUpdate` to the same recipients. Typing and peer read receipts
exclude the acting account.
An activity item is published to its account alone, as a `NewMessage` (or `MessageUpdate`) of its stream.
Edits require message ownership and preserve its ID, client message ID, original arrival time and reactions. Publication happens after the transaction commits. The fan-out is at most
once; a socket reads kept events from the journal instead (above), and clients catch up through conversation sync
after a reported loss.

Database versions advance a shared row inside the write transaction. A conversation page keeps its upper bound
and follows versions as a keyset; a chat changed during paging appears on the next incremental scan. This keeps
clocks on different HTTP nodes out of cursor ordering. Conversation arrays, including their last-message
previews, stay below a 3 MiB serialized budget; a cut page's forward link starts after the last included chat,
and every page's sync link keeps the snapshot's upper bound. If a single preview exceeds the array budget,
its reaction summary is omitted from the listing; its text, identity and stored message remain unchanged.

Posts use the authenticated account as sender. A repeated `(chat, sender, clientmessageid)` returns the original
message and emits no duplicate event. Message IDs and versions come from the shared version clock, and a
message page keeps its watermark in backward links. Incremental pages filter by changed version and order by
arrival ID. While an incremental answer is cut, its segment start stays above the prior watermark even when
the last row is a changed older message; only the final page reports that prior watermark. This keeps the
native engine following every backward continuation. Message arrays stay below a 3 MiB serialized budget.

Read horizons are stored per `(chat, account)` and returned only in that account's conversation listing.
Their own endpoints receive a `ConversationUpdate` with `propertiesUpdated` and the changed property. Other
members get an internal `ReadReceipt` resource (`id`, `mri`, `consumptionhorizon`); that is separate from the own
horizon update, which must never mark another member's conversation read.

Own user properties persist `userGraphSettings` as its native JSON text and `appConsumptionHorizon` as JSON text
or an object. Mark-all-read may be reset, as in Teams. Writes retain the account's other properties and publish
`CustomUserProperties` only to that account. An account with no own properties receives the native 404/subcode-1
shape that the client treats as empty preferences.

## Rich messages

A post or an edit is stored as Teams gives it back (`docs/protocol/chat.md`, "Mentions" and "A message as read"), so
every reader, the push channel and the sync see the same message (`src/chatsvc/rich.rs`):

- **Mentions.** The compose box's wrapper (`<readonly itemtype="…/Mention">`) is taken off and its span kept; such
  a message's HTML is then written out again by html5ever, as Teams writes it (`itemscope=""`). HTML without that
  wrapper, and a `Text` message's content, are kept byte for byte. `properties.mentions` stays a JSON string, each
  entry's `itemid` a number. A mention of everyone is an entry like any other, its `mri` the chat's id.
- **Quotes.** `properties.qtdMsgs` (a JSON string as sent) is read back as a list of objects: `messageId` a number,
  `sender` and `time` as sent, `message`, `sharedRefId` and `replyChainId` null, and `validationResult` `Valid` for
  a message of the same chat that the writer may read and that isn't deleted (`Invalid`, the server's own word, for
  any other, a deleted one as a missing one: nothing is told about a message elsewhere). `hasValidMsgReferences` is true only when there is at least one quote and every quote is valid (for an empty
  list it is false, the server's own choice: the native client sends `qtdMsgs` only when it quotes). The quote in the HTML is
  kept as sent; Teams' line feed after it is not added (it changes nothing a reader parses).
- **Edits** keep the message's id, client message id and arrival; `properties.edittime` is the edit's time and
  `properties.composetime` the compose time the editing client sends, as Teams keeps it (`edit.json`).
- **Refused** (400): `mentions` or `qtdMsgs` that aren't a list of objects, or a JSON string of one.
- **Not kept**: `importance`, `subject` and `title` (the native client sends them empty), a forward's
  `forwardTemplateId` and `originalMessageContext` (the native client doesn't forward), and the push delivery's
  `mentiontypes` beside the resource (no client reads it). Who a mention reaches is [below](#who-gets-an-item).

## The account's own streams

`48:notes`, `48:notifications` and `48:mentions` are each kept as a thread only their account is in, under an id no
request can name. A request's `48:…` id always resolves to the caller's own stream, so the membership check that
guards every chat also keeps one account's stream from another. A stream is no thread: its thread and member reads
answer 404, as does any other `48:` id (`48:generic`: 400).

- **Notes** (`48:notes`): `POST …/48:notes/messages` writes a note, and its edit, delete and message pages work as
  a chat's. They answer 404 until the first note is written, and are in no conversation list, as with Teams. A
  note is pushed to the account's own endpoints.
- **The activity feed** (`48:notifications`) and **the mentions stream** (`48:mentions`) are written by the server
  only (a post there is refused, 403). They are in the conversation list once they hold an item, with their
  `consumptionhorizon` and `consumptionHorizonBookmark`. The account marks an item read or unread (`PUT
  …/messages/<item>/properties?name=isread`), removes it (`DELETE …/messages/<item>?behavior=softDelete`), moves
  the feed's own horizon and marks all read (the two properties). Each change moves the item's or the stream's
  version, so the account's other clients sync it.

### Who gets an item

An item is written in the transaction that writes its message, from the thread's people as they are then. It
carries a preview of the message, so only someone who is in the thread and may read the message gets one, whatever
the message's mentions name. A disabled account gets none. The sender gets none for their own message.

| What happened | Who gets an item | `activityType` / `activitySubtype` |
|---|---|---|
| a mention of a person | that person; also in their `48:mentions` (`mentions` / `mention` in a 1:1 chat, `person` in a group chat or channel) | 1:1 chat `mentions` / `mention`; group chat `mentionInChat` / `person`; channel `mention` / `person` |
| a mention of everyone (`everyone`, `team`, `channel`) naming the thread itself (a channel: or its team) | everyone in the thread; none in a 1:1 chat | `mentionInChat` or `mention` / `everyone`, `team`, `channel` |
| a reply in a channel post's thread | the people who wrote in that thread; people with "Follow all new threads" on (`followAllRc`) | `reply` / `reply` |
| a post or reply in a channel | people whose `channelNotificationSettings` have `allNewPosts` `On` (replies: with `includeReplies`) | `follow` / `channelNewMessage` (the server's own names: no capture shows Teams') |
| a reaction | the message's sender; one item per reactor | `reactionInChat` or `reaction` / the reaction's key |

The account's own settings of the thread apply: a muted thread (`alerts` `false`) tells of nothing but a mention of
the account itself. A person named and also taken in by a mention of everyone gets one item, their own.

A change of the message changes the items it made, for the people in the thread at that moment:

- **An edit** rewrites each item where it is, with the new wording and as read as it was; an item its account
  removed stays removed. Someone it newly mentions gets an item, and a mention's item of someone it no longer
  mentions is removed, in the feed and in `48:mentions`.
- **A reaction** given again, or a second one of the same person, is new activity: the reactor's earlier item is
  removed and a new one written, so it has the time of the new reaction. One taken back leaves the item on the
  newest reaction of theirs that stays, at the time it had; the last one taken back removes the item.
- **A delete** removes the message's items, so its text stays in no preview of theirs.

Someone who left the thread keeps their items as they were then: a later edit, delete or reaction changes none of
them and pushes them nothing, as it stays out of what they may read (below).

## A group chat's people

`PUT v1/threads/<chat>/members` (`{"members": [{"id", "role": "Admin", "shareHistoryTime"}]}`) adds people to a
group chat and `DELETE v1/threads/<chat>/members/<mri>` takes one out (the caller's own MRI: they leave). Anyone in
the chat may do both, as in Teams, where a group chat's every member is an `Admin`. A 1:1 chat keeps its two
people (400), a team's channel takes and loses people through its team (400), and only accounts that can sign in
are added (400). Someone already in the chat is no change (200).

- **History.** Each member has the first message they may read: all of it (`shareHistoryTime` `0`), from a time on
  (a positive time), or from their joining (`-1`). Message pages, the page around a time, the conversation list's
  preview, edits, reactions and activity items all keep to it. A message from before it answers 404.
- **Leaving.** Someone removed or gone keeps reading what they could until then, the event of their going
  included, and nothing written or changed after it; their thread and member reads and every write answer 403. The
  chat stays in their conversation list at the version it had when they left and never moves again. Added back,
  they read from where the new add says.
- **Events.** Each change writes the thread's own message (`ThreadActivity/AddMember` with each target's
  `sharehistorytime`, `ThreadActivity/DeleteMember`), moves the thread's `rosterVersion`, and publishes the message
  (`NewMessage`) and the thread (`ThreadUpdate`) to the people in the chat after the change. Someone taken out is
  told the event of their going and nothing after it. A thread's own message can't be edited, deleted or reacted to.
- **Concurrent changes.** A post, edit or reaction checks its sender's membership and reads the thread's people in
  its own transaction, under the shared version clock. Its event and its activity items go to those people and
  nobody else, so a member removed while the request ran is told nothing of a message written after their removal.
  A read receipt and typing go to the people their own write or moment of the clock finds, the same way. A message
  read (a page, the page around a time, the conversation list's preview) takes the reader's seat in the statement
  that reads the messages, so a removal never falls between a check and the read.

A group started from a 1:1 chat is a new chat (`POST v1/threads` with the three people, as the client sends it);
the 1:1 chat and its history stay its two people's.

## Scaling

- HTTP nodes keep no state of their own: Postgres is the source of truth, so any node answers any request. SQLite
  means a single node; it needs no Redis.
- With Redis, a write's events go to `Fanout::publish` on a topic such as `push:<mri>`, and every node that holds a
  subscriber for it delivers them. Each node keeps one Redis subscription per topic and feeds its local subscribers
  from it. Without Redis the same `Fanout` trait is one process's broadcast.
- Delivery through the fan-out is at most once. A subscriber that falls behind learns how many payloads it missed.
  When a node's Redis subscription breaks, its subscriptions end: a subscriber subscribes again.
- Several deployments may share one Redis. Every Redis key and channel of a deployment is under its namespace,
  `ngts:<namespace>:<name>`: the sign-in and setup counters (`login:<hash>`, `setup:<hash>`), the fan-out's topics
  (`push:<mri>`, `trouter:<mri>`) and the journal (`journal:<topic>`, `cursors:<topic>`). The namespace is
  `redis_namespace`, or else the tenant id the nodes read from the database: the nodes of one deployment agree with
  no configuration, and another deployment's sign-in holds, pushes and replays never meet them. The command line's
  account commands reach the nodes in the same namespace. The names are built in one place,
  `fanout::keyspace` (`Keyspace::key`, and `Shared`, the only way to a node's Redis connection): new code that keeps
  anything in Redis names it there, never with a bare key.
- A Trouter socket lives on one node. Its node subscribes to the account's `push:<mri>` and `trouter:<mri>` (control:
  a registration taken, a socket's claim to its endpoint, an event the journal didn't keep, a disconnect) topics. A
  kept event is appended to the account's journal before it is published, so the node that is woken reads it there,
  in order; a socket that lagged or reconnected reads from its endpoint's cursor. Registrations are rows, so any node takes one and any node reads it.
- Pictures and files live in the shared store and their rows in the database, so one node takes an upload and
  another serves it. A download link one node signed opens on every node. Each account's stored bytes are counted
  in one row updated in a single statement, so two nodes can't both pass the quota.
- Signing keys are rows in the database. A node rotates the key when it is due (checked hourly) and drops keys that
  can no longer verify a live token or file link. Retired keys are kept for the access lifetime, or
  `media.link_lifetime` when that is longer, plus three hourly reload intervals and the verifier's 60-second clock
  allowance, covering two missed maintenance checks. Nodes that cannot reload for longer than three hours should be
  taken out of service until their database connection recovers.
  `ngteams-server keys rotate` creates a key immediately; running nodes switch at their next hourly check, or
  earlier if verifying an unknown key triggers a reload. Rotation is serialized in the database across nodes.
- Account administration keeps its state in the database: the setup code's digest, the admin role, the audit. A
  setup code issued by one node is taken by any node. Setup attempts are counted in Redis when it is configured,
  else per node.
- A call is a row (`calls`): its parties, their callbacks and where it stands, as JSON beside its version. A
  request on any node reads the row, makes its step and stores it only if the version is still the one it read;
  otherwise it reads again. What the step tells a party is published on `call:<endpoint>`, and the node that holds
  that calling endpoint's socket delivers it. Every node looks twice a second for calls that are due (a ring's end,
  a call whose keep-alives stopped, an ended call's row five minutes later); the version check lets one of them act.
- Migrations take a Postgres advisory lock, so nodes starting together take turns.

## The client

`ngteams` signs in to this server from its account window ("Own server": address, user name, password), behind
`NGTEAMS_OWN_SERVER=1`: [personal-build.md](personal-build.md#own-server-accounts). A deployment with a certificate
the system doesn't trust gives its certificate authority to each account as `ca_file`.

## Tests

`cargo test -p ngteams-server` runs the e2e tests: the real client crates (`teams-auth`'s session and clients,
`teams-core`'s account bring-up and engine, `teams-call`'s call machine, `teams-media` on its file device backend
through its loopback TURN server) against an in-process server on loopback, on SQLite. With `NGTEAMS_SERVER_PG_URL`
(a Postgres the tests may create databases on) the Postgres cases run too, and with `NGTEAMS_SERVER_REDIS_URL` the
Redis cases. The picture and file tests keep their blobs in the S3 store `NGTEAMS_SERVER_S3_URL` names
(`http://<access key>:<secret>@<host>:<port>/<bucket>`; they make the bucket), else in a temporary directory. CI's
Coverage job sets all three from its service containers (RustFS for S3). Each test's deployment has a database and a
Redis namespace of its own, so the tests run in parallel on one Postgres and one Redis. Locally:

```sh
podman run -d --rm --name ngts-pg -e POSTGRES_PASSWORD=ngts -p 55432:5432 docker.io/library/postgres:17
podman run -d --rm --name ngts-redis -p 56379:6379 docker.io/library/redis:7
podman run -d --rm --name ngts-s3 -p 59000:9000 -e RUSTFS_ACCESS_KEY=ngtsaccess -e RUSTFS_SECRET_KEY=ngtssecret \
  docker.io/rustfs/rustfs:1.0.5
NGTEAMS_SERVER_PG_URL=postgres://postgres:ngts@127.0.0.1:55432/postgres \
NGTEAMS_SERVER_REDIS_URL=redis://127.0.0.1:56379 \
NGTEAMS_SERVER_S3_URL=http://ngtsaccess:ngtssecret@127.0.0.1:59000/ngts-test cargo test -p ngteams-server
podman stop ngts-pg ngts-redis ngts-s3
```

With `NGTS_TEST_COTURN` (a coturn's `host:port`) and `NGTS_TEST_COTURN_SECRET` one more case relays a
call through that coturn with the credentials made from its secret (coturn relays between loopback peers only
with `allow-loopback-peers`, which it takes only beside a password for its own CLI: `COTURN_CLI` is that option,
any value):

```sh
podman run -d --rm --name ngts-coturn --network host docker.io/coturn/coturn -n --listening-ip=127.0.0.1 \
  --listening-port=53478 --relay-ip=127.0.0.1 --use-auth-secret --static-auth-secret=test-secret \
  --realm=rtcmedia --allow-loopback-peers "$COTURN_CLI"
NGTS_TEST_COTURN=127.0.0.1:53478 NGTS_TEST_COTURN_SECRET=test-secret \
  cargo test -p ngteams-server relayed_by_coturn
podman stop ngts-coturn
```
