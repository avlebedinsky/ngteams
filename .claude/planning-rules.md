- Do the task's actual job first: the feature working end to end. The fence and AGENTS.md's hard rules are already
  built and settled; follow them, but don't add fence tightening, new denials or new guards the task doesn't ask for.
- The lab accounts (a, b, c) are known bot accounts nobody outside contacts. Don't design, test or fix for an outsider
  calling, messaging or inviting them, beyond what the existing fence already does.
- A review finding whose only failure needs an outsider to contact a lab account, or that only makes a
  defence-in-depth check stricter while the fence still blocks the write, is minor: fix it only if it is small, and
  never widen it into a redesign.
- When writing a plan: plan the feature's work, not extra guard layers. A task item that only hardens the fence for
  a case no realistic sequence reaches doesn't belong in it.
