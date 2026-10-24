# Calling: lab-to-lab calls in the web client

Status: 2026-10-24, M3 Task 5. Written from live runs of `proto-lab run ab-call` (a calls b from their 1:1 chat) and
`proto-lab run abc-call` (a calls the lab's a–b–c group chat), each step of `--call` (below), Chrome for Testing 155
headless, muted, with fake devices, the fence armed with the lab rule. The captures stay outside the repository; the
fixtures in `tests/fixtures/calling/chat/` are synthetic, shaped like them. Ids, hosts' numbers, tokens and addresses
below are placeholders. The echo test call is in [calling-echo.md](calling-echo.md); the fence's side of a call is in
[fence-targets.md](fence-targets.md#the-call-registry).

The code: the scenarios are `crates/proto-lab/src/scenario/ab_call.rs` and `abc_call.rs`; the start's row is
`chat_call_start` in `crates/teams-fence/src/contacting/call.rs`.

## The runs

| `--call` | Browsers | What is held back (labelled forced denies) | What happens |
|---|---|---|---|
| `start-held` | a | every `POST epconv` (`the call start, held back`) | a clicks the call button(s); nothing rings |
| `answer`, `decline` | a, each callee | everything each callee's page sends to the calling services and every Trouter delivery response it sends (`a callee's sends while ringing, held back`), and after its click under its own label | the callee never shows the incoming call (below), so there is nothing to click: these steps wait for Task 6 |
| `cancel` | a, each callee | the callees' sends as above; a's own join of the running call (`the caller's join of the running call, held back`) | a hangs up 8 s after it placed the call |
| `timeout` | a, each callee | as `cancel` | nobody acts for 50 s, then a hangs up if its call is still up |

Each callee opens the Calls app first: the client starts its calling stack only then (as for the test call,
[calling-echo.md](calling-echo.md#what-has-to-be-up-before-a-call)). a loads at the chat, opens the Calls app, and goes
back to the chat with the Chat app's button (in the same document: a deep link would reload the page and lose the
stack).

## Placing a call from a chat

A 1:1 chat's header shows one call button, the kind placed last (`default-chat-call-audio-button` or
`default-chat-call-video-button`), and the other in its options (`dropdown-calling-toggle-more-options-btn`, then
`chat-call-audio-button` / `chat-call-video-button`). A group chat's header shows **Meet now** (`audio-drop-in-button`,
which starts a meeting: M4) and its options (`audio-drop-in-more-options-button`) hold one item, **Audio call**
(`audio-drop-in-dropdown-menu-item-audio-button`); a group chat offers no video call to start. The group call asks for
a confirmation (`group-start-call-confirmation-btn`) before it rings everyone.

## The start

`POST https://api.flightproxy.teams.microsoft.com/api/v2/epconv` with no query, as the test call's
([calling-echo.md](calling-echo.md#the-call-step-by-step)), with these differences:

- `conversationRequest` has **no `scenario`** (the test call's is `UserInitiatedTestCall`);
- `participants.to` names the callees: `[{id: "8:orgid:<guid>", participantId: "<guid>"}]`, one for a 1:1 call, every
  other member for the group call (b and c);
- `groupChat` is `null` for a 1:1 call (it names only its callee) and `{threadId: "19:<id>@thread.v2", messageId:
  null}` for the group call; `groupContext`, `meetingInfo`, `meetingData`, `contentSharing`, `capabilities` stay
  `null`;
- `callInvitation.callModalities` is `["Audio"]` for a 1:1 call and `["Audio", "ScreenViewer"]` for the group call;
- `callInvitation.mediaContent` carries `requiredFeatures: "nonByPass"` in a 1:1 call and `mediaDescriptions`
  (`descriptions: [{mid, direction}]`, `requestId`) in the group call, besides the test call's `blob`, `contentType`
  (`application/sdp-ngc-1.0`), `clientLocation`, `applyChannelParameters` and `mediaLegId`.

**Video.** The header's video call posts the same start as its audio call: `callModalities` stays `["Audio"]`, and the
offer is the same (an audio line `sendrecv`, the main video line `recvonly`, an application sharing video line
`inactive`, a data line `sendrecv`). The camera comes later, on the running call (Task 6 captures it). The group
call's offer has one audio line and eleven video lines (six `sendrecv` main video lines, four `recvonly`, one
application sharing line) and a data line; its `mediaDescriptions` lists the `recvonly` ones.

**A failed start** (held back by the forced deny) is tried twice more by the client, about a second apart, with the
same body; then it shows a retry screen (`calling-retry-screen`, `calling-retry-cancelbutton`).

**The fence** maps it as a call in the chat it carries (`WriteKind::Call` on `Scope::Conversation`, the caller and
every callee added): the group chat `groupChat.threadId` names, or for a 1:1 start the 1:1 chat of the caller and its
one callee as the chat service names it. Only the captured shape maps; anything else on that URL is unreadable
([fence-targets.md](fence-targets.md#the-call-registry)). The start's answer is the conversation, as for the test
call, and registers the call.

## The invitation

The callee's calling stack's endpoint receives the invitation over Trouter: a delivery at the endpoint's root
(`POST /v4/f/<endpoint>/`), its body `{"evt": <n>, "gp": "<base64 JSON>"}`, `evt` 107 for a 1:1 call and 109 for a
group call. Decoded, `gp` holds:

- `callNotification`: `from` (the caller: its MRI, display name, endpoint, participant id, tenant, region), `to` (the
  callee, endpoint all zeros), `links` (`attach`, `progress` and `reject`, each
  `https://api.flightproxy.teams.microsoft.com/api/v2/ep/<cc host>/cc/v1/forked/<call>/<n>/i1/<n>/<op>?i=…`;
  `mediaAnswer: "cc://ma"`; `udpTransport: "udp://<relay>:3478/"`), `mediaContent` (the caller's offer in a 1:1
  call, `sdp-ngc-1.0`, with `requiredFeatures`; in a group call the call's mixer's, `sdp-ngc-0.5`, `fromMixer`,
  `originator: "mcGvc"`, `skipIceReinvite`), `udpKey` (`sessionKey`, `ticket`), the caller's capabilities;
  in a group call also `fromMixer`, `conversationType: "groupCall"`, `isDialOut: true`;
- `conversationInvitation`: the `conversationController` (the conversation's URL on the flight proxy),
  `isMultiParty`, `isBroadcast`; in a group call also `groupCallInitiator` (the caller's MRI) and `isHostless`;
- in a group call `groupChat` (`threadId`, `messageId: "0"`) and `meetingProperties`;
- `debugContent` (the call id, the call controller's instance, the start time).

The invitation names only the caller and the one callee it is addressed to; a group call's other callees appear in no
field of it (only the group chat's id does).

## What the callee sends while it rings

At once (within a fraction of a second) the callee's page answers the delivery (`3:::` with status 200) and posts the
invitation's `attach` link: `{"attach": {"requireMediaContent": false, "links": {"end": <a callback of its own>}, …},
"endpointCapabilities": …, "additionalActions": [{"name": "join", "url": <the conversationController>,
"waitForResponse": true, "input": {"conversationRequest": {"roster", "links"}, "participants": {"from": <the
callee>}, …}}]}`. Whether that `join` action joins the call or only attaches the callee's signaling to the
conversation isn't settled by the capture (Task 6 settles it before anything maps it).

With both held back:

- the service delivers the invitation again three times, about 2 s apart (the page answers each `404`), and the page
  retries `attach` twice, 4 s apart;
- **the page shows no incoming call**: a recorder of every control id naming a call, a ring, an answer, a decline, a
  toast or a notification saw none appear in the 1:1 or the group runs, only a missed-call row afterwards. So with
  these sends held back there is no answer or decline button to click. (What the captures establish is that no
  incoming call showed with these sends denied, not which of them the client waits for.)
- the call controller treats the callee as unreachable: in the group call the caller receives, at about 25 s,
  `addParticipantFailure` for each callee with `code 580` and, inside it, the call controller's `480` "Callee only
  had Trouter endpoints registered and PNH got delivery errors from all of them".

## The ring's ends

- **The caller cancels** (`cancel`): a's `leave` on the conversation, its `callTransactionEnd` with `code 487`,
  `phrase: "CallEndReasonLocalUserInitiated"` and `callQualityDiagnosticsInformation: {cancelationDuration: <s>}`
  (mapped as the call's `leave`); a receives its call's `conversationEnd` and call `end` deliveries. The callee, whose
  `attach` never went, receives nothing of the call after it (no end, no cancel): a missed call appears in its
  activity feed (`48:notifications`, `activityType: call`, `activitySubtype: missedCall`) and its call log
  (`48:calllogs`).
- **Nobody acts, 1:1** (`timeout`): about 22 s after the start a receives a `callProgress` at its `progress` callback,
  `status: "forwarded"`, `forwardingDestinationType: "voicemail"`, then an acceptance at its `acceptance` callback whose
  `acceptedBy` is the callee's MRI with `endpointType: "voicemail"`: the callee's own voicemail takes the call, and
  a's call connects to it (the registry binds the acceptance, its acknowledgement passes, a's follow-ups pass). The
  roster then lists the callee with a voicemail endpoint (`GraphCommunicationsClient-Voicemail`). a's tone is
  recorded into the callee's voicemail, which reaches only the callee's own mailbox.
- **Nobody acts, group call** (`timeout`): the call's mixer accepts a's leg at once (about 3 s after the start, an
  acceptance at a's callback, before anyone answered); at about 25 s each callee's `addParticipantFailure` arrives,
  then the call's `end` and `conversationEnd`; the group call ends without voicemail.
- **The caller's join after it left** (group calls): after its call ended or it hung up, a's page posts a join to the
  conversation controller itself (`POST …/ep/<conv host>/conv/<id>`, a `conversationRequest` with its roster and
  links), and retries it twice, 4 s apart. A join isn't mapped; the scenario holds it back by name.
- **Answered, declined, answered elsewhere**: not captured yet. The invitation names the `reject` link a decline goes
  to. Capturing them needs the callee's ring-time sends let through first (Task 6).

## Seen beside the call

- The caller's own call log's read horizon: `PUT …/v1/users/ME/conversations/48:calllogs/properties?name=consumptionhorizon`
  when a call ends; it reaches nobody, but no row carves it out, so it is denied and on the expected-deny list.
- Deliveries at the caller's endpoint root with `evt` 115 and 116 (no `gp`) during and after a group call: seen, not
  reviewed.
