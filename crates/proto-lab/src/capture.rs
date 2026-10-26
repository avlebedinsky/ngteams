//! The capture recorder: what an armed run saw, as JSONL under `captures/<scenario>/<ts>/` in the lab's data dir,
//! outside the repository. Raw captures hold tokens and real identities; they never enter the repo, not even
//! sanitized. `proto-lab export` ([`crate::sanitize`] and the fixture guard) turns one into a local reference copy,
//! from which an agent writes a synthetic fixture with made-up values.
//!
//! A run records when it is armed with a [`Capture`] ([`crate::arming::ArmedBrowser::arm_capturing`]). The arming then
//! also installs the WebRTC hooks ([`RtcHooks`], `assets/rtc_hooks.js`) in every page and iframe target, the same way
//! and at the same point as the WebSocket guard: through a binding of their own ([`RTC_BINDING`]) and
//! `Page.addScriptToEvaluateOnNewDocument`, before the target's code runs.
//!
//! **The format.** `events.jsonl`, one JSON object per line, in the order recorded; every entry has `seq` (from 1),
//! `ts_ms` (Unix milliseconds) and `type`, and `session` is the CDP session it came from (`null`: the browser):
//! - `cdp`: a protocol event as the browser sent it, `method` and `params`: every `Network.*` event (requests and
//!   responses with their headers, WebSocket creation, handshake, frames sent and received, closing, `EventSource`
//!   messages) of every target, and `Target.attachedToTarget` / `Target.detachedFromTarget`, which say what each
//!   session is.
//! - `request_body`: `request_id`, `postData` (and `base64Encoded`), fetched for a request whose body the event left
//!   out.
//! - `response_body`: `request_id`, `body`, `base64Encoded`, fetched once the response has loaded, for every response
//!   but scripts, stylesheets, images, media, fonts and manifests, and none over [`MAX_BODY`]; or `error` when the
//!   browser no longer had it.
//! - `verdict`: one of the fence's decisions, `verdict` `allow` or `deny`, with its `cause` on a deny. On `channel`
//!   `http`: `request_id` (the `Fetch` id, on that session), `network_id` (the `Network` request id), `method`, `url`,
//!   and on a deny the whole paused `request` (headers and body: a denied write never reaches the network events'
//!   responses, and it is what mapping writes needs). A request pauses on its target's session and the browser's, so
//!   each gets two. On `channel` `web_socket`: the socket's `url`, `socket` and `frame_seq` (the guard's numbers), and
//!   the `frame` (`kind` `text`, `binary` in base64, `unreadable` or `foreign`, and `data`).
//! - `rtc`: what the WebRTC hooks reported, `payload` as the page sent it: `pc` (the connection's number in its
//!   context), `event` (`create` with the configuration, `createOffer`, `createAnswer`, `setLocalDescription`,
//!   `setRemoteDescription` with the description, `addIceCandidate` and `icecandidate` with the candidate, the state
//!   changes, `stats` with every `getStats()` entry, `close`, and `<method>:error`; every data channel the connection
//!   makes (`createDataChannel`) or is given (`datachannel`) with its `label` and `id`, its `dc:open`, and each message
//!   it sends (`dc:send`) or receives (`dc:message`) with its `data`: `text`, or binary as `hex` (its first 4096 bytes)
//!   with its whole `length`; and from the audio tap on every
//!   received audio track, `audio` every [`TAP_INTERVAL`] with the `track` id, the tap's context `state`, its sample
//!   `rate`, the `rms` level of the newest [`TAP_WINDOW`] samples and `tone`, the share of their power at
//!   [`TAP_TONE_HZ`], then `audio:ended`, or `audio:error`), `ts` and `data`; `context` is the execution context.
//! - `arming_failure`: a target the fence couldn't arm, as `fence.log` records it (`target_type`, `step`, `cause`,
//!   `resume_sent`, `held`, `close`, `containment` and `verdict` `breach` or `contained`), plus its `session`,
//!   `target_id` and `error`.
//!
//! A capture made [`Capture::without_request_bodies`] (`proto-lab login`'s) has no request body anywhere: no
//! `request_body` entries, and no `postData` or `postDataEntries` in a request event or a denied request's verdict.
//!
//! The directory is made `0700` and the file `0600`.

use std::collections::HashMap;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::arming::ArmingFailure;
use crate::cdp::{Cdp, Event, SessionId};
use crate::fence_http::{DenyCause, Verdict};
use crate::fence_ws::{GuardRequest, GuardScript, Payload};

/// The capture's file in its directory.
pub const EVENTS_FILE: &str = "events.jsonl";

/// The binding the WebRTC hooks report through. The hooks take it off the global before any page code runs.
pub const RTC_BINDING: &str = "__labCaptureRtc";

/// How often the hooks take a connection's `getStats()` unless told otherwise.
pub const DEFAULT_STATS_INTERVAL: Duration = Duration::from_secs(1);

/// The frequency whose share of the received audio the hooks' audio tap measures: the fake microphone's tone
/// (`echo-call`'s `TONE_HZ`).
pub const TAP_TONE_HZ: u32 = 440;

/// How often the audio tap measures a received audio track.
pub const TAP_INTERVAL: Duration = Duration::from_millis(250);

/// How many of a track's newest samples each measurement reads (the analyser's window: about 170 ms at 48 kHz).
pub const TAP_WINDOW: u32 = 8192;

/// The largest response body fetched, in bytes as sent (CDP's `encodedDataLength`, a float).
pub const MAX_BODY: f64 = 8_388_608.0;

/// Resource types whose bodies are code or media, not protocol, and aren't fetched.
const SKIPPED_TYPES: [&str; 7] = [
    "Script",
    "Stylesheet",
    "Image",
    "Media",
    "Font",
    "Manifest",
    "TextTrack",
];

const HOOKS_TEMPLATE: &str = include_str!("../assets/rtc_hooks.js");

/// The WebRTC hooks of one run.
#[derive(Clone)]
pub struct RtcHooks {
    source: String,
}

impl RtcHooks {
    /// The hooks, taking each connection's stats every `stats_interval` (at least every millisecond).
    #[must_use]
    pub fn new(stats_interval: Duration) -> Self {
        let interval = stats_interval.as_millis().max(1);
        Self {
            source: HOOKS_TEMPLATE
                .replace("{{BINDING}}", RTC_BINDING)
                .replace("{{STATS_INTERVAL_MS}}", &interval.to_string())
                .replace("{{TAP_INTERVAL_MS}}", &TAP_INTERVAL.as_millis().to_string())
                .replace("{{TAP_WINDOW}}", &TAP_WINDOW.to_string())
                .replace("{{TONE_HZ}}", &TAP_TONE_HZ.to_string()),
        }
    }

    /// The script: an expression that installs the hooks in the context it runs in and gives `"hooked"` (or why not).
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

impl Default for RtcHooks {
    fn default() -> Self {
        Self::new(DEFAULT_STATS_INTERVAL)
    }
}

/// Why a capture couldn't be made or written.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("not a scenario name (lowercase letters, digits, - and _, starting with a letter or digit): {0:?}")]
    BadScenario(String),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("the capture lost events: {0}")]
    Lost(String),
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> CaptureError + '_ {
    move |source| CaptureError::Io {
        path: path.to_owned(),
        source,
    }
}

struct Writer {
    file: File,
    seq: u64,
    /// The first write that failed; every entry after it is dropped.
    failure: Option<String>,
}

/// One capture's `events.jsonl`. Each entry is written at once (no buffer), so the file is whole up to the last entry
/// at any time.
pub struct Recorder {
    dir: PathBuf,
    writer: Mutex<Writer>,
}

impl Recorder {
    /// Makes `<captures_dir>/<scenario>/<ts>/` (`<ts>` as [`capture_dir_name`], with `-2`, `-3`, … if taken) and its
    /// `events.jsonl`.
    ///
    /// # Errors
    /// [`CaptureError::BadScenario`] for a scenario name that isn't a plain lowercase name, and [`CaptureError::Io`]
    /// when the directories or the file can't be made.
    pub fn create(captures_dir: &Path, scenario: &str) -> Result<Self, CaptureError> {
        let valid = scenario.len() <= 64
            && scenario
                .bytes()
                .next()
                .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
            && scenario
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_');
        if !valid {
            return Err(CaptureError::BadScenario(scenario.to_owned()));
        }
        let parent = captures_dir.join(scenario);
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&parent)
            .map_err(io_at(&parent))?;
        let dir = unique_dir(&parent, &capture_dir_name(SystemTime::now()))?;
        let path = dir.join(EVENTS_FILE);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(io_at(&path))?;
        Ok(Self {
            dir,
            writer: Mutex::new(Writer {
                file,
                seq: 0,
                failure: None,
            }),
        })
    }

    /// The capture's directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The capture's `events.jsonl`.
    #[must_use]
    pub fn events_file(&self) -> PathBuf {
        self.dir.join(EVENTS_FILE)
    }

    fn writer(&self) -> MutexGuard<'_, Writer> {
        self.writer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Appends one entry (an object; anything else goes in as `value`), numbered and timestamped. A failed write is
    /// kept for [`Self::finish`], and every entry after it is dropped.
    pub fn record(&self, entry: Value) {
        let mut fields = match entry {
            Value::Object(fields) => fields,
            other => Map::from_iter([("value".to_owned(), other)]),
        };
        let mut writer = self.writer();
        if writer.failure.is_some() {
            return;
        }
        writer.seq += 1;
        fields.insert("seq".to_owned(), json!(writer.seq));
        fields.insert("ts_ms".to_owned(), json!(unix_ms(SystemTime::now())));
        let mut line = Value::Object(fields).to_string();
        line.push('\n');
        if let Err(error) = writer.file.write_all(line.as_bytes()) {
            writer.failure = Some(error.to_string());
        }
    }

    /// How many entries were written.
    #[must_use]
    pub fn entries(&self) -> u64 {
        self.writer().seq
    }

    /// Syncs the file.
    ///
    /// # Errors
    /// [`CaptureError::Lost`] when an entry couldn't be written, and [`CaptureError::Io`] when the file can't be
    /// synced.
    pub fn finish(&self) -> Result<(), CaptureError> {
        let writer = self.writer();
        if let Some(failure) = &writer.failure {
            return Err(CaptureError::Lost(failure.clone()));
        }
        writer.file.sync_all().map_err(io_at(&self.events_file()))
    }
}

/// Makes `<parent>/<name>`, or `<name>-2`, `<name>-3`, … when it exists.
fn unique_dir(parent: &Path, name: &str) -> Result<PathBuf, CaptureError> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    for attempt in 1..=1000_u32 {
        let dir = match attempt {
            1 => parent.join(name),
            _ => parent.join(format!("{name}-{attempt}")),
        };
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_at(&dir)(error)),
        }
    }
    let taken = io::Error::new(io::ErrorKind::AlreadyExists, "every name is taken");
    Err(io_at(&parent.join(name))(taken))
}

fn unix_ms(time: SystemTime) -> u64 {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

/// A capture directory's name: the UTC time as `YYYYMMDDTHHMMSSZ`.
#[must_use]
pub fn capture_dir_name(time: SystemTime) -> String {
    let seconds = time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (days, of_day) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (of_day / 3600, of_day % 3600 / 60, of_day % 60);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

/// The Gregorian date of a day count since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let shifted = days + 719_468;
    let era = shifted / 146_097;
    let of_era = shifted % 146_097;
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let day_of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

/// What an armed run records, handed to [`crate::arming::ArmedBrowser::arm_capturing`].
pub struct Capture {
    recorder: Arc<Recorder>,
    hooks: RtcHooks,
    /// The resource type of each response not yet loaded, by (session, request id).
    pending: Mutex<HashMap<(Option<SessionId>, String), String>>,
    /// Whether request bodies are recorded ([`Capture::without_request_bodies`]).
    request_bodies: bool,
}

impl Capture {
    #[must_use]
    pub fn new(recorder: Arc<Recorder>, hooks: RtcHooks) -> Self {
        Self {
            recorder,
            hooks,
            pending: Mutex::new(HashMap::new()),
            request_bodies: true,
        }
    }

    /// The same capture, recording no request body: a request's `postData` and `postDataEntries` are taken out of its
    /// `Network.requestWillBeSent` and of a denied request's verdict, and no `request_body` is fetched. `proto-lab
    /// login` records this way: its sign-in form's posts carry the account's password.
    #[must_use]
    pub fn without_request_bodies(mut self) -> Self {
        self.request_bodies = false;
        self
    }

    #[must_use]
    pub fn recorder(&self) -> &Arc<Recorder> {
        &self.recorder
    }

    /// The WebRTC hooks arming installs.
    #[must_use]
    pub fn hooks(&self) -> &RtcHooks {
        &self.hooks
    }

    fn pending(&self) -> MutexGuard<'_, HashMap<(Option<SessionId>, String), String>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records one event of the connection, if it is one the capture keeps, and fetches the bodies it announces.
    pub(crate) fn observe(&self, cdp: &Cdp, event: &Event) {
        let method = event.method.as_str();
        if method.starts_with("Network.") {
            self.record_cdp(event);
            self.follow_network(cdp, event);
        } else if matches!(method, "Target.attachedToTarget" | "Target.detachedFromTarget") {
            self.record_cdp(event);
        } else if method == "Runtime.bindingCalled" && event.params["name"] == RTC_BINDING {
            let payload = &event.params["payload"];
            let parsed = payload
                .as_str()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .unwrap_or_else(|| payload.clone());
            self.recorder.record(json!({
                "type": "rtc",
                "session": session(event),
                "context": event.params["executionContextId"],
                "payload": parsed,
            }));
        }
    }

    fn record_cdp(&self, event: &Event) {
        let mut params = event.params.clone();
        if !self.request_bodies && event.method == "Network.requestWillBeSent" {
            strip_body(&mut params["request"]);
        }
        self.recorder.record(json!({
            "type": "cdp",
            "session": session(event),
            "method": event.method,
            "params": params,
        }));
    }

    fn follow_network(&self, cdp: &Cdp, event: &Event) {
        let params = &event.params;
        let Some(id) = params["requestId"].as_str() else {
            return;
        };
        let key = (event.session_id.clone(), id.to_owned());
        match event.method.as_str() {
            "Network.requestWillBeSent" => {
                let request = &params["request"];
                if self.request_bodies && request["hasPostData"] == true && request.get("postData").is_none() {
                    self.fetch(cdp, event, id, "Network.getRequestPostData", "request_body");
                }
            }
            "Network.responseReceived" => {
                let kind = params["type"].as_str().unwrap_or("Other").to_owned();
                self.pending().insert(key, kind);
            }
            "Network.loadingFinished" => {
                let kind = self.pending().remove(&key);
                let skipped = kind.as_deref().is_some_and(|kind| SKIPPED_TYPES.contains(&kind));
                let large = params["encodedDataLength"].as_f64().unwrap_or_default() > MAX_BODY;
                if !skipped && !large {
                    self.fetch(cdp, event, id, "Network.getResponseBody", "response_body");
                }
            }
            "Network.loadingFailed" => {
                self.pending().remove(&key);
            }
            _ => {}
        }
    }

    /// Fetches a body in the background and records it (or why it couldn't be had) as an entry of `kind`.
    fn fetch(&self, cdp: &Cdp, event: &Event, id: &str, method: &'static str, kind: &'static str) {
        let (cdp, recorder) = (cdp.clone(), Arc::clone(&self.recorder));
        let (session_id, id) = (event.session_id.clone(), id.to_owned());
        tokio::spawn(async move {
            let outcome = cdp.call(session_id.as_ref(), method, json!({"requestId": id})).await;
            let mut entry = json!({
                "type": kind,
                "session": session_id.as_ref().map(SessionId::as_str),
                "request_id": id,
            });
            match outcome {
                Ok(Value::Object(fields)) => {
                    for (name, value) in fields {
                        entry[name] = value;
                    }
                }
                Ok(_) => {}
                Err(error) => entry["error"] = json!(error.to_string()),
            }
            recorder.record(entry);
        });
    }

    /// Records the fence's decision on one paused request.
    pub(crate) fn http_verdict(&self, event: &Event, verdict: Verdict) {
        let params = &event.params;
        let request = &params["request"];
        let mut entry = json!({
            "type": "verdict",
            "channel": "http",
            "session": session(event),
            "request_id": params["requestId"],
            "network_id": params["networkId"],
            "method": request["method"],
            "url": request["url"],
        });
        set_verdict(&mut entry, verdict);
        if matches!(verdict, Verdict::Deny(_)) {
            entry["request"] = request.clone();
            if !self.request_bodies {
                strip_body(&mut entry["request"]);
            }
        }
        self.recorder.record(entry);
    }

    /// Records a paused request or response the fence couldn't answer (the browser refused the answer): its stage,
    /// method, URL, the response's status or error, and the browser's error. Such a request stays paused until the page
    /// gives it up; nothing of it was sent past the pause.
    pub(crate) fn unanswered(&self, event: &Event, error: &str) {
        let mut entry = paused_entry("unanswered", event);
        entry["error"] = json!(error);
        self.recorder.record(entry);
    }

    /// Records a response the fence paused at the response stage and continued (the login origin's): its method, URL
    /// and status or error.
    pub(crate) fn continued(&self, event: &Event) {
        self.recorder.record(paused_entry("response_continued", event));
    }

    /// Records a target the fence couldn't arm, as `fence.log` does, plus its session, target id and error.
    pub(crate) fn arming_failure(&self, session: Option<&SessionId>, failure: &ArmingFailure) {
        let record = failure.record();
        self.recorder.record(json!({
            "type": "arming_failure",
            "session": session.map(SessionId::as_str),
            "target_id": failure.target_id,
            "target_type": record.target_type,
            "step": record.step,
            "cause": record.cause,
            "resume_sent": record.resume_sent,
            "held": record.held,
            "close": record.close,
            "containment": record.containment,
            "verdict": if record.breach() { "breach" } else { "contained" },
            "error": failure.error,
        }));
    }

    /// Records the fence's decision on one WebSocket frame the guard asked about.
    pub(crate) fn ws_verdict(&self, event: &Event, script: &GuardScript, verdict: Verdict) {
        let request = event.params["payload"]
            .as_str()
            .and_then(|payload| GuardRequest::from_payload(payload, script).ok());
        let mut entry = json!({"type": "verdict", "channel": "web_socket", "session": session(event)});
        match request {
            Some(request) => {
                let (kind, data) = match &request.frame.payload {
                    Payload::Text(text) => ("text", text.as_str()),
                    Payload::Binary(base64) => ("binary", base64.as_str()),
                    Payload::Unreadable => ("unreadable", ""),
                    Payload::Foreign => ("foreign", ""),
                };
                entry["url"] = json!(request.frame.url);
                entry["socket"] = json!(request.socket);
                entry["frame_seq"] = json!(request.seq);
                entry["frame"] = json!({"kind": kind, "data": data});
            }
            None => entry["malformed"] = json!(true),
        }
        set_verdict(&mut entry, verdict);
        self.recorder.record(entry);
    }
}

/// Takes a CDP `Request`'s body out (`postData`, `postDataEntries`); `hasPostData` stays, so the capture still says
/// there was one.
fn strip_body(request: &mut Value) {
    if let Some(request) = request.as_object_mut() {
        request.remove("postData");
        request.remove("postDataEntries");
    }
}

/// A capture entry of `kind` for a paused request or response: its session, ids, stage, method, URL and the response's
/// status or error (no header, no body).
fn paused_entry(kind: &str, event: &Event) -> Value {
    let params = &event.params;
    let request = &params["request"];
    let stage = if params.get("responseStatusCode").is_some() || params.get("responseErrorReason").is_some() {
        "response"
    } else {
        "request"
    };
    json!({
        "type": kind,
        "session": session(event),
        "request_id": params["requestId"],
        "network_id": params["networkId"],
        "stage": stage,
        "method": request["method"],
        "url": request["url"],
        "status": params["responseStatusCode"],
        "response_error": params["responseErrorReason"],
    })
}

fn session(event: &Event) -> Value {
    event
        .session_id
        .as_ref()
        .map_or(Value::Null, |session| json!(session.as_str()))
}

fn set_verdict(entry: &mut Value, verdict: Verdict) {
    match verdict {
        Verdict::Allow => entry["verdict"] = json!("allow"),
        Verdict::Deny(cause) => {
            entry["verdict"] = json!("deny");
            entry["cause"] = cause_value(cause);
        }
    }
}

/// A deny's cause as the capture names it.
fn cause_value(cause: DenyCause) -> Value {
    match cause {
        DenyCause::Unclassified => json!("unclassified"),
        DenyCause::LogUnavailable => json!("log_unavailable"),
        DenyCause::Malformed => json!("malformed"),
        DenyCause::ForeignSocket => json!("foreign_socket"),
        DenyCause::Forced => json!("forced"),
        DenyCause::Fenced(reason) => json!({"fenced": serde_json::to_value(reason).unwrap_or(Value::Null)}),
    }
}
