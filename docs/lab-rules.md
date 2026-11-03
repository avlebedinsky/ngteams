# The lab's rules in full

CLAUDE.md's hard rules carry the short form; this is the full text the lab fence implements
(`crates/teams-fence`; how it reads a target: [protocol/fence-targets.md](protocol/fence-targets.md)). Widening whom a
lab write may reach, or weakening a test of the rule below, needs the user's sign-off; anything else that passes the
tests (a new request, frame or write kind included) goes ahead without asking.

**The one rule: lab accounts talk and call only each other, the echo service and the user's own account, both ways.**
It governs the lab browser and every lab binary (HTTP and Trouter frames alike). The personal build is under its own
rules ([personal-build.md](personal-build.md)).

## Who the lab may reach

- The 3 lab accounts (`lab.toml`'s `accounts`, by MRI or UPN);
- the approved service participants (`service_participants`: the echo service, by MRI);
- the contacts (`contacts`, by MRI, and by UPN when given): the user's own account. It is reached as a lab account is,
  for everything: messages, reactions, chats created, members added, invites, meetings and calls either way.

Apps and bots (any `28:` MRI) aren't people: one on a chat or a team, Teams' own apps every new team holds or one
someone added, a team made from a template that adds apps, or an app a write adds never makes a write contacting.

## What is denied

- **A write that reaches a person outside the lab**: a message (post, reply, edit, delete, forward, channel post,
  scheduled or drafted send), a reaction, a call (start, join, answer, decline, `add`, a lobby's admit or removal), a
  chat created, a member added or removed, an invite, a meeting, a mail, a share, a team or channel change. It is
  denied when anyone on its target, read fresh and in full (no paging key), or anyone it adds, is a person not listed
  above. A team and its channels are read from the team's own thread.
- **A write that may reach people and whose target can't be read**: the member read failed, was paged or cut short; an
  org-wide team; a mutating request on a people-reaching service (chat, calling, Graph, Outlook, SharePoint, AMS, the
  middle tier's team, meeting and invite paths, CSA, provisioning) that no row maps or carves out; a write the fence
  can't tie to a target (a `Scope::New` for a kind that creates none, a join naming no chat).
- **An incoming call from outside**: the lab offers, answers or declines only an invitation addressed to the account
  whose caller isn't a person outside the lab; every other one rings unanswered, nothing sent for it.

## Everything else is allowed

Reads by any method (a `POST` search or people lookup too), the account's own session, tokens and settings, presence
(any lab account's availability), a status note, typing and read receipts, a target nobody else is in, team and channel
admin on teams of the lab's people, and what a call sends once its entry was let in (follow-ups, `leave`, ring-time
sends, Trouter delivery responses), each still a sealed type in its captured shape and logged.

## The fence and the transport

These aren't the lab rule; they stay as they are.

- **Nothing runs live against the tenant before the browser fence is armed**: HTTP interception on every host and
  target, the WebSocket guard, the service worker neutralized.
- **The fence logs every write**: the native client sends a contacting write only as `Authorized<_>` with a
  `WritePermit` from `Fence::check` for its exact target, logged to `fence.log` (a query's key names, never its
  values); only a sealed `Mutating` type in `teams-proto` builds one. A lab binary's transport refuses any request
  `teams_fence::contacting` doesn't call `None` (the personal build sends it, logged).
- The transport is `https` only, no proxy, no redirects, to its services' bases, each token only to its own service's
  host, and never resends a write (the HTTP client's retries stay off). A mock origin exists only under
  `test-support`, which a lab binary refuses.
- **No write ever comes from a `cargo test` process**: a live test spawns a normally built lab binary
  (`--target-dir target/lab`) and reads its output and `fence.log`. Both builds; only the fence policy differs.
- **Lab binaries get a `WritePermit` only from the lab policy** (`Fence::open_lab`, `Fence::open_lab_account`: roster
  from `lab.toml`, the lab's `fence.log`); `teams-fence`'s `test-support` is only a `[dev-dependencies]` feature.
- **Lab and personal stay apart**: a lab binary refuses the personal build's data, keyring items and accounts; the
  user's own account is only someone the lab may reach, never an account a lab binary signs in as.
- **No token or credential goes to a third party** (a bot, an app's backend; a bot invoke stays denied). Sign-in and
  renewal stay in `teams-auth`'s own clients and touch only the account's own credentials.
