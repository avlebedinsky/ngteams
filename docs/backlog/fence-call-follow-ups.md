---
worth: yes
where: crates/teams-fence/src/contacting/call.rs, crates/teams-fence/src/decision.rs
added: 2026-10-20
---
# M3 maps the call writes: lab-only calls started and answered, the follow-ups by a call registry

M0 Task 18: `proto-lab run echo-call` places the web client's test call and the fence allows the call start (a new
call of lab account a and the approved echo service). Every later request lands on the call that now exists, is no
row of the calling family in `teams_fence::contacting`, and is denied: `PUT …/conv/<id>/updateEndpointMetadata`,
`POST …/cc/v1/mcProxy/<call>/…/applyChannelParameters`, `POST …/cc/v1/callAcceptance/<call>/…/acknowledge` (and the
same answer over Trouter, a delivery response with a body), **`POST …/conv/<id>/add`**, which is how the client brings
the echo bot in, and `POST …/conv/<id>/leave`. The leg reaches Teams' media controller, the echo never joins, no tone
comes back (`docs/protocol/calling-echo.md`), and `echo-call` exits 1 on the denied follow-ups. The native client of
M0 Task 19 has the same writes to make: its `add` is `teams_proto::AddToCall` (an invite on `Scope::Call`), denied as
partial today, and `echo_call::call::start` sends nothing.

## The path (the user's decisions of 2026-10-23)

The hard rules (CLAUDE.md, "Calls go only to the lab, both ways") settle what is allowed; mapping it is M3's. A call is
a contacting write whose participants are its callees, its caller and whoever it adds.

- **Starting**: allowed when every callee is a lab account or the echo service (`Scope::Call` with its `to[]`, or
  `Scope::New` as the test call's start is today), in the shape the call start's capture shows. The calling stack's
  own Trouter endpoint (`SkypeSpacesWeb`) is registered freely: the registrar is no contacting service.
- **Answering**: allowed when the incoming call's invitation (the Trouter delivery) names a caller and participants
  that are all lab accounts or the echo service. Answering is the `acceptance` request and, where the flow uses one,
  a typed delivery response for that call (`teams-proto` adds the frame type; the browser's frame rule lets that one
  response through). Every other incoming call is ignored, never answered.
- **Follow-ups** (`updateEndpointMetadata`, `applyChannelParameters`, `acknowledge`, `mediaRenegotiation`, hold,
  `updateCallState`, `controlVideoStreaming`, screen sharing, `leave`): allowed on a call in the fence's **call
  registry**, the calls this run's fence permitted a start or an answer for, keyed by the conversation and call ids
  the start's answer and the acceptance bind together. `add` is judged per invitee like a start. A call not in the
  registry is `Partial` and denied.
- **No authoritative call roster is read, and the roster isn't watched.** The fence trusts the calls it permitted
  itself. A participant from outside the lab could appear only through a tenant policy bot, forwarding on the callee's
  account or a transfer, none of which the lab expects.
- **Media** (TURN, ICE, DTLS-SRTP, over UDP or TCP to the relay hosts) isn't classified: it carries a call the
  signaling fence permitted.

So M3 adds, each a row of the calling family and a test (no new mechanism): the call start to lab callees, the
`acceptance` and its typed delivery response, the follow-ups above on a registered call, `add` per invitee; and the
registry in `teams-fence`, which the browser fence and the native transport share like the classifier. Then
`echo-call` runs until the tone comes back.

## What was tried before

Under the fence of M0–M2, which allowed a write only after a fresh, complete read of its target's members, there was
no such read for a call. Decided with Codex (2026-10-20, `task-18-calling-fence` discussion): a registry alone wasn't
enough then, because nothing proved the call's membership could change only through this session's fenced writes. The
conversation's `getAllParticipants` (`POST …/conv/<id>/getAllParticipants?i=&e=`, body `{from, scope, scenario: 0}`,
the page's `X-Skypetoken`) was tried as that read (`call-follow-ups` discussion): every `call` read answered `200
{"participants":[]}` while a was in the call, which is no membership, so nothing was committed. The call controller's
operations (`acknowledge`, `applyChannelParameters`) also need their call id bound to the conversation, and that
binding shows only in a delivered `conversationUpdate` (or the acceptance callback): the registry's keys have to come
from there.
