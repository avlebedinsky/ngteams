---
worth: later
where: crates/proto-lab/src/sanitize.rs:105
added: 2026-10-20
---
# Fix Task 9's two remaining sanitizer findings in another fixer pass after the review cap?

Raised in M0 Task 9 (capture recorder, sanitizer, `proto-lab export`) of
`docs/plans/20261019-native-teams-m0-lab-and-spikes.md`. Task 9 used up its review budget: 3 revmux rounds, then one
extra fixer and the `04-after-decision` round that decision-01 allowed. Gating findings per round went 8 → 3 → 2 → 2,
and each round's findings were new mechanisms. The last round, on `m0-task-9-capture-r3` (a870d19), still reports two:

- **CRITICAL `crates/proto-lab/src/sanitize.rs:105`**: the `p` query parameter of a Teams meeting link (the join
  passcode) survives sanitizing. `p` is in neither secret-key list, and the value is shorter than the 32-character
  opaque-token minimum. The numeric meeting id survives too. So export would write a join credential.
- **MAJOR `crates/proto-lab/src/sanitize.rs:721`**: `name_variants` splits names on punctuation, so from an
  email-shaped display name (`jane.doe@example.com`) it learns domain labels such as `com` and replaces them everywhere.
  Every `*.com` host in the capture becomes `<name:N>`. The export still succeeds and the fixture is useless.

Full findings: `~/.local/state/plan-exec-revmux/20261019-native-teams-m0-lab-and-spikes/task9-r4-findings.txt`.

## The question

Should these get one more fixer and a review round, beyond the plan-exec-revmux rule "after the after-decision round, no
further fixer or round" (SKILL.md:443-448) and decision-01's terminal condition? The alternative is to leave Task 9
incomplete.

- **For (Claude):** the user asked that every major be fixed, including immaterial and pre-existing ones (progress
  log, 2026-10-19 21:57). Both fixes look small. With Task 11's merge held, an unmerged r4 branch exposes nothing.
- **Against (Codex):** decision-01 already weighed that instruction when it set the terminal condition. The once-per-task
  cap limits how many attempts the orchestrator may grant, and a second "this is the last one" is no stronger a
  boundary than the first. The fix-every-major rule decides which findings need a fix; it does not lift the cap.

**What settles it:** whether the user's fix-every-major instruction outranks the per-task attempt cap. Only the user
can answer that. If it does, run the repair below as `m0-task-9-capture-r4` from a870d19, review it with revmux, and
merge when it comes back clean.

## Repair directions, if approved (agreed with Codex)

- **Meeting links:** match the real Teams hosts and join-path formats (`teams.microsoft.com/meet/…`,
  `/l/meetup-join/…`, `teams.live.com/meet/…`) at every encoded layer. Use exact host matches, not suffix matches that
  let lookalike domains through. Replace passcodes and meeting ids with placeholders. fixture-guard must separately
  reject a surviving meeting credential, encoded forms included. Don't assume every `p` in every URL is a secret;
  check any other parameter names and URL formats before adding them.
- **Name learning:** fix the replacement (`name_variants`) and also the backstop's own word split
  (`sanitize/backstop.rs:53-59`). Changing only one of them turns the corruption into a refused export. Text that looks
  like an identifier (email, UPN, MRI, domain) must not add its domain labels as name words. Recognized names must
  still be fully redacted, and the existing `Doe, Jane` regression tests must still pass. A rule like "never learn a
  label of a dotted token" is too broad if it stops redacting real dotted personal names.
- **Hosts survive:** "the host appears somewhere in the output" is too weak a check. Compare each URL occurrence for a
  defined set of public protocol hosts, and allow the identifier replacements that are meant to happen. Tenant and
  hosted domains are redacted on purpose (`sanitize.rs:14-16`, `sanitize.rs:292-296`), so a check on every host would
  reject valid exports. Whether this is a runtime check or targeted tests is up to the implementation.
- **Regression tests:** the reported short passcode never reaches a written fixture. Email-shaped display names leave
  public endpoint hosts intact. Contaminated output is refused, and an existing fixture is left unchanged. A normal
  export still succeeds.

## What the run did instead (decision-02)

- decision-01's whole-run stop was explicitly narrowed to the Task 9 chain. Task 9 is not merged and not done. Task 11
  (which merged r1's sanitizer and wires `export`) is not merged into the run branch. Task 18's export did not run.
- Work that doesn't touch sanitize or export went on.
- The plan stays open, with Tasks 9, 11 and 18 unchecked.

To undo, if the answer is "fix it": run the repair above. When it comes back clean, merge Task 9, merge the run branch
into Task 11 so its sanitizer and export become Task 9's version, then merge Task 11 and continue with Task 18.

Transcript: `~/.local/state/plan-exec-revmux/20261019-native-teams-m0-lab-and-spikes/discussions/decision-02/transcript.md`
