---
worth: yes
where: crates/teams-proto/src/loki.rs, crates/teams-core/src/profile_card.rs
added: 2026-11-15
---
# Own server: the engine never takes Loki's regional base

On an own-server account the profile card shows a person's presence, title and office, but none of Loki's sections
(names from Loki, organization, working with, schedule, automatic replies). The server serves them all under
`/loki/` and its configuration names `LokiUrl` as `<origin>/loki/` (`docs/server.md`, "People").

The cause is in the client. `LokiConfiguration::read_answer` takes `LokiUrl` only as
`https://<label>.loki.delve.office.com/`, while `HttpTransport::set_loki_region` on an own cloud takes only
`/loki/` on the account's origin. No answer passes both, so `profile_card::configure` never sets the region and
every `Service::Loki` read is refused before it is sent.

Make the configuration's read cloud-aware (`read_answer_in(cloud, body)`, as `PresenceSubscription::new_in` is), or
let `configure` hand the answer's `LokiUrl` to `set_loki_region`, which already checks it against the cloud. Then
extend `ngteams-server`'s `people::an_engine_names_and_draws_a_colleague_and_reads_their_card` to assert the card's
organization and working-with sections.
