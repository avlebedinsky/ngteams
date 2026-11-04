---
name: task-dispatch
description: Run a session as a task dispatcher in Claude Code or Codex. Send each incoming task to its own background agent, queue beyond the user's concurrency limit, isolate edits in git worktrees, track a resumable board and status page, and review landed work when the queue drains. Use for "agent per task", "dispatch these", "run tasks in parallel", "up to N agents", "resume the dispatch", or "continue dispatch", and subsequent tasks while dispatch mode is active. Reading or editing this skill alone does not activate dispatch mode.
---

# Task dispatch

Keep this conversation available while background agents work. Once the user requests dispatch mode, apply it to
later tasks until they end it. Coordinate, record state, answer status questions, and report results; don't take over
implementation inline.

While workers, reviews, or eligible queued tasks remain, stay active: consume runtime completion events using the
host's wait mechanism and start eligible work. Answer new steering/status requests in commentary, then continue
monitoring. A spawn receipt or ETA is an update, not a reason to end the turn. Report meaningful results promptly;
don't busy-poll or send empty heartbeat updates. Finish the turn when the board is idle/complete, the user explicitly
stops work, or every remaining task requires user/external input and no independent work can progress. Surface
required questions promptly; waiting never supplies authorization. Host waiting details are in the runtime reference.

Read the matching host section of [references/runtimes.md](references/runtimes.md) before starting agents. In ngteams,
also read [references/ngteams.md](references/ngteams.md). Resolve `<skill-dir>` to this skill's absolute directory;
pass absolute helper paths to workers. The canonical repository path is `.agents/skills/task-dispatch`; the Claude
discovery path `.claude/skills/task-dispatch` links to it.

## Routing and capacity

- Code work, investigations, research, runs, and tests get workers, even for small tasks. Answer questions using
  context already held directly. Answer the user's question before reporting board status.
- Each independent task gets one worker. Send corrections and follow-ups to its current worker; after completion,
  create a new task naming the earlier result. Preserve the user's whole scope, including screenshots.
- Default to **2 concurrent workers**, or the user's latest limit, constrained by the host's available capacity.
  Reviews count. Queue excess work FIFO and report its position. A waiting worker frees a logical slot once it
  yields; the host adapter governs actual capacity. Answered tasks resume first.
- Dependencies wait for their prerequisite to land. Serialize tasks clearly rewriting the same function/file;
  separate worktrees and rebasing suffice for incidental overlap. Never trim scope to avoid a collision.
- Start the next eligible task when reporting a completion. Reviews may run alongside newly arriving tasks.

## Durable board

Keep `<repo>/.dispatch/<run>/progress.json` in the primary checkout, ignored by Git. Use a unique timestamp plus
suffix for `<run>` so simultaneous sessions cannot collide. Write atomically via a sibling temporary file and rename
on every change, including task receipt, follow-up, worker start/return, question, slot count, and review base.
The dispatcher alone writes the board. Workers append only their own event logs.

```json
{
  "owner": 48213,
  "owner_token": "unique-run-owner",
  "runtime": "codex",
  "sessions": ["actual-session-id-if-known"],
  "updated_at": "2026-11-03T12:00:00Z",
  "slots": 2,
  "review_base": "full-commit-id",
  "completion_notice": {
    "pending": false, "sent_at": null,
    "desktop_pending": false, "desktop_sent_at": null
  },
  "standing": ["User instructions applying to later tasks"],
  "tasks": [
    {
      "num": 1, "title": "Escape closes search", "agent": "escape-search",
      "agent_id": "actual-runtime-worker-id", "state": "running", "flow": "edit",
      "text": "User's request and context needed to resume, including follow-ups",
      "worktree": "/repo/.dispatch/run/worktrees/escape-search",
      "branch": "dispatch/run/escape-search", "base_commit": "full-base-id",
      "transcript": "/repo/.dispatch/run/events/1.jsonl", "shells": [],
      "result_notice": {"pending": false, "sent_at": null}
    },
    {"num": 2, "title": "Pinned chats", "agent": "", "state": "queued", "after": 1,
     "text": "User's request", "note": "Needs #1's view"}
  ]
}
```

`owner` is a verified persistent host PID, or `null` if unavailable. Never use a temporary tool-shell PID. Keep real
session IDs when available, but don't depend on a transcript existing. `flow` is `edit`, `scratch` (read-only), or
`review`; always set it. `transcript` may be a Claude JSONL or the portable event log in the runtime adapter.
Store explicit worktree/branch/base metadata even when the host stores it too. Older Claude boards remain readable.

Persist announcement state with the same atomic board writes. On a new worker result, set its `result_notice.pending`
and clear its prior `sent_at` before reporting it; after actually posting the notice, clear `pending` and record `sent_at`.
Resume pending notices; a timestamp acknowledges delivery, not just a status-page update. If interrupted between posting and recording,
check available conversation history before replaying; don't claim exactly-once delivery when that history is unavailable.
For older boards with absent fields, baseline already-recorded completed results and an already-settled drain as
nonpending without inventing a sent timestamp; newly discovered results or later settlement of unfinished work still
need notices. Missing fields alone must not replay historical results.

`completion_notice` tracks the current drain: any new task receipt or follow-up reopens it by clearing both channels'
pending flags and sent timestamps. When all required tasks, checks, cleanup, and review settle, set the chat notice
pending and promptly announce that all tasks are done in commentary. Record delivery before the final response ends
the turn. In ngterm also send one native desktop notification, using the separate desktop receipt and instructions
in the runtime reference. Resume pending notices; don't repeat a channel already sent for the same drain. Report
waiting, blocked, or failed work distinctly; it prevents all-done notices, including a failed landing or preserved
unfinished work. Only tasks explicitly deferred or canceled by the user are exclusions; disclose them in the
completion summary. An eligible review is work remaining even after the last implementation worker returns.

States: `queued`, `running`, `waiting on user`, `landed <hash>`, `done` (read-only result), and `failed`. Record the
question for a waiting task. Keep unfinished tasks' original words, follow-ups, relevant standing instructions,
and screenshot descriptions with paths. A completed task can replace `text` with its result/check summary. Never
store credentials, raw captures, or debug logs. Remove a dropped task only at the user's request.

Fetch `origin/main` for a new board and set `review_base` to its full commit ID. Resume keeps the existing base.
A landed task stays unreviewed until a successful pass covers it. Show a compact board when asked or when changes
matter: task number, title, worker, state, and landed hash.

## Start a worker

Persist the queued task and intended worktree before spawning; persist its returned ID immediately afterwards.
Use the host adapter for isolation/lifecycle. Editing/review workers get dedicated worktrees; read-only workers
use the primary checkout. Base editing worktrees on `origin/main`, not unpushed primary-checkout commits. If a task
needs uncommitted work, arrange an isolated handoff; don't stash or reset the user's checkout.

Give each worker a self-contained brief:

1. Quote the user's task and add relevant prior results, preferences, screenshot context, and dependencies.
   Give absolute paths for the repo, worktree, helpers, and optional event log. Require every operation in the
   assigned worktree and explicit reading of repository instructions when the host doesn't load them.
2. State the complete expected behavior. Resolve reversible ambiguities reasonably and report choices. Return
   questions only for consequential missing decisions or required authorization. Apply relevant project skills.
3. Run focused checks proportional to the change; ngteams specifics are in its reference. Use background shells
   for long work, track handles/PIDs, and stop owned processes before handback. Preserve artifacts delivered to the
   user and explicitly requested running applications. Don't kill other sessions' processes.
4. Commit editing work and land it with `<skill-dir>/scripts/land.sh` from the worktree, subject to existing user
   authorization and repository rules. This workflow doesn't override user restrictions on commit, merge, push,
   or review order. If landing is unavailable/unauthorized, preserve the branch and report it ready; don't mark it
   landed or repeatedly retry. Never force-push or reset the primary checkout.
5. Report changes, choices, checks/results, `LANDED <hash>` or the exact blocker, and that landed work awaits review.
   List owned shell handles and their stopped state. Put questions first.

The ngteams landing helper rebases on `origin/main`, checks workspace compilation, and serializes the final fetch,
rebase, push, and primary-checkout fast-forward under a shared lock. Read its header for exit codes. Exit 4 means
nothing was pushed: report the checkout conflict and preserve the work. Exit 6 means pushed but primary checkout
behind: verify the remote and report it. Retry a transient fetch/push failure once after checking the remote;
preserve work and report persistent failures.

## Completion, cleanup, and review

When a worker returns, answer pending user questions, verify material claims against Git if needed, then report its
result briefly using its announcement state. Update the board before dispatching more work. If part of the task is
missing, continue it or queue a follow-up within its original authorized scope; don't call it complete.

Check/stop all task-owned background commands, not just the handles its report lists. For a landed, clean worktree,
use `<skill-dir>/scripts/cleanup.sh <absolute-worktree>` from outside it. Keep unfinished worktrees and report where
they are. `--discard` is only for work the user explicitly dropped. Remove owned scratch files and empty worktrees;
preserve requested output and applications. Never infer ownership from an unrelated process name.

When no task is running or eligible to start and new commits landed since `review_base`, freeze `<base>..<head>`
for one review/check pass in its own worktree. Don't repeat a failed pass endlessly: record the blocker and retain
the base until resolved. Tasks waiting on the user needn't prevent reviewing landed work. Size review/checks to
what changed, follow repository review requirements, and fix findings in scope. The review worker follows the same
isolation, process cleanup, commit, and landing conventions. New tasks may land while it runs, outside its frozen range.

The review report names the range, findings/fixes, checks and omissions, failures/outcomes, landed fix hashes, and
stopped shells. Advance `review_base` only after success: to the last fix commit if only its fixes landed after
`<head>`, otherwise to `<head>` so concurrent commits get another pass. If more work landed and the board drains,
review the next range. A blocked fix/check/landing leaves the base unchanged and remaining work visible.

## Resume across either host

On mode entry or explicit resume, inspect `.dispatch/*/progress.json`; after compaction reread only your own run.
Skip boards with live verified owners. For ownerless boards, check runtime agents/session state before claiming
them; a recent timestamp alone doesn't prove death. A PID may be reused: verify host identity where possible.
Don't combine boards owned by other sessions or discard their records. Resume recent abandoned work and say so;
ask about old potentially obsolete tasks while continuing independent new work on a new board.

Claim the abandoned board with the new owner/token/runtime/session; preserve its base, standing instructions, and
task numbers. Inspect running/waiting tasks using runtime state and:

```sh
<skill-dir>/scripts/agent-state.sh --ledger <absolute-progress.json> --task <number>
```

This reads explicit worktree metadata on either host and falls back to old Claude metadata. Believe Git state over
prose: a task may have pushed before the session died. Mark proven landings and clean them up; queue unlanded work
for continuation in its existing worktree. If nothing remains, restart from persisted text. Read-only tasks restart
unless their result was recorded. Re-present unanswered questions and keep those tasks waiting.

Reuse genuinely live workers via the host's steering/resume tool. Otherwise spawn a fresh worker with the saved
brief and existing worktree, without creating another one. Tell it to inspect status, branch history, and unfinished
rebases first; preserve changes and stop only verified old task processes. Interrupted reviews keep their frozen
range. Prioritize continuations and interrupted reviews, then new tasks. Report unseen landings, continuations,
and restarts.

## Optional status page

The page supplements chat notices; updating it does not deliver a result to the user. It reads the ledger plus Claude
transcripts or portable event logs and binds loopback only. In ngterm
(`NGTERM_ENABLED=1`) start it with dispatch mode after writing the ledger; elsewhere start it when requested:

```sh
<skill-dir>/scripts/status-page.sh --ledger <absolute-progress.json> --exit-with <verified-owner-pid>
```

Omit `--exit-with` if no persistent PID is known and stop it explicitly when done. Keep its shell handle/PID.
The Python standard-library server starts without a build and announces `http://127.0.0.1:47231/`; if occupied, choose another port
with `--port` and use that URL. Don't terminate another board's server to free the port.

For ngterm, inspect `ngtermctl tree --json`; enable this session's vertical split if needed, then open the overlay:

```sh
ngtermctl session split on --axis vertical --target "$NGTERM_SESSION_ID" --socket "$NGTERM_SOCKET"
ngtermctl session overlay open --url http://127.0.0.1:47231/ --js --chromeless --pane right \
  --target "$NGTERM_SESSION_ID" --socket "$NGTERM_SOCKET"
```

At mode end stop the owned server and close its overlay with `ngtermctl session overlay close --pane right` and
the same target/socket flags. Keep the ledger for unfinished/unreviewed work. The server is independent of the app;
changes run `python3 -B -m unittest discover -s <skill-dir>/dispatch-status -p 'test_*.py'`.

Keep replies short: task number, worker name, queue position/dependency or result. Don't speculate about worker
activity or promise outcomes. Continue reversible work on reasonable defaults; silence never supplies authorization.
