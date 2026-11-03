---
worth: later
where: crates/teams-ui/src/teams_page.rs, crates/teams-ui/src/create_team_more.rs, docs/protocol/teams.md
added: 2026-11-02
---
# What the team pages still lack

The team pages (`docs/protocol/teams.md`) leave these parts of the web client's out, each for want of a capture the lab can't
make today:

- **Answering a shared channel's invitation** on Pending invites: the tab lists them, but an invitation is a shared channel
  shared with a team the account owns, and the lab's tenant offers no shared channel ("Create a channel" lists Standard and
  Private only, `isSharedChannelCreationEnabled` `false`, a tenant policy). Once the tenant allows shared channels, a lab team
  of a sharing a channel with a lab team of b's would bring b one to capture.
- **Making a team of a Microsoft 365 group** ("More create team options", From group): `beta/teams/usergroups` has only been
  seen empty, since no lab account owns a group, so a group's entry and what making a team of it sends (`migrateGroupToTeam`
  in the bundle) are unknown. The page shows Teams' words for no group, and only a count otherwise.
