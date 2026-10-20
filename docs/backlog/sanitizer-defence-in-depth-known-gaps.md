---
worth: later
added: 2026-10-20
---
# sanitizer is defence in depth; known gaps

After six review rounds on M0 Task 9 the sanitizer (`crates/proto-lab/src/sanitize*`) and the fixture guard's
invitation labels (`crates/fixture-guard/src/labels.rs`) still had edge-case leaks, so the user changed the rule:
captures never enter the repo, not even sanitized. `proto-lab export` only writes a local reference copy under the
lab's data dir, and committed fixtures are written synthetic-from-capture with made-up values and a `_synthetic`
marker that the guard enforces. The sanitizer stays as defence in depth for that local copy. These are its known gaps.
Examples below describe shapes, not values.

Found in review round 6 (`~/.local/state/plan-exec-revmux/20261019-native-teams-m0-lab-and-spikes/task9-r6-findings.txt`;
its two invitation-label gaps, a digit-prefixed passcode cut short and German's missing `Kenncode`, were fixed in M1
Task 6):

- **Dotted display names whose last part is also a TLD** (`crates/proto-lab/src/sanitize/words.rs`,
  `without_host_tld` and `is_host_name`). A lowercase `<first>.<surname>` whose surname is a Public Suffix List
  top-level entry (`kim`, `wang`, `green`, `ford`, …) loses the surname word. Three lowercase labels ending in such
  an entry lose every word. The export's name backstop (`sanitize/backstop.rs`, `words`) uses the same split, so it
  doesn't catch either case. Fix: keep every word of a dotted name part unless the part is an email, URL or MRI. If
  dropping `com`-like labels is still wanted, limit it to a short fixed list of infrastructure TLDs, and apply the
  same limit to the 3-label host rule.

Known limits, documented since earlier rounds:

- **Ordinary-word over-redaction**: a name word that is also an ordinary word (or a JSON key, enum value or path
  segment) is replaced everywhere, damaging structure, apart from inside a host: there it is left, and export warns.
- **Names never seen under a name field** are not learned, so they are neither replaced nor caught by the backstop.
- **Words under 3 characters** are outside the name backstop.

The open question that decides whether this is worth fixing: will a reference copy, or anything derived from one by
something other than a hand-written synthetic fixture, ever leave the local machine or get committed? While the
synthetic-from-capture rule and the guard's marker hold, these gaps only affect a local file that only the lab reads.
If that changes, or a real capture shows one of these shapes in a field an agent copies by hand, fix them first.
