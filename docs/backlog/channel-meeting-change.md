---
worth: later
where: docs/protocol/calling.md (A channel's meeting: Schedule a meeting), crates/teams-ui/src/channel_view.rs (meeting_card)
added: 2026-11-03
---
# Change or cancel a channel's meeting from its card

Teams' card for a meeting scheduled in a channel offers its organizer Edit: the meeting's details, where it can be
changed (Send update) or cancelled (Cancel meeting, with a note). The native channel view draws the card without Edit:
the cancellation is built (`CancelChannelEvent`, `EventWrite::CancelInChannel`, used by `teams-headless … channel-schedule
--cancel`), the change is not.

What building it needs:

1. **The event read**: how the details read the group's event (its `iCalUid` on `v2.0/groups/smtp:<mailbox>/…`) wasn't
   looked at; a `team-manage` step run opening a card's Edit shows it.
2. **The change**: `PUT v2.0/groups/smtp:<mailbox>/calendars/events/<iCalUid>?isOnlineMeeting=<bool>` with
   `{"updateMessageFormat", "updatedCalendarEvent"}` (seen held back), a fence row on `Scope::TeamCalendar` beside the
   creation's and its contacting case, and a write in `teams-proto`.
3. **The card's Edit** in `channel_view.rs` for the organizer (`ScheduledMeeting::organized_by`), opening the scheduling
   form on the read event with the channel row, its Cancel meeting sending `EventWrite::CancelInChannel`.
4. The organizer's calendar copy reads `isOrganizer` `false`: its calendar card should still offer what an organizer's
   does.
