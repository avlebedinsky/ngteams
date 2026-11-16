# Group call: a callee's answer ended `Failed("accept")` once after a media node restart

In QA of the server's mixer node (Linux, round 1), `binary::calls::group::a_killed_media_node_ends_the_call_for_everyone_and_a_new_one_starts_once_a_node_runs_again`
failed once in 6 runs. The media node was killed, the call ended for all, a new node started, and a started a new call
in the same chat. b answered and connected. Then c's answer ended `Failed("accept")`. The layout was `--role api` +
`--role media` on Postgres and Redis, with the loopback coturn. That run took 49.8 s; its passing runs took 28–37 s,
so the host was loaded.

What was run after that, with the nodes' logs at `info` (`NGTS_QA_SERVER_LOG=info`, commands as in QA's
`commands.md`): the same test 25 times alone on an idle host (24–34 s each), all passed. Also once with the ring
timeout cut to 3 s and once to 1 s: both passed, so a ring that runs out before the answer is not the cause.

What the failed run's api log tells: only the two `503 "no media node takes it"` lines of the start with no node, which
every passing run has too. No line for c's answer. A media failure other than "gone" logs a warning and answers `503`,
so c's `accept` was most likely refused with a `4xx`. The server logs no `4xx`. Candidates: the node answered
`404`/`410` for the leg or room (`MediaError::Gone`, answered `410 MediaGone`), or the call state refused it
(`answerable`: the seat no longer ringing, or its leg reserved over).

To do:
- Log a refused group `accept` (status and reason) at `info`, then run the test under load (four threads with the other
  `binary::calls::` tests, or `stress` beside it) until it fails again.
- From that line, find which refusal it is, and fix it with a test.
