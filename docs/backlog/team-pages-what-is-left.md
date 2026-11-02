---
worth: soon
where: crates/teams-ui/src/manage_team.rs, crates/teams-ui/src/team_dialogs.rs, docs/protocol/teams.md
added: 2026-11-02
---
# What the team pages still lack

The team pages (`docs/protocol/teams.md`) leave these parts of the web client's out:

- **Manage team's Apps tab**: the capture named no read of a team's installed apps (only `beta/users/apps/batchedDefinitions`,
  definitions by id), so it isn't offered. Its installed list needs a capture of what the tab reads before it can be built;
  adding and removing an app goes on from there.
- **Change picture** in Team details: `POST beta/teams/<group>/profilepicture?threadId=<thread>` takes the image's bytes, which
  the sealed writes' JSON body (`Wire`) can't carry.
- **"More create team options"** in Create team: from another team (`POST beta/teams/<team>/clone`) or a Microsoft 365 group
  (`GET beta/teams/usergroups`, `PUT beta/teams/migrateGroup/<group>`); not captured. The dialog creates from scratch only.
- **Analytics' "Select a channel"**, and the Giphy content rating under Fun stuff (`giphyRating`, whose values no capture named).
- **Answering a shared channel's invitation** on Pending invites: the tab lists them; no invitation has come to the lab to
  capture what answering one sends.
