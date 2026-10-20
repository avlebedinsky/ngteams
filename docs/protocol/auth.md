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
Everything else that isn't from the CA probe is what the client code says it does; the points that still need a
capture of the web client are listed in [To confirm by capture](#to-confirm-by-capture).

## The two layers

Teams auth has two layers.

1. **Entra (AAD) access tokens**, one per audience (resource). The client gets them from
   `login.microsoftonline.com` and keeps one refresh token per account.
2. **The skypetoken**, a Teams-issued token. The client gets it by POSTing an Entra token for
   `https://api.spaces.skype.com` to Teams' **authz** endpoint. The same response gives the per-region base URLs of the
   Teams services (the "region GTMs").

Each Teams service accepts one of the two, or both. The service table is [below](#audience--service-table).

## Client and redirect

| Client | ID | Kind | Redirect | Used by us |
|---|---|---|---|---|
| Teams desktop (OneAuth) | `1fec8e78-bce4-4aaf-ab1b-5451cc387264` | public native client | `https://login.microsoftonline.com/common/oauth2/nativeclient`; on macOS also `msauth.com.microsoft.teams2://auth` | yes |
| Teams web | `5e3ce6c0-2b1f-4285-8d4b-75ee78787346` | SPA | the Teams web origin | no |

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

### Scope rule

The web client turns a resource into a scope by appending `/.default`. A few resources are passed as they are, because
they already name a scope: `https://substrate.office.com/M365.Access`, `https://graph.microsoft.com/.default`,
`https://graph.microsoft.com/Files.ReadWrite`, the consent-service scopes, and the consumer (`*.fl.teams…`) ones. The
desktop shell instead hands the bare resource to OneAuth, which is the v1 "resource" form of the same request. For us
the v2 endpoint with `<resource>/.default` is equivalent.

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
the fence's own-state list).

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
- **For us:** use authz. It's what prod does by default, it gives us the skypetoken that chat needs, and authzcore
  would only matter if Microsoft flips the flight for the whole tenant. Both are on the fence's own-state list.
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

### Errors

- **403**: the user may not use Teams. The body is JSON with `errorCode` and `message`. The codes the client knows
  include `UserLicenseNotPresentTrialEligible`, `UserLicenseNotPresentTrialExpired`,
  `UserLicenseNotPresentTrialHardBlocked`, `TeamsDisabledForTenantForbidden`, `TrialLicenseDisabledForTenantForbidden`,
  `UserNotFoundInSkypeTeam`, `UserChatDisabled`, and admin variants. The lab accounts got
  `UserLicenseNotPresentTrialEligible` before they were licensed; the "start trial" page Task 8 saw was that refusal.
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

Chat specifics: prod sets `useChatServiceAfd`, so the chat base URL is `chatServiceAfd` (falling back to `chatService`
if absent). Chat and presence requests carry `BehaviorOverride: redirectAs404`; a wrong region answers 404 with a
`Location`, and the client re-bases on it and retries, up to 10 times for chat and 3 for presence. On an AFD host
(`teams.microsoft.com`, `teams.cloud.microsoft`, …) the new base is the origin plus the `/api/chatsvc/<region>/`
prefix of that `Location`.

## Audience × service table

"Entra (aud X)" means `Authorization: Bearer <Entra access token for X>`.

| Service | Base URL from | Credential (prod defaults) | Entra audience |
|---|---|---|---|
| authz | fixed (table above) | Entra | `https://api.spaces.skype.com` |
| Middle tier (`/api/mt/…`) | GTM `middleTier` | Entra **and** `x-skypetoken: <skypetoken>` (`requireSkypetokenForMT` on), except calls whose source is on the STM list, which get Entra only ([STM](#skype-token-migration-stm)) | `https://api.spaces.skype.com` |
| MT image service | GTM `mtImageService` | none added by this binding; the client sets a separate image auth cookie (to confirm) | — |
| Chat service (chatsvc, messages, threads, endpoints) | GTM `chatServiceAfd` | `Authentication: skypetoken=<skypetoken>`, then `RegistrationToken` once an endpoint is registered | — |
| Chat service aggregator (CSA) | GTM `chatSvcAggAfd` / `csaResource` | Entra only (`disableSkypeTokenOnCsa` on); CSA rejects tokens for `teams.microsoft.com`, `api.spaces.skype.com` and `ic3` | `https://chatsvcagg.teams.microsoft.com` |
| Presence (`unifiedPresence`) | GTM `unifiedPresence` | Entra, unless the presence settings ask for `x-skypetoken` | the GTM base URL itself (likely `https://presence.teams.microsoft.com`) |
| AMS / URL preview (`ams`, `amsV2`, `urlp`, `urlpV2`) | GTMs | `Authorization: skype_token <skypetoken>` | — |
| User profile service | GTM `userProfileService` | Entra | `https://api.spaces.skype.com` |
| Other regional services (default binding) | their GTM | Entra | the GTM base URL (origin) |
| IC3 gateway in "token migration" mode (chat, trouter, calling) | — | Entra with `X-MS-Migration: True`; falls back to the skypetoken when the service says so | `https://ic3.teams.office.com` (CAE) or `https://ic3-non-cae.teams.office.com` |
| Microsoft Graph | `graphEndpointBaseUrl` | Entra | `https://graph.microsoft.com` |
| Outlook / EWS / mail and calendar | `ewsResource` | Entra | `https://outlook.office.com` |
| Substrate search | `substrateBaseUrl` | Entra | `https://outlook.office.com/search` (`substrateTokenUrl`); some calls use the scope `https://substrate.office.com/M365.Access` |
| Substrate (other) | `substrateBaseUrl` | Entra | `https://substrate.office.com` |
| SharePoint / OneDrive files | per site | Entra | the SharePoint host |
| Office roaming settings | — | Entra | `https://api.office.net` |
| Meeting artifacts, attendance, templates | `teams.microsoft.com/api/…` | Entra | app ID `6bc3b958-689b-49f5-9006-36d165f30e00` |
| App Studio / developer portal | — | Entra | `https://dev.teams.microsoft.com` |

CAE: prod lists `api.spaces.skype.com`, `chatsvcagg`, `outlook.office.com`, `ic3.teams.office.com`, SharePoint and
presence as CAE-capable audiences. A CAE client advertises the capability when it asks Entra for a token (the
`xms_cc` claim, `cp1`) and must then handle claims challenges. We don't advertise it in M0 (the CA probe didn't
either), so our tokens are ordinary ones.

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

  For us this means both forms exist on the middle tier at once. Which form a given endpoint gets depends on the
  source tag of the calling feature, which isn't visible on the wire; the capture shows which requests come without
  `x-skypetoken`, and whether a listed endpoint still accepts a request that carries both also needs a capture.

For M0/M1 the skypetoken path stays the default for chat and the unlisted middle-tier calls; the rest of STM matters
later if enforcement is turned on.

## Open questions

1. ~~Does one refresh token really yield every audience in the table for the lab accounts, or does Conditional Access
   block some of them (Outlook, Graph, SharePoint) for a non-compliant Linux device?~~ **Answered (Task 14,
   `docs/spikes/auth.md`): yes.** One refresh grant each gave a token for `chatsvcagg`, presence, `ic3`, Graph,
   Outlook, Outlook search and substrate, for all 3 lab accounts, with no Conditional Access block. SharePoint wasn't
   asked: it needs a site host.
2. ~~What is the real `tokens.expiresIn` of a skypetoken (the client defaults to 24 hours only in the authzcore
   path)?~~ **Answered (2026-10-20, live):** the skypetoken carries `exp`, equal to the exchanged Entra token's `exp`;
   `expiresIn` is the same lifetime in seconds, about 2–2.5 h for a fresh Entra token (see [Response](#response)).
3. Which runtime setting makes macOS use `nativeclient` instead of `msauth.com.microsoft.teams2://auth`? It doesn't
   matter while `nativeclient` works for us.
4. Does presence accept a token whose audience is the presence origin, or does it want `api.spaces.skype.com`? The
   client passes the GTM base URL as the resource; the resulting audience needs a capture.
5. Is the `RegistrationToken` (from the chat service's `Set-RegistrationToken` response header after registering an
   endpoint) needed for reads, or only for the event channel? Belongs to the chat notes, not here.
6. Will the user's own account (the product, not the lab) face stricter Conditional Access (MFA, device compliance)
   than the bot accounts? Not testable in M0 without that account.
7. Trouter's credential under STM (skypetoken vs IC3 Entra token): needs a capture.

## To confirm by capture

The lab accounts are licensed now, so the `load` capture of account A (Task 11) can be taken. It should show:

- [ ] the authz POST the web client makes: URL (authz, not authzcore), the headers above, an empty body, status 200.
      Our own POST to authz with an empty body and without the `x-ms-client-*` headers gets 200 (Live, under
      [Response](#response)); which endpoint and headers the web client itself uses still needs the capture;
- [x] the authz response: `tokens.skypeToken`, `tokens.expiresIn`, `region`, `partition`, the `regionGtms` names and
      their hosts, `licenseDetails`. Confirmed by our own exchange for all 3 accounts (Live, under
      [Response](#response) and [Region GTMs](#region-gtms));
- [x] that `api.spaces.skype.com` is the authz audience: authz turns a token for it into a skypetoken (our own
      exchange, all 3 accounts);
- [ ] the chat service host the client really uses (`chatServiceAfd` vs `chatService`) and whether a 404 `Location`
      region hop happens;
- [ ] which middle-tier requests carry both `Authorization: Bearer` and `x-skypetoken`, and which carry
      `X-MS-Migration: True` with no `x-skypetoken` (the STM-listed sources);
- [ ] the audiences of the Bearer tokens the page sends to presence, CSA, Graph, Outlook and substrate (log `aud` and
      `scp` only);
- [ ] whether any request carries `X-MS-Migration: True` with an IC3 Entra token, and which;
- [ ] the shape of a 401 claims challenge, if one happens;
- [x] the body of the 403 the lab accounts get today: the `errorCode` is `UserLicenseNotPresentTrialEligible` for all 3,
      seen by `proto-lab auth`'s own exchange (Task 14, `docs/spikes/auth.md`). Its JSON layout beyond `errorCode` is
      still unrecorded.

Then replace the synthetic fixtures below with sanitized ones exported from that capture.

## Fixtures

`tests/fixtures/auth/*.json` are **synthetic**: every value is made up, every secret or identifier is an
angle-bracket placeholder, and every regional host is under `example.test`. Each file has a `_synthetic` note and
holds one exchange as `{request, response}`; `response` has `status`, `headers` and `body`. The shapes come from the
bundle (authz) and from Entra's documented v2 token endpoint (the rest). They exist so that Tasks 13 and 14 can test
against wiremock now.

Most placeholders have the sanitizer's `<kind:id>` shape (`<token:N>`, `<guid:N>`, `<mri:a>`). A few are written by
hand with kinds the sanitizer convention doesn't define: `<code:1>` and `<verifier:1>` (the authorization code and
PKCE verifier), `<token:skype>` (the skypetoken), `<skypeid:a>`, and `<client-version>`, which has no id at all. The
skypetoken placeholder isn't a JWT, so a test of the `exp` expiry rule has to build an unsigned JWT at run time (the
fixture guard rejects committed ones). Tests should treat every placeholder as an opaque string, never match on its
text: sanitized captures exported by `proto-lab export` will replace these files and may name or number the
placeholders differently.

| File | Exchange |
|---|---|
| `entra-token-code.json` | code → tokens for `api.spaces.skype.com` |
| `entra-token-refresh.json` | refresh grant for a second audience (Graph) with the same refresh token |
| `entra-error-invalid-grant.json` | expired refresh token (`invalid_grant`, `AADSTS700082`) |
| `entra-error-aadsts50011.json` | unregistered redirect URI |
| `entra-error-aadsts53003.json` | Conditional Access block |
| `entra-error-429.json` | throttled, with `Retry-After` |
| `authz-ok.json` | authz with a skypetoken and region GTMs |
| `authzcore-ok.json` | authzcore: MRI and GTMs, no skypetoken |
| `authz-403-no-license.json` | authz refusing an unlicensed user |
