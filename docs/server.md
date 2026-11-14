# The own server (`ngteams-server`)

`crates/ngteams-server` is a Teams-compatible server ngteams can sign in to instead of Microsoft's. It speaks the
wire the client already speaks to Microsoft (the shapes in `docs/protocol/`), with every service under one HTTPS
origin. The client sends a request to it only for an account signed in to that server.

What it serves today: sign-in (the token endpoint), authz, the chat service, teams and channels (CSA, the middle
tier's and the provisioning service's team routes), account administration (the admin API and the first-admin
setup), the registrar and the Trouter push socket. The registrar and Trouter have no GTM (the client finds them on
the origin). Other services are added one at a time. Authz names only the services the server serves, so the client
keeps every other service off.

The chat service serves chat creation, conversation sync, thread/member reads, message posting, edits, soft deletion,
reactions, transient typing, account preferences and message pages (newest, backward, incremental and around a
requested time) under `/api/chatsvc/<region>/v1/`. These routes require an IC3 bearer token and current membership;
disabled users are refused even while their token is live. A repeated 1:1 roster returns the existing chat, and
group rosters and names persist in SQLite or PostgreSQL. Requests are limited to 64 KiB. Authz advertises
`chatServiceAfd`. Account streams (`48:notifications`, `48:mentions`, `48:notes`), rich quote normalization and
membership changes are still being added.

## Stack

| Need | Library |
|---|---|
| HTTP | axum 0.8 on hyper 1 and tower; tower-http for request timeouts, body limits and traces |
| Push socket | tokio-tungstenite on hyper's upgrade (the tungstenite the client links; axum's `ws` would add a second one) |
| TLS | rustls (aws-lc-rs) through axum-server, or plain HTTP behind a reverse proxy |
| Database | Diesel 2.4: Postgres through diesel-async (tokio-postgres, no libpq), SQLite through Diesel's own connection on the blocking pool; bb8 pools; embedded migrations per backend |
| Passwords | argon2 (Argon2id, PHC strings) |
| Tokens | jsonwebtoken (EdDSA, Ed25519 keys from aws-lc-rs, `kid` per key); opaque refresh tokens kept as SHA-256 |
| Fan-out, replay, counters | redis-rs 1.8: a connection manager with RESP3 pushes and resubscription after a reconnect; Redis Streams for the replay journal (Redis 6.2 or later, or Valkey) |
| Files | object_store: a local directory, memory or S3 |
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

Commands: `serve`, `migrate`, `user add|list|disable|enable|reset-password|promote|demote|delete`, `setup-code`,
`keys rotate`. A password is read from the first line of standard input, never from the command line. Disabling or
deleting an account, or resetting its password, revokes its refresh tokens: its sessions end when their access
tokens run out (one hour by default). `user add --admin` and `user promote` make an admin; `user list` ends each
line with `admin` or `user`. The command line keeps the same guards as the admin API (the last enabled admin stays)
and its actions are audited as `cli`.
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
| `storage_url`, `storage_options` | `memory:///`, none | `file:///<dir>`, `s3://<bucket>[/<prefix>]`, with `object_store`'s option names (`aws_region`, `aws_endpoint`, …) |
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
| Middle tier | `middleTier` | `/api/mt/part/<partition>/` (the partition as `<letters>-<digits>`: `own-01`) | the team routes |
| Chat service aggregator | `chatSvcAggAfd` | `/api/csa/<region>/` | yes |
| Provisioning | `teamsAndChannelsProvisioningService` | `/fabric/<region>/templates/api/` (the client takes a region of letters only) | yes |
| Presence | `presenceUPS` | `/ups/<geo>/` | no |
| Registrar | (the origin) | `/registrar/prod/V2/` | yes |
| Trouter | (the origin) | `wss://<origin>/trouter/v4/c` | yes |
| AMS | `amsV2` | `/ams/` | no |
| Graph | (the origin) | `/graph/v1.0/` | `me` |
| User profiles | `userProfileService` | `/api/userprofilesvc/<region>/` | a team's people search |
| Files | (the origin) | `/sp/` | no |
| Loki | (the origin) | `/loki/` | no |
| Search | (the origin) | `/search/` | no |
| Flight proxy | (the origin) | `/flightproxy/api/v2/` | no |
| TURN credentials | (the origin) | `/trap/tokens` | no |

## Push: the registrar and Trouter

The wire is Teams' (`docs/protocol/trouter.md`): socket.io 0.9 over a WebSocket at `/trouter/v4/c`, with the
client's query (`tc`, `timeout`, `epid`). The handshake carries no credential; one with another `Origin` than the
public origin is refused.

- **Authentication.** The server sends `1::`. The first frame must be `user.authenticate` with the account's `ic3`
  token within `trouter.auth_timeout`, else the socket gets `0:::{"reason":"timeout"}`. A token that isn't valid, or
  a disabled account, gets `0:::{"reason":"unauthorized"}`. When the token runs out the socket gets `unauthorized`
  too, and the client comes back with a new token (it normally reconnects before that).
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
- **Deliveries.** A messaging endpoint with a live registration gets the account's `push:<mri>` events as `3:::`
  deliveries at `/trouter/v4/f/<endpoint>/messaging`, each body an `EventMessage` around the fan-out's resource:
  `NewMessage`, `MessageUpdate`, `ConversationUpdate`, `CustomUserProperties` and `ThreadUpdate` (a new chat; the
  client reads it as an unknown kind). `ReadReceipt` stays internal: Teams delivers no peer read receipt. The
  calling stack's socket gets no chat events. The client's answers (`3:::` with a status) are accepted and not
  needed.
- **Keep-alives.** `ping` is answered `["pong"]`, any other acknowledged event `[]`. A socket silent for two ping
  intervals and 20 seconds is closed.
- **Replay.** Every delivered event but typing (and other `Control/…` messages) is also kept in the account's
  journal for 15 minutes, at most 512 entries: a Redis Stream `journal:push:<mri>`, or memory on a single node. Each
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
  - the socket missed control words, or the node's Redis subscription was cut off since the socket last read the
    journal.
- **A slow reader.** A replay ends, like the socket, when the token runs out, the node stops or the control topic
  says so, however slowly the client reads.
- **Shutdown.** A node closes its sockets with `0::` when it stops; clients reconnect to another node after their
  backoff. `ngteams_server::trouter::disconnect` closes an account's sockets on every node.

## Chat events

Chat creation publishes to every member, including the acting account's other endpoints, on `push:<mri>`.
The internal JSON envelope is
`{"resourceType":"ThreadUpdate","resource":<thread>}`; the resource has the thread-read shape. Trouter delivery
wraps this internal event separately. A message post publishes `NewMessage` with the stored message as `resource`;
an edit, soft deletion or reaction publishes `MessageUpdate` to the same recipients. Typing and peer read receipts
exclude the acting account.
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

## Scaling

- HTTP nodes keep no state of their own: Postgres is the source of truth, so any node answers any request. SQLite
  means a single node; it needs no Redis.
- With Redis, a write's events go to `Fanout::publish` on a topic such as `push:<mri>`, and every node that holds a
  subscriber for it delivers them. Each node keeps one Redis subscription per topic and feeds its local subscribers
  from it. Without Redis the same `Fanout` trait is one process's broadcast.
- Delivery through the fan-out is at most once. A subscriber that falls behind learns how many payloads it missed.
- A Trouter socket lives on one node. Its node subscribes to the account's `push:<mri>` and `trouter:<mri>` (control:
  a registration taken, a socket's claim to its endpoint, an event the journal didn't keep, a disconnect) topics. A
  kept event is appended to the account's journal before it is published, so the node that is woken reads it there,
  in order; a socket that lagged or reconnected reads from its endpoint's cursor. Registrations are rows, so any node takes one and any node reads it.
- Signing keys are rows in the database. A node rotates the key when it is due (checked hourly) and drops keys that
  can no longer verify a live token. Retired keys are kept for the access lifetime plus three hourly reload intervals
  and the verifier's 60-second clock allowance, covering two missed maintenance checks. Nodes that cannot reload
  for longer than three hours should be taken out of service until their database connection recovers.
  `ngteams-server keys rotate` creates a key immediately; running nodes switch at their next hourly check, or
  earlier if verifying an unknown key triggers a reload. Rotation is serialized in the database across nodes.
- Account administration keeps its state in the database: the setup code's digest, the admin role, the audit. A
  setup code issued by one node is taken by any node. Setup attempts are counted in Redis when it is configured,
  else per node.
- Migrations take a Postgres advisory lock, so nodes starting together take turns.

## Tests

`cargo test -p ngteams-server` runs the e2e tests: the real client crates (`teams-auth`'s session and clients,
`teams-core`'s account bring-up) against an in-process server on loopback, on SQLite. With `NGTEAMS_SERVER_PG_URL`
(a Postgres the tests may create databases on) the Postgres cases run too, and with `NGTEAMS_SERVER_REDIS_URL` the
Redis cases. CI's Coverage job sets both from its service containers. Locally:

```sh
podman run -d --rm --name ngts-pg -e POSTGRES_PASSWORD=ngts -p 55432:5432 docker.io/library/postgres:17
podman run -d --rm --name ngts-redis -p 56379:6379 docker.io/library/redis:7
NGTEAMS_SERVER_PG_URL=postgres://postgres:ngts@127.0.0.1:55432/postgres \
NGTEAMS_SERVER_REDIS_URL=redis://127.0.0.1:56379 cargo test -p ngteams-server
podman stop ngts-pg ngts-redis
```
