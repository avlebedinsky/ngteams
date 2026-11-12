---
worth: maybe
where: crates/teams-ui/src/profile_card.rs, crates/teams-proto/src/loki.rs
added: 2026-11-11
---
# Profile card: LinkedIn

Teams' profile card has a LinkedIn tab and section (the action "Find LinkedIn profile for {name}", "Several possible
matches for {name}", "Show profile matches"): LinkedIn profiles matched by the person's name, read through Loki's
`api/v2/linkedin/profiles` (`docs/protocol/loki.md`). The app's card (`crate::profile_card`) leaves it out: the
matches are a third party's (LinkedIn's) profiles of whoever shares the name, not the directory's, and showing them
needs the read's answer captured with a match (the lab accounts have none) and a decision on whether the app should
show third-party profiles at all.
