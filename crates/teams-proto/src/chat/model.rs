//! The chat service's answers as the native client reads them (`docs/protocol/chat.md`): the conversation list, message
//! pages, messages with their versions, edits, deletes, reactions, mentions and quotes, threads and member lists.
//!
//! **Reading never fails on what this build doesn't know.** Every object keeps the fields it doesn't name (`extra`,
//! `serde(flatten)`); a message type, a conversation kind or a mention kind it doesn't name is `Unknown` with the text
//! it came as; a property in a form no capture showed is [`Field::Unknown`] with its value, never guessed at. A list
//! entry that isn't even the shape every captured one had (no `id`, a known field of another JSON type) is kept as it
//! came in the page's `unreadable`, and the rest of the page is read.
//!
//! Numbers the service writes sometimes as numbers and sometimes as strings of digits (a message's `id` and `version`,
//! `sequenceId`, a quoted message's `messageId`) are read in either form.

use serde::de::{DeserializeOwned, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use teams_fence::{is_group_chat_id, one_to_one_pair};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::chat::html::{self, RichText, ThreadActivity};

/// A value that may be missing, in a form a capture showed ([`Field::Known`]), or in another one, kept as it came
/// ([`Field::Unknown`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Field<T> {
    Absent,
    Known(T),
    Unknown(Value),
}

impl<T> Field<T> {
    /// The known value, if there is one.
    #[must_use]
    pub fn known(self) -> Option<T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Absent | Self::Unknown(_) => None,
        }
    }
}

/// The conversation list (`GET v1/users/ME/conversations`, or its `syncState` link followed).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConversationList {
    pub conversations: Vec<Conversation>,
    /// Entries that aren't the shape of a conversation, kept as they came.
    pub unreadable: Vec<Value>,
    /// `_metadata` as it came: `totalCount`, `forwardLink`, `syncState`, and anything else.
    pub metadata: Map<String, Value>,
    /// The answer's other top-level fields.
    pub extra: Map<String, Value>,
}

impl ConversationList {
    /// Reads an answer's body: a JSON object, its `conversations` a list (or missing) and its `_metadata` an object
    /// (or missing).
    ///
    /// # Errors
    /// [`Malformed`] for any other body.
    pub fn from_body(body: &[u8]) -> Result<Self, Malformed> {
        const WHAT: &str = "a conversation list";
        let mut top = object_body(body, WHAT)?;
        let (conversations, unreadable) = entries(top.remove("conversations"), WHAT)?;
        Ok(Self {
            conversations,
            unreadable,
            metadata: metadata(&mut top, WHAT)?,
            extra: top,
        })
    }

    /// `_metadata.totalCount`.
    #[must_use]
    pub fn total_count(&self) -> Field<u64> {
        number_field(self.metadata.get("totalCount"))
    }

    /// `_metadata.syncState`: the full URL of the next incremental read, followed only on the same chat base.
    #[must_use]
    pub fn sync_state(&self) -> Option<&str> {
        link(&self.metadata, "syncState")
    }

    /// `_metadata.forwardLink`, the next page, when it isn't empty (an empty one was all a capture showed).
    #[must_use]
    pub fn forward_link(&self) -> Option<&str> {
        link(&self.metadata, "forwardLink")
    }
}

/// A conversation as the list gives it.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Conversation {
    pub id: String,
    /// The conversation's version (ms): raised by every change.
    #[serde(default, deserialize_with = "lenient_number")]
    pub version: Option<u64>,
    #[serde(rename = "lastUpdatedMessageId", default, deserialize_with = "lenient_number")]
    pub last_updated_message_id: Option<u64>,
    #[serde(rename = "lastUpdatedMessageVersion", default, deserialize_with = "lenient_number")]
    pub last_updated_message_version: Option<u64>,
    /// The last message, without its `properties`; `{}` (here `None`) for an empty chat, and so is one that isn't the
    /// shape of a message (the raw value stays in [`Conversation::last_message_raw`]).
    #[serde(rename = "lastMessage", default)]
    pub last_message_raw: Option<Value>,
    /// The account's own membership: `role`, `interest`, `isReader`, … as they came.
    #[serde(rename = "memberProperties", default)]
    pub member_properties: Map<String, Value>,
    /// `addedBy`, `consumptionhorizon`, `isemptyconversation`, … as they came (booleans mostly as strings).
    #[serde(default)]
    pub properties: Map<String, Value>,
    /// `threadType`, `productThreadType`, `rosterVersion`, `topics`, … as they came. A chat's topic wasn't seen in any
    /// capture, so no field names it: a team's name is `spaceThreadTopic`.
    #[serde(rename = "threadProperties", default)]
    pub thread_properties: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Conversation {
    /// What kind of conversation it is, from `threadProperties.threadType` and the id (see [`ConversationKind`]).
    #[must_use]
    pub fn kind(&self) -> ConversationKind {
        ConversationKind::of(&self.id, self.thread_property("threadType"))
    }

    /// Whether the native client only reads it: anything but a 1:1 or a group chat.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.kind().is_read_only()
    }

    /// A thread property that came as a string.
    #[must_use]
    pub fn thread_property(&self, name: &str) -> Option<&str> {
        self.thread_properties.get(name).and_then(Value::as_str)
    }

    /// The last message, if the list gave one of a message's shape.
    #[must_use]
    pub fn last_message(&self) -> Option<Message> {
        let raw = self.last_message_raw.as_ref()?;
        if raw.as_object().is_some_and(Map::is_empty) {
            return None;
        }
        serde_json::from_value(raw.clone()).ok()
    }

    /// The account's own read horizon (`properties.consumptionhorizon`), when it has read anything.
    #[must_use]
    pub fn consumption_horizon(&self) -> Option<ConsumptionHorizon> {
        self.properties
            .get("consumptionhorizon")
            .and_then(Value::as_str)
            .map(ConsumptionHorizon::parse)
    }
}

/// What kind of conversation a thread is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConversationKind {
    /// `threadType` `chat` with a 1:1 chat's id (`19:<oid>_<oid>@unq.gbl.spaces`).
    OneToOne,
    /// `threadType` `chat` with a group chat's id (`19:<id>@thread.v2`, not a meeting's).
    Group,
    /// `threadType` `space`: a team, read as its general channel (its other channels in `topics`). Read-only.
    Channel,
    /// `threadType` `streamofnotifications`: the account's activity feed (`48:notifications`). Read-only.
    Notifications,
    /// Anything else, with the `threadType` it came with (empty when there was none). Read-only: a meeting's chat is
    /// one, since no capture showed its `threadType`.
    Unknown(String),
}

impl ConversationKind {
    /// The kind of conversation `id` whose `threadProperties.threadType` is `thread_type`.
    #[must_use]
    pub fn of(id: &str, thread_type: Option<&str>) -> Self {
        match thread_type {
            Some("chat") if one_to_one_pair(id).is_some() => Self::OneToOne,
            Some("chat") if is_group_chat_id(id) => Self::Group,
            Some("space") => Self::Channel,
            Some("streamofnotifications") => Self::Notifications,
            other => Self::Unknown(other.unwrap_or_default().to_owned()),
        }
    }

    /// Anything but a 1:1 or a group chat.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        !matches!(self, Self::OneToOne | Self::Group)
    }
}

/// A read horizon, `<arrival id>;<ms>;<clientmessageid>`: the last message read and when the horizon was set. A part
/// that isn't the form a capture showed is `None`; `raw` keeps the whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsumptionHorizon {
    /// The last message read: its id (its arrival time, ms).
    pub last_read_arrival: Option<u64>,
    /// When the horizon was set (ms).
    pub updated: Option<u64>,
    /// The last message read's `clientmessageid`.
    pub client_message_id: Option<String>,
    pub raw: String,
}

impl ConsumptionHorizon {
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let parts: Vec<&str> = raw.split(';').collect();
        let (arrival, updated, client) = match parts.as_slice() {
            [arrival, updated, client] => (arrival.parse().ok(), updated.parse().ok(), Some(*client)),
            _ => (None, None, None),
        };
        Self {
            last_read_arrival: arrival,
            updated,
            client_message_id: client.filter(|id| !id.is_empty()).map(str::to_owned),
            raw: raw.to_owned(),
        }
    }
}

/// A page of messages: the first page, a sync read, a backward page or the read around a time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MessagePage {
    /// Newest first, as the service orders them.
    pub messages: Vec<Message>,
    /// Entries that aren't the shape of a message, kept as they came.
    pub unreadable: Vec<Value>,
    /// `_metadata` as it came: `syncState`, `backwardLink`, `forwardLink`, the segment times, and anything else.
    pub metadata: Map<String, Value>,
    /// `tenantId` and any other top-level field.
    pub extra: Map<String, Value>,
}

impl MessagePage {
    /// Reads an answer's body: a JSON object, its `messages` a list (or missing) and its `_metadata` an object (or
    /// missing).
    ///
    /// # Errors
    /// [`Malformed`] for any other body.
    pub fn from_body(body: &[u8]) -> Result<Self, Malformed> {
        const WHAT: &str = "a message page";
        let mut top = object_body(body, WHAT)?;
        let (messages, unreadable) = entries(top.remove("messages"), WHAT)?;
        Ok(Self {
            messages,
            unreadable,
            metadata: metadata(&mut top, WHAT)?,
            extra: top,
        })
    }

    /// `_metadata.syncState`: the full URL of the next sync read.
    #[must_use]
    pub fn sync_state(&self) -> Option<&str> {
        link(&self.metadata, "syncState")
    }

    /// `_metadata.backwardLink`: the older messages (the bundle's name; never captured).
    #[must_use]
    pub fn backward_link(&self) -> Option<&str> {
        link(&self.metadata, "backwardLink")
    }

    /// `_metadata.forwardLink`: the newer messages (the bundle's name; never captured).
    #[must_use]
    pub fn forward_link(&self) -> Option<&str> {
        link(&self.metadata, "forwardLink")
    }

    /// `_metadata.lastCompleteSegmentStartTime` (ms; 0 seen on the read around a time).
    #[must_use]
    pub fn last_complete_segment_start(&self) -> Field<u64> {
        number_field(self.metadata.get("lastCompleteSegmentStartTime"))
    }

    /// `_metadata.lastCompleteSegmentEndTime` (ms).
    #[must_use]
    pub fn last_complete_segment_end(&self) -> Field<u64> {
        number_field(self.metadata.get("lastCompleteSegmentEndTime"))
    }
}

/// A message (`type` `Message`), or a thread's event in the same list.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Message {
    /// Its arrival time (ms) as the service names it.
    #[serde(deserialize_with = "string_or_number")]
    pub id: String,
    /// 1, 2, … in the chat.
    #[serde(rename = "sequenceId", default, deserialize_with = "lenient_number")]
    pub sequence_id: Option<u64>,
    /// Raised by every edit, delete and reaction change (ms).
    #[serde(default, deserialize_with = "lenient_number")]
    pub version: Option<u64>,
    #[serde(rename = "conversationid", default)]
    pub conversation_id: Option<String>,
    #[serde(rename = "messagetype", default)]
    pub message_type: MessageType,
    /// As it came: HTML for `RichText/Html`, XML for a thread's event, empty once deleted.
    #[serde(default, deserialize_with = "string_or_null")]
    pub content: String,
    /// `…/contacts/<MRI>`: see [`Message::sender`].
    #[serde(default)]
    pub from: Option<String>,
    /// The sender's own id for it, kept across edits and deletes; none on a thread's event.
    #[serde(rename = "clientmessageid", default, deserialize_with = "lenient_string")]
    pub client_message_id: Option<String>,
    /// The name the sender's client gave.
    #[serde(rename = "imdisplayname", default)]
    pub display_name: Option<String>,
    /// The name from the sender's token.
    #[serde(rename = "fromDisplayNameInToken", default)]
    pub display_name_in_token: Option<String>,
    /// ISO time, seven fraction digits.
    #[serde(rename = "composetime", default)]
    pub compose_time: Option<String>,
    /// ISO time, seven fraction digits.
    #[serde(rename = "originalarrivaltime", default)]
    pub arrival_time: Option<String>,
    /// Seen only on a deleted message, set to its `clientmessageid`.
    #[serde(rename = "skypeeditedid", default)]
    pub skype_edited_id: Option<String>,
    /// References to the message's AMS objects (images): only `[]` was ever seen, so the entries are kept as they come.
    #[serde(rename = "amsreferences", default)]
    pub ams_references: Option<Vec<Value>>,
    /// `{}`, or a count per reaction key (`{"emotions": {"like": 1}}`).
    #[serde(rename = "annotationsSummary", default)]
    pub annotations_summary: Option<Value>,
    /// `mentions`, `files`, `emotions`, `edittime`, `deletetime`, `qtdMsgs`, … as they came.
    #[serde(default)]
    pub properties: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Message {
    /// The sender's MRI: the last segment of `from` (`…/contacts/<MRI>`). For a thread's event it is the thread's id.
    #[must_use]
    pub fn sender(&self) -> Option<&str> {
        let from = self.from.as_deref()?;
        let (_, last) = from.rsplit_once("/contacts/")?;
        (!last.is_empty() && !last.contains('/')).then_some(last)
    }

    /// `originalarrivaltime` in ms since the epoch.
    #[must_use]
    pub fn arrival_time_ms(&self) -> Option<i64> {
        self.arrival_time.as_deref().and_then(iso_millis)
    }

    /// `composetime` in ms since the epoch.
    #[must_use]
    pub fn compose_time_ms(&self) -> Option<i64> {
        self.compose_time.as_deref().and_then(iso_millis)
    }

    /// `properties.edittime` (ms, set by the service): the message was edited.
    #[must_use]
    pub fn edit_time(&self) -> Option<u64> {
        number_field(self.properties.get("edittime")).known()
    }

    /// `properties.deletetime` (ms): the message was deleted.
    #[must_use]
    pub fn delete_time(&self) -> Option<u64> {
        number_field(self.properties.get("deletetime")).known()
    }

    /// A soft-deleted message: `properties.deletetime` set.
    #[must_use]
    pub fn is_deleted(&self) -> bool {
        self.delete_time().is_some()
    }

    /// `properties.emotions`: one entry per reaction key, a key taken back left with no users.
    #[must_use]
    pub fn reactions(&self) -> Field<Vec<Reaction>> {
        match self.properties.get("emotions") {
            None => Field::Absent,
            Some(value @ Value::Array(_)) => list_of(value),
            Some(other) => Field::Unknown(other.clone()),
        }
    }

    /// `properties.mentions`, read as a JSON string or as a list (the bundle's reader takes both). **Not captured**: the
    /// form is the web bundle's (`docs/protocol/chat.md`), so an entry of another form makes it all
    /// [`Field::Unknown`].
    #[must_use]
    pub fn mentions(&self) -> Field<Vec<Mention>> {
        json_list(self.properties.get("mentions"))
    }

    /// `properties.qtdMsgs`, the messages this one quotes: a JSON string as sent, a list as read back.
    #[must_use]
    pub fn quotes(&self) -> Field<Vec<QuotedMessage>> {
        json_list(self.properties.get("qtdMsgs"))
    }

    /// `properties.files`, a JSON string of a list: received files. Only `"[]"` was ever seen, so the entries are kept
    /// as they come (parsed, never fetched).
    #[must_use]
    pub fn files(&self) -> Field<Vec<Value>> {
        json_list(self.properties.get("files"))
    }

    /// The content as rich text: parsed HTML for `RichText/Html`, the text as it came for `Text`; `None` for other
    /// types (a thread's event, typing, anything unknown).
    #[must_use]
    pub fn rich_text(&self) -> Option<RichText> {
        match self.message_type {
            MessageType::RichTextHtml => Some(html::parse(&self.content)),
            MessageType::Text => Some(RichText::plain(&self.content)),
            _ => None,
        }
    }

    /// A thread's event (`ThreadActivity/…`), its XML content read: who did it, to whom, the roster version.
    #[must_use]
    pub fn thread_activity(&self) -> Option<ThreadActivity> {
        matches!(self.message_type, MessageType::AddMember)
            .then(|| html::thread_activity(&self.content))
            .flatten()
    }
}

/// A message's `messagetype`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(from = "String", into = "String")]
pub enum MessageType {
    /// `Text`: plain text (seen in the notifications stream).
    Text,
    /// `RichText/Html`: what the web client posts.
    RichTextHtml,
    /// `ThreadActivity/AddMember`: members added (a chat's first event).
    AddMember,
    /// `Control/Typing`: typing (seen sent, never in a read).
    Typing,
    /// Any other, as it came (empty when there was none).
    Unknown(String),
}

impl MessageType {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Text => "Text",
            Self::RichTextHtml => "RichText/Html",
            Self::AddMember => "ThreadActivity/AddMember",
            Self::Typing => "Control/Typing",
            Self::Unknown(text) => text,
        }
    }
}

impl Default for MessageType {
    fn default() -> Self {
        Self::Unknown(String::new())
    }
}

impl From<String> for MessageType {
    fn from(text: String) -> Self {
        match text.as_str() {
            "Text" => Self::Text,
            "RichText/Html" => Self::RichTextHtml,
            "ThreadActivity/AddMember" => Self::AddMember,
            "Control/Typing" => Self::Typing,
            _ => Self::Unknown(text),
        }
    }
}

impl From<MessageType> for String {
    fn from(kind: MessageType) -> Self {
        kind.as_str().to_owned()
    }
}

/// One reaction key and who reacted with it.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Reaction {
    pub key: String,
    #[serde(default)]
    pub users: Vec<ReactionUser>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A user's standing reaction.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ReactionUser {
    pub mri: String,
    /// The service's time (ms).
    #[serde(default, deserialize_with = "lenient_number")]
    pub time: Option<u64>,
    /// The value the reacting client sent, as a string.
    #[serde(default, deserialize_with = "lenient_string")]
    pub value: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One entry of `properties.mentions`, in the web bundle's form (not captured). Its `item_id` is the `itemid` of the
/// mention's span in the content ([`crate::chat::Node::Mention`]).
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Mention {
    #[serde(rename = "itemid", deserialize_with = "string_or_number")]
    pub item_id: String,
    #[serde(default)]
    pub mri: Option<String>,
    #[serde(rename = "mentionType", default)]
    pub kind: MentionKind,
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,
    /// `@type`, `appId` (a bot's), and anything else.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A mention's `mentionType`, as the web bundle names them.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(from = "String", into = "String")]
pub enum MentionKind {
    Person,
    Bot,
    Everyone,
    Tag,
    Channel,
    Team,
    /// Any other (the bundle also names `community`, `nearby`, `share-contact`), as it came; empty when there was none.
    Unknown(String),
}

impl MentionKind {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Person => "person",
            Self::Bot => "bot",
            Self::Everyone => "everyone",
            Self::Tag => "tag",
            Self::Channel => "channel",
            Self::Team => "team",
            Self::Unknown(text) => text,
        }
    }
}

impl Default for MentionKind {
    fn default() -> Self {
        Self::Unknown(String::new())
    }
}

impl From<String> for MentionKind {
    fn from(text: String) -> Self {
        match text.as_str() {
            "person" => Self::Person,
            "bot" => Self::Bot,
            "everyone" => Self::Everyone,
            "tag" => Self::Tag,
            "channel" => Self::Channel,
            "team" => Self::Team,
            _ => Self::Unknown(text),
        }
    }
}

impl From<MentionKind> for String {
    fn from(kind: MentionKind) -> Self {
        kind.as_str().to_owned()
    }
}

/// One entry of `properties.qtdMsgs`: a message this one quotes.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct QuotedMessage {
    /// A number as read back, a string as sent.
    #[serde(rename = "messageId", deserialize_with = "string_or_number")]
    pub message_id: String,
    #[serde(default)]
    pub sender: Option<String>,
    #[serde(default, deserialize_with = "lenient_number")]
    pub time: Option<u64>,
    /// `Valid` as read back.
    #[serde(rename = "validationResult", default)]
    pub validation_result: Option<String>,
    /// `message`, `sharedRefId`, `replyChainId`, and anything else.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A thread read (`GET v1/threads/<id>`).
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Thread {
    pub id: String,
    #[serde(default)]
    pub members: Vec<ThreadMember>,
    /// `threadType`, `createdat`, `creator`, `capabilities`, … as they came.
    #[serde(default)]
    pub properties: Map<String, Value>,
    #[serde(rename = "rosterVersion", default, deserialize_with = "lenient_number")]
    pub roster_version: Option<u64>,
    #[serde(default, deserialize_with = "lenient_number")]
    pub version: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Thread {
    /// What kind of conversation it is, from `properties.threadType` and the id.
    #[must_use]
    pub fn kind(&self) -> ConversationKind {
        ConversationKind::of(&self.id, self.properties.get("threadType").and_then(Value::as_str))
    }
}

/// A member of a thread, as the thread read and the member list give it.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ThreadMember {
    /// The member's MRI.
    pub id: String,
    #[serde(default)]
    pub role: Option<String>,
    /// `capabilities`, `userLink`, `rangeStart`, … as they came.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A thread's member list (`GET v1/threads/<id>/members`). Only an unpaged one was ever seen.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct MemberList {
    #[serde(default)]
    pub members: Vec<ThreadMember>,
    #[serde(rename = "totalMemberCount", default, deserialize_with = "lenient_number")]
    pub total_member_count: Option<u64>,
    #[serde(rename = "rosterVersion", default, deserialize_with = "lenient_number")]
    pub roster_version: Option<u64>,
    /// `eTag`, `isDeleted`, any paging key, and anything else.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Why an answer couldn't be read as its model. It names what was expected, never the body.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the chat service's answer isn't the shape of {0}")]
pub struct Malformed(pub &'static str);

/// A single object (a thread, a member list) from a body.
pub(crate) fn object<T: DeserializeOwned>(body: &[u8], what: &'static str) -> Result<T, Malformed> {
    serde_json::from_slice(body).map_err(|_| Malformed(what))
}

fn object_body(body: &[u8], what: &'static str) -> Result<Map<String, Value>, Malformed> {
    match serde_json::from_slice(body) {
        Ok(Value::Object(top)) => Ok(top),
        _ => Err(Malformed(what)),
    }
}

/// The page's list, each entry read on its own: those that aren't the model's shape are kept apart. A missing list is
/// an empty one; a list that isn't a list is malformed.
fn entries<T: DeserializeOwned>(list: Option<Value>, what: &'static str) -> Result<(Vec<T>, Vec<Value>), Malformed> {
    let list = match list {
        None => return Ok((Vec::new(), Vec::new())),
        Some(Value::Array(list)) => list,
        Some(_) => return Err(Malformed(what)),
    };
    let mut read = Vec::new();
    let mut unreadable = Vec::new();
    for entry in list {
        match serde_json::from_value(entry.clone()) {
            Ok(value) => read.push(value),
            Err(_) => unreadable.push(entry),
        }
    }
    Ok((read, unreadable))
}

fn metadata(top: &mut Map<String, Value>, what: &'static str) -> Result<Map<String, Value>, Malformed> {
    match top.remove("_metadata") {
        None => Ok(Map::new()),
        Some(Value::Object(metadata)) => Ok(metadata),
        Some(_) => Err(Malformed(what)),
    }
}

/// A link in `metadata`, when it is a non-empty string.
fn link<'m>(metadata: &'m Map<String, Value>, name: &str) -> Option<&'m str> {
    metadata
        .get(name)
        .and_then(Value::as_str)
        .filter(|link| !link.is_empty())
}

/// A number as the service writes it: a JSON number, or a string of digits.
fn number_field(value: Option<&Value>) -> Field<u64> {
    match value {
        None => Field::Absent,
        Some(value) => number(value).map_or_else(|| Field::Unknown(value.clone()), Field::Known),
    }
}

fn number(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) => text.parse().ok(),
        _ => None,
    }
}

/// A list property, as a list or a JSON string of one; any entry not of `T`'s shape makes it all unknown.
fn json_list<T: DeserializeOwned>(value: Option<&Value>) -> Field<Vec<T>> {
    match value {
        None => Field::Absent,
        Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
            Ok(list @ Value::Array(_)) => list_of(&list).or_unknown(Value::String(text.clone())),
            _ => Field::Unknown(Value::String(text.clone())),
        },
        Some(list @ Value::Array(_)) => list_of(list),
        Some(other) => Field::Unknown(other.clone()),
    }
}

fn list_of<T: DeserializeOwned>(list: &Value) -> Field<Vec<T>> {
    serde_json::from_value(list.clone()).map_or_else(|_| Field::Unknown(list.clone()), Field::Known)
}

impl<T> Field<T> {
    /// An unknown value replaced with `raw` (the form it came in).
    fn or_unknown(self, raw: Value) -> Self {
        match self {
            Self::Unknown(_) => Self::Unknown(raw),
            other => other,
        }
    }
}

/// An ISO time (RFC 3339, any number of fraction digits) in ms since the epoch.
fn iso_millis(text: &str) -> Option<i64> {
    let time = OffsetDateTime::parse(text, &Rfc3339).ok()?;
    i64::try_from(time.unix_timestamp_nanos() / 1_000_000).ok()
}

/// A number or a string of digits, `None` for anything else (never an error).
fn lenient_number<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    Ok(number(&Value::deserialize(deserializer)?))
}

/// A string, or a number as its text; `None` for anything else (never an error).
fn lenient_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(text) => Some(text),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    })
}

/// A string, or a number as its text; anything else is an error (the entry isn't the model's shape).
fn string_or_number<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::String(text) => Ok(text),
        Value::Number(number) => Ok(number.to_string()),
        _ => Err(serde::de::Error::custom("not a string or a number")),
    }
}

/// A string; `null` is an empty one. Anything else is an error.
fn string_or_null<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}
