---
worth: later
where: crates/teams-ui/src/account_prompt.rs, crates/teams-ui/tests/, scripts/test-windows-app.ps1
added: 2026-11-14
---
# Show the "Microsoft 365 / Own server" sign-in choice in a real Windows window

Nothing an agent may run on Windows shows the sign-in choice in a real window. A lab build runs only
`--lab <alias>`, an alias holds one account and can't add another, so neither the first-run screen nor the rail's
"+" opens there. `accounts_window` and `own_server` are Linux-only, and `scripts/test-windows-app.ps1` has no scene
for the account prompt. Windows sees the choice, the three fields and the Microsoft-only prompt in the view tests
alone (`account_prompt`), and the Own server form only as a lab alias's password-again screen.

Add a `teams-ui` scene target for the account prompt (as `chat_list_window` and the other scenes are) and list it
in the script's `$scenes`: the choice with Microsoft 365 chosen, Own server chosen with its three fields filled,
the password masked and revealed, and a failure line under the fields, in a light and a dark theme. It draws the
view only and signs nothing in.
