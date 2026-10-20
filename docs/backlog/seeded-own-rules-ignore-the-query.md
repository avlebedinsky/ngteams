---
worth: yes
where: crates/proto-lab/src/classify.rs:108
added: 2026-10-21
---
# the seeded own-state rules let a request out whatever its query

`OwnRule` matches the method, the exact origin and the path segment by segment, and `UrlPattern::matches` never looks
at the query (the module docs say so: "The query plays no part"). So the Entra token endpoint (`/*/oauth2/v2.0/token`),
the four authz endpoints and `login`'s sign-in form (`/*/GetCredentialType`, `/*/login`, `/kmsi`, `/*/kmsi`) are
`Own` with any query at all, including parameters no capture showed. The hard rules want an allowance to match the
exact captured shape, query included.

What the local captures show (2026-10-21, every scenario): the token endpoint always has a query, either
`client-request-id` alone or `brk_client_id`, `brk_redirect_uri`, `client-request-id` and `client_id`; the authz
exchange never has one. The sign-in form isn't in any capture (`login` records none), so its query shapes are unknown.

Suggested fix: give each seeded rule the query keys its capture showed (none for authz, the two key sets above for the
token endpoint) and make any other key set, or a repeated key, `Unknown`; capture `login` under the fence first to
review the sign-in form's queries. `Classifier::with_own` (the tests' local stand-ins) and the fence_http tests that
use it go along.

Found by the revmux review of M1 Task 14 (round 2, `docs/plans/20261020-native-teams-m1-headless-chat-core.md`),
looking for other own-state rules with the path-only gap of the registration allowance. Not fixed then: it changes the
M0 rule machinery every scenario and `login` depend on, and the sign-in form needs a capture first. The calling and
messaging stacks' own-state (`crates/proto-lab/src/calling.rs`) and the call start now require no query.
