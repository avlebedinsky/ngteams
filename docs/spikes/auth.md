# Spike 1: native sign-in through the lab profile

Date: 2026-10-20. M0 Task 14. Tool: `proto-lab auth <a|b|c>` (Chrome for Testing 155 under the armed fence, Rust for
everything after the redirect). Accounts: lab a, b, c in the company tenant.

## Question

Can our own Rust code sign a lab account in to Teams with no human: authorize in the account's browser profile, take
the code, redeem and refresh it, and exchange the result for a skypetoken, all with the fence armed?

## How it runs

1. **Authorize** (browser). `teams-auth` builds the request: the native Teams client
   (`1fec8e78-bce4-4aaf-ab1b-5451cc387264`), the `nativeclient` redirect, PKCE S256, a random `state`, `login_hint`,
   scope `https://api.spaces.skype.com/.default openid profile offline_access`. `proto-lab` opens it in the account's
   persistent profile, headless, under the armed fence, fail-closed with the seeded own-state list (no sign-in form
   is allowed through: if Entra wanted a password, the run would stop, not type one).
2. **Catch the redirect** (the fence). Entra answers the signed-in profile with a 302 to the redirect URI. The fence
   pauses the login origin's responses as well as every request, and catches the 302 whose `Location` is the
   registered redirect URI. It hands the URL to the run and fails the response, so the browser never follows it. A
   `GET` to the redirect URI made by script is caught at the request stage the same way. Nothing else changes: every
   other paused response is continued, and every request is still decided as before. See "The redirect catch" below.
3. **Code → tokens** (Rust). The code is accepted only if the URL is the redirect URI and carries the request's
   `state`. It is redeemed at the v2 token endpoint. The refresh token is stored only after the ID token names the
   account and its object id from `lab.toml`.
4. **Refresh**: one refresh grant for `api.spaces.skype.com` from the stored refresh token, and, with `--audiences`,
   one per other audience in `docs/protocol/auth.md`.
5. **Skypetoken**: `POST https://teams.microsoft.com/api/authsvc/v1.0/authz` with the fresh `api.spaces.skype.com`
   token and no body. On success the skypetoken goes into the Secret Service next to the refresh token, and the region
   (`region`, `partition`, `regionGtms`, the skypetoken's expiry) goes to `~/.local/share/teams-lab/auth/<alias>.json`
   (mode 0600).

The CLI keeps secrets in the Secret Service under the lab's application attribute `teams-lab`. It prints only each
token's `aud`, `exp`, scopes and tenant, plus the skypetoken's expiry and region, or the refusal.

## Result

### Entra: passed for all 3 accounts, no interaction

| | a | b | c |
|---|---|---|---|
| authorize | redirected to `nativeclient` (3xx), caught | same | same |
| sign-in page shown by Entra | none | none | none |
| fence during the browser step | 0 denied, nothing breached | same | same |
| code → token, `aud` | `https://api.spaces.skype.com` | same | same |
| scopes | `Authorization.ReadWrite Region.ReadWrite user_impersonation` | same | same |
| access token lifetime | ~2–2.5 h | same | same |
| refresh token stored, then refreshed | yes | yes | yes |

The whole sign-in, browser and token requests included, takes 2–3 s per account. Entra showed no sign-in or
consent page: the profile's session was enough. The browser sent no mutating request, so the fence denied nothing and
wrote nothing to `fence.log`.

**What Conditional Access did: nothing.** It didn't ask for MFA, didn't check the device and didn't block any
audience. One refresh token yields a token for every audience in the auth notes' table (open question 1 there). These
are the answers to one refresh grant each, the same for a, b and c:

| Resource asked | `aud` of the token | Scopes (abridged) |
|---|---|---|
| `https://api.spaces.skype.com` | the same | `Authorization.ReadWrite Region.ReadWrite user_impersonation` |
| `https://chatsvcagg.teams.microsoft.com` | the same | `user_impersonation` |
| `https://presence.teams.microsoft.com` | the same | `PresenceRW` |
| `https://ic3.teams.office.com` | the same | `Teams.AccessAsUser.All` |
| `https://graph.microsoft.com` | the same | about 40 delegated scopes, among them `ChatMessage.Send`, `ChannelMessage.Send`, `Mail.Send`, `Files.ReadWrite.All` |
| `https://outlook.office.com` | the same | `EWS.AccessAsUser.All`, `Mail.ReadWrite`, `Mail.Send`, `Calendars.ReadWrite`, … |
| `https://outlook.office.com/search` | `66a88757-258c-4c72-893c-3e8bed4d6899` (an app ID, not the URL) | `SubstrateSearch-Internal.ReadWrite` |
| `https://substrate.office.com` | the same | `Files.ReadWrite`, `SubstrateSearch-Internal.ReadWrite`, `Tasks.ReadWrite`, … |

The Graph and Outlook tokens can send messages and mail, so they are as sensitive as the skypetoken. In M0 the lab only
reads their claims. Any use of them is a write path that has to go through the fence like everything else.

### Skypetoken: refused for all 3 accounts (blocked)

authz answered **403 with `errorCode` `UserLicenseNotPresentTrialEligible`** for a, b and c, in each CLI run and in
the live test. This is the same refusal `proto-lab login` saw from the web client in Task 8. Now the error code is
known too: the accounts have no Teams license and could start the Exploratory trial. The exchange itself works:
request, headers, parsing of the answer, the expiry rule and storing are tested on `wiremock`. Nothing was stored, and
no region file was written.

We never call `…/authz/signuptrial`. `AuthzClient` refuses any endpoint whose last path segment isn't `authz`, and the
fence's own-state rule matches `authz` as a whole path.

## The redirect catch

The catch only adds denials:

- A fence given a catch (`proto-lab auth` only) asks `Fetch` for one extra pattern on the browser session: the
  responses of the authorize endpoint's origin (`https://login.microsoftonline.com/*`) at the response stage. Pages and
  workers keep their request-stage `*`.
- A paused response is either the redirect, which is caught and failed, or anything else, which is continued. Its
  request already passed the request stage, and continuing a response sends nothing.
- A request-stage `GET` to the redirect URI is caught and failed before the fence decides it. Any other method to that
  URL goes through the fence's normal decision.
- A fence without a catch pauses no response. A response paused anyway is failed and fails the run.
- A caught redirect isn't a deny and doesn't fail the run. The URL (it holds the code) is handed over as a `Secret`
  and appears in no log.

Tested on a local stand-in for Entra (`crates/proto-lab/tests/auth_redirect.rs`):

- a 302 to an `https` redirect page is caught at the response stage, before the browser asks for the page;
- a 302 to the custom scheme `msauth.com.microsoft.teams2://auth` is caught too (without the response-stage pattern
  that test fails: Chrome won't navigate to the scheme);
- a script navigation to the redirect is caught at the request stage;
- an unrelated redirect of the same origin goes through;
- without a catch, the same authorize request lands on the redirect page.

## How to run it

```sh
cargo build -p proto-lab
target/debug/proto-lab auth a                  # Entra, refresh, skypetoken
target/debug/proto-lab auth a --audiences      # plus one token per other audience
target/debug/proto-lab auth a --no-skypetoken  # Entra only
PROTO_LAB_LIVE=1 cargo test -p proto-lab --features live --test auth_live                     # a, b, c: Entra + refresh
PROTO_LAB_LIVE=1 cargo test -p proto-lab --features live --test auth_live -- --include-ignored  # also the skypetoken
```

Without `PROTO_LAB_LIVE=1` the live tests skip, so `cargo test --all-features` stays offline. The skypetoken test is
`#[ignore]` until the license blocker is gone.

## What remains

- **The skypetoken, live.** Blocked on a Teams license or the Exploratory trial for the lab accounts
  (`docs/backlog/lab-accounts-have-no-teams-license.md`). Once they have Teams, run the ignored live test. It should
  pass as it is, then:
  - record the real `tokens.expiresIn` and whether the skypetoken carries `exp` (auth notes' open question 2);
  - confirm the region and the `regionGtms` names, and replace `tests/fixtures/auth/authz-ok.json` with a sanitized
    export.
- **Headers.** We send no `x-ms-client-type` or `x-ms-client-version`. authz didn't object before refusing the license,
  but whether a licensed account's exchange needs them is unknown until then.
- **Renewal.** `AuthzKind::TokenRefresh` exists, but nothing schedules renewal yet. That belongs to the M1 client.
- **The user's own account** may face stricter Conditional Access than the bot accounts (auth notes' open question
  6). Not testable in M0.
- **CAE.** We don't advertise CAE (`xms_cc`), so our tokens are ordinary ones, and claims challenges aren't handled.
