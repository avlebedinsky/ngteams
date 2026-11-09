# Runtime adapters

Workers are ngterm companion sessions on every host: `scripts/companion.sh` starts, steers, reads, and closes them,
and `scripts/companion-wait.py` reports when one needs the dispatcher. The host sections below cover only how the
dispatcher itself waits and owns its processes; the companion sections cover what each worker runtime needs. Use the
actual tools and schemas the session exposes. Never invent a tool, and don't use the host's in-process agents.

## Dispatcher in Claude Code

- Run the waiter with `run_in_background` and a long fallback (`--timeout 3600`); its exit re-invokes this session,
  so a turn may end while companions work. Handle every event line it printed, record the seen markers on the board,
  then start a new waiter. Exit 124 is only the fallback: restart it. Keep exactly one waiter, and stop it with
  `TaskStop` at mode end.
- User messages arrive while the waiter runs; answer them, then leave it running. So does companion mail, after a tool
  call or by waking the idle session.
- Use `run_in_background` for other long commands and `TaskStop` for owned ones; keep their IDs on the board.
- The owner PID is `$CLAUDE_PID` and session ID is `$CLAUDE_CODE_SESSION_ID` when provided. Verify the owner exists.
  Do not use the PID of a short-lived tool shell.

## Dispatcher in Codex

- Run the waiter in a shell session with `--timeout 60` and keep the turn active: handle its events, update the board,
  and run it again. A 124 exit is a timeout, not a completion: start newly eligible work and wait again without a
  heartbeat message. Handle user steering in commentary between waits. Companion mail arrives after a tool call, so
  at the latest when the waiter returns. If the turn must end, run
  `companion-wait.py --once` first and report that monitoring resumes on the next turn; companions keep working.
- Use shell sessions and their returned handles for long commands; poll/resume only the matching handle. Terminate
  owned processes with the shell/session facility or their recorded PIDs.
- Use the runtime's real persistent PID/session ID when exposed. Otherwise record `owner: null`, a unique
  `owner_token`, and update `updated_at` on board changes; never substitute a temporary shell PID. A recent ownerless
  board needs a live-session check or explicit takeover before resuming.

## Claude Code companions

- `companion.sh start --runtime claude` runs `claude --permission-mode auto` with a fixed session UUID and the
  brief's path as the first prompt. Its transcript is the JSONL the helper prints; record it as `transcript` so the
  status page follows it. It loads `CLAUDE.md`/`AGENTS.md` and `.claude/skills` from its working directory.
- Mail reaches it after its next tool call and wakes it when idle; `companion.sh send` types instead when an idle one
  hasn't taken the mail within seconds.
- A worktree under the repository is trusted with it. Starting one outside a trusted folder stops it on Claude's
  trust prompt (a `completed` or `blocked` event with no report; read its screen).

## Codex companions

- `companion.sh start --runtime codex` runs `codex --no-daemon -C <dir>` with the user's configured model, approval,
  and sandbox settings. `--no-daemon` keeps the TUI and its hooks in the companion's own process: on Codex's shared
  daemon, hooks run with the environment of whichever pane started it and the status lands there.
- Mail reaches it after its next tool call or at the end of a turn, but doesn't wake an idle one, so `companion.sh send`
  types to a Codex that has stopped. It tells that from Codex's screen, because Codex's hooks report `blocked` for a
  turn that ended on a question and stay `active` after an interrupt.
- Codex's session ID isn't known at start, so the pane's relaunch command is pinned to a plain shell: after an ngterm
  relaunch the pane is open without an agent. An unshown pane first reports `asleep`; once woken (or shown) its shell
  is readable and the waiter reports `exited`; continue the task in a new companion.
- Codex keeps no stable on-disk transcript schema to rely on. Record an absolute portable event log such as
  `<run-dir>/events/<num>.jsonl` as `transcript`, and tell the companion to emit:

  ```sh
  <skill-dir>/scripts/task-event.py <event-path> investigate 'Reading the task context'
  <skill-dir>/scripts/task-event.py <event-path> implement 'Updating the search view'
  <skill-dir>/scripts/task-event.py <event-path> check 'Running the targeted tests'
  ```

  Emit when a phase or meaningful activity changes. Phases are `investigate`, `implement`, `check`, `commit`, `land`,
  `done`; review and fix companions use `review`, `gate`, `land`, `done`; read-only companions may use `verify`.
  `done` means a handback, not proof of landing: the dispatcher's ledger remains authoritative. Keep event text free
  of secrets, raw tool output, and captures. A new companion continuing the same task keeps its event log.

## Either host

Persist enough to resume without transcripts: task text and follow-ups, screenshot descriptions/paths, runtime,
companion session and Claude session UUID, brief and report paths, worktree/branch/base, review state, checks and
shell handles, questions, and result. If ngterm or `ngtermctl` is unavailable, explain that dispatch mode cannot run
here and keep the board recoverable; do not silently perform queued tasks inline.

### All-tasks-done desktop notice in ngterm

Chat completion is required on every host. Also use ngterm's native notifier once the current drain's
required tasks, reviews, checks, and cleanup are complete. Recheck newly received task events first;
a new task reopens the drain. Waiting, blocked, or failed work prevents this notice, including a failed landing or
preserved unfinished work. Only tasks explicitly deferred or canceled by the user are exclusions; disclose them
in the completion summary and notification body.

When `ngtermctl notify` and this session's `NGTERM_SESSION_ID`/`NGTERM_SOCKET` are available, arm
`completion_notice.desktop_pending` in the same atomic board write as the chat's pending flag, before posting either
notice. Then call:

```sh
ngtermctl notify 'All tasks are done. Reviews, checks, and cleanup are complete.' --title 'Task dispatch complete' \
  --target "$NGTERM_SESSION_ID" --socket "$NGTERM_SOCKET"
```

Use the actual completion summary when there are exclusions. Scope to this session explicitly; never use the
frontmost/`active` default. After successful submission, atomically clear `desktop_pending` and record
`desktop_sent_at`; the command's success acknowledges submission, not that the user saw it. On resume, skip a channel
with a receipt for this drain and retry an explicitly pending desktop notice without replaying the chat notice.
Missing desktop fields on an older, already-settled board are a baseline, not a new notification request.

If the notifier or session scope is unavailable, deliver chat completion and report the desktop limitation. If a
submission fails, preserve its pending state and record the failure; don't retry in a loop or hold the turn open
solely for it. Reconcile ambiguous submission outcomes against available history before retrying. These instructions
provide no background wake or guaranteed OS delivery after the turn ends.
