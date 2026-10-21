---
worth: later
where: crates/teams-fence/src/membership.rs (parse_group, LabGroup)
added: 2026-10-20
---
# a group chat's member read is complete only by assumption, and only for the lab's recorded chat

M1 Task 8 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`) read the a–b–c group chat's thread and its
member list as a, b and c (reference copies `reference/chat/thread-group-{a,b,c}.jsonl`, outside the repo; shapes in
`docs/protocol/fence-targets.md`, synthetic fixtures `tests/fixtures/chatsvc/thread-group*.json`) and decided with
Codex that the answers don't show the member list is complete: two endpoints agreeing, with `totalMemberCount`, fit
both showing, and counting, a caller-visible subset just as well; no paging form was ever seen (`pageSize=1` returned
everything); equal `eTag`s show observed equality, not one roster revision (the list's `rosterVersion` is 0); the
filtered view is refused to members (403 `AclCheckFailed`). Transcript:
`~/.local/state/plan-exec-revmux/20261020-native-teams-m1-headless-chat-core/discussions/task-8-group-complete/transcript.md`.

**Since the user's decision (2026-10-21, "No, we need to fix group chats. Loosen the fence if needed.")** the fence
admits one group chat by an accepted assumption instead of evidence: the chat `chats.toml` records as `abc`, when its
two fresh reads agree on exactly a, b and c, made by a, unpaged and counted, in the shape seen live
(`fence-targets.md`, "The lab's group chat"; designed with Codex, transcript
`~/.local/state/plan-exec-revmux/20261020-native-teams-m1-headless-chat-core/discussions/group-chats-fence/transcript.md`).
The residual risk is written there: a participant the service shows to no member would still get the write.

**Still owed:**

- Evidence that would make the assumption a fact, for chats specifically (any one): authoritative evidence that an
  unfiltered `/members` read and its `totalMemberCount` cover every current participant whatever the caller may see;
  the chat member list's real paging and terminal-page form (no lab chat is big enough, and the lab can create chats
  of its 3 accounts only; a read-only look at a large tenant thread would show the paging form for another thread
  template, with other people's member lists, so it wasn't done); what equal `eTag`s across the two endpoints mean.
  With it, the rule could cover other lab-only group chats; without it, it must not grow.
- Group chats the native client creates (`Action::CreateChat` with two others) aren't recorded, so their writes are
  denied (`crates/teams-core/tests/engine.rs`, `a_group_chat_made_natively_is_denied_its_first_message_under_the_live_read`;
  the engine's other new-group tests run on a labelled stand-in source). Admitting one needs an enrolment that binds
  the id the service returns to the exact create the fence permitted (its kind, `New`, its added members) and then
  the same fresh checks; a permit, an outbox row or a caller-written `chats.toml` entry alone isn't enough.
- `record_abc_chat` accepts any well-formed group chat id; the record is trusted lab data, as `lab.toml` is. It
  narrows where the assumption is made; the reads still decide.
