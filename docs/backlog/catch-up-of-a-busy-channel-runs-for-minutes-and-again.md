---
worth: soon
where: crates/teams-core/src/sync.rs, crates/teams-core/src/engine.rs
added: 2026-10-28
---
# a busy channel's sync pages back through its whole history, and the catch-up runs for minutes, over and over

The personal build's second account (254 conversations, channels with tens of thousands of posts) had its engine busy
with one catch-up for minutes: every chat it opened stayed empty under "Loading older messages…", since the engine
answered no request until the catch-up was over. The store reads are now answered while a catch-up read
waits (`engine/inbox.rs`); this item is what is left.

What the store and the app's log showed (run of 2026-10-28, 09:07 UTC on):

- A channel's sync answer (`…/messages?startTime=0&syncState=…`) has a `backwardLink` that pages back through the
  channel's whole history, not only what changed since: one channel's walk stored posts back to 2022. `sync_stored`
  follows it for up to `MAX_PAGES` (50) pages of 200 at 1.5–5 s a page, so one channel's sync takes up to ~4 min,
  and two busy channels (`thread.tacv2`, a meeting chat) kept a gap link.
- With a gap kept, the next sync reads 50 pages of the gap, then the answer again from the newest, 49 more pages of
  posts it already has, and fails `GapPending` when that answer is cut again. A failed conversation keeps the
  conversation list's sync link where it was, so the next catch-up lists (and syncs) everything since then again; it
  is tried again from the tick 30 s later (`catch_up_retry`). The list's sync link had stayed at the first run's
  (20:55 the evening before) all morning.
- Meanwhile every write, older-history read, presence request and on-demand sync of that account waits for the
  catch-up (only the store reads no longer do): a message sent from it goes out minutes late.

Not captured: what a sync answer of a channel looks like on the web client (whether it pages back at all, what its
`startTime` is). Next step: capture the web client's sync of a busy channel; then either take a sync answer's
`backwardLink` only for what changed since the kept sync (stop at a page older than the sync state's time), or keep
the answer's link moving on when its rest is history rather than a gap. Running the catch-up a conversation at a time
between the loop's other requests would bound the wait of a write too.
