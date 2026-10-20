---
worth: later
where: crates/teams-fence/src/membership.rs:374
added: 2026-10-20
---
# can the fence ever call a group chat's member read complete, and does M1 exit without it?

M1 Task 8 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`) created the a–b–c group chat (`proto-lab run
abc-chat`, its id in the lab's `chats.toml`) and read its members as a, b and c with `proto-lab probe thread`
(reference copies `reference/chat/thread-group-{a,b,c}.jsonl`, outside the repo; shapes in
`docs/protocol/fence-targets.md` and the synthetic `tests/fixtures/chatsvc/thread-group*.json`). What the service
answered, the same for all three:

- the thread read (`v1/threads/<id>`, and the batch form the web client uses) lists a, b and c, with no paging key and
  no member count; `rosterVersion` = `eTag` = the creation time;
- the member list (`v1/threads/<id>/members`) lists the same three with `totalMemberCount: 3`, the same `eTag`,
  `rosterVersion: 0`, and no next link, at `pageSize=100` and at `pageSize=1` alike;
- the filtered view (`v1/threads/<id>/filtered`) is refused to every member (403, `AclCheckFailed`: an `Admin` of a
  chat lacks `GetFilteredThread`).

The candidate rule was a paired read: the thread read and the member list both 2xx, unpaged and listing the same
distinct members, `totalMemberCount` equal to that list, equal non-empty `eTag`s, `threadType` `chat`, a
`19:<id>@thread.v2` id that isn't a meeting's, the whole operation within the 10 s budget.

**Decided with Codex (2026-10-20): the evidence is not sufficient, so group chats stay `Partial(Unsupported)`** and the
rule isn't implemented, not even unwired. Claude started from "sufficient" (the service's own `totalMemberCount`
standing in for the 1:1 id on "not a page"; two endpoints agreeing while the filtered view is forbidden standing in for
"not a filtered view") and conceded Codex's objection: agreement and a matching count are just as consistent with both
endpoints exposing, and counting, the same caller-visible subset, and a homogeneous 3-member roster seen identically by
all three can't tell the two apart. `pageSize=1` returning everything shows no paging form at all; the 403 shows only
that the filtered endpoint is gated; equal `eTag`s show observed equality, not that they name one roster revision (the
list's own `rosterVersion` is 0). A `≤ 100` cap adds nothing: a larger roster silently cut to three would pass it.
Transcript: `~/.local/state/plan-exec-revmux/20261020-native-teams-m1-headless-chat-core/discussions/task-8-group-complete/transcript.md`.

**What would settle it** (any of these, for chats specifically):

- authoritative evidence that an unfiltered `/members` read and its `totalMemberCount` cover every current
  participant whatever the caller may see;
- the chat member list's real paging and terminal-page form (never seen: no lab chat is big enough, and the lab can
  create chats of its 3 accounts only); a read-only look at a large tenant thread would show the paging form, but for
  another thread template and with other people's member lists, so it wasn't done;
- evidence that equal `eTag`s across the two endpoints mean the same roster revision;
- which chat kinds the evidence covers (only `threadType` `chat`, never meetings).

**The decision for the user** (the plan's Decisions for the user): whether M1 exits on 1:1 chats, with group chats
built and tested on `teams-mock` only, or waits for that evidence. Choosing to go on doesn't make group reads complete
or loosen the fence.

**Meanwhile**: `LiveMembershipSource` answers every group chat `Partial(Unsupported)` without a read
(`LiveMembershipSource::reads_in_full` says so up front), so any write to one is denied. `abc-chat`'s messaging run
refuses to start before it opens a browser (`abc_chat::messaging_blocked`): a denied message would stay unsent in a's
client and be sent again on every later load of a's profile. The chat itself exists (a, b, c, empty: its first message
was force-denied. If the web client sends it again, `ab-chat` and `abc-chat` force-deny it by the `lab fence probe`
marker, and any other scenario on a's profile sees the fence deny it as a group chat write, unexpected there; no resend
was in `fence.log` by 2026-10-20 23:00). To take this up:
add the read in `crates/teams-fence/src/membership.rs` behind the evidence, make `reads_in_full` say yes for it, and
run `proto-lab run abc-chat --headless` (Task 8's last live item).
