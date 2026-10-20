---
worth: no
where: crates/proto-lab/src/arming.rs:618
added: 2026-10-20
---
# a containment kill can't tell a socket Chrome dropped just before it from its own

Accepted risk, kept so the next review of the containment doesn't re-argue it. When a target can't be armed and its
close isn't confirmed, `Shared::kill_to_contain` sends `SIGKILL` to the browser's process group and counts the
connection's end that follows as expected (`contained`), so the held target is recorded `browser_killed` and
contained, not breached. Before the signal it checks `Cdp::is_closed()`, which only says the connection's reader has
seen the connection end (`source.next()` gave `None` or an error, then `Replies::close`). If Chrome closes the devtools
socket (or the socket breaks) just before the kill and the reader hasn't read that EOF yet, the check passes, the
later end is taken as the kill's own, and `connection_lost` is never set, although the drop came first and could have
released the held target (the `drop` case of `browser.rs`'s
`a_held_target_runs_only_once_its_parent_s_auto_attach_ends_or_its_connection_drops`).

Why not fixed: once the kill is sent the connection always ends, and an EOF already on the wire can't be ordered
against the signal from userspace. The window is narrow: the fence never drops its own connection, and an unreadable
message makes the reader mark the connection closed while the writer still holds the socket, so the trigger needs
Chrome to close a live devtools socket, or the socket to break, in the instant before the `SIGKILL`. The doc on
`Shared::contain` and on the `contained` field states what is checked and names this window.

Reported by revmux on M1 Task 2, round 1 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`). Revisit only if
something can order the two, for example a Chrome-side record of why the socket closed.
