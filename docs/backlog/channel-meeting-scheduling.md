---
worth: soon
where: docs/protocol/calling.md (A channel's meeting: Schedule a meeting), crates/teams-ui/src/shell.rs (meet_now_button)
added: 2026-11-03
---
# Schedule a meeting from a channel's Meet now menu

Teams' channel header has Meet now with a caret whose menu holds Meet now and Schedule a meeting. The native header
offers Meet now only. Schedule a meeting opens the scheduling form with the channel as a row of its own, and its Send
posts the event to the team's group calendar (`POST …/v2.0/groups/smtp:<team mailbox>/calendars/default/events`, the
team's mailbox an attendee, `skypeTeamsData.cid` the channel). The capture with the event held back is written up in
`docs/protocol/calling.md`.

What building it needs:

1. **A fence row** for the group calendar's event creation: a `Meeting` write reaching the whole team through its
   mailbox, judged by the channel's team read fresh (`Scope::Team` of the channel), a contacting row in
   `crates/teams-fence/src/contacting*` and its case in `crates/teams-fence/tests/contacting.rs`. Its later change,
   cancel and delete on the group calendar were not captured.
2. **The write** in `teams-proto` (the creation's keys at the top level, not under `calendarEvent`) and an event
   draft that names a channel in `teams-core`'s calendar.
3. **The form**: the channel row (and "Send personal invites") in `crates/teams-ui/src/event_form.rs`, opened from the
   menu's second item with the channel filled in.
4. **A live run** in a team of the lab's own, the event created, read back from the channel and cancelled.
