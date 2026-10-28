---
name: task-dispatch
description: Session mode where the user sends tasks one after another and every task goes to a dedicated background agent of its own, up to 4 at once, the rest queued in order — each editing agent in its own git worktree, finishing with revmux, the gate, its own rebase, merge and push to main, and nothing of the task left behind afterwards (worktree, target dir, branch, shells, temp files) — while this thread stays free to take the next task, answer questions and report results. Use it whenever the user says they'll send several tasks one by one, "start an agent for each task", "dispatch these", "run tasks in parallel", "up to N agents", "agent per task", "в этой сессии буду кидать задачи по одной", "запускай агента на каждую задачу", or otherwise wants this conversation to act as a dispatcher rather than do the work itself — and keep using it for every later task in that session, even when a later message doesn't mention agents.
---

# Task dispatch

The user wants to keep firing tasks at this session without waiting for each one to finish. So this thread is a
dispatcher: it turns each task into a well-briefed background agent, keeps at most 4 running, and reports what they
did. It does no task work itself, because a turn spent building or debugging is a turn the user can't talk to it.

The mode lasts for the rest of the session once the user has asked for it. Every later message that is a task gets
dispatched, whether or not it mentions agents.

## What counts as a task

- **A task** is anything that needs reading code, editing, building, testing, running something or researching, even
  a small one. It gets an agent: the user asked for one per task, and a "quick" fix done inline still blocks the
  thread.
- **Not a task**: a question about the dispatch itself ("what's running?", "what did #2 change?"), or one answered
  from what this thread already holds. Answer those directly. Answer any question the user sends first, in the
  opening lines of your reply, before any status: the user is reading for that answer.
- **A follow-up to a running task** ("also make it red", "use the other endpoint") goes to that agent with
  `SendMessage` (by its name). It is the same task, and a second agent would collide with the first. A follow-up to a
  task that has already landed is a new task: a fresh agent, whose brief names the landed commit.
- **Several tasks in one message** become several agents, one each.

## Slots and the queue

- At most **4 agents run at once**. A task beyond that waits in a FIFO queue. Tell the user its place ("queued, #2
  in line").
- When an agent finishes, start the next queued task in the same reply that reports the finished one.
- An agent that returned with a question for the user holds no slot while it waits. When the user answers, resume it
  with `SendMessage`. A resume counts toward the 4, and it goes before anything still queued.
- **A task that depends on another** (it needs the other's code, or says "after that…") waits until that one lands,
  even when a slot is free. Say so when you queue it.
- **Two tasks that touch the same code** can still run side by side in separate worktrees. The later one rebases onto
  the earlier one's commit when it lands. When both clearly rework the same function or file, run them one after the
  other: the second agent's conflict resolution would undo work it never saw.

Keep a small ledger in your head and show it when asked, or when it changes in a way the user should know about:

| # | Task (a few words) | Agent | State |
|---|--------------------|-------|-------|
| 1 | Escape closes search | `escape-search` | running |
| 2 | pinned chats | — | queued (after #1) |

The states are queued, running, waiting on user, landed `<hash>`, and failed.

## Starting an agent

Use the `Agent` tool with `subagent_type: "general-purpose"`, a short `description`, and a `name` the user can tell
apart (`escape-search`, not `agent-3`). Don't use `fork`: the dispatcher's context is a pile of other tasks, and a
clean brief serves the agent better.

- **A task that edits the repo** runs with `isolation: "worktree"`. Four agents in one checkout would overwrite each
  other's files and break each other's builds. The worktree starts at main's last commit, without the main checkout's
  uncommitted changes. If a task needs those, say so in the brief, or ask the user to commit them first.
- **A read-only task** (an investigation, a question about the code, a screenshot, reading the personal build's data)
  runs in the main checkout, with no worktree.

### The brief

The agent knows only what the brief says. CLAUDE.md loads by itself, but the user's memories and this conversation
don't. Write the brief the way you'd hand the task to a colleague:

1. **The task in the user's own words**, quoted. Then add what you know that bears on it: earlier tasks of this
   session it touches, files or crates already mentioned, the user's corrections or preferences that apply. Don't add
   your guess at the solution: an agent handed a hypothesis tends to come back with it confirmed.
2. **How to work**:
   - Follow CLAUDE.md's hard rules, patterns and gate. Use the `teams-ui` skill for anything the app shows.
   - Decide unclear details yourself, the way the user most likely means them, and say in the report what you chose.
     Come back with a question only when no reasonable choice exists or the step can't be undone. Don't stop for "rule
     questions": CLAUDE.md's first rule says which changes need sign-off.
   - Run anything over ~30 s (cargo, tests, revmux) with `run_in_background` and pick it up from its notification.
     Never use polling loops: `until ! pgrep -f …` matches its own command line and never ends.
   - **Leave nothing running or lying around.** Your background shells outlive you: they keep running after you
     report. So before you report, stop every one you started (`TaskStop`), finished or not, along with anything they
     spawned (a test binary, a dev server, a browser), and delete every file you made outside the worktree (scratch
     scripts, logs, screenshots in `/tmp` or the scratchpad), unless the report hands it to the user. The dispatcher
     removes the worktree itself.
   - The personal build is installed or run only as CLAUDE.md allows. Install it only when this task asks for it.
     The one process you leave running is the installed `teams`, when the task asked you to start it for the user.
     Start it detached, with its working directory outside the worktree, so the worktree's cleanup doesn't kill it:
     `cd ~ && setsid -f ~/.local/bin/teams >/dev/null 2>&1`. Say in the report that it is running.
3. **For an editing task, how to finish**, in this order:
   1. A revmux round on the change (the `revmux:revmux` skill). Fix every major finding, the immaterial and
      pre-existing ones too, and re-run revmux on the fixes until it is clean.
   2. The gate over the crates the change touched: `cargo fmt --all --check`, `cargo clippy -p … --all-targets
      --all-features -- -D warnings`, `scripts/lint-report.sh`, `cargo test -p …`, plus `cargo test -p fixture-guard`
      when `tests/fixtures/` or `docs/` changed.
   3. Commit on the worktree's branch with a conventional message. Add no AI attribution and no Co-Authored-By line.
   4. Land it by running `<repo>/.claude/skills/task-dispatch/scripts/land.sh` from inside the worktree, in the
      background. Give the path in full, because the worktree has its own copy of the repo. The script first rebases
      the branch onto `origin/main` and runs `cargo check --workspace --all-targets`. The per-crate gate doesn't
      compile a crate the change didn't touch, so this check is what catches a caller that main's newer commits
      added somewhere else. Then it waits for a lock shared by every worktree, rebases once more onto whatever
      landed meanwhile (checking again if that moved the branch), pushes the branch as `origin/main`, and
      fast-forwards the main checkout to it. Only the task's own commits are pushed. Its exit code says what to do
      next:
      - **2** (rebase conflict): resolve it with `git rebase origin/main`, rerun the gate on the touched crates, and
        run the script again. A resolution that changes real logic gets its own revmux round first.
      - **3** (the workspace doesn't compile on the rebased branch): fix it, commit, and run the script again.
      - **4** (the change can't land cleanly: the main checkout isn't on `main`, its `main` holds commits origin
        doesn't, its uncommitted files overlap the change, or the branch started from commits origin doesn't hold):
        nothing was pushed. Stop and report it. Never stash, reset, force-push or touch the main checkout's files.
      - **5** (the fetch or the push failed): nothing changed locally. Run the script again, and if it fails twice,
        report it.
      - **6** (pushed, but the main checkout couldn't fast-forward): the change is on `origin/main`. Report that the
        main checkout is behind.
4. **The report**: a few lines. Say what changed and why, the landed hash (`LANDED <hash>`) or why it didn't land,
   the revmux and gate outcome, the choices you made on unclear points, and anything left for the user. End with the
   IDs of the background shells you started, each marked stopped. Put a question for the user first.

## When an agent comes back

The completion arrives as a notification. In one reply:

1. Answer anything the user asked meanwhile.
2. Report the task in two or three lines: what it did, the commit, and anything the user must decide or check. Relay
   the substance, because the agent's report isn't shown to the user. A failure is reported as one, with its reason,
   never smoothed over.
3. **Once the task is done, nothing of it stays.** The worktree's `target/` alone is gigabytes, and stale worktrees
   fill the disk. Each step removes one kind of leftover:
   - Stop every shell the agent started that is still running (`TaskStop`), whatever its report says. The one
     exception is the personal `teams` app the task was asked to start for the user. Look at the
     session's background tasks, not only the IDs the report lists. A returned agent's shells keep running: wait
     loops left behind have run for hours. Do this before ever saying "nothing is running".
   - If it landed, run `<repo>/.claude/skills/task-dispatch/scripts/cleanup.sh <worktree path>`. The script stops any
     process still running inside the worktree, then removes the worktree with its `target/`, its branch and git's
     record of it. It refuses unless the branch is on `origin/main` and the worktree has nothing uncommitted, so it
     never throws work away.
   - Delete any file outside the worktree that the agent says it left.
   - An agent that changed nothing leaves no worktree: the harness removes an unchanged one by itself.
   - A task that didn't land isn't done. Keep its worktree while it is being fixed or waits on the user, say where it
     is, and keep it out of new work. Once it lands, clean it up as above. If the user drops the task, run the script
     with `--discard`, which also deletes the unlanded work. Run it only on the user's word.
4. Start the next queued task, if a slot is free.

If an agent's report is unclear or looks wrong (it claims it landed but `git log origin/main` doesn't show the commit,
or tests "pass" with no output), check the claim before passing it on: one `git` command, not a re-run of its work.

## Keep the thread light

The dispatcher's replies stay short, because the user is busy sending tasks:

- A dispatch takes a line or two: "Started `escape-search` (#1) on the Escape fix, in its own worktree."
- Don't narrate what an agent is probably doing, and don't predict its result. If the user asks before it's done, say
  it's still running.
- When you offer an option, state the default and carry on with it. An unanswered offer means continue.
