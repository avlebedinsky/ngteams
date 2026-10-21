---
worth: yes
where: crates/teams-core/src/sync.rs:596
added: 2026-10-21
---
# an ordinary catch-up consumes an unreadable entry that carries a pending row's clientmessageid

A message page can hold entries the model can't read at all (`MessagePage::unreadable`: an entry whose `properties`
isn't an object, say). One of them may be our own message, carrying the `clientmessageid` of an outbox row whose
earlier attempt may have arrived (its answer was lost). Inside the outbox's check (`CatchUp::sync_since`) such a page
fails the check (`CatchUpError::CheckFellShort`) before it is applied or any link moves past it, so the row waits and
is given up at `MAX_FAILED_CHECKS` rather than going out twice. An ordinary catch-up (`catch_up`, `sync_messages`,
`load_older` called outside a check) has no such guard: it counts the entry in `Synced::skipped` and moves its sync
link past the page. A later check (after the row's backoff, or after `Store::resend` adopts a row of an earlier run)
then reads only what changed since, finds no stored message with the row's `clientmessageid`, and sends the row again
with the same `clientmessageid`. Whether the service dedupes by that id is unverified, so the others may see the
message twice.

Not guarded in ordinary catch-up on purpose: failing the page there would stall the conversation's sync with no
bound. A pending row of an earlier run is never checked until an explicit resend, and a row of this run is checked
only when `send`/`send_due` runs again after its `not_before`, so nothing bounds how long the page would be refused
and every later message of that chat would be lost to sync meanwhile.

The fix must keep the uncertainty without blocking the conversation's sync indefinitely: for example, record on the
row (a store migration) that an unreadable entry carrying its `clientmessageid` was seen, and have the check treat
that as "can't tell" (fail the check, the row given up at `MAX_FAILED_CHECKS`) while the catch-up itself moves on.

Surfaced by the revmux review of M1 Task 17 (branch `m1-task-17-sync-r3`, decision discussed with Codex); deferred
because it needs state the store doesn't keep.
