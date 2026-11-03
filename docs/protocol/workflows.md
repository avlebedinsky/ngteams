# Channel workflows

The channel Workflows entry uses the account's native Power Automate connection. It does not install the Teams
Workflows app or hand a Teams credential to a connector. The current Teams web client requests a token for
`https://service.flow.microsoft.com/`; the native `TokenClient` can obtain that audience from its existing account
session. Its token goes only to `api.flow.microsoft.com`. Connection management uses the separate
`https://service.powerapps.com/` audience, only at `api.powerapps.com`.

The permitted native read probe independently checked the signed-in account's MRI and UPN using its own native
access token before each service. Catalog, environment and connection reads returned 200. The test account had one
accessible default environment and no connected apps. The browser entry attempted to install the Workflows app
and its automation bot; that request was blocked before sending. Native discovery does not require that install.
The browser client's Flow token preauthorization failure did not apply to the existing native client.

## Templates and configuration

The current Teams configuration supplies this catalog request:

`GET /providers/Microsoft.ProcessSimple/galleries/public/teamsworkflowtemplates`

Its query filters `properties/categoryNames` for `microsoftteams_channel`, excludes `reported`, and sets
`useNewConfigUrl=true` and `supportBigTiles=true`. The response is an object with a `value` array. Each entry has
`id`, `name`, `description`, `icons`, `categories` and `configUrl`; these are not Flow resource properties.

The Microsoft-hosted flow-creation page's current client reads a template with:

`GET /providers/Microsoft.ProcessSimple/galleries/public/templates/<id>?$expand=definition,connectionReferences.api.properties`

That public read returned 200 for the screenshot's reminder, Planner, SharePoint, email and webhook templates.
Its `properties` contain `displayName`, `description`, `definition`, expanded `connectionReferences` and metadata.
The definition includes the actual workflow parameters, triggers, nested actions and expressions. The native form
preserves expressions and edits literal inputs and recurrence settings. The captured channel templates use
`teams.group` and `teams.channel` parameters or literal Teams recipient group/channel fields. The selected channel's
service ids supply those values, never its displayed name.

A connected account is bound by connection name, `source=Embedded`, its actual Microsoft Power Apps API id and
`tier=NotSpecified`, matching Microsoft's Power Automate client. Connection metadata is not copied into runtime
connection references. New flows are saved stopped so configuration does not itself start a schedule.

## Lifecycle routes

The classic Flow resource provider operations are grounded in Microsoft's current Power Automate client source:
[Power Automate server](https://github.com/microsoft/power-platform-skills/blob/main/plugins/power-automate/server/mcp.mjs).
The client includes these classic routes as its supported fallback from the newer environment-scoped API. The
native client uses the fixed classic Microsoft origin, without speculative regional hosts.

All environment routes use `api-version=2016-11-01`. Under
`/providers/Microsoft.ProcessSimple/environments`, discovery is a GET. Under `<environment>/flows`, listing and a
single flow read are GETs, creation is POST, updates are PATCH and deletion is DELETE. A flow's `start`, `stop` and
`triggers/<trigger>/run` operations are POSTs. Its `runs` collection is read with GET. Create and update carry a
`properties` object containing the definition and its connection references.

List continuations can read only the same environment's exact flow-list route on `api.flow.microsoft.com`, without
userinfo, a port or a fragment. The UI reads full definitions before identifying workflows available in this channel;
a list summary or a substring in a title does not establish a channel destination.

## Connections and authentication

The Power Automate portal's current route map identifies the classic Power Apps connection routes. The native
probe verified `GET /providers/Microsoft.PowerApps/connections` with `api-version=2016-11-01` and the exact
`environment eq '<environment>'` filter. Connection creation is PUT to
`/providers/Microsoft.PowerApps/apis/<connector>/connections/<name>`, with the same environment filter; its
properties contain empty connection parameters and the environment name. `getConsentLink` is POST with a fixed
Microsoft Power Automate redirect. These operation bodies also follow Microsoft's client source above.

The view opens only HTTPS consent links on Microsoft's `consent.azure-apim.net` or `consent.azure-apihub.net`
subdomains, with no userinfo, explicit port or fragment. It never posts a bearer token to a service-returned
`firstPartyLoginUri` or a template URL. Connector sign-in is completed by the user in the browser, followed by a fresh
native connection read. Only a connection whose status is `Connected` can be bound in the form.

## Execution boundaries

Every mutation reaches the engine's outbox and has one HTTP attempt, with no automatic resend after a timeout,
throttle or uncertain outcome. A read-only app refuses mutations before token lookup. Lab cloud mutations are also
refused independently of the old request classifier: a server-side schedule cannot establish a fresh complete lab
human audience before each future notification. Offline synthetic services exercise mutations; no Planner,
SharePoint, email, webhook or Teams notification was executed in the tenant during implementation.

The AI Workflows service is separate from these verified cloud-flow routes. Its authenticated reference is still
blocked by the automatic Workflows bot install in the permitted lab session; the native cloud-flow API alone is not
proof of AI workflow access. That surface remains a task blocker, not an empty-list or licensing conclusion.
