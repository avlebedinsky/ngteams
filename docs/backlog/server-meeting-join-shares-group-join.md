# One join path for group calls and meetings on the own server

In `crates/ngteams-server/src/calling/`, `meeting::start` repeats most of `group::start`, and `meeting::let_in`
repeats `group::join`: reserve the seat, make the leg on the media node, seed it, do the join step, and undo the
reservation on a failure. The "store the call, or on a conflict give the running call" tail is in three places
(`group::open`, `meeting::open_waiting`, `meeting::meet_now`). A fix to the join or the rollback path must go into
each copy.

Put the shared middle of `group::join` and `meeting::let_in` into one function in `group.rs` that takes the account,
its endpoint with its links, and the offer. Let the three start paths use one helper for "insert, or the running
call". Then reduce the `pub(super)` items of `group.rs` that only `meeting.rs` uses. The e2e tests in
`tests/e2e/calling/meetings.rs` and `group_signaling.rs` cover the behavior that must not change.
