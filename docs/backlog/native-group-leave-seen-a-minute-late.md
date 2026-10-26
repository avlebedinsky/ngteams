---
worth: later
where: crates/teams-call/src/machine.rs, crates/teams-media/src/session.rs
added: 2026-10-26
---
# the others see a native client leave a group call about a minute after its `leave`

M3 Task 21's live run (`crates/teams-app/tests/call_live.rs`, `docs/m3-acceptance.md`): in the a–b–c group call c's
app hangs up (Ctrl+Shift+H); its `leave` goes to the conversation's `leave` link and is answered `204`, c's call ends
(`hung-up`) and its media says goodbye (RTCP BYE and DTLS close, `MediaSession::hang_up`). Yet the roster deliveries
a's and b's apps get keep c `active` with its call's media streams until 34–71 s later, when one entry turns c
`inactive` (70–71 s in three runs, 34 s in a fourth). a's and b's grids show c until then. When c's web
client left the same kind of call (`abc-call-mixed` 20261026T032038Z) the native caller stopped listing it 7 s after
the page's `leave`, whose body is the same as the native one (`participants.from`, `conversationTransactionEnd`,
`callTransactionEnd`, field for field).

So the service takes the native leg's end only once something times out (its media, or a keep-alive), not from the
`leave`. Unknown: what the web client does besides the `leave` that ends its leg at once (a request the browser fence
logged as allowed, an RTCP or STUN exchange on the media path, or the order of the media's close and the `leave`).
Next step: capture a web callee's hang-up in a group call with its media path (the WebRTC hooks' `rtc` events around
the click) and compare it with the native order of `leave` and `hang_up`.

Meanwhile the live test waits up to 180 s for the others to stop listing a leaver (`LEFT_SEEN`) and reports the time;
nothing else depends on it (c's own call ends at once, and its join afterwards works).
