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
(`docs/protocol/calling-echo.md`). The native client of Task 19 has the same writes to make: its `add` is
`teams_proto::AddToCall` (an invite on `Scope::Call`), denied as partial today, and `echo_call::call::start` sends
nothing until this is resolved.

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

**Tried 2026-10-20: `getAllParticipants` isn't that read.** Settled with Codex (`call-follow-ups` discussion): a
sealed call source in `teams-fence` sends it itself (`POST …/conv/<id>/getAllParticipants?i=&e=`, body `{from, scope,
scenario: 0}`, the page's `X-Skypetoken`), reads both `call` and `lobby`, and counts an answer as complete only when
it is strict, unpaged, unmasked, small and lists the requester. The follow-ups were mapped behind it (`add` only for
the `lab.toml` echo, with its callbacks at an endpoint the fence let this run register). In one live diagnostic run,
with writes still denied, every `call` read answered `200 {"participants":[]}` while a was in the call. That is not a
membership, so everything stayed denied and the code wasn't committed. Codex also held two more conditions before any
read could grant a permit: evidence that the read can't silently leave participants out (an independent check, as
the 1:1 chat's id gives), and provenance that the conversation is the test call this page started (the start's
answer). The call controller's operations (`acknowledge`, `applyChannelParameters`) also need their call id bound to
the conversation, and that binding shows only in a delivered `conversationUpdate`.

Then map the follow-ups (strict bodies; `add` as an invite with its `to[].id` added; `leave`, acknowledgements and
channel parameters as call writes on the call's scope, the call id correlated with the conversation through the start's
answer and the acceptance callback) and run `echo-call` until the tone comes back.
