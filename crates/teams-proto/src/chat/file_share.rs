//! A file shared on to another chat or channel, as Teams' "Share this file" does it (`docs/protocol/chat.md`, "Files:
//! shared on"): one ordinary post to each recipient's conversation, its text the optional message and its
//! `properties.files` the file's entry again ([`share_entry`]). The post is all that is written: Teams makes no grant
//! and no new link for it, and the recipients open the file by its sharing link.
//!
//! - **The entry** is the received one with what Teams resets in a share: `fileInfo.itemId`, `fileChicletState.state`,
//!   `state`, `altText`, `progressComplete`, `sharepointIds`, `publication` and `site` null, `isUploadError` false,
//!   and `permissionScope` the scope of the file's sharing link as the dialog read it ([`LinkPermission`]).
//! - **Share outside Teams** (the dialog's "…") and the Settings of "Link created" open SharePoint's own sharing
//!   for the file in the browser: OneDrive's page of the file ([`onedrive_page`]), whose Share offers Share, Copy link
//!   and Manage access. Teams opens SharePoint's share page in a frame with a token it posts, which a browser can't be
//!   given (lab, 2026-11-18: the same page opened by its address refuses even the file's owner).
//! - **The link's permission** is read on the file's site (`<site>_api/v2.0/shares/u!<link>/permission`, the sharing
//!   token as Graph's: [`share_token`]). The dialog shows its words under the file's name ("Anyone with the link can
//!   edit"); its scope goes into the entry.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value};

use super::files::{SharedFile, own_cloud_of, shared_files_in};

/// The longest permission description kept, in characters.
const MAX_DESCRIPTION: usize = 200;

/// The keys a share sets to null, as Teams' share posts them.
const RESET: [&str; 6] = [
    "altText",
    "progressComplete",
    "sharepointIds",
    "publication",
    "site",
    "state",
];

/// The entry of `file` as a share of it to another conversation posts it (see the module docs), on `cloud`'s file
/// service; `None` when the file came with no entry or its entry doesn't read as a linked file there. `scope` is its
/// sharing link's scope, when the dialog read one.
#[must_use]
pub fn share_entry(cloud: &crate::Cloud, file: &SharedFile, scope: Option<&str>) -> Option<Value> {
    let mut entry: Map<String, Value> = serde_json::from_str(file.entry.as_deref()?).ok()?;
    for key in RESET {
        entry.insert(key.to_owned(), Value::Null);
    }
    entry.insert("isUploadError".to_owned(), Value::Bool(false));
    if let Some(info) = entry.get_mut("fileInfo").and_then(Value::as_object_mut) {
        info.insert("itemId".to_owned(), Value::Null);
    }
    if let Some(chiclet) = entry.get_mut("fileChicletState").and_then(Value::as_object_mut) {
        chiclet.insert("state".to_owned(), Value::Null);
    }
    match scope.filter(|scope| is_scope(scope)) {
        Some(scope) => entry.insert("permissionScope".to_owned(), Value::from(scope)),
        None => entry.remove("permissionScope"),
    };
    let entry = Value::Object(entry);
    is_share_entry_in(cloud, &entry).then_some(entry)
}

/// [`share_entry`] for a view that knows no account: on Microsoft's file service, else on the own server its links
/// name. The engine checks the entry again against its account's cloud before it posts it.
#[must_use]
pub fn share_entry_any(file: &SharedFile, scope: Option<&str>) -> Option<Value> {
    share_entry(&crate::Cloud::Microsoft, file, scope).or_else(|| {
        let entry: Map<String, Value> = serde_json::from_str(file.entry.as_deref()?).ok()?;
        share_entry(&own_cloud_of(&entry)?, file, scope)
    })
}

/// OneDrive's page of `file` (see the module docs): `<site>_layouts/15/onedrive.aspx?id=<its path>&parent=<its
/// folder's>`, its path the one of its own address on a personal site of Microsoft's file service; `None` for any
/// other file (an own server's has no such page).
#[must_use]
pub fn onedrive_page(file: &SharedFile) -> Option<String> {
    let object = file.object_url.as_deref()?;
    let site = crate::DriveSite::from_web_url(object)?;
    let url = url::Url::parse(object).ok()?;
    let path = percent_encoding::percent_decode_str(url.path()).decode_utf8().ok()?;
    let (folder, name) = path.rsplit_once('/')?;
    if name.is_empty() || folder.is_empty() {
        return None;
    }
    // as OneDrive writes them: every character but a letter, a digit, `-`, `_` and `.` escaped, a space as `%20`
    let escaped = |text: &str| percent_encoding::utf8_percent_encode(text, &PAGE_QUERY).to_string();
    Some(format!(
        "{}_layouts/15/onedrive.aspx?id={}&parent={}",
        site.base(),
        escaped(&path),
        escaped(folder)
    ))
}

/// What [`onedrive_page`] escapes in its query.
const PAGE_QUERY: percent_encoding::AsciiSet = percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.');

/// Whether `entry` is one a share posts on `cloud`: one file's entry, read with a link on that cloud's file service.
#[must_use]
pub fn is_share_entry_in(cloud: &crate::Cloud, entry: &Value) -> bool {
    if !entry.is_object() {
        return false;
    }
    let read = shared_files_in(cloud, Some(&Value::Array(vec![entry.clone()])));
    read.unread.is_none() && read.files.len() == 1
}

/// The sharing token of a sharing link: `u!` and the link in base64url without padding, as Graph and SharePoint name
/// a shared item by its link.
#[must_use]
pub fn share_token(share_url: &str) -> String {
    format!("u!{}", URL_SAFE_NO_PAD.encode(share_url.as_bytes()))
}

/// Who a file's sharing link reaches and what they may do, as the share dialog shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkPermission {
    /// The link's scope: `anonymous` (anyone), `organization`, `users` (people chosen) or `existingAccess`.
    pub scope: String,
    /// What the link lets people do, in the service's words ("Anyone with the link can edit"), when it gives them.
    pub description: Option<String>,
}

/// Whether `scope` is a sharing link's scope the service names.
fn is_scope(scope: &str) -> bool {
    matches!(scope, "anonymous" | "organization" | "users" | "existingAccess")
}

/// Reads the answer of a sharing link's permission (`shares/u!<link>/permission`): its `link`'s `scope` and
/// `localizedDescription`; `None` for an answer with no link of a known scope.
#[must_use]
pub fn read_link_permission(body: &[u8]) -> Option<LinkPermission> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let link = body.get("link")?.as_object()?;
    let scope = link.get("scope")?.as_str().filter(|scope| is_scope(scope))?;
    let description = link
        .get("localizedDescription")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|words| !words.is_empty() && !words.chars().any(char::is_control))
        .map(|words| words.chars().take(MAX_DESCRIPTION).collect());
    Some(LinkPermission {
        scope: scope.to_owned(),
        description,
    })
}
