# The own server's calendar: what Exchange tells by mail

The own server sends no mail, so some of what Exchange tells an event's people has no carrier yet
(`crates/ngteams-server/src/calendar/`, `docs/server.md`, "The calendar and meetings"):

- An account taken off an event by its organizer's change loses the event at once. Exchange sends it a
  cancellation, and its calendar shows the event cancelled until it is taken off.
- An answer's note (`respond`'s `comment`) and `sendResponse` are accepted and dropped. The organizer reads the
  answer on the event; nothing tells it that an answer came.
- A reminder (`isReminderSet`, `reminderMinutesBeforeStart`) is kept and read back; nothing fires at its time.
- An attendee that is no account of the server is refused (400 `UnknownAttendee`), so an event can't name a
  guest by address.

Decide each with the client's view of it in hand: the first two fit the activity feed (a "canceled" item exists;
an "accepted" one would need a kind the client reads), a reminder is the client's to fire from the stored event,
and guests need a product decision (list them without an invitation, or keep refusing).
