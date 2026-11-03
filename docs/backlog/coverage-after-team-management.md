---
worth: now
where: coverage.toml; crates/teams-proto, crates/teams-core, crates/teams-store
added: 2026-11-03
---
# Coverage floors broken by team management and channel meetings

The review pass over `b07db99..6858f2c` (manage teams, create channels, team apps and pictures, channel meetings,
calendar cache) ran `scripts/coverage.sh -p <crate>` against the saved floors in `coverage.toml`. teams-fence,
teams-call, teams-log and teams-trouter pass (the first two after tests the pass added). Three don't:

| crate | measured (lines / functions / regions) | floor |
|---|---|---|
| teams-proto | 85.4 / 84.1 / 85.0 | 93 / 91 / 91 |
| teams-core | 81.5 / 80.7 / 79.5 | 91 / 92 / 89 |
| teams-store | 96.3 / 97.3 / 88.6 | regions 90 |

The gaps are almost all the new team pages' code, which shipped with UI tests but few tests of its own:
`teams-proto/src/team_read.rs` (436 lines uncovered), `write/team.rs` (222), `write/channel.rs` (194),
`channel_read.rs` (94); `teams-core/src/team_admin.rs` (373), `calendar.rs` (177), `team_apps.rs` (64), and the
engine's routing of them (`engine.rs`); `teams-store/src/teams.rs` (89 regions).

Each crate needs its own tests (answers parsed from synthetic, marked fixtures; each write's wire form and target; the
engine's requests against the mock transport) until `scripts/coverage.sh -p <crate>` passes again. The floors stay as
they are.
