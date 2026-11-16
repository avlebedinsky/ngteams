---
worth: later
where: crates/teams-core/src/engine.rs (Core::run), crates/ngteams-server/tests/e2e/first_write_cancel.rs
added: 2026-11-16
---
# the engine's shutdown waits for a write that waits for its token, and that write can still go out

Cause: `Core::run` takes the next request only after `on_request` for the current one ends. A `Request::Shutdown` sent
while a write waits (here: for a token that the account's server holds back) waits in the inbox until that write ends.
Sign-out, account removal and quit all shut the engine down, so each one waits for that write.

Measured (QA of task 352, Linux, debug build, own-server account, a TCP proxy holds the token connection):

- The token never comes: the shutdown ends after the token client's 30 s timeout (30.0 s). The write fails ("no token
  for the request") and nothing reaches the server.
- The token comes 2 s after the shutdown starts: the write is still sent (row `Sent`), and the shutdown ends 2.2 s
  after it started.

In both cases, after the shutdown ends, nothing more goes out, no engine socket stays open and nothing panics.

Expected: once the user signs out, removes the account or quits, nothing more goes out, and the shutdown does not wait
for a token.

Test that shows it: `cargo test -p ngteams-server --test e2e first_write_cancel` (its children print a `qa:` line with
the shutdown time and the write's row; run a child directly as `.teamlead/qa/352/round-1/commands.md` says). Evidence:
`.teamlead/qa/352/round-1/linux/06-qa-children.log` and `04-first_write-qa-tests.log`.

Suggested next step: let `Core::run` take a `Shutdown` while a request runs (select on the inbox beside the request's
future), and decide what an interrupted write's row becomes.
