---
worth: yes
where: crates/proto-lab/src/calling.rs:636
added: 2026-10-20
---
# the fence can't judge writes on an existing call, so the echo never joins

M0 Task 18: `proto-lab run echo-call` places the web client's test call and the fence allows the call start (a new
call of lab account a and the approved echo service). Every later request lands on the call that now exists and stays
unknown, so the fence denies it: `PUT …/conv/<id>/updateEndpointMetadata`, `POST …/cc/v1/mcProxy/<call>/…/
applyChannelParameters`, `POST …/cc/v1/callAcceptance/<call>/…/acknowledge` (and the same answer over Trouter, a
delivery response with a body), **`POST …/conv/<id>/add`**, which is how the client brings the echo bot in, and
`POST …/conv/<id>/leave`. The leg reaches Teams' media controller, the echo never joins, no tone comes back
(`docs/protocol/calling-echo.md`). The native client of Task 19 has the same writes to make.

The fence allows a write only after a fresh, complete read of its target's members (`MembershipSource`,
`crates/teams-fence/src/decision.rs`); for a `Scope::Call` there is no such read. Decided with Codex (2026-10-20,
`task-18-calling-fence` discussion): a registry that remembers the participants the fence permitted at the start and on
`add`s is not enough by itself, because nothing yet proves the call's membership can change only through this
session's fenced writes (the controller or the invited bot could change it, roster callbacks may be late or missed).
Acceptable ways forward, any one of them:

- an authoritative read: the conversation's `getAllParticipants` link (on the conversation service, in the call
  start's answer; the client's `getAllParticipantsAsync` takes a scope `call`/`lobby`) — method and answer not
  captured yet; a `LiveMembershipSource` for `Scope::Call` would read it with the page's skypetoken, like the chat read;
- protocol evidence that a test call's membership is fixed except for the caller's own `add`;
- an ordered, complete roster stream with evidence of its guarantees.

Then map the follow-ups (strict bodies; `add` as an invite with its `to[].id` added; `leave`, acknowledgements and
channel parameters as call writes on the call's scope, the call id correlated with the conversation through the start's
answer and the acceptance callback) and run `echo-call` until the tone comes back.
