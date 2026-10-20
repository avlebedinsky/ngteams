# Auth: Entra tokens, authz and the skypetoken

Status: 2026-10-19, M0 Task 12. Written from static analysis only, with no live traffic:

- the web bundle of the Windows MSIX, version `26260.1717.5229.7049` (imported with `proto-lab bundles import`; the
  production cloud config, the auth service chunks and the discover/authz code);
- `ms-teams.exe` from the same MSIX (strings);
- the macOS app `Microsoft Teams.app` `26261.303.5165.1465` from `MicrosoftTeams.pkg` (Info.plist, strings, and the
  x86_64 disassembly of `OneAuthProvider::Initialize`);
- the CA probe of 2026-10-19 (`docs/spikes/ca-probe.md`), the only live result so far.

Update 2026-10-20: the lab accounts now have a Teams license (Microsoft 365 Business Basic), and `proto-lab auth`'s
own authz exchange succeeds for all 3 (`docs/spikes/auth.md`). What that exchange showed is marked **Live** below.

Update 2026-10-20, later: the web client itself was captured with the fence armed (M0 Task 11): 3 `load` runs of
account A (one from before the license, with two authz 403s) and 26 `ab-chat` runs of accounts A and B. The captures
stay under the lab's data dir; they were read locally, the auth exchanges through `proto-lab export` reference copies.
What they showed is marked **Capture** below. In the `load` runs the fence was fail-closed and denied every write. In
the `ab-chat` runs it allowed the mapped writes inside the a–b lab chat (chat creation, messages, typing, read
receipts; their answers are in `fence-targets.md`) and denied every other write and every unmapped POST. For the
requests it denied, the capture shows what the client sent, never what the service answered.
Everything else that isn't from the CA probe is what the client code says it does; the
[To confirm by capture](#to-confirm-by-capture) list says what the capture settled and what is still open.

**The web client is not our client.** It signs in as the SPA client `5e3ce6c0-…` with MSAL.js and asks for CAE
tokens, so its tokens live much longer than ours (see [CAE](#cae-and-token-lifetimes)). Headers and audiences carry
over to us, except that the `Origin` it sends authz picks the host of its AFD GTMs ([Region GTMs](#region-gtms));
lifetimes don't.

## The two layers

Teams auth has two layers.

1. **Entra (AAD) access tokens**, one per audience (resource). The client gets them from
   `login.microsoftonline.com` and keeps one refresh token per account.
2. **The skypetoken**, a Teams-issued token. The client gets it by POSTing an Entra token for
   `https://api.spaces.skype.com` to Teams' **authz** endpoint. The same response gives the per-region base URLs of the
   Teams services (the "region GTMs").

Each Teams service accepts one of the two, or both. The service table is [below](#audience--service-table).
**Capture:** the web client now sends an Entra token almost everywhere, chat included; the skypetoken goes only to a
handful of middle-tier calls, the TURN credential endpoint and the AMS token exchanges.

## Client and redirect

| Client | ID | Kind | Redirect | Used by us |
|---|---|---|---|---|
| Teams desktop (OneAuth) | `1fec8e78-bce4-4aaf-ab1b-5451cc387264` | public native client | `https://login.microsoftonline.com/common/oauth2/nativeclient`; on macOS also `msauth.com.microsoft.teams2://auth` | yes |
| Teams web | `5e3ce6c0-2b1f-4285-8d4b-75ee78787346` | SPA | `<web origin>/v2/authv2` (Capture) | no |

Where each redirect comes from:

- **Windows** (`ms-teams.exe`): the client ID and the `nativeclient` URI sit together in the data that
  `OneAuthProvider::Initialize` (`oneauth_provider_win.cpp`) uses. The shell can also go through the Windows broker
  (WAM, `msauth://Microsoft.AAD.BrokerPlugin`, with a `wamDisabled` switch) and knows nested app auth
  (`child_client_id`, `brk_redirect_uri`). Neither matters on Linux.
- **macOS** (`MSTeams`, `oneauth_provider_mac.mm`): `OneAuthProvider::Initialize` builds the OneAuth AAD
  configuration (`initWithClientId:redirectUri:defaultSignInResource:capabilities:`) with the same client ID, the
  default sign-in resource `https://api.spaces.skype.com`, and a redirect picked by a runtime setting:
  `msauth.com.microsoft.teams2://auth` by default, `https://login.microsoftonline.com/common/oauth2/nativeclient` when
  the setting is on. The app doesn't register the `msauth.com.microsoft.teams2` scheme in `CFBundleURLTypes` (it only
  queries `msauth`/`msauthv3`, the Authenticator broker), so the custom-scheme redirect is consumed inside OneAuth's
  own web view. Both URIs are therefore registered on the Teams client; the Mac config cross-checks the redirect we
  use.
- The MSA (consumer) side of the same configuration uses client `8ec6bc83-69c8-4392-8f08-b3c986009232` with resource
  `service::api.fl.teams.microsoft.com::MBI_SSL`. It's out of scope: the lab is a work tenant.

**Decision: native client `1fec8e78-…` with the `nativeclient` redirect.**

- It's a public native client, so the refresh token is a regular one with a sliding lifetime and no
  `refresh_token_expires_in`. The CA probe saw exactly that for all 3 accounts, and a refresh grant worked. A SPA
  redirect would cap the refresh token at 24 hours and force a daily silent re-login through the lab profile.
  **Capture:** the web client's own answers carry `refresh_token_expires_in` of 86399 s on the code redemption and
  a little less on each refresh, so the SPA cap is real.
- `nativeclient` is a page on Microsoft's login host. The code can be taken on navigation to it (as the CA probe did)
  or from the `Location` header at the CDP `Fetch` response stage. The browser never touches a Teams host for this.
- **Fallback:** `msauth.com.microsoft.teams2://auth`. Chrome won't navigate to a custom scheme, so the code has to be
  taken from the 302 `Location` at the `Fetch` response stage, and the navigation failed. Task 14 implements this
  path in any case, because it's the same code.

## Flow

1. **Authorize** (browser, lab profile, fence armed): PKCE (S256) at
   `https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize` with `client_id` = the native client,
   `response_type=code`, the `nativeclient` redirect, `login_hint` = the account's UPN, and
   `scope=https://api.spaces.skype.com/.default openid profile offline_access`. The profile's SSO cookie signs the user
   in without interaction.
2. **Code → tokens**: POST `…/organizations/oauth2/v2.0/token` as a public client (`grant_type=authorization_code`,
   `code`, `redirect_uri`, `code_verifier`, `scope`). The answer has an access token for `api.spaces.skype.com`, a
   refresh token and an ID token. Entra adds `client_info` (the account's uid/utid) only when the request sends
   `client_info=1`, as MSAL does; we don't send it (nor did the CA probe), so we don't get it. The ID token's `oid`
   and `tid` identify the account instead. `login_hint` doesn't decide who signs in (the profile's SSO session does),
   so `teams-auth` stores the refresh token only when the ID token's `aud` is the client and its `preferred_username`
   (and `upn`, if present) is the requested account; an answer without a readable ID token stores nothing. The claims
   are decoded without a signature check: the ID token comes straight from the token endpoint over TLS, in answer to
   our own PKCE-bound redemption (OpenID Connect Core 3.1.3.7).
3. **Other audiences**: POST the same endpoint with `grant_type=refresh_token`, the one refresh token and
   `scope=<resource>/.default` (plus `openid profile offline_access`). Store the newest refresh token each time; Entra
   may rotate it.
4. **Skypetoken**: POST to authz with `Authorization: Bearer <api.spaces.skype.com token>` and no body. Keep the
   skypetoken, its expiry and the region GTMs.
5. **Service calls**: each request gets the credential its service wants (table below), against the base URL that
   the region GTMs give for that service.
6. **Renewal**: Entra tokens are refreshed before `expires_in` runs out, or when a service answers 401 with a claims
   challenge (`WWW-Authenticate: Bearer … error="insufficient_claims", claims="…"`): the client retries with the
   claims passed to Entra, up to 3 times (`claimsChallenge401RetryMax`). The skypetoken is renewed before its expiry,
   or when a service answers 401; the renewal POST carries `ms-teams-authz-type: TokenRefresh`.

**Capture: how the web client does steps 1–3.** Its MSAL.js (browser SKU, 5.6.x) authorizes at
`…/common/oauth2/v2.0/authorize` with PKCE (S256), `prompt=none`, `response_mode=fragment`, `login_hint`, the
`/.default` scope of `api.spaces.skype.com` plus `openid profile offline_access`, and its SPA redirect; the code comes
back in the fragment of the 302 to that redirect. It redeems the code at `…/common/…/token` and does every later
refresh grant at `…/<tenant id>/…/token`, always with `client_info=1` and an `X-AnchorMailbox` form field (both MSAL's,
not needed by us). The answer fields are `token_type`, `scope`, `expires_in`, `ext_expires_in`, `access_token`,
`refresh_token`, `id_token`, plus `refresh_token_expires_in` (SPA only) and `client_info` (asked for). The granted
`scope` lists the resource's scopes and then `<resource>/.default`; Graph's also starts with `email openid profile`.
Every refresh grant answered with a new refresh token. No token-endpoint error was seen.

### Scope rule

The web client turns a resource into a scope by appending `/.default`. A few resources are passed as they are, because
they already name a scope: `https://substrate.office.com/M365.Access`, `https://graph.microsoft.com/.default`,
`https://graph.microsoft.com/Files.ReadWrite`, the consent-service scopes, and the consumer (`*.fl.teams…`) ones. The
desktop shell instead hands the bare resource to OneAuth, which is the v1 "resource" form of the same request. For us
the v2 endpoint with `<resource>/.default` is equivalent.

**Capture:** the rule is applied literally. The Outlook resource is `https://outlook.office.com/`, so the web client
asks for `https://outlook.office.com//.default` (two slashes) and Entra grants scopes spelled the same way.

### One refresh token for every audience?

**Yes, by design; to be proven live.** The evidence:

- The desktop client asks OneAuth for every audience silently, from one signed-in account and one client ID. At
  startup it pre-fetches ("token warming", `enableTokenWarming` on in prod) all of: `api.spaces.skype.com`,
  `chatsvcagg.teams.microsoft.com`, `outlook.office.com/`, `outlook.office.com/search`, `substrate.office.com`,
  `ic3.teams.office.com`, `graph.microsoft.com`, and the meeting-artifacts app `6bc3b958-689b-49f5-9006-36d165f30e00`.
  There is no second login or second client for any of them.
- Entra refresh tokens issued to a public client are multi-resource: one can be redeemed for any resource the client
  is pre-authorized for. The Teams native client is first-party, so its pre-authorizations cover its own services.

What is not known: whether Conditional Access treats the other audiences (Outlook, Graph, SharePoint) as strictly as
`api.spaces.skype.com`, which passed. A CA block shows up as `AADSTS53003` on that one refresh grant, and the other
audiences keep working. Task 14 checks this with a refresh grant per audience (the Entra token endpoint is already on
the fence's own-state list). (Answered since, see [Open questions](#open-questions) 1.)

**Capture:** the web client warms tokens the same way. Right after the code redemption it chains refresh grants, each
with the newest refresh token, for `api.spaces.skype.com` (again, now asking for CAE), `chatsvcagg`,
`outlook.office.com/`, `outlook.office.com/search`, `substrate.office.com`, `ic3.teams.office.com`,
`graph.microsoft.com` and the meeting-artifacts app, and later `m365copilotapp.svc.cloud.microsoft`; every one got 200.
In the `ab-chat` runs the same chain went on after Copilot with presence, asked for as
`https://presence.teams.microsoft.com//.default` (two slashes, the scope rule again), then the Office shell and
Delve tokens; no `load` run got that far.

## authz: the skypetoken exchange

### Endpoints

| Cloud | authz (v1, skypetoken) | authzcore (v2, no skypetoken) |
|---|---|---|
| commercial (prod) | `https://teams.microsoft.com/api/authsvc/v1.0/authz` | `https://teams.microsoft.com/api/authsvc/v1.0/authzcore` |
| commercial, old host | `https://authsvc.teams.microsoft.com/v1.0/authz` | `https://authsvc.teams.microsoft.com/v1.0/authzcore` |
| GCC High | `https://gov.teams.microsoft.us/api/authsvc/v1.0/…` | same path |
| DoD | `https://dod.teams.microsoft.us/api/authsvc/v1.0/…` | same path |
| China (21Vianet) | `https://teams.microsoftonline.cn/api/authsvc/v1.0/…` | same path |
| France / Germany sovereign | `https://teams.sovcloud.fr/…`, `https://teams.sovcloud.de/…` | same path |
| consumer (Teams free) | `https://teams.live.com/api/auth/v1.0/authz/consumer` | `…/authzcore/consumer` |

The old `authsvc.teams.microsoft.com` host is used only when a "deprecated default URL" switch is on. authz is global,
not regional: the region comes back in its answer. An optional `region_gtms_env` query parameter selects a test
environment; prod doesn't set it.

### authz vs authzcore

- **authz** is the default. It returns the skypetoken plus the region GTMs, region, partition and license details.
- **authzcore** is the "AuthZ V2" flow. The client uses it only when the `enableAuthzV2` setting is on and the user
  isn't anonymous. The setting isn't set in the shipped prod config, so it's off by default and a remote flight turns
  it on. authzcore returns no skypetoken: the client logs that "the response is from authzcore and has no Skype
  token", takes the MRI and Skype ID from the `mri` and `skypeId` fields, and sets the expiry to 24 hours unless told
  otherwise. It re-polls authzcore about every 23 hours (`authzV2PollingInterval`). In that mode the middle tier
  gets an Entra token only, every middle-tier request is forced into token migration (`X-MS-Migration: True`), and
  the IC3-backed services (broadcast and calling are visible in the bundle) use Entra tokens for
  `https://ic3.teams.office.com`. How chat authenticates without a skypetoken in this mode isn't clear from the
  bundle.
- **For us:** use authz. It's what prod does by default, it gives us the skypetoken and the region GTMs, and authzcore
  would only matter if Microsoft flips the flight for the whole tenant. Both are on the fence's own-state list.
  **Capture:** every authz POST the web client made went to `…/authz`, never to `authzcore` (4 POSTs in 3 runs, see
  [Request](#request); the other runs made none and reused the skypetoken from an earlier run). Chat itself no
  longer needs the skypetoken (see the [service table](#audience--service-table)); the few calls that still do are
  listed there.
- **Never call `…/authz/signuptrial`** (or `…/authzcore/signuptrial`). It's the "start a Teams trial" variant of the
  same POST, a write that changes the account's licensing. The fence's own-state rules match `authz` and `authzcore`
  as whole paths, so `signuptrial` stays denied; keep it that way.

### Request

`POST <authz URL>`, no body for a normal (non-guest, non-anonymous) user. Headers:

| Header | Value |
|---|---|
| `Authorization` | `Bearer <Entra token for https://api.spaces.skype.com>` |
| `claimsChallengeCapable` | `true` (always, unless the client disables claims challenges) |
| `X-MS-Migration` | `True` (prod sets `enableSTMHeaderForAuthZ`) |
| `ms-teams-authz-type` | `ExplicitLogin` on first sign-in, `TokenRefresh` on renewal |
| `clientRequestId`, `x-ms-request-id` | a fresh correlation GUID |
| `x-ms-client-type`, `x-ms-client-version` | the client type (`web` when unset) and version |
| `X-Client-UI-Language` | the UI locale, when known |
| `ConsistencyLevel` | `Organization;ScenarioId=<id>` only in a license-refresh scenario |

Anonymous and guest users post to `…/visitor` or `…/guest` with a JSON body (tenant ID or meeting URL). Not our case.

**Capture (all 4 authz POSTs of the web client, in 3 of the 29 runs: two 403s in the `load` run from before the
license, then one 200 in a later `load` run and one in an `ab-chat` run; every other run reused the skypetoken it
already had and posted none).** The URL was always `https://teams.microsoft.com/api/authsvc/v1.0/authz`, also when
the page itself ran on `teams.cloud.microsoft` (a cross-site CORS request then), with `Content-Length: 0` and no body.
Headers, apart from the browser's own:

- `Authorization: Bearer` with the web client's Entra token for `https://api.spaces.skype.com` (CAE-capable, see
  [CAE](#cae-and-token-lifetimes));
- `claimsChallengeCapable: true`, `X-MS-Migration: True`, `ms-teams-authz-type: ExplicitLogin` (every capture was a
  fresh page load; no `TokenRefresh` was seen);
- `clientRequestId` and `x-ms-request-id` with one value, a GUID, sometimes prefixed `Core-`;
- `x-ms-client-type: web` and `x-ms-client-version` (two numbers joined by a slash, the same value the chat service's
  `ClientInfo` header names `clientVer`);
- `Origin` and `Referer` of the page.

No `X-Client-UI-Language` and no `ConsistencyLevel`. Our own request is the table above minus the `x-ms-client-*`
headers, `X-Client-UI-Language` and `ConsistencyLevel`, and it carries no `Origin`; so it differs from the web
client's only in the `x-ms-client-*` headers and `Origin`. authz answers both with the same fields, but the `Origin`
isn't neutral: the AFD host of the GTMs follows it (see [Region GTMs](#region-gtms)).

### Response

The client reads these top-level fields:

- `tokens.skypeToken` (also accepted as `skypetoken`) and `tokens.expiresIn` in seconds. The token object can also
  come as a top-level `skypeToken` object (checked first) or `skypeGuestUserToken`, with the same two fields inside.
- The skypetoken is itself a JWT. Its `skypeid` claim gives the user's MRI as `8:<skypeid>`, and its `rgn` claim is
  the fallback when the response has no `region`.
- **Expiry** comes from the JWT's `exp` claim (seconds, so ×1000 for milliseconds). Only when the token has no `exp`
  does the client fall back to the time it parsed the response + `expiresIn` × 1000. When the `expiresIn`-based time is
  more than a minute later than `exp`, the client logs an `IncorrectExpiry` mark and still uses `exp`. We schedule
  renewal the same way, from `exp` first, so a token is never used past its own `exp` even if `expiresIn` says more.
- `regionGtms`: an object from service name to base URL (below).
- `region`, `partition`, `userRegion`, `userPartition`, `regionOverride`, `regionSettings`.
- `licenseDetails` (flags such as `isFreemium`, `isCopilot`, `isTeamsSmb`), `consumerLicenseDetails`.
- `skypeGuestUserToken`, `ocdiRedirect`: guest and redirect cases, not ours.
- authzcore only: `mri`, `skypeId` instead of `tokens`.

The client treats a GTM table shorter than a minimum length as missing and refuses to cache it.

**Live (2026-10-20, our own exchange for lab a, b, c, all alike).** The headers of the table above minus
`x-ms-client-type`, `x-ms-client-version` and `X-Client-UI-Language`, no body: 200. The answer's top-level fields were
exactly `tokens`, `region`, `partition`, `regionGtms`, `regionSettings`, `licenseDetails`, `isMultiGeo` (false) and
`ocdiRedirect` (the string `MicrosoftDefault`). No `userRegion`, `userPartition`, `regionOverride`,
`consumerLicenseDetails` or top-level `skypeToken` object.

- `tokens` holds three fields: `skypeToken`, `expiresIn` and `tokenType` (`SkypeToken`).
- The skypetoken is a JWT (about 3 KB) with the claims `aad_appid`, `aad_iat`, `aad_pft`, `aad_uti`, `acct`,
  `altsecid`, `csi`, `exp`, `iat`, `rgn`, `scp`, `skypeid` and `tid`; no `nbf`. `rgn` matched `region`.
- **Its `exp` is the `exp` of the Entra token that was exchanged, to the second**, and `expiresIn` is that `exp` minus
  the skypetoken's `iat` (one second short of it). So the lifetime is whatever the Entra token had left, about 2–2.5 h
  for a fresh one, not 24 hours. A skypetoken can't outlive its Entra token: renewing it means refreshing the
  `api.spaces.skype.com` token first, and exchanging an old Entra token gives a short skypetoken.
- `region` `amer`, `partition` `amer03` for all 3 accounts.
- `regionSettings`: five flags (`isAppsDiscoveryEnabled`, `isContactMigrationEnabled`, `isFederationEnabled`,
  `isOutOfOfficeIntegrationEnabled`, `isUnifiedPresenceEnabled`). `licenseDetails`: 38 fields, among them
  `isTeamsEnabled` and `isTeamsSmb` true, `isFreemium` and `isTrial` false.

**Capture (the web client's 200).** The same top-level fields, `tokens` fields, claims, `regionSettings` and
`licenseDetails` as our own exchange, and 112 GTMs. Again the skypetoken's `exp` equals the exchanged Entra token's
`exp` to the second; its `iat` is 301 s after the Entra token's (Entra back-dates `iat` by 5 minutes, authz doesn't).
Because the web client's Entra token was a long-lived CAE one, its `expiresIn` was about 22 h (80283 s; 76477 s in an
`ab-chat` run). The skypetoken lifetime is therefore not a Teams setting at all: it is whatever the exchanged token has
left, about 2 h for ours, about a day for the web client's.

### Errors

- **403**: the user may not use Teams. The body is JSON with `errorCode` and `message`. The codes the client knows
  include `UserLicenseNotPresentTrialEligible`, `UserLicenseNotPresentTrialExpired`,
  `UserLicenseNotPresentTrialHardBlocked`, `TeamsDisabledForTenantForbidden`, `TrialLicenseDisabledForTenantForbidden`,
  `UserNotFoundInSkypeTeam`, `UserChatDisabled`, and admin variants. The lab accounts got
  `UserLicenseNotPresentTrialEligible` before they were licensed; the "start trial" page Task 8 saw was that refusal.
  **Capture:** the web client's two 403s had exactly those two fields, `errorCode` (that code) and a one-sentence
  English `message`, as `application/json; charset=utf-8`. The answer lists `X-Error-Code` among its exposed headers
  but didn't send one.
- **401**: the Entra token was refused; refresh it (with claims, if challenged) and retry.
- **Retries** (`enableAuthzRetriesAndThrottleClients` on in prod): retryable statuses (throttling, 5xx) are retried with
  exponential backoff and jitter, and the failure is cached for a short TTL so callers don't hammer authz. 403 and
  proxy-auth errors aren't retried.

### Region GTMs

The client looks up every regional service by name in `regionGtms` and resolves request paths against that base URL.
Names it uses, most-used first: `middleTier`, `mtImageService`, `groupsServiceV2`, `urlpV2`, `urlp`, `amsV2`,
`presenceUPS`, `ams`, `userIntelligenceService`, `unifiedPresence`, `peopleProfileService`, `chatService`,
`chatServiceAfd`, `chatSvcAggAfd`, `virtualEventsService`, `userProfileService`, `teamsPushServiceAfd`, `mcpService`,
`iC3LocationService`, `consumerLicenses`, `authService`, `appsSuggestionService`, `searchService`, `graphService`, and a
few rarer ones. We persist the whole object as returned, not a fixed list.

**Live (2026-10-20):** 112 entries for each of a, b and c. Present from the list above: `middleTier`,
`mtImageService`, `urlpV2`, `urlp`, `amsV2`, `presenceUPS`, `ams`, `userIntelligenceService`, `unifiedPresence`,
`peopleProfileService`, `chatService`, `chatServiceAfd`, `chatSvcAggAfd`, `virtualEventsService`, `userProfileService`,
`teamsPushServiceAfd`, `mcpService`, `iC3LocationService`, `appsSuggestionService`. Absent: `groupsServiceV2`,
`consumerLicenses`, `authService`, `searchService` (there is a `search`) and `graphService`, so code that wants them
needs a fallback. Many of the others are `…S2S` and `calling…` entries. Some bases as given (no trailing slash):

| Name | Base URL |
|---|---|
| `chatServiceAfd` | `https://teams.microsoft.com/api/chatsvc/amer` |
| `chatService` | `https://amer.ng.msg.teams.microsoft.com` |
| `middleTier` | `https://teams.microsoft.com/api/mt/part/amer-03` |
| `ams` | `https://us-api.asm.skype.com` |
| `unifiedPresence` | `https://presence.teams.microsoft.com` |
| `teamsPushServiceAfd` | `https://teams.microsoft.com/api/tps` |

**Capture:** the web client's GTMs had the same 112 names, but the AFD bases (24 of them: `chatServiceAfd`,
`chatSvcAggAfd`, `middleTier`, `mtImageService`, `presenceUPS`, `teamsPushServiceAfd`, `userProfileService`,
`iC3LocationService`, `calling_registrarUrl` and others) were on `teams.cloud.microsoft`, the origin its page ran on
and sent as `Origin`, where ours, posted without an `Origin`, were on `teams.microsoft.com`. It looks like authz picks
the AFD host from the caller's origin; either host serves the same paths. `presenceUPS` is
`<AFD host>/ups/<geo>` (`noam` for the lab). One entry (`acmService`) came with a leading space before `https://`:
values are kept as returned, so a consumer has to trim. The page also resolved AFD services against its own origin
whatever the GTMs said: in `ab-chat` runs that landed on `teams.microsoft.com` through a deep link, chat, middle tier,
CSA and presence requests all went to `teams.microsoft.com`.

Chat specifics: prod sets `useChatServiceAfd`, so the chat base URL is `chatServiceAfd` (falling back to `chatService`
if absent). Chat and presence requests carry `BehaviorOverride: redirectAs404`; a wrong region answers 404 with a
`Location`, and the client re-bases on it and retries, up to 10 times for chat and 3 for presence. On an AFD host
(`teams.microsoft.com`, `teams.cloud.microsoft`, …) the new base is the origin plus the `/api/chatsvc/<region>/`
prefix of that `Location`.

**Capture:** every chat request went to `chatServiceAfd` (`<AFD host>/api/chatsvc/amer`), none to `chatService`
(`amer.ng.msg…`). That host showed up only in answers: as the `chatService` GTM, and in the `Location` header of the
201s to chat creation and message posts, which name the new resource as
`https://amer.ng.msg.teams.microsoft.com/v1/…` (`fence-targets.md`). Every chat and presence request carried
`BehaviorOverride: redirectAs404`. No region hop happened: no 404 came with a `Location`. The 404s seen were ordinary
misses (`users/ME/properties`, and the call-log conversation `48:calllogs`, which answered 404 in some runs and 200 in
others), with a diagnostic code in the `ContextId` answer header and no `Location`. How a hop looks is still unseen;
the lab's home region matches its GTMs.

## Audience × service table

"Entra (aud X)" means `Authorization: Bearer <Entra access token for X>`; "STM" means the request also carries
`X-MS-Migration: True` ([STM](#skype-token-migration-stm)). Rows marked **Capture** say what the web client sent in the
captures of 2026-10-19/20; the fence denied most POSTs and every unmapped write, so for those it shows the request,
not whether the service accepts it. Rows marked *bundle* weren't seen on the wire and say what the client code does.

| Service | Base URL from | Credential | Entra audience |
|---|---|---|---|
| authz (POST) | fixed (table above) | **Capture:** Entra, STM | `https://api.spaces.skype.com` |
| authsvc license checks (`/api/authsvc/v1.0/lfts/…`, GET) | fixed, `teams.microsoft.com` | **Capture:** Entra | `https://api.spaces.skype.com` |
| Middle tier (`/api/mt/…`) | GTM `middleTier` | **Capture:** three forms. Most calls (GET and POST): Entra, STM, no `x-skypetoken`. A few: Entra **and** `x-skypetoken`, no STM (`v2.0/places/findPlaces`, `v2.0/users/licenseDetails`, `beta/sharepoint/sites/validSites`, `beta/tenant/verifiedDomains`). Two: Entra only (`beta/me/files/availability`, `v1/mcps/initArtifactFolder`) | `https://api.spaces.skype.com` |
| MT images (`…/beta/users/<id>/profilepicturev2/…`, GET) | GTM `middleTier` (`mtImageService` has the same base) | **Capture:** no `Authorization`; image requests rely on a cookie the client gets from POST `…/beta/users/<id>/cookiev2` (Entra, STM; denied by the fence). Without it the answer is 401 with `WWW-Authenticate: Bearer authorization_uri="https://login.microsoftonline.com/common"` | — |
| Chat service (chatsvc: messages, threads, conversations, properties) | GTM `chatServiceAfd` | **Capture:** Entra, STM, `BehaviorOverride: redirectAs404`. Never the bundle's `Authentication: skypetoken=…`, and no `RegistrationToken` (no endpoint was registered in any run) | `https://ic3.teams.office.com` |
| Chat service aggregator (CSA) | GTM `chatSvcAggAfd` / `csaResource` | **Capture:** Entra, with or without STM; never a skypetoken (`disableSkypeTokenOnCsa` on). *Bundle:* CSA rejects tokens for `teams.microsoft.com`, `api.spaces.skype.com` and `ic3` | `https://chatsvcagg.teams.microsoft.com` |
| Push service (`/api/tps/…`) | GTM `teamsPushServiceAfd` | **Capture:** Entra, no STM | `https://chatsvcagg.teams.microsoft.com` (the CSA token) |
| Presence | GTM `presenceUPS` (`<AFD host>/ups/<geo>`), not `unifiedPresence` | **Capture:** Entra, `BehaviorOverride: redirectAs404`, no skypetoken, no STM; every request was a POST (`v1/presence/getpresence` and the like), all denied by the fence | `https://presence.teams.microsoft.com/` (with the slash) |
| AMS token exchanges (`ams`, `amsV2`) | GTMs | **Capture:** POST `/v1/skypetokenauth` with `Authorization: skype_token <skypetoken>` on both hosts; on `amsV2` also POST `/v1/<id>/aadtokenauth` with Entra, STM. All denied by the fence | `https://ic3.teams.office.com` (`aadtokenauth`) |
| URL preview (`urlp`, `urlpV2`) | GTMs | *bundle:* `Authorization: skype_token <skypetoken>`; not seen | — |
| Trouter (`go.trouter.teams.microsoft.com`) | fixed; `calling_trouterUrl` | **Capture:** no credential on the HTTP probe (`GET /?check=…`) or the WebSocket handshake; the first frame the client sends, a `user.authenticate` event, carries `Authorization: Bearer …` and `X-MS-Migration: True` as its headers (denied by the WebSocket guard) | `https://ic3.teams.office.com` |
| TURN credentials (`/trap/tokens`, GET, `api-version: 2`) | fixed, `teams.microsoft.com` | **Capture:** `x-skypetoken` alone, or Entra with STM; answers 200 with relay credentials for the `rtcmedia` realm valid 7 days (`expires` 604800) | `https://ic3.teams.office.com` (Entra form) |
| User profile service | GTM `userProfileService` | *bundle:* Entra | `https://api.spaces.skype.com` |
| Other regional services (default binding) | their GTM | *bundle:* Entra | the GTM base URL (origin) |
| IC3 gateway in "token migration" mode | — | *bundle:* Entra, STM; falls back to the skypetoken when the service says so. **Capture:** only the CAE audience was used; `https://ic3-non-cae.teams.office.com` never appeared | `https://ic3.teams.office.com` (CAE) or `https://ic3-non-cae.teams.office.com` |
| Microsoft Graph | `graphEndpointBaseUrl` | **Capture:** Entra (GET `/v1.0/…`, 200) | `https://graph.microsoft.com` |
| Outlook / mail and calendar | `ewsResource` | **Capture:** Entra, sent to `substrate.office.com/userknowledgebase/…` (GET, 204) | `https://outlook.office.com/` (with the slash) |
| Substrate search (`substrate.office.com/search/api/v1/suggestions`, POST) | `substrateBaseUrl` | **Capture:** Entra; the token is asked for as `https://outlook.office.com/search/.default` but its `aud` is the application ID `66a88757-258c-4c72-893c-3e8bed4d6899`, not the URL | `https://outlook.office.com/search` (`substrateTokenUrl`); *bundle:* some calls use the scope `https://substrate.office.com/M365.Access` |
| Substrate (other) | `substrateBaseUrl` | **Capture:** the token is warmed, but no request used it | `https://substrate.office.com` |
| SharePoint / OneDrive files | per site | *bundle:* Entra | the SharePoint host |
| Office roaming settings | — | *bundle:* Entra | `https://api.office.net` |
| Meeting artifacts, attendance, templates | `teams.microsoft.com/api/…` | **Capture:** the token is warmed, but no request used it | app ID `6bc3b958-689b-49f5-9006-36d165f30e00` |
| App Studio / developer portal | — | *bundle:* Entra | `https://dev.teams.microsoft.com` |

Not Teams' own, seen in the captures: Delve's live persona card (`loki.delve.office.com`) gets an Entra token whose
audience is an application ID, and the Office shell's nav bar (`webshell.suite.office.com`) a bearer token that isn't
a JWT. Neither matters for M0.

### CAE and token lifetimes

Prod lists `api.spaces.skype.com`, `chatsvcagg`, `outlook.office.com`, `ic3.teams.office.com`, SharePoint and presence
as CAE-capable audiences. A CAE client advertises the capability when it asks Entra for a token (the `xms_cc` claim,
`cp1`) and must then handle claims challenges. We don't advertise it in M0 (the CA probe didn't either), so our tokens
are ordinary ones.

**Capture.** The web client advertises it by sending `claims={"access_token":{"xms_cc":{"values":["CP1"]}}}` on the
refresh grant, for `api.spaces.skype.com`, `chatsvcagg`, `outlook.office.com/`, `outlook.office.com/search`, `ic3`
and presence (`https://presence.teams.microsoft.com/`, warmed only in the `ab-chat` runs); not for Graph, substrate,
the meeting-artifacts app or Copilot. The tokens that came back with `xms_cc` (spaces, `chatsvcagg`, Outlook, `ic3`,
presence) lived about 20–28 hours (`expires_in` 72 000–100 000 s); the others about 2 hours (7 200–9 000 s), the
Outlook search token included: it was asked with `claims` but came back without `xms_cc`, its `aud` an application
ID. The very first token, from the code redemption without `claims`, was a 2-hour one, and the client re-asked for
the same audience with CAE at once. That is why the web client's skypetoken lasts about a day and ours about 2 hours
([Response](#response)). No claims challenge (a 401 with `insufficient_claims`) came in any capture, so its shape is
still only what the bundle says.

### Skype token migration (STM)

Microsoft is moving IC3 services from the skypetoken to Entra tokens. In prod `skypeTokenMigrationEnabled`,
`enableSkypeTokenMigrationForMT` and `enableSkypeTokenMigrationForTrouter` are on, and enforcement
(`enableSTMEnforcement`) is off. What the client does:

- Requests marked for migration carry `X-MS-Migration: True` and an Entra token for the IC3 gateway audience (CAE or
  non-CAE).
- A 401 whose `WWW-Authenticate` names the highest protocol the service accepts (CAE, AAD or Skype) makes the client
  retry with that credential, falling back CAE → AAD → skypetoken. It remembers the fallback for 24 hours
  (`stmFallbackMapTimerInHr`).
- For the middle tier, which requests are migrated comes from `stmSourceList`, matched against the "source" tag each
  middle-tier call is made with. The shipped prod config already lists 20 sources, every one with
  `disableSkypeTokenInHeader` on: among them `userProfileService`, `teamsAndChannels`, `files`, `people`,
  `peoplePhase2`, `policyService`, `userFeatureSettingsService`, `tenants-service-endpoints`, `safelinks`,
  `appsPlatform`, `invite` and the sensitivity-label ones. A remote flight can change the list.
  - A middle-tier request whose source is listed gets the migration headers (`X-MS-Migration: True` and a companion
    header) and the Entra token only, with no `x-skypetoken`.
  - Any other middle-tier request still gets both the Entra token and `x-skypetoken` (`requireSkypetokenForMT`),
    unless `enableAuthzV2` is on, which forces every middle-tier request into the Entra-only form.

**Capture.** Migration is the common case now, not the exception. `X-MS-Migration: True` was on the authz POST, on
every chat request, on most middle-tier calls, on some CSA calls, on the AMS `aadtokenauth` exchange, on the Entra form
of `/trap/tokens` and inside trouter's `user.authenticate` frame: about 4 600 requests over the captures. It was the
only migration header; the "companion header" the bundle mentions never appeared. Every Entra token sent with it was
the CAE `ic3` one, except on the middle tier and authz (their `api.spaces.skype.com` token) and CSA (its `chatsvcagg`
token). No answer carried a migration header and no 401 named a protocol, so the fallback was never exercised. Which
middle-tier calls keep `x-skypetoken` is in the table above; whether a migrated endpoint would also accept the
both-headers form is still unknown, since no request sent both.

For us this means: chat goes with the `ic3` Entra token and STM, as the web client does. The fence's live membership
read (Task 11) is not that form: it sends back the page's `ic3` bearer token with only an `Accept` header, no
`X-MS-Migration` and no `BehaviorOverride` (`fence-targets.md`). Its `GET v1/threads/<id>` succeeded that way in the
`ab-chat` runs, so the chat service took the web client's CAE `ic3` token without STM on at least that read.

**Live (M1 Task 3, 2026-10-20, `proto-lab probe chat a` and `b`):** the chat service takes **our** `ic3` token too
(the native client's, from the stored refresh token, no `xms_cc`, a ~2 h lifetime) and does not care about the extra
headers on reads: one page of the conversation list
(`GET v1/users/ME/conversations?view=msnp24Equivalent&pageSize=200&startTime=1&targetType=All`, the bundle's
`myconversations` request) answered 200 in all four forms, the token with
both `X-MS-Migration: True` and `BehaviorOverride: redirectAs404`, with either one alone, and with neither; and the
fence's own read (`GET v1/threads/<the a–b chat>?view=msnp24Equivalent`, token and `Accept` only) was complete with
both members, for a and for b. No 401, no `WWW-Authenticate`, no migration header in any answer; the answer headers
were `cache-control`, `content-length`, `content-type`, `contextid`, `date`, `ms-cv`, `pragma`, `x-cache`,
`x-ms-latency`, `x-msedge-ref`. The page's body is `{conversations, _metadata: {totalCount, syncState,
forwardLink}}` (5 conversations for a); the two pages are kept as local reference copies
(`reference/chat/probe-conversations-{a,b}.jsonl`) for the chat notes, never in the repo. So M1's native client
sends chat requests in the web client's form (the `ic3` bearer token, `X-MS-Migration: True`,
`BehaviorOverride: redirectAs404`): all of them work on reads, and that is the form whose behaviour (a region hop
as a 404 with `Location`) the web client relies on. The skypetoken is
still needed for the few middle-tier calls above, the TURN credentials and the AMS exchanges, so authz stays part of
sign-in. Whether chat still accepts the skypetoken form is unknown and not needed.

## Open questions

1. ~~Does one refresh token really yield every audience in the table for the lab accounts, or does Conditional Access
   block some of them (Outlook, Graph, SharePoint) for a non-compliant Linux device?~~ **Answered (Task 14,
   `docs/spikes/auth.md`): yes.** One refresh grant each gave a token for `chatsvcagg`, presence, `ic3`, Graph,
   Outlook, Outlook search and substrate, for all 3 lab accounts, with no Conditional Access block. SharePoint wasn't
   asked: it needs a site host.
2. ~~What is the real `tokens.expiresIn` of a skypetoken (the client defaults to 24 hours only in the authzcore
   path)?~~ **Answered (2026-10-20, live and capture):** the skypetoken carries `exp`, equal to the exchanged Entra
   token's `exp`; `expiresIn` is the same lifetime in seconds: about 2–2.5 h for our fresh Entra token, about 21–22 h
   for the web client's CAE one (see [Response](#response)).
3. Which runtime setting makes macOS use `nativeclient` instead of `msauth.com.microsoft.teams2://auth`? It doesn't
   matter while `nativeclient` works for us.
4. Does presence accept a token whose audience is the presence origin, or does it want `api.spaces.skype.com`? **Half
   answered (capture):** the web client sends a token for `https://presence.teams.microsoft.com/` to `presenceUPS`.
   Every presence request was a POST the fence denied, so whether presence accepts it (and a non-CAE one like ours)
   is still open.
5. Is the `RegistrationToken` (from the chat service's `Set-RegistrationToken` response header after registering an
   endpoint) needed for reads, or only for the event channel? **Capture:** the web client registered no chat
   endpoint and sent no `RegistrationToken` in any run, and its chat reads worked with the `ic3` token alone. For the
   event channel it belongs to the chat notes.
6. Will the user's own account (the product, not the lab) face stricter Conditional Access (MFA, device compliance)
   than the bot accounts? Not testable in M0 without that account.
7. ~~Trouter's credential under STM (skypetoken vs IC3 Entra token)~~ **Answered (capture):** the `ic3` Entra token
   with `X-MS-Migration: True`, sent inside the first socket frame (`user.authenticate`), not in a header of the
   handshake. Whether trouter accepts it was not seen: the WebSocket guard denied the frame.
8. ~~Does the chat service accept **our** `ic3` token (native client, no CAE) the way it accepts the web client's?~~
   **Answered (live, M1 Task 3): yes.** For a and b, our non-CAE `ic3` token read the conversation list (200 with and
   without `X-MS-Migration`, with and without `BehaviorOverride`) and the a–b chat's thread (complete), with no claims
   challenge and no protocol named in a 401 ([STM](#skype-token-migration-stm)). So M1 needs neither CAE (`xms_cc`
   `CP1` and claims-challenge handling) nor the `https://ic3-non-cae.teams.office.com` audience.
9. How does a region hop (a 404 with `Location`) look on the wire? None happened: the lab's home region matches its
   GTMs. M1 Task 3's probe saw none either: every read on `chatServiceAfd` (`<AFD host>/api/chatsvc/amer`) answered
   200, with no `Location`, with and without `BehaviorOverride: redirectAs404`.

## To confirm by capture

The `load` and `ab-chat` captures of 2026-10-19/20 (M0 Task 11) settled what they could:

- [x] the authz POST the web client makes: `…/authz` (never `authzcore`) on `teams.microsoft.com`, an empty body,
      200 once licensed; headers as listed under [Request](#request) (**Capture**). No `X-Client-UI-Language`;
- [x] the authz response: `tokens.skypeToken`, `tokens.expiresIn`, `region`, `partition`, the `regionGtms` names and
      their hosts, `licenseDetails`. Confirmed by our own exchange for all 3 accounts (Live) and by the web client's
      (Capture, with its GTMs on the page's own AFD host), under [Response](#response) and [Region GTMs](#region-gtms);
- [x] that `api.spaces.skype.com` is the authz audience: authz turns a token for it into a skypetoken (our own
      exchange, all 3 accounts, and the web client's);
- [x] the chat service host: `chatServiceAfd` on the page's own AFD host, never `chatService`. A 404 `Location` region
      hop didn't happen, so its shape is still open (question 9);
- [x] which middle-tier requests carry both `Authorization: Bearer` and `x-skypetoken` (four paths), which carry
      `X-MS-Migration: True` with no `x-skypetoken` (all the others seen but two, which carry neither): see the
      [table](#audience--service-table);
- [x] the audiences of the Bearer tokens the page sends to presence (`https://presence.teams.microsoft.com/`), CSA
      (`chatsvcagg`), Graph, Outlook (`https://outlook.office.com/`) and substrate search (an application ID); whether
      presence accepts its token is still open (question 4);
- [x] whether any request carries `X-MS-Migration: True` with an IC3 Entra token: yes, chat, the AMS `aadtokenauth`
      exchange, `/trap/tokens` and trouter's authenticate frame ([STM](#skype-token-migration-stm));
- [ ] the shape of a 401 claims challenge: none happened. The only 401s were middle-tier image requests without their
      cookie, with a plain `WWW-Authenticate: Bearer authorization_uri="…"`. Still open. M1 Task 3's probe with our
      own non-CAE `ic3` token got no 401 at all on the chat service, so nothing in M1 depends on it: CAE's claims
      challenges are meant for a client that advertises `CP1`, which ours doesn't (question 8);
- [x] the body of the 403 the lab accounts got before the license: `errorCode` `UserLicenseNotPresentTrialEligible`
      and a `message`, nothing else (Task 14's own exchange and the web client's, under [Errors](#errors)).

The fixtures below were then rewritten from the capture's reference copies where it showed the shape.

## Fixtures

`tests/fixtures/auth/*.json` are **synthetic**: every value is made up, every secret or identifier is an
angle-bracket placeholder, and every regional host is under `example.test`. Each file has a `_synthetic` note and
holds one exchange as `{request, response}`; `response` has `status`, `headers` and `body`. They exist so that the
teams-auth tests (Tasks 13 and 14) run against wiremock.

Four of them are **written from a capture's reference copy**: the auth exchanges of the `load` captures were cut out
locally and run through `proto-lab export`, and the fixture was written by hand from that copy with the same
structure and made-up values (captures never enter the repo, not even sanitized). The two authz ones copy the web
client's request headers and answer layout. The two Entra ones copy the answer's fields and headers but keep our
native-client request: the capture is of the web client's SPA, whose answers also carry `refresh_token_expires_in` and
`client_info`, which the native client doesn't get. The other five show what no capture had (an Entra error, an
authzcore answer) and stay made up from the documented or bundle shape; each note says which.

Most placeholders have the sanitizer's `<kind:id>` shape (`<token:N>`, `<guid:N>`, `<mri:a>`). A few are written by
hand with kinds the sanitizer convention doesn't define: `<code:1>` and `<verifier:1>` (the authorization code and
PKCE verifier), `<token:skype>` (the skypetoken), `<skypeid:a>`, and `<client-version>`, which has no id at all. The
skypetoken placeholder isn't a JWT, so a test of the `exp` expiry rule has to build an unsigned JWT at run time (the
fixture guard rejects committed ones). Tests should treat every placeholder as an opaque string, never match on its
text: a later rewrite from another reference copy may name or number the placeholders differently.

| File | Exchange | Source |
|---|---|---|
| `entra-token-code.json` | code → tokens for `api.spaces.skype.com` | written from the capture's reference copy |
| `entra-token-refresh.json` | refresh grant for a second audience (Graph) with the same refresh token | written from the capture's reference copy |
| `entra-error-invalid-grant.json` | expired refresh token (`invalid_grant`, `AADSTS700082`) | made up, no capture shows it |
| `entra-error-aadsts50011.json` | unregistered redirect URI | made up, no capture shows it |
| `entra-error-aadsts53003.json` | Conditional Access block | made up, no capture shows it |
| `entra-error-429.json` | throttled, with `Retry-After` | made up, no capture shows it |
| `authz-ok.json` | authz with a skypetoken and region GTMs | written from the capture's reference copy |
| `authzcore-ok.json` | authzcore: MRI and GTMs, no skypetoken | made up, no capture shows it |
| `authz-403-no-license.json` | authz refusing an unlicensed user | written from the capture's reference copy |
