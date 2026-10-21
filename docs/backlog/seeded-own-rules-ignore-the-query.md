---
worth: yes
where: crates/proto-lab/src/classify.rs (SIGN_IN, QueryRule::Any)
added: 2026-10-21
---
# the sign-in form's own-state rules let a request out whatever its query

M2 Task 1 gave every seeded own-state rule the query its captures show (`QueryRule`): the Entra token endpoint takes
only its two captured parameter sets (the request id alone, or with the broker's three parameters), each parameter
once and in its captured value forms, and the authz endpoints take no query; anything else is `Unknown`. One part is
left: `login`'s sign-in form (`/*/GetCredentialType`, `/*/login`, `/kmsi`, `/*/kmsi`, `OwnKind::EntraSignIn`) still
takes any query (`QueryRule::Any`), because no capture has shown its queries yet. The hard rules want an allowance to
match the exact captured shape, query included.

`login` now records its run under `captures/login/<ts>/` (fail-closed, with no request body: the form's posts carry
the password), so the next time a lab session expires and `login` runs, its capture shows the form's queries.

Suggested fix: after a `login` run, read its capture's sign-in form requests (method, path, query keys and value
forms), give each `SIGN_IN` rule that query set (`QueryRule::OneOf`) in place of `QueryRule::Any`, with tests that any
other key set or a repeated key is `Unknown`. Task 24's sign-in window guard reuses these shapes. Not done blind:
`login` runs only when a session has expired, and a rule narrowed without a capture could break it then.

Found by the revmux review of M1 Task 14 (round 2, `docs/plans/completed/20261020-native-teams-m1-headless-chat-core.md`);
cut to the sign-in form by M2 Task 1.
