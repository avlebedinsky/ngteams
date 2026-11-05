# ngteams dispatch conventions

Apply these when working in ngteams. The repository's current `AGENTS.md` and user instructions govern
project rules; this reference does not duplicate them.

- Read `AGENTS.md`, especially Checks, and any instructions applicable in the worker's worktree. For anything the app
  shows, read and apply `.agents/skills/teams-ui/SKILL.md` even when a host cannot invoke it as a named skill.
- Build the whole feature shown in the user's request/screenshots and the Teams reference. Obtain the needed
  permitted captures, reads, and lab runs. Disabled controls and placeholder messages count as unfinished work.
- A task's quick checks include `cargo fmt --all`, `cargo check -p <touched-crate> --all-targets`, relevant focused
  tests, and the UI skill's screenshots where applicable. The landing helper also checks workspace compilation.
  Task workers never run revmux or another multi-agent review; only the review pass does.
- For code that needs it, the review pass runs the available revmux skill once, as one task directory over the whole
  frozen range (every task landed since the last successful pass), not once per task or commit. Run the checks that
  range needs: for example clippy, `scripts/lint-report.sh`, affected tests, and `fixture-guard` for documentation or
  fixtures. Follow repository rules when deciding whether a failure is a flake. Stop and report a run that
  reaches someone outside the allowed lab contacts; never dismiss it as a flake.
  Fix every major finding, including immaterial and pre-existing ones. The only other revmux round re-reviews the
  pass's own fixes, when their size warrants it.
- Don't stop for rule questions: `AGENTS.md` states what needs sign-off. Use background-command notifications or
  session handles, never `pgrep -f` wait loops that can match their own command line.
- If an existing CPU cap applies, propagate its quota into cargo jobs and test threads. Inspect the cap using its
  available tool/skill; do not require a missing `~/.claude/skills/cpu-cap` installation.
- Commit with a conventional message and no AI attribution or Co-Authored-By line.
- An explicit personal-build installation request is handled promptly from the main checkout with
  `scripts/install-personal.sh`, in the background, outside the task queue. An install conditioned on another task
  waits for that task to land. Follow current personal-build rules. Start the installed app only when requested,
  detached from a working directory outside a disposable worktree, and preserve that requested process on cleanup.
- The shared `scripts/land.sh` and `scripts/cleanup.sh` currently target `origin/main`; `land.sh` runs cargo checks.
  These are ngteams defaults, not assumptions about every repository. Adapt explicit remote/branch/check settings
  before reusing the helpers elsewhere.
