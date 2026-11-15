# Real-binary calls e2e: ring timing under a heavily loaded host

On a host far past its cores (load average 40–55 on 16 cores), `binary::calls::flows`'
`a_declined_a_cancelled_and_an_unanswered_call_are_missed_calls_in_the_chat` and
`a_call_to_someone_offline_disabled_or_deleted_ends_cleanly` fail now and then, and pass when rerun. Two shapes have been seen. b's client never got the incoming call within `WAIT`: its log since the mark held
only the chat's message deliveries. Or b's client ended a ringing call with `Failed("progress")`. The flow uses a
5-second ring timeout, so a starved client or node misses the ring window. `nodes`' ring-through-a-restart case once
had Answer refused with "the call can't do that now" under the same load. That may be the same timing.

The audio checks no longer flake under this load: they re-measure over later windows. These are signalling and ring
deadlines. Find out whether the client's ring progress or the push delivery is starved, or whether the test's deadlines
are too tight for such a host. Then either fix the client's timing, or scale the flow's ring timeout and `WAIT` for
QA runs. To reproduce, run `binary::calls` with a coturn, Postgres and Redis (`docs/server.md`) on a heavily loaded
host.
