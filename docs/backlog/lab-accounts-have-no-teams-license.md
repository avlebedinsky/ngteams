---
worth: later
added: 2026-10-19
---
# Give the lab accounts Teams: a license, or the Exploratory trial?

Found by M0 Task 8 (`docs/plans/20261019-native-teams-m0-lab-and-spikes.md`), in the first live runs under the armed
fence on 2026-10-19. All three lab accounts are signed in to Microsoft: their profiles' Entra session works, and the web
client's token requests succeed without any sign-in page. But Teams refuses each of them. Its authz exchange
(`POST https://teams.microsoft.com/api/authsvc/v1.0/authz`) answers 403 (once, on a first visit, 500), and the web
client shows "Enjoy your Teams trial" with a "Start trial" button: the Microsoft Teams Exploratory trial. The accounts
have no Teams license.

Nothing live after Task 8 works without one: the `load` and `ab-chat` scenarios and the live membership source
(Task 11), the auth notes' capture (Task 12), the skypetoken exchange (Task 14, which is this same authz call), the echo
call (Tasks 18–19).

Options:
- an admin assigns a Teams license to the three lab accounts;
- the user approves starting the Exploratory trial on them: a self-service trial that puts a license on each account in
  the company tenant (which its admins see) and runs out;
- other test accounts that already have Teams.

An agent can do none of these: a license or a trial is a change in the company tenant beyond the lab accounts'
conversations, and the fence denies the trial's request anyway (it isn't on the own-state list). The unknown that
settles it: whether the user, or the tenant's admin, will license the lab accounts, and which way.

Meanwhile: `proto-lab login` records each account's MRI from the claims of the tokens the page sends and reports
"Teams refused the account (authz answered 403)"; `proto-lab status` reports the same instead of `ok`. Nothing to undo.
Once the accounts have Teams, `proto-lab status` should print `ok` for each.
