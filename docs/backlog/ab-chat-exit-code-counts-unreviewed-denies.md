---
worth: yes
where: crates/proto-lab/src/scenario/mod.rs:73
added: 2026-10-20
---
# `ab-chat` exits 1 on the client's unreviewed fail-closed denies

Found by the M0 acceptance check (`docs/m0-acceptance.md`, criterion 2). `proto-lab run ab-chat --headless` reaches
its goal (a's message shows on b), passes the live enforcement test (the forced probe fails in a's page and never
shows on b), allows writes only on the a–b chat, and still exits 1: its `DenyPolicy::ForcedAnd(&TELEMETRY_HOSTS)`
expects only the forced probe and `*.events.data.microsoft.com`, and the web client makes ~470–575 other requests and
~50–70 WebSocket frames per run that the fence denies as `Unclassified`. So the exit code can't tell a healthy run
from one where a mapped write on the a–b chat was wrongly denied; today that is read off the output by hand.

The classes seen on 2026-10-20 (two runs, both accounts): own chat properties (`PUT …/chatsvc/<region>/v1/users/ME/
properties`), middle-tier and apps lookups by `POST` (`fetchShortProfile`, `aggregatedEntitlements`, `eligibilities`,
`batchedDefinitions`, `fetch`, `fetchfeaturesettings`, `useraggregatesettings`, `effectivePolicies`, `cookiev2`,
`files/availability`, `mcps/initArtifactFolder`, `sharetoteams/installApp`, `engagementSurfaces/actions`), presence
reads by `POST` (`/ups/<region>/v1/presence/getpresence/`), the async gateway's and AMS's token exchanges
(`aadtokenauth`, `skypetokenauth`), `tps …/metadata/pushupdates`, substrate search suggestions, the editor service,
`augloop.office.com` posts and frames, Trouter frames, `admin.microsoft.com` and `browser.pipe.aria.microsoft.com`
logging.

Way forward: a reviewed **expected-deny** list for the writing scenarios (host + method + path pattern, frames by
host), which changes only how a run is judged, never a verdict: every request on it stays denied. Anything on a
mapped target (the a–b chat), any deny whose cause isn't `Unclassified` (a partial or refused membership), and any
class not on the list keep failing the run. Each entry needs a reason it isn't needed for the scenario, and the list
is tested against the classes above and against a mapped write that must stay unexpected.
