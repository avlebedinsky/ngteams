---
worth: yes
where: crates/proto-lab/src/scenario/expected.rs:168
added: 2026-10-21
---
# Bot invokes and message extensions: denied by the fence, not yet designed for

Whenever a compose box or an edit box opens, the web client asks its message extensions for their commands: a `POST
v1/agents/28:<bot id>/invoke` on the chat service with `name` `composeExtension/fetchCommands`, sent to two of
Microsoft's own apps (Workflows and Updates) in the lab's runs of 2026-10-20 and 2026-10-21 (`ab-chat-full`). The body
is a JSON object with the keys `appId`, `conversation` (an object with the chat's `id`), `imdisplayname`, `name`,
`userAadToken` and `value` (an object with `commandContext`).

The fence denies them as unknown, and the writing scenarios' reviewed expected-deny list (`expected.rs`, "bot
invokes") only stops them from failing a run. They stay denied in M1:

- `userAadToken` is an Entra token for the lab account, which the chat service hands on to the app's own backend: a
  credential leaving for a third party, whatever the invoke itself does.
- No milestone builds bots or message extensions yet, and the native client doesn't need them to chat.

A fence rule for bot invokes has to decide:

- **The target**: the bot and the conversation together. Which conversation an invoke acts in is in its body; whether
  the bot is in that conversation, and what the bot may post there afterwards, needs a membership read the fence
  doesn't have for bots.
- **The token**: whether an invoke may carry the account's Entra token at all, and to which apps (an approved list,
  like the approved service participants), or whether a native client leaves it out.
- **Which invokes are reads**: `composeExtension/fetchCommands` and a query of a message extension may only read,
  while a submit or a card action makes the bot write; the name alone may not tell them apart for every app.

The user decided on 2026-10-21 to leave bots and message extensions for the long tail (M5) and not to forget them;
this item is that reminder. The mention picker's people lookups, decided the same day, didn't need them: the picker
offers a lab account from the middle tier's profiles alone (`crates/proto-lab/src/post_read.rs`).
