---
worth: later
where: docs/personal-build.md
added: 2026-10-22
---
# Approve or amend the personal build's rules (`docs/personal-build.md`) and their CLAUDE.md text

M2 Task 26 (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`) wrote up the personal build from the
direction of 2026-10-21: the `teams` binary and its data, its fence, its reads, its sign-in, what the lab can't show,
the first run and Conditional Access, what "agents never build a personal build" means, how lab and personal binaries
are kept apart, and the CLAUDE.md text it needs. The task ends at a stop for the user's decision; the run went on
without it, so this item holds the decision. **Task 27 (the personal build) waits on it.**

What is asked: approve `docs/personal-build.md` and its proposed CLAUDE.md text, or amend them. The choices that
matter most (each argued in the doc):

1. **Choice A**: the personal fence permits a write when a fresh read of the chat lists the account (membership), not
   only when the whole roster is read as in the lab; a read that doesn't show the account is denied.
2. **Choice B**: a member in a form no capture showed (a guest, another tenant, a consumer account) makes the write
   `Unknown`, so such chats stay read-only until the lab captures that form.
3. **Choice C**: the sign-in window in a `--personal` mode whose egress gate reaches only Entra's sign-in hosts and
   the identity provider's hosts the user names, with the guard's two redirect gaps
   (`webkit-sign-in-guard-redirects.md`) accepted knowingly because they reach no Teams host; the alternative is to
   wait for a complete guard.
4. `PostRead` (profile lookups, `getpresence`) for anyone the account's stored chats list; typing, read receipts and
   presence sent only after the first-run screen is confirmed.
5. Agents compile the `teams` crate and test its lib on the mock only: no runnable (non-`test-support`) personal
   binary in any profile, no install, no run, no access to its data, keyring items or logs. This is stricter than the
   plan's wording ("a release or installed personal binary"), since a debug build reaches the tenant as well.

The unknown that settles it: the user's judgment of the trade-offs above (how much of daily use to give up for
caution, and whether the sign-in gaps are acceptable for the user's own sign-in); nothing in the repo can settle it.

Meanwhile: no code that loosens the fence was written and CLAUDE.md is unchanged; the doc is a proposal only. On
approval, CLAUDE.md gets the approved text first, then Task 27 implements it; to reject, amend or delete the doc and
this item. No Codex discussion was held: the doc puts each open choice to the user rather than deciding it.
