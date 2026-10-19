// Conditional Access probe: can a lab account get Teams tokens for the native Teams client ID?
// Runs PKCE authorize in the lab profile (SSO cookies, no password), catches the code at the
// nativeclient redirect, redeems it and tries one refresh. Prints claims only, never tokens.
// Teams/Skype hosts are blocked in the browser; nothing is sent to Teams services.
// Usage: node ca-probe.mjs <a|b|c> <login-hint-upn>
import { createHash, randomBytes } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

const AUTOMATION = "/path/to/lab-automation";
const { chromium } = await import(`${AUTOMATION}/node_modules/playwright/index.mjs`);

const [alias, loginHint] = process.argv.slice(2);
if (!/^[abc]$/.test(alias ?? "") || !/@/.test(loginHint ?? "")) {
  throw new Error("usage: ca-probe.mjs <a|b|c> <login-hint-upn>");
}

const LAB = join(homedir(), ".local/share/teams-lab");
const DIAG = join(LAB, "diagnostics");
mkdirSync(DIAG, { recursive: true, mode: 0o700 });

const CLIENT_ID = "1fec8e78-bce4-4aaf-ab1b-5451cc387264";
const REDIRECT = "https://login.microsoftonline.com/common/oauth2/nativeclient";
const AUTHORITY = "https://login.microsoftonline.com/organizations/oauth2/v2.0";
const SCOPE = "https://api.spaces.skype.com/.default openid profile offline_access";
const BLOCKED_HOSTS = /(^|\.)(teams\.microsoft\.com|teams\.cloud\.microsoft|skype\.com|skype\.net|trouter\.[a-z.]+|teams\.live\.com)$/;

const b64url = (buf) => buf.toString("base64").replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const verifier = b64url(randomBytes(48));
const challenge = b64url(createHash("sha256").update(verifier).digest());
const state = b64url(randomBytes(16));

const claims = (jwt) => {
  try {
    return JSON.parse(Buffer.from(jwt.split(".")[1], "base64url").toString("utf8"));
  } catch {
    return null;
  }
};
const pick = (c) => c && {
  aud: c.aud, appid: c.appid ?? c.azp, scp: c.scp, tid: c.tid, amr: c.amr, acr: c.acr,
  deviceid: c.deviceid ?? null, xms_cc: c.xms_cc ?? null, ttl_minutes: Math.round((c.exp - c.iat) / 60),
};
const report = (result) => {
  const path = join(LAB, `ca-probe-${alias}.json`);
  writeFileSync(path, JSON.stringify(result, null, 2), { mode: 0o600 });
  console.log(`RESULT ${JSON.stringify(result)}`);
};

const authorizeUrl = `${AUTHORITY}/authorize?${new URLSearchParams({
  client_id: CLIENT_ID,
  response_type: "code",
  redirect_uri: REDIRECT,
  scope: SCOPE,
  code_challenge: challenge,
  code_challenge_method: "S256",
  state,
  login_hint: loginHint,
})}`;

const context = await chromium.launchPersistentContext(join(LAB, "profiles", alias), {
  headless: true,
  locale: "en-US",
  viewport: { width: 1280, height: 900 },
  ignoreDefaultArgs: ["--enable-automation"],
  args: ["--disable-blink-features=AutomationControlled"],
});
const blocked = [];
await context.route("**/*", (route) => {
  const host = new URL(route.request().url()).hostname;
  if (BLOCKED_HOSTS.test(host)) {
    blocked.push(`${route.request().method()} ${host}`);
    return route.abort("blockedbyclient");
  }
  return route.continue();
});
const page = context.pages()[0] ?? await context.newPage();
let redirect = null;
page.on("framenavigated", (frame) => {
  if (frame === page.mainFrame() && frame.url().startsWith(REDIRECT)) redirect = new URL(frame.url());
});

try {
  await page.goto(authorizeUrl, { waitUntil: "domcontentloaded", timeout: 45_000 });
  const deadline = Date.now() + 60_000;
  while (!redirect && Date.now() < deadline) {
    await page.waitForTimeout(500);
    const host = new URL(page.url()).hostname;
    if (host !== "login.microsoftonline.com" && host !== "login.microsoft.com") {
      throw new Error(`left the login host: ${host}`);
    }
  }
  if (!redirect) {
    const text = (await page.locator("body").innerText().catch(() => "")).replace(/\s+/g, " ").slice(0, 600);
    const screenshot = join(DIAG, `${alias}-${Date.now()}-ca-probe-interaction.png`);
    await page.screenshot({ path: screenshot }).catch(() => {});
    report({ alias, outcome: "interaction-required", aadsts: /AADSTS\d+/.exec(text)?.[0] ?? null, pageText: text, screenshot, blocked });
  } else if (redirect.searchParams.get("error")) {
    report({
      alias, outcome: "authorize-error",
      error: redirect.searchParams.get("error"),
      description: (redirect.searchParams.get("error_description") ?? "").slice(0, 400),
      blocked,
    });
  } else {
    if (redirect.searchParams.get("state") !== state) throw new Error("state mismatch");
    const token = async (body) => {
      const response = await fetch(`${AUTHORITY}/token`, {
        method: "POST",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({ client_id: CLIENT_ID, ...body }),
      });
      return { status: response.status, json: await response.json() };
    };
    const first = await token({
      grant_type: "authorization_code",
      code: redirect.searchParams.get("code"),
      redirect_uri: REDIRECT,
      code_verifier: verifier,
      scope: SCOPE,
    });
    const result = { alias, outcome: first.status === 200 ? "token" : "token-error", blocked };
    if (first.status === 200) {
      result.access = pick(claims(first.json.access_token));
      result.hasRefreshToken = Boolean(first.json.refresh_token);
      result.refreshTokenExpiresIn = first.json.refresh_token_expires_in ?? null; // only set for SPA-style RTs
      result.idTokenUpn = claims(first.json.id_token ?? "")?.preferred_username ?? null;
      if (first.json.refresh_token) {
        const refreshed = await token({ grant_type: "refresh_token", refresh_token: first.json.refresh_token, scope: SCOPE });
        result.refresh = refreshed.status === 200
          ? { ok: true, aud: claims(refreshed.json.access_token)?.aud }
          : { ok: false, error: refreshed.json.error, codes: refreshed.json.error_codes };
      }
    } else {
      result.error = first.json.error;
      result.codes = first.json.error_codes;
      result.description = String(first.json.error_description ?? "").split("\r\n")[0].slice(0, 400);
    }
    report(result);
  }
} finally {
  await context.close();
}
