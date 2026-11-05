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
  `streamembed.aspx`), which streams it in DASH parts and never downloads it ([below](#streaming-a-recording)).
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

## Streaming a recording

A meeting recording's download is blocked for those who may only watch it (an attendee's view of another's meeting):
its item's download link (`download.aspx`, the one Graph's `shares/…/driveItem` hands out) answers 403 "Access
denied" with `x-ms-invokeapp: 1; RequireReadOnly`, whatever the token. The player streams it instead, as below; seen as
the organizer in a lab capture of 2026-11-05 and a lab probe the same day (the organizer's own recording, read in the
browser), the user's own account's debug log showing the download refused for an attendee.

- **The presentation**: `GET <site>/_api/v2.1/drives/<drive>/items/<item>/content?format=dash` with the site's
  SharePoint token, `prefer: noredirect` (the MPD itself rather than a redirect to it), `applymediaedits: true` and
  `enablecdn`. With `enablecdn: 1` (the web player's) the MPD's parts are on SharePoint's CDN
  (`_api_cached/v2.1/drives/<drive>/items/<item>/oneDrive.transcode?…`, read with `X-Authorization: Bearer <site
  token>`), each part encrypted whole in AES-128-CBC (`ContentProtection` `urn:mpeg:dash:sea:2012`), its key from
  `https://<region>-mediap.svc.ms/transform/VideoProtectionKey?…` or, as the web player takes it, from the
  `EncKey` of the `g_fileInfo` the embed page (`streamembed.aspx`, posted the site token as `access_token`) holds.
  With `enablecdn: 0` the MPD has no `ContentProtection` and its parts are the transform service's own, in the clear.
- **The MPD**: static, one period, `BaseURL` `https://<region>-mediap.svc.ms/transform/`; an audio adaptation set
  (`mp4a.40.2`, a representation `audcopy`) and a video one (`avc1`, `vcopy`: the recording's own streams, remuxed),
  each a `SegmentTemplate` with a timescale (16000 for both in the capture), `initialization` and `media` addresses
  (`videotranscode?provider=Spo&docId=<the item's address with a signature and its expiry>&…&part=initsegment` or
  `part=mediasegment&segmentTime=$Time$`, `track=audio|video`, `quality=$RepresentationID$`) and a `SegmentTimeline`
  of 6 s segments; `mediaPresentationDuration` the recording's length.
- **A part**: `GET` of its address, no token or cookie (its `docId` signs it): fragmented MP4, an `ftyp` and `moov`
  for an initialization segment, a `moof` (`tfhd` with the defaults, `tfdt`, `trun` of sizes) and its `mdat` for a
  media segment.

## What `ngteams` does

`teams_proto::recap` reads MCPS's answer (`MeetingContents`, `Service::Mcps`, the GTM `mcpService`) and the
transcript's address (`TranscriptLocation`: only a personal site's, read with that site's token, as the chat-file
upload's reads are); `teams_proto::transcript` reads the `WebVTT`. `teams_core::recap` makes both reads when the Recap
tab asks; nothing is stored. The tab (`teams_ui::recap_view`) shows the parts, plays the recording, and draws the
transcript beside it (no panes' pills: the transcript is the only pane). The recording is streamed
(`teams_proto::recording`, `teams_core::recap`): the presentation with `enablecdn: 0`, an MPD with `ContentProtection`
or parts off the transform service refused, each part read as the player (`teams_media::player::stream`) asks for it,
demultiplexed there and decoded by `GStreamer`; a recording that can't be streamed is played from its download link as
any shared video ([graph.md](graph.md#a-shared-files-content)) before the player says it can't be opened. Nothing of
it is stored. Share copies the recap link (without `fileUrl`, which only the item's own read gives), Watch in browser
opens the recording's sharing link (Stream's player). Left out, as no read backs them or they write: Share's
recipients (a grant and a post), Delete recap content and the transcript's Delete, "Go to Meeting Recaps" (a Teams
app), the attendance report (the organizer's reports), Notes (Loop), feedback, Download as .docx, and everything
Copilot: the AI summary, the Speakers chip's speaker insights and their "Upgrade Copilot" upsells (left out by the
user's choice).
