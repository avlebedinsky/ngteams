---
name: task-dispatch
description: Run a session as a task dispatcher in Claude Code or Codex inside ngterm. Send each incoming task to its own ngterm companion session running whichever agent fits it best, Claude Code or Codex; queue beyond the user's concurrency limit, isolate edits in git worktrees, track a resumable board and status page, and once each task lands have one companion run revmux over it in a mode sized to the task and another assess the findings and fix the eligible ones. Use for "agent per task", "dispatch these", "run tasks in parallel", "up to N agents", "resume the dispatch", or "continue dispatch", and subsequent tasks while dispatch mode is active. Reading or editing this skill alone does not activate dispatch mode.
---

# Task dispatch

Keep this conversation available while workers run in **ngterm companion sessions**: interactive Claude Code or
Codex sessions placed under this session in ngterm's sidebar, where the user can watch and talk to them. Once the
user requests dispatch mode, apply it to later tasks until they end it. Coordinate, record state, answer status
questions, and report results; don't take over implementation inline, and don't use the host's in-process agents.

Companions need ngterm: `NGTERM_ENABLED=1`, `NGTERM_SESSION_ID`, and a working `ngtermctl`. Without them, say that
dispatch mode can't run here, keep the board recoverable, and don't perform queued tasks inline.

While companions, reviews, or eligible queued tasks remain, stay active: keep one companion waiter running (below)
and handle each event it reports. Answer new steering/status requests in commentary, then continue monitoring. A
start receipt or ETA is an update, not a reason to stop watching. Report meaningful results promptly; don't
busy-poll or send empty heartbeat updates. Finish when the board is idle/complete, the user explicitly stops work,
or every remaining task requires user/external input and nothing independent can progress. Surface required
questions promptly; waiting never supplies authorization.

Read the matching host section of [references/runtimes.md](references/runtimes.md) before starting companions. In
ngteams, also read [references/ngteams.md](references/ngteams.md). Resolve `<skill-dir>` to this skill's absolute
directory; pass absolute helper paths to companions. The canonical repository path is `.agents/skills/task-dispatch`;
the Claude discovery path `.claude/skills/task-dispatch` links to it.

## Routing and capacity

- Code work, investigations, research, runs, and tests get companions, even for small tasks. Answer questions using
  context already held directly. Answer the user's question before reporting board status.
- Each independent task gets one companion. Send corrections and follow-ups to its current companion; after
  completion, create a new task naming the earlier result. Preserve the user's whole scope, including screenshots.
- Default to **2 concurrent companions**, or the user's latest limit. Review and fix companions count. Queue excess
  work FIFO and report its position. A companion waiting on the user stays open but frees its slot. Free slots go
  first to landed tasks' reviews and fixes, then answered tasks and continuations, then new tasks.
- Dependencies wait for their prerequisite to land. Serialize tasks clearly rewriting the same function/file;
  separate worktrees and rebasing suffice for incidental overlap. Never trim scope to avoid a collision.
- Start the next eligible work when reporting a completion. A task's review runs alongside other tasks.

### Claude or Codex

Pick the runtime that fits the task best, record it with a one-line reason (`runtime`, `why`), and name it when
reporting the start ("#3 → codex: protocol bug with a failing test").

| Claude Code fits | Codex fits |
|---|---|
| Anything the app shows: views, layout, themes, sandbox screenshots, matching a reference UI | Engine, protocol, store, transport, or media logic with a clear expected behavior |
| Browser/capture work, live or lab runs that need judgment along the way | A bug with a reproducer, stack trace, log, or failing test |
| Skills, prompts, docs, plans, repository instructions | Compile, lint, coverage, flaky-test, and cross-crate refactoring work |
| Open-ended investigation, research, tasks with product ambiguity | Performance, concurrency, and algorithmic correctness |
| revmux review rounds (the default reviewer, below) | |

A mixed task goes by the part that dominates the work. When neither column clearly wins, pick the runtime with fewer
running companions. The user's naming of a runtime always wins. Avoid a runtime whose account is nearly out of usage
(its own footer warns, or `ngswap usage` shows it near the limit); if a companion hits its limit mid-task, close it
and continue the task in the other runtime in the same worktree.

## Durable board

Keep `<repo>/.dispatch/<run>/progress.json` in the primary checkout, ignored by Git. Use a unique timestamp plus
suffix for `<run>` so simultaneous sessions cannot collide. Write atomically via a sibling temporary file and rename
on every change, including task receipt, follow-up, companion start/return, event handled, question, slot count, and
review state. The dispatcher alone writes the board. Companions never write it: they work in their own worktree (a
revmux companion also writes its findings file) and hand back through their report file and, for Codex, their event
log.

```json
{
  "owner": 48213,
  "owner_token": "unique-run-owner",
  "runtime": "claude",
  "sessions": ["dispatcher-session-id-if-known"],
  "ngterm_session": "dispatcher's NGTERM_SESSION_ID",
  "updated_at": "2026-11-03T12:00:00Z",
  "slots": 2,
  "completion_notice": {
    "pending": false, "sent_at": null,
    "desktop_pending": false, "desktop_sent_at": null
  },
  "standing": ["User instructions applying to later tasks"],
  "tasks": [
    {
      "num": 1, "title": "Escape closes search", "agent": "escape-search", "role": "task",
      "runtime": "codex", "why": "key-handling bug with a failing test",
      "session": "ngterm-companion-session-id", "window": "ngterm-window-id", "claude_session": null,
      "state": "running", "flow": "edit",
      "text": "User's request and context needed to resume, including follow-ups",
      "brief": "/repo/.dispatch/run/briefs/1.md", "report": "/repo/.dispatch/run/reports/1.md",
      "report_seen": 0, "status_seen": 0,
      "worktree": "/repo/.dispatch/run/worktrees/escape-search",
      "branch": "dispatch/run/escape-search", "base_commit": "full-base-id",
      "transcript": "/repo/.dispatch/run/events/1.jsonl",
      "review": {"mode": "focused", "why": "one contained key-handling fix", "range": "onto..head",
                 "round": 1, "state": "reviewing", "worktree": "/repo/.dispatch/run/worktrees/escape-search-review",
                 "fixes": []},
      "result_notice": {"pending": false, "sent_at": null}
    },
    {"num": 3, "title": "Review #1", "agent": "escape-search-revmux", "role": "revmux", "of": 1,
     "runtime": "claude", "session": "ngterm-companion-session-id", "window": "ngterm-window-id",
     "claude_session": "uuid", "state": "running", "flow": "review",
     "text": "Review #1's range onto..head, mode focused",
     "brief": "/repo/.dispatch/run/briefs/3.md", "report": "/repo/.dispatch/run/reports/3.md",
     "worktree": "/repo/.dispatch/run/worktrees/escape-search-review",
     "branch": "dispatch/run/escape-search-review", "base_commit": "full-base-id"},
    {"num": 2, "title": "Pinned chats", "agent": "", "state": "queued", "after": 1,
     "text": "User's request", "note": "Needs #1's view"}
  ]
}
```

`owner` is a verified persistent host PID, or `null` if unavailable. Never use a temporary tool-shell PID. `role` is
`task`, `revmux`, or `fix`; `flow` is `edit`, `scratch` (read-only), or `review` (both review roles); always set both.
`session` is the companion's full ngterm session ID and `window` the ngterm window it opened in; pass that window to
every later `companion.sh` call, since ngterm reads and targets only the frontmost window otherwise. `claude_session` is
the Claude Code session UUID of a Claude companion. `transcript` is that Claude companion's JSONL, or a Codex
companion's portable event log. `report_seen` and `status_seen` are the report mtime and status change time already
handled. A task's `review` tracks its own review: `mode` (the revmux profile, or `none`) with its reason, the reviewed
`range`, the `round`, `state` (`queued`, `reviewing`, `fixing`, `reviewed`, `skipped`, or `blocked`), its worktree, and
landed fix hashes. Review and fix companions are board entries of their own with `of` naming the task. Like any task
they get a brief, a report file, `text`, and the review worktree's `worktree`, `branch`, and `base_commit`, so resume
can inspect what they left. Older boards with `agent_id` and no `session` came from in-process agents, and older
`review_base` fields came from batch review passes; resume their tasks as below and review their landed, unreviewed work
per task.

Persist announcement state with the same atomic board writes. On a new companion result, set its
`result_notice.pending` and clear its prior `sent_at` before reporting it; after actually posting the notice, clear
`pending` and record `sent_at`. Resume pending notices; a timestamp acknowledges delivery, not just a status-page
update. If interrupted between posting and recording, check available conversation history before replaying; don't
claim exactly-once delivery when that history is unavailable. For older boards with absent fields, baseline
already-recorded completed results and an already-settled drain as nonpending without inventing a sent timestamp;
newly discovered results or later settlement of unfinished work still need notices. Missing fields alone must not
replay historical results.

`completion_notice` tracks the current drain: any new task receipt or follow-up reopens it by clearing both channels'
pending flags and sent timestamps. When all required tasks, checks, cleanup, and review settle, set the chat notice
pending and promptly announce that all tasks are done in commentary. Record delivery before the final response ends
the turn. Also send one native desktop notification, using the separate desktop receipt and instructions in the
runtime reference. Resume pending notices; don't repeat a channel already sent for the same drain. Report waiting,
blocked, or failed work distinctly; it prevents all-done notices, including a failed landing or preserved unfinished
work. Only tasks explicitly deferred or canceled by the user are exclusions; disclose them in the completion summary.
A landed task whose review hasn't settled is work remaining.

States: `queued`, `running`, `waiting on user`, `landed <hash>`, `done` (read-only result), and `failed`. Record the
question for a waiting task. Keep unfinished tasks' original words, follow-ups, relevant standing instructions,
and screenshot descriptions with paths. A completed task can replace `text` with its result/check summary. Never
store credentials, raw captures, or debug logs. Remove a dropped task only at the user's request.

A landed task stays unreviewed until its review is `reviewed` or `skipped`. Show a compact board when asked or when
changes matter: task number, title, runtime, companion name, state, landed hash, and review state.

## Start a companion

Persist the queued task, chosen runtime, and intended worktree before starting; persist the returned session
immediately afterwards. Editing and review companions get dedicated worktrees, which the dispatcher creates under
the run directory from `origin/main` (or the review head) before starting:

```sh
git -C <repo> worktree add -b dispatch/<run>/<name> <repo>/.dispatch/<run>/worktrees/<name> origin/main
```

Read-only companions use the primary checkout. Don't base work on unpushed primary-checkout commits. If a task needs
uncommitted work, arrange an isolated handoff; don't stash or reset the user's checkout.

Write the brief to `<run>/briefs/<num>.md` and start the companion in its working directory:

```sh
<skill-dir>/scripts/companion.sh start --runtime claude|codex --name <agent> --cwd <worktree-or-checkout> \
  --brief <run>/briefs/<num>.md --run-dir <run>
```

It prints `{"session", "window", "runtime", "claude_session", "transcript"}`; record all of them (for Codex, set
`transcript` to `<run>/events/<num>.jsonl` instead). Claude runs in auto permission mode and Codex with the user's
configured approval and sandbox settings; both load the repository's instructions from their working directory. A
relaunch of ngterm resumes a Claude companion's conversation and never reruns a first prompt.

The brief is self-contained:

1. Quote the user's task and add relevant prior results, preferences, screenshot context, and dependencies.
   Give absolute paths for the repo, worktree, helpers, report file, and (Codex) event log. Require every operation
   in the assigned worktree and reading the repository instructions that apply there.
2. State the complete expected behavior. Resolve reversible ambiguities reasonably and report choices. Ask only for
   consequential missing decisions or required authorization. Apply relevant project skills.
3. Run focused checks proportional to the change; ngteams specifics are in its reference. Never run revmux or
   another multi-agent review of the task's own change; its own review follows once it lands. Stop owned background
   processes before handing back. Preserve artifacts delivered to the user and explicitly requested running
   applications. Don't kill other sessions' processes.
4. Commit editing work and land it with `<skill-dir>/scripts/land.sh` from the worktree, subject to existing user
   authorization and repository rules. This workflow doesn't override user restrictions on commit, merge, push,
   or review order. If landing is unavailable/unauthorized, preserve the branch and report it ready; don't mark it
   landed or repeatedly retry. Never force-push or reset the primary checkout.
5. **Hand back through the report file**, as the last action before stopping: questions first, then changes,
   choices, checks/results, the `RANGE` and `LANDED` lines `land.sh` printed or the exact blocker, that landed work
   awaits review, and background processes stopped. A companion that needs a decision writes its question there and
   stops. Don't ask the user in the session itself: the dispatcher relays questions. Overwrite the file on each
   handback.

The ngteams landing helper rebases on `origin/main`, checks workspace compilation, and serializes the final fetch,
rebase, push, and primary-checkout fast-forward under a shared lock. Read its header for exit codes. Exit 4 means
nothing was pushed: report the checkout conflict and preserve the work. Exit 6 means pushed but primary checkout
behind: verify the remote and report it. Retry a transient fetch/push failure once after checking the remote;
preserve work and report persistent failures.

## Watch, steer, and close

Keep exactly one waiter running while any companion runs (host details in the runtime reference):

```sh
<skill-dir>/scripts/companion-wait.py --ledger <absolute-progress.json> [--timeout <seconds>]
```

It watches running and waiting tasks and returns one JSON line per task that needs attention. A `report` or
`completed` event on a task waiting on the user means the user answered in the companion's own pane: set the task
`running` again and handle the event as usual.

- `report`: a new report file and the turn has ended. Read it, then record `report_seen` = its `mtime` and
  `status_seen` = its `status_changed_at`, and handle it as a completion below.
- `completed`: the companion stopped without a new report. Read its screen with `companion.sh screen --session <id>
  --window <window>`; if it asked something, record the question and mark the task waiting on the user; if it stopped
  early, send a nudge to finish and write the report. Record `status_seen`.
- `blocked`: a dialog or question holds it. Read the screen; answer within your authority by typing into it, or relay
  the question to the user. Record `status_seen`.
- `exited`: ngterm sees another program or an idle shell in the pane, not the agent, as with a Codex companion after
  an ngterm relaunch (its pane comes back as a plain shell). Close that session, then continue as for `gone`.
- `gone`: the session closed. Check Git and the report, then continue the task in a new companion or record it.
- `asleep`: ngterm sees no program in the pane, which is how a companion that hasn't been shown since an ngterm
  relaunch looks (restored panes spawn only when shown or woken). Run `companion.sh wake --session <id> --window
  <window>`. A Claude companion then resumes its conversation and idles: send it a continuation brief saying a
  terminal relaunch interrupted it and to continue its task and hand back through its report. A Codex pane comes back
  as a plain shell, which the next read reports `exited`.

`exited`, `asleep`, and `gone` fire only after two reads agree, and a window ngterm can't read reports nothing.

Update the board before restarting the waiter. Steer a running or waiting companion by writing the message to
`<run>/briefs/<num>-<n>.md` and calling `companion.sh send --session <id> --window <window> --file <that file>`; keep
the follow-up in the task's `text`. When the user answers a waiting task's question, send the answer the same way, set
the task `running`, and count it in the slots again.

Close a companion (`companion.sh close --session <id> --window <window>`) once its result is recorded and its worktree
is landed and cleaned or it was read-only; closing ends every process it started. Keep companions open that wait on the
user or hold failed/unfinished work, and say where they are. A closed Claude companion's conversation continues with
`companion.sh start ... --resume <claude_session>` in the same directory, the new brief as its next prompt.

## Completion, cleanup, and review

When a report arrives, answer pending user questions, verify material claims against Git if needed, then report the
result briefly using its announcement state. Update the board before dispatching more work. If part of the task is
missing, send it back to the same companion or queue a follow-up within its original authorized scope; don't call it
complete. A task that landed gets its review queued in the same board write.

For a landed, clean worktree, use `<skill-dir>/scripts/cleanup.sh <absolute-worktree>` from outside it, then close
the companion. Keep unfinished worktrees and report where they are. `--discard` is only for work the user explicitly
dropped. Remove owned scratch files and empty worktrees; preserve requested output and applications. Never infer
ownership from an unrelated process name.

### Each landed task's review: a revmux companion, then a fix companion

Every task that lands gets its own review of exactly what it landed: the `RANGE` from its report, or
`<onto>..<landed hash>` worked out from Git when the line is missing. Reviews of different tasks run in parallel and
don't wait for the queue to drain. A read-only task, or one that landed nothing, has no review.

**Pick the revmux mode from the task**, record it with its reason, and name it when reporting the review's start
("#1 review: focused — one contained key-handling fix"):

| the task landed | mode |
|---|---|
| only prompts, Markdown, or comments; nothing that builds or runs | `none`: mark the review `skipped` and say why |
| a small, contained change in one area, low risk | `focused` |
| a real change with real risk: a feature, a shared API, a write path, calls, auth or tokens, the lab fence, a store migration, concurrency, or several crates | `comprehensive` |
| either of the above while one runtime is out of usage | `claude-only` or `codex-only` with the same reasoning |

Use `grill-me` or `expert` only when the user asked for them in words. Repository rules on what needs review win over
this table (ngteams: its reference). A re-review round picks its mode from what the fixes touched, by the revmux
skill's own re-review table: `final` by default, `comprehensive` when the fixes reached code the review hasn't seen.

1. **Review.** Create the review worktree from `origin/main` (it holds the range) and start a `revmux` companion in
   it: Claude by default, Codex when Claude is unavailable (both have the revmux skill). Its brief: run the revmux
   skill once over the range with the chosen profile, without asking; **fix nothing and commit nothing**, since the
   fix step belongs to the next companion; write every finding to `<run>/reviews/<task>-<round>/findings.md`
   (severity, whether revmux verified it, provenance such as immaterial or pre-existing, file:line, the claim,
   revmux's suggested fix) with the path of revmux's JSON result; then hand back through its report. A degraded
   round (missing sources) is re-run before handing back. This overrides the fix step of any standing revmux
   instructions for this companion only.
2. **Assess and fix.** Start a `fix` companion in the same worktree, routed by the findings' areas with the routing
   table; on a tie, prefer the runtime that didn't write the task, for a fresh read. Its brief names the task, the
   range, the findings file, and the checks the change needs. It checks each finding against the current code and
   records a verdict: **fix** when it is real and eligible (every critical or major, including immaterial and
   pre-existing ones; verified minors; unverified minors it confirms are real); **reject** with the reason when the
   code shows it isn't real; **defer** to the repository's backlog when it is real but too large for this review;
   **ask** when it needs a product decision. It fixes the eligible findings, runs the checks they need, commits,
   lands with `land.sh`, and reports the verdict table, the `RANGE`/`LANDED` lines, checks run and left out, and
   questions. Skip this companion when the round found nothing.
3. **Re-review the fixes.** When the fixes are more than trivial, run another round over the fix companion's `RANGE`
   only, then another fix companion, and repeat until a round finds no critical or major issue. A round that repeats
   the same findings without progress is a blocker: mark the review `blocked`, record why, and stop its loop.

Close each review companion once its report is recorded, and clean up the review worktree after the last fix lands.
Mark the review `reviewed` when a round finds nothing left to fix or its fixes are trivial and landed. Report per
task: mode, findings and their verdicts, fixes and their hashes, checks and omissions, deferred items, and questions.
A blocked fix, check, or landing leaves the review open and visible; don't retry it endlessly.

## Resume across either host

On mode entry or explicit resume, inspect `.dispatch/*/progress.json`; after compaction reread only your own run.
Skip boards with live verified owners. For ownerless boards, check host session state before claiming them; a recent
timestamp alone doesn't prove death. A PID may be reused: verify host identity where possible. Don't combine boards
owned by other sessions or discard their records. Resume recent abandoned work and say so; ask about old
potentially obsolete tasks while continuing independent new work on a new board.

Claim the abandoned board with the new owner/token/runtime/session; preserve its standing instructions, task numbers,
and review states. Companions outlive their dispatcher: run `companion-wait.py --ledger <board> --once` to collect
reports and stops that arrived meanwhile, and `companion.sh state --session <id> --window <window>` for each open
task: `agent` means Claude or Codex still runs in it, `no-agent` that ngterm sees another program or a shell there,
`unread` that it can't see the pane's program (an unshown pane after a relaunch: wake it as for `asleep`), `gone` a
closed session, and `unknown` that ngterm can't read that window (not a closure). Inspect the work itself with:

```sh
<skill-dir>/scripts/agent-state.sh --ledger <absolute-progress.json> --task <number>
```

Believe Git state over prose: a task may have pushed before the session died. Mark proven landings and clean them
up. A companion whose agent still runs keeps its task: steer it with `companion.sh send`. Close a `no-agent` pane
first. A closed one with unlanded work
continues in a new companion on its existing worktree (a Claude one via `--resume <claude_session>` when the
conversation is useful), told to inspect status, branch history, and unfinished rebases first, preserve changes, and
stop only verified old task processes. Tasks from in-process-agent boards continue the same way in companions. If
nothing remains, restart from persisted text. Read-only tasks restart unless their result was recorded. Re-present
unanswered questions and keep those tasks waiting. Interrupted reviews keep their range, mode, and round and continue
at the step they reached; a landed task with no review recorded gets one. Prioritize interrupted reviews and
continuations, then new tasks. Report unseen landings,
continuations, and restarts.

## Status page

The page supplements chat notices; updating it does not deliver a result to the user. It reads the ledger plus
Claude transcripts or portable event logs and binds loopback only. Start it with dispatch mode after writing the
ledger:

```sh
<skill-dir>/scripts/status-page.sh --ledger <absolute-progress.json> --exit-with <verified-owner-pid>
```

Omit `--exit-with` if no persistent PID is known and stop it explicitly when done. Keep its shell handle/PID.
The Python standard-library server starts without a build and announces `http://127.0.0.1:47231/`; if occupied,
choose another port with `--port` and use that URL. Don't terminate another board's server to free the port.

Inspect `ngtermctl tree --json`; enable this session's vertical split if needed, then open the overlay:

```sh
ngtermctl session split on --axis vertical --target "$NGTERM_SESSION_ID" --socket "$NGTERM_SOCKET"
ngtermctl session overlay open --url http://127.0.0.1:47231/ --js --chromeless --pane right \
  --target "$NGTERM_SESSION_ID" --socket "$NGTERM_SOCKET"
```

At mode end stop the owned server and waiter, and close its overlay with `ngtermctl session overlay close --pane
right` and the same target/socket flags. Keep the ledger for unfinished/unreviewed work. The server is independent of
the app; changes run `python3 -B -m unittest discover -s <skill-dir>/dispatch-status -p 'test_*.py'`.

Keep replies short: task number, runtime, companion name, queue position/dependency or result. Don't speculate about
companion activity or promise outcomes. Continue reversible work on reasonable defaults; silence never supplies
authorization.
