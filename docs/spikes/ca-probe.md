# Conditional Access probe: Teams tokens for a third-party client

Date: 2026-10-19. Tool: `tools/ca-probe/ca-probe.mjs`. Accounts: lab a, b, c (company tenant).

## Question
Will the company tenant issue tokens for the official native Teams client ID to a client that isn't Teams, on Linux, without a managed device?

## What we used (found in the Windows MSIX)
- Native Teams client ID: `1fec8e78-bce4-4aaf-ab1b-5451cc387264`. The web bundle hardcodes it as the `appid` claim of Teams tokens.
- Redirect URI: `https://login.microsoftonline.com/common/oauth2/nativeclient`. It's a string in `ms-teams.exe`, used by OneAuth. The shell also knows the WAM broker (`msauth://Microsoft.AAD.BrokerPlugin`) and nested app auth (`child_client_id`, `brk_redirect_uri`).
- The web client uses a different ID, `5e3ce6c0-2b1f-4285-8d4b-75ee78787346`. It's a SPA registration whose refresh tokens are short-lived, so we didn't use it.
- Skypetoken resource: `authzResource = https://api.spaces.skype.com`. The exchange goes through `https://teams.microsoft.com/api/authsvc/v1.0/authz`, and a newer `authzcore` variant exists.

## Flow
1. PKCE authorize at `login.microsoftonline.com/organizations/oauth2/v2.0/authorize`. It runs in the lab Chromium profile, which already holds the SSO session cookie, with `login_hint`. The scope is `https://api.spaces.skype.com/.default openid profile offline_access`.
2. The code is caught on navigation to the `nativeclient` redirect. That page lives on Microsoft's login host, so the browser never touches a Teams host.
3. Node redeems the code at `/token` as a public client, then does one `refresh_token` grant.

## Result: passed for all 3 accounts, no interaction
| | a | b | c |
|---|---|---|---|
| access token `aud` | `https://api.spaces.skype.com` | same | same |
| `appid` | `1fec8e78-…` | same | same |
| `scp` | `Authorization.ReadWrite Region.ReadWrite user_impersonation` | same | same |
| `amr` / `acr` | `pwd` / `1` | same | same |
| device ID in token | none | none | none |
| access token lifetime | ~2.2 h | ~2.2 h | ~2.3 h |
| refresh token | yes, no `refresh_token_expires_in` (not SPA-limited) | yes | yes |
| refresh grant | ok | ok | ok |

No consent prompt, no MFA, no device-compliance block. Conditional Access doesn't stop a non-Microsoft client from getting Teams tokens for these accounts. The browser blocked no Teams/Skype requests during the probe, because none were made.

## Not covered yet
- The skypetoken exchange (`authz`). It's a request to a Teams host, so it waits for the fence (M0 Tasks 6–7).
- Tokens for other audiences (chatsvc, presence, Graph, Outlook) through the same refresh token.
- The **user's own** account. It may be under stricter Conditional Access policies than the bot accounts (MFA, device compliance).
- CAE (`xms_cc` claim). The probe didn't request CAE capability, so the claim isn't in the token.
