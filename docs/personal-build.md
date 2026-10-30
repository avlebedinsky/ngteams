# The personal build: its rules

Status: **approved by the user on 2026-10-23, as written, all five choices included**; in force in CLAUDE.md's hard
rules, and implemented in M2 Task 27 (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`: `crates/ngteams`,
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

**The decision of 2026-10-27** (in the user's words: the personal build "should be able to do
anything actual MS Teams can do", with no limitations): the personal build is the user's Teams. Its fence no longer
limits what it does: it logs every send and denies none, whatever the kind and target of a write (a new chat, a
channel post, a meeting chat, any call, meetings, sharing, transfer, PSTN and apps included as Teams handles them),
whatever a read names, and whatever the classifier makes of a plain request. It reads no members. What stays is how
the app behaves (it sends what the user did in the UI or a setting the user turned on, and the follow-ups that needs;
one call at a time across its accounts, as a Teams client holds one), that every send is logged first and every write
goes through the outbox under its permit, and the rules that guard the repo, the user's credentials and the lab:
captures and tokens never in the repo or a log, sign-in only in `teams-auth`'s clients, the keep-apart checks and
features, no write from a `cargo test` process, the lab's rules and fence unchanged. Agents may read the personal
build's data dirs (the user's decision of 2026-10-26) and run it themselves only in a read-only mode the app enforces
(the same day's decision; the mode is `ngteams --read-only`, [What agents may do with the personal
build](#what-agents-may-do-with-the-personal-build)), never its keyring items or tokens. The sections below say the
current rule; where an older paragraph here (the 2026-10-22 proposal, the 2026-10-24 call rules, Choices A–C) describes
a limit, it is the record of what was decided then, and this decision supersedes it. Matching Teams' own retry of a
failed send is a follow-up (`docs/backlog/personal-build-retry-like-teams.md`): the outbox keeps its rules meanwhile.

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
design. Its fence asks no question at all: it is the user's Teams, used by the user in the user's own window, so it
may do whatever the Teams client lets its user do, and its fence is an audit log (every send logged before it goes)
that denies nothing. What the lab's rules hold that isn't about whom a request reaches stays: sealed request types,
permits only from `teams-fence`, every write through the outbox, no retry the outbox didn't decide.

What it does not change: the lab, its fence and its hard rules. A lab binary keeps the lab fence, and nothing in the
personal build widens what a lab account may do.

## The binary and its data

| | lab (`teams-app`, `teams-headless`, `proto-lab`) | personal (`ngteams`) |
|---|---|---|
| crate | `crates/teams-app` | `crates/ngteams` (a lib beside `main.rs`, as `teams-headless` and `teams-app` have) |
| UI | `teams-ui` | the same `teams-ui` |
| account | one or more lab aliases per process (`--lab`), from `lab.toml` | the user's own work accounts, each one the sign-in window signs in ([Several accounts](#several-accounts)) |
| config | `~/.config/teams-lab/lab.toml` | `~/.config/ngteams/accounts.toml` (written by each first sign-in, below) |
| data dir | `$XDG_DATA_HOME/teams-lab/` (or `lab.toml`'s `data_dir`) | `$XDG_DATA_HOME/ngteams/` (`~/.local/share/ngteams/`), `0700` |
| store | `<lab data>/store/<alias>.sqlite` | `~/.local/share/ngteams/store/<oid>.sqlite` (`0600`, as `teams-store` makes it) |
| picture cache | `<lab data>/store/<alias>.sqlite-cache/pictures.sqlite` | `~/.local/share/ngteams/store/<oid>.sqlite-cache/pictures.sqlite` (`0600`, its dir `0700`; the pictures messages show, up to 512 MB; a snapshot leaves it out) |
| fence log | `<lab data>/fence.log` | `~/.local/share/ngteams/fence.log` (`0600`) |
| region (no token) | `<lab data>/auth/<alias>.json` | `~/.local/share/ngteams/regions/<oid>.json` |
| UI settings | `<lab data>/ui/<alias>.toml` | `~/.local/share/ngteams/settings.toml` |
| diagnostic log | under the lab's data dir | under `~/.local/share/ngteams/` (`0600`, rotated; below) |
| Secret Service application | `teams-lab` (`LabPaths::KEYRING_APPLICATION`) | `ngteams` |
| credentials | through the broker (`teams-app secrets`) | `KeyringSecretStore::new("ngteams")` in-process: **no broker** |
| window | only in the UI sandbox (`UI_SANDBOX_DIR`), `DISPLAY` unset | the user's desktop session: **no sandbox** |
| bus | the sandbox's private bus | the user's session bus (notifications, tray, Secret Service) |
| sign-in | `proto-lab auth` (the armed Chrome fence) | its own sign-in window, a window of `ngteams` itself (below) |
| live lock | `~/.local/share/teams-lab/live.lock` around every live command | none: it is the user's own app |
| calls | the UI sandbox's private PipeWire and fake camera portal; the lab's call rule (a call judged by its entry); the echo service from `lab.toml` | the desktop's PipeWire and camera portal (fixed when built); no call rule: the fence logs every call send and denies none ([Calls](#calls)); the echo service from the web bundle (`teams_proto::TEAMS_ECHO_BOT_MRI`) |

`accounts.toml` holds, for each account, what its first sign-in learned, never a token: the UPN the user typed, and
the ID token's `oid` and `tid`. Every later sign-in must give the same `oid` and `tid` (the ID token's `preferred_username` checked as
`teams_auth::token::TokenClient::exchange_code` checks it today, then the pair compared), so a sign-in that lands in
another account or another tenant stores nothing and says so. Removing an account is an explicit "Sign out and
remove" in the app (or `ngteams --forget <UPN>`), which deletes its `ngteams` items, its `accounts.toml` entry and
its region; its store stays, to delete by hand.

The engine is built as `teams_core::lab::start` builds it (the `Session`, the transport, the push channel, the
outbox), from the account's `accounts.toml` entry and the region the authz exchange gives instead of `lab.toml`; Task 27 factors the
shared part out of `lab.rs` rather than copying it. The engine lock (`Store::engine_lock`) keeps one `ngteams` per
account, as it keeps one lab engine per alias.

The app has a diagnostic log (the user's decision of 2026-10-22), in both builds: a file under the data dir, `0600`
and rotated. At `info` it carries no payload, as every log line does today. At `debug` it carries full HTTP requests
and responses and Trouter frames (bodies, headers, message text), with credentials redacted by the logger itself:
CLAUDE.md's token rule is unchanged (no token, cookie or code reaches any log or stdout). For the personal build
that log holds the user's colleagues' messages: it is the user's data, like the store and `fence.log`. Agents may read
it (the user's decision of 2026-10-26), and it never goes into the repo, not even sanitized.

## Several accounts

The user may add several of their own work accounts (the user's decision of 2026-10-27), all of them running at once
in one window: the rail shows each account's avatar above Settings, a click switches to it at once, the "+" under the
avatars adds one, and an account's menu has "Sign out and remove". Every account notifies and rings whichever one the
window shows, its notifications naming it.

- **Each account is the user's own and stands alone.** It is in `accounts.toml` (`[[account]]` tables, in the order
  added; a single account's `account.toml` and `region.json` from before are moved there, and to
  `regions/<oid>.json`, at the next start), it has its own store (`store/<oid>.sqlite`), region, refresh token and
  skypetoken (`ngteams` items keyed by its UPN), its own engine (its own engine lock) and its own personal
  fence, `Fence::open_personal(paths, upn)`, which logs that account's sends (and denies none). Nothing of one account is used for another. A lab account is refused as before, by UPN or MRI.
- **Adding one** goes through the same window as the first sign-in (the account asked for, then Microsoft's sign-in
  window bound to that account's identity provider hosts); a sign-in that stores nothing leaves no account behind:
  an entry holding only its UPN is taken out again, and one the user wrote its `idp_hosts` into, or one whose window
  refused an identity provider's host, stays as a pending entry (never an account in the rail, never an engine) that
  the next sign-in of that UPN takes up. A refused host is named on the prompt (the host alone, never its URL) with
  where to add it: `idp_hosts` in that account's entry of `~/.config/ngteams/accounts.toml`, by hand.
  Once it goes through, the account's engine starts and its avatar shows; nothing else restarts.
- **Removing one** stops its engine first, then forgets it as `ngteams --forget <UPN>` does: its `ngteams` items,
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

In `teams-fence`, behind a cargo feature `personal` that only `crates/ngteams/Cargo.toml` enables. `Fence` keeps its
one type (the transport, the outbox and the push channel all take an `Arc<Fence>` today) and gains a policy inside
it: the lab's roster and group chat as now, or, only with `personal`, the personal account. The only way to a
personal policy is `Fence::open_personal(paths, upn)`, which exists only with the feature, reads that account from
the personal paths' `accounts.toml` and opens the personal `fence.log`; one per account. Permits, `Authorized<_>`, `PostReadPermit` and the sealed
`MembershipSource` stay where they are: nothing outside `teams-fence` mints one, in either build.

### What it permits

Everything, logged. The fence denies no write, read, request or frame:

1. **A write** goes through the outbox (durable rows) or its transient kind (typing, the account's own availability)
   as a sealed `Mutating` type sent as `Authorized<_>`, with the permit `Fence::check` gives for its exact `Target`,
   whatever its kind and target: a message, an edit, a delete, a reaction, typing and read receipts, a chat created,
   members changed, an invite, a channel post or reply, a meeting chat, any call write. No member read, no limit on
   whom it adds. The permit is the plumbing that ties a write to its log line; the outbox's rules, the retry table and
   "no resend across runs" are unchanged.
2. **Everything else** goes too: a `GET`, a plain request whatever `teams_fence::contacting` makes of it (one it calls
   `None` logged `allowed` as in the lab; one it calls a write or can't read logged `sent_unjudged` with that
   classification), an own-state request, a read by `POST` naming anyone (`Fence::check_post_read`, which permits
   every one through a personal fence), a frame and a call's delivery response.
3. **It comes from the user**: from an explicit action in the UI or a setting the user turned on, and the follow-ups
   that action needs (a call's setup, keep-alive and end writes; an offered invitation's ring-time sends), as Teams
   does. Nothing else is sent on a timer the user didn't enable, and nothing replays an earlier run's row without
   `resend`. This is how the app behaves, not a fence check.
4. **It is logged first**: every send has its line in the personal `fence.log` before it goes (method, host and path
   shape for a request, the target for a write, the kind for a frame; never a body, query, header or token), and one
   that can't be logged isn't sent.

**One call at a time** across the app's accounts (the call floor): a start, a join or an answer while another account
is in a call or rings is refused and logged, as a Teams client holds one active call.

### Its reads

Any read, by any method, whoever it names or its results list: a search, a suggestion list, a people lookup, a
presence read or subscription. The sealed `OwnState` and `PostRead` entries stay the shapes the native client builds
its own-state requests and reads by `POST` in (each logged by its kind); a new read is an ordinary change. Each token
still goes only to its own service's host, and none to a third party.

### What the lab can't show, and what the personal build does

| | lab | personal build |
|---|---|---|
| channel writes | denied (no lab-only team) | allowed by the fence; the app has no composer for them yet |
| starting a chat | allowed among lab accounts (`CreateChat`) | allowed by the fence; the app doesn't offer it yet |
| server search | a read, allowed | allowed; the title bar's search asks Substrate (`docs/protocol/search.md`), in the read-only mode too |
| meeting chats | none in the lab | allowed |
| chats with guests or other tenants | none in the lab | allowed |
| calls | lab-only calls | the user's calls, no fence limit ([Calls](#calls)) |
| own presence | the same writes and policy | the same, behind the presence setting |
| status note, out-of-office, profile | allowed in the lab browser; the native client sends none | allowed by the fence; the app sends none yet |

A shape the lab never captured may fail at the service rather than at the fence; the user reports it by its reason (the
`fence.log` line's kind), and a lab capture settles it.

### The settings that write on their own

Typing, read receipts and presence are the writes that happen without a per-item action. On the first run the app
shows the three with Teams' own defaults pre-selected (typing on, read receipts on, presence following the app) and
**sends none of them until the user confirms that screen**; afterwards each is a setting in `settings.toml`. A read
receipt the native client sends moves the service's horizon but not the web client's "last read" mark (M1), so the
user's other clients may open a chat at an older mark.

## Calls

The personal build makes, answers, declines and joins calls with the user's colleagues, audio and video, and the fence
limits none of them: meetings, screen and content sharing, transfer, forwarding, call park and PSTN are allowed as
Teams handles them, though the app builds only 1:1 and group-chat calls today. Every call write the app sends is a
sealed type in a captured shape (that is how it is built correctly, not a permission), sent as `Authorized<_>` through
the outbox under a fresh `Fence::check`, logged to the personal `fence.log` before it goes, never resent after an
unknown outcome (a call controller acting twice is worse than a failed step) and never sent by a later run.

What the app does, as Teams does:

- **A start or a join** goes on the user's action ("Call", "Join", "Make a test call"); the fence reads no chat for it.
- **Any readable invitation addressed to the account** is offered (rung, notified, shown with the name the invitation
  gives), whoever it is from, and answered or declined only by the user's action on it; a decline is the captured
  rejection, not a dismissed banner. It stops being offered when its ring ends.
- **One call or ringing invitation at a time** across the app's accounts (their fences share one call floor): another
  invitation isn't offered and rings on the user's other clients.
- **A call's own sends** (its setup, keep-alive and end writes, renegotiation, an offered invitation's ring-time sends,
  its typed delivery responses) go without a further action. The call registry keeps the bookkeeping the call machine
  reads its deliveries by and the floor holds the calls by; it decides nothing.
- **The test call** calls the echo service the web bundle names (`TEAMS_ECHO_BOT_MRI`).

### What else the calls bring

- **The calling stack's Trouter socket**: its registration (`SkypeSpacesWeb`) goes as an `OwnState` entry through
  `Transport::send_own`, and its frames are logged (`FrameSink::logging`), as the messaging socket's are.
- **Media** (TURN, ICE, DTLS-SRTP) isn't classified: it carries a call the user placed or took, on `teams-media`'s own
  sockets, relayed candidates only (no local address in an offer).
- **The microphone** opens only after the user's start, join or answer was taken. **The camera** is off at every
  start, join and answer, and only the user turns it on; it comes through the desktop portal, whose permission the
  desktop asks once. Which PipeWire the build opens (the desktop's) is fixed when it is built.
- **`fence.log`**: every call write and every call frame has its line, as chat writes do.

### The lab accounts as test callees

The user may call the lab accounts from the personal build. No lab permission changes: the lab never calls, answers,
declines or sends a ring-time send to the user's account, no lab config names it, and the lab fence denies every
contacting write in those chats. Such a call rings on the lab account and is ignored, unless the user answers it on a
device of their own signed in as that account. The user places it when no lab live run holds the live lock.

### What the lab can't show

- **Colleagues' desktop and mobile clients**: the lab proves the native client against the web client in the armed
  browser, both directions; the desktop clients' own stacks only the user's checklist (M3 Task 23) meets.
- **Real colleagues as callees and callers**: the calls are tested on `teams-mock`; agents run the personal build only
  in its read-only mode, which places and answers no call, and may read its `fence.log` and logs.

## Its sign-in

The lab signs in through `proto-lab auth` in the armed Chrome; the sign-in window (`teams-signin`) is offline-only,
because its WebKit guard (WPE WebKit's `send-request` in a web process extension) has three known gaps (`docs/backlog/webkit-sign-in-guard-redirects.md`): a synchronous
load's redirect and a kept-alive load's redirect after its page is gone are followed without asking it, and a later
web process whose extension didn't load (a process swap, a restarted process) sends its requests without asking it,
since arming is checked for the view's first page only. So it can't promise that every request is decided and logged
before it goes. The personal build has no lab profile, so it needs the window, under a policy for the user's own
sign-in:

- **A window of the personal build's own app**: `ngteams` opens the sign-in window itself, a GPUI window beside its
  others (`teams_signin::window::Windows`, asked from the sign-in running off GPUI's foreground), so it is gated as
  `Fence::open_personal` is. The page's engine is `ngteams` run again as a worker process (`teams_wpe::worker`, served
  right after `ngteams`' refusals), under `bwrap` with the vendored WPE WebKit overlaid where the system has none. The
  window, the guard and the egress code stay in `teams-signin`'s lib; the host gate below and the code that lifts
  `NOT_LIVE` sit behind a `personal` feature of `teams-signin` that only `crates/ngteams` enables (the same manifest
  test as `teams-fence/personal`). The lab's `teams-signin` binary keeps only `--lab`, `NOT_LIVE` and its loopback
  bound, and a normal build of it has no live mode compiled in; built in one cargo invocation with `ngteams`, it gets
  the feature unified in and refuses to run, as every lab binary does (below). The window takes the authorize request
  and hands back the redirect in process (`teams_auth::signin::sign_in_with`); it logs to the personal `fence.log`
  through the personal paths, never `LabLocations`. It opens on the user's desktop: the lab window's sandbox check
  (`cli::window_refusal`: `DISPLAY` unset, the UI sandbox's compositor) is replaced by `ngteams`' own refusals (no
  `test-support`, the install marker and the data-dir check, below), all of them before a window opens.
  `scripts/install-personal.sh` builds `ngteams` with `cargo build --release -p ngteams` and installs it with the web
  process extension module beside it.
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
  and downloads off, an ephemeral session. A window closed after it refused a navigation to a host the user could
  name in `idp_hosts` (`https`, a DNS name off the bound, none of Microsoft's service domains) answers that host
  alone (`{"refused": <host>}`), and the app names it with where to add it; nothing adds it by itself.
- **What it promises, said plainly**: a *host* bound for every load of the soup session (every HTTP(S) connection
  WebKit makes goes through the gate), and a method and path bound only as far as the guard sees requests: not for
  the two redirect gaps, and not at all for a later web process whose extension didn't load. Outside the soup
  session nothing is bound, as `teams_signin::egress` says of the black hole today: DNS lookups, and networking by
  the WebKit features switched off (`window::FEATURES_OFF`) and WebRTC, which stay off. A request inside the window
  the guard doesn't see reaches nothing but the identity hosts. That is accepted for the user's own sign-in because
  nothing the window can reach is a Teams service: whatever a sign-in page sends there changes only the account's
  own sign-in state.
- **Multi-factor prompts and federated sign-in** (the lab showed neither): the user completes them in the window
  (Authenticator number matching, a code by text or call, the identity provider's own form). What WPE WebKit can't
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

1. Run `scripts/install-personal.sh` (a release build installed under `~/.local/`, with `packaging/com.nickalie.ngteams.desktop` and its icon, `packaging/icons/`).
   Know that the tenant's sign-in logs show a sign-in of the Teams desktop client from Linux.
2. Start `ngteams`, type the work UPN, sign in through the window, completing any multi-factor or identity-provider
   prompt.
3. If Entra refuses, the window shows its `AADSTS` code and the app stores nothing: `53000`/`53001` (a compliant or
   joined device is required), `53003` (blocked by a Conditional Access policy), `50076`/`50079` (multi-factor
   required or to be registered), `50158` (an external challenge), `530003` (a managed device is required). A device
   requirement can't be met by this client: stop there, and the personal build isn't usable on this tenant.
4. After the sign-in: the app shows the account it stored (UPN, and that the tenant matched). Check the chat list
   and read a few chats back; check `~/.local/share/ngteams/fence.log` holds reads and own-state only so far.
5. Review the settings screen (typing, read receipts, presence) and confirm it; nothing of them is sent before.
6. Send a first message to a colleague told beforehand, and check it arrived, in another client, once.
7. Expect re-sign-in: a sign-in frequency policy, a password change or a revoked session ends the refresh token
   (`invalid_grant`); the app then shows "signed out", sends nothing, and reopens the window.
8. Report anything odd (a deny's reason, an `AADSTS` code) before relying on it, never a token, a chat id or a log
   file. The answers to question 6 go into `auth.md` in our own words.

## What agents may do with the personal build

- **Allowed**: the gate compiles the `ngteams` crate (`cargo clippy -p ngteams --all-targets --all-features`,
  `cargo clippy --workspace --all-targets --all-features`, `cargo test --workspace`) and runs its tests on `teams-mock` (`cargo test -p ngteams`). `crates/ngteams` dev-depends
  on `teams-fence` with `test-support`, as `teams-app` and `teams-headless` do, so every build that compiles its
  tests builds its two binaries with `test-support`, and they refuse to run; a manifest test checks that
  dev-dependency. Agents may read `~/.local/share/ngteams/` (the store, `fence.log`, the diagnostic log, settings) and
  `~/.config/ngteams/` at any time (the user's decision of 2026-10-26).
- **Read-only runs, on the agent's own initiative**: an agent may start the installed `ngteams` in its read-only mode,
  look at and screenshot its window, drive its UI to navigate, and read its logs and store while it runs. That mode
  sends nothing anyone else sees or that changes the account: no message, edit, delete, reaction, call (start, join,
  answer or decline), chat created, typing, read receipt, availability or status write, and no delivery response that
  answers a call; reads, the app's own token renewal and the push socket's own session frames go. The app enforces it
  in that mode, not the agent's care: a send attempted there is refused and logged. The mode is `ngteams --read-only`
  (`crates/ngteams/src/read_only.rs`), run by `scripts/run-personal-read-only.sh`: on a snapshot of the data dirs
  (`ngteams snapshot`), in a UI sandbox of its own, its tokens through a `get`-only broker outside it (`ngteams secrets`),
  its transports read-only (every write, call write and registration refused below the views and logged as
  `refused_read_only`), with no push channel (a catch-up by reads every minute) and no calls. The script refuses an
  installed binary without the mode.
- **Only on the user's request**, each time: running `scripts/install-personal.sh`, and starting the installed `ngteams`
  detached for the user, its output discarded, then leaving it alone.
- **Not allowed**: producing an `ngteams` binary without `test-support` other than through
  `scripts/install-personal.sh` on the user's request (a plain `cargo build` or `cargo build --release` at the
  workspace root, `cargo build -p ngteams`, `cargo run -p ngteams`, `cargo build --workspace`, `cargo install`); signing it
  in; driving, testing or capturing with it in its sending mode; reading or writing the `ngteams` Secret
  Service items; receiving the user's tokens. Tests of the personal build use temporary dirs and
  `MemorySecretStore`; a source test checks that only `crates/ngteams` names the `ngteams` application.

The rule doesn't rest on conduct alone. Two mechanisms make a runnable personal binary hard to produce by accident:

- **`default-members`** in the root `Cargo.toml` lists every member but `crates/ngteams`, so a plain `cargo build`,
  `cargo build --release` or `cargo run` at the root skips it; only `-p ngteams`, `--workspace` or the gate's
  commands reach it, and the gate's leave only `test-support` binaries.
- **An install marker**: `ngteams` refuses to run unless it was compiled with an
  environment variable only `scripts/install-personal.sh` sets (read with `option_env!`), checked before anything
  else, the window included. A `cargo build -p ngteams` gives a binary that refuses to start.

A manifest test pins both (`crates/ngteams` absent from `default-members`, the marker read in both `main`s, the
script the only place that sets it). Neither stops an agent who sets the variable on purpose: that stays forbidden
by the rule above, as it is today for the lab's `test-support` refusal. After the user's first run the
`ngteams` items sit on the session bus agents share, so a started binary would act as the user's account;
the marker is what keeps an accidental build from starting, and the read-only mode what keeps an agent's run from
sending.

## How the lab and personal binaries are kept apart

- **Data dirs**: each refuses the other's. `ngteams` refuses a personal data dir that is, contains or lies inside the
  lab's data dir (the default and `lab.toml`'s `data_dir`, when `lab.toml` exists), and every lab binary refuses a
  lab data dir that is, contains or lies inside `$XDG_DATA_HOME/ngteams/`. The lab-side check sits where every lab
  path is made, `LabPaths`' construction: both `LabConfig`'s validation and `LabLocations::lab_paths`' fallback when
  `lab.toml` is missing (`LabPaths::new` on the default data dir, unvalidated today) go through it, so
  `Fence::open_lab`, `teams-app`, `teams-headless`, `proto-lab` (its directory-only commands included) and
  `teams-signin --lab` all get it before a path reaches a writer. Paths are compared after resolving symlinks.
- **Keyring names**: the app opens only its own Secret Service application, `ngteams`; the lab's broker already
  refuses a client naming another application (`teams_auth::broker`), and `teams-headless` and `proto-lab` name only
  `teams-lab`. Neither reads, writes or deletes the other's items.
- **Accounts**: `ngteams` refuses to sign in, or to keep, an account `lab.toml` names (its UPNs and MRIs), so a lab
  account is never driven by the personal fence; the lab binaries know only `lab.toml`'s accounts.
- **`test-support`**: it refuses a build with it (`teams_fence::test_support_enabled`), as every lab binary does.
- **The sign-in windows**: the lab's `teams-signin` has only `--lab`, keeps `NOT_LIVE`, its loopback bound and its
  sandbox check (`cli::window_refusal`), and is a lab binary under every rule here; the personal window is a
  window of `ngteams` itself, with `ngteams`' refusals in place of the sandbox check. The host
  gate and the lifted `NOT_LIVE` exist only under `teams-signin/personal`.
- **The `personal` feature**: only `crates/ngteams/Cargo.toml` enables `teams-fence/personal` and
  `teams-signin/personal` (a manifest test over every workspace manifest). Feature unification would bring them into
  a lab binary built in the same cargo invocation as `ngteams` (`--workspace`, `--all-features`), so every lab binary,
  `teams-signin` included, refuses to run when `teams_fence::personal_enabled()` is true, beside its `test-support`
  refusal; the lab's normal build (`cargo build -p proto-lab -p teams-headless -p teams-app --target-dir
  target/lab`) never includes `ngteams`. A manifest test over `cargo metadata`/`cargo tree -p teams-app -e features`
  checks that building `teams-app` alone resolves `teams-fence` and `teams-signin` without it.
- **The fence's constructors**: `Fence::open_personal` exists only with `personal`; a `fence-seal` case shows it
  doesn't compile without the feature, a source test that no crate but `ngteams` calls it, and `ngteams` never calls
  `Fence::open_lab`.

## The CLAUDE.md text it needs (proposal)

*The record of the 2026-10-23 approval; [the decision of 2026-10-27](#the-personal-build-its-rules) supersedes its limits, and CLAUDE.md holds the current rule.*

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

> - **The personal build (`ngteams`) has a fence of its own, and only it.** The user set this on <date of approval>.
>   It is the user's own account on the user's desktop: the `ngteams` and `ngteams-signin` binaries, its data
>   under `~/.local/share/ngteams/` and `~/.config/ngteams/`, its Secret Service items under `ngteams`, no broker,
>   no sandbox. Its fence is `teams-fence`'s personal policy, behind the `personal` feature only `crates/ngteams`
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
>   refuses to run with `personal` or `test-support` built in; `ngteams` refuses the lab's.
> - **Agents never produce, install or run a runnable personal binary, and never touch its data.** Agents may
>   compile the `ngteams` crate in the gate and test it on `teams-mock`; any `ngteams` or `ngteams-signin`
>   binary they leave is a `test-support` one, which refuses to run. They never build either without
>   `test-support` (debug or release, a plain `cargo build` at the root included), never set the install marker,
>   never run `scripts/install-personal.sh` or either binary, and never read or write `~/.local/share/ngteams/` (its
>   diagnostic log included), `~/.config/ngteams/` or the `ngteams` items. They never receive the user's
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

*The record of the 2026-10-23 approval; [the decision of 2026-10-27](#the-personal-build-its-rules) supersedes its limits, and CLAUDE.md holds the current rule.*

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

- **Reads** (as of 2026-10-27). The lab's native client sends its reads by `POST` and its registration as plain
  requests the classifier lets through. Through the personal fence the transport sends every plain request
  (`Transport::send`), logged `allowed` when the classifier calls it `None` and `sent_unjudged` with its
  classification otherwise (`FenceLog::record_unjudged`); the registrations go as their own-state entries; the profile
  lookup, the presence read and the presence subscription (UPS's `v1/pubsub/subscriptions/<epid>`,
  `PresenceSubscription`) go as `PostRead` under a `Fence::check_post_read` permit, which a personal fence gives for
  any names, each logged by its kind; its Trouter frames are logged by kind (`FrameSink::logging`). UPS's presence
  deliveries are never answered. The engine reads the presence of everyone its views ask about but a bot or a service
  (`28:`, `48:`), which has none.
- **Writes** (as of 2026-10-27). `Fence::check` through a personal fence (`decision/audit.rs`) logs every write and
  permits it; it reads no members, so the personal engine's membership source is `FailClosed`, never asked. Typing
  and read receipts are writes like any other there. An allow `fence.log` can't take is still `LogUnavailable`.
- **`accounts.toml`.** Before the first sign-in, `ngteams` opens a window of its own asking for the work account
  (`teams_ui::account_prompt`); "Sign in" adds the UPN there (the user may add `idp_hosts`) and opens the sign-in
  window (bound to that account's hosts), which adds the object id and tenant; that account's fence opens
  only once both are there, and a sign-in that goes through starts the app afresh. A UPN typed that is one already is
  that account; another is a new account, taken out again when its sign-in stores nothing while its entry holds only
  its UPN (one with `idp_hosts`, or whose window refused an identity provider's host, stays pending). The rail's "+"
  opens the same window to add an account while the app runs. An account a sign-in named that can't start without one signs in
  again once the others run (or on that window, with none running). `ngteams --account <UPN>` skips the window and goes
  to the sign-in window at once, under the same rule.
- **Removing an account** is "Sign out and remove" in its account menu, or `ngteams --forget <UPN>` (its
  `ngteams` items, its `accounts.toml` entry, its region; the store stays, to delete by hand). The flag is
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

- **The fence** (as of 2026-10-27, `teams-fence`, `decision/audit.rs`): every call write is permitted and logged; an
  operation on a call moves the call registry as the lab's call rule moves it (`resolve`, or for an answer or a
  decline `resolve_answer`), every delivery response with a delivery id is permitted as the lab's is, and the personal
  fence decides calls by the lab's bookkeeping (`CallRule::Entry`), never by the registry's own stricter rule. An
  offered invitation is any readable one addressed to the account whose caller isn't the account, offered only while
  no registered call is live and no other offered one is answered or rings with its end bound. The call floor still
  refuses a start, a join or an answer while another account is busy with a call.
- **No chat is created for the test call**: the native test call needs none (its `add` names the 1:1 chat of the
  account and the bot, as the lab's native echo call does).
- **What a call sends besides its sealed writes and typed delivery responses**, as the lab's native calls showed
  (the `allowed` lines of M3 Tasks 15, 16 and 21 in the lab's `fence.log`, and `docs/protocol/endpoints.md`): the
  calling registration (`POST registrations`, `SkypeSpacesWeb`), which goes as its own-state entry
  (`calling_endpoint_registration`, `Transport::send_own`), and the TURN credentials (`GET trap/tokens`), a read. The
  web client's calling policy reads (`useraggregatesettings`, `effectivePolicies`) are the browser's, never the native
  client's: unneeded.
- **The devices** are `teams-media`'s PipeWire backend as this build allows it: the desktop's, the camera through the
  desktop's camera portal; a build with `test-support` (every `cargo test` of `crates/ngteams`) only the UI sandbox's.
  Without them the app runs without calls (`ngteams::calling::open`). The first run's screen says what a call sends
  without a click (an incoming call told it rings here), which has no switch.

The native client's requests today, by kind (examples; a new one is an ordinary change):

- **`GET`s**: the chat service's reads, Graph's two probed shapes (a user's name and their 96×96 picture), CSA's
  team reads, a message's picture on AMS (`MessagePicture`), the TURN credentials (`RelayTokens`), and with no token
  a reaction's animation on Teams' CDN (`Service::Emoticons`).
- **Own-state**: the messaging and the calling stacks' registrations of their own Trouter endpoints
  (`EndpointRegistration`, `CallingRegistration`).
- **Frames**: Trouter's `user.authenticate`, its keep-alive ping, a message-loss acknowledgement and the disconnect;
  beside them the calls' typed delivery responses, under [Calls](#calls).
- **Reads by `POST`**: the profile lookup (`ShortProfileLookup`, the middle tier's `fetchShortProfile`), the presence
  read (`PresenceRead`, UPS's `getpresence`) and the presence subscription (`PresenceSubscription`, the account's own
  endpoint subscribed to the people it names).
