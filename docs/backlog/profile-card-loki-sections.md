---
worth: maybe
where: crates/teams-proto/src/loki.rs, crates/teams-core/src/profile_card.rs, crates/teams-ui/src/profile_card.rs
added: 2026-11-11
---
# Profile card: Loki's other sections

Browser Teams' card also reads `api/v1/peopleInsights/peoplecard` (insights), `api/v2/extendeduserinfo` (skills,
topics, interests, schools, birthday), `api/v1/extendeduserinfo/upacustomproperties` and `api/v2/graphql` (badges,
the org explorer) (`docs/protocol/loki.md`). The lab tenant answers all of them empty, so their shapes and how Teams
draws them are unknown; the app reads and shows none. Capture them from a tenant that fills them before building the
sections.
