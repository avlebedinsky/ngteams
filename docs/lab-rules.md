# The lab's rules in full

CLAUDE.md carries the short form. This rule applies to implementation actions independently of any network fence.
Existing fence policy, old allowlists and historical captures never authorize an outside recipient.

**The one rule: lab accounts call, message and share only with each other, both ways.**
It governs every implementation run, lab browser and lab binary (HTTP and Trouter frames alike).
The personal build remains unrestricted for the user's normal use ([personal-build.md](personal-build.md)).

## Who the lab may reach

Only the three dedicated accounts in `lab.toml`'s `accounts`, by exact MRI or UPN. An account whose MRI is unknown
matches nobody by MRI. The personal user, echo service, bots and external contacts are never lab recipients.
Existing `contacts` and `service_participants` config entries are metadata only and cannot grant permission.
A conversation containing even one participant outside the three accounts is denied, including a mixed conversation.

## Before every implementation action

Before a call, answer, join, message, edit, reaction or share, independently verify the acting account's exact lab
MRI and UPN, the intended conversation and the selected thread, and a fresh full server recipient list including
self. Reject missing, hidden, deleted, duplicate, paged or truncated membership and any personal, echo, bot or
external participant. Recheck the selected thread immediately before the action; a call click checks it in the
same page evaluation. An incoming call must additionally match the exact expected caller, callee and conversation.

`teams-lab-targets` contains the shared identity/recipient checks. The browser's `ActionTargets` performs fresh,
bounded reads using only its own profile's observed credential; it does not ask the network fence for permission.
A cached conversation, display name, successful deep-link navigation or visible call button is insufficient.
A share must also prove its effective audience is limited to those exact accounts; verifying the chat alone does
not authorize an anonymous, organization-wide or inherited external grant. The native attachment flow checks this
independently before sharing or posting. Never run a contacting scenario until these preflights are present.

The harness refuses echo-call and legacy file/image, meeting, team and channel scenario entrypoints whose actions
have not established independent audience checks. Update and offline-test their preflights before enabling them.
Personal client behavior stays unrestricted for its user's normal use.

## What is denied

- **A write that reaches a participant outside the lab**: a message (post, reply, edit, delete, forward, channel post,
  scheduled or drafted send), a reaction, a call (start, join, answer, decline, `add`, a lobby's admit or removal), a
  chat created, a member added or removed, an invite, a meeting, a mail, a share, a team or channel change. It is
  denied when anyone on its target, read fresh and in full (no paging key), or anyone it adds, is not listed
  above. A team and its channels are read from the team's own thread.
- **A write that may reach people and whose target can't be read**: the member read failed, was paged or cut short; an
  org-wide team; a mutating request on a people-reaching service (chat, calling, Graph, Outlook, SharePoint, AMS, the
  middle tier's team, meeting and invite paths, CSA, provisioning) that no row maps or carves out; a write the fence
  can't tie to a target (a `Scope::New` for a kind that creates none, a join naming no chat).
- **An incoming call from outside**: the lab offers, answers or declines only an invitation addressed to the account
  whose caller is a lab account; every other one rings unanswered, nothing sent for it.

## Everything else is allowed

Reads by any method (a `POST` search or people lookup too), the account's own session, tokens and settings, presence
(the account's own availability), a status note, typing and read receipts, a target nobody else is in, team and channel
admin on teams of the lab's people, and what a call sends once its entry was let in (follow-ups, `leave`, ring-time
sends, Trouter delivery responses), each still a sealed type in its captured shape and logged.

## Transport and test execution

Transport authorization does not replace these operational checks. A neighboring change may remove the historical
fence infrastructure; implementation workflows must remain lab-only with or without it.

- **No write to the tenant comes from a `cargo test` process**: offline tests use synthetic identities and local
  mock services. Live runs use normally built lab binaries, the live lock and an outer timeout.
- Read credentials stay in memory, are never printed, and go only to their original validated Teams service over
  HTTPS, with redirects, proxies and retries disabled.
- **Lab and personal stay apart**: a lab binary never signs in as the user or uses the personal build's data or
  credentials. The personal account is never an implementation recipient.
