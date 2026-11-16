# Calls: a late `progress` that the Microsoft service refuses fails the ring

A callee's client sends `progress` right after its `attach`. If the caller cancels at that moment, the order can be
one of two:

- The ring's end reaches the client before the `progress` goes. The client then does not send it, and the ring ends
  as the end says (#371, `Outcome::RingOver`).
- The `progress` goes first, and the service has ended the ring already. Our own server answers such a `progress`
  with 200 (0e9e439d), so the ring ends as the end says. We do not know how the Microsoft service answers it. If it
  answers with an error status, the machine ends the ring as `Failed("progress")` ("Call failed"), and the end that
  comes after it finds no call to end.

To do: capture a cancel at the moment of the callee's `progress` on a lab call between two Microsoft lab accounts.
If the service answers that `progress` with an error status, do not fail the ring at once on that answer: wait a
short time for the ring's end, and fail it only if no end comes. The machine's `Step::Ringing` failure in
`crates/teams-call/src/machine.rs` (`Calls::on_outcome`) is the place.
