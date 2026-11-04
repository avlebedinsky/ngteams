# Runtime adapters

Read only the section for the current host. Tool names below describe available interfaces; use the actual tools
and schemas exposed by the session. Never invent a tool or start a nested CLI to bypass missing agent support.

## Claude Code

- Start a named `general-purpose` agent with `Agent`, a clean brief, and background execution. For edits use
  `isolation: "worktree"`. Record its actual worktree, branch, and agent identifier when returned.
- Send steering with `SendMessage`; use the runtime's agent listing to distinguish live agents from old metadata.
- Consume background-agent completion notifications while work remains. Use the host's exposed event/yield mechanism
  to wait while keeping the conversation available; don't assume ending a turn guarantees a later wake. Keep any
  blocking wait at most 60 seconds and report only meaningful changes.
- Use `run_in_background` for long shell commands and `TaskStop` to stop owned commands. Keep their IDs in the task.
- The owner PID is `$CLAUDE_PID` and session ID is `$CLAUDE_CODE_SESSION_ID` when provided. Verify the owner exists.
  Do not use the PID of a short-lived tool shell.
- The status server discovers Claude transcripts by name. Preserve `sessions` and optionally set `transcript` to
  the actual JSONL. Legacy boards without explicit worktree metadata can be inspected with
  `scripts/agent-state.sh <name> <session-id>...`.

## Codex

- Use the exposed agent-spawn tool (for example `collaboration.spawn_agent` or `spawn_agent`). When supported,
  choose `fork_turns: "none"` and provide a complete brief. Otherwise explicitly scope inherited context to this task.
  Inherit the session's model settings unless the user or applicable instructions specify otherwise.
- **Spawning does not imply isolation.** Before spawning an editor, create a branch and worktree from `origin/main`
  (or the review head), using a unique run/task name:

  ```sh
  git -C <repo> worktree add -b dispatch/<run>/<task> <repo>/.dispatch/<run>/worktrees/<task> origin/main
  ```

  Give the worker the absolute worktree path and require every file and shell operation to run there. Record
  `worktree`, `branch`, `base_commit`, and the returned `agent_id` in the ledger. Agents share a filesystem; a spawn
  alone does not move the worker's current directory. Read-only workers use the main checkout.
- Send steering to a running worker with its message tool (for example `collaboration.send_message`). Resume an
  idle worker with the runtime's follow-up/resume tool (`collaboration.followup_task` where exposed); a message
  alone may not start another turn. Close/interrupt only workers owned by this board, using available lifecycle tools.
- After dispatching or answering an ETA/status question, keep the turn active with `collaboration.wait_agent` when
  exposed, using `timeout_ms: 60000` or less. This waits for agent mailbox updates and can return early for user input;
  it returns a summary, so consume the delivered messages/results before updating the board. A timeout is not a
  completion: process any newly eligible work and wait again without a heartbeat message. Handle user steering in
  commentary and resume monitoring. Agent events can arrive at message/tool boundaries; recheck for queued results
  before waiting or ending the turn, including after the last worker returns and a review is due. Don't busy-poll
  agent listings or launch shell sleep loops. If this tool is absent, use the exposed runtime event wait; if none is
  available, report that monitoring limitation and preserve the board rather than promise an automatic wake.
- Respect the runtime's actual capacity, including the dispatcher and other live agents. `slots` records the user's
  requested limit; run no more than that or the available worker capacity. A waiting worker frees a slot only after
  it has actually yielded/stopped. If it still occupies a runtime slot, persist its question and close it when
  possible, then create a replacement when the answer arrives.
- Use shell sessions and their returned handles for long commands; poll/resume only the matching handle. Terminate
  owned processes with the shell/session facility or their recorded PIDs. Runtime tools belong outside JavaScript
  orchestration calls if the host exposes them only as direct tools.
- Use the runtime's real persistent PID/session ID when exposed. Otherwise record `owner: null`, a unique
  `owner_token`, and update `updated_at` on board changes; never substitute a temporary shell PID. A recent ownerless
  board needs a live-session check or explicit takeover before resuming.
- Do not assume Claude's transcript layout or a stable Codex on-disk transcript schema. Use the portable event log
  for the page: record an absolute `transcript` path such as `<run-dir>/events/1.jsonl`, then give the worker:

  ```sh
  <skill-dir>/scripts/task-event.py <event-path> investigate 'Reading the task context'
  <skill-dir>/scripts/task-event.py <event-path> implement 'Updating the search view'
  <skill-dir>/scripts/task-event.py <event-path> check 'Running the targeted tests'
  ```

  Emit when a phase or meaningful activity changes. Phases are `investigate`, `implement`, `check`, `commit`, `land`,
  `done`; review workers use `review`, `gate`, `land`, `done`; read-only workers may use `verify`. `done` means a
  handback, not proof of landing: the dispatcher's ledger remains authoritative. Keep event text free of secrets,
  raw tool output, and captures. A new worker continuing the same task keeps its event log.

## Either host

Persist enough to resume without transcripts: task text and follow-ups, screenshot descriptions/paths, agent ID,
worktree/branch/base, checks and shell handles, questions, and result. Read the repository's applicable `AGENTS.md`
and `CLAUDE.md` explicitly when they aren't automatically loaded. If background agents are unavailable, explain
that dispatch mode cannot run in this host and keep the board recoverable; do not silently perform queued tasks inline.

### All-tasks-done desktop notice in ngterm

Chat completion is required on every host. In ngterm (`NGTERM_ENABLED=1`), also use its native notifier once the
current drain's required tasks, reviews, checks, and cleanup are complete. Recheck newly received task events first;
a new task reopens the drain. Blocked or waiting work prevents this notice. Disclose explicitly deferred/canceled
tasks in the completion summary and notification body.

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
