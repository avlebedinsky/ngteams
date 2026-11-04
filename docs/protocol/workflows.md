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
connection references. The full editor saves new flows stopped so configuration does not itself start a schedule; the
card editor below saves as Teams does.

## The webhook template in Teams

Seen 2026-11-04 with `proto-lab run team-manage` steps: lab account a, its own lab-only team's General channel,
channel menu → Workflows → "Send webhook alerts to a channel". The shots stay in the lab's capture dir.

A template whose gallery entry has `properties.metadata.userIntentMarkup` opens in Teams' card editor, not a form of
the definition. The markup's `automation.template` lists `parameters` (`id`, `typeName` such as `Teams.Group` and
`Teams.Channel`, `dependencies`), `editorBindings` (a parameter with a `label` resource gets its own label), a
`summary` resource formatted with the channel, and `dataBindings` naming which action inputs each parameter fills.
The screen shows a back chevron, "Untitled workflow" and "Collapse all"/"Expand all"; one rounded card with a
"Microsoft Teams" chip, the title "Send webhook alerts to <channel>", the template's description and two required
dropdowns, "Team the channel is in" and "Channel", defaulting to the channel Workflows was opened from; Save and Back.
There is no name field, connection control or definition. The dropdowns list the account's teams and the chosen team's
channels with a check on the current one; they are type-to-filter comboboxes. The card's chevron folds it to its title
and description. Back returns to the template list. Edit on a saved workflow opens the same card titled with the
workflow's name.

Save sends `POST …/powerautomate/flows` with `properties`: `displayName` (the card's title), `templateName` (the
template id), `connectionReferences.<key> = {id: "/providers/Microsoft.PowerApps/apis/shared_teams",
connectionName, source: "NotSpecified"}` and the template's definition with the recipient group and channel ids filled,
`description` set to the template's, and one more action, `Do_Not_Remove_FlowIL` (an `InitializeVariable` of
`flow.IL (DO NOT REMOVE)`), that the template's first actions now run after. Its `metadata.userIntentMarkup` is the
markup as a JSON string with each parameter's `value` and `valueDisplayName`, the template's `displayName` and its
connection references. No `state` is sent and the workflow is created on (201). Teams then reads
`triggers/manual/listCallbackUrl`, the flow, its triggers, connections, runs and owners, and shows the Overview:
Overview (below); Back and Workflows Home. Delete asks "Delete automation" ("Would you like to delete
your automation? Alternatively, you can turn it off.") with Delete and Cancel, then returns to the template list.
The native client uses the classic Flow routes for the same operations.

The two dropdowns are type-to-filter comboboxes. Opening one lists every choice (teams in the service's order) with a
check on the chosen one; typing filters to the names holding the text, or "No results match your search". Choosing
another team clears the channel: its field shows "Type or select a channel", the card's title becomes "Send webhook
alerts to channel" and Save turns off until a channel is chosen. Typed text left in a field without choosing keeps
the earlier choice (Teams leaves the text shown; the native field shows the chosen name again).

## The Overview

Seen 2026-11-04 the same way, on a workflow made from the webhook template and deleted afterwards. Teams reads the
flow, its triggers, connections, runs and `owners`, and resolves the owners' names through Graph.

- **Rename** turns the name into a field holding it, selected, and the button into a primary Save, off while the field
  is empty; Save shows "Saving" and sends `PATCH …/flows/<flow>` with only `{"properties": {"displayName": <name>}}`.
  The answer is the flow, and the Overview stays with the new name.
- **Turn off** shows its spinner, sends `POST …/flows/<flow>/stop` with `{}`, reads the flow again and stays on the
  Overview: the state line becomes a filled grey circle with a bar and "Inactive", the button "Turn on" (which sends
  `start` the same way). Copy webhook link stays.
- **Owners**, below Run history: "Adding an owner gives them full control of this flow. They'll be able to add or
  remove other users as owners, access the run history, and can update, edit or delete this flow." with a Learn more
  link (`https://go.microsoft.com/fwlink/?linkid=839912`, Microsoft's page on team flows), then each owner (initial,
  name; "Owner" beside the one who made the flow, Remove beside a co-owner), then "Add a co-owner" with a search box ("Enter names, email or user groups"). Typing searches the directory (Teams asks
  Graph for users and groups whose name, address or UPN starts with the text) and lists who it finds above the box,
  name and address, or "<text> is not in your organization's directory.". Picking someone adds them at once:
  `POST …/flows/<flow>/modifyowners?cascadeoperation=true` with `{"put": [{"name": <object id>, "properties":
  {"principal": {"id": <object id>, "displayName", "email", "type": "User"}}}]}`. Remove sends
  `DELETE …/flows/<flow>/owners/<object id>` with no question. The owners read answers each owner as
  `properties.principal` (`id`, `type`); the flow's `properties.creator.objectId` names who made it.
- **Workflows Home** (external-link icon) closes the dialog and opens the Workflows app in Teams' main window: its
  Home tab (Find templates, Build from scratch, Create a workflow, Your workflows with Refresh), beside Templates and
  Chat (the Workflows bot's chat). The native client has no app pages, so its Workflows Home is the dialog's own page
  of every template and workflow.
- The channel's list: [below](#the-workflow-list-and-its-menu).

The native client sends the same bodies on the classic Flow routes. Its co-owner and people searches use the engine's
directory people search (the one Manage team's Add member uses), keeping only people whose name or address starts with
the text as Graph's `startswith` does, and owners' names the engine's profile lookups, rather than Graph from Power
Automate's token bundle.

## The Teams connection behind the scenes

When the template opens, Teams makes the Microsoft Teams connection itself; the user never sees a sign-in:

1. `PUT …/connectivity/apis/shared_teams/connections/shared-teams-<32 hex>?$filter=environment eq '<environment>'
   &$expand=ConsentLink` with `{"properties": {"connectionParameters": {}, "consentInfo": {"redirectUrl":
   "https://make.powerautomate.com/oauth/redirect"}, "environment": {"name": <environment>}}}`. The 201 answer is the
   connection, status `Error`/`Unauthenticated`, with `consentInfo.consentLink` and `consentInfo.firstPartyLoginUri`
   (the same `data` on a `*.consent.azure-apihub.net` host, paths `/login` and `/firstPartyLogin`).
2. `POST https://<environment id>.environment.api.powerplatform.com/powerautomate/users/me/onBehalfOfTokenBundle
   ?api-version=1` with the Flow token and no body. The answer's `audienceToToken` maps audiences (Graph, API Hub,
   Power Platform, Power Apps and others) to tokens Power Automate issued for the actor.
3. `POST <firstPartyLoginUri>` with `{"accessToken": <the bundle's https://apihub.azure.com token>}` and no
   `Authorization`: 200, and the connection is signed in.

The connector's metadata marks it `IsFirstParty` and `IsOnbehalfofLoginSupported`. The template's dropdowns then read
the account's teams through the connection (`apim/teams/<connection>/beta/me/teamwork` on the API Hub runtime). The
environment host is the environment name in lower case without dashes, with a dot before its last two characters.

The native `AuthorizeConnection` makes the same three requests: the connection through the classic Power Apps route
with the same body and `$expand=ConsentLink`, the bundle on the environment host with the account's own Flow token,
and the bundle's API Hub token posted only to a `firstPartyLoginUri` on a Microsoft consent host's exact
`/firstPartyLogin` path. The bundle never leaves the transport, and the account's own tokens never go to the consent
host. A connection the service already reports `Connected` is reused. The native card editor takes the teams and
channels from the account's store rather than through the connector.

## Teams' cards for the other templates

Seen 2026-11-04 the same way, for every template the channel's dialog lists: the weekly reminder, Planner, the two
SharePoint templates and forwarding email. Each was saved once and deleted again; the lab team's only member was the
lab account. Every one opens in the same card editor as the webhook template, never a form of the definition.

The markup drives the card. Its `parameters` are the fields, in `editorBindings` order, each by `typeName`;
`optional: true` leaves the red star off; `dependencies` name the fields a field's choices depend on. Its
`dataBindings` map each action or trigger (by name, possibly nested in a loop or branch) to the inputs each field
fills: `${context['parameters']['X']}` into `${json['inputs']['parameters']['groupId']}`, or into a trigger's
`recurrence`. The labels and the card's title are the Workflows app's own words, which the markup names only by
resource key; a field without a label resource gets its kind's ("Start date", "Time", "Time zone", "Day", "Channel",
"SharePoint library", "SharePoint folder").

| `typeName` | field |
|---|---|
| `Teams.Group`, `Teams.Channel` | the comboboxes above; the template's label ("Select team", "Team to notify", "Team") |
| `Teams.MessageBody` | a message box ("Add message") with Teams' formatting bar; the text goes to `body/messageBody` |
| `BuiltIn.Frequency` | fixed (`Week`), not shown |
| `BuiltIn.Interval` | "Repeat every (weeks)", 1 by default, "Specify the interval" when empty; anything but a whole number from 1 to 71 shows "Enter a number between 1 and 71, with no decimals" under it and keeps Save off |
| `BuiltIn.StartDate` | a date field (`11/4/2026`) with a calendar; earlier days can't be picked; sent as `2026-11-04T00:00:00` |
| `BuiltIn.TimeOfDay` | a combobox of half-hours, the next five-minute mark by default (`8:05 PM`); sent as `20:05` into `schedule.hours`/`schedule.minutes` |
| `BuiltIn.Timezone` | Windows' zones (`Turkey Standard Time`, shown "(UTC+03:00) Istanbul"), the local one by default |
| `BuiltIn.WeekDays` | seven round toggles S M T W T F S, the template's (Monday) by default; sent in week order into `schedule.weekDays` |
| `Planner.GroupId`, `Planner.PlanId`, `SharePoint.Site`, `SharePoint.List`, `SharePoint.Library` | comboboxes of a connector's dynamic list; empty, with "No options available", until what they depend on is chosen |
| `SharePoint.FolderPath`, `Outlook.MailFolderPath` | a folder picker: "Your folders", the app's name ("SharePoint", "Outlook") and the folders browsed into, then each folder (choose it, or its chevron to look inside) |
| `Outlook.ToLine`, `Outlook.From` | people pickers ("Enter part of a name or email address to find people"): typing asks Graph for users and mail-enabled groups whose display name, mail or sign-in name starts with the text; the chosen show as chips; sent as their addresses joined by `;` |
| `Outlook.SubjectFilter` | a text field |

The titles read "Send a message to <channel> every <n> week(s) on <day, or "selected days"> at <time>" ("every few
week(s)" while the interval is invalid), "Notify <channel> when a Planner task is completed in <plan>", "Notify
<channel> when files or list items are changed in <list>", "Notify <channel> when a new file is uploaded or created
within <library>" and "Forward the message to <channel> when the email meets your selected conditions"; an unchosen
name reads as its kind ("channel", "plan", "list"). The card's chips are the template's apps (trigger's first), each
with its connector icon: Planner, Office 365 Users, Microsoft Teams, SharePoint, Office 365 Outlook.

A connector field's choices come the way Teams reads them, on the environment's Power Platform host with the Flow
token. `GET …/powerautomate/apis/<connector>/apiOperations/<operation>` (the bound action's `host.operationId`) gives
the operation's `inputsDefinition.properties`; the bound input's `x-ms-dynamic-list` (or `x-ms-dynamic-tree`) is posted
back with the values it refers to, `POST …/apis/<connector>/connections/<connection>/listEnum` (or `listTree`) with
`{"parameters": {…}, "dynamicInvocationDefinition": <it>}`; a `parameterReference` takes the value of the field bound
to that input of the same action. A tree's browse adds the folder's `selectionState`. The answer is `value[]` of
`value` and `displayName`. The SharePoint site is the team's own by default, its `teamSiteInformation.sharepointSiteUrl`
from the teams read (the connector's `GetDataSets` listed nothing in the lab); its list ends with "Enter a site URL".

Every app's connection is made the way the webhook template's is, behind the scenes: an existing signed-in one of the
environment is reused (Teams reused the connections it made earlier in the session), otherwise `PUT` a
`shared-<connector>-<hex>` connection, the on-behalf-of bundle and the first-party login. Planner's, Office 365 Users',
SharePoint's and Office 365 Outlook's were made so. The native client also signs in again a connection of the same
connector a failed sign-in left unauthenticated, rather than making another.

Save sends what the webhook's does, with each field's input filled from the bindings: an optional field left empty
takes its input out of the action (an empty `folderPath` or `to` is not sent), and the kept markup holds each value and
its `valueDisplayName`. A Planner Save took several seconds; Teams showed "Saving…" and then a toast, "Still setting up
your workflow / Workflow's saving - you can safely leave this page." The weekly reminder's Overview adds Run and a line
"Active: Next run: 11/9/2026 8:05:00 PM    Last modified: 11/4/2026 8:07:01 PM" (the next run is the trigger's
`nextExecutionTime`).

Edit reads the workflow again and opens the same card, its heading the workflow's name, with the values the kept markup
holds. Its Save is `PATCH …/flows/<flow>` with `definition`, `connectionReferences` (the workflow's own connections),
`displayName` and `templateName`. After a Rename, Edit's heading is the new name and its Save keeps it: the
`displayName` sent is the workflow's name, not the card's title. Save goes back to the Overview, or to the list when Edit
came from the list's menu.

## The workflow list and its menu

Under "Your workflows", "Available in this channel" asks for the flows whose operations name this team and channel
(`$filter=operations/any(…teams.groupId…teams.channelId…)`) and "Workflows" for those the account owns with
`flowState=active`: both list only workflows that are on. A workflow turned off leaves the channel's list at once. A
row is the name (semibold, grey) over "Active", or "Active: Next run: <when>" for a schedule, which also shows a
recurrence mark and Run. "See all" (beside the heading while there are rows) leads to the Workflows app. With none:
"Create an automation for your channel", "Pick one of our ready-to-go templates or build your own from scratch" and
"View templates".

A click on a row opens its Overview. Its "..." menu holds Run (a schedule or a workflow started by hand), Edit, Manage,
Turn off (Turn on), then Details, then Delete. Manage opens the workflow's page in Power Automate in a new browser tab,
`https://make.powerautomate.com/environments/<environment>/flows/<flow>/details`. Turn off spins in the row's Run
place, sends `stop` and reads the list again. Details is the Overview. Delete asks Teams' "Delete automation" question
over the list (Delete spins while it goes) and the list stays. The native client opens Manage's page in the system
browser.

## Lifecycle routes

The classic Flow resource provider operations are grounded in Microsoft's current Power Automate client source:
[Power Automate server](https://github.com/microsoft/power-platform-skills/blob/main/plugins/power-automate/server/mcp.mjs).
The client includes these classic routes as its supported fallback from the newer environment-scoped API. The
native client uses the fixed classic Microsoft origin, without speculative regional hosts.

All environment routes use `api-version=2016-11-01`. Under
`/providers/Microsoft.ProcessSimple/environments`, discovery is a GET. Under `<environment>/flows`, listing and a
single flow read are GETs, creation is POST, updates are PATCH and deletion is DELETE. A flow's `start`, `stop` and
`triggers/<trigger>/run` operations are POSTs. Its `runs` collection and `runs/<run>/actions` are read with GET. A webhook trigger’s
`listCallbackUrl` POST reads its address for copying; the native app never invokes that returned address. Create and update carry a
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
subdomains, with no userinfo, explicit port or fragment. It never posts the account's own bearer token to a
service-returned `firstPartyLoginUri` or a template URL; only the card editor's Microsoft Teams connection posts
Power Automate's own on-behalf-of API Hub token there ([below](#the-teams-connection-behind-the-scenes)). In the full
editor, connector sign-in is completed by the user in the browser, followed by a fresh native connection read. Only a
connection whose status is `Connected` can be bound in the form.

## Execution boundaries

Every mutation reaches the engine's outbox and has one HTTP attempt, with no automatic resend after a timeout,
throttle or uncertain outcome. A read-only app refuses mutations before token lookup. The transport applies no global
lab policy. Before a live lab run, the agent must establish the complete reach of any automation, including later
notifications, under the current lab-contact rule. The card editor's templates were created, edited and deleted live
by a lab account in a team whose only member it was; no workflow was run.

## AI workflows

The public Microsoft Workflows host at `https://automations.teams.cloud.microsoft/home` provides independently
readable client code without installing its Teams app. That client classifies flows using the
`shared_m365copilotv2` and `shared_agentnode` API ids as cloud AI workflows. The native tabs use those exact ids.

The same reference gates AI workflows on the account’s Copilot entitlement. A captured own-account
`POST beta/users/useraggregatesettings` on the discovered Teams middle tier requests `userCopilotSettings` alongside
the other settings selectors. This is a read operation. The native client uses the captured request and requires
both `userCopilotSettings.value.isCopilotAllowedByAdmin` and
`userCopilotSettings.value.eligibility.isCopilotEligible`; missing settings remain unknown/error. The lab account’s
actual response allows Copilot by policy but reports `isCopilotEligible=false`. The UI shows that access state.

The reference additionally combines Copilot Actions from a separately authenticated Augloop session with cloud AI
flows. That session’s list and lifecycle exchange could not be verified with the ineligible permitted lab account.
For eligible accounts the tab shows verified cloud AI flows and explicitly reports the unavailable Copilot Actions
source, rather than claiming a complete or empty AI list. Copilot Actions management remains an implementation
blocker. Opening the Teams modal would install an automation bot, which was blocked; no such install was sent.

## Validation limits

The service’s actual read catalogs, environments and connections were verified with the normal native auth client.
The card editor's creation, edits, Rename, Turn off, deletion, the row menu and the connector lists were exercised live
on the lab tenant (2026-11-04); running a workflow and Manage's page were not. The full editor's creation and running
were exercised against synthetic local services only.
The advanced definition view preserves unsupported action types; a visual action graph and separate Copilot Actions
session are not implemented.

## Runs with inputs

Microsoft’s current client distinguishes empty management runs from payload-bearing manual runs. For Button,
PowerApp, PowerAppV2, Skills and PowerPages triggers, the Logic Flows connector delivers `triggerBody()` with an
API Hub credential. The native form uses the trigger’s declared fields and validates required values, types,
allowed values and simple nested constraints. The transport reads the full flow again and validates the payload
against its fresh schema before starting a run. Unresolved references and unsupported compound input schemas are
refused rather than silently accepted.

A read-only native probe independently verified the actor and both new token audiences. The environment-scoped
Power Platform `/powerautomate/apis/shared_logicflows?api-version=1` returned401 with the native client’s token.
The supported classic Power Apps metadata route
`/providers/Microsoft.PowerApps/apis/shared_logicflows?api-version=2016-11-01&$filter=environment eq '<environment>'`
returned200 and its actual `properties.primaryRuntimeUrl`. No runtime was invoked by that probe.

The typed input run discovers this metadata freshly using only the Power Apps audience. It requires the exact
Microsoft `shared_logicflows` name/id, HTTPS, no userinfo/port/query/fragment, the selected environment’s host prefix
under `.azure-apihub.net`, and the exact `/apim/logicflows` path. Only then does it obtain an
`https://apihub.azure.com` token and POST the JSON body once to
`<primaryRuntimeUrl>/<flow>/triggers/<trigger>/run?api-version=2016-11-01`. Generic requests cannot choose this
runtime or use its credential. Metadata rejection occurs before API Hub credential lookup. Local synthetic tests
prove payload delivery, distinct credential audiences, fresh validation, no replay and external/cross-environment
URL refusal. Input runs retain the same authenticated actor, schema, first-party route and read-only mutation checks;
lab reach is established by the agent before a live run.
