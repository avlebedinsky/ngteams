//! Captures file attachment request shapes. Explicit file/media holds retain writes for inspection, while the plain chat message exercises delivery.

use std::time::Duration;

use crate::browser::Media;
use crate::capture_hold::{HeldRequest, Hold};
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{
    COMPOSE, COMPOSE_CSS, DELIVERY, chat_link, chat_requests, load, nonce, one_to_one_id, save_screenshot_as, send,
    setup, wait_for_newest,
};
use super::ab_chat_full::{CHAT_OPEN, PAGE_CALL, clear_compose, message_action};
use super::ab_image::{AttachmentWaits, MEDIA_WRITE, SETTLE, media_rules};
use super::dsl::{Driver, Key, RequestPattern, StepError, js_string};
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The label of the capture hold of every write to the file services ([`FILE_SERVICE`]).
pub const FILE_WRITE: &str = "a file service write";

/// The label of the capture hold of every `POST` or `PUT` whose body names a file of this scenario ([`FILE_MARKER`]).
pub const FILE_MESSAGE: &str = "a write naming a lab file";

/// What every file name and caption of this scenario holds ([`Texts`]); a write whose body holds it is denied.
pub const FILE_MARKER: &str = "lab-file-";

/// The labels of a write of the file: what [`is_file_write`] counts.
pub const FILE_LABELS: [&str; 3] = [FILE_WRITE, MEDIA_WRITE, FILE_MESSAGE];

/// A request to a file service, any path: a `SharePoint` or `OneDrive` host (the account's own `<tenant>-my`, a site, the
/// consumer hosts), `SharePoint`'s media hosts (`*.svc.ms`), and Graph (whose drive, share and batch paths reach the
/// same files).
pub const FILE_SERVICE: &str = r"(?i)^https?://([^/?#]*\.)?(sharepoint\.com|sharepoint-df\.com|microsoftpersonalcontent\.com|onedrive\.com|onedrive\.live\.com|svc\.ms|graph\.microsoft\.com)(:[0-9]+)?(/|\?|#|$)";

/// The methods [`hold_file_writes`] denies on [`FILE_SERVICE`]: every one but a read (and a preflight, which Chrome
/// answers without the page).
pub const FILE_METHODS: [&str; 5] = ["POST", "PUT", "PATCH", "DELETE", "MERGE"];

/// How long the client gets to try a write of the file once it is in the compose box or sent.
pub const FILE_WAIT: Duration = Duration::from_secs(60);

/// How long the compose box gets to show a dropped or pasted file.
const ATTACH_WAIT: Duration = Duration::from_secs(15);

/// What a's messages and file are called; each holds the run's nonce, the caption and the file name [`FILE_MARKER`]
/// as well, the plain message never.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texts {
    /// The plain message b shows.
    pub plain: String,
    /// The text sent with the file.
    pub caption: String,
    /// The file's name.
    pub file_name: String,
}

impl Texts {
    #[must_use]
    pub fn new(nonce: &str) -> Self {
        Self {
            plain: format!("lab file {nonce} plain"),
            caption: format!("{FILE_MARKER}{nonce} caption"),
            file_name: format!("{FILE_MARKER}{nonce}.txt"),
        }
    }

    /// The file name without its extension, which a page may show apart from it.
    #[must_use]
    pub fn file_stem(&self) -> &str {
        self.file_name.strip_suffix(".txt").unwrap_or(&self.file_name)
    }
}

/// The made-up test file: a few lines of plain text naming the run, nothing taken from anything real.
#[must_use]
pub fn test_file(nonce: &str) -> Vec<u8> {
    format!("A made-up text file of the lab's ab-file scenario, run {nonce}.\nIt holds nothing else.\n").into_bytes()
}

/// The rules [`hold_file_writes`] forces, in order (a deny names the first rule that matched it): the file services'
/// writes ([`FILE_WRITE`]), the AMS objects' (`ab-image`'s [`MEDIA_WRITE`]), then any `POST` or `PUT` naming a lab file
/// ([`FILE_MESSAGE`]).
///
/// # Errors
/// Never: the patterns are fixed.
pub fn file_rules() -> Result<Vec<Hold>, regex::Error> {
    let mut rules = Vec::new();
    for method in FILE_METHODS {
        rules.push(Hold::new(method, FILE_SERVICE)?.labelled(FILE_WRITE));
    }
    rules.extend(media_rules()?);
    for method in ["POST", "PUT"] {
        rules.push(
            Hold::new(method, ".")?
                .with_body_containing(FILE_MARKER)
                .labelled(FILE_MESSAGE),
        );
    }
    Ok(rules)
}

/// Forces the denies of [`file_rules`] on `session`.
///
/// # Errors
/// Never: the patterns are fixed.
pub fn hold_file_writes(session: &LabSession) -> Result<(), ScenarioError> {
    for rule in file_rules().map_err(|error| setup(error.to_string()))? {
        session.hold(rule);
    }
    Ok(())
}

/// Whether `denial` is a write of the file the run forced ([`FILE_LABELS`]).
#[must_use]
pub fn is_file_write(denial: &HeldRequest) -> bool {
    denial
        .label
        .as_deref()
        .is_some_and(|label| FILE_LABELS.contains(&label))
}

/// The page expression that hands the compose box ([`COMPOSE_CSS`]) `content` as a file named `file_name` of type
/// `mime`, as a clipboard paste (`how` `"paste"`) or a drop (`"drop"`): it builds the file in the page and dispatches
/// the event the editor listens for. It answers `true` once dispatched, `false` without a compose box.
#[must_use]
pub fn file_event(content: &[u8], file_name: &str, mime: &str, how: &str) -> String {
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.encode(content);
    let file_name = js_string(file_name);
    let mime = js_string(mime);
    let compose = js_string(COMPOSE_CSS);
    let how = js_string(how);
    format!(
        r#"(() => {{
  const editor = document.querySelector({compose});
  if (!editor) return false;
  editor.focus();
  const bytes = Uint8Array.from(atob("{data}"), (c) => c.charCodeAt(0));
  const file = new File([bytes], {file_name}, {{ type: {mime} }});
  const transfer = new DataTransfer();
  transfer.items.add(file);
  if ({how} === "paste") {{
    editor.dispatchEvent(new ClipboardEvent("paste", {{ clipboardData: transfer, bubbles: true, cancelable: true }}));
  }} else {{
    for (const type of ["dragenter", "dragover", "drop"]) {{
      editor.dispatchEvent(new DragEvent(type, {{ dataTransfer: transfer, bubbles: true, cancelable: true }}));
    }}
  }}
  return true;
}})()"#
    )
}

/// The page expression: whether the page shows `file_name` (whole, its extension too) outside every message
/// (`chat-pane-message`): in the text of the innermost element holding it (a name drawn in pieces, the stem apart
/// from the extension, counts), a title or a label. That is the compose box's attachment, before the message is
/// sent: the name holds the run's nonce, and the caption holds the stem but never the extension (live, 2026-10-22: a
/// test on the stem alone took the typed caption for the file).
#[must_use]
pub fn shows_attachment(file_name: &str) -> String {
    format!(
        r#"(() => {{
  const name = {name};
  const labelled = (e) => (e.getAttribute('title') ?? '').includes(name) || (e.getAttribute('aria-label') ?? '').includes(name);
  const holds = (e) => (e.textContent ?? '').includes(name);
  const innermost = (e) => holds(e) && ![...e.children].some(holds);
  return [...document.body.querySelectorAll('*')]
    .filter((e) => e.tagName !== 'SCRIPT' && e.tagName !== 'STYLE')
    .some((e) => (labelled(e) || innermost(e)) && !e.closest('[data-tid="chat-pane-message"]'));
}})()"#,
        name = js_string(file_name)
    )
}

/// The page expression that makes the page's file pickers hand over `content` as a file named `file_name` of type
/// `mime`, without a dialog: a click on (or `showPicker` of) any `<input type="file">` sets its files to it and fires
/// its `input` and `change`, and `showOpenFilePicker` answers with it. The browser's own dialog never opens (a headless
/// one shows to nobody). `window.__labFileHanded` counts the hand-overs. It answers `true` once installed.
#[must_use]
pub fn hand_file_to_pickers(content: &[u8], file_name: &str, mime: &str) -> String {
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.encode(content);
    let file_name = js_string(file_name);
    let mime = js_string(mime);
    format!(
        r#"(() => {{
  const bytes = Uint8Array.from(atob("{data}"), (c) => c.charCodeAt(0));
  const file = new File([bytes], {file_name}, {{ type: {mime} }});
  window.__labFileHanded = window.__labFileHanded ?? 0;
  const hand = (input) => {{
    const transfer = new DataTransfer();
    transfer.items.add(file);
    input.files = transfer.files;
    input.dispatchEvent(new Event('input', {{ bubbles: true }}));
    input.dispatchEvent(new Event('change', {{ bubbles: true }}));
    window.__labFileHanded += 1;
  }};
  const click = HTMLInputElement.prototype.click;
  HTMLInputElement.prototype.click = function () {{
    if (this.type === 'file') {{ hand(this); return; }}
    return click.call(this);
  }};
  const picker = HTMLInputElement.prototype.showPicker;
  HTMLInputElement.prototype.showPicker = function () {{
    if (this.type === 'file') {{ hand(this); return; }}
    return picker?.call(this);
  }};
  window.showOpenFilePicker = async () => {{
    window.__labFileHanded += 1;
    return [{{ kind: 'file', name: file.name, getFile: async () => file }}];
  }};
  return true;
}})()"#
    )
}

/// The page expression that clicks the compose box's attach button: the first visible button among the controls around
/// the compose box (its sixth ancestor's, as [`COMPOSE_CONTROLS`] names them) whose label or title speaks of attaching
/// (live, 2026-10-22: "Attach files"; a test on "file" alone took the account's "Your profile" button). It answers the
/// label of what it clicked, `null` when there is none.
pub const CLICK_ATTACH: &str = r#"(() => {
  const named = (e) => [e.getAttribute('aria-label'), e.getAttribute('title')].some((text) => /attach/i.test(text ?? ''));
  const shown = (e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
  const box = document.querySelector('[contenteditable="true"][role="textbox"]');
  if (!box) return null;
  let scope = box;
  for (let i = 0; i < 6 && scope.parentElement; i++) scope = scope.parentElement;
  const button = [...scope.querySelectorAll('button, [role="button"]')].find((e) => named(e) && shown(e));
  if (!button) return null;
  button.click();
  return button.getAttribute('aria-label') ?? button.getAttribute('title') ?? '';
})()"#;

/// The page expression that picks, in a menu open on the page, the item that uploads from the computer (its text
/// speaks of uploading from this device or the computer). It answers `true` once clicked, `false` when there is none.
pub const PICK_UPLOAD: &str = r#"(() => {
  const shown = (e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
  const item = [...document.querySelectorAll('[role="menuitem"], [role="menuitemradio"], [role="option"]')]
    .find((e) => shown(e) && /upload|this device|computer/i.test(e.innerText ?? ''));
  if (!item) return false;
  item.click();
  return true;
})()"#;

/// The page expression that reads what the page says in a popup after the attach button (a dialog, an alert, a
/// popover or a tooltip that is shown): each one's text on one line, at most 300 characters, at most 5. Live,
/// 2026-10-22: "Sorry, you need `OneDrive` to share files", once the account's own file storage check
/// (`me/files/availability`, a read by `POST`) was denied.
pub const PAGE_NOTICES: &str = r#"(() => {
  const shown = (e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
  return [...document.querySelectorAll('[role="dialog"], [role="alertdialog"], [role="alert"], [role="tooltip"], [role="status"], [data-popper-placement], .fui-PopoverSurface')]
    .filter(shown)
    .map((e) => (e.innerText ?? '').replace(/\s+/g, ' ').trim().slice(0, 300))
    .filter((text) => text.length > 0)
    .slice(0, 5);
})()"#;

/// The page expression that names the controls around the compose box, for a run whose file wasn't taken: each visible
/// button's label, title or `data-tid` (the page's own words, never a message's text), at most 40.
pub const COMPOSE_CONTROLS: &str = r#"(() => {
  const shown = (e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
  const box = document.querySelector('[contenteditable="true"][role="textbox"]');
  let scope = box;
  for (let i = 0; i < 6 && scope?.parentElement; i++) scope = scope.parentElement;
  return [...(scope ?? document).querySelectorAll('button, [role="button"], [role="menuitem"]')]
    .filter(shown)
    .map((e) => e.getAttribute('aria-label') ?? e.getAttribute('title') ?? e.getAttribute('data-tid') ?? '?')
    .slice(0, 40);
})()"#;

/// The page expression that marks the newest message (`chat-pane-message`) naming `stem` (this run's file name without
/// its extension) as [`super::ab_chat_full::MARKED`] (the mark taken off any other), `true` when there is one.
///
/// Every write naming a lab file is denied in the run, so such a message can only be this run's file left unsent; a
/// message naming another run's file, or none, is never marked, so it is never deleted.
#[must_use]
pub fn mark_unsent_file(stem: &str) -> String {
    format!(
        r#"(() => {{
  for (const e of document.querySelectorAll('[data-lab-mark]')) e.removeAttribute('data-lab-mark');
  const stem = {stem};
  const names = (e) => (e.innerText ?? '').includes(stem)
    || [...e.querySelectorAll('[title], [aria-label]')]
      .some((n) => (n.getAttribute('title') ?? '').includes(stem) || (n.getAttribute('aria-label') ?? '').includes(stem));
  const found = [...document.querySelectorAll('[data-tid="chat-pane-message"]')].filter(names);
  const newest = found[found.length - 1];
  if (!newest) return false;
  newest.setAttribute('data-lab-mark', '');
  return true;
}})()"#,
        stem = js_string(stem)
    )
}

/// Runs `ab-file` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, no Chrome, a browser whose capture setup fails.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let session_options = SessionOptions {
        capture: Some(ScenarioName::AbFile.as_str().to_owned()),
        capture_request_bodies: true,
        display: options.display,
        media: Media::System,
        user_agent: None,
    };
    let a = LabSession::open_with(paths, Alias::A, session_options.clone()).await?;
    let b = match LabSession::open_with(paths, Alias::B, session_options).await {
        Ok(b) => b,
        Err(error) => {
            // a's browser goes down whatever happened
            let _ = a.close().await;
            return Err(error.into());
        }
    };
    let nonce = nonce();
    let mri = |alias| config.account(alias).mri.clone().unwrap_or_default();
    let chat_id = one_to_one_id(&mri(Alias::A), &mri(Alias::B));
    let file = File {
        mri_a: mri(Alias::A),
        mri_b: mri(Alias::B),
        texts: Texts::new(&nonce),
        content: test_file(&nonce),

        options,
        waits: AttachmentWaits {
            attach: ATTACH_WAIT,
            upload: FILE_WAIT,
            settle: SETTLE,
        },
    };

    Ok(finish(a, b, chat_id.as_deref(), &file).await)
}

async fn finish(mut a: LabSession, mut b: LabSession, chat_id: Option<&str>, file: &File) -> ScenarioReport {
    let mut report = ScenarioReport::new(ScenarioName::AbFile);
    // Arm the rules before either client loads or retries an earlier attachment.
    if let Err(error) = hold_file_writes(&a) {
        report.failures.push(error.to_string());
    }

    if let Err(error) = file.run(chat_id, &mut a, &mut b, &mut report).await {
        report.failures.push(error.to_string());
        for (alias, session) in [(Alias::A, &a), (Alias::B, &b)] {
            if let Some(note) = save_screenshot_as(alias, session, "failure.png").await {
                report.notes.push(note);
            }
        }
    }
    let writes = a.held_requests().iter().filter(|denial| is_file_write(denial)).count();
    report
        .notes
        .push(format!("a: {writes} write(s) of the file denied in all"));
    // both browsers go down and both captures are finished, whatever happened to either
    for (alias, session) in [(Alias::A, a), (Alias::B, b)] {
        report.close_account(alias, session).await;
    }
    report
}

/// One run's accounts, texts, file and probe.
struct File {
    mri_a: String,
    mri_b: String,
    texts: Texts,
    content: Vec<u8>,

    options: RunOptions,
    waits: AttachmentWaits,
}

impl File {
    /// Loading, a's plain message and file, a's reload, then b's read.
    async fn run(
        &self,
        chat_id: Option<&str>,
        a: &mut LabSession,
        b: &mut LabSession,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        for (alias, session) in [(Alias::A, &mut *a), (Alias::B, &mut *b)] {
            load(alias, session, self.options.load_timeout).await?;
            report.notes.push(format!("{alias}: the web client loaded"));
        }
        let chat_id = chat_id
            .ok_or_else(|| setup("lab.toml has no MRI for a or b: run `proto-lab login` for them".to_owned()))?;

        let opened = chat_requests(chat_id).map_err(|error| setup(error.to_string()))?;
        let mut drive_a = a.driver();
        drive_a.set_call_timeout(PAGE_CALL);
        drive_a
            .navigate("a opens the chat with b", &chat_link(&self.mri_b))
            .await?;
        drive_a.wait_for("a's compose box", COMPOSE, CHAT_OPEN).await?;

        send(&mut drive_a, "a (the plain message)", &self.texts.plain).await?;
        report.notes.push("a: plain message sent".to_owned());
        let outcome = self.send_file(a, &mut drive_a, report).await;
        if let Some(note) = save_screenshot_as(Alias::A, a, "after-file.png").await {
            report.notes.push(note);
        }
        outcome?;
        self.reload(a, report).await?;
        self.b_reads(b, &opened, report).await
    }

    /// a types the caption, hands the compose box the test file ([`Self::attach`]; a run whose file the box doesn't take
    /// fails here, before anything is sent, naming the controls around the box) and presses Enter; the client must try
    /// a write of the file, denied, within [`FILE_WAIT`]. Then [`SETTLE`] more, noting every deny since the
    /// file was handed over.
    async fn send_file(
        &self,
        a: &LabSession,
        drive: &mut Driver,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let before = a.held_requests().len();
        let before_file = file_writes(a).len();
        drive.skip_seen();
        drive
            .type_into("a types the caption", COMPOSE, &self.texts.caption)
            .await?;
        let Some(how) = self.attach(drive, report).await? else {
            let controls = drive
                .evaluate("a's compose controls", COMPOSE_CONTROLS)
                .await
                .unwrap_or_default();
            report
                .notes
                .push(format!("a: the controls around the compose box: {controls}"));
            let notices = drive
                .evaluate("what a's page says", PAGE_NOTICES)
                .await
                .unwrap_or_default();
            report.notes.push(format!("a: what the page says: {notices}"));
            // the caption, which names the file, isn't left in the box for a later run to send with its own text
            let _ = drive.press("a closes the popup", Key::ESCAPE).await;
            let cleared = clear_compose(drive).await;
            report.notes.push(format!(
                "a: the caption taken out of the compose box: {}",
                cleared.is_ok()
            ));
            return Err(setup(
                "the compose box showed no file after a drop, a paste and its attach button".to_owned(),
            ));
        };
        report.notes.push(format!("a: the compose box took the file ({how})"));
        let early = file_writes(a).len().saturating_sub(before_file);
        report
            .notes
            .push(format!("a: {early} write(s) of the file denied before Enter"));
        drive.press_enter("a sends the file").await?;
        let Some(first) = wait_file_write(a, before_file, self.waits.upload).await else {
            note_denials(a, before, report);
            return Err(setup(format!(
                "the client tried no write of the file within {}s of it",
                self.waits.upload.as_secs()
            )));
        };
        report.notes.push(format!(
            "a: first write of the file denied: {} {} ({})",
            first.method,
            first.url,
            first.label.as_deref().unwrap_or("-")
        ));
        tokio::time::sleep(self.waits.settle).await;
        note_denials(a, before, report);
        let caption_post = RequestPattern::new(Some("POST"), super::ab_chat::MESSAGE_POST)
            .map_err(|error| setup(error.to_string()))?
            .with_body_containing(FILE_MARKER);
        let posted = drive
            .wait_for_request("a's file post", &caption_post, Duration::from_millis(500))
            .await;
        report.notes.push(match posted {
            Ok(seen) => format!(
                "a: a message post naming the file tried to go out: {:?} {:?}",
                seen.status, seen.failed
            ),
            Err(_) => "a: no message post naming the file was tried".to_owned(),
        });
        Ok(())
    }

    /// Hands the compose box the test file as [`Texts::file_name`]: a drop on it, else a paste into it, else its attach
    /// button with the page's file pickers made to hand the file over ([`Self::attach_by_picker`]); which one the page
    /// took (it shows the file's whole name outside the messages, [`shows_attachment`]), or `None` when it showed it
    /// after none.
    ///
    /// # Errors
    /// When the compose box is gone, or the page doesn't answer.
    async fn attach(
        &self,
        drive: &mut Driver,
        report: &mut ScenarioReport,
    ) -> Result<Option<&'static str>, ScenarioError> {
        for how in ["drop", "paste"] {
            let event = file_event(&self.content, &self.texts.file_name, "text/plain", how);
            if drive.evaluate(&format!("a's file ({how})"), &event).await? != serde_json::Value::Bool(true) {
                return Err(setup("a's compose box went away before the file".to_owned()));
            }
            if self.attached(drive, how).await? {
                return Ok(Some(how));
            }
        }
        if self.attach_by_picker(drive, report).await? {
            return Ok(Some("picker"));
        }
        Ok(None)
    }

    /// The attach button: the page's file pickers are made to hand the file over ([`hand_file_to_pickers`]), the
    /// button is clicked ([`CLICK_ATTACH`]) and, if a menu opens, its upload item ([`PICK_UPLOAD`]); whether the page
    /// then shows the file. What was clicked is noted.
    async fn attach_by_picker(&self, drive: &mut Driver, report: &mut ScenarioReport) -> Result<bool, ScenarioError> {
        let hand = hand_file_to_pickers(&self.content, &self.texts.file_name, "text/plain");
        drive.evaluate("a's file pickers hand the file over", &hand).await?;
        let clicked = drive.evaluate("a clicks the attach button", CLICK_ATTACH).await?;
        report.notes.push(format!("a: attach button clicked: {clicked}"));
        if clicked.is_null() {
            return Ok(false);
        }
        let picked = drive
            .wait_until_true(
                "a picks the upload item",
                PICK_UPLOAD,
                "an upload item in a menu",
                self.waits.attach,
            )
            .await;
        report.notes.push(format!("a: upload item picked: {}", picked.is_ok()));
        let handed = drive
            .evaluate("a's file pickers", "window.__labFileHanded ?? 0")
            .await?;
        report
            .notes
            .push(format!("a: file pickers handed the file over {handed} time(s)"));
        self.attached(drive, "picker").await
    }

    /// Whether the page shows the file attached within [`ATTACH_WAIT`] ([`shows_attachment`]).
    async fn attached(&self, drive: &mut Driver, how: &str) -> Result<bool, ScenarioError> {
        match drive
            .wait_until_true(
                &format!("a's compose box shows the file ({how})"),
                &shows_attachment(&self.texts.file_name),
                "the file's name beside the compose box",
                self.waits.attach,
            )
            .await
        {
            Ok(()) => Ok(true),
            Err(StepError::Timeout { .. }) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// a's web client is loaded again; for [`SETTLE`] the run notes whether it tries the file again, then deletes this
    /// run's file if a's chat shows it unsent.
    async fn reload(&self, a: &mut LabSession, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        let before = file_writes(a).len();
        load(Alias::A, a, self.options.load_timeout).await?;
        tokio::time::sleep(self.waits.settle).await;
        let again = file_writes(a).len().saturating_sub(before);
        report.notes.push(format!(
            "a: after loading again, {again} write(s) of the file denied within {}s",
            self.waits.settle.as_secs()
        ));
        let outcome = self.discard_unsent(a).await;
        report.notes.push(match outcome {
            Ok(note) => format!("a: unsent file: {note}"),
            Err(error) => format!("a: unsent file: couldn't delete it: {error}"),
        });
        Ok(())
    }

    /// Deletes, from a's chat with b, this run's file left unsent ([`mark_unsent_file`]), from its actions; without
    /// one, nothing is deleted. What it finds and does is noted; it fails nothing.
    async fn discard_unsent(&self, a: &LabSession) -> Result<String, ScenarioError> {
        let mut drive = a.driver();
        drive.set_call_timeout(PAGE_CALL);
        drive
            .navigate("a opens the chat with b again", &chat_link(&self.mri_b))
            .await?;
        drive.wait_for("a's compose box", COMPOSE, CHAT_OPEN).await?;
        let mark = mark_unsent_file(self.texts.file_stem());
        let found = drive
            .wait_until_true(
                "a finds this run's unsent file",
                &mark,
                "a message naming this run's file",
                self.waits.attach,
            )
            .await;
        match found {
            Ok(()) => {}
            Err(StepError::Timeout { .. }) => return Ok("none of this run's shown after loading again".to_owned()),
            Err(error) => return Err(error.into()),
        }
        message_action(&mut drive, "a", "delete").await?;
        let gone = drive
            .wait_until_true(
                "a's unsent file is gone",
                &format!("!{mark}"),
                "no message naming this run's file",
                self.waits.attach,
            )
            .await;
        Ok(match gone {
            Ok(()) => "deleted from its actions".to_owned(),
            Err(_) => "delete picked, but this run's file is still shown".to_owned(),
        })
    }

    /// b opens the chat with a, shows a's plain message, and must not have the probe; whether it shows the file's name
    /// is noted.
    async fn b_reads(
        &self,
        b: &LabSession,
        opened: &RequestPattern,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let mut drive = b.driver();
        drive.set_call_timeout(PAGE_CALL);
        drive
            .navigate("b opens the chat with a", &chat_link(&self.mri_a))
            .await?;
        drive
            .wait_for_request("b reads the a–b chat", opened, CHAT_OPEN)
            .await?;
        wait_for_newest(&mut drive, "b shows a's plain message", &self.texts.plain, DELIVERY).await?;

        let named = drive
            .shows_text("b shows the file's name", self.texts.file_stem())
            .await?;
        report.notes.push(format!("b: shows the file's name: {named}"));
        Ok(())
    }
}

/// Notes every deny of `session` after the first `before`: method, URL (without its query) and label or cause.
fn note_denials(session: &LabSession, before: usize, report: &mut ScenarioReport) {
    for denial in session.held_requests().iter().skip(before) {
        let why = denial.label.clone().unwrap_or_else(|| "capture hold".to_owned());
        report
            .notes
            .push(format!("a: denied: {} {} ({why})", denial.method, denial.url));
    }
}

/// `session`'s writes of the file denied so far, in order.
fn file_writes(session: &LabSession) -> Vec<HeldRequest> {
    session.held_requests().into_iter().filter(is_file_write).collect()
}

/// Waits up to `timeout` for a write of the file denied after the first `before`; the first such one.
async fn wait_file_write(session: &LabSession, before: usize, timeout: Duration) -> Option<HeldRequest> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(denial) = file_writes(session).into_iter().nth(before) {
            return Some(denial);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(test)]
#[path = "../../tests/support/scenario/ab_file_driver_tests.rs"]
mod driver_tests;
