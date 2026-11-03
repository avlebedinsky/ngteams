---
worth: soon
where: docs/protocol/calling.md (The lobby), crates/teams-call/src/machine.rs (renegotiated), crates/teams-fence/src/calls.rs
added: 2026-11-03
---
# A native joiner let in from a meeting's lobby fails a second after it is admitted

The lobby works both ways natively (2026-11-03, `teams-headless a join-meeting --admit` and `b join-meeting`): b waits
(`lobby`), a is told b waits, a's admit is answered `202`. What follows the admit on b's side isn't handled yet:

1. The mixer's first offer to b is a session of its own: every line numbered anew from `1` (the lobby's one audio line
   was `0`) and its RTP header extensions renumbered, which the lobby's peer connection refuses ("The order of m-lines
   in subsequent offer doesn't match", then "RTP extension ID reassignment not supported"). `teams-core` now sets the
   call's media up again for that offer and answers it (`Calling::rebuild`; its answer was taken, `202`).
2. About a second later another renegotiation comes, its `debugContent` naming `IsRetargetNegotiationPending`: the
   call is moved to another call controller (retargeted). The call registry binds its answer link to no call (it names
   a call other than the one registered), so the fence observes `Nothing`, and the machine fails the call there
   (`renegotiated`: a renegotiation the fence didn't read fails it).

To do: capture the joiner's side of an admission (a lab browser as the joiner: b's `meeting-steps` run failed its
enforcement probe three times on 2026-11-03, "compose box holds 1 characters"), to see the retarget's whole exchange
(the `retargetCompletion` callback, the new call's acceptance or links), then let the registry follow a retarget of a
call it registered and the machine answer it.
