//! A message as the native client writes it ([`MessageText`]): one typed rich-text value ([`RichText`], the model reads
//! give) and the MRI of each mention in it, from which both the HTML `content` and `properties.mentions` (and a reply's
//! `properties.qtdMsgs`) are built, so a mention can't be in the markup without being in the request's target.
//!
//! Raw HTML can't be passed: `content` is always rendered here, from the model, in the forms `docs/protocol/chat.md`
//! records. What a message renders to:
//!
//! - text runs escaped, their marks as `b`, `i`, `u`, `s`, `code` (outermost first); `br`; a paragraph as `p`;
//! - a link as `<a href>`, its `href` only `http`, `https` or `mailto`;
//! - a reply's quote as the captured reply (`reply.json`): the `blockquote` of itemtype Reply naming the quoted message,
//!   its sender and its time, with a `preview` paragraph of the quote's text, plus a `qtdMsgs` entry; a quote that
//!   names no message as a plain `blockquote`;
//! - a mention in the web bundle's form ([`mention_span`], [`mention_entry`]): **not captured yet**
//!   (`docs/backlog/mention-not-captured-under-the-fence.md`); those two functions are the only place it is built, so
//!   a captured form replaces them alone;
//! - an image is refused: sending one is an attachment (M1 Task 20 decides), not a message's text.
//!
//! Every mentioned MRI is a participant of the message ([`MessageText::participants`]), so the fence judges them
//! together with the chat's members, exactly as `docs/protocol/fence-targets.md` maps a message: a quote adds nobody
//! (the quoted message is the chat's own).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use teams_fence::Mri;
use url::Url;

use crate::chat::html::{MAX_CONTENT, MAX_DEPTH, MENTION_ITEMTYPE, Marks, Node, Quote, REPLY_ITEMTYPE, RichText};

/// Why a [`MessageText`] can't be built.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ComposeError {
    /// A mention in the text whose `item_id` names no mentioned MRI.
    #[error("the mention {0:?} names nobody")]
    UnknownMention(String),
    /// A mentioned MRI whose `item_id` no mention in the text has.
    #[error("the mention {0:?} isn't in the text")]
    UnusedMention(String),
    /// An `item_id` given twice, or held by two mentions in the text.
    #[error("the mention {0:?} is there twice")]
    RepeatedMention(String),
    /// A mention of anyone but a work account (`8:orgid:<object id>`): a bot's mention needs its app id, which isn't
    /// modelled.
    #[error("a mention of {0}, which isn't a work account")]
    NotAPerson(Mri),
    /// An image: sending one is an attachment, not a message's text.
    #[error("an image can't be sent in a message's text")]
    Image,
    /// A link whose `href` isn't `http`, `https` or `mailto`.
    #[error("a link that isn't http, https or mailto")]
    LinkScheme,
    /// A quote that names a message but not all of its sender, the sender's name and a time in milliseconds.
    #[error("a reply's quote without its sender, the sender's name or its time")]
    Quote,
    /// A text read only in part ([`RichText::truncated`]).
    #[error("a text that was read only in part")]
    Truncated,
    /// Containers nested deeper than [`MAX_DEPTH`].
    #[error("containers nested deeper than {MAX_DEPTH}")]
    TooDeep,
    /// HTML longer than [`MAX_CONTENT`] bytes.
    #[error("content longer than {MAX_CONTENT} bytes")]
    TooLong,
}

/// A message's text: a [`RichText`] and the MRI each of its mentions names, by the mention's `item_id`. Everything a
/// post or an edit sends of it (`content`, `properties.mentions`, `properties.qtdMsgs`) and the participants its target
/// adds are built once, when it is made, from that one value.
///
/// Mentions are numbered in the order they appear (`itemid` `0`, `1`, …, as the web client numbers them), whatever the
/// `item_id`s given; each mention appears once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Stored", into = "Stored")]
pub struct MessageText {
    text: RichText,
    mentioned: BTreeMap<String, Mri>,
    html: String,
    mentions: Vec<Value>,
    quoted: Vec<Value>,
    participants: BTreeSet<Mri>,
}

impl MessageText {
    /// `text`, whose mentions name the MRIs of `mentioned` by their `item_id`.
    ///
    /// # Errors
    /// See [`ComposeError`]: every mention in `text` must name one entry of `mentioned` and every entry must be named
    /// once; only work accounts are mentioned; no image, no link but `http`, `https` and `mailto`, a reply's quote
    /// complete; the text read in full, at most [`MAX_DEPTH`] deep and its HTML at most [`MAX_CONTENT`] bytes.
    pub fn new(text: RichText, mentioned: impl IntoIterator<Item = (String, Mri)>) -> Result<Self, ComposeError> {
        let mut table = BTreeMap::new();
        for (item_id, mri) in mentioned {
            if !is_work_account(&mri) {
                return Err(ComposeError::NotAPerson(mri));
            }
            if table.insert(item_id.clone(), mri).is_some() {
                return Err(ComposeError::RepeatedMention(item_id));
            }
        }
        if text.truncated {
            return Err(ComposeError::Truncated);
        }
        let mut render = Render::new(&table);
        render.nodes(&text.nodes, 0)?;
        if let Some(unused) = table.keys().find(|item_id| !render.used.contains(item_id.as_str())) {
            return Err(ComposeError::UnusedMention(unused.clone()));
        }
        if render.html.len() > MAX_CONTENT {
            return Err(ComposeError::TooLong);
        }
        let Render {
            html,
            mentions,
            quoted,
            participants,
            ..
        } = render;
        Ok(Self {
            text,
            mentioned: table,
            html,
            mentions,
            quoted,
            participants,
        })
    }

    /// Plain text as one paragraph, no mention.
    ///
    /// # Errors
    /// [`ComposeError::TooLong`] when its HTML is longer than [`MAX_CONTENT`].
    pub fn plain(text: &str) -> Result<Self, ComposeError> {
        let paragraph = RichText {
            nodes: vec![Node::Paragraph(RichText::plain(text).nodes)],
            truncated: false,
        };
        Self::new(paragraph, [])
    }

    /// The text as given.
    #[must_use]
    pub fn rich_text(&self) -> &RichText {
        &self.text
    }

    /// The MRI each mention names, by the `item_id` given.
    #[must_use]
    pub fn mentioned(&self) -> &BTreeMap<String, Mri> {
        &self.mentioned
    }

    /// The HTML `content` it is sent as.
    #[must_use]
    pub fn html(&self) -> &str {
        &self.html
    }

    /// Whom the message mentions. A write of it adds them to its target.
    #[must_use]
    pub fn participants(&self) -> &BTreeSet<Mri> {
        &self.participants
    }

    /// `properties.mentions` as sent: a JSON string of the mention entries.
    pub(crate) fn mentions_property(&self) -> String {
        Value::Array(self.mentions.clone()).to_string()
    }

    /// `properties.qtdMsgs` as sent (a JSON string of the quoted messages), when the text quotes one.
    pub(crate) fn quoted_property(&self) -> Option<String> {
        (!self.quoted.is_empty()).then(|| Value::Array(self.quoted.clone()).to_string())
    }
}

/// How a [`MessageText`] is stored (the outbox's payload): the text and its mention table, checked again when read.
#[derive(Serialize, Deserialize)]
struct Stored {
    text: RichText,
    mentioned: BTreeMap<String, String>,
}

impl TryFrom<Stored> for MessageText {
    type Error = ComposeError;

    fn try_from(stored: Stored) -> Result<Self, ComposeError> {
        Self::new(
            stored.text,
            stored
                .mentioned
                .into_iter()
                .map(|(item_id, mri)| (item_id, Mri::new(mri))),
        )
    }
}

impl From<MessageText> for Stored {
    fn from(text: MessageText) -> Self {
        Self {
            text: text.text,
            mentioned: text
                .mentioned
                .into_iter()
                .map(|(item_id, mri)| (item_id, mri.as_str().to_owned()))
                .collect(),
        }
    }
}

/// A mention's span in `content`, in the web bundle's form (not captured; see the module docs): the name shown, marked
/// with the mention's index in the message.
fn mention_span(index: usize, name: &str) -> String {
    format!(
        "<span itemtype=\"{MENTION_ITEMTYPE}\" itemscope itemid=\"{index}\">{}</span>",
        escape(name)
    )
}

/// A mention's entry in `properties.mentions`, in the web bundle's form (not captured; see the module docs).
fn mention_entry(index: usize, mri: &Mri, name: &str) -> Value {
    json!({
        "@type": MENTION_ITEMTYPE,
        "itemid": index.to_string(),
        "mri": mri.as_str(),
        "mentionType": "person",
        "displayName": name,
    })
}

/// A work account's MRI: `8:orgid:` and an object id (hex digits and dashes).
fn is_work_account(mri: &Mri) -> bool {
    mri.as_str()
        .strip_prefix("8:orgid:")
        .is_some_and(|oid| !oid.is_empty() && oid.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'-'))
}

/// Renders the model into HTML and collects what the properties and the target need.
struct Render<'a> {
    table: &'a BTreeMap<String, Mri>,
    html: String,
    mentions: Vec<Value>,
    used: BTreeSet<&'a str>,
    quoted: Vec<Value>,
    participants: BTreeSet<Mri>,
}

/// The marks in the order they open, with their tags.
const MARK_TAGS: [(Marks, &str); 5] = [
    (Marks::BOLD, "b"),
    (Marks::ITALIC, "i"),
    (Marks::UNDERLINE, "u"),
    (Marks::STRIKE, "s"),
    (Marks::CODE, "code"),
];

impl<'a> Render<'a> {
    fn new(table: &'a BTreeMap<String, Mri>) -> Self {
        Self {
            table,
            html: String::new(),
            mentions: Vec::new(),
            used: BTreeSet::new(),
            quoted: Vec::new(),
            participants: BTreeSet::new(),
        }
    }

    fn push(&mut self, pieces: &[&str]) {
        for piece in pieces {
            self.html.push_str(piece);
        }
    }

    /// Renders `nodes`, inside `depth` containers.
    fn nodes(&mut self, nodes: &[Node], depth: usize) -> Result<(), ComposeError> {
        if depth > MAX_DEPTH {
            return Err(ComposeError::TooDeep);
        }
        for node in nodes {
            self.node(node, depth)?;
        }
        Ok(())
    }

    fn node(&mut self, node: &Node, depth: usize) -> Result<(), ComposeError> {
        match node {
            Node::Text { text, marks } => self.text(text, *marks),
            Node::Break => self.html.push_str("<br>"),
            Node::Paragraph(children) => {
                self.html.push_str("<p>");
                self.nodes(children, depth + 1)?;
                self.html.push_str("</p>");
            }
            Node::Mention { item_id, text } => self.mention(item_id, text)?,
            Node::Link { href, children } => {
                if !is_link(href) {
                    return Err(ComposeError::LinkScheme);
                }
                self.push(&["<a href=\"", &escape(href), "\">"]);
                self.nodes(children, depth + 1)?;
                self.html.push_str("</a>");
            }
            Node::Image(_) => return Err(ComposeError::Image),
            Node::Quote(quote) => self.quote(quote, depth)?,
        }
        Ok(())
    }

    fn text(&mut self, text: &str, marks: Marks) {
        let open: Vec<&str> = MARK_TAGS
            .iter()
            .filter(|(mark, _)| marks.contains(*mark))
            .map(|(_, tag)| *tag)
            .collect();
        for tag in &open {
            self.push(&["<", tag, ">"]);
        }
        self.html.push_str(&escape(text));
        for tag in open.iter().rev() {
            self.push(&["</", tag, ">"]);
        }
    }

    fn mention(&mut self, item_id: &str, name: &str) -> Result<(), ComposeError> {
        let Some((key, mri)) = self.table.get_key_value(item_id) else {
            return Err(ComposeError::UnknownMention(item_id.to_owned()));
        };
        if !self.used.insert(key.as_str()) {
            return Err(ComposeError::RepeatedMention(item_id.to_owned()));
        }
        let index = self.mentions.len();
        self.html.push_str(&mention_span(index, name));
        self.mentions.push(mention_entry(index, mri, name));
        self.participants.insert(mri.clone());
        Ok(())
    }

    /// A reply's quote (it names a message) in the captured form, or a plain quote.
    fn quote(&mut self, quote: &Quote, depth: usize) -> Result<(), ComposeError> {
        let Some(message_id) = &quote.message_id else {
            self.html.push_str("<blockquote>");
            self.nodes(&quote.body, depth + 1)?;
            self.html.push_str("</blockquote>");
            return Ok(());
        };
        let (Some(sender), Some(name), Some(time)) = (&quote.sender, &quote.sender_name, &quote.time) else {
            return Err(ComposeError::Quote);
        };
        let time_ms: u64 = time.parse().map_err(|_| ComposeError::Quote)?;
        within_depth(&quote.body, depth + 1)?;
        let preview = RichText {
            nodes: quote.body.clone(),
            truncated: false,
        }
        .plain_text();
        self.push(&[
            "<blockquote itemscope itemtype=\"",
            REPLY_ITEMTYPE,
            "\" itemid=\"",
            &escape(message_id),
            "\"><strong itemprop=\"mri\" itemid=\"",
            &escape(sender),
            "\">",
            &escape(name),
            "</strong><span itemprop=\"time\" itemid=\"",
            &escape(time),
            "\"></span><p itemprop=\"preview\">",
            &escape(preview.trim_end_matches('\n')),
            "</p></blockquote>",
        ]);
        self.quoted
            .push(json!({"messageId": message_id, "sender": sender, "time": time_ms}));
        Ok(())
    }
}

/// Checks that `nodes`, inside `depth` containers, nest no deeper than [`MAX_DEPTH`], as [`Render::nodes`] checks what
/// it renders: for content that is kept in the text but not rendered (a reply's preview, sent as its plain text), so a
/// text too deep to store and read back is refused when it is made.
fn within_depth(nodes: &[Node], depth: usize) -> Result<(), ComposeError> {
    if depth > MAX_DEPTH {
        return Err(ComposeError::TooDeep);
    }
    for node in nodes {
        match node {
            Node::Paragraph(children) | Node::Link { children, .. } => within_depth(children, depth + 1)?,
            Node::Quote(quote) => within_depth(&quote.body, depth + 1)?,
            Node::Text { .. } | Node::Break | Node::Mention { .. } | Node::Image(_) => {}
        }
    }
    Ok(())
}

/// An `http`, `https` or `mailto` URL.
fn is_link(href: &str) -> bool {
    Url::parse(href).is_ok_and(|url| matches!(url.scheme(), "http" | "https" | "mailto"))
}

/// `text` with `&`, `<`, `>` and `"` escaped, for an element's text or a quoted attribute.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            other => escaped.push(other),
        }
    }
    escaped
}
