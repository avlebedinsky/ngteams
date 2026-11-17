//! A channel post that came by email, and a post's subject (`docs/protocol/channels.md`, "A post that came by email").
//!
//! - **The shape** (the web client's reader of a chat service message): `properties.subject` is the post's subject as
//!   text. `properties.emaildetails` is a JSON string of an object: `from` and `sender` (each `{name, smtp}`), `to`
//!   and `cc` (lists of them), `emailLink` (the address of the original `.eml` file in the team's SharePoint) and
//!   `totalRecipients`. A post with `emaildetails` is an email post.
//! - **What is kept**: a recipient with an address or a name; the email link only when it is an `https` link on a
//!   SharePoint host, or on an own server's file service (`/sp/`). The web client shows at most
//!   [`SHOWN_RECIPIENTS`] recipients, the `to` list first, and counts the others.

use serde_json::{Map, Value};
use url::Url;

/// The most recipients the web client names on an email post's "To" line.
pub const SHOWN_RECIPIENTS: usize = 3;

/// The longest text kept of a subject, a name or an address, in characters.
const MAX_TEXT: usize = 1000;

/// A sender or a recipient of an email, as `emaildetails` names one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmailUser {
    pub name: Option<String>,
    pub smtp: Option<String>,
}

impl EmailUser {
    /// The text that names the person: the name, else the address.
    #[must_use]
    pub fn label(&self) -> &str {
        self.name.as_deref().or(self.smtp.as_deref()).unwrap_or_default()
    }

    fn of(value: &Value) -> Option<Self> {
        let user = Self {
            name: text(value.get("name")),
            smtp: text(value.get("smtp")),
        };
        (user.name.is_some() || user.smtp.is_some()).then_some(user)
    }
}

/// What `properties.emaildetails` tells of the email a post came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmailDetails {
    pub from: Option<EmailUser>,
    pub sender: Option<EmailUser>,
    pub to: Vec<EmailUser>,
    pub cc: Vec<EmailUser>,
    /// The original `.eml` file, when its link is one this client may open (see the module docs).
    pub email_link: Option<String>,
    /// How many people the email went to: `totalRecipients`, else the count of `to` and `cc`.
    pub total_recipients: usize,
}

impl EmailDetails {
    /// Reads `properties.emaildetails` (a JSON string of an object, or the object). `None` when there is none or it is
    /// not an object.
    #[must_use]
    pub fn of(value: Option<&Value>) -> Option<Self> {
        let parsed;
        let object = match value? {
            Value::String(text) => {
                parsed = serde_json::from_str::<Value>(text).ok()?;
                parsed.as_object()?
            }
            Value::Object(object) => object,
            _ => return None,
        };
        let users = |key: &str| -> Vec<EmailUser> {
            object
                .get(key)
                .and_then(Value::as_array)
                .map(|list| list.iter().filter_map(EmailUser::of).collect())
                .unwrap_or_default()
        };
        let (to, cc) = (users("to"), users("cc"));
        let counted = to.len() + cc.len();
        let total_recipients = object
            .get("totalRecipients")
            .and_then(Value::as_u64)
            .and_then(|total| usize::try_from(total).ok())
            .map_or(counted, |total| total.max(counted));
        Some(Self {
            from: object.get("from").and_then(EmailUser::of),
            sender: object.get("sender").and_then(EmailUser::of),
            to,
            cc,
            email_link: text(object.get("emailLink")).filter(|link| is_email_link(link)),
            total_recipients,
        })
    }

    /// Reads it from a message's `properties`.
    #[must_use]
    pub fn of_properties(properties: &Map<String, Value>) -> Option<Self> {
        Self::of(properties.get("emaildetails"))
    }

    /// The recipients the "To" line names, as the web client picks them: the `to` list first, then `cc`, at most
    /// [`SHOWN_RECIPIENTS`].
    #[must_use]
    pub fn shown_recipients(&self) -> Vec<&EmailUser> {
        self.to.iter().chain(&self.cc).take(SHOWN_RECIPIENTS).collect()
    }

    /// How many recipients the "To" line does not name.
    #[must_use]
    pub fn more_recipients(&self) -> usize {
        self.total_recipients.saturating_sub(self.shown_recipients().len())
    }
}

/// A post's subject (`properties.subject`), trimmed. `None` when it is empty or absent.
#[must_use]
pub fn subject(properties: &Map<String, Value>) -> Option<String> {
    text(properties.get("subject"))
}

/// Whether `link` is an original email's link that this client may open: an `https` link on a SharePoint host, or a
/// link to an own server's file service.
#[must_use]
pub fn is_email_link(link: &str) -> bool {
    if crate::chat::is_file_link(link) {
        return true;
    }
    Url::parse(link)
        .ok()
        .and_then(|url| crate::Cloud::own(&url.origin().ascii_serialization()).ok())
        .is_some_and(|cloud| crate::chat::is_file_link_in(&cloud, link))
}

fn text(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    (!text.is_empty()).then(|| text.chars().take(MAX_TEXT).collect())
}
