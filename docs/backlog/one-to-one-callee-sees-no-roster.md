---
worth: yes
where: crates/teams-call/src/machine.rs, crates/teams-ui/src/call_view.rs, crates/teams-app/tests/call_live.rs
added: 2026-10-26
---
# a native 1:1 callee's call shows no participants, so `call_live`'s echo check can't read its microphone

`call_live`'s `one-to-one` case (`crates/teams-app/tests/call_live.rs`, `echo_check`) asserts that b's microphone heard
something at both ends of its window, read from b's view trace (`call-participant self`, `mic-sound`). That line is
traced only when the call's participant grid renders (`CallBar::grid` returns early while `view.participants` is
empty), and a native 1:1 callee's call never gets a participant: b's app logged `call 1: 0 participant(s)` for the
whole 1:1 call in both runs of 2026-10-26 12:15Z and 12:20Z, and in the acceptance run 23 (09:20Z) too, where the
check didn't exist yet. A group call's callee gets its participants (run 23's call 3: 1–2 participants). So the check
fails with `[None, None]` (no line at all), not a silent microphone.

Two parts:
- why the 1:1 callee's `rosterUpdate` deliveries (its `attach`'s roster subscription) give no participant: delivered
  and not read (`read::roster`, `Roster::apply`), or not delivered for a 1:1 call's subscription;
- the check: read b's own microphone level from a line that doesn't depend on the grid, or fail with a message that
  says the grid never showed.

The rest of the case (the echo margin itself: 38–39 dB measured, b playing a's speech at 0.0016–0.0019) ran.
