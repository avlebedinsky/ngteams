---
worth: maybe
where: crates/ngteams-server/src/people, crates/ngteams-server/src/admin/api.rs
added: 2026-11-15
---
# Own server: profiles and pictures are set only on the command line

An account's profile fields, its manager, its out-of-office flag and its picture are set by the operator with
`ngteams-server profile …` (`docs/server.md`, "People"). The admin API has no route for them, and an account can't
change its own picture or turn its automatic replies on from ngteams.

`crate::people::update_profile` and `set_picture` are the library calls a route would use. Decide who may change
what (an admin everything; an account its own picture and out-of-office flag), add the admin API routes and the
client's UI, and push a changed `isOutOfOffice` to the account's presence watchers (today the next read or change
carries it).
