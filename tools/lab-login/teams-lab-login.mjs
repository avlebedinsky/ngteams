// Signs the 3 lab accounts into Microsoft in persistent profiles, without ever opening Teams.
// Interim tool until `proto-lab login` (M0 Task 8); reuses lab-automation's Playwright and account loader.
// Usage: node teams-lab-login.mjs <a|b|c|all>
// Codes arrive via files <lab>/inbox/<alias>.code (or .choice), written by the operator.
import { mkdirSync, writeFileSync, readFileSync, existsSync, unlinkSync, chmodSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

const AUTOMATION = "/path/to/lab-automation";
const { loadAccounts } = await import(`${AUTOMATION}/scripts/claude-setup-token.mjs`);
const { chromium } = await import(`${AUTOMATION}/node_modules/playwright/index.mjs`);

const LAB = join(homedir(), ".local/share/teams-lab");
const INBOX = join(LAB, "inbox");
const DIAG = join(LAB, "diagnostics");
for (const dir of [LAB, INBOX, DIAG, join(LAB, "profiles")]) {
  mkdirSync(dir, { recursive: true, mode: 0o700 });
  chmodSync(dir, 0o700);
}

const ALIASES = { labbot: "a", labbot2: "b", labbot3: "c" };
const START_URL = "https://myaccount.microsoft.com/";
const NAV_HOSTS = [
  /^login\.microsoftonline\.com$/, /^login\.microsoft\.com$/, /^login\.live\.com$/,
  /^device\.login\.microsoftonline\.com$/, /^autologon\.microsoftazuread-sso\.com$/,
  /^myaccount\.microsoft\.com$/, /^mysignins\.microsoft\.com$/, /^account\.activedirectory\.windowsazure\.com$/,
];
// Mini-fence: nothing may reach Teams/Skype services from this browser.
const BLOCKED_HOSTS = /(^|\.)(teams\.microsoft\.com|teams\.cloud\.microsoft|skype\.com|skype\.net|trouter\.[a-z.]+|teams\.live\.com)$/;

const status = (alias, state, extra = {}) => {
  const record = { alias, state, at: new Date().toISOString(), ...extra };
  writeFileSync(join(LAB, `status-${alias}.json`), JSON.stringify(record, null, 2), { mode: 0o600 });
  console.log(`STATUS ${JSON.stringify(record)}`);
};

const shot = async (page, alias, name) => {
  const path = join(DIAG, `${alias}-${Date.now()}-${name}.png`);
  await page.screenshot({ path }).catch(() => {});
  return path;
};

async function visibleField(page, selectors) {
  for (const selector of selectors) {
    const locator = page.locator(selector).first();
    if (!await locator.isVisible().catch(() => false)) continue;
    const ok = await locator.evaluate((el) => {
      const r = el.getBoundingClientRect();
      return Number(getComputedStyle(el).opacity) > 0 && r.width >= 100 && r.height >= 20;
    }).catch(() => false);
    if (ok) return locator;
  }
  return null;
}

async function clickFirst(page, selectors) {
  for (const selector of selectors) {
    const locator = page.locator(selector).first();
    if (await locator.isVisible().catch(() => false)) {
      return locator.click({ timeout: 5_000 }).then(() => true).catch(() => false);
    }
  }
  return false;
}

async function typeInto(page, locator, value) {
  await locator.click({ timeout: 5_000 }).catch(() => {});
  await page.keyboard.press("Control+a").catch(() => {});
  await locator.pressSequentially(value, { delay: 40 + Math.floor(Math.random() * 80), timeout: 60_000 });
}

const pause = (page) => page.waitForTimeout(400 + Math.floor(Math.random() * 1_500));

function takeInbox(alias, kind) {
  const path = join(INBOX, `${alias}.${kind}`);
  if (!existsSync(path)) return null;
  const value = readFileSync(path, "utf8").trim();
  unlinkSync(path);
  return value;
}

async function login(account) {
  const alias = ALIASES[account.profile];
  const profileDir = join(LAB, "profiles", alias);
  mkdirSync(profileDir, { recursive: true, mode: 0o700 });
  const context = await chromium.launchPersistentContext(profileDir, {
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
  let passwordSent = false;
  let emailSent = false;
  let lastMfaKey = "";
  const deadline = Date.now() + 15 * 60_000;
  try {
    status(alias, "starting", { email: account.email });
    await page.goto(START_URL, { waitUntil: "domcontentloaded", timeout: 45_000 });
    while (Date.now() < deadline) {
      await page.waitForTimeout(700);
      const url = new URL(page.url());
      if (!NAV_HOSTS.some((re) => re.test(url.hostname))) {
        const path = await shot(page, alias, "unexpected-host");
        throw new Error(`stopped on unexpected host ${url.hostname} (screenshot ${path})`);
      }
      if (url.hostname === "myaccount.microsoft.com" || url.hostname === "mysignins.microsoft.com") {
        // The SPA lands here first and then bounces to login; success needs the STS session cookie,
        // no login form, and the page still on this host after it settles.
        await page.waitForLoadState("networkidle", { timeout: 20_000 }).catch(() => {});
        await page.waitForTimeout(3_000);
        const settled = new URL(page.url()).hostname;
        const cookieNames = (await context.cookies()).map((c) => `${c.domain}:${c.name}`)
          .filter((n) => /login\.microsoftonline\.com/.test(n)).sort();
        const session = cookieNames.some((n) => /:ESTSAUTH(PERSISTENT)?$/.test(n));
        const loginForm = await visibleField(page, ['input[name="loginfmt"]', 'input[name="passwd"]']);
        if (session && !loginForm && (settled === "myaccount.microsoft.com" || settled === "mysignins.microsoft.com")) {
          status(alias, "done", { cookieNames, blockedRequests: blocked, screenshot: await shot(page, alias, "done") });
          return;
        }
        continue;
      }
      const text = await page.locator("body").innerText().catch(() => "");

      const password = await visibleField(page, ['input[name="passwd"]', 'input[type="password"]']);
      if (password && passwordSent) {
        if (await page.locator("#passwordError").isVisible().catch(() => false)
          || /password is incorrect|account has been locked/i.test(text)) {
          throw new Error(`password rejected (screenshot ${await shot(page, alias, "password-rejected")})`);
        }
        continue;
      }
      if (password) {
        await pause(page);
        await typeInto(page, password, account.password);
        await clickFirst(page, ['#idSIButton9', 'input[type="submit"]', 'button[type="submit"]']);
        passwordSent = true;
        status(alias, "password-submitted");
        continue;
      }
      const email = await visibleField(page, ['input[name="loginfmt"]', 'input[type="email"]']);
      if (email && !emailSent) {
        await pause(page);
        await typeInto(page, email, account.email);
        await clickFirst(page, ['#idSIButton9', 'input[type="submit"]', 'button[type="submit"]']);
        emailSent = true;
        status(alias, "email-submitted");
        continue;
      }
      if (await clickFirst(page, ['#otherTile', '[data-test-id="otherTile"]'])) continue;

      // MFA registration / security info changes would be writes to the account: stop.
      if (/more information required|keep your account secure|let's keep your account secure|set up your account/i.test(text)) {
        throw new Error(`MFA registration requested; not proceeding (screenshot ${await shot(page, alias, "registration")})`);
      }

      const otc = await visibleField(page, ['input[name="otc"]', '#idTxtBx_SAOTCC_OTC', 'input[autocomplete="one-time-code"]', 'input[type="tel"]']);
      if (otc) {
        const key = `otc:${page.url()}`;
        if (key !== lastMfaKey) {
          lastMfaKey = key;
          const prompt = text.split("\n").map((s) => s.trim()).filter(Boolean).slice(0, 8).join(" | ");
          status(alias, "need-code", { prompt, screenshot: await shot(page, alias, "need-code") });
        }
        const code = takeInbox(alias, "code");
        if (code) {
          await typeInto(page, otc, code);
          await clickFirst(page, ['#idSubmit_SAOTCC_Continue', '#idSIButton9', 'input[type="submit"]', 'button[type="submit"]']);
          status(alias, "code-submitted");
          lastMfaKey = "";
          await page.waitForTimeout(2_000);
          if (/incorrect|didn't enter the expected|wrong code|try again/i.test(await page.locator("body").innerText().catch(() => ""))) {
            status(alias, "code-rejected", { screenshot: await shot(page, alias, "code-rejected") });
          }
        }
        continue;
      }

      const proofs = page.locator('[data-value][role="button"], div.table[role="button"], #idDiv_SAOTCS_Proofs [role="button"]');
      if (await proofs.first().isVisible().catch(() => false)) {
        const options = (await proofs.allInnerTexts()).map((s) => s.replace(/\s+/g, " ").trim()).filter(Boolean);
        const key = `proofs:${options.join("/")}`;
        if (key !== lastMfaKey) {
          lastMfaKey = key;
          status(alias, "need-choice", { options, screenshot: await shot(page, alias, "need-choice") });
        }
        const choice = takeInbox(alias, "choice");
        if (choice !== null) {
          await proofs.nth(Number(choice)).click({ timeout: 5_000 }).catch(() => {});
          status(alias, "choice-submitted", { choice: options[Number(choice)] });
          lastMfaKey = "";
        }
        continue;
      }

      if (/approve sign.in|open your authenticator|enter the number/i.test(text)) {
        const key = `approve:${text.slice(0, 200)}`;
        if (key !== lastMfaKey) {
          lastMfaKey = key;
          const number = /\b(\d{2,3})\b/.exec(await page.locator("#idRichContext_DisplaySign, .displaySign").first().innerText().catch(() => ""))?.[1];
          status(alias, "need-approval", { number: number ?? null, screenshot: await shot(page, alias, "need-approval") });
        }
        continue;
      }

      if (passwordSent && await page.locator('#KmsiCheckboxField, input[name="DontShowAgain"]').first().isVisible().catch(() => false)) {
        await page.locator('#KmsiCheckboxField, input[name="DontShowAgain"]').first().check({ timeout: 5_000 }).catch(() => {});
        await clickFirst(page, ['#idSIButton9', 'button:has-text("Yes")', 'input[value="Yes"]']);
        status(alias, "kmsi-accepted");
        continue;
      }
      if (passwordSent && await clickFirst(page, ['#idSIButton9', 'button:has-text("Yes")'])) continue;
    }
    throw new Error(`timed out (screenshot ${await shot(page, alias, "timeout")})`);
  } catch (error) {
    status(alias, "failed", { error: error.message, blockedRequests: blocked });
    throw error;
  } finally {
    await context.close();
  }
}

const wanted = process.argv[2] ?? "all";
const accounts = loadAccounts(join(AUTOMATION, "lab-accounts.json"))
  .filter((a) => ALIASES[a.profile] && (wanted === "all" || ALIASES[a.profile] === wanted));
if (accounts.length === 0) throw new Error(`no account for ${wanted}`);
for (const account of accounts) {
  try {
    await login(account);
  } catch (error) {
    console.error(`${ALIASES[account.profile]}: ${error.message}`);
    process.exitCode = 1;
  }
}
