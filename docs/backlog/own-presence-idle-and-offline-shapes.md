---
worth: yes
where: crates/teams-core/src/own_presence.rs, crates/proto-lab/src/scenario/own_presence.rs
added: 2026-10-23
---
# the app reports no Away when idle and no Offline on quit: neither shape was captured

M2 Task 20 asked for Available while the window is used, Away after the idle time and Offline on quit. Only three
own-presence requests were ever captured (Tasks 18–19, `docs/protocol/trouter.md`, "Own presence"): the endpoint `PUT`
(`Available`), the manual status (`Busy`, `DoNotDisturb`, `Away`, `Available`) and its reset. So the app's policy
(`teams_core::own_presence`) reports Available (the endpoint `PUT`, every 2 min) and, once idle, nothing: the endpoint
lapses on its own (read `Offline` once, 5 min 20 s after one `PUT`), so others see Offline, not Away. On quit it sends
nothing; the endpoint goes after the push channel closes (in the browser runs, `Offline` 10–25 s after the browser
closed). The manual `Away` was rejected for idle (settled with Codex,
`discussions/task-20-away-offline`): it is account-wide server state, so it would override the account's other devices
(a phone in use would show Away), clear a status the user set elsewhere when it is reset on the next use, and outlive
the app after a suspend or a crash, with no known lifetime.

Open: how the web client itself reports idle (an endpoint `PUT` with another `activity`, a different request, or
nothing until its endpoint lapses) and whether it sends anything on sign-out or close. Neither was driven: every
`own-presence` run kept its page in use or closed it at once.

Suggested next step: a live `own-presence-idle` scenario (a's web client left untouched past its idle time, then
closed), under the fence with the endpoint `PUT` mapped, capturing what it sends and b's reads of a beside it. If it
shows a per-endpoint idle shape (an `Away` activity on the endpoint `PUT`, say), map it in its captured shape under the
user's exception and let the policy send it when idle; if it shows an explicit offline request, the same for the
quit. Until then the policy stays as it is: no shape is guessed.
