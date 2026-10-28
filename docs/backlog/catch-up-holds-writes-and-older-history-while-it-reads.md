---
worth: later
where: crates/teams-core/src/engine/inbox.rs
added: 2026-10-28
---
# a catch-up holds the account's writes and older-history reads while it reads the service

While a catch-up read waits on the service, the engine's inbox answers the store's reads only; every write, older-history
read, presence request and on-demand sync of that account waits for the whole catch-up. A sync answer is now read back
only to where the last sync ended (`docs/protocol/chat.md`, "Sync answers"), so a catch-up is one page per changed
conversation, but a page of 200 channel posts takes 2 to 25 s, and a catch-up after a long time away (or the first
after the conversation list's sync link was stuck, which brought 200 conversations) still runs for minutes: a message
sent meanwhile goes out that much later.

Also seen then: a kept gap's progress is stored only once its walk ends, so a page that times out (a channel page over
the 30 s request timeout) starts the gap again from its first page next time.

The unknown that settles it: how long catch-ups take now on the personal build's busy account. If they stay within
seconds, drop this; if not, run the catch-up a conversation at a time between the loop's other requests (and store a
gap's progress per page).
