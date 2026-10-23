# The lab fence guards contacting writes only

## Overview

- The user's rule of 2026-10-23 replaces the M0–M2 default-deny fence: **lab accounts may contact only each other;
  everything else is allowed.** In the user's words: "lab accounts can't send messages or make calls to other accounts
  or common chats/channels. They can do that only with each other", "lab accounts may leave reaction only to each
  other", and typing, read receipts and presence are fine "if other will see it". The lab browser is default-allow
  too.
- The problem it solves: every new feature that touched a new endpoint first paid for a capture, an exact-shape
  allowlist entry, a review, tests, an `endpoints.md` row and an expected-deny review. The team spent more time getting
  around its own fence than building Teams features.
- What stays: the fence still sees every request (the browser stays armed, the native transport checks every request),
  still judges every contacting write by its target's fresh, complete member list, and still never answers a call
  from outside the lab. What goes: the own-state lists, the read-by-`POST` lists, the own-availability exception, most of the expected-deny
  list, and the single-enrolled group chat.
- **Calls (M3) aren't blocked by this plan.** A call between lab accounts is a contacting write like a message: it may
  be started, answered and carried on when its participants are lab accounts (or the echo service). The fence trusts
  the calls it permitted itself (a registry of the calls this run started or answered), since no endpoint gives an
  authoritative call roster, and it doesn't watch the roster (the user's decision of 2026-10-23: a participant from
  outside the lab can appear only through a tenant policy bot, forwarding on the callee's account or a transfer, none
  of which the lab expects). This plan writes that rule down and removes what forbade it; mapping the call writes is
  M3's.
- **Where "default allow" stops**: on the few services whose purpose is reaching people (the chat service, calling,
  Graph, Outlook, SharePoint sharing, AMS, the middle tier's team, meeting and invite paths, CSA writes), a mutating
  request is treated as contacting unless it's a mapped write or a named carve-out. Everywhere else every request is
  allowed. A carve-out is a table row and a test, added as needed, with no capture review.

## Context (from discovery)

- **Base: the M2 branch after M2 Task 25** (`native-teams-m2-gpui-daily-driver`; the user's decision of 2026-10-23,
  revised the same day from "after M2 merges": the old fence slows M2's remaining work too much). M2 Tasks 1–26 are
  done there; M2 Tasks 27 (the personal build) and 28 wait until this plan is complete, then build on the new fence.
  Every path below is as it is on that branch. Task 2 re-runs discovery there before any code changes.
- **The personal build keeps its own rules** (`docs/personal-build.md`, approved 2026-10-23). Where it relies on a
  piece this plan removes for the lab (`PostRead`, `OwnState`, `check_post_read`, `OwnAvailability`, the host
  allowlist, `OwnFrame`, `FrameRecord`, `Classifier::sign_in`, `OwnRule`, `RequestOutcome::Own`), the piece stays for
  the personal build, or moves into the personal fence or `teams-signin`; its rules don't change. The personal build's
  code doesn't exist yet (M2 Task 27), so Task 2 reads `docs/personal-build.md` and M2 Task 27's text for what it will
  need. Every "delete" below means "delete unless Task 2 found a personal-build need".
- **Browser fence** (`crates/proto-lab`): `src/classify.rs` (`Classification::{Read, Own, PostRead, Write, Unknown}`,
  `OwnKind`, `OwnRule`, `UrlPattern`, the per-run classifier sets, `map_write`), `src/post_read.rs`,
  `src/availability.rs`, `src/fence_http.rs`, `src/fence_ws.rs` (`FrameClassifier`, `FrameRule`), `src/calling.rs`
  (`call_start`, `CALL_START_URL`, the Trouter host check), `src/session.rs`, `src/scenario/expected.rs` (~40 classes),
  `src/scenario/*` (each picks a classifier and a `DenyPolicy`; on M2 also `ab_file.rs`, `chat_shapes.rs`,
  `own_presence.rs`). Tests: `tests/{fence_http,fence_targets,fence_ws,expected_denies,post_reads,own_availability,
  capture,calling,login,auth_redirect,scenario_dsl,abc_chat,ab_trouter}.rs`. `spikes/echo-call/tests/signaling.rs`
  calls `proto_lab::calling::call_start`.
- **Sign-in window** (`crates/teams-signin`): `src/guard.rs` and `tests/window.rs` use `Classifier::sign_in()`,
  `OwnRule` and `Classification::{Own, PostRead, OwnAvailability}`; `docs/personal-build.md` says the personal sign-in
  window keeps the lab's `sign_in` rules.
- **Fence core** (`crates/teams-fence`): `src/decision.rs` (`Fence::check`, `Fence::lab_accounts_only`, the
  own-account check for `OwnAvailability`, the `AccountScope` deny), `src/post_read.rs`, `src/membership.rs`
  (`LiveMembershipSource`, `LabGroup`, `chats.toml`, the abc shape check, `is_group_chat_id`, `reads_in_full`, the
  member-list checks: `totalMemberCount`, `counts_match`, `hidden`, duplicates, `isDeleted`), `src/target.rs`
  (`WriteKind::{Message, Edit, Delete, Reaction, Typing, ReadReceipt, ChatCreate, MemberChange, Call,
  OwnAvailability}`, `Scope`), `src/log.rs` and `src/lib.rs` (`FrameRecord`). Tests:
  `tests/{fence_core,group_chat,lab_config,post_read,own_availability,membership,log_processes}.rs`.
- **Native client** (`crates/teams-proto`): `src/own.rs` (sealed `OwnState`), `src/post_read.rs` (sealed `PostRead`,
  `AuthorizedPostRead`), `src/write/availability.rs`, `src/frame.rs` (sealed `OwnFrame`, `FrameSink`), `src/http.rs`
  (host allowlist), `src/client.rs` (`Transport::{read, own_state, deliver}`), `src/endpoint.rs` (`Service`, incl.
  `Graph`). Users: `teams-trouter`, `teams-core` (`profiles.rs`, `presence.rs`, `own_presence.rs`,
  `outbox/transient.rs`, `engine.rs`), `teams-headless`, `teams-mock`, `teams-ui`, `teams-app`. `Transport` fakes:
  `teams-ui/tests/own_presence.rs`, `teams-headless/tests/cli.rs`, `teams-trouter/tests/session.rs`,
  `teams-proto/tests/own_availability.rs`, `teams-core/tests/{engine,outbox,sync,own_availability}.rs`,
  `fence-seal/tests/ui/proto_register_fake_transport.rs`.
- **Seal proofs** (`crates/fence-seal/tests/ui/`): `proto_own_state`, `proto_own_frame`, `proto_raw_request`,
  `proto_raw_frame`, `proto_post_read*`, `post_read_permit*`, `proto_availability_literal`,
  `proto_registered_endpoint`, `proto_presence_read_literal`, `proto_register_fake_transport`.
- **Docs and rules**: `CLAUDE.md` hard rules and Patterns; `docs/protocol/{endpoints,fence-targets,trouter,
  calling-echo,auth}.md`; backlog `seeded-own-rules-ignore-the-query.md`, `group-chat-member-read-completeness.md`,
  `bot-invokes-and-message-extensions.md`.
- **Out of scope**: the personal build's rules; mapping the call writes (M3: the call start to lab callees, answering,
  the follow-ups, the call registry). This plan only makes sure nothing in the rules or the code's structure forbids
  them (Technical Details, "Calls").

## Development Approach

- **testing approach**: TDD (tests first). Each task writes its failing tests first, then the code.
- complete each task fully before moving to the next; small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task, success and error cases
- **CRITICAL: all tests must pass before starting next task** - no exceptions. So the order is additive first: a new
  API lands next to the old one, callers move, and the old one is deleted in the task that moves its last caller (or
  in Task 8, after a check that nothing uses it).
- **CRITICAL: update this plan file when scope changes during implementation**
- **every task updates `CLAUDE.md` for what its code changed, in the same change** (the user's rule of 2026-10-23:
  `CLAUDE.md` reflects the current state, with no history): the hard rules reach the quoted text of Technical Details,
  minus its history notes, as the code that backs each part lands; Patterns lines change as their code does
- each task passes the gate: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D
  warnings`, `scripts/lint-report.sh`, `cargo test --workspace`; `cargo test -p fixture-guard` after touching any
  `tests/fixtures/` or `docs/`
- a test that pinned an old deny is rewritten to pin the new verdict, never just removed while the request is still
  contacting

## Testing Strategy

- **unit tests**: `teams_fence::contacting` gets a table test with a row per family, mapped write, carve-out and
  `Unreadable` form, from its own synthetic fixtures (`crates/teams-fence/tests/fixtures/`, written from the other
  crates' synthetic fixtures and the reference copies, never from captures directly). Membership tests for 1:1, group
  and meeting chats.
- **fence suite**: `fence-seal` still proves only `Fence::check` mints a `WritePermit` and only `teams-proto`
  implements `Mutating`; a runtime test proves the native transport refuses a plain request `contacting` doesn't call
  `None`, without sending it (the mock origin sees nothing).
- **browser tests**: `proto-lab`'s fence tests rewritten to the new verdicts; arming tests untouched.
- **live**: Task 10 (lab accounts only).
- M2's GPUI UI tests stay green throughout.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope

## Solution Overview

- **One classifier for both sides**: `teams_fence::contacting(&Request) -> Contact`, pure (no IO). `Request` holds the
  method, the URL, the headers that change what a request does, and the body; a `Policy` value carries the echo call's
  callee. The browser fence and the native transport both call it.
- **Three verdicts**: `Contact::None` (allowed), `Contact::Write(Target)` (judged by `Fence::check` against the
  target's fresh member list), `Contact::Unreadable` (a mutating request on a contacting service that is neither a
  mapped write nor a carve-out: denied). A request counts as mutating when its method isn't `GET`/`HEAD`/`OPTIONS`,
  when a `GET` has a body, or when it carries `X-HTTP-Method-Override` or `X-HTTP-Method` naming another method.
- **Contacting services by host, default `Unreadable`** (Technical Details): on those hosts every mutating request is
  contacting unless the table maps or carves it out. A chat-service host whose path the classifier can't read is
  `Unreadable`, never `None`. Every other host is `None`.
- **Every allowed mutating request is logged** to `fence.log` as `allowed` with its method, host and path shape (ids
  replaced by their kind: `<chat>`, `<message>`, `<mri>`, `<guid>`), never a query value, header or body. That's the
  record that shows a family is missing, and the data Task 9's catalog review works from.
- **Non-contacting `WriteKind`s** (`Typing`, `ReadReceipt`, `OwnAvailability`) stay in the outbox (it orders writes on a
  chat's read horizon and on presence), and `Fence::check` permits them without a member read, logged. `Typing` and
  `ReadReceipt` on any chat; `OwnAvailability` keeps its own-account check (the account's own scope only) and
  `RegisteredEndpoint`, which cost nothing and keep a lab account from setting another's presence. The `AccountScope`
  deny for contacting kinds stays.
- **Group chats** are judged like 1:1 chats: one fresh member-list read, complete and all lab accounts (or the echo
  service). `chats.toml` stays lab data for the scenarios; the fence no longer reads it.
- **The native transport** gains a plain `Request` class (any method, an unsealed trait), checked by `contacting` and
  refused unless `None`, next to `Read` and `Authorized`. `https` only, no proxy, no redirects, each token only to its
  own service's host; a new service is a code change with no review.
- **Frames**: on a Trouter socket the guard parses the socket.io 0.9 type and allows `0` (disconnect), `1` (connect),
  `2` (heartbeat) and `5` (event) only; `3` (message, the delivery response), `4` (JSON message), `6` (ack), binary
  frames and anything that doesn't parse are denied. Frames on calling-host sockets stay as today's `calling` frame
  rules have them. Other sockets: allowed. The native `FrameSink` keeps typed frames (no raw text, no delivery
  response) but drops the per-frame `fence.log` record and the review rule.

## Technical Details

### The contacting services and their table

Task 2 settled the host list from `docs/protocol/endpoints.md`, `calling-echo.md` and `auth.md` (and the hosts
`ab-file`'s and `ab-image`'s forced denies name); it is in `contacting.rs` (`Service`, `service_of`) and below. On each
service below, a mutating request is `Unreadable` unless a row maps it or carves it out. Hosts are matched in lower
case on a dot boundary (the host or any host under it), a trailing dot dropped, any scheme and port; on a Teams origin
(`teams.microsoft.com`, `teams.cloud.microsoft`, `teams.live.com` and the hosts under them) the service is told by the
path's first two segments, each decoded on its own and in any case (a later segment that doesn't decode leaves the
service to its row, which refuses it); a mutating request whose first two segments can't be read (one empty with more
path after it, not UTF-8 once decoded, or holding `/ \ ; ? #` once decoded) is `Unreadable`. A method override on a
contacting service is `Unreadable` (the fence can't tell which method the service acts on); a mutating request whose
URL doesn't parse is `Unreadable`.

**Chat service** (a Teams origin's `/api/chatsvc/…`, `*.msg.teams.microsoft.com`): any version, any case of `ME`
(`ME`, `me`, the account's own MRI, which `Policy::with_own_account` names), any path `chat_service_path` can read or
not. A mapped write needs a URL `chat_service_path` reads (`https`, an exact AFD host or a regional host, `v1`), so the
mapping and the live member read accept the same URLs; any other chat service URL is `Unreadable` unless carved out.
Any path segment that is empty or holds `/ ; ? #` once decoded, and any query key twice in any case, is `Unreadable`.

| Request | Verdict |
|---|---|
| `POST v1/users/ME/conversations/{chat}/messages`, `Text`, `RichText`, `RichText/*` | `Write(Message)` on the chat, mentions added (today's `message_write`, `chat_only`, `mentioned`) |
| same path, `Control/Typing`, `Control/ClearTyping`, the body read by `strict_json` with only the captured keys (`content`, `contenttype`, `messagetype`; so no `mentions`, no `crossPostChannels`) and `content` empty or absent, `{chat}` a chat id by `chat_scope`; a typing body in any other form is `Unreadable` | `None` (typing) |
| `PUT …/messages/{id}`, `DELETE …/messages/{id}` | `Write(Edit)`, `Write(Delete)` |
| `PUT`/`DELETE …/messages/{id}/properties?name=emotions` (exactly one query key) | `Write(Reaction)` |
| `PUT v1/users/ME/conversations/{chat}/properties?name=consumptionhorizon` (exactly one query key, `{chat}` a chat id) | `None` (read receipt) |
| `PUT v1/users/ME/conversations/{id}/properties?name=<n>`, `<n>` in `OWN_VIEW_PROPERTIES` (`lastTimeFavorited`, seen on a channel; `favorite`, the bundle's form in `writes.json`), exactly one query key, `{id}` a `19:` chat or channel id | `None` (the account's own view of a conversation) |
| `PUT v1/users/ME/conversations/48:{notifications,mentions}/messages/{id}/properties?name=isread`, exactly one query key, body exactly `{"isread": <string>}` | `None` (own feed) |
| `PUT v1/users/ME/properties?name=<n>`, exactly one query key | `None` (own settings) |
| `PUT`, `POST`, `DELETE` on `v1`/`v2` `users/ME/endpoints`, `…/endpoints/<id>`, `…/endpoints/<id>/active`, `…/endpoints/<id>/activeendpoint`, `users/ME/activeendpoint` | `None` (own session) |
| `POST v1/threads` | `Write(ChatCreate)` with the body's members |
| `PUT v1/threads/{chat}/members[/{m}]`, `DELETE v1/threads/{chat}/members/{m}` | `Write(MemberChange)` |
| anything else: other `48:` streams and paths (notes, `sharedcalllogs`, forward, move), `8:…`/`28:…` conversation ids, drafts and scheduled sends, `PUT`/`DELETE` on a conversation itself, bot invokes (`agents/…`), channel and team ids, reply chains, any id segment holding `/ ; ? #`, any duplicate query key | `Unreadable` |

**Calling** (`*.flightproxy.teams.microsoft.com`, whose `ep/<host>/…` forms reach the hosts behind it,
`api.fl.teams.microsoft.com`, and every `*.skype.com` host but AMS's: `*.conv.skype.com`, `*.cc.skype.com`,
`*.broker.skype.com` and any other Skype host, since one whose purpose is unknown counts as contacting): every mutating
request is `Unreadable` except the echo call start that `calling.rs` maps today (`CALL_START_URL`, no query, the
captured shape), `Write(Call)` to the policy's echo callee (`Policy::with_echo_callee`; without one it is
`Unreadable`). Until M3 maps the
rest (Technical Details, "Calls"), a call to a lab account, the `acceptance` link and the follow-ups stay
`Unreadable`; each becomes a row of this table, not a new mechanism.

**Graph** (`*.graph.microsoft.com`, every version, `$batch` included): every mutating request `Unreadable`.

**Outlook, OWA and Substrate** (`*.outlook.office.com`, `*.outlook.office365.com`, `*.outlook.cloud.microsoft`,
`*.outlook.live.com`, `*.substrate.office.com`): every
mutating request `Unreadable` except `POST /search/api/<version>/suggestions` (the one `endpoints.md` lists) and
`…/query` on Substrate: search reads.

**SharePoint and OneDrive** (`*.sharepoint.com`, `*.sharepoint-df.com`, `*.microsoftpersonalcontent.com`,
`*.onedrive.com`, `onedrive.live.com`, the media hosts `*.svc.ms`: `ab-file`'s list): every mutating request
`Unreadable`.

**AMS and the async gateway** (`*.asyncgw.teams.microsoft.com`, `*.asm.skype.com`, a Teams origin's `/asm/…` and
`/ams/…`): object creation, `…/objects/<id>/forward` and an object's content upload are `Unreadable` (no mapping is
left: the M2 mapping of the a–b chat's image object was removed, and the fence can't tell whether the account made an
object); the token exchanges (`POST …/v1/skypetokenauth`, `POST …/v1/<id>/aadtokenauth`), which trade the account's
own token for the media service's cookie, are `None`.

**Middle tier and apps** (a Teams origin's `/api/mt/…` and `/api/apps/…`): a mutating request with a segment (in any
case) `team(s)`, `chat(s)`, `channel(s)`, `conversation(s)`, `thread(s)` or `group(s)`, a segment of its Outlook REST
proxy (`v2.0`, `event(s)`, `message(s)`; `v2.0/me/calendars/default/calendarView` is read through it), or a segment
holding `meeting`, `invite`, `calendar` or `mail`, is `Unreadable`, except `POST …/beta/teams/apps/aggregatedEntitlements`
(the app catalogue of the account's teams, a read); the rest of the middle tier is `None`, out-of-office with automatic
replies (`…/automaticRepliesSetting`) included: the user counted it as own settings on 2026-10-23.

**CSA** (a Teams origin's `/api/csa/…`, `*.chatsvcagg.teams.microsoft.com`): mutating requests `Unreadable` except
`POST …/api/<version>/teams/users/me/chatsAndChannelsByIds`, the read by `POST` `endpoints.md` lists.

**Everything else**: `None` (telemetry, logging, apps, middle-tier settings and profile lookups, searches, presence and
`forceavailability`, the registrar, Entra and authz, the push service `/api/tps/…`, whose `pushupdates` `POST` is the
account's own subscription, Trouter's HTTP side, the page and its static hosts).

Bodies are read by the strict JSON reader (case-folded duplicate keys → `Unreadable`), path segments percent-decoded,
query values exact.

### Membership (`teams-fence/src/membership.rs`)

- 1:1 chats: unchanged.
- Group chats (`19:…@thread.v2` by `is_group_chat_id`) and meeting chats (`19:meeting_…@thread.v2`, by a new
  `is_meeting_chat_id` predicate, so `is_group_chat_id`'s other users, `teams-proto/src/chat/model.rs` among them,
  don't change): one fresh member-list read, `Complete` when there's no paging key, `totalMemberCount` equals the
  members listed (`counts_match`), no member is `hidden` or `isDeleted`, no MRI appears twice, every MRI is in a form
  the captures showed, and every member is a lab account or an approved service. Otherwise the existing reasons:
  `Partial(Paginated)`, `Partial(Truncated)`, `Partial(FetchFailed(Timeout))`, and a non-lab member denies.
- Deleted: `LabGroup`, the `chats.toml` reading, `PartialReason::UnrecordedGroupChat`, the thread read, the `eTag`
  comparison and the two-read budget. The thread read's `threadType` check goes because the chat id already says what
  the chat is, and the member list carries everything the fence judges; the other checks stay.
- `LiveMembershipSource::new` and `for_fence` both admit group and meeting chats; `for_fence` stays the lab constructor.

### Calls (what M3 builds on)

The rule, as the new hard rules state it: a call is a contacting write whose participants are its callees, its
caller and whoever it adds.

- **Starting**: allowed when every callee is a lab account or the echo service (`Scope::Call` with its `to[]`), in the
  shape the call start's capture shows; the native client registers the calling stack's own Trouter endpoint
  (`SkypeSpacesWeb`) freely: it's the account's own session (registrar → `None`).
- **Answering**: allowed when the incoming call's invitation (the Trouter delivery) names a caller and participants
  that are all lab accounts or the echo service. Answering is the `acceptance` request and, where the flow uses one, a
  typed delivery response for that call; every other incoming call is ignored, never answered.
- **Follow-ups** (`updateEndpointMetadata`, `applyChannelParameters`, `acknowledge`, `mediaRenegotiation`, hold,
  `updateCallState`, `controlVideoStreaming`, screen sharing, `leave`): allowed on a call in the fence's **call
  registry**, the calls this run's fence permitted a start or an answer for, keyed by the conversation and call ids
  the start's answer and the acceptance bind together. `add` is judged per invitee like a start. A call not in the
  registry is `Partial` and denied.
- **No roster watch**: the user decided on 2026-10-23 not to check later roster updates for non-lab participants.
- **Media** (TURN, ICE, DTLS-SRTP, over UDP or TCP to the relay hosts) doesn't go through the HTTP transport and isn't
  classified: it carries a call the signaling fence already permitted. Audio and video are the same call.
- What this plan does for it: the hard-rules text below says the above; Task 1 drops the Patterns line "Don't stand in
  a registry of what the fence permitted" (see Task 1's scope note); the calling family is a table M3 adds rows to;
  `Scope::Call` and `WriteKind::Call` stay; the frame rule keeps delivery responses denied until M3 adds typed ones
  for a registered call; Task 7 rewrites `docs/backlog/fence-call-follow-ups.md` to this path.

### The new `CLAUDE.md` hard rules

They replace, on the M2 branch's `CLAUDE.md`, everything from "These hold for every agent and every change…" through
the end of the bullet "The native client's fourth list, `teams-proto`'s sealed `PostRead`…"; the bullets on captures
and tokens, the personal build and protocol notes stay as they are:

> These hold for every agent and every change, with no exceptions. The fence's scope is the user's rule of 2026-10-23,
> which replaced M0–M2's default-deny model: **lab accounts contact only each other; everything else is allowed.**
>
> - **A contacting write goes only to the lab.** A contacting write is one that reaches other people: a message (a
>   post, reply, edit, delete, forward, channel post, a scheduled or drafted send), a reaction, a call (starting,
>   joining or answering one), a chat created, a member added or removed, an invite, a meeting, a mail, a share. It is
>   allowed only when every participant of its target, read fresh and complete (no paging key), is one of the 3 lab
>   accounts (`lab.toml`'s MRIs) or an approved service participant (the echo service, by its MRI). That holds for any
>   chat whose members are only lab accounts, group and meeting chats included, a natively created one too. A target
>   whose members can't be read in full, or a write the fence can't tie to a target with certainty, is denied.
> - **Everything else is allowed by default**, in the lab browser and in the native client, with no capture, review or
>   allowlist entry needed: reads by any method (a `POST` search or people lookup too, whoever its results list), the
>   account's own session, tokens and settings (out-of-office with automatic replies included), presence, a status
>   note, typing and read receipts (others may see these; that is fine).
> - **Contacting writes are classified first, by service** (`teams_fence::contacting`, shared by the browser fence and
>   the native transport). On a service whose purpose is reaching people (the chat service, calling, Graph, Outlook,
>   SharePoint, AMS, the middle tier's team, meeting and invite paths, CSA), a mutating request is contacting unless
>   the table maps it to a target or carves it out by name (typing, read receipts, the account's own feed, settings and
>   endpoints); one it can't read is denied, never let through. A carve-out or a new service is a table row and a test,
>   no capture review. Nothing allows a request before this classification runs. Every allowed mutating request is
>   logged to `fence.log` by its method, host and path shape. When unsure whether something reaches people, treat it
>   as contacting.
> - **No token or credential goes to a third party** (a bot, an app's backend): a bot invoke stays denied. Sign-in and
>   token renewal stay in `teams-auth`'s own clients and touch only the account's own credentials.
> - **Calls go only to the lab, both ways.** A call may be started when every callee is a lab account or the echo
>   service, and answered when its invitation names only lab accounts or the echo service; every other incoming call
>   is ignored, never answered (no `acceptance`, no delivery response for it, no scenario click). A write on an
>   existing call is allowed only on a call the fence itself permitted a start or an answer for in this run (its call
>   registry, keyed by the ids the start's answer and the acceptance bind); an `add` is judged per invitee. The roster
>   isn't watched (the user's decision of 2026-10-23). Media (TURN, ICE, DTLS-SRTP) isn't classified: it carries a
>   call the signaling fence permitted.
> - **Live browser runs are armed** (M0 Tasks 6–7): HTTP interception on every host and target, the WebSocket guard,
>   the service worker neutralized. The arming is what lets the fence deny a contacting write; it lets the rest
>   through.
> - **The native client sends a contacting write only as `Authorized<_>`** with a `WritePermit` from `Fence::check`
>   for its exact target, logged to `fence.log` by `Fence::check`; a sealed `Mutating` type in `teams-proto` is the
>   only way to build one, and the transport refuses any other request `teams_fence::contacting` doesn't call `None`.
>   The transport is `https` only, with no proxy and no redirects, and hands each token only to its own service's
>   host. A write is never resent by the transport (the HTTP client's own retries stay off: an HTTP/2 retry could post
>   twice). A mock origin (HTTP or socket) exists only under a `test-support` feature, which a lab binary refuses to
>   run with. No write ever comes from a `cargo test` process: a live test spawns a normally built lab binary (built
>   with `--target-dir target/lab`) and reads its output and `fence.log`.

## What Goes Where

- **Implementation Steps**: code, tests and docs in this repo, on the M2 branch after M2 Task 25.
- **Post-Completion**: the `CLAUDE.md` edit if the agent's auto mode refuses it.

## Implementation Steps

### Task 1: The new hard rules in `CLAUDE.md`

**Files:**
- Modify: `CLAUDE.md`

- [x] ➕ scope changed by the user on 2026-10-23: `CLAUDE.md` states only the project's current state, with no
      history (no dates, decision records, milestone or task numbers, war stories). So Task 1 doesn't paste the quote
      word for word or mark lines "(being removed)": it states the new rule as policy, without the parts that name
      code not yet written (`teams_fence::contacting`, the `allowed` log, the call registry's keys), and says the code
      is still stricter, pointing at this plan; the Patterns keep describing today's code, the process rules the new
      rule voids ("Own-state is added only after a review", the expected-deny review, "Don't stand in a registry")
      reworded or dropped, and all history removed from the file
- [x] replace the hard-rules block named in Technical Details with the new rule, as above
- [x] ⚠️ the auto-mode classifier refused this edit on 2026-10-23 as self-modification: if it refuses again, stop and
      ask the user to apply it (or to allow the edit); don't work around the refusal
- [x] `cargo test -p fixture-guard` passes

### Task 2: Discovery, the endpoint sweep, and `teams_fence::contacting` (additive)

**Files:**
- Create: `crates/teams-fence/src/contacting.rs`, `crates/teams-fence/tests/contacting.rs`,
  `crates/teams-fence/tests/fixtures/contacting/*` (synthetic, marked)
- Modify: `crates/teams-fence/src/lib.rs`, `crates/teams-fence/src/target.rs`, `crates/teams-fence/Cargo.toml`
- Modify: this plan (Technical Details' host list, own-view property names, carve-outs; ➕ tasks)

- [x] on the M2 branch, list every use of the pieces Context names (`teams-signin`, M2's code, and what
      `docs/personal-build.md` and M2 Task 27 will need) and add a ➕ task, or a note on the task that deletes it, for each
- [x] go through every row of `docs/protocol/endpoints.md`, `calling-echo.md` and `auth.md`: settle each contacting
      service's hosts, the carve-outs and the own-view property names, and write them into Technical Details
- [x] write the table test first: a row per mapped write, carve-out and `Unreadable` form of Technical Details,
      including every shape `proto-lab/tests/fence_targets.rs` pins, method overrides and `GET` with a body, an
      unreadable chat-service path, `8:`/`28:`/`48:notes` conversations, a duplicate `name`, a Graph `$batch`, an
      Outlook `sendmail`, a bot invoke, a calling-host `acceptance`
- [x] add `contacting.rs` next to `proto-lab`'s `map_write` (copied in, not moved yet: `proto-lab` keeps building
      unchanged), with the families, carve-outs and the echo call start (the callee in `Policy`)
- [x] `WriteKind::is_contacting()`: false for `Typing`, `ReadReceipt`, `OwnAvailability`
- [x] tests pass
- ➕ discovery (Task 2): the uses of each piece, and the personal build's needs, are noted on the task that would
      delete it (Tasks 4, 5, 6, 8). `docs/personal-build.md`'s header still reads "A proposal, not yet approved"
      (line 3), though the user approved it on 2026-10-23; not this plan's to change.

### Task 3: `Fence::check` and group chats under the new rule

**Files:**
- Modify: `crates/teams-fence/src/{decision,membership,lab,log,lib}.rs`
- Modify: `crates/teams-fence/tests/{fence_core,group_chat,lab_config,membership,own_availability}.rs`
- Modify: `crates/proto-lab/src/scenario/abc_chat.rs` (`reads_in_full`), `crates/proto-lab/tests/abc_chat.rs`

- [x] write tests first: `Typing` and `ReadReceipt` permitted on any chat with no member read, logged;
      `OwnAvailability` still only on the account's own scope; a lab-only group chat not in `chats.toml` is
      `Complete`; a lab-only meeting chat is `Complete`; a paged read, a count mismatch, a `hidden` or `isDeleted`
      member, a duplicate MRI, a non-lab member and a timeout deny with the existing reasons; 1:1 unchanged
- [x] `Fence::check`: non-contacting kinds without a member read
- [x] `membership.rs` as in Technical Details; `is_meeting_chat_id` added, `is_group_chat_id` unchanged
- [x] add the `allowed` `fence.log` record (method, host, path shape) for Tasks 4 and 5 to use
- [x] `lab_accounts_only`, `check_post_read` and `PostReadPermit` stay for now (callers move in Tasks 4–5)
- [x] tests pass
- ➕ note (Task 3): `LabGroup`, `with_lab`, `parse_group` and `PartialReason::{UnrecordedGroupChat,
      ChatsUnreadable, Inconsistent, UnexpectedRoster}` are deleted (no caller left); the group read is `parse_members`
      over one member-list read, still bounded by the source's deadline (`with_deadline`, tests only). Callers of
      `with_lab` moved too: `teams-core/tests/engine.rs` (the group stand-in and `Groups` gone; a natively created
      lab-only group chat now takes its first message), `teams-headless/tests/cli.rs`, `teams-app/tests/window.rs`,
      and `fence-seal`'s `live_with_lab` case became `live_with_deadline` (`live_mock_origin.stderr` follows
      `for_fence`'s signature). Tests that pinned the old denies now pin the new verdicts:
      `proto-lab/tests/fence_targets.rs` (a group chat is read), `teams-core/tests/engine.rs` (typing goes out without
      a member read; the denied row uses the outsider's chat), `teams-proto/tests/chat_write.rs` (typing and read
      receipts pass on a chat with an outsider; a lab-only group chat, a new one included, is written to). Typing and
      read receipts skip the member read only on a chat (1:1, group or meeting id), as `contacting` carves them out
      only there; on a channel they are judged like any write. The `allowed` record is
      `FenceLog::record_allowed(&AllowedRecord { method, url })` with `teams_fence::path_shape`, which also names
      opaque ids `<id>`.

### Task 4: The lab browser fence: HTTP, frames and scenarios together

**Files:**
- Modify: `crates/proto-lab/src/{classify,fence_http,fence_ws,calling,session,lib}.rs`,
  `crates/proto-lab/src/commands/{auth,login,status}.rs`
- Modify: `crates/proto-lab/src/scenario/{mod,expected,load,ab_chat,ab_chat_full,ab_image,ab_file,ab_trouter,abc_chat,chat_shapes,echo_call,own_presence}.rs`
  (and any other scenario on the M2 branch)
- Delete: `crates/proto-lab/src/post_read.rs`, `crates/proto-lab/src/availability.rs`
- Modify: `crates/proto-lab/tests/{fence_http,fence_targets,fence_ws,expected_denies,calling,login,auth_redirect,capture,scenario_dsl,ab_trouter}.rs`
- Delete or rewrite: `crates/proto-lab/tests/{post_reads,own_availability}.rs`
- Modify: `crates/teams-signin/src/guard.rs`, `crates/teams-signin/tests/window.rs` (they keep `sign_in` and `OwnRule`,
  moved into `teams-signin` if `proto-lab` no longer needs them)
- Modify: `spikes/echo-call/tests/signaling.rs` (or keep `proto_lab::calling::call_start` as a re-export)

- [ ] rewrite the tests first:
      - HTTP: requests the old fence denied (off-list own state, `PostRead` naming a non-lab MRI, settings, apps,
        presence) are let through and logged `allowed`; every contacting shape is `Write` and judged; `Unreadable` is
        denied; a forced deny still wins before classification
      - frames: socket.io types `0`, `1`, `2`, `5` allowed on Trouter sockets; `3`, `4`, `6`, binary and unparsable
        denied; calling-host frames as today; foreign-realm refusal and the token check unchanged
      - scenarios: a run whose denies are only its forced denies and the reviewed expected list passes; any other
        deny fails the run naming its class
- [ ] `Classification` becomes `Read | Allow | Write(Target) | Unknown`, `classify` calls `teams_fence::contacting`,
      `proto-lab`'s own `map_write` and its helpers are deleted; one `Classifier` with `with_test_call_callee`
- [ ] `FrameClassifier` collapses to the frame rule; `FrameRule`, `seeded`/`calling`/`messaging`/`without_activity` go
      (keeping what the calling-host frames need)
- [ ] `expected.rs` shrinks to the contacting-service classes a run meets but doesn't need, each with its reason (no
      capture review): bot invokes on editor open, the account's own channel entry
      (`PUT v1/threads/<channel>/members/<own>?returnThreadMetadata`) and channel properties, delivery responses, and
      `echo-call`'s call follow-ups (still exit 1); every scenario stops picking a classifier
- [ ] tests pass; arming tests (`tests/browser.rs` and friends) untouched and green
- ➕ note (Task 2 discovery): `teams-signin` is the only crate outside `proto-lab` that classifies at run time:
      `src/guard.rs` uses `Classifier::sign_in`, `OwnRule` (`Guard::with_own`, `window.rs`'s hook),
      `Classification::{Own, PostRead, OwnAvailability}` and `RequestOutcome::Own`; `tests/{window,guard}.rs` build
      `OwnRule::new(OwnKind::EntraSignIn, …)` and check the `own` log names `entra_token` and `entra_sign_in`. The
      personal sign-in window keeps the lab's `sign_in` rules and extends them (`docs/personal-build.md` lines 193,
      212-214), so `sign_in`, `OwnRule`, `OwnKind::{EntraToken, Authz, EntraSignIn}` and `RequestOutcome::Own` stay
      (moved into `teams-signin` if `proto-lab` stops needing them). `spikes/echo-call/tests/signaling.rs` calls
      `proto_lab::calling::call_start` and `TEST_CALL_SCENARIO`: switch it to `teams_fence::contacting` (and
      `teams_fence::TEST_CALL_SCENARIO`) or keep a re-export. `strict_json` has users in `calling.rs`,
      `availability.rs` and `post_read.rs`: whichever survive need a copy or `teams-fence`'s (crate-private today).

### Task 5: The native transport: plain requests, checked by `contacting`

**Files:**
- Modify: `crates/teams-proto/src/{client,http,lib,endpoint}.rs`
- Create: `crates/teams-proto/src/request.rs` (the unsealed `Request` trait); `EndpointRegistration` and the
  `PostRead` requests move onto it
- Delete: `crates/teams-proto/src/own.rs`, `crates/teams-proto/src/post_read.rs` (unless Task 2 found a personal-build
  user)
- Modify: `crates/teams-proto/src/write/availability.rs` (stays `Mutating`, kind `OwnAvailability`; the
  captured-values-only rule text goes, the values it builds stay)
- Modify: `crates/teams-trouter/src/registration.rs`, `crates/teams-core/src/{profiles,presence,own_presence,engine}.rs`,
  `crates/teams-core/src/outbox/transient.rs`, `crates/teams-headless/src/{availability,main}.rs`,
  `crates/teams-mock/src/{chatsvc,ups,world,lib}.rs`
- Modify (fakes and callers): `crates/teams-proto/tests/{policy,http,fence_types,csa,graph,presence,own_availability}.rs`,
  `crates/teams-core/tests/{engine,outbox,sync,own_availability}.rs`, `crates/teams-ui/tests/own_presence.rs`,
  `crates/teams-headless/tests/{cli,chat_live}.rs`, `crates/teams-app/tests/ui_live.rs`,
  `crates/teams-mock/tests/mock.rs`, `crates/teams-trouter/tests/session.rs`
- Modify: `crates/fence-seal/src/lib.rs`, `crates/fence-seal/tests/ui/*`
- Modify: `crates/teams-fence/src/{decision,post_read,lib}.rs`, `crates/teams-fence/tests/post_read.rs` (delete
  `check_post_read`, `PostReadPermit`, `lab_accounts_only` once their last caller is gone, unless the personal build
  uses them)

- [ ] write tests first: a `Request` that `contacting` calls `None` is sent (any method, any MRIs in its body) and
      logged `allowed` when mutating; one it calls `Write` or `Unreadable` is refused with a typed error and never
      reaches the mock origin; a profile lookup naming a non-lab MRI is sent; a Graph `POST` is refused
- [ ] `Transport` becomes `read`, `send<R: Request>`, `deliver<R: Mutating>`; `send` builds the URL from the request's
      `Service`, runs `teams_fence::contacting`, refuses anything but `None`; the host allowlist becomes the
      per-`Service` base (`https`, no proxy, no redirects, each token only to its own service's host)
- [ ] move every caller and fake onto `send`; M2's GPUI UI tests stay green
- [ ] `fence-seal`: delete `proto_own_state`, `proto_post_read*`, `post_read_permit*`; update
      `proto_register_fake_transport`, `proto_availability_literal`, `proto_registered_endpoint`,
      `proto_presence_read_literal` to the new API; keep that `Mutating` and `WritePermit` can't be made outside their
      crates; add a case that a `Request` can't be handed to `deliver`
- [ ] tests pass
- ➕ note (Task 2 discovery): the personal build relies on `PostRead` / `AuthorizedPostRead`, `check_post_read`,
      `PostReadPermit` and the lab's `lab_accounts_only` limit for lab builds (`docs/personal-build.md` lines 11, 78,
      148-152, 382, 429; M2 Task 27: "a `PostRead` for a non-lab MRI the account's chats name is allowed"), on the
      sealed `OwnState` list (lines 146-147), on the host allowlist (lines 144, 362-363: "the fixed host allowlist,
      `https` only, no proxy, no redirects", for both builds), and on `WriteKind::OwnAvailability` on `Scope::Account`
      (lines 113-117, 380). So `teams-proto/src/{own,post_read}.rs`, `check_post_read`, `PostReadPermit` and
      `lab_accounts_only` stay for the personal build: the lab's callers (`teams-core` `profiles.rs`, `presence.rs`,
      which also calls `lab_accounts_only`) move onto `send`, the pieces aren't deleted, and the per-`Service` base
      keeps the allowlist's form. `EndpointRegistrar` and `register` (from `own.rs`) are used by `teams-trouter`
      (`registration.rs`, `session.rs`) and `teams-core/src/engine.rs`. The `Transport` fakes that implement
      `own_state` and `post_read`: `teams-core/tests/{outbox,sync,engine,own_availability}.rs`,
      `teams-headless/tests/cli.rs`, `teams-ui/tests/own_presence.rs`, `teams-trouter/tests/session.rs`,
      `teams-proto/tests/{fence_types,own_availability}.rs`, `fence-seal/tests/ui/proto_register_fake_transport.rs`;
      the host allowlist's `accepts_csa_base` and `accepts_presence_base` are used by `teams-core/src/lab.rs`.

### Task 6: Native frames: typed, unreviewed

**Files:**
- Modify: `crates/teams-proto/src/frame.rs`, `crates/teams-trouter/src/{lib,session}.rs`,
  `crates/teams-fence/src/{log,lib}.rs` (`FrameRecord`)
- Modify: `crates/teams-proto/tests/frame.rs`, `crates/teams-trouter/tests/session.rs`,
  `crates/teams-fence/tests/{fence_core,log_processes}.rs`, `crates/teams-headless/tests/chat_live.rs`,
  `crates/teams-app/tests/ui_live.rs`, `crates/fence-seal/tests/ui/proto_own_frame.*`

- [ ] write tests first: the sink sends every typed frame without a `fence.log` record; no constructor takes raw
      text (`proto_raw_frame` stays) and none makes a delivery response yet (M3 adds a typed one for a registered
      call)
- [ ] drop `FrameRecord` (unless the personal build uses it) and the "reviewed list" rule; the trait stays sealed only
      to keep raw text out, so adding a frame (e.g. `user.activity`) is an ordinary change
- [ ] tests pass
- ➕ note (Task 2 discovery): the personal build logs its sealed `OwnFrame` list to the personal `fence.log`
      (`docs/personal-build.md` lines 146-147), so `FrameRecord` stays; only the lab's per-frame review rule goes.
      `FrameSink` and `OwnFrame` are `teams-trouter/src/session.rs`'s only way to send a frame.

### Task 7: The endpoint catalog and protocol notes

**Files:**
- Modify: `docs/protocol/endpoints.md`, `docs/protocol/fence-targets.md`, `docs/protocol/trouter.md`
- Delete: `docs/backlog/seeded-own-rules-ignore-the-query.md`, `docs/backlog/group-chat-member-read-completeness.md`
- Modify: `docs/backlog/bot-invokes-and-message-extensions.md`

- [ ] `endpoints.md`: replace the Browser-fence column with "contacting?" (`yes: <service>`, `mapped: <kind>`,
      `carve-out`, `no`); keep the Native column
- [ ] `fence-targets.md`: the services, the table and the member read; the abc shape section goes
- [ ] delete the two backlog items the rule settles; `bot-invokes…`: bot invokes stay denied (a token to a third
      party), say so
- [ ] rewrite `docs/backlog/fence-call-follow-ups.md` to the path in Technical Details, "Calls": the call registry
      instead of an authoritative read, starting and answering lab-only calls, no roster watch (the user's decisions
      of 2026-10-23), and the follow-ups to map in M3
- [ ] `cargo test -p fixture-guard` passes

### Task 8: Remove what nothing uses any more

**Files:**
- Modify: whatever is left of `OwnKind`, `OwnRule`, `UrlPattern`, `RequestOutcome::Own`, the `post_read` log record
  and the `chats_file` readers

- [ ] grep for each piece Context names; delete the ones with no caller left and no personal-build user; list the
      ones kept and why in this plan
- ➕ note (Task 2 discovery): the `chats.toml` readers (`LabChats`, `record_abc_chat`, `chats_file`) have callers
      beyond the fence (`teams-headless/src/lab.rs`, `proto-lab`'s `abc_chat.rs`, `chat_shapes.rs`, `thread_probe.rs`,
      and the tests of `teams-headless`, `teams-app`, `teams-core`, `proto-lab`), so they stay as lab data; `LabGroup`
      and `PartialReason::UnrecordedGroupChat` have no caller outside `teams-fence`. `is_group_chat_id` stays
      (`teams-proto/src/chat/model.rs`); `Fence::open_lab_account` stays (`teams-headless`, `teams-app`).
- [ ] tests pass

### Task 9: Live check under the new fence

Every command under `flock ~/.local/share/teams-lab/live.lock` and `timeout --signal=INT`, `--headless`, one browser
per lab profile, from a normal build (`cargo build -p proto-lab`, the lab binaries with `--target-dir target/lab`),
run in the background.

- [ ] `proto-lab scenario load`, `ab-chat`, `ab-chat-full`, `ab-trouter`: each passes with only its forced denies and
      the reviewed expected list; read `fence.log`'s `allowed` lines and check every mutating one against the table —
      one that reaches people is a missing family: stop, add it (Task 2's file) and rerun
- [ ] `abc-chat`: passes; the group chat's write permitted through the new member read
- [ ] `teams-headless` (or `teams-app --lab`): a message, a reaction, typing and a read receipt in the a–b chat; a
      profile lookup and a presence read of a non-lab colleague go out and come back (reads only)
- [ ] the forced-deny probe of every writing scenario still fails in the page and never reaches the other account
- [ ] a breached run is reported as breached (CLAUDE.md, "A fence breach is never a flake"), never rerun into a pass

### Task 10: Verify acceptance criteria

- [ ] every row of Technical Details' table has a table-test row, and both sides call the one classifier
- [ ] every mutating request the live runs logged `allowed` was checked against the table (Task 9)
- [ ] a natively created lab-only group chat is writable (mock test)
- [ ] no delivery response can be sent from either side yet (frame tests), and nothing in the rules or the types
      stops M3 from adding one for a registered call
- [ ] the full gate passes: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D
      warnings`, `scripts/lint-report.sh`, `cargo test --workspace`

### Task 11: [Final] Update documentation

- [ ] `CLAUDE.md`: check that the hard rules and Patterns match what the code now does, with no history, and drop the
      hard rules' note that the code is stricter than the rule (same refusal note as Task 1)
- [ ] `README.md`, if it names the removed lists or classifiers
- [ ] move this plan to `docs/plans/completed/`

## Post-Completion

*Items requiring the user's decision or action - informational only*

**Order against M2.** The user decided on 2026-10-23 that this plan runs on the M2 branch after M2 Task 25 and before
M2 Task 27; the M2 run stopped after Task 25 for it. When this plan is complete, M2 Tasks 27 and 28 resume on the new
fence. The M2 plan is not edited by this plan: if Task 27's text needs adjusting to the new fence, that's a request
about that plan.

**`CLAUDE.md`.** The agent's auto mode refused editing `CLAUDE.md` on 2026-10-23. If it refuses again in Tasks 1 or
11, the user applies the change (Technical Details' rule, stated as the current state without history) or allows the
edit.

**The personal build's rules** (`docs/personal-build.md`) are the user's and unchanged; whether to simplify them the
same way is the user's call.
