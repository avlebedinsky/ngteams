# The own server's meetings: unused ones, and options a call must enforce

Two things the scheduling side leaves to later work (`crates/ngteams-server/src/calendar/meeting.rs`,
`options.rs`):

- The scheduling form makes an online meeting when it opens on a Teams meeting and the web client deletes it on
  closing (`PUT beta/me/calendarEvents/schedulingService/delete`, which the native client never sends and the
  server doesn't serve). A meeting no event took, and every Meet now meeting, stays with its chat. Nothing caps
  how many an account makes. Give `Scheduled` meetings without an event an age after which housekeeping removes
  them with their chat, and bound the meetings an account may make in a period.
- Meeting options (`AutoAdmittedUsers`, `PresenterOption`, `AllowMeetingChat`, `AttendeeRestrictions`) are kept
  per chat and read back. Nothing enforces them: the lobby, who presents and attendees' devices belong to the
  call's conversation, and `AllowMeetingChat: Disabled` should refuse an attendee's post in the meeting's chat.
  The server's call controller should read them when a conversation starts and tell a change in the
  conversation's next update (`meetingDetails.meetingCapability`).
