# The personal build: its rules

Status: **approved by the user on 2026-10-23, as written, all five choices included**; in force in CLAUDE.md's hard
rules, and implemented in M2 Task 27 (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`: `crates/teams`,
the `personal` features of `teams-fence` and `teams-signin`). The proposal below was written on 2026-10-22, before the
lab fence was rewritten around "lab accounts contact only each other" (`docs/plans/completed/20261023-lab-fence-contacting-writes-only.md`);
where that rewrite changed what a sentence here assumed, [How the implementation reads it](#how-the-implementation-reads-it)
says how. The current CLAUDE.md wins on everything about the lab; this text wins on the personal build's own rules,
and nothing was loosened.

[Calls](#calls) was added for M3 (`docs/plans/20261024-native-teams-m3-calls.md`, Task 2) from the user's decisions
of 2026-10-24; its wording was approved on 2026-10-24 through a codex-discussion the user delegated the approval to
(transcript: `~/.local/state/plan-exec-revmux/20261024-native-teams-m3-calls/discussions/task-2-call-rules/`), and
CLAUDE.md carries its rule. M3 Task 22 implemented it ([How the implementation reads it](#how-the-implementation-reads-it),
"Calls").

It writes up the direction the user gave on 2026-10-21 (the plan's "Decisions of 2026-10-21", last bullet) against
the code as it stands after M2 Tasks 1–16 and 24: `teams-fence` (`Fence`, `Fence::open_lab`, `LiveMembershipSource`,
`Fence::check_post_read`), `teams-app` (the lab app and its broker), `teams-signin` (the sign-in window, still
loopback-only), `teams-auth` (`KeyringSecretStore`, `Session`, `signin`) and `teams-ui` (the views, `Settings`). Where
it chooses between options the direction leaves open, the choice is marked **Choice** and listed again under
[What the user is asked to decide](#what-the-user-is-asked-to-decide).

## Why the personal build needs rules of its own

The lab's fence answers one question: could anyone outside the 3 lab accounts notice this request? A write passes
only when every participant of its target, read fresh, is a lab account or an approved service. The personal build
is the user's own account talking to the user's own colleagues, so every write it is for fails that question by
design. Its fence answers a different one: **is this write one the user asked for, to a conversation the user is in,
in a shape we know?** Everything else the lab's rules hold stays: sealed request types, permits only from
`teams-fence`, every write logged before it goes, nothing sent that no capture showed, no retry the outbox didn't
decide.

What it does not change: the lab, its fence and its hard rules. A lab binary keeps the lab fence, and nothing in the
personal build widens what a lab account may do.

## The binary and its data

| | lab (`teams-app`, `teams-headless`, `proto-lab`) | personal (`teams`) |
|---|---|---|
| crate | `crates/teams-app` | `crates/teams` (a lib beside `main.rs`, as `teams-headless` and `teams-app` have) |
| UI | `teams-ui` | the same `teams-ui` |
| account | one or more lab aliases per process (`--lab`), from `lab.toml` | the user's own work accounts, each one the sign-in window signs in ([Several accounts](#several-accounts)) |
| config | `~/.config/teams-lab/lab.toml` | `~/.config/teams/accounts.toml` (written by each first sign-in, below) |
| data dir | `$XDG_DATA_HOME/teams-lab/` (or `lab.toml`'s `data_dir`) | `$XDG_DATA_HOME/teams/` (`~/.local/share/teams/`), `0700` |
| store | `<lab data>/store/<alias>.sqlite` | `~/.local/share/teams/store/<oid>.sqlite` (`0600`, as `teams-store` makes it) |
| fence log | `<lab data>/fence.log` | `~/.local/share/teams/fence.log` (`0600`) |
| region (no token) | `<lab data>/auth/<alias>.json` | `~/.local/share/teams/regions/<oid>.json` |
| UI settings | `<lab data>/ui/<alias>.toml` | `~/.local/share/teams/settings.toml` |
| diagnostic log | under the lab's data dir | under `~/.local/share/teams/` (`0600`, rotated; below) |
| Secret Service application | `teams-lab` (`LabPaths::KEYRING_APPLICATION`) | `teams-personal` |
| credentials | through the broker (`teams-app secrets`) | `KeyringSecretStore::new("teams-personal")` in-process: **no broker** |
| window | only in the UI sandbox (`UI_SANDBOX_DIR`), `DISPLAY` unset | the user's desktop session: **no sandbox** |
| bus | the sandbox's private bus | the user's session bus (notifications, tray, Secret Service) |
| sign-in | `proto-lab auth` (the armed Chrome fence) | `teams-signin-personal`, a second binary of `crates/teams` (below) |
| live lock | `~/.local/share/teams-lab/live.lock` around every live command | none: it is the user's own app |
| calls | the UI sandbox's private PipeWire and fake camera portal; the lab's call rule (a call judged by its entry); the echo service from `lab.toml`; OpenH264 in `<lab data>/openh264/` | the desktop's PipeWire and camera portal (fixed when built); the personal call rule ([Calls](#calls)) on the call registry; the echo service from the web bundle (`teams_proto::TEAMS_ECHO_BOT_MRI`); OpenH264 in `~/.local/share/teams/openh264/` (`scripts/fetch-openh264.sh ~/.local/share/teams`, once; without it the calls carry audio only) |

`accounts.toml` holds, for each account, what its first sign-in learned, never a token: the UPN the user typed, and
the ID token's `oid` and `tid`. Every later sign-in must give the same `oid` and `tid` (the ID token's `preferred_username` checked as
`teams_auth::token::TokenClient::exchange_code` checks it today, then the pair compared), so a sign-in that lands in
another account or another tenant stores nothing and says so. Removing an account is an explicit "Sign out and
remove" in the app (or `teams --forget <UPN>`), which deletes its `teams-personal` items, its `accounts.toml` entry and
its region; its store stays, to delete by hand.

The engine is built as `teams_core::lab::start` builds it (the `Session`, the transport, the push channel, the
outbox), from the account's `accounts.toml` entry and the region the authz exchange gives instead of `lab.toml`; Task 27 factors the
shared part out of `lab.rs` rather than copying it. The engine lock (`Store::engine_lock`) keeps one `teams` per
account, as it keeps one lab engine per alias.

The app has a diagnostic log (the user's decision of 2026-10-22), in both builds: a file under the data dir, `0600`
and rotated. At `info` it carries no payload, as every log line does today. At `debug` it carries full HTTP requests
and responses and Trouter frames (bodies, headers, message text), with credentials redacted by the logger itself:
CLAUDE.md's token rule is unchanged (no token, cookie or code reaches any log or stdout). For the personal build
that log holds the user's colleagues' messages: it is the user's data, like the store and `fence.log`, and never goes
into the repo, not even sanitized.

## Several accounts

The user may add several of their own work accounts (the user's decision of 2026-10-27), all of them running at once
in one window: the rail shows each account's avatar above Settings, a click switches to it at once, the "+" under the
avatars adds one, and an account's menu has "Sign out and remove". Every account notifies and rings whichever one the
window shows, its notifications naming it.

- **Each account is the user's own and stands alone.** It is in `accounts.toml` (`[[account]]` tables, in the order
  added; a single account's `account.toml` and `region.json` from before are moved there, and to
  `regions/<oid>.json`, at the next start), it has its own store (`store/<oid>.sqlite`), region, refresh token and
  skypetoken (`teams-personal` items keyed by its UPN), its own engine (its own engine lock) and its own personal
  fence, `Fence::open_personal(paths, upn)`, which permits only that account's writes, to conversations its own fresh
  read lists it in. Nothing of one account is used for another. A lab account is refused as before, by UPN or MRI.
- **Adding one** goes through the same window as the first sign-in (the account asked for, then Microsoft's sign-in
  window bound to that account's identity provider hosts); a sign-in that stores nothing leaves no account behind.
  Once it goes through, the account's engine starts and its avatar shows; nothing else restarts.
- **Removing one** stops its engine first, then forgets it as `teams --forget <UPN>` does: its `teams-personal` items,
  its `accounts.toml` entry and its region; its store stays, to delete by hand.
- **Calls, one at a time across the accounts.** The accounts' fences share one call floor (`teams_fence::CallFloor`):
  while any account has a call up or ringing, an invitation to another account isn't offered (it rings on the user's
  other clients) and no other account starts, joins or answers a call. The calls' devices are the one PipeWire the
  build opens, shared.
- **The settings** are one file for the app (`settings.toml`): the first account's last chat and manual status are
  its top-level keys as before, the other accounts' under `[accounts."<mri>"]`. Typing, read receipts, presence and
  the first run's confirmation hold for every account.
- **`fence.log`** is one file for every account; each decision names its target, which names the account's own chats.

## The personal fence

### Where it lives

In `teams-fence`, behind a cargo feature `personal` that only `crates/teams/Cargo.toml` enables. `Fence` keeps its
one type (the transport, the outbox and the push channel all take an `Arc<Fence>` today) and gains a policy inside
it: the lab's roster and group chat as now, or, only with `personal`, the personal account. The only way to a
personal policy is `Fence::open_personal(paths, upn)`, which exists only with the feature, reads that account from
the personal paths' `accounts.toml` and opens the personal `fence.log`; one per account. Permits, `Authorized<_>`, `PostReadPermit` and the sealed
`MembershipSource` stay where they are: nothing outside `teams-fence` mints one, in either build.

### What it permits

A write is permitted only when all of these hold, each checked when the fence decides:

1. **It is a sealed `Mutating` write sent as `Authorized<_>`**, through the outbox (durable rows) or its transient
   kind (typing, the account's own availability), with its permit from `Fence::check` for its exact `Target`. No new
   way out: the outbox's rules, the retry table and "no resend across runs" are unchanged.
2. **Its kind is one the personal build sends in M2**: `Message`, `Edit`, `Delete`, `Reaction` (the keys a capture
   showed), `Typing`, `ReadReceipt`, and the account's own availability once Tasks 18–19 have mapped it. Every other
   kind (`ChatCreate`, `MemberChange`, `Invite`, `ChannelPost`, `Call`, `CallAnswer`, `Meeting`, `BotReply`) is
   denied by kind, and so is any `Scope::New` target. M3 adds the call writes, each only as [Calls](#calls) allows
   (the test call's chat create its only `ChatCreate`, its `add` of the echo bot its only `Invite`, and its chat
   create and start its only `Scope::New` writes).
3. **Its target is a conversation the account is a member of, by a fresh member read**: the fence's personal
   membership source (a new sealed source in `teams-fence`) reads the conversation's thread from the chat service at
   decision time (`GET v1/threads/<id>?view=msnp24Equivalent`, the read `LiveMembershipSource` already sends) and the
   write passes only when the answer is in a shape a capture showed and lists the account's own MRI as a member.
   Never a cache, never the store's member list. A paged or truncated answer (any paging key, next link or
   continuation) is a shape no capture showed: no capture has ever seen a member read page
   (`docs/protocol/fence-targets.md`: the thread read had no paging key, and the member list's `pageSize=1` got every
   member and no next link), so it is `Unknown` and denied, as `parse_thread` and `parse_group` already treat a page
   marker as `Partial(Paginated)`. A group chat whose read
   pages stays read-only until a lab capture shows the paging form and a review admits it.
4. **The conversation is a kind the lab has captured**: a 1:1 chat whose id names the account, or a group chat
   (`19:<id>@thread.v2`, `threadType` `chat`, not a meeting's id). Channels stay read-only (both builds, the user's
   decision of 2026-10-21); meeting chats (`19:meeting_…`), the account's own streams (`48:…`) and any thread whose
   type or member shape no capture showed are denied.
5. **It comes from an explicit action in the UI, or from a setting the user turned on.** A message, an edit, a
   delete, a reaction or "send again" is a key or a click on that one item; typing, read receipts and the account's
   availability go out only while their setting is on (below). The one exception is the call's own sends
   [Calls](#calls) names (a registered call's setup, keep-alive and end writes, and an offered invitation's ring-time
   sends), which go without a further action. Nothing else is sent on a timer the user didn't enable, and nothing
   replays an earlier run's row without `resend`.
6. **It is logged first**: the decision goes to the personal `fence.log` before the request is sent, and an allow
   that can't be logged is denied, as in the lab.

**The account's own availability, a write without a conversation.** Rules 3 and 4 hold for every write but this
one and the call writes [Calls](#calls) gives targets of their own (an invitation addressed to the account, a
registered call, the test call); a call's start and join are held to them, as that section says. Task 19 gives the
availability a scope of its own, "the account itself", which `Fence::judge` permits without a member read only for
the fence's own account; in the personal build it is permitted only for the fence's own account (its `accounts.toml` entry), only on
its own Trouter endpoint (the one the push channel registered this run, any other id `Unknown`), only with the
availability values a capture showed, and only while the presence setting is on (rule 5). Rules 1, 2, 5 and 6 hold
for it as for every write. A status note, out-of-office or a profile field is not part of it: those stay
unmapped and denied.

**Choice A: membership, not roster completeness.** The lab fence needs a complete member list, because it must
prove nobody outside the lab is there. The personal fence asks only whether the account is in the conversation, so
one thread read, unpaged and in a captured shape, that lists the account answers it; the lab's paired read (the
thread and its member list agreeing, `totalMemberCount` equal to the list, equal `eTag`s) isn't required. A read that
fails, is in an unknown shape, is paged or truncated (rule 3), or doesn't list the account is denied. What this
gives up: the thread read carries no member count, so a member the service leaves out of it is neither seen nor
checked (it can't make the write `Unknown` under Choice B), and a group whose read pages stays read-only until its
paging form is captured. The other option, a complete roster as in the lab's group rule (the paired read, every
check of it), sees a member the thread read leaves out whenever the count disagrees, at the cost of a second read
per write and every group whose two reads disagree staying read-only. Task 27's tests are written for whichever is
approved.

**Choice B: outsiders of another kind.** A member whose MRI is in a form the lab never captured (a guest, a
federated user of another tenant, a consumer account) makes the read's shape unknown, and the write is denied, as
`Field::Unknown` is never guessed. That leaves chats with external people read-only until a shape is seen; the
alternative is to accept any MRI form as "a member", which a capture can't back. It covers the members the read
lists: under Choice A's membership read that is every member the service puts in an unpaged thread read, not a
member it leaves out (above); under the complete-roster option, every member the paired read counts. Bots (`28:`
members) already have a captured form: a message to a chat with a bot in it passes like any other, and bot invokes
and Adaptive Card actions stay unmapped and denied (`docs/backlog/bot-invokes-and-message-extensions.md`).

### Its reads

- **`GET`s** on the transport's host allowlist, as the lab sends them (the chat service, Graph's two probed shapes,
  CSA's team reads, and with no token a reaction's animation on Teams' CDN, `Service::Emoticons`); a new host or shape
  is added under the same review rule as in the lab.
- **Own-state and frames**: the same sealed `OwnState` and `OwnFrame` lists, in their captured shapes; each logged to
  the personal `fence.log` before it goes. No entry is added for the personal build alone.
- **`PostRead`**, the profile lookups and `getpresence`, in their captured shapes, for **anyone in the account's
  chats**: every MRI it names is the account or a member the store holds for one of the account's conversations
  (none twice, never a service's `28:` id, never empty), checked when the fence decides. The store's member
  lists come from the account's own thread reads, so this names nobody the account doesn't already chat with; the
  lab's limit (lab accounts only) stays for lab builds.
- **Never a search or a suggestion list** in M2, in either build: people search, message search (Substrate) and
  suggestions stay unmapped. Search is local, over the store.

### What the lab can't show, and what the personal build does without it

| | lab | personal build in M2 |
|---|---|---|
| channel writes | denied (no lab-only team) | read-only, no composer (the user's decision) |
| starting a chat | denied (a native group create isn't enrolled; a 1:1 needs people search) | not offered; `ChatCreate` denied by kind |
| server search | denied (it lists other tenant users) | local search only |
| meeting chats | none in the lab | read shown, writes denied until a capture shows the thread's shape |
| chats with guests or other tenants | none in the lab | read shown, writes denied until a capture shows the member form (Choice B) |
| images and files | as Tasks 21–23 end | the same: shown or a placeholder with a web link |
| calls | M3 (lab-only calls) | until M3 Task 22: incoming calls ignored, never answered, the user's other clients still ring; then [Calls](#calls) |
| own presence | Tasks 18–20 | the same writes and policy, behind the presence setting |
| status note, out-of-office, profile | never | never in M2 |

The personal build can't capture anything for the lab: agents never see its traffic, and its `fence.log` is the
user's. When a deny in daily use names a shape the lab never saw, the user reports its reason (the kind and the
shape problem, not the chat), and a lab capture settles it.

### The settings that write on their own

Typing, read receipts and presence are the writes that happen without a per-item action. On the first run the app
shows the three with Teams' own defaults pre-selected (typing on, read receipts on, presence following the app) and
**sends none of them until the user confirms that screen**; afterwards each is a setting in `settings.toml`. A read
receipt the native client sends moves the service's horizon but not the web client's "last read" mark (M1), so the
user's other clients may open a chat at an older mark.

## Calls

The personal build makes, answers, declines and joins 1:1 and group-chat calls with the user's colleagues, audio and
video. Every call write is first mapped in the lab from a capture (M3), as a sealed type in `teams-proto`; the
personal build sends the same sealed writes, under its own rule. Meetings, screen and content sharing, transfer,
forwarding, replacement, call park, PSTN and call waiting are not part of it.

### The writes, and to whom

| write | when it may go | its target |
|---|---|---|
| start | on the user's action in that chat ("Call") | an existing 1:1 or group chat whose one fresh, unpaged thread read in a captured shape lists the account, and every callee the start names |
| join | on the user's action on that chat's running call ("Join") | the same kind of chat, read the same way, and a captured answer or delivery binding the running call to that chat |
| answer | on the user's explicit action on that still-offered invitation (its banner or notification) | any readable invitation addressed to the account, whoever it is from |
| decline | the same | the same; the captured rejection, so the ringing stops everywhere and the caller sees it declined (a dismissed banner is no decline) |
| the user's own writes in a call (mute, camera, hang up) | on the user's action | a registered call (below) |
| the call's own writes (acknowledgements, channel parameters, renegotiation, keep-alives, `leave` on a failure) | without a further action | a registered call, as that call's captured setup, keep-alive and end writes |
| the ring-time sends | without a click | an offered invitation, as the captured sends the callee's client makes by itself while it rings, bound to it; never one that answers, declines, joins or adds |
| the test call: the chat with the bot, the start, the bot's `add` | on the user's action ("Test call") | the echo service only (below) |
| any other `add`, a new chat, a transfer, a forward | never | |

What each row holds, beside its own condition:

- **Every call write** is a sealed type in a captured shape, sent as `Authorized<_>` through the outbox with a fresh
  `Fence::check` for its exact target, logged to the personal `fence.log` before it goes (an allow that can't be
  logged is denied), never resent after an unknown outcome (a call controller acting twice is worse than a failed
  step) and never sent by a later run.
- **A start is tied to its chat by what it carries**: the chat the start names, or, for a 1:1 start that names only
  its callee, the two accounts' 1:1 chat as the chat service names it. The fence reads that chat, never one taken
  from the engine's or the view's context. Rules 3 and 4 above hold for it: no channel, meeting chat, `48:` stream,
  paged answer or member form no capture showed.
- **A registered call** is one the user started, joined or answered in this run, which the fence permitted and
  registered (the call registry the lab's fence uses, keyed as the captures bind the start's answer, the acceptance and
  the deliveries). Every follow-up on an entered call, whether requested by the user or generated by the call's
  machinery, names its registered call by its conversation, call and service as the captures bind them, never by a bare
  id; only an operation a capture showed; and only until the call ends, its captured end exchange included. A reconnect
  keeps a live call's registration and never restores an ended call.
- **An offered invitation** is a readable invitation addressed to the account, arriving while no call is up and no
  other invitation rings. The engine offers it (`Event::IncomingCall`) and the app shows it: a ringtone, a
  notification with Answer and Decline, a banner in the window, the caller's name as the invitation gives it (no
  profile lookup for a caller outside the account's chats). It stops being offered when its ring ends: answered,
  declined, cancelled, answered elsewhere (the user's phone, say) or timed out. Nothing answers or declines it by
  itself: not a delivery, a timer, a setting, a reconnect or an action on another invitation.
- **One call or ringing invitation at a time**: an invitation arriving during a call or another ring isn't offered,
  and nothing is sent for it (no busy decline); it rings on the user's other clients.
- **A call delivery response** (a Trouter frame answering a delivery) goes only as a captured, typed `OwnFrame` bound
  to the delivery it answers, under the same rules as the write it stands for: none for an invitation that isn't
  offered, and no acceptance before the user's action.

### The test call

A named exception for the echo service alone, the one place the personal build creates a chat, starts a call in no
chat and adds anyone. On the user's "Test call": the 1:1 chat of exactly the account and the echo bot, created in
its captured shape (`POST v1/threads`, `docs/protocol/calling-echo.md`); the start in the test call's captured shape
(`UserInitiatedTestCall`, no callee, no chat); and the `add` of exactly that bot to that call. The bot's MRI is a
constant in `teams-proto`, taken from the web bundle's `teamsEchoBotMri`, never from the network, a delivery or the
UI. Every other `add` and every other new chat stay denied.

### What else the calls bring

- **The calling stack's Trouter socket**: its registration (`SkypeSpacesWeb`) goes as an `OwnState` entry through
  `Transport::send_own`, and its frames are logged (`FrameSink::logging`), as the messaging socket's are. The
  personal build still sends no plain request: each read a call needs is a `GET`, an `OwnState` entry or a
  `PostRead`.
- **Media** (TURN, ICE, DTLS-SRTP) isn't classified, as in the lab: it carries a call the signaling fence permitted,
  on `teams-media`'s own sockets, relayed candidates only (no local address in an offer).
- **The microphone** opens only after the user's start, join or answer passed the fence. **The camera** is off at
  every start, join and answer, and only the user turns it on; it comes through the desktop portal, whose permission
  the desktop asks once. Which PipeWire the build opens (the desktop's) is fixed when it is built.
- **`fence.log`**: every call write and every call frame has its line, as chat writes do.

### What the fence can't see

- **The call's roster isn't read or watched.** The fence trusts the calls it permitted itself. A participant the
  service brings in (a tenant policy bot, forwarding on a callee's account, a colleague's client adding someone) isn't
  seen; for the personal build that is accepted, since the user is talking to whoever the call has, as in any client.
  It permits no write of its own beyond the rule above.
- **A caller in a form no capture showed** (a federated caller of another tenant, a consumer account) makes the
  invitation unreadable: it is ignored, not offered, until a lab capture shows that form (as Choice B treats member
  forms).

### The lab accounts as test callees

The user may call the lab accounts from the personal build: the user creates a 1:1 chat with each lab account once
from their own client, and the personal rule (a call in a chat that lists the account) covers those calls with no
exception of its own. No lab permission changes: the lab never calls, answers, declines or sends a ring-time send to
the user's account, no lab config names it, and the lab fence denies every contacting write in those chats. Such a
call rings on the lab account and is ignored, unless the user answers it on a device of their own signed in as that
account. The user places it when no lab live run holds the live lock.

### What the lab can't show

- **Colleagues' desktop and mobile clients**: the lab proves the native client against the web client in the armed
  browser, both directions; the desktop clients' own stacks only the user's checklist (M3 Task 23) meets: 1:1 audio
  and video with a colleague both ways, a group-chat call, a late join, a decline the colleague sees, a device switch
  mid-call, a call answered on the user's phone, a call across a token renewal.
- **Callers outside the account's chats and outside the tenant**: the checklist's incoming call from someone outside
  the user's chats covers the first; a caller of another tenant is met only if one calls, and until a capture shows
  its form, its invitation is ignored (above).
- **Real colleagues as callees and callers at all**: the policy is tested on `teams-mock` (M3 Task 22); agents never
  run the personal build, and the user reports a problem by its reason (the `fence.log` line's kind, never a chat id
  or a log).

### The CLAUDE.md text

CLAUDE.md's hard rules carry this section as the rule "The personal build's calls are the user's, in the account's
own chats", with three edits to the personal fence's rule (its kinds include the calls of the next rule; a call write
is a target beside the chats and the account's availability; the call's own sends are the exception to "only from an
explicit action"). The lab's side ("Calls go only to the lab, both ways") gained the decline, the join and the
ring-time sends in the same change. CLAUDE.md holds the current text; nothing here loosens it.

## Its sign-in

The lab signs in through `proto-lab auth` in the armed Chrome; the sign-in window (`teams-signin`) is offline-only,
because its WebKit guard has three known gaps (`docs/backlog/webkit-sign-in-guard-redirects.md`): a synchronous
load's redirect and a kept-alive load's redirect after its page is gone are followed without asking it, and a later
web process whose extension didn't load (a process swap, a restarted process) sends its requests without asking it,
since arming is checked for the view's first page only. So it can't promise that every request is decided and logged
before it goes. The personal build has no lab profile, so it needs the window, under a policy for the user's own
sign-in:

- **A binary of its own, built only with the personal build**: `teams-signin-personal`, a second binary of
  `crates/teams`, so it is gated as `Fence::open_personal` is. The window, the guard and the egress code stay in
  `teams-signin`'s lib; the host gate below and the code that lifts `NOT_LIVE` sit behind a `personal` feature of
  `teams-signin` that only `crates/teams` enables (the same manifest test as `teams-fence/personal`). The lab's
  `teams-signin` binary keeps only `--lab`, `NOT_LIVE` and its loopback bound, and a normal build of it has no live
  mode compiled in; built in one cargo invocation with `teams`, it gets the feature unified in and refuses to run,
  as every lab binary does (below). `teams` starts `teams-signin-personal` from beside its own executable with the
  authorize request on stdin (as `teams_auth::signin` does now); it logs to the personal `fence.log` through the
  personal paths, never `LabLocations`. It opens on the user's desktop: the lab window's sandbox check
  (`cli::window_refusal`: `DISPLAY` unset, the UI sandbox's compositor) is replaced by `teams`' own refusals (no
  `test-support`, the install marker and the data-dir check, below), all of them before a window opens.
  `scripts/install-personal.sh` builds both binaries with one `cargo build --release -p teams` and installs them with
  the web process extension module beside them.
- **A bounded set of hosts**: Entra's sign-in host (`login.microsoftonline.com`), its static content hosts (the
  `aadcdn`/`logincdn` hosts the sign-in page loads from), and the federated identity provider's hosts the user
  names in the account's `accounts.toml` entry (`idp_hosts`, empty by default), `https` only. The network session's egress proxy (today
  `teams_signin::egress`'s black hole) becomes a host gate: it opens a `CONNECT` tunnel only to those hosts on 443,
  refuses and logs every other connection, and is the soup session's only way out, so the requests the guard can't
  see (the redirects and an unarmed web process's) reach no other host either. No Teams, Office or Graph host is on
  it: the window can only sign in.
- **The guard's decisions inside that bound**: navigations only to those hosts and the `nativeclient` redirect
  (caught and refused, as now); `GET`s to them; `POST`s to Entra's sign-in paths (the lab's `sign_in` rules plus the
  multi-factor and federation return paths, which the lab never showed and which no capture can review before the
  first personal sign-in) and to the identity provider's hosts. Every decision the guard sees is logged; the
  proxy's are too. The rest of Task 24's hardening stays: WebSocket, beacons, service workers, WebRTC, new windows
  and downloads off, an ephemeral session.
- **What it promises, said plainly**: a *host* bound for every load of the soup session (every HTTP(S) connection
  WebKit makes goes through the gate), and a method and path bound only as far as the guard sees requests: not for
  the two redirect gaps, and not at all for a later web process whose extension didn't load. Outside the soup
  session nothing is bound, as `teams_signin::egress` says of the black hole today: DNS lookups, and networking by
  the WebKit features switched off (`window::FEATURES_OFF`) and WebRTC, which stay off. A request inside the window
  the guard doesn't see reaches nothing but the identity hosts. That is accepted for the user's own sign-in because
  nothing the window can reach is a Teams service: whatever a sign-in page sends there changes only the account's
  own sign-in state.
- **Multi-factor prompts and federated sign-in** (the lab showed neither): the user completes them in the window
  (Authenticator number matching, a code by text or call, the identity provider's own form). What WebKitGTK can't
  do is not worked around: a passkey or security key (WebAuthn), a client certificate or a device-bound broker
  (Windows Hello, Intune's company portal) fails in the window, and the app says so. Registering a new method
  (`mysignins`, `aka.ms/mfasetup`) isn't on the host list: the user does that in a normal browser.
- **Never a workaround of Conditional Access**: no other client id, no borrowed user agent or device claim, no
  scraped browser cookie. The client is the native `1fec8e78-…` with the `nativeclient` redirect (`auth.md`), as in
  the lab.

**Choice C: the sign-in route.** Why the window can't simply allow any host: the host gate is the only complete
bound over the guard's gaps. Whatever the guard misses (a redirect it never sees, a web process that never asked
it) is stopped only by where the gate lets a connection go. With any host allowed, the window, holding the user's
fresh Entra session, could load the Teams web app (a redirect, a link on a sign-in page, a navigation in an unarmed
process) and that app would run with the user's session and no fence at all: messages, read receipts, presence,
anything it sends. An allowlist of the identity hosts rules that out by construction.

The options:

- **The host-bounded window above** (the proposal): Entra's sign-in hosts, their static content hosts and the
  identity provider's hosts named in the account's `accounts.toml` entry, nothing else. Its three gaps (two redirects, an unarmed web
  process) are accepted knowingly for the user's own sign-in, because they can reach no Teams host. Its cost: a
  sign-in page that loads from a host not on the list (an identity provider's CDN, a captcha) fails until the user
  adds that host.
- **A denylist instead, a weaker option the user may pick**: any host except Microsoft's service domains (Teams,
  Office, Graph, SharePoint, Skype and the like). It signs in through any identity provider or page without naming
  its hosts first. It is weaker because it is only as complete as the list: a Microsoft service host or a newly
  added domain missing from it is reachable with the user's session, unfenced, and nothing notices; and every host
  a sign-in page names, anyone's, can be sent whatever the gaps let through.
- **Wait for a complete WebKit guard** (`webkit-sign-in-guard-redirects.md`'s inspecting egress guard or a WebKit
  change), which keeps the personal build signed out until then.
- **The system browser with a loopback redirect**, which the native client's registration isn't known to accept and
  which would run the user's own browser profile unfenced.
- **The device code flow**, never probed and often blocked by Conditional Access.

## The first run, and Conditional Access

`auth.md` question 6 (will the user's account face stricter Conditional Access than the lab's) can only be answered
by the user's first sign-in. The lab accounts got tokens for every audience with a password alone, no device in the
token and no Conditional Access block (`docs/spikes/ca-probe.md`, M0 Task 14); the user's account may not.

The first run, for the user (agents never do any of it):

1. Run `scripts/install-personal.sh` (a release build installed under `~/.local/`, with `packaging/teams.desktop`).
   Know that the tenant's sign-in logs show a sign-in of the Teams desktop client from Linux.
2. Start `teams`, type the work UPN, sign in through the window, completing any multi-factor or identity-provider
   prompt.
3. If Entra refuses, the window shows its `AADSTS` code and the app stores nothing: `53000`/`53001` (a compliant or
   joined device is required), `53003` (blocked by a Conditional Access policy), `50076`/`50079` (multi-factor
   required or to be registered), `50158` (an external challenge), `530003` (a managed device is required). A device
   requirement can't be met by this client: stop there, and the personal build isn't usable on this tenant.
4. After the sign-in: the app shows the account it stored (UPN, and that the tenant matched). Check the chat list
   and read a few chats back; check `~/.local/share/teams/fence.log` holds reads and own-state only so far.
5. Review the settings screen (typing, read receipts, presence) and confirm it; nothing of them is sent before.
6. Send a first message to a colleague told beforehand (a self-chat is a `48:` stream, not a writable target in
   M2), and check it arrived, in another client, once.
7. Expect re-sign-in: a sign-in frequency policy, a password change or a revoked session ends the refresh token
   (`invalid_grant`); the app then shows "signed out", sends nothing, and reopens the window.
8. Report anything odd (a deny's reason, an `AADSTS` code) before relying on it, never a token, a chat id or a log
   file. The answers to question 6 go into `auth.md` in our own words.

## What "agents never build a personal build" means once the code exists

- **Allowed**: the gate compiles the `teams` crate (`cargo clippy -p teams --all-targets --all-features`,
  `cargo clippy --workspace --all-targets --all-features`, `cargo test --workspace`) and runs its tests on `teams-mock` (`cargo test -p teams`). `crates/teams` dev-depends
  on `teams-fence` with `test-support`, as `teams-app` and `teams-headless` do, so every build that compiles its
  tests builds its two binaries with `test-support`, and they refuse to run; a manifest test checks that
  dev-dependency.
- **Not allowed**: producing a `teams` or `teams-signin-personal` binary without `test-support`, in any profile
  (a plain `cargo build` or `cargo build --release` at the workspace root, `cargo build -p teams`,
  `cargo run -p teams`, `cargo build --workspace`, `cargo install`, `--release` or not: a debug build reaches the
  tenant as well as a release one); running `scripts/install-personal.sh`; starting either binary.
- **Never touched by an agent**: the `teams-personal` Secret Service items, `~/.config/teams/`,
  `~/.local/share/teams/` (the store, `fence.log`, the diagnostic log, settings), the user's sign-in. Tests of the
  personal build use temporary dirs and `MemorySecretStore`; a source test checks that only `crates/teams`' two
  `main`s name the `teams-personal` application.

The rule doesn't rest on conduct alone. Two mechanisms make a runnable personal binary hard to produce by accident:

- **`default-members`** in the root `Cargo.toml` lists every member but `crates/teams`, so a plain `cargo build`,
  `cargo build --release` or `cargo run` at the root skips it; only `-p teams`, `--workspace` or the gate's
  commands reach it, and the gate's leave only `test-support` binaries.
- **An install marker**: `teams` and `teams-signin-personal` refuse to run unless they were compiled with an
  environment variable only `scripts/install-personal.sh` sets (read with `option_env!`), checked before anything
  else, the window included. A `cargo build -p teams` gives a binary that refuses to start.

A manifest test pins both (`crates/teams` absent from `default-members`, the marker read in both `main`s, the
script the only place that sets it). Neither stops an agent who sets the variable on purpose: that stays forbidden
by the rule above, as it is today for the lab's `test-support` refusal. After the user's first run the
`teams-personal` items sit on the session bus agents share, so a started binary would act as the user's account;
the marker is what keeps an accidental build from starting.

## How the lab and personal binaries are kept apart

- **Data dirs**: each refuses the other's. `teams` refuses a personal data dir that is, contains or lies inside the
  lab's data dir (the default and `lab.toml`'s `data_dir`, when `lab.toml` exists), and every lab binary refuses a
  lab data dir that is, contains or lies inside `$XDG_DATA_HOME/teams/`. The lab-side check sits where every lab
  path is made, `LabPaths`' construction: both `LabConfig`'s validation and `LabLocations::lab_paths`' fallback when
  `lab.toml` is missing (`LabPaths::new` on the default data dir, unvalidated today) go through it, so
  `Fence::open_lab`, `teams-app`, `teams-headless`, `proto-lab` (its directory-only commands included) and
  `teams-signin --lab` all get it before a path reaches a writer. Paths are compared after resolving symlinks.
- **Keyring names**: `teams` opens only `teams-personal`; the lab's broker already refuses a client naming another
  application (`teams_auth::broker`), and `teams-headless` and `proto-lab` name only `teams-lab`. Neither reads,
  writes or deletes the other's items.
- **Accounts**: `teams` refuses to sign in, or to keep, an account `lab.toml` names (its UPNs and MRIs), so a lab
  account is never driven by the personal fence; the lab binaries know only `lab.toml`'s accounts.
- **`test-support`**: both refuse a build with it (`teams_fence::test_support_enabled`), as every lab binary does
  today.
- **The sign-in windows**: the lab's `teams-signin` has only `--lab`, keeps `NOT_LIVE`, its loopback bound and its
  sandbox check (`cli::window_refusal`), and is a lab binary under every rule here; the personal window is
  `teams-signin-personal`, a binary of `crates/teams`, with `teams`' refusals in place of the sandbox check. The host
  gate and the lifted `NOT_LIVE` exist only under `teams-signin/personal`.
- **The `personal` feature**: only `crates/teams/Cargo.toml` enables `teams-fence/personal` and
  `teams-signin/personal` (a manifest test over every workspace manifest). Feature unification would bring them into
  a lab binary built in the same cargo invocation as `teams` (`--workspace`, `--all-features`), so every lab binary,
  `teams-signin` included, refuses to run when `teams_fence::personal_enabled()` is true, beside its `test-support`
  refusal; the lab's normal build (`cargo build -p proto-lab -p teams-headless -p teams-app --target-dir
  target/lab`) never includes `teams`. A manifest test over `cargo metadata`/`cargo tree -p teams-app -e features`
  checks that building `teams-app` alone resolves `teams-fence` and `teams-signin` without it.
- **The fence's constructors**: `Fence::open_personal` exists only with `personal`; a `fence-seal` case shows it
  doesn't compile without the feature, a source test that no crate but `teams` calls it, and `teams` never calls
  `Fence::open_lab`.

## The CLAUDE.md text it needs (proposal)

CLAUDE.md's hard rules name the lab today ("only within the 3 lab accounts", "Lab binaries get a `WritePermit` only
from `teams-fence`", "Agents never build or run a personal build"). The proposal keeps every lab rule as it is, names
which hard rules are the lab's alone and which hold for both builds, and adds two rules for the personal build.

Which hard rules are which, after the change:

- **The lab's alone** (they govern the lab and every lab binary, never the personal build): "Writes go only through
  the fence, and only within the 3 lab accounts", "One group chat counts as read in full: the lab's own", "A denied
  request may be allowed only if nothing it does or names leaves the lab", and "Nothing runs live against the
  tenant before the browser fence is armed".
- **Both builds**: "An allowance matches the request's exact captured shape, query included" (the personal build
  adds no entry of its own to the sealed lists, and any entry for both builds is reviewed in its exact shape), "The
  native client sends only what its sealed policy allows" (its transport limits included: no raw request or frame,
  no retry the transport didn't decide, the fixed host allowlist, `https` only, no proxy, no redirects), "Captures
  and tokens never enter the repo, not even sanitized" (tokens never reach logs or stdout: the diagnostic log
  redacts credentials in both builds), the agents rule as edited below, and "Protocol notes are written in our own
  words".
- **The personal build's alone**: the two new rules below.

The text to add, after the rule "The native client sends only what its sealed policy allows":

> - **The personal build (`teams`) has a fence of its own, and only it.** The user set this on <date of approval>.
>   It is the user's own account on the user's desktop: the `teams` and `teams-signin-personal` binaries, its data
>   under `~/.local/share/teams/` and `~/.config/teams/`, its Secret Service items under `teams-personal`, no broker,
>   no sandbox. Its fence is `teams-fence`'s personal policy, behind the `personal` feature only `crates/teams`
>   enables, opened only by `Fence::open_personal`. A write goes out only as a sealed `Mutating` type sent as
>   `Authorized<_>` through the outbox, with a `WritePermit` from `Fence::check` for its exact target; only the
>   kinds `docs/personal-build.md` lists; only to a 1:1 or group chat whose thread, read fresh when the fence
>   decides, unpaged and in a shape a capture showed, lists the account as a member (never a channel, a meeting
>   chat, a `48:` stream, a new chat, a paged answer or an unknown shape), or, the one exception, the account's own
>   availability on its own registered endpoint in a captured value (Tasks 18–19); only from an explicit action in
>   the UI or a setting the user turned on; logged to its `fence.log` before it goes. Its reads are the lab's
>   sealed lists, and `PostRead` may name anyone the account's stored chats list, never a search or a suggestion
>   list. Its sign-in window reaches only Entra's sign-in hosts and the identity provider's hosts `account.toml`
>   names, through its egress gate. The rules that hold for both builds hold for it; nothing in it changes a lab
>   rule: every lab binary keeps the lab fence, refuses the personal data dir, keyring items and accounts, and
>   refuses to run with `personal` or `test-support` built in; `teams` refuses the lab's.
> - **Agents never produce, install or run a runnable personal binary, and never touch its data.** Agents may
>   compile the `teams` crate in the gate and test it on `teams-mock`; any `teams` or `teams-signin-personal`
>   binary they leave is a `test-support` one, which refuses to run. They never build either without
>   `test-support` (debug or release, a plain `cargo build` at the root included), never set the install marker,
>   never run `scripts/install-personal.sh` or either binary, and never read or write `~/.local/share/teams/` (its
>   diagnostic log included), `~/.config/teams/` or the `teams-personal` items. They never receive the user's
>   tokens, chat ids or logs; the user reports a problem by its reason.

And these edits to rules that exist:

- "Writes go only through the fence, and only within the 3 lab accounts." gains, at its end: "This rule, the
  group-chat rule, the denied-request rule and the browser-fence rule govern the lab and every lab binary; the
  personal build's writes are governed by its own rule."
- "Agents never build or run a personal build and never receive the user's tokens." becomes "Lab binaries get a
  `WritePermit` only from `teams-fence`'s lab policy." with the rest of that rule's text unchanged (the personal
  side moves to the new rule above).
- "The native client sends only what its sealed policy allows (M1 Tasks 5, 15), as the browser fence does for the
  browser." gains: "This holds for the personal build too; only its fence's policy differs."
- "Captures and tokens never enter the repo, not even sanitized." gains: "This holds for the personal build too;
  it makes no captures."
- "Live runs" in "Patterns learned" gains: "The personal build is never a live run of an agent's; the live lock is
  the lab's."

The diagnostic log's own change to CLAUDE.md (the M1 pattern "No log line carries a payload" amended for `debug`)
comes with the plan's diagnostic log task, not with this text.

## What the user is asked to decide

Approve this document (and the CLAUDE.md text above), or amend it. The choices that matter most:

1. **The personal fence's question** (Choice A): a write passes when one fresh, unpaged thread read of its chat
   lists the account (membership), or only when the whole roster is read and agrees as in the lab's group rule.
   Either way a paged or truncated read is denied, so a group whose read pages stays read-only until a lab capture
   shows the paging form.
2. **Unknown member forms** (Choice B): chats with guests or people of another tenant stay read-only until a lab
   capture shows their member form; under the membership read this covers the members the read lists, not one the
   service leaves out of it.
3. **The sign-in route** (Choice C): the WebKit window with a host gate (Entra and the user's identity provider
   only), its three known gaps (two redirects, an unarmed later web process) accepted knowingly for the user's own
   sign-in because they can reach no Teams host; or the weaker denylist (any host but Microsoft's service domains),
   which is only as safe as its list; or wait for a complete guard. Any host at all isn't offered: it would let the
   window load the Teams web app with the user's session, unfenced.
4. **`PostRead` for anyone in the account's stored chats**, the account's own availability as the one write without
   a conversation, and the settings that write on their own (typing, read receipts, presence) sent only after the
   first-run screen is confirmed.
5. **What agents may do**: compile and mock-test only; no runnable personal binary in any profile (a plain
   `cargo build` included, kept out by `default-members` and the install marker), no install, no run, no access to
   its data, keyring items or logs, the diagnostic log included.

Task 27 waited on this decision; the user approved it on 2026-10-23.

## How the implementation reads it

Written after the approval and the lab fence's rewrite, by Task 27:

- **Reads.** The lab's native client now sends its reads by `POST` and its registration as plain requests the
  classifier lets through; the personal build sends **no plain request** (`Refusal::Personal`): `GET`s, the
  registration as its sealed own-state entry, the profile lookup and the presence read only as `PostRead` under a
  `Fence::check_post_read_among` permit naming the account or people of its stored chats (`ChatPeople`, refreshed by
  the engine from its store before a lookup starts), and its Trouter frames logged by kind (`FrameSink::logging`).
- **Typing and read receipts.** The lab fence permits them on a chat without a member read; the personal fence keeps
  rule 3 for them as for every write: the chat's thread is read and must list the account.
- **Membership.** The personal source's answer is `Membership::Listed` (one unpaged thread read in a captured shape,
  every member `8:orgid:<guid>` or `28:<guid>`, a group chat's `threadType` `chat`, a 1:1 chat's members exactly the
  two its id names); the lab fence denies a listing, and the personal fence denies anything but one.
- **`accounts.toml`.** Before the first sign-in, `teams` opens a window of its own asking for the work account
  (`teams_ui::account_prompt`); "Sign in" adds the UPN there (the user may add `idp_hosts`) and opens the sign-in
  window (`teams-signin-personal --account <UPN>`), which adds the object id and tenant; that account's fence opens
  only once both are there, and a sign-in that goes through starts the app afresh. A UPN typed that is one already is
  that account; another is a new account, taken out again when its sign-in stores nothing. The rail's "+" opens the
  same window to add an account while the app runs. An account a sign-in named that can't start without one signs in
  again once the others run (or on that window, with none running). `teams --account <UPN>` skips the window and goes
  to the sign-in window at once, under the same rule.
- **Removing an account** is "Sign out and remove" in its account menu, or `teams --forget <UPN>` (its
  `teams-personal` items, its `accounts.toml` entry, its region; the store stays, to delete by hand). The flag is
  refused while the account's engine runs
  (it takes the account's engine lock first and holds it until everything is deleted), so a running app's session
  can't keep sending or store a renewed token again. Each account has a store of its own (`store/<oid>.sqlite`), so a
  store kept after a forget is never opened for another account.
- **The display name** a post carries (`imdisplayname`) is the ID token's `name`, kept in the account's
  `accounts.toml` entry by every sign-in that gives one; an entry without one falls back to the UPN's local part.
- **The lab's credential broker** (`teams-app secrets`) builds no fence and refuses neither `test-support` nor
  `personal`, as before; the app itself refuses both.
- **CLAUDE.md** carries the approved text without its date (CLAUDE.md holds current rules only), its references to
  the lab rules that the rewrite renamed adjusted, and the reads sentence as above.

Written by M3 Task 22, for [Calls](#calls):

- **The fence** (`teams-fence`, `decision/personal_calls.rs`): a start (`WriteKind::Call` on the chat it carries) is
  permitted only on a 1:1 chat naming the account or a group chat, whose one fresh thread read (`Listed`) lists the
  account and every participant the start names, the account among them; the test call's start (`Scope::New`) only
  with exactly the account and `TEAMS_ECHO_BOT_MRI`; an `add` only of exactly that bot to a call the fence permitted
  for it; a join only of a running call an offered invitation bound to its group chat, from the account, that chat's
  fresh read listing the account; every other operation only as the call registry resolves it (a call this run's
  fence registered, or an invitation it offered, in a state open for it), with no roster and no further read. An
  offered invitation is any readable one addressed to the account whose caller isn't the account, offered only while
  no registered call is live and no other offered one is answered or rings with its end bound (a start still waiting
  for its answer doesn't count: the call machine takes no invitation meanwhile). Its receipt reads no chat. A start or
  a join pends in the registry only as the account's own (`Fence::pend_start`).
- **No chat is created for the test call**: the native test call needs none (its `add` names the 1:1 chat of the
  account and the bot, as the lab's native echo call does), so `ChatCreate` stays denied.
- **What a call sends besides its sealed writes and typed delivery responses**, as the lab's native calls showed
  (the `allowed` lines of M3 Tasks 15, 16 and 21 in the lab's `fence.log`, and `docs/protocol/endpoints.md`): the
  calling registration (`POST registrations`, `SkypeSpacesWeb`), which goes as its own-state entry
  (`calling_endpoint_registration`, `Transport::send_own`), and the TURN credentials (`GET trap/tokens`), a read. The
  web client's calling policy reads (`useraggregatesettings`, `effectivePolicies`) are the browser's, never the native
  client's: unneeded. The personal build sends no plain request for a call (`crates/teams/tests/calls.rs`).
- **The devices** are `teams-media`'s PipeWire backend as this build allows it: the desktop's, the camera through the
  desktop's camera portal; a build with `test-support` (every `cargo test` of `crates/teams`) only the UI sandbox's.
  Without them the app runs without calls (`teams::calling::open`). The first run's screen says what a call sends
  without a click (an incoming call told it rings here), which has no switch.
