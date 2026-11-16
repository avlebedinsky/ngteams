# Real-binary calls e2e: ring timing and audio levels under a heavily loaded host

On a host far past its cores (load average 40–55 on 16 cores), `binary::calls::flows`'
`a_declined_a_cancelled_and_an_unanswered_call_are_missed_calls_in_the_chat` and
`a_call_to_someone_offline_disabled_or_deleted_ends_cleanly` fail now and then, and pass when rerun. b's client
never got the incoming call within `WAIT`: its log since the mark held only the chat's message deliveries. The flow
uses a 5-second ring timeout, so a starved client or node misses the ring window. A second shape was seen before #371:
b's client ended a ringing call with `Failed("progress")`. That occurs when the ring's end reaches b's client before
its `progress` goes. Since #371, the client then ends the ring as the end says (`Outcome::RingOver`). If
`Failed("progress")` occurs again, it has a different cause. `nodes`' ring-through-a-restart case once
had Answer refused with "the call can't do that now" under the same load. That may be the same timing.

The audio checks re-measure over up to 3 later windows, and still flake now and then under this load. In one run at
load 40, `own_relay`'s first call, `accounts`' own call and the credential-expiry call's last check heard the tone,
but weakly: 0.06–0.18 after every re-measure, below `HEARD` (0.25). All three passed when rerun. Find out whether a
starved client really drops the tone that long, or whether `HEARD` and the windows are too strict for such a host.

The missing incoming call is a signalling and ring deadline failure. Find out whether the push delivery is starved, or
whether the test's deadlines are too tight for such a host. Then either fix the client's timing, or scale the flow's ring timeout and `WAIT` for
QA runs. To reproduce, run `binary::calls` with a coturn, Postgres and Redis (`docs/server.md`) on a heavily loaded
host.
