//! Captures image attachment request shapes between two lab accounts. Explicit media holds retain upload shapes without sending uploads; normal chat messages still exercise delivery.

use std::io::Write as _;
use std::time::Duration;

use flate2::Compression;
use flate2::write::ZlibEncoder;

use crate::browser::Media;
use crate::capture_hold::{HeldRequest, Hold};
use crate::config::{Alias, LabConfig, LabLocations, LabPaths};
use crate::session::{LabSession, SessionOptions};

use super::ab_chat::{
    COMPOSE, COMPOSE_CSS, DELIVERY, chat_link, chat_requests, load, nonce, one_to_one_id, save_screenshot_as, send,
    setup, wait_for_newest,
};
use super::ab_chat_full::{CHAT_OPEN, PAGE_CALL, message_action};
use super::dsl::{Driver, RequestPattern, StepError, js_string};
use super::{RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// The label of the capture hold of every write to the media service ([`hold_media_writes`]).
pub const MEDIA_WRITE: &str = "a media service write";

/// A write to an AMS object, on any host (the media service's own, `*.asm.skype.com`, or the web client's origin, which
/// the bundle routes `/asm/<region>/v1/objects` and `/ams/v1/objects` through): its creation (`POST …/v1/objects/`),
/// its content (`PUT …/v1/objects/<id>/content/<kind>`), a forward (`POST …/v1/objects/<id>/forward`), and any other
/// method on an object.
pub const MEDIA_OBJECT: &str = r"(?i)/v[0-9]+/objects(/|\?|$)";

/// The methods [`hold_media_writes`] denies on [`MEDIA_OBJECT`]: every one but a read.
pub const MEDIA_METHODS: [&str; 4] = ["POST", "PUT", "PATCH", "DELETE"];

/// How long the client gets to try to create the image's object once the image is in the compose box or sent.
pub const MEDIA_WAIT: Duration = Duration::from_secs(60);

/// How long the run watches after the first media write denied, and after a's page loads again.
pub const SETTLE: Duration = Duration::from_secs(30);

/// How long the compose box gets to show a pasted image.
const PASTE_WAIT: Duration = Duration::from_secs(15);

/// The attachment, upload and observation intervals of the attachment scenarios.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AttachmentWaits {
    pub(super) attach: Duration,
    pub(super) upload: Duration,
    pub(super) settle: Duration,
}

impl Default for AttachmentWaits {
    fn default() -> Self {
        Self {
            attach: PASTE_WAIT,
            upload: MEDIA_WAIT,
            settle: SETTLE,
        }
    }
}

/// The page expression that marks the newest message showing an image whose text holds `caption` as
/// [`super::ab_chat_full::MARKED`] (the mark taken off any other), `true` when there is one.
///
/// With this run's caption ([`Texts::caption`], which holds the run's nonce) such a message can only be this run's
/// image left unsent: every media write of the run is denied, so its image never reached the media service. Any other
/// message showing an image, delivered or an earlier run's, is never marked, so it is never deleted.
#[must_use]
pub fn mark_unsent_image(caption: &str) -> String {
    let caption = js_string(caption);
    format!(
        r#"(() => {{
  for (const e of document.querySelectorAll('[data-lab-mark]')) e.removeAttribute('data-lab-mark');
  const found = [...document.querySelectorAll('[data-tid="chat-pane-message"]')]
    .filter((e) => (e.innerText ?? '').includes({caption}))
    .filter((e) => e.querySelector('img[src^="blob:"], img[src^="data:"], img[src*="/v1/objects/"]'));
  const newest = found[found.length - 1];
  if (!newest) return false;
  newest.setAttribute('data-lab-mark', '');
  return true;
}})()"#
    )
}

/// The test image's side, in pixels.
const SIDE: u32 = 48;

/// What a's messages say; each holds the run's nonce.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texts {
    /// The plain message b shows.
    pub plain: String,
    /// The text sent with the image.
    pub caption: String,
    /// The image's file name.
    pub file_name: String,
}

impl Texts {
    #[must_use]
    pub fn new(nonce: &str) -> Self {
        Self {
            plain: format!("lab image {nonce} plain"),
            caption: format!("lab image {nonce} caption"),
            file_name: format!("lab-image-{nonce}.png"),
        }
    }
}

/// Forces a deny ([`Hold`], labelled [`MEDIA_WRITE`]) on every [`MEDIA_METHODS`] request of `session` whose URL
/// names an AMS object ([`MEDIA_OBJECT`]), whatever its host and body.
///
/// # Errors
/// Never: the pattern is fixed.
pub fn hold_media_writes(session: &LabSession) -> Result<(), ScenarioError> {
    for rule in media_rules().map_err(|error| setup(error.to_string()))? {
        session.hold(rule);
    }
    Ok(())
}

/// The rules [`hold_media_writes`] forces.
///
/// # Errors
/// Never: the pattern is fixed.
pub fn media_rules() -> Result<Vec<Hold>, regex::Error> {
    MEDIA_METHODS
        .iter()
        .map(|method| Ok(Hold::new(method, MEDIA_OBJECT)?.labelled(MEDIA_WRITE)))
        .collect()
}

/// Whether `denial` is a media write the run forced ([`MEDIA_WRITE`]).
#[must_use]
pub fn is_media_write(denial: &HeldRequest) -> bool {
    denial.label.as_deref() == Some(MEDIA_WRITE)
}

/// A made-up PNG, `SIDE` pixels square, of two colours in a pattern of 8-pixel squares, a little different for each
/// `seed`: the scenario's test image, generated rather than taken from anything real.
#[must_use]
pub fn test_png(seed: u8) -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..SIDE {
        // each row starts with its filter type, 0 (none)
        raw.push(0);
        for x in 0..SIDE {
            let light = ((x / 8) + (y / 8)) % 2 == 0;
            let pixel = if light { [0xe8, 0xd0, seed] } else { [0x20, 0x60, 0xa0] };
            raw.extend_from_slice(&pixel);
        }
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    // writing to a Vec doesn't fail
    let compressed = encoder
        .write_all(&raw)
        .and_then(|()| encoder.finish())
        .unwrap_or_default();
    let mut header = Vec::new();
    header.extend_from_slice(&SIDE.to_be_bytes());
    header.extend_from_slice(&SIDE.to_be_bytes());
    // 8 bits per channel, colour type 2 (RGB), deflate, the standard filters, no interlace
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in [(*b"IHDR", header.as_slice()), (*b"IDAT", &compressed), (*b"IEND", &[])] {
        png_chunk(&mut png, kind, data);
    }
    png
}

/// Appends a PNG chunk: its length, kind, data and the CRC-32 of kind and data.
fn png_chunk(png: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) {
    let length = u32::try_from(data.len()).unwrap_or(u32::MAX);
    png.extend_from_slice(&length.to_be_bytes());
    png.extend_from_slice(&kind);
    png.extend_from_slice(data);
    let mut crc = flate2::Crc::new();
    crc.update(&kind);
    crc.update(data);
    png.extend_from_slice(&crc.sum().to_be_bytes());
}

/// The page expression that hands the compose box ([`COMPOSE`]) `png` as a file named `file_name`, as a clipboard
/// paste (`how` `"paste"`) or a drop (`"drop"`) of it ([`super::ab_file::file_event`] of an `image/png`). It answers
/// `true` once dispatched, `false` without a compose box.
#[must_use]
pub fn image_event(png: &[u8], file_name: &str, how: &str) -> String {
    super::ab_file::file_event(png, file_name, "image/png", how)
}

/// The page expression: whether the compose box holds an image.
#[must_use]
pub fn compose_has_image() -> String {
    format!(
        "!!document.querySelector({})?.querySelector('img')",
        js_string(COMPOSE_CSS)
    )
}

/// Runs `ab-image` (see the module docs).
///
/// # Errors
/// When the run can't start: no lab config, no Chrome, a browser whose capture setup fails.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let config = LabConfig::load(&LabLocations::from_env()?)?;
    let session_options = SessionOptions {
        capture: Some(ScenarioName::AbImage.as_str().to_owned()),
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
    let image = Image {
        mri_a: mri(Alias::A),
        mri_b: mri(Alias::B),
        texts: Texts::new(&nonce),

        options,
        waits: AttachmentWaits::default(),
    };

    Ok(finish(a, b, chat_id.as_deref(), &image).await)
}

async fn finish(mut a: LabSession, mut b: LabSession, chat_id: Option<&str>, image: &Image) -> ScenarioReport {
    let mut report = ScenarioReport::new(ScenarioName::AbImage);
    // Arm the rules before either client loads or retries an earlier attachment.
    for denied in [hold_media_writes(&a)] {
        if let Err(error) = denied {
            report.failures.push(error.to_string());
        }
    }

    if let Err(error) = image.run(chat_id, &mut a, &mut b, &mut report).await {
        report.failures.push(error.to_string());
        for (alias, session) in [(Alias::A, &a), (Alias::B, &b)] {
            if let Some(note) = save_screenshot_as(alias, session, "failure.png").await {
                report.notes.push(note);
            }
        }
    }
    let media = a.held_requests().iter().filter(|denial| is_media_write(denial)).count();
    report.notes.push(format!("a: {media} media write(s) denied in all"));
    // both browsers go down and both captures are finished, whatever happened to either
    for (alias, session) in [(Alias::A, a), (Alias::B, b)] {
        report.close_account(alias, session).await;
    }
    report
}

/// One run's accounts, texts and probe.
struct Image {
    mri_a: String,
    mri_b: String,
    texts: Texts,

    options: RunOptions,
    waits: AttachmentWaits,
}

impl Image {
    /// Loading, the enforcement test, a's plain message and image, a's reload, then b's read.
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
        let outcome = self.send_image(a, &mut drive_a, report).await;
        if let Some(note) = save_screenshot_as(Alias::A, a, "after-image.png").await {
            report.notes.push(note);
        }
        outcome?;
        self.reload(a, report).await?;
        self.b_reads(b, &opened, report).await
    }

    /// a types the caption, puts the test image into the compose box (a paste, else a drop) and presses Enter; the
    /// client must try to create the image's object, denied, within [`MEDIA_WAIT`]. Then [`SETTLE`] more, noting the
    /// media writes denied and whether a post with the caption went out.
    async fn send_image(
        &self,
        a: &LabSession,
        drive: &mut Driver,
        report: &mut ScenarioReport,
    ) -> Result<(), ScenarioError> {
        let before = media_writes(a).len();
        drive.skip_seen();
        drive
            .type_into("a types the caption", COMPOSE, &self.texts.caption)
            .await?;
        let taken = put_image(drive, &self.texts.file_name, self.waits.attach).await?;
        report.notes.push(match taken {
            Some(how) => format!("a: the compose box took the image ({how})"),
            None => "a: the compose box showed no image after a paste and a drop".to_owned(),
        });
        let early = media_writes(a).len().saturating_sub(before);
        report
            .notes
            .push(format!("a: {early} media write(s) denied before Enter"));
        drive.press_enter("a sends the image").await?;
        let Some(first) = wait_media_write(a, before, self.waits.upload).await else {
            return Err(setup(format!(
                "the client tried no media write within {}s of the image",
                self.waits.upload.as_secs()
            )));
        };
        report
            .notes
            .push(format!("a: first media write denied: {} {}", first.method, first.url));
        tokio::time::sleep(self.waits.settle).await;
        for denial in &media_writes(a)[before..] {
            report
                .notes
                .push(format!("a: media write denied: {} {}", denial.method, denial.url));
        }
        let caption_post = RequestPattern::new(Some("POST"), super::ab_chat::MESSAGE_POST)
            .map_err(|error| setup(error.to_string()))?
            .with_body_containing(&self.texts.caption);
        let posted = drive
            .wait_for_request("a's caption post", &caption_post, Duration::from_millis(500))
            .await;
        report.notes.push(match posted {
            Ok(seen) => format!(
                "a: a message post with the caption went out: {:?} {:?}",
                seen.status, seen.failed
            ),
            Err(_) => "a: no message post with the caption went out".to_owned(),
        });
        Ok(())
    }

    /// a's web client is loaded again; for [`SETTLE`] the run notes whether it tries the image again.
    async fn reload(&self, a: &mut LabSession, report: &mut ScenarioReport) -> Result<(), ScenarioError> {
        let before = media_writes(a).len();
        load(Alias::A, a, self.options.load_timeout).await?;
        tokio::time::sleep(self.waits.settle).await;
        let again = media_writes(a).len().saturating_sub(before);
        report.notes.push(format!(
            "a: after loading again, {again} media write(s) denied within {}s",
            self.waits.settle.as_secs()
        ));
        let outcome = self.discard_unsent(a).await;
        report.notes.push(match outcome {
            Ok(note) => format!("a: unsent image: {note}"),
            Err(error) => format!("a: unsent image: couldn't delete it: {error}"),
        });
        Ok(())
    }

    /// Deletes, from a's chat with b, this run's image left unsent while its media writes are denied (which the client
    /// would otherwise try again on a later load): the newest message showing an image with this run's caption
    /// ([`mark_unsent_image`]), from its actions, as a deletes a message in `ab-chat-full`. Without one, nothing is
    /// deleted. What it finds and does is noted; it fails nothing.
    async fn discard_unsent(&self, a: &LabSession) -> Result<String, ScenarioError> {
        let mut drive = a.driver();
        drive.set_call_timeout(PAGE_CALL);
        drive
            .navigate("a opens the chat with b again", &chat_link(&self.mri_b))
            .await?;
        drive.wait_for("a's compose box", COMPOSE, CHAT_OPEN).await?;
        let mark = mark_unsent_image(&self.texts.caption);
        let found = drive
            .wait_until_true(
                "a finds this run's unsent image",
                &mark,
                "a message with an image and this run's caption",
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
                "a's unsent image is gone",
                &format!("!{mark}"),
                "no message with an image and this run's caption",
                self.waits.attach,
            )
            .await;
        Ok(match gone {
            Ok(()) => "deleted from its actions".to_owned(),
            Err(_) => "delete picked, but this run's image is still shown".to_owned(),
        })
    }

    /// b opens the chat with a, shows a's plain message, and must not have the probe; whether it shows the caption is
    /// noted.
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

        let caption = drive.shows_text("b shows the caption", &self.texts.caption).await?;
        report.notes.push(format!("b: shows the caption: {caption}"));
        Ok(())
    }
}

/// Puts the test image ([`test_png`]) into the compose box as `file_name`: a paste, else a drop; which one the editor
/// took, or `None` when it showed no image after either.
///
/// # Errors
/// When the compose box is gone, or the page doesn't answer.
async fn put_image(
    drive: &mut Driver,
    file_name: &str,
    attach_wait: Duration,
) -> Result<Option<&'static str>, ScenarioError> {
    let png = test_png(0x40);
    for how in ["paste", "drop"] {
        let event = image_event(&png, file_name, how);
        if drive.evaluate(&format!("a's image ({how})"), &event).await? != serde_json::Value::Bool(true) {
            return Err(setup("a's compose box went away before the image".to_owned()));
        }
        match drive
            .wait_until_true(
                &format!("a's compose box shows the image ({how})"),
                &compose_has_image(),
                "an image in the compose box",
                attach_wait,
            )
            .await
        {
            Ok(()) => return Ok(Some(how)),
            Err(StepError::Timeout { .. }) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

/// `session`'s media writes denied so far, in order.
fn media_writes(session: &LabSession) -> Vec<HeldRequest> {
    session.held_requests().into_iter().filter(is_media_write).collect()
}

/// Waits up to `timeout` for a media write denied after the first `before`; the first such one.
async fn wait_media_write(session: &LabSession, before: usize, timeout: Duration) -> Option<HeldRequest> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(denial) = media_writes(session).into_iter().nth(before) {
            return Some(denial);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(test)]
#[path = "../../tests/support/scenario/ab_image_driver_tests.rs"]
mod driver_tests;
