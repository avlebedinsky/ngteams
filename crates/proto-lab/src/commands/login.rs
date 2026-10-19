//! `proto-lab login <a|b|c>`: signs a lab account into the Teams web client in its persistent profile, headless, under
//! the armed fence (fail-closed), and records the account's MRI in `lab.toml`.
//!
//! The browser opens Teams. A profile that is still signed in to Microsoft is passed straight through, and the web
//! client loads. If Microsoft shows its sign-in page instead, the sign-in is automatic, following the proven
//! `tools/lab-login/teams-lab-login.mjs`:
//! - the email and the password come from lab-automation's account file ([`crate::accounts`]) and are typed only into
//!   a field a person could type into (shown, sized, enabled);
//! - the password is sent once; a rejection stops the run;
//! - "Stay signed in?" is answered yes;
//! - a request to register an MFA method stops the run, and so does a prompt to pick a method or approve in an app;
//! - a request for a one-time code writes `status-<alias>.json` with `need-code` and waits for the code in
//!   `inbox/<alias>.code` (in the lab's data dir), up to `--code-wait`.
//!
//! The fence lets Entra's sign-in form through for this command only ([`Classifier::sign_in`]); everything else
//! mutating is denied as in every run, and the denies are expected (the web client loads in a reduced state).
//!
//! Once the web client has loaded, the account's MRI is made from the object id in the claims of a bearer token the
//! page sent for that account's UPN, and recorded in `lab.toml` ([`record_account_mri`]). An account that is signed
//! in but that Teams refuses (no Teams license; the web client then offers a trial, which is a tenant-visible change and
//! is never started here) gets its MRI recorded too, and the report says Teams refused it. Each state goes to
//! `status-<alias>.json` (mode `0600`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::accounts::{ACCOUNTS_FILE, AccountsError, AccountsFile, Credentials};
use crate::cdp::{Cdp, CdpError, SessionId};
use crate::classify::Classifier;
use crate::config::{Alias, ConfigError, LabConfig, LabLocations, MriRecord, record_account_mri};
use crate::session::{FenceReport, LabSession, NotAGuid, REFUSAL_GRACE, SessionError, SessionState, TEAMS_URL};

/// What a sign-in page shows, as [`probe`] finds it. A field is the selector of the one element of its kind a person
/// could type into (the probe marks it), or `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignInProbe {
    pub email: Option<String>,
    pub password: Option<String>,
    /// A one-time code.
    pub otc: Option<String>,
    /// "Use another account".
    pub other_tile: Option<String>,
    /// The "Stay signed in?" / "Don't show this again" checkbox.
    pub kmsi: Option<String>,
    pub signals: BTreeSet<Signal>,
    /// The first lines of the page's text, for the `need-code` status.
    pub prompt: String,
}

/// Something else a sign-in page shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Signal {
    /// A primary button (Next, Sign in, Yes).
    Next,
    /// The password error under the field.
    PasswordError,
    /// The page says the password is wrong or the account is locked.
    Rejected,
    /// The page asks to register an MFA method or more security info.
    Registration,
    /// The page asks to approve the sign-in in an app.
    Approval,
    /// The page asks to pick a verification method.
    Proofs,
}

impl Signal {
    const ALL: [(Self, &str); 6] = [
        (Self::Next, "next"),
        (Self::PasswordError, "passwordError"),
        (Self::Rejected, "rejected"),
        (Self::Registration, "registration"),
        (Self::Approval, "approval"),
        (Self::Proofs, "proofs"),
    ];
}

impl SignInProbe {
    /// Reads what the probe script returned; anything missing or of the wrong type counts as absent.
    #[must_use]
    pub fn from_value(value: &Value) -> Self {
        let field = |name: &str| {
            value[name]
                .as_str()
                .filter(|selector| !selector.is_empty())
                .map(str::to_owned)
        };
        Self {
            email: field("email"),
            password: field("password"),
            otc: field("otc"),
            other_tile: field("otherTile"),
            kmsi: field("kmsi"),
            signals: Signal::ALL
                .iter()
                .filter(|(_, name)| value[*name].as_bool() == Some(true))
                .map(|(signal, _)| *signal)
                .collect(),
            prompt: value["prompt"].as_str().unwrap_or_default().to_owned(),
        }
    }

    #[must_use]
    pub fn shows(&self, signal: Signal) -> bool {
        self.signals.contains(&signal)
    }
}

/// What has been sent so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub email_sent: bool,
    pub password_sent: bool,
}

/// Why a sign-in stops for good.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignInStop {
    #[error("the password was rejected")]
    PasswordRejected,
    #[error("Microsoft asks to register an MFA method; not proceeding")]
    MfaRegistration,
    #[error("Microsoft asks to pick an MFA method; not proceeding")]
    MfaChoice,
    #[error("Microsoft asks to approve the sign-in in an app; not proceeding")]
    MfaApproval,
}

/// The next step on a sign-in page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing to do yet.
    Wait,
    TypeEmail(String),
    TypePassword(String),
    ClickOtherTile(String),
    /// Tick the checkbox and press the primary button.
    AcceptKmsi(String),
    ClickNext,
    /// A one-time code goes into this field.
    NeedCode(String),
    Stop(SignInStop),
}

/// The next step for what the page shows, in the order the proven script takes them. The password goes in once; after
/// it, a password field still showing means waiting, or a stop if the page says it was wrong. Buttons are pressed
/// only once the password went in.
#[must_use]
pub fn next_action(probe: &SignInProbe, progress: Progress) -> Action {
    if let Some(password) = &probe.password {
        if !progress.password_sent {
            return Action::TypePassword(password.clone());
        }
        if probe.shows(Signal::PasswordError) || probe.shows(Signal::Rejected) {
            return Action::Stop(SignInStop::PasswordRejected);
        }
        return Action::Wait;
    }
    if let Some(email) = probe.email.as_ref().filter(|_| !progress.email_sent) {
        return Action::TypeEmail(email.clone());
    }
    mfa_or_confirm(probe, progress)
}

fn mfa_or_confirm(probe: &SignInProbe, progress: Progress) -> Action {
    if let Some(tile) = &probe.other_tile {
        return Action::ClickOtherTile(tile.clone());
    }
    if probe.shows(Signal::Registration) {
        return Action::Stop(SignInStop::MfaRegistration);
    }
    if let Some(otc) = &probe.otc {
        return Action::NeedCode(otc.clone());
    }
    if probe.shows(Signal::Proofs) {
        return Action::Stop(SignInStop::MfaChoice);
    }
    if probe.shows(Signal::Approval) {
        return Action::Stop(SignInStop::MfaApproval);
    }
    match (&probe.kmsi, progress.password_sent) {
        (Some(kmsi), true) => Action::AcceptKmsi(kmsi.clone()),
        (None, true) if probe.shows(Signal::Next) => Action::ClickNext,
        _ => Action::Wait,
    }
}

/// The probe: finds the one element of each kind a person could use, marks it with `data-proto-lab`, and reports the
/// marker selectors and the page's signals.
const PROBE: &str = r#"(() => {
  for (const old of document.querySelectorAll('[data-proto-lab]')) old.removeAttribute('data-proto-lab');
  const shown = (el) => {
    const style = getComputedStyle(el);
    const box = el.getBoundingClientRect();
    return style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity) > 0
      && box.width > 0 && box.height > 0;
  };
  const typable = (el) => shown(el) && !el.disabled && !el.readOnly
    && el.getBoundingClientRect().width >= 100 && el.getBoundingClientRect().height >= 20;
  const mark = (kind, selectors, usable) => {
    for (const selector of selectors) {
      for (const el of document.querySelectorAll(selector)) {
        if (usable(el)) { el.setAttribute('data-proto-lab', kind); return `[data-proto-lab="${kind}"]`; }
      }
    }
    return null;
  };
  const any = (selectors) => selectors.some((s) => [...document.querySelectorAll(s)].some(shown));
  const text = document.body ? document.body.innerText : '';
  return {
    email: mark('email', ['input[name="loginfmt"]', 'input[type="email"]'], typable),
    password: mark('password', ['input[name="passwd"]', 'input[type="password"]'], typable),
    otc: mark('otc', ['input[name="otc"]', '#idTxtBx_SAOTCC_OTC', 'input[autocomplete="one-time-code"]', 'input[type="tel"]'], typable),
    otherTile: mark('other-tile', ['#otherTile', '[data-test-id="otherTile"]'], shown),
    kmsi: mark('kmsi', ['#KmsiCheckboxField', 'input[name="DontShowAgain"]'], shown),
    next: any(['#idSIButton9', 'input[type="submit"]', 'button[type="submit"]']),
    passwordError: any(['#passwordError']),
    rejected: /password is incorrect|account has been locked/i.test(text),
    registration: /more information required|keep your account secure|set up your account/i.test(text),
    approval: /approve sign.in|open your authenticator|enter the number/i.test(text),
    proofs: any(['[data-value][role="button"]', 'div.table[role="button"]', '#idDiv_SAOTCS_Proofs [role="button"]']),
    prompt: text.split('\n').map((line) => line.trim()).filter(Boolean).slice(0, 8).join(' | ').slice(0, 400),
  };
})()"#;

/// Presses the first primary button shown.
const PRESS_NEXT: &str = r#"(() => {
  for (const selector of ['#idSubmit_SAOTCC_Continue', '#idSIButton9', 'input[type="submit"]', 'button[type="submit"]']) {
    for (const el of document.querySelectorAll(selector)) {
      const box = el.getBoundingClientRect();
      if (box.width > 0 && box.height > 0 && getComputedStyle(el).visibility !== 'hidden') { el.click(); return true; }
    }
  }
  return false;
})()"#;

/// Why `login` failed.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Accounts(#[from] AccountsError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("the sign-in page: {0}")]
    Cdp(#[from] CdpError),
    #[error("{0}")]
    SignIn(#[from] SignInStop),
    #[error("the sign-in page went away from its field before it was filled")]
    FieldGone,
    #[error("the page stopped on an unexpected host: {0}")]
    UnexpectedHost(String),
    #[error("the web client didn't load within {timeout:?} ({})", authz.map_or_else(|| "no authz answer".to_owned(), |status| format!("authz answered {status}")))]
    Timeout { timeout: Duration, authz: Option<u16> },
    #[error("no one-time code arrived in {} within {wait:?}", inbox.display())]
    NoCode { inbox: PathBuf, wait: Duration },
    #[error("the web client loaded, but no request showed the account's identity")]
    NoIdentity,
    #[error("the account's {0}")]
    BadIdentity(#[from] NotAGuid),
    #[error("can't write the status file: {0}")]
    Status(#[source] std::io::Error),
    #[error("the fence was breached: {0}")]
    Breach(String),
}

/// Reads the page: what a person would see to act on.
///
/// # Errors
/// When the page can't be evaluated.
pub async fn probe(cdp: &Cdp, page: &SessionId) -> Result<SignInProbe, CdpError> {
    let result = evaluate(cdp, page, PROBE).await?;
    Ok(SignInProbe::from_value(&result))
}

async fn evaluate(cdp: &Cdp, page: &SessionId, expression: &str) -> Result<Value, CdpError> {
    let params = json!({"expression": expression, "returnByValue": true});
    let result = cdp.call(Some(page), "Runtime.evaluate", params).await?;
    Ok(result["result"]["value"].clone())
}

/// Carries out one step on the page. [`Action::NeedCode`], [`Action::Wait`] and [`Action::Stop`] do nothing here.
///
/// # Errors
/// When the page can't be driven, or a field it named is gone.
pub async fn perform(
    cdp: &Cdp,
    page: &SessionId,
    action: &Action,
    credentials: &Credentials,
    progress: &mut Progress,
) -> Result<(), LoginError> {
    match action {
        Action::TypeEmail(field) => {
            type_into(cdp, page, field, credentials.email()).await?;
            press_next(cdp, page).await?;
            progress.email_sent = true;
        }
        Action::TypePassword(field) => {
            // counted as sent before anything is typed: a failure halfway must not lead to a second try
            progress.password_sent = true;
            type_into(cdp, page, field, credentials.password().expose()).await?;
            press_next(cdp, page).await?;
        }
        Action::ClickOtherTile(tile) => click(cdp, page, tile).await?,
        Action::AcceptKmsi(checkbox) => {
            let tick = format!(
                "(() => {{ const box = document.querySelector({}); if (!box) return false; if (!box.checked) box.click(); return true; }})()",
                js_string(checkbox)
            );
            if evaluate(cdp, page, &tick).await? != json!(true) {
                return Err(LoginError::FieldGone);
            }
            press_next(cdp, page).await?;
        }
        Action::ClickNext => press_next(cdp, page).await?,
        Action::NeedCode(_) | Action::Wait | Action::Stop(_) => {}
    }
    Ok(())
}

/// Focuses the field, selects what's in it, and types `text` a character at a time, as a person would.
async fn type_into(cdp: &Cdp, page: &SessionId, field: &str, text: &str) -> Result<(), LoginError> {
    let focus = format!(
        "(() => {{ const el = document.querySelector({}); if (!el) return false; el.focus(); el.select(); return true; }})()",
        js_string(field)
    );
    if evaluate(cdp, page, &focus).await? != json!(true) {
        return Err(LoginError::FieldGone);
    }
    let mut buffer = [0u8; 4];
    for (index, character) in text.chars().enumerate() {
        let params = json!({"text": character.encode_utf8(&mut buffer)});
        cdp.call(Some(page), "Input.insertText", params).await?;
        tokio::time::sleep(keystroke_gap(index)).await;
    }
    Ok(())
}

/// 40–120 ms between keystrokes, varied.
fn keystroke_gap(index: usize) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |now| now.subsec_nanos());
    let spread = (u64::from(nanos) / 7 + index as u64 * 37) % 80;
    Duration::from_millis(40 + spread)
}

async fn click(cdp: &Cdp, page: &SessionId, selector: &str) -> Result<(), LoginError> {
    let script = format!(
        "(() => {{ const el = document.querySelector({}); if (!el) return false; el.click(); return true; }})()",
        js_string(selector)
    );
    if evaluate(cdp, page, &script).await? != json!(true) {
        return Err(LoginError::FieldGone);
    }
    Ok(())
}

/// A selector as a JavaScript string literal.
fn js_string(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

async fn press_next(cdp: &Cdp, page: &SessionId) -> Result<(), LoginError> {
    evaluate(cdp, page, PRESS_NEXT).await?;
    Ok(())
}

/// `status-<alias>.json` in the lab's data dir: the login's current state, for whoever runs it.
pub struct StatusFile {
    path: PathBuf,
    alias: Alias,
}

impl StatusFile {
    #[must_use]
    pub fn new(data_dir: &Path, alias: Alias) -> Self {
        Self {
            path: data_dir.join(format!("status-{alias}.json")),
            alias,
        }
    }

    /// Replaces the file with `{alias, state, at_unix_ms, ...extra}`, readable by the owner only.
    ///
    /// # Errors
    /// When the file can't be written.
    pub fn write(&self, state: &str, extra: &Value) -> std::io::Result<()> {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

        let at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |now| u64::try_from(now.as_millis()).unwrap_or(u64::MAX));
        let mut record = json!({"alias": self.alias.as_str(), "state": state, "at_unix_ms": at});
        if let (Some(record), Some(extra)) = (record.as_object_mut(), extra.as_object()) {
            record.extend(extra.iter().map(|(key, value)| (key.clone(), value.clone())));
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&self.path)?;
        // a file left by another tool may be wider
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(format!("{record:#}\n").as_bytes())
    }
}

/// Takes the one-time code someone left in `inbox/<alias>.code`: read, trimmed and removed. `None` while there is none.
///
/// # Errors
/// When the file is there but can't be read or removed.
pub fn take_code(inbox: &Path, alias: Alias) -> std::io::Result<Option<String>> {
    let path = inbox.join(format!("{alias}.code"));
    let code = match std::fs::read_to_string(&path) {
        Ok(code) => code.trim().to_owned(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    std::fs::remove_file(&path)?;
    Ok(Some(code))
}

/// How a login runs.
#[derive(Clone, Copy, Debug)]
pub struct LoginOptions {
    /// How long the whole sign-in and load may take.
    pub timeout: Duration,
    /// How long to wait for a one-time code once one is asked for.
    pub code_wait: Duration,
}

/// What a login did.
#[derive(Debug)]
pub struct LoginReport {
    /// It had to sign in (the profile's session had expired).
    pub signed_in: bool,
    /// `None` when the web client loaded; the status of Teams' refused authz exchange when the account is signed in
    /// but Teams won't serve it (no Teams license). Its MRI is recorded either way.
    pub refused: Option<u16>,
    pub mri: MriRecord,
    pub fence: FenceReport,
}

/// What the browser part of a login found.
struct BrowserOutcome {
    signed_in: bool,
    refused: Option<u16>,
    mri: String,
}

/// Signs `alias` in and records its MRI (see the module docs).
///
/// # Errors
/// When the lab config or the account file is refused, the browser can't be run under the fence, the sign-in stops,
/// the web client doesn't load in time, the identity isn't shown, the MRI can't be recorded, or the fence was
/// breached. The status file says `failed` then.
pub async fn login(alias: Alias, options: LoginOptions) -> Result<LoginReport, LoginError> {
    let locations = LabLocations::from_env()?;
    let config = LabConfig::load(&locations)?;
    let paths = config.paths().clone();
    let status = StatusFile::new(paths.data_dir(), alias);
    let outcome = async {
        let upn = config.account(alias).upn.clone();
        let accounts = AccountsFile::load(Path::new(ACCOUNTS_FILE), |name| std::env::var(name).ok())?;
        let credentials = accounts.for_alias(alias, &upn)?;
        status.write("starting", &json!({})).map_err(LoginError::Status)?;
        let mut session = LabSession::open(&paths, alias, Classifier::sign_in()).await?;
        let run = Run {
            alias,
            upn: &upn,
            credentials,
            status: &status,
            inbox: paths.data_dir().join("inbox"),
            options,
        };
        let signed = run.drive(&mut session).await;
        // the browser goes down whatever happened, before anything else is reported
        let fence = session.close().await;
        let signed = signed?;
        let fence = fence?;
        if fence.breached() {
            return Err(LoginError::Breach(fence.summary()));
        }
        let mri = record_account_mri(&locations, alias, &signed.mri)?;
        Ok(LoginReport {
            signed_in: signed.signed_in,
            refused: signed.refused,
            mri,
            fence,
        })
    }
    .await;
    let written = match &outcome {
        Ok(report) => match report.refused {
            None => status.write("done", &json!({"fence": report.fence.summary()})),
            Some(code) => status.write(
                "teams-refused",
                &json!({"authz_status": code, "fence": report.fence.summary()}),
            ),
        },
        Err(error) => status.write("failed", &json!({"error": error.to_string()})),
    };
    let report = outcome?;
    written.map_err(LoginError::Status)?;
    Ok(report)
}

/// One login's moving parts.
struct Run<'a> {
    alias: Alias,
    upn: &'a str,
    credentials: &'a Credentials,
    status: &'a StatusFile,
    inbox: PathBuf,
    options: LoginOptions,
}

/// How often the sign-in page is looked at.
const TICK: Duration = Duration::from_millis(700);
/// How long the page gets after a code before it is looked at again.
const CODE_SETTLE: Duration = Duration::from_secs(3);
/// How long the identity may take to show once the web client has loaded.
const IDENTITY_WAIT: Duration = Duration::from_secs(30);

impl Run<'_> {
    /// Opens Teams and signs in where asked, until the web client loads (or Teams refuses the signed-in account); then
    /// waits for the identity.
    async fn drive(&self, session: &mut LabSession) -> Result<BrowserOutcome, LoginError> {
        session.navigate(TEAMS_URL).await?;
        let mut progress = Progress::default();
        let refused = self.until_loaded(session, &mut progress).await?;
        let signed_in = progress.email_sent || progress.password_sent;
        let state = if refused.is_some() {
            "signed-in-teams-refused"
        } else {
            "loaded"
        };
        self.status
            .write(state, &json!({"signed_in": signed_in}))
            .map_err(LoginError::Status)?;
        let identity_deadline = tokio::time::Instant::now() + IDENTITY_WAIT;
        while session.watch().identity_for(self.upn).is_none() && tokio::time::Instant::now() < identity_deadline {
            if !session.pump(TICK).await {
                break;
            }
        }
        let identity = session.watch().identity_for(self.upn).ok_or(LoginError::NoIdentity)?;
        Ok(BrowserOutcome {
            signed_in,
            refused,
            mri: identity.mri()?,
        })
    }

    /// Signs in where asked until the web client loads (`None`) or Teams refuses the account (its authz status),
    /// a refusal counting once it has stood for [`REFUSAL_GRACE`].
    async fn until_loaded(&self, session: &mut LabSession, progress: &mut Progress) -> Result<Option<u16>, LoginError> {
        let deadline = tokio::time::Instant::now() + self.options.timeout;
        let mut refused_at = None;
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(LoginError::Timeout {
                    timeout: self.options.timeout,
                    authz: session.watch().last_authz_status(),
                });
            }
            if !session.pump(TICK).await {
                return Err(LoginError::Breach(session.report().summary()));
            }
            match session.watch().state() {
                SessionState::Loaded => return Ok(None),
                SessionState::Refused(status) => {
                    if refused_at.get_or_insert_with(tokio::time::Instant::now).elapsed() >= REFUSAL_GRACE {
                        return Ok(Some(status));
                    }
                }
                SessionState::Loading => refused_at = None,
                SessionState::Elsewhere(host) => return Err(LoginError::UnexpectedHost(host)),
                SessionState::SignInShown => self.sign_in_step(session, progress).await?,
            }
        }
    }

    /// One look at the sign-in page and the step it calls for.
    async fn sign_in_step(&self, session: &LabSession, progress: &mut Progress) -> Result<(), LoginError> {
        let (cdp, page) = (session.cdp(), session.page());
        let found = probe(cdp, page).await?;
        let action = next_action(&found, *progress);
        let before = *progress;
        match &action {
            Action::Stop(stop) => return Err((*stop).into()),
            Action::NeedCode(field) => return self.enter_code(cdp, page, field, &found.prompt).await,
            _ => perform(cdp, page, &action, self.credentials, progress).await?,
        }
        let state = match action {
            Action::TypeEmail(_) if !before.email_sent => "email-submitted",
            Action::TypePassword(_) => "password-submitted",
            Action::AcceptKmsi(_) => "kmsi-accepted",
            _ => return Ok(()),
        };
        self.status.write(state, &json!({})).map_err(LoginError::Status)
    }

    /// Asks for a one-time code through the status file and waits for it in the inbox.
    async fn enter_code(&self, cdp: &Cdp, page: &SessionId, field: &str, prompt: &str) -> Result<(), LoginError> {
        let status = |error| LoginError::Status(error);
        self.status
            .write("need-code", &json!({"prompt": prompt}))
            .map_err(status)?;
        let deadline = tokio::time::Instant::now() + self.options.code_wait;
        loop {
            if let Some(code) = take_code(&self.inbox, self.alias).map_err(status)? {
                type_into(cdp, page, field, &code).await?;
                press_next(cdp, page).await?;
                self.status.write("code-submitted", &json!({})).map_err(status)?;
                // the page moves on, or shows the field again for another code
                tokio::time::sleep(CODE_SETTLE).await;
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(LoginError::NoCode {
                    inbox: self.inbox.clone(),
                    wait: self.options.code_wait,
                });
            }
            tokio::time::sleep(TICK).await;
        }
    }
}
