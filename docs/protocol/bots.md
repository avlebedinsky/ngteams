# Bots: invokes, card actions and their answers

How a Teams client asks an app's bot for something: a card's button, a message extension's commands, its dialog
and its submit. The client never reaches a bot or an app's backend itself. It posts one request to its own chat
service, and the service reaches the bot. The same request serves a Microsoft account and an own-server account;
only the token rule below differs.

What was seen: the Workflows app's message extension in the lab's runs (lab account a, `team-manage`, 2026-11-05):
`composeExtension/fetchCommands`, `fetchTask` and `submitAction`, with their answers. Everything marked **unseen**
below follows the public Teams and Bot Framework documentation and waits for a capture: no bot in the lab tenant
shows those parts within the lab rule (`docs/lab-rules.md`). The own server follows this page, so a later capture
that differs changes the client and the server together.

The native client: [`teams_proto::bot`](../../crates/teams-proto/src/bot.rs) (the body and the answers),
[`teams_core::bots`](../../crates/teams-core/src/bots.rs) (one attempt through the outbox),
[`teams_proto::chat::card`](../../crates/teams-proto/src/chat/card.rs) (what a card's buttons and inputs carry).
Synthetic fixtures: `tests/fixtures/bots/`.

## The request

`POST <chat service>/v1/agents/28:<bot id>/invoke`, with the chat service's own headers and bearer (on Microsoft the
`https://ic3.teams.office.com` token, `X-MS-Migration: True`, `BehaviorOverride: redirectAs404`) and a JSON body:

| Field | What it is |
|---|---|
| `name` | the invoke's name (below) |
| `appId` | the app's id; the Workflows app's differs from its bot's id in the path |
| `value` | what the invoke asks, by name (below) |
| `imdisplayname` | the account's display name |
| `userAadToken` | a Microsoft account only: see the token rule |
| `conversation` | `{"id": …}`: the chat, or a channel post's `<channel>;messageid=<post>` chain. Seen left out of a `fetchCommands` sent while no conversation was open |
| `messageId` | the card message an action acts on: **own design, unseen** (the Bot Framework gives the bot this as `replyToId`; where Teams puts it in the request isn't known) |

## The token rule

In every captured invoke, `userAadToken` is the account's token for the Teams service itself: audience
`https://api.spaces.skype.com`, scopes `Authorization.ReadWrite Region.ReadWrite user_impersonation`, issued to the
Teams web client. It is the same token the client already sends to Teams' authz and middle tier, and it goes to
Microsoft's chat service, the service it was issued for. It is not a token for the bot or its app: nothing seen shows
the service passing it on, and the Bot Framework gives a bot no user token in an activity.

- **Microsoft account:** the native client sends `userAadToken` with the account's Teams service token, in every
  invoke, as Teams does. No other token goes into an invoke. The client never opens a connection to a bot, never
  follows a bot's `serviceUrl`, and never asks for a token for an app's audience. The transport adds the token when it
  sends; the typed request never holds it, and the debug log redacts it.
- **Own-server account:** the same path on the account's own origin (`<origin>/api/chatsvc/<region>/v1/agents/…`),
  with that account's own bearer, and **no `userAadToken`**: the bearer already names the caller. The own server
  ignores the field if a client sends it and never copies a user token into what it sends a bot.

Still out of scope under this rule: `signin/tokenExchange` (single sign-on, which hands a token for the app's own
audience to the app) and any on-behalf-of token for an app.

## The invokes

| `name` | When | `value` | Seen |
|---|---|---|---|
| `composeExtension/fetchCommands` | an editor opens (the compose box, a message's actions): the app's commands for that context | `{"commandContext": "Message"}` (capitalised; `Compose`, `CommandBox` by analogy, unseen) | seen |
| `composeExtension/fetchTask` | an action command is chosen: its dialog | `commandId`, `commandContext` (`message`; `compose`, `commandbox` unseen), `context` `{"theme": "default"}` (`dark`, `contrast`), and on a message `messagePayload` | seen |
| `composeExtension/submitAction` | the dialog's submit | the same, with `data`: the card's `Action.Submit` `data` merged with the answers by input id, each a string (a date `2026-01-03`, a time `08:00`, a switch's value) | seen |
| `composeExtension/query` | a search command's query | `commandId`, `commandContext` `compose`, `parameters` (`[{name, value}]`), `queryOptions` (`skip`, `count`), `context` | unseen |
| `composeExtension/selectItem` | a search result is chosen | the result preview's `tap` value | unseen |
| `adaptiveCard/action` | a card's `Action.Execute` is pressed, or its `refresh` runs | `{"action": {"type": "Action.Execute", "id", "verb", "data"}, "trigger": "manual" \| "automatic"}` | unseen |
| `task/fetch` | a card button whose `msteams.type` is `task/fetch` | `{"data": <the button's data>, "context"}` | unseen |
| `task/submit` | a card dialog's submit | `{"data": <its data and answers>, "context"}` | unseen |
| `message` | a card's plain `Action.Submit` or `messageBack`: the bot gets a message activity carrying `value` | the data merged with the answers | **own design**, unseen |

`messagePayload` describes the message an action command runs on: `id`, `replyToId` (a channel post's id; none in a
chat), `createdDateTime`, `deleted` false, `importance` `normal`, `locale` `en-us`, `from` (`{"user":
{"userIdentityType": "aadUser", "id": <object id>, "displayName"}}`, or `{"application": {"applicationIdentityType":
"bot", "id", "displayName"}}` for a bot's message), `body` (`contentType` `html`, `content`), empty `attachments`,
`mentions` and `reactions` lists, and `linkToMessage` (the message's Teams link, as `workflows.md` gives it).

What a card's buttons carry (`teams_proto::chat::card::BotAction`): an `Action.Submit`'s `data` as given, its
`msteams.type` (`messageBack` with `text`, `displayText` and `value`; `imBack` with its text; `invoke`; `signin`;
`task/fetch`), its `id` and `associatedInputs` (`none` sends no answers); an `Action.Execute`'s `verb`; a hero card
button's type and `value`; and a card's `refresh` (its `Action.Execute` and the `userIds` it runs for by itself).
`imBack` and `messageBack`'s `displayText` post a visible message as the user, through the ordinary message write.

## The answers

The chat service answers with the bot's status and body. Seen: HTTP 200 with
`{"responseType": <kind>, <kind>: …}`, `application/json`; and HTTP 200 with an empty `text/plain` body.

| Answer | Shape | Seen |
|---|---|---|
| done | 200, empty body (a `submitAction`; a `fetchCommands` of an app with nothing for the context) | seen |
| commands | `responseType` `composeExtension`, `{"type": "command", "commands": [{id, type: action \| query, title, fetchTask, taskInfo: {width}, imageUrls}]}` | seen |
| dialog | `responseType` `task`, `{"type": "continue", "value": {width, height, title, card: {contentType, content: <Adaptive Card>}}}` | seen (a card, `width` 600) |
| dialog with a web page | the same with `value.url` in place of `card` | unseen; the native client says the app opens in Teams on the web |
| results | `composeExtension` `{"type": "result", "attachmentLayout": "list" \| "grid", "attachments": [<card with its preview>]}` | unseen |
| message | `composeExtension` `{"type": "message", "text"}`, `task` `{"type": "message", "value"}` | unseen |
| sign-in | `composeExtension` `{"type": "auth" \| "config", "suggestedActions"}` | unseen |
| `Action.Execute`'s | the bot's own `{"statusCode", "type", "value"}`: `application/vnd.microsoft.card.adaptive` (a card that takes the pressed card's place), `…activity.message` (a line of text), `application/vnd.microsoft.error` (`{code, message}`), `…activity.loginRequest` (sign in) | unseen, and whether the service wraps it in `responseType` isn't known: the native client reads both |
| an error status | 4xx or 5xx | unseen; the native client shows it and sends nothing again |

An answer of another shape is kept as it came (`InvokeAnswer::Unknown`), never guessed at.

## The native client

An invoke is one attempt through the outbox (`Outbox::try_bot_invoke`), with no row: it may make the app act, so an
unknown outcome is told to the asker and never sent again, and a later run sends nothing. Read-only mode sends none.
The engine names the asker (`EngineHandle::bot_invoke`); no view builds the request. Until the bots feature leaves its
flag (`NGTEAMS_BOTS=1`), no view calls it: cards' bot buttons still open the message in Teams on the web.
