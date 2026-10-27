---
worth: later
where: crates/teams-core/src/outbox.rs, docs/personal-build.md (the decision of 2026-10-27)
added: 2026-10-27
---
# The personal build's failed sends: retry them as Teams does?

On 2026-10-27 the user decided the personal build is the user's Teams: its fence logs every send and denies none, and
whether a failed send is tried again should follow Teams' own behaviour. The outbox doesn't, for either build:

- a write whose outcome isn't known (a timeout, a network error, a 5xx but 503) goes again only when it is a post the
  chat's sync can look for by its `clientmessageid`; any other write is failed, never resent;
- no row of an earlier run goes out without an explicit `Store::resend` (the web client resends a failed send on a
  later load);
- a call write is never resent after an unknown outcome.

Those rules were made for the lab (a write sent twice could reach someone twice) and the outbox keeps them for the
personal build too until this is settled. To decide: which of the web client's retries the personal build should take
(a failed post resent on the next start, an edit or a reaction tried again after an unknown outcome), each from a
capture of the web client's behaviour, and how the outbox tells a personal row from a lab one without loosening the
lab's rules. A call write stays unresent either way: a call controller acting twice is worse than a failed step.
