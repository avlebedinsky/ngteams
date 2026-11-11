---
worth: next
where: crates/teams-signin/src/native/mod.rs, crates/teams-platform/src/windows_process.rs
added: 2026-11-11
---
# Keep sign-in worker pipes out of unrelated Windows children

Round 1 Windows review A7 is real. `Worker::start` creates inheritable duplicates of its private stdin/stdout pipes
inside the running GUI. Its explicit CreateProcess handle list protects the intended child, but another thread's
ordinary `std::process::Command` can inherit those duplicates during the short transfer lifetime. That unrelated
child can retain the output writer, delaying EOF after a worker crash, or retain the input reader. The existing
Transfer documentation's runner/stopped-GUI constraint is not met by this caller.

Defer this minor to a dedicated process-boundary change. A shared mutex around only Transfer is insufficient:
Rust std uses its own private spawn lock, and UI dependencies and helper launch sites also need accounting.
Prefer private named-pipe rendezvous with peer validation and noninheritable endpoints, or route every relevant
launch through an enforced common boundary. Keep atomic job assignment and the worker's exact handle list.
Correct the Transfer caller contract as part of that change, rather than declaring the race an accepted exception.

Regression: hold the worker launch at the inheritable-handle window, spawn an unrelated long-lived child, kill the
worker without a final protocol event, and require immediate EOF while the unrelated child remains alive. Prove
that the unrelated process cannot read Start or a worker response. Rerun native platform and sign-in gates.
