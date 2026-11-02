# The lab's rules in full

CLAUDE.md's hard rules carry the short form; this is the full text the lab fence implements
(`crates/teams-fence`; how it reads a target: [protocol/fence-targets.md](protocol/fence-targets.md)). Changing a
test here, or widening whom a lab request may name or reach, needs the user's sign-off; a new request, frame or write
kind that passes these tests goes ahead without asking.

**Lab accounts contact only each other (and call the user's own accounts named as call guests); everything else is
allowed.** The rules on writes, calls and the browser fence govern the lab browser and every lab binary (HTTP and
Trouter frames alike). The personal build is under its own rules ([personal-build.md](personal-build.md)).

## Contacting writes

- **A contacting write goes only to the lab.** It is one that reaches people: a message (post, reply, edit, delete,
  forward, channel post, scheduled or drafted send), a reaction, a call (start, join, answer), a chat created, a member
  added or removed, an invite, a meeting, a mail, a share. It is allowed only when every participant of its target,
  read fresh and complete (no paging key), is one of the 3 lab accounts (`lab.toml`'s MRIs) or an approved service
  participant (the echo service, by MRI); for a call alone, a call guest too. A target whose members can't be read in
  full, or a write the fence can't tie to a target with certainty, is denied.
- **A team and its channels** (approved by the user on 2026-11-03): a write on a team, or on one of its channels, is
  judged by the team's members, read fresh and complete from the team's own thread. Teams' own apps that every new team
  holds as members (`lab.toml`'s `team_apps`, by MRI, from a capture) aren't counted as people it reaches; anyone else
  on the team, an app or a bot someone added included, must be a lab account or an approved service participant. A
  team app is never someone a write may add, and counts nowhere but on a team.
- **Everything else is allowed**, with no capture or review: reads by any method (a `POST` search or people lookup
  too), the account's own session, tokens and settings (out-of-office included), presence, a status note, typing and
  read receipts.
- **When unsure whether a request reaches people, treat it as contacting.** On a people-reaching service (chat,
  calling, Graph, Outlook, SharePoint, AMS, the middle tier's team, meeting and invite paths, CSA) a mutating request
  is contacting unless known not to be (typing, read receipts, own feed, settings, endpoints); unreadable is denied.
- A new contacting write kind (a meeting's, say) is mapped from a capture and let through only by the test above; the
  examples here are today's, not a closed list.

## Calls go only to the lab, both ways

A call's participants are its caller, callees and whoever it adds; every call write is mapped only from a capture, in
its captured shape.

- **Call guests** (the user's own accounts in `lab.toml`'s `call_guests`, by MRI) are approved for calls only (start,
  join, answer, decline, `add`, and what a call sends once let in), in their 1:1 chat with a lab account or a chat of
  lab accounts plus them. A guest is never the caller, joiner, answerer or adder of a lab write, and nothing else of
  the lab reaches one (no message, reaction, chat created, member added or invite, even in a chat holding one).
- Each entry is a fresh decision. A **start** passes when every callee is a lab account, call guest or the echo
  service and its chat holds only lab accounts and call guests by a fresh, complete read; it is tied to its chat by
  what it carries (the chat it names, or for a 1:1 start naming only its callee, the two accounts' 1:1 chat), never
  by client context; the test call's start, naming no chat, passes only in its captured shape. A **join** passes only
  in the chat it names when that chat's fresh, complete read lists the joiner and only lab accounts and call guests.
  An **`add`** is judged per invitee like a start.
- An incoming invitation is **offered** only when readable, addressed to the account, and naming a caller and
  participants that are all lab accounts, call guests or the echo service. Only an offered invitation is answered or
  declined (a decline rejects the call as the web client does), tied by the call leg the answer names, until its ring
  ends. Every other incoming call is ignored: no answer, decline, join or scenario click.
- Once an entry is let in, everything the call sends goes with no further decision, each send logged (follow-ups,
  `leave` and end, the callee's ring-time sends, Trouter delivery responses), still as sealed types in their captured
  shape, a delivery response only as a typed `OwnFrame` for its delivery. No transfer, forwarding or replacement is
  mapped; the roster isn't watched; media (TURN, ICE, DTLS-SRTP) isn't classified.

## The fence and the transport

- **Nothing runs live against the tenant before the browser fence is armed**: HTTP interception on every host and
  target, the WebSocket guard, the service worker neutralized.
- **The native client sends a contacting write only as `Authorized<_>`** with a `WritePermit` from `Fence::check` for
  its exact target, logged to `fence.log`; only a sealed `Mutating` type in `teams-proto` builds one. A lab binary's
  transport refuses any request `teams_fence::contacting` doesn't call `None` (the personal build sends it, logged).
- The transport is `https` only, no proxy, no redirects, to its services' bases, each token only to its own service's
  host, and never resends a write (the HTTP client's retries stay off). A mock origin exists only under
  `test-support`, which a lab binary refuses.
- **No write ever comes from a `cargo test` process**: a live test spawns a normally built lab binary
  (`--target-dir target/lab`) and reads its output and `fence.log`. Both builds; only the fence policy differs.
- **Lab binaries get a `WritePermit` only from the lab policy** (`Fence::open_lab`, `Fence::open_lab_account`: roster
  from `lab.toml`, the lab's `fence.log`); `teams-fence`'s `test-support` is only a `[dev-dependencies]` feature.
- **No token or credential goes to a third party** (a bot, an app's backend; a bot invoke stays denied). Sign-in and
  renewal stay in `teams-auth`'s own clients and touch only the account's own credentials.
