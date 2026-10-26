---
worth: yes
where: crates/teams-call/src/machine.rs, crates/teams-media/src/session.rs (the answer to a renegotiation)
added: 2026-10-26
---
# b's web client renegotiates again after every answer of a's native client, until the service refuses (429)

`ab-call-native`'s first phase (a's native client calls, b's page answers, both turn their cameras on): once b's page
turned its camera on it posted `renegotiate`, a's native client answered (its video line `recvonly`, its camera not on
yet; the data line's `x-ssrc-range` a new one in every answer), the page acknowledged the answer and at once
renegotiated again, about every 400 ms. In the run of 2026-10-26 11:49Z this went on 22 s (53 renegotiations) until
the call controller ended the call: `remote 429 Too many media renegotiations have occurred on this call leg.`, so a
heard nothing of b and the run failed. The capture of 03:26Z (before the lab's fence changed) shows the same loop (18
renegotiations), ended by a glare (`491`) when the native camera turned on; the runs of 11:53Z and 11:58Z had none or
one renegotiation.

Unknown: what in the native answer makes the page offer again (the video line staying `recvonly` after the native
camera is on, the data line's ssrc changing per answer, or something the page reads from the answer's media
descriptions). Next step: compare the native answer with a web callee's answer to the same renegotiation in
`ab-call --call video`, line by line.
