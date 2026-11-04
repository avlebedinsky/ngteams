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

## Native call binding and lab validation

The canonical `teams-call` tracker binds the exact local endpoint/participant leg, conversation's add link,
account-owned callbacks and current roster's validated recorder command URL. Caption requests also verify the
authenticated actor and credential audience/scopes. These are current-call and service identity checks; the native
client has no global lab roster admission or automatic hang-up based on changed participants. Before a live lab
run, the agent establishes the call's complete reach under [the lab-contact rule](../lab-rules.md). A failed
caption invite/command is reported in the caption pane and cannot fail the voice call.
