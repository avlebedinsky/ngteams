---
worth: yes
added: 2026-10-20
---
# A mention has never been captured: its wire form is the bundle's only

M1 Task 7 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`) asked `ab-chat-full` to have a mention b.
Under the armed fence it can't: typing `@` in the compose box opens the mention picker
(`AutocompletePopup-Mentions`), whose only suggestion is the "Share someone's contact info" message extension (around
it the client sent two bot invokes, `composeExtension/fetchCommands`, denied as unknown; they also come when an edit
box opens). b is never offered: the chat partner
shows as "Unknown User" because the middle tier's profile lookups (`users/fetchShortProfile`, `users/fetch`) and
Substrate search are `POST`s the fence denies (live, 2026-10-20). So `docs/protocol/chat.md` describes the mention
markup and `properties.mentions` from the web bundle only, marked not captured, and `tests/fixtures/chatsvc/mention.json`
is bundle-derived. The browser mapping (`Classifier::mapped`) takes a message's or an edit's mentioned MRIs as added
participants, in that bundle form, so a wrong guess there only denies more.

Decided with Codex (transcript `discussions/task-7-mention-under-fence/` of the run): keep the fence as it is and leave
the live mention open, rather than add a reviewed allowance for the profile lookup inside a capture task. An allowance
for `fetchShortProfile` would be a read of another person's profile by `POST`, not the account's own state, and might
not be enough for the picker anyway.

What is still owed, and how it can be closed:
- Task 12's mention body must match a capture, and none exists. Either its own native client sends a mention to the
  lab chat (the fence allows a message whose mentioned MRIs are lab accounts) and the answer and b's read of it are
  checked, or a reviewed profile-read allowance lets the web client's picker offer b and `ab-chat-full` mentions b.
- M1 Task 12 built the native client's mention from the bundle form, in `mention_span` and `mention_entry`
  (`crates/teams-proto/src/chat/compose.rs`) alone; a capture replaces those two functions and `mention.json`.
- Either way the capture must show the content's mention span, `properties.mentions` (string or list, its keys), and
  how b's read gives it back; then `mention.json` is rewritten from that capture and `chat.md` updated.
- A scenario that picks from the picker must check the suggestion is b before it presses Enter (the old step picked
  whatever came first).
