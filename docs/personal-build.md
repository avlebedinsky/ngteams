# The personal build: its rules, for the user's decision

Status: 2026-10-22, M2 Task 26 (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`). **A proposal, not yet
approved.** Nothing here is in force: CLAUDE.md's hard rules still name the lab only, and no code that loosens the
fence for a personal build exists or may be written before the user approves this text or an amended one
(`docs/backlog/personal-build-rules-await-the-users-decision.md`). Task 27 implements what is approved, as the
approved CLAUDE.md text says.

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
| account | one lab alias per process, from `lab.toml` | the user's own work account, the one the sign-in window signs in |
| config | `~/.config/teams-lab/lab.toml` | `~/.config/teams/account.toml` (written by the first sign-in, below) |
| data dir | `$XDG_DATA_HOME/teams-lab/` (or `lab.toml`'s `data_dir`) | `$XDG_DATA_HOME/teams/` (`~/.local/share/teams/`), `0700` |
| store | `<lab data>/store/<alias>.sqlite` | `~/.local/share/teams/store.sqlite` (`0600`, as `teams-store` makes it) |
| fence log | `<lab data>/fence.log` | `~/.local/share/teams/fence.log` (`0600`) |
| region (no token) | `<lab data>/auth/<alias>.json` | `~/.local/share/teams/region.json` |
| UI settings | `<lab data>/ui/<alias>.toml` | `~/.local/share/teams/settings.toml` |
| Secret Service application | `teams-lab` (`LabPaths::KEYRING_APPLICATION`) | `teams-personal` |
| credentials | through the broker (`teams-app secrets`) | `KeyringSecretStore::new("teams-personal")` in-process: **no broker** |
| window | only in the UI sandbox (`UI_SANDBOX_DIR`), `DISPLAY` unset | the user's desktop session: **no sandbox** |
| bus | the sandbox's private bus | the user's session bus (notifications, tray, Secret Service) |
| sign-in | `proto-lab auth` (the armed Chrome fence) | `teams-signin --personal` (below) |
| live lock | `~/.local/share/teams-lab/live.lock` around every live command | none: it is the user's own app |

`account.toml` holds what the first sign-in learned, never a token: the UPN the user typed, and the ID token's `oid`
and `tid`. Every later sign-in must give the same `oid` and `tid` (the ID token's `preferred_username` checked as
`teams_auth::token::TokenClient::exchange_code` checks it today, then the pair compared), so a sign-in that lands in
another account or another tenant stores nothing and says so. Changing account is an explicit "sign out and forget
this account" in the app, which deletes the `teams-personal` items, `account.toml` and, after a confirmation, the
store.

The engine is built as `teams_core::lab::start` builds it (the `Session`, the transport, the push channel, the
outbox), from `account.toml` and the region the authz exchange gives instead of `lab.toml`; Task 27 factors the
shared part out of `lab.rs` rather than copying it. The engine lock (`Store::engine_lock`) keeps one `teams` per
account, as it keeps one lab engine per alias.

## The personal fence

### Where it lives

In `teams-fence`, behind a cargo feature `personal` that only `crates/teams/Cargo.toml` enables. `Fence` keeps its
one type (the transport, the outbox and the push channel all take an `Arc<Fence>` today) and gains a policy inside
it: the lab's roster and group chat as now, or, only with `personal`, the personal account. The only way to a
personal policy is `Fence::open_personal(paths)`, which exists only with the feature, reads `account.toml` from the
personal paths and opens the personal `fence.log`. Permits, `Authorized<_>`, `PostReadPermit` and the sealed
`MembershipSource` stay where they are: nothing outside `teams-fence` mints one, in either build.

### What it permits

A write is permitted only when all of these hold, each checked when the fence decides:

1. **It is a sealed `Mutating` write sent as `Authorized<_>`**, through the outbox (durable rows) or its transient
   kind (typing, the account's own availability), with its permit from `Fence::check` for its exact `Target`. No new
   way out: the outbox's rules, the retry table and "no resend across runs" are unchanged.
2. **Its kind is one the personal build sends in M2**: `Message`, `Edit`, `Delete`, `Reaction` (the keys a capture
   showed), `Typing`, `ReadReceipt`, and the account's own availability once Tasks 18–19 have mapped it. Every other
   kind (`ChatCreate`, `MemberChange`, `Invite`, `ChannelPost`, `Call`, `CallAnswer`, `Meeting`, `BotReply`) is
   denied by kind, and so is any `Scope::New` target.
3. **Its target is a conversation the account is a member of, by a fresh member read**: the fence's personal
   membership source (a new sealed source in `teams-fence`) reads the conversation's thread from the chat service at
   decision time (`GET v1/threads/<id>?view=msnp24Equivalent`, the read `LiveMembershipSource` already sends) and the
   write passes only when the answer is in a shape a capture showed and lists the account's own MRI as a member.
   Never a cache, never the store's member list.
4. **The conversation is a kind the lab has captured**: a 1:1 chat whose id names the account, or a group chat
   (`19:<id>@thread.v2`, `threadType` `chat`, not a meeting's id). Channels stay read-only (both builds, the user's
   decision of 2026-10-21); meeting chats (`19:meeting_…`), the account's own streams (`48:…`) and any thread whose
   type or member shape no capture showed are denied.
5. **It comes from an explicit action in the UI, or from a setting the user turned on.** A message, an edit, a
   delete, a reaction or "send again" is a key or a click on that one item; typing, read receipts and the account's
   availability go out only while their setting is on (below). Nothing is sent on a timer the user didn't enable,
   and nothing replays an earlier run's row without `resend`.
6. **It is logged first**: the decision goes to the personal `fence.log` before the request is sent, and an allow
   that can't be logged is denied, as in the lab.

**Choice A: membership, not roster completeness.** The lab fence needs a complete member list, because it must
prove nobody outside the lab is there. The personal fence asks only whether the account is in the conversation, so
a read that lists the account answers it even when the service pages a large group's members; a read that fails,
is in an unknown shape, or doesn't list the account (a page without it included) is denied. The other option, a
complete roster as in the lab, would deny every write in a group too large for one read; Task 27's tests are written
for whichever is approved ("a partial read is denied" then means "a read that doesn't show the account").

**Choice B: outsiders of another kind.** A member whose MRI is in a form the lab never captured (a guest, a
federated user of another tenant, a consumer account) makes the read's shape unknown, and the write is denied, as
`Field::Unknown` is never guessed. That leaves chats with external people read-only until a shape is seen; the
alternative is to accept any MRI form as "a member", which a capture can't back. Bots (`28:` members) already have a
captured form: a message to a chat with a bot in it passes like any other, and bot invokes and Adaptive Card
actions stay unmapped and denied (`docs/backlog/bot-invokes-and-message-extensions.md`).

### Its reads

- **`GET`s** on the transport's host allowlist, as the lab sends them (the chat service, Graph's two probed shapes,
  CSA's team reads); a new host or shape is added under the same review rule as in the lab.
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
| calls | M3 | incoming calls ignored, never answered; the user's other clients still ring |
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

## Its sign-in

The lab signs in through `proto-lab auth` in the armed Chrome; the sign-in window (`teams-signin`) is offline-only,
because its WebKit guard misses two kinds of redirect (`docs/backlog/webkit-sign-in-guard-redirects.md`) and so
can't promise that every request is decided and logged before it goes. The personal build has no lab profile, so
it needs the window, under a policy for the user's own sign-in:

- **A mode of its own**: `teams-signin --personal`, started by `teams` with the authorize request on stdin (as
  `teams_auth::signin` does now) and logging to the personal `fence.log`. `--lab` keeps `NOT_LIVE` and stays
  loopback-only; the personal mode lifts it only for the hosts below.
- **A bounded set of hosts**: Entra's sign-in host (`login.microsoftonline.com`), its static content hosts (the
  `aadcdn`/`logincdn` hosts the sign-in page loads from), and the federated identity provider's hosts the user
  names in `account.toml` (`idp_hosts`, empty by default), `https` only. The network session's egress proxy (today
  `teams_signin::egress`'s black hole) becomes a host gate: it opens a `CONNECT` tunnel only to those hosts on 443,
  refuses and logs every other connection, and is the session's only way out, so the redirects the guard can't see
  reach no other host either. No Teams, Office or Graph host is on it: the window can only sign in.
- **The guard's decisions inside that bound**: navigations only to those hosts and the `nativeclient` redirect
  (caught and refused, as now); `GET`s to them; `POST`s to Entra's sign-in paths (the lab's `sign_in` rules plus the
  multi-factor and federation return paths, which the lab never showed and which no capture can review before the
  first personal sign-in) and to the identity provider's hosts. Every decision the guard sees is logged; the
  proxy's are too. The rest of Task 24's hardening stays: WebSocket, beacons, service workers, WebRTC, new windows
  and downloads off, an ephemeral session.
- **What it promises, said plainly**: a complete *host* bound (every connection goes through the gate), and a
  method and path bound only as far as the guard sees requests (not for the two redirect gaps). A redirected
  request inside the window reaches nothing but the identity hosts. That is accepted for the user's own sign-in
  because nothing the window can reach is a Teams service: whatever a sign-in page sends there changes only the
  account's own sign-in state.
- **Multi-factor prompts and federated sign-in** (the lab showed neither): the user completes them in the window
  (Authenticator number matching, a code by text or call, the identity provider's own form). What WebKitGTK can't
  do is not worked around: a passkey or security key (WebAuthn), a client certificate or a device-bound broker
  (Windows Hello, Intune's company portal) fails in the window, and the app says so. Registering a new method
  (`mysignins`, `aka.ms/mfasetup`) isn't on the host list: the user does that in a normal browser.
- **Never a workaround of Conditional Access**: no other client id, no borrowed user agent or device claim, no
  scraped browser cookie. The client is the native `1fec8e78-…` with the `nativeclient` redirect (`auth.md`), as in
  the lab.

**Choice C: the sign-in route.** The other routes, for the record: (1) wait for a complete WebKit guard
(`webkit-sign-in-guard-redirects.md`'s inspecting egress guard or a WebKit change), which keeps the personal build
signed out until then; (2) the system browser with a loopback redirect, which the native client's registration
isn't known to accept and which would run the user's own browser profile unfenced; (3) the device code flow, never
probed and often blocked by Conditional Access. The proposal is the host-bounded window above.

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

- **Allowed**: the gate compiles the `teams` crate (`cargo clippy --workspace --all-targets --all-features`,
  `cargo test --workspace`) and runs its lib's tests on `teams-mock` (`cargo test -p teams`). Any `teams` binary
  such a build leaves behind is built with `test-support` and refuses to run.
- **Not allowed**: producing a `teams` binary without `test-support`, in any profile (`cargo build -p teams`,
  `cargo run -p teams`, `cargo build --workspace`, `cargo install`, `--release` or not: a debug build reaches the
  tenant as well as a release one); running `scripts/install-personal.sh`; starting any `teams` binary; running the
  sign-in window in `--personal` mode outside the offline tests.
- **Never touched by an agent**: the `teams-personal` Secret Service items, `~/.config/teams/`,
  `~/.local/share/teams/` (the store, `fence.log`, settings), the user's sign-in. Tests of the personal build use
  temporary dirs and `MemorySecretStore`; a source test checks that only `crates/teams/src/main.rs` names the
  `teams-personal` application.

## How the lab and personal binaries are kept apart

- **Data dirs**: each refuses the other's. `teams` refuses a personal data dir that is, contains or lies inside the
  lab's data dir (the default and `lab.toml`'s `data_dir`, when `lab.toml` exists), and every lab binary refuses a
  lab data dir that is, contains or lies inside `$XDG_DATA_HOME/teams/` (a check in `LabConfig`'s validation, so
  `Fence::open_lab`, `teams-app`, `teams-headless`, `proto-lab` and `teams-signin --lab` all get it). Paths are
  compared after resolving symlinks.
- **Keyring names**: `teams` opens only `teams-personal`; the lab's broker already refuses a client naming another
  application (`teams_auth::broker`), and `teams-headless` and `proto-lab` name only `teams-lab`. Neither reads,
  writes or deletes the other's items.
- **Accounts**: `teams` refuses to sign in, or to keep, an account `lab.toml` names (its UPNs and MRIs), so a lab
  account is never driven by the personal fence; the lab binaries know only `lab.toml`'s accounts.
- **`test-support`**: both refuse a build with it (`teams_fence::test_support_enabled`), as every lab binary does
  today.
- **The `personal` feature**: only `crates/teams/Cargo.toml` enables `teams-fence/personal` (a manifest test over
  every workspace manifest). Feature unification would bring it into a lab binary built in the same cargo
  invocation as `teams` (`--workspace`, `--all-features`), so every lab binary refuses to run when
  `teams_fence::personal_enabled()` is true, beside its `test-support` refusal; the lab's normal build
  (`cargo build -p proto-lab -p teams-headless -p teams-app --target-dir target/lab`) never includes `teams`. A
  manifest test over `cargo metadata`/`cargo tree -p teams-app -e features` checks that building `teams-app` alone
  resolves `teams-fence` without it.
- **The fence's constructors**: `Fence::open_personal` exists only with `personal`; a `fence-seal` case shows it
  doesn't compile without the feature, a source test that no crate but `teams` calls it, and `teams` never calls
  `Fence::open_lab`.

## The CLAUDE.md text it needs (proposal)

CLAUDE.md's hard rules name the lab today ("only within the 3 lab accounts", "Lab binaries get a `WritePermit` only
from `teams-fence`", "Agents never build or run a personal build"). The proposal keeps every lab rule as it is,
says that the lab rules govern every lab binary, and adds one rule for the personal build. The text to add, after
the rule "The native client sends only what its sealed policy allows":

> - **The personal build (`teams`) has a fence of its own, and only it.** The user set this on <date of approval>.
>   It is the user's own account on the user's desktop: the `teams` binary, its data under `~/.local/share/teams/`
>   and `~/.config/teams/`, its Secret Service items under `teams-personal`, no broker, no sandbox. Its fence is
>   `teams-fence`'s personal policy, behind the `personal` feature only `crates/teams` enables, opened only by
>   `Fence::open_personal`. A write goes out only as a sealed `Mutating` type sent as `Authorized<_>` through the
>   outbox, with a `WritePermit` from `Fence::check` for its exact target; only the kinds `docs/personal-build.md`
>   lists; only to a 1:1 or group chat whose thread, read fresh when the fence decides and in a shape a capture
>   showed, lists the account as a member (never a channel, a meeting chat, a `48:` stream, a new chat or an
>   unknown shape); only from an explicit action in the UI or a setting the user turned on; logged to its
>   `fence.log` before it goes. Its reads are the lab's sealed lists, and `PostRead` may name anyone the account's
>   stored chats list, never a search or a suggestion list. Its sign-in window reaches only Entra's sign-in hosts
>   and the identity provider's hosts `account.toml` names, through its egress gate. Nothing in it changes a lab
>   rule: every lab binary keeps the lab fence, refuses the personal data dir, keyring items and accounts, and
>   refuses to run with `personal` or `test-support` built in; `teams` refuses the lab's.
> - **Agents never produce, install or run a runnable personal binary, and never touch its data.** Agents may
>   compile the `teams` crate in the gate and test its lib on `teams-mock`; any `teams` binary they leave is a
>   `test-support` one, which refuses to run. They never build it without `test-support` (debug or release),
>   never run `scripts/install-personal.sh` or a `teams` binary, never run the sign-in window's `--personal` mode
>   against a real host, and never read or write `~/.local/share/teams/`, `~/.config/teams/` or the
>   `teams-personal` items. They never receive the user's tokens, chat ids or logs; the user reports a problem by
>   its reason.

And these edits to rules that exist:

- "Writes go only through the fence, and only within the 3 lab accounts." gains, at its end: "This rule and the
  ones below it govern every lab binary; the personal build is governed by its own rule."
- "Agents never build or run a personal build and never receive the user's tokens." becomes "Lab binaries get a
  `WritePermit` only from `teams-fence`'s lab policy." with the rest of that rule's text unchanged (the personal
  side moves to the new rule above).
- "The native client sends only what its sealed policy allows (M1 Tasks 5, 15), as the browser fence does for the
  browser." gains: "This holds for the personal build too; only its fence's policy differs."
- "Live runs" in "Patterns learned" gains: "The personal build is never a live run of an agent's; the live lock is
  the lab's."

## What the user is asked to decide

Approve this document (and the CLAUDE.md text above), or amend it. The choices that matter most:

1. **The personal fence's question** (Choice A): a write passes when a fresh read of its chat lists the account,
   not when the whole roster is read; large group chats stay writable.
2. **Unknown member forms** (Choice B): chats with guests or people of another tenant stay read-only until a lab
   capture shows their member form.
3. **The sign-in route** (Choice C): the WebKit window with a host gate (Entra and the user's identity provider
   only), its two redirect gaps accepted knowingly for the user's own sign-in because they can reach no Teams host;
   or wait for a complete guard.
4. **`PostRead` for anyone in the account's stored chats**, and the settings that write on their own (typing, read
   receipts, presence) sent only after the first-run screen is confirmed.
5. **What agents may do**: compile and mock-test only; no runnable personal binary in any profile, no install, no
   run, no access to its data, keyring items or logs.

Task 27 waits on this decision.
