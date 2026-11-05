# A meeting's recap

How the web client's Recap tab reads what a meeting left, and how `ngteams` reads it. Seen in a lab capture of
2026-11-05 (`meeting-steps` as lab account a: two "Meet now" meetings with nobody invited, one recorded twice with
transcription, one transcribed and not recorded; the Recap, its parts, its transcript, Share's Copy link, Watch in
browser and "Download as .vtt"). No value from it is copied here; the ids below are placeholders.

## What the web client shows

- **The meeting chat's header** has tabs after the meeting's name: Chat, Shared, Recap, then the meeting's apps
  (Attendance, Breakout Rooms, Q&A, Meeting Whiteboard, "+1" for more) and "+" to add one; on the right Join, the
  participants' count, Search, chat details and "…".
- **The chat** shows, once the meeting ended, a recap card: the meeting's name and the span of its first recording,
  Share and "View recap", its contents (a Transcript chip) and the recording's thumbnail ("Saving…" while it is
  processed, "2 recordings" with two, "Expires in 120 days"). A recording's own message ("Recording has been saved to
  <organizer>'s OneDrive…") and the start and stop lines stay in the chat. "View recap" opens the Recap tab.
- **The Recap tab**: a box with the meeting's day and span (the earliest start to the latest end of what it left) and
  its menu of the meeting's sessions, the one shown checked; "Part 1", "Part 2" when it was recorded more than once;
  Share (a dialog: recipients to share with and grant access to, a message, Copy link, Share), Watch in browser (the
  recording in Stream) and "…" (Go to Meeting Recaps, Delete recap content). Under it the recording's player, the
  Speakers chip and, for an account without Microsoft 365 Copilot, an upsell ("Upgrade to Microsoft 365 Copilot for AI
  speaker insights", Upgrade Copilot); beside it "Shared files" (the files of the chat, and the organizer's attendance
  report) and the panes: AI summary (for such an account the upsell "Upgrade your Copilot plan for AI summaries") and
  Transcript, plus Notes when notes were taken.
- **The transcript pane**: Search, Download (as .docx or .vtt) and, for the organizer, Delete; "AI-generated content
  may be incorrect", "<name> started transcription", each speaker's turn (their picture, name and time into the
  recording) with its paragraphs, and "<name> stopped transcription" for a transcript alone. While the recording plays
  the paragraph spoken is highlighted; a paragraph clicked plays from it. A search shows "1/3" with up and down
  arrows, each place its words stand counted, the one shown in orange; "Sync to video" brings the pane back to the
  playing paragraph. "Is this transcript useful?" asks for feedback.
- **A meeting transcribed and not recorded**: no player, no parts, no Watch in browser; the files and panes take the
  whole width.
- **Upgrade Copilot** opens Microsoft 365's offer page
  (`offer.m365.cloud.microsoft/launchpad/v1/launchpad.metaos.html?partnerId=teamsmetaoswebcommercial&experienceId=meetingnotes`)
  in a dialog of the client's.

## The reads

- **What the meeting left**: `GET <mcpService>/contents/?threadId=<the meeting's chat>` (the trailing slash as sent;
  the chat's recap card's read adds `&organizerId=<oid>`), the base the region's GTM `mcpService`
  (`https://teams.microsoft.com/api/mcps/<region>`), with a token for the meeting-artifacts app
  `6bc3b958-689b-49f5-9006-36d165f30e00`. The answer is `{changeKey, resources, invalidResources, deletedResources,
  countOfFilteredContentsByChatHistory, paginationInfo: {hasNextPage}}`; a resource is
  `{id, type, location, metadata, subType, addedBy, additionDateTime}`. `type` was `Recording` (its `location` the
  recording's sharing link, `https://<tenant>-my.sharepoint.com/:v:/g/personal/<organizer>/<token>`) and
  `TranscriptV2` (its `location`
  `https://<tenant>-my.sharepoint.com/personal/<organizer>/_api/v2.1/drives/<drive>/items/<item>/versions/current/media/transcripts/<id>/content`,
  and its `metadata.webUrl` the item's sharing link). `metadata` holds `threadId`, `callId`, `driveId`, `driveItemId`,
  `startTime` and `endTime` (.NET ticks), `organizerId`, `ownerTenantId`, `meetingJoinUrl`, `transcriptId` and more.
  A recording and its transcript share the item; a meeting transcribed alone has a transcript on an item of its own
  and no recording. A recording still processing isn't listed yet.
- **The recording** is played by the web client in SharePoint's embedded player (Graph `POST
  drives/<drive>/items/<item>/preview`, the site's `_api/v2.0`/`v2.1` item reads with a SharePoint token,
  `streamembed.aspx`). `ngteams` plays it as any shared video: Graph's item by the sharing link
  (`shares/u!<link>/driveItem`, its download link) and the bytes at that link ([graph.md](graph.md#a-shared-files-content)).
- **The transcript's words**: the player reads `…/_api_cached/v2.1/drives/<drive>/items/<item>/cdnmedia/transcripts?…`
  (signed query, a compressed JSON); "Download as .vtt" reads
  `GET https://<tenant>-my.sharepoint.com/personal/<organizer>/_api/v2.1/drives/<drive>/items/<item>/media/transcripts/<id>/streamContent?is=1&applymediaedits=false`
  with a SharePoint token (audience the site's origin; the token's `aud` SharePoint's
  first-party application id, not the URL), answered `text/vtt`. Its cues are named `<utterance>-<n>`: the cues of one
  utterance are one paragraph of the pane, a speaker's utterances in a row one turn (`<v Name>` names the speaker).
- **The license**: the web client reads Graph's `users/<oid>/licenseDetails` to choose the Copilot upsell; both
  accounts it was seen with had no Copilot plan.
- **Share → Copy link** copies
  `https://teams.microsoft.com/l/meetingrecap?driveId&driveItemId&sitePath=<sharing link>&fileUrl=<the file's address>?web=1&threadId&organizerId&tenantId&callId&threadType=meeting&meetingType&subType=RecapSharingLink_RecapCore&recapType=RecordingAndTranscript`.
  **Watch in browser** opens the file's address with `?web=1&referrer=Teams.TEAMS-WEB&referrerScenario=RecapOpenInStreamButton.view`.

## What `ngteams` does

`teams_proto::recap` reads MCPS's answer (`MeetingContents`, `Service::Mcps`, the GTM `mcpService`) and the
transcript's address (`TranscriptLocation`: only a personal site's, read with that site's token, as the chat-file
upload's reads are); `teams_proto::transcript` reads the `WebVTT`. `teams_core::recap` makes both reads when the Recap
tab asks; nothing is stored. The tab (`teams_ui::recap_view`) shows the parts, plays the recording, and draws the
transcript beside it (no panes' pills: the transcript is the only pane); Share copies the recap link (without `fileUrl`, which only the item's own read gives), Watch in browser
opens the recording's sharing link (Stream's player). Left out, as no read backs them or they write: Share's
recipients (a grant and a post), Delete recap content and the transcript's Delete, "Go to Meeting Recaps" (a Teams
app), the attendance report (the organizer's reports), Notes (Loop), feedback, Download as .docx, and everything
Copilot: the AI summary, the Speakers chip's speaker insights and their "Upgrade Copilot" upsells (left out by the
user's choice).
