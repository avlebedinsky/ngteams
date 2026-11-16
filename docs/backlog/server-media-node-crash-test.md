# Test a crashed media node through its heartbeat's expiry

The e2e tests end a group call when its media node stops in order (`MediaNode::stop`). That stop removes the node's
registry entry at once. No test crashes a node: its heartbeat stops, its entry stays in Redis and expires after
`mixer.dead_after`, and only then does the API node's sweeper end its calls with `503`. Thus a regression in the
entry's expiry (`Registry::beat`'s `EX`) would keep a crashed node's calls open, and no test would fail.

An in-process node cannot crash: its tasks and thread stop only through `stop`. Run `serve --role media` as a child
process of the binary e2e harness (`tests/e2e/binary/support.rs`) on Postgres and Redis, with `mixer.heartbeat = 1`
and `mixer.dead_after = 3` and a fixed `mixer.internal_listen`. Its readiness is the internal listener's
`/health/live`, because a media role has no public listener. Start a group call from an in-process API node that
shares the database and Redis, send `SIGKILL` to the media process, and assert that each party's end is `503` within
`dead_after` plus two heartbeats and two media sweeps.
