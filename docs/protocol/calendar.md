# The calendar (read)

Status: 2026-10-30. Written from the web client's captures of its calendar read (`ab-chat`, `load`, 2026-10-20 to
2026-10-24: method, path, query names and value forms, header names, statuses and the answer's key paths only) and from
the field names and value lists the web client's code reads an event by (the desktop client's bundle the lab keeps,
read locally; nothing of it copied). The lab accounts' calendars are empty, so every captured answer held no event: an
event's value forms below are the bundle's, not a capture's, and the native reader takes them leniently. The native
client's read is in `crates/teams-proto/src/calendar.rs` (`CalendarView`), its use in
`crates/teams-core/src/calendar.rs`, and the view in `crates/teams-ui/src/{calendar.rs,calendar_view.rs}`.

## Where the web client reads it

The web client reads the account's calendar from the middle tier (`Service::MiddleTier`, the region's GTM
`middleTier`, `https://<AFD host>/api/mt/part/<partition>`), with the account's token for
`https://api.spaces.skype.com`:

- `GET v2.0/me/calendars/default/calendarView?StartDate=<ISO>&EndDate=<ISO>&shouldDecryptData=true`, the query in that
  order, the window's ends in UTC with milliseconds and `Z` (`2026-10-24T21:00:00.000Z`: local midnight of the first
  and of the day after the last day shown). Its captured headers besides the token: `x-ms-client-caller`
  (`calling-cal-adapter`), `x-ms-client-requesttype`, `x-ms-client-type` (`web`), `x-ms-client-version`,
  `x-ms-migration: True`. The captures show windows of three and six days.
- It answered `200` with `{"type": "Microsoft.SkypeSpaces.MiddleTier.Models.CalendarEvent", "value": []}`: every event
  of the window in `value`, no paging key.

The bundle names more of the same service's calendar paths (an event by id, by its iCal uid, by its join link, a
group's or a connected account's calendar view, the scheduling form's reads and an event's creation, forward and
import); none of them was captured, and the native client uses none.

## An event

The fields the web client selects for an event, with what the native client reads (the bundle's value lists for the
enums):

| Field | What it is | Read as |
|---|---|---|
| `objectId` | the event's id | its id; an event without one is skipped |
| `subject` | its title (a cancelled meeting's usually renamed `Canceled: …` by the service) | text, empty when absent |
| `startTime`, `endTime` | its start and end, an ISO date and time | with an offset: an instant; without one: a local time, as a JavaScript `Date` parses it; an event whose times don't read is skipped |
| `isAllDayEvent` | all day | a flag |
| `eventType` | `Single`, `Occurrence`, `Exception`, `RecurringMaster` | an occurrence or exception is drawn with the recurring mark; a master is left out (its occurrences come on their own) |
| `isCancelled` | cancelled | a flag: the event is drawn outlined |
| `isOnlineMeeting`, `skypeTeamsMeetingUrl`, `joinOnlineMeetingUrl`, `shortOnlineMeetingJoinUrl` | a Teams meeting and its join link | the first `https` link of the two first fields |
| `skypeTeamsData` | a JSON object as a string; its `cid` is the meeting's chat (`19:meeting_…@thread.v2`) | the meeting's chat, else the thread id the join link's path names (`/l/meetup-join/<thread, percent-encoded>/…`) |
| `organizerName`, `organizerAddress`, `isOrganizer` | the organizer | text and a flag |
| `myResponseType` | `None`, `Organizer`, `Accepted`, `Tentative`, `Declined`, `NotResponded` | a declined meeting isn't shown (as in Teams); tentative and not answered are drawn with a striped edge |
| `showAs` | `Free`, `Tentative`, `Busy`, `OOF`, `WorkingElsewhere`, `Unknown` | free and tentative drawn striped |
| `location` | where | text (or an object's `displayName`) |
| `attendees` | who is invited | each one's name and address, when readable |
| `isPrivate`, `bodyPreview`, `onlineMeetingConferenceId` | private, the start of its text, the dial-in id | read; the text is shown in the event's card |

Other selected fields (`categories`, `cleanGlobalObjectId`, `iCalUid`, `eventTimeZone`, `lastModifiedTime`,
`schedulingServiceUpdateUrl`, `seriesMasterId`, `meetingTemplateId`, the dial-in numbers, …) aren't read.

## What the native client does with it

- **Reads only**: one read per window a view shows, when it changes, on Refresh and every five minutes while the
  calendar is shown; nothing is stored. A window wider than 62 days is never asked for.
- **Joining**: a meeting's Join goes through the call machinery's join of a running call in its chat
  (`docs/protocol/calling.md`, "Joining a running call"), offered only once an invitation named a running call in the
  meeting's chat. Joining a meeting nobody's call has rung for yet (its conversation looked up by its link) was never
  captured, nor were a meeting's creation (the calendar's New) and its lookup by meeting id and passcode ("Join with an
  ID"): the calendar says so instead of sending them.
