---
worth: yes
where: crates/proto-lab/src/arming.rs:429
added: 2026-10-20
---
# An arming step that times out under load counts as a breach: real or false?

Found by the M0 acceptance check (`docs/m0-acceptance.md`, criterion 2). On a loaded machine (load average 14–47 on
20 CPUs), one arming step of a target still held by the debugger can go unanswered for `STEP_TIMEOUT` (10 s,
`arming.rs:114`). Arming then gives up on the target and closes it. Depending on how that goes, the run is reported
as breached. The open question is whether such a breach is real (something of the target could have run unguarded)
or false (the target never ran, and the verdict only reflects that its close wasn't confirmed).

**Current behaviour** (Task 11 live fix, `ArmingFailure::closed_before_it_ran`, `FenceReport::breached`):

- the failed step is the resume itself (`Runtime.runIfWaitingForDebugger` sent and refused or unanswered): the
  target may have run, so it is a **breach**;
- an earlier step failed and `Target.closeTarget` confirmed the close: nothing ran, so it is **no breach**, only a
  warning in the notes ("target(s) not armed in time were closed before they ran");
- an earlier step failed and the close wasn't confirmed (no answer, an error, `success: false`, or no target id): this
  is a **breach** too, even though the resume was never sent.

**Evidence, `ab-chat` run 1 (2026-10-20)**: an iframe's `Network` step (`Network.enable`) got no answer within 10 s.
That step comes before the resume, so the iframe was never resumed. The report counted a breach, so by the code its
close wasn't confirmed either. Browser-level `Fetch` stayed on throughout, and no frame was sent without an allow.
Run 2, right after it on the same build, was clean. The arming failure's details (step, close result) survive only in
that run's printed output: `fence.log` doesn't record arming failures.

**Why it is worth doing either way**: if the breach is real, the fence has a hole under load that needs closing. If it
is false, every loaded run risks a breach verdict that has to be read by hand, the same way the exit code is today
(`ab-chat-exit-code-counts-unreviewed-denies.md`).

**What would settle it**:

1. On Chrome 155, check what happens to a target held by `waitForDebuggerOnStart` whose arming gave up before the
   resume and whose close failed or went unanswered. Does it stay paused until the browser closes, or can anything
   resume it (its session detaching, the parent frame navigating, the connection dropping)? Test it with a page whose
   held iframe would send a request and a frame as soon as it runs, and assert that the server gets nothing.
2. Record each arming failure (target type, failed step, whether it was the resume, close result) in the capture or
   `fence.log`, so a breach can be judged after the run, not only from its output.
3. Depending on 1: if a held, never-resumed target can't run, an unconfirmed close before the resume becomes a warning
   rather than a breach (re-checking with `Target.getTargets` that the target is gone or still held). If it can run,
   the close has to be confirmed (retried, or the browser killed). Either way, consider a step timeout that allows for
   load, or a retry of the timed-out step before giving up.
