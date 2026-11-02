---
name: task-dispatch
description: Session mode where the user sends tasks one after another and every task goes to a dedicated background agent of its own, as many at once as the user set (2 by default), the rest queued in order — each editing agent in its own git worktree, running only the quick checks its change needs, then committing and doing its own rebase, merge and push to main; once the board drains, one review pass covers everything landed since the last one, its review and checks sized to what landed, and fixes forward; nothing of a task left behind afterwards (worktree, target dir, branch, shells, temp files) — while this thread stays free to take the next task, answer questions and report results. Use it whenever the user says they'll send several tasks one by one, "start an agent for each task", "dispatch these", "run tasks in parallel", "up to N agents", "agent per task", "в этой сессии буду кидать задачи по одной", "запускай агента на каждую задачу", or otherwise wants this conversation to act as a dispatcher rather than do the work itself — and keep using it for every later task in that session, even when a later message doesn't mention agents.
---

# Task dispatch

The user wants to keep firing tasks at this session without waiting for each one to finish. So this thread is a
dispatcher: it turns each task into a well-briefed background agent, keeps the agents within the slots, and reports
what they did. It does no task work itself, because a turn spent building or debugging is a turn the user can't talk
to it.

A review and broad test runs per task cost more than they find, so a task runs only the quick checks its own change
needs, then commits and lands it. Once the board drains, one review pass looks at everything landed since the last
pass, with the review and checks that range needs (CLAUDE.md, "Checks"), and fixes forward.

The mode lasts for the rest of the session once the user has asked for it. Every later message that is a task gets
dispatched, whether or not it mentions agents.

## What counts as a task

- **A task** is anything that needs reading code, editing, building, testing, running something or researching, even
  a small one. It gets an agent: the user asked for one per task, and a "quick" fix done inline still blocks the
  thread.
- **Not a task**: a question about the dispatch itself ("what's running?", "what did #2 change?"), or one answered
  from what this thread already holds. Answer those directly. Answer any question the user sends first, in the
  opening lines of your reply, before any status: the user is reading for that answer.
- **Installing the personal build isn't a task either.** When the user asks for an install, run
  `scripts/install-personal.sh` yourself from the main checkout, in the background, at once, and report when it
  finishes. It is one command, and someone asking for an install wants the new build now. Queueing it behind running
  agents only holds it up. "Install when #3 is done" waits for #3 to land, not for a free slot.
- **A follow-up to a running task** ("also make it red", "use the other endpoint") goes to that agent with
  `SendMessage` (by its name). It is the same task, and a second agent would collide with the first. A follow-up to a
  task that has already landed is a new task: a fresh agent, whose brief names the landed commit.
- **Several tasks in one message** become several agents, one each.

## Slots and the queue

- At most **2 agents run at once**, unless the user set another number ("up to 3 agents"). The number is the user's,
  and they can change it at any time; use the latest one they gave. The review pass holds a slot like a task.
  A task beyond the slots waits in a FIFO queue. Tell the user its place ("queued, #2 in line").
- When an agent finishes, start the next queued task in the same reply that reports the finished one.
- An agent that returned with a question for the user holds no slot while it waits. When the user answers, resume it
  with `SendMessage`. A resume counts toward the slots, and it goes before anything still queued.
- **A task that depends on another** (it needs the other's code, or says "after that…") waits until that one lands,
  even when a slot is free. Say so when you queue it.
- **Two tasks that touch the same code** can still run side by side in separate worktrees. The later one rebases onto
  the earlier one's commit when it lands. When both clearly rework the same function or file, run them one after the
  other: the second agent's conflict resolution would undo work it never saw.
- **Don't trim a task to keep it out of another's way.** A new task owns everything its message and screenshots
  show to be wrong. Leave a part of it to another task only when that task's own words (the user's message, not your
  reading of it) cover the same thing in the same place. Otherwise the part falls between the two briefs and nobody
  fixes it. If the scopes overlap, queue the new task after the other or let it rebase. Don't cut its scope.

Keep a small ledger, in this thread and in the ledger file (below), and show it when asked, or when it changes in a
way the user should know about:

| # | Task (a few words) | Agent | State |
|---|--------------------|-------|-------|
| 1 | Escape closes search | `escape-search` | landed `a1b2c3d`, unreviewed |
| 2 | pinned chats | — | queued (after #1) |
| 3 | Review `0f1e2d3..a1b2c3d` | `review-1` | running |

The states are queued, running, waiting on user, landed `<hash>`, and failed. A landed task stays unreviewed until a
review pass covers its commit.

## The ledger file

The ledger also lives in `<scratchpad>/dispatch-ledger.json`, `<scratchpad>` being the session's scratchpad dir from
the system prompt, in every session (inside ngterm the status page reads it). Write it when dispatch mode starts, and
again on every change (a dispatch, a state change, a landing, a note, a new slot count, a new review base), to a temp
file renamed over it, so a read never catches half a file:

```json
{"slots": 2, "review_base": "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c", "tasks": [
  {"num": 1, "title": "Escape closes search", "agent": "escape-search", "state": "landed a1b2c3d"},
  {"num": 2, "title": "pinned chats", "agent": "", "state": "queued", "after": 1, "note": "needs #1's view"},
  {"num": 3, "title": "Review 0f1e2d3..a1b2c3d", "agent": "review-1", "state": "running", "flow": "review"}
]}
```

- `slots` is the user's number of agents at once (2 when they gave none).
- `review_base` is the commit up to which everything landed has been reviewed: the next pass starts from it. Set it when dispatch mode starts to `git rev-parse origin/main` (after a `git fetch`), and move it only when a
  pass returns (below). It is kept in the file so it survives this thread's context being compacted.
- A task's `state` is the ledger's (`queued`, `running`, `waiting on user`, `landed <hash>`, `failed`). Optional:
  `note` (a line the user should see on the card), `after` (the task a queued one waits for), `transcript` (the
  agent's JSONL, only when its name doesn't find it: the server looks names up in this session's subagents dir) and
  `flow`: `review` for the review pass (always set it: it runs in a worktree, which would read as a task),
  otherwise `edit` or `scratch`, told by whether the agent has a worktree when left out.

## The status page

Inside ngterm (`NGTERM_ENABLED=1`), the ledger is also shown as a live page in the session's right pane, by default:
one card per task with its phase, what its agent is doing now and an estimated finish, read from the agents'
transcripts (this skill's `dispatch-status/`). A task's phases are investigate, implement, check, commit, land; the review
pass's are review, gate (its checks), land. The user watches it instead of asking "what's running?". Set it up in the
reply that starts dispatch mode:

1. **Write the ledger file** (above) before the server starts.
2. **Start the server** with `run_in_background`, and keep its shell ID:

   ```sh
   <repo>/.claude/skills/task-dispatch/scripts/status-page.sh --ledger <scratchpad>/dispatch-ledger.json
   ```

   The script builds the server when it was never built or its sources changed (in its own target dir, so it never
   waits on another build's lock: well under a minute cold), and otherwise starts it at once. It serves `http://127.0.0.1:47231/` and prints one ready line. A port in use (another session's page) gets `--port
   <n>` and that port in the URL below.
3. **Open it in the right pane.** The split must be shown: if `ngtermctl tree --json` gives this session
   (`$NGTERM_SESSION_ID`) `"split": false` or no `hasSplit`, run `ngtermctl session split on --axis vertical --target
   "$NGTERM_SESSION_ID" --socket "$NGTERM_SOCKET"` first. Then:

   ```sh
   ngtermctl session overlay open --url http://127.0.0.1:47231/ --js --chromeless --pane right \
     --target "$NGTERM_SESSION_ID" --socket "$NGTERM_SOCKET"
   ```

4. **Stop it** when dispatch mode ends: `TaskStop` the server's shell and `ngtermctl session overlay close --pane right
   --target "$NGTERM_SESSION_ID" --socket "$NGTERM_SOCKET"`. When the session itself ends, the server exits on its
   own: it watches the Claude Code process that started it (`$CLAUDE_PID`).

Outside ngterm no server is started. Mention once that the page can be had at `http://127.0.0.1:47231/`, and start it
as in step 2 if the user wants it.

The server is a Rust crate of the skill, with its own workspace and lockfile: no part of the app, its builds or its
coverage gate, and CI doesn't run it. A change to it runs its own checks there: `cargo test`, `cargo clippy
--all-targets` and `cargo fmt --check` in `dispatch-status/`.

## Starting an agent

Use the `Agent` tool with `subagent_type: "general-purpose"`, a short `description`, and a `name` the user can tell
apart (`escape-search`, not `agent-3`). Don't use `fork`: the dispatcher's context is a pile of other tasks, and a
clean brief serves the agent better.

- **A task that edits the repo** runs with `isolation: "worktree"`. Several agents in one checkout would overwrite
  each other's files and break each other's builds. The worktree starts at main's last commit, without the main
  checkout's uncommitted changes. If a task needs those, say so in the brief, or ask the user to commit them first.
- **A read-only task** (an investigation, a question about the code, a screenshot, reading the personal build's data)
  runs in the main checkout, with no worktree.

### The brief

The agent knows only what the brief says. CLAUDE.md loads by itself, so the brief doesn't repeat its rules; the
user's memories and this conversation don't load. Write the brief the way you'd hand the task to a colleague:

1. **The task in the user's own words**, quoted. Then add what you know that bears on it: earlier tasks of this
   session it touches, files or crates already mentioned, the user's corrections or preferences that apply. Don't add
   your guess at the solution: an agent handed a hypothesis tends to come back with it confirmed.
2. **How to work**:
   - Use the `teams-ui` skill for anything the app shows.
   - Decide unclear details yourself, the way the user most likely means them, and say in the report what you chose.
     Come back with a question only when no reasonable choice exists or the step can't be undone. Don't stop for "rule
     questions": CLAUDE.md's hard rules say which changes need sign-off.
   - **Build the whole feature the task asks for.** For a Teams feature, that means everything the user's screenshot
     or real Teams shows (see the `teams-ui` skill's "Find the Teams reference"). A control left disabled, a "not
     available yet" note or a "Not done" list in the report is unfinished work. If a part needs a capture, a read or a
     lab run nobody has made yet, make it. Leave a part out only when it truly can't be built now, and say in the
     report what blocks it.
   - Run anything over ~30 s (cargo, tests) with `run_in_background` and pick it up from its notification. Don't
     write polling loops: `until ! pgrep -f …` matches its own command line and never ends.
   - **When the session runs under a CPU cap** (the `cpu-cap` skill), the brief gives its size: run cargo with
     `CARGO_BUILD_JOBS=<n>` and tests with about n threads (`-- --test-threads=<n>`). Read n from
     `~/.claude/skills/cpu-cap/scripts/cpu-cap.sh status` before writing the brief: its `quota:` line, rounded down,
     at least 1. With `cap: none` or `quota: none`, leave the bullet out.
   - **Leave nothing running or lying around.** Your background shells outlive you: they keep running after you
     report. So before you report, stop every one you started (`TaskStop`), finished or not, along with anything they
     spawned (a test binary, a dev server, a browser), and delete every file you made outside the worktree (scratch
     scripts, logs, screenshots in `/tmp` or the scratchpad), unless the report hands it to the user. The dispatcher
     removes the worktree itself.
   - Install the personal build only when this task asks for it. The one process you leave running is the installed
     `ngteams`, when the task asked you to start it for the user. Start it detached, with its working directory
     outside the worktree, so the worktree's cleanup doesn't kill it:
     `cd ~ && setsid -f ~/.local/bin/ngteams >/dev/null 2>&1`. Say in the report that it is running.
3. **For an editing task, how to finish**, in this order:
   1. Run the quick checks the change needs, and no more (CLAUDE.md, "Checks"): `cargo fmt --all` and `cargo check
      -p <touched crates> --all-targets` at least, plus the test or two that show your own change works, and for a UI
      task the screenshots the `teams-ui` skill names. Broader suites and review are the review pass's.
   2. Commit on the worktree's branch with a conventional message. Add no AI attribution and no Co-Authored-By line.
   3. Land it by running `<repo>/.claude/skills/task-dispatch/scripts/land.sh` from inside the worktree, in the
      background. Give the path in full, because the worktree has its own copy of the repo. It rebases onto
      `origin/main`, checks the whole workspace compiles (the one check that catches a caller main's newer commits
      added elsewhere), pushes the branch as `origin/main` under a lock shared by every worktree, and fast-forwards
      the main checkout. Its header lists each exit code and what to do next. On exit 4 nothing was pushed: stop and
      report it, and never stash, reset, force-push or touch the main checkout's files.
4. **The report**: a few lines. Say what changed and why, the landed hash (`LANDED <hash>`) or why it didn't land,
   what was checked (it compiles; any test or screenshot it ran) and that the change is unreviewed, the
   choices you made on unclear points, and anything left for the user. End with the IDs of the background shells you
   started, each marked stopped. Put a question for the user first.

## The review pass

**When it starts.** When the board drains (no agent running, nothing queued; an agent waiting on the user doesn't
count, its work isn't landed), and `origin/main` (after a `git fetch`) has moved past the ledger's `review_base`,
start one review agent in the same reply that reports the last task. Record the range in the ledger first: `<base>` is
`review_base`, `<head>` is `git rev-parse origin/main` now. Add it to the ledger as a task with `"flow": "review"`,
titled `Review <base7>..<head7>`, its agent named `review-<n>`. It runs with `isolation: "worktree"` (it fixes and
lands like a task) and holds a slot. Nothing has landed since the base: no pass. A range of prompt or Markdown edits
only needs no pass: move `review_base` to `<head>` and say so.

**While it runs**, a new task is dispatched as usual and lands on top. Its commit isn't in the pass's range: the next
pass covers it.

**Its brief** carries the same "How to work" bullets as a task's (2 above), then:

1. The range `<base>..<head>`, and that the worktree starts at `<head>` or past it (`git merge-base --is-ancestor
   <head> HEAD` says so).
2. Size the pass to what the range can break (CLAUDE.md, "Checks"). Code that can break something gets a revmux round
   (the `revmux:revmux` skill, the range as the diff); fix every major finding, the immaterial and pre-existing ones
   too, and review the fixes again only when they are big enough to need it. Then the checks the range needs over
   the crates it touched, such as clippy, `scripts/lint-report.sh` and the tests its changes can affect
   (`fixture-guard` when it touched `docs/` or a `tests/fixtures/`). Rerun only what failed after a fix. A test that
   fails is run again alone: one that then passes is a load flake (README, "Known flakes under load"); one that
   fails or passes by turns alone is a real flake, fixed when its cause is found and otherwise reported. A fence
   breach is never a flake. Commit fixes as they go, conventional messages, no AI attribution.
3. Land the fixes with `land.sh`, as a task does. A rebase conflict with a task that landed meanwhile is resolved by
   the pass. A pass that found nothing to fix has nothing to land.
4. The report: the range, what was reviewed and checked and what was left out, what each round fixed, every test
   that failed with its verdict (fixed, load flake that passes alone, real flake), the landed hashes (`LANDED <hash>`
   each) or why they didn't land, and the background shells, each marked stopped.

**When it returns**, move `review_base`: to the pass's last landed commit when nothing but the pass's own commits
landed after `<head>`, otherwise to `<head>` (the pass's fixes then come up again in the next range, which costs
little). A pass that couldn't land (exit 4, or a fix it couldn't make) leaves `review_base` where it was; report what
is left. Clean up after it as after a task (below). If tasks landed while it ran and the board is drained again,
start the next pass.

## When an agent comes back

The completion arrives as a notification. In one reply:

1. Answer anything the user asked meanwhile.
2. Report the task in two or three lines: what it did, the commit and that it is unreviewed until the next pass, and
   anything the user must decide or check. For the review pass: what it reviewed and checked, what it found and
   fixed, each failing test's verdict, its commits. Relay the substance, because the agent's report isn't shown
   to the user. A failure is reported as one, with its reason, never smoothed over. Anything the agent left undone
   of what its task asked for (a disabled control, a "Not done" item) is still that task's work. Queue a follow-up
   task for it in the same reply, and say so. Don't offer one and wait for a yes: the user asked for the whole
   feature the first time.
3. **Once the task is done, nothing of it stays.** The worktree's `target/` alone is gigabytes, and stale worktrees
   fill the disk. Each step removes one kind of leftover:
   - Stop every shell the agent started that is still running (`TaskStop`), whatever its report says. The one
     exception is the personal `ngteams` app the task was asked to start for the user. Look at the
     session's background tasks, not only the IDs the report lists: a returned agent's shells keep running. Do this
     before saying "nothing is running".
   - If it landed, run `<repo>/.claude/skills/task-dispatch/scripts/cleanup.sh <worktree path>`. The script stops any
     process still running inside the worktree, then removes the worktree with its `target/`, its branch and git's
     record of it. It refuses unless the branch is on `origin/main` and the worktree has nothing uncommitted, so it
     never throws work away.
   - Delete any file outside the worktree that the agent says it left.
   - An agent that changed nothing leaves no worktree: the harness removes an unchanged one by itself.
   - A task that didn't land isn't done. Keep its worktree while it is being fixed or waits on the user, say where it
     is, and keep it out of new work. Once it lands, clean it up as above. If the user drops the task, run the script
     with `--discard`, which also deletes the unlanded work. Run it only on the user's word.
4. Start the next queued task, if a slot is free. If none is queued and nothing runs, start the review pass
   (above).

If an agent's report is unclear or looks wrong (it claims it landed but `git log origin/main` doesn't show the commit,
or tests "pass" with no output), check the claim before passing it on: one `git` command, not a re-run of its work.

## Keep the thread light

The dispatcher's replies stay short, because the user is busy sending tasks:

- A dispatch takes a line or two: "Started `escape-search` (#1) on the Escape fix, in its own worktree."
- Don't narrate what an agent is probably doing, and don't predict its result. If the user asks before it's done, say
  it's still running.
- When you offer an option, state the default and carry on with it. An unanswered offer means continue.
