# Live captions

Observed in the commercial Teams web client on 2026-11-03, with two dedicated lab human participants and the
user-approved Microsoft built-in caption service. Raw tenant captures and Microsoft client code are kept outside
the repository. Tests use handwritten synthetic data.

More → Language and speech → Show live captions (Alt+Shift+C) opens an ephemeral pane. Hide closes it for the
current viewer; it does not stop other viewers' captions or create a saved transcript. The pane shows the service's
speaker name and updates an utterance in place until final. The built-in service is hidden from People.

Settings offers **Meeting spoken language**, caption styles, and top/bottom position. Choosing a spoken language
asks whether everyone is speaking it and warns that it changes the language for the meeting. This is distinct
from translated captions. These lab accounts exposed no translated-caption menu; translation entitlement must not
be inferred from the available spoken languages.

## Service startup

The connected conversation's `links.addParticipant` is posted with `participants.from` naming the local registered
endpoint and participant leg, and one `participants.to` entry for Microsoft's built-in commercial recording and
transcription identity (`CAPTION_SERVICE_MRI` in `teams-proto::captions`). This route differs from the ordinary human add's
`addParticipantAndModality`.

`participantInvitationData.botData` names `clientInfo: Teams-R4`, `consumerType: Teams`,
`mode: RecordingAndTranscription`, the current call chain ID, exact thread ID and recorder feature flags. Its
`initiatorUserToken` is an own-account delegated bearer for `RECORDER_AUDIENCE` in the same module, with
`access_recorder_service`. The invitation's HTTP Authorization uses the normal IC3 audience. Own Trouter callbacks
`conversation/addParticipantSuccess/` and `conversation/addParticipantFailure/` receive the asynchronous result.
The success callback's `participants` is an array of operation results, not a call roster.

A real `rosterUpdate` has a participant map. The exact service's active endpoint metadata supplies `commandUrl`,
`spokenLanguage`, and `processingModes.closedCaptions.state` (`Inactive` then `Active`). It can also name other
processing modes; captions never start those. The service joining causes a media fork and a data channel. Its initial offer explicitly has `newOffer: true` and
`fromMixer: true`, so the native media creates a fresh peer and preserves mute, camera and participants. Later offers
have `newOffer: false` and renegotiate the established peer; recreating it would lose the existing ICE connection.

## Commands

The allocated command URL has the observed shape:

```
https://api.flightproxy.teams.microsoft.com/api/v2/ep/<allocated>.callrecorder.teams.cloud.microsoft:<port>/v2/oncommand/<guid>
```

Only that validated Microsoft namespace and exact current-call roster-issued URL receives the recorder credential.
No arbitrary proxy backend, redirect, username, query or fragment is accepted. Headers use recorder Authorization,
the own Skype token in `X-Skypetoken`, the current chain ID and a fresh `requestid`.

The start body names the local `participantMri` and `participantLegId` (the start's `participants.from.participantId`,
not the registered endpoint ID), current UTC timestamp, `action: start`, `mode: transcription`,
`processingModes: [closedCaptions]`, and `participantSkypeToken: ""`. The empty body field is intentional in the
observed current client; the Skype credential is in the header. A successful start returned HTTP 200, followed by
Active service metadata and live text.

Changing spoken language uses the same authenticated command endpoint and identity fields, `action: setLanguage`,
and `actionParameters: {spokenLanguage: <code>, type: setLanguage}`. The current client also names the same mode and
processingModes. The native English (UK) change returned HTTP 200 and the service metadata updated to `en-gb`. The native route
verifies the same current call, actor, service link and credential audience immediately before sending it.

## Text stream

Data channel `main-channel`, framed with the existing Teams data-channel packet header, **data ID 3**. Unlike MCP
(data ID 18), the payload starts directly with JSON, without an MCP version byte:

```
{"version":"1.1","recognitionResults":[{
  "id":"synthetic-session/1", "userId":"<mri:a>",
  "displayName":"Lab Speaker", "text":"Synthetic caption.", "isFinal":false,
  "spokenLanguage":"en-us", "trackLanguage":"en-us",
  "timestampAudioSent":123456789, "duration":3000000, "temporaryStorageOnly":true
}]}
```

The service emits a new sequential result ID for each interim/final update. The speaker, `timestampAudioSent`
and track language identify the continuing utterance; a final result can shorten the earlier interim duration. It also sends `events`, including
`LanguageMismatch` with current/detected language and speaker ID. Unknown events/versions are ignored. Native
caption text stays in bounded memory, is cleared on hide/end, and is excluded from debug logs, traces and storage.
Future translated-caption support should preserve audio timestamp, track language and speaker when identifying utterances.

The app's own "Save live captions of every call" (off by default) is the one exception to keeping the text out of
storage: with it on, every call's captions are started as above for the account alone (the same invite and start, no
other request), their pane left hidden unless the user shows it, and each utterance's final result is kept in the
account's store as the call's transcript (`teams-store` schema 21, by chat; a rejoin within five minutes goes on in the
same one). Nothing of it goes back to Teams, and the logs and traces still carry no caption text.

## Native call binding and lab validation

The canonical `teams-call` tracker binds the exact local endpoint/participant leg, conversation's add link,
account-owned callbacks and current roster's validated recorder command URL. Caption requests also verify the
authenticated actor and credential audience/scopes. These are current-call and service identity checks; the native
client has no global lab roster admission or automatic hang-up based on changed participants. Before a live lab
run, the agent establishes the call's complete reach under [the lab-contact rule](../lab-rules.md). A failed
caption invite/command is reported in the caption pane and cannot fail the voice call.

## Local translation (not Teams')

Separately from Teams' own translation below, the native client can translate the captions it shows on the user's
own computer: "Translate to" in the caption settings, done by the installed Claude Code (`claude`) or Codex CLI
(`codex app-server`) with the model picked in Settings → Calls (by default Claude Haiku or GPT-6-Luna; Codex rejects a
plain `luna` model for a ChatGPT account). Nothing is sent
to Teams and the meeting is unaffected; the caption text goes to the chosen tool and its provider. How it schedules,
keeps the call's context and bounds it is in `crates/teams-ui/src/translate.rs` and its `session` module.

## Translated captions: reference and verification prerequisite

Microsoft's [live caption guidance](https://support.microsoft.com/en-gb/teams/meetings/use-live-captions-in-microsoft-teams-meetings)
requires Teams Premium or Microsoft 365 Copilot for the organizer (making translation available to the meeting),
or for an individual viewer (making it available to that viewer). The separate
[multilingual meeting guidance](https://support.microsoft.com/en-us/teams/meetings/multilingual-speech-recognition-in-microsoft-teams)
confirms that **Translate to** can be available without enabling multilingual speech recognition. A spoken-language
picker alone does not establish translation entitlement. Local web authentication state for the dedicated lab
accounts reports `licenseDetails.isTPManagement=false` and `licenseDetails.isCopilot=false`; these are the same
flags read by the first-party calling implementation. The captured lab caption settings expose no **Translate to**.
An eligible dedicated lab account is needed to verify a successful translated stream; no translation controls have
been added on the assumption that source-language captions demonstrate this capability.

Static inspection of the first-party Teams web calling implementation on 2026-11-04 establishes the following
request and track behavior. These are **source observations, not successful native or live translation captures**:

- For an ordinary participant, requesting a subtitle track uses the existing recorder command endpoint, actor,
  participant leg and credentials, with `action: setLanguage` and
  `actionParameters: {applyForAllParticipants: false, subtitleLanguages: [<target>], type: setLanguage}`.
  The mode remains `transcription`, with only `closedCaptions` in `processingModes`. A streaming/broadcast path uses
  different semantics and must not be inferred from this participant path.
- Translation-off in the reference is a local selection: it selects the original caption track without stopping
  the shared caption service. The reference's local off marker is not a service language code. Hiding captions and
  changing the meeting spoken language remain separate operations.
- The service can advertise `textTracks` in its endpoint metadata. A track has a `source` and `lang`; a track with
  `translations` advertises separate language choices. The reference treats machine translation as source `Mt`,
  distinct from recognition source `Ai`, captioner source `Cart`, and broadcast source `Cea708`.
- On data ID 3, translated entries arrive in `textTracks`, alongside the original `recognitionResults` array.
  The reference preserves the entry's speaker, text, final/interim status, timing, source and track language.
  The existing native parser reads only `recognitionResults`; parsing another array without choosing its source
  and target would mix original and translated speech in one pane.
- A successful language command changes the reference's selected translation language. A command failure does not
  establish that the requested language is active. A verified implementation must retain the previous selection,
  distinguish policy/license rejection from network failure, and keep the voice call and original captions usable.

Before implementing the user-facing path, capture an eligible lab meeting's language options, exact target codes,
command result, advertised tracks and interleaved original/translated interim/final entries. Verify enable, target
change, local off, hide/reopen and call end against that stream. Preserve speaker and audio timestamp independently
for each source/track, and retain the caption text's bounded, memory-only lifecycle. No third-party translation
service or additional bot is needed or authorized by this protocol.
