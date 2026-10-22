---
worth: later
where: docs/protocol/channels.md (No channel write), crates/teams-proto/src/write/
added: 2026-10-22
---
# Channel writes need a team of the lab accounts only

M2 Task 16 made teams and channels read-only in both builds, as the user decided on 2026-10-21: the Teams rail lists
the account's teams and their channels (CSA's teams read), and a channel's posts and reply chains are shown from the
chat service, with no composer and no message action. Nothing the native client sends can post, reply, react, mark
read or type in a channel: `teams-proto` has no sealed write for one (`WriteKind::ChannelPost` is mapped nowhere), and
the browser fence leaves the web client's channel writes `Unknown` and denied (`docs/protocol/endpoints.md`).

Why they can't be mapped now: a channel write is seen by every member of its team, and the lab accounts' teams are
the tenant's, shared with people outside the lab (the M2 plan's premise; Task 16's probe found two teams and no team
of the lab's own). A write there is
one the tenant's directory shows to people outside the lab, which the hard rules forbid; and the fence has no complete
read of a channel's audience anyway (a channel's thread read listed one member, the team's roster is CSA's or the
middle tier's, and a shared or private channel has its own).

What would settle it, in order:

1. A team whose members are exactly the lab accounts a, b and c, created by the user (creating a team is itself a write
   the tenant's directory shows: the user's decision, never an agent's), recorded the way `chats.toml` records the lab's
   group chat.
2. An authoritative, complete read of that team's and its channels' members at decision time (the team's roster from
   CSA or the middle tier, a channel's own for a private or shared one), probed read-only first, and a change to the
   hard rules naming that team, in the user's words, before any code relies on it.
3. Captures of the web client's channel post, reply, edit, delete, reaction and read in that team under the armed
   fence, each mapped from its capture (`ChannelPost` and the reply chain's `<channel>;messageid=<post>` target), then
   sealed native writes and a composer in the channel view.

Until then the personal build stays read-only in channels too.
