//! A message's HTML content read into a small typed model ([`RichText`]): text runs with their marks, line breaks,
//! paragraphs, mentions, quotes, links and images. Parsed by html5ever (through `scraper`), as a browser would, and
//! then walked without recursion.
//!
//! - **Kept**: text, `br`; `p`, `div`, `li`, `h1`–`h6` and `pre` as paragraphs; `b`/`strong`, `i`/`em`, `u`/`ins`,
//!   `s`/`strike`/`del`, `code` as marks; a mention (`<span itemtype="http://schema.skype.com/Mention" itemid="<n>">`,
//!   as captured: inside a `readonly` element as sent, which is unknown markup and so gives its text, bare as read
//!   back); a quote (`<blockquote>`; a reply's, `itemtype="http://schema.skype.com/Reply"`,
//!   with the quoted message's id, its sender's MRI and name and its time, as `docs/protocol/chat.md` shows it); a link
//!   with an `http`, `https` or `mailto` `href`; an image with an `https` `src`.
//! - **Unknown markup is kept as text**: an element this model doesn't name, a link of another scheme or an image of
//!   another source gives its text (an image its `alt`), never its tag or attributes.
//! - **Nothing active survives**: `script`, `style`, `template`, frames, `object`/`embed` and the document's head are
//!   dropped with everything in them, and the model holds no attribute but the few above, so no event handler, style
//!   or `javascript:` URL can reach it.
//! - **Bounded**: containers nested deeper than [`MAX_DEPTH`] give their content to the one above, so the model's depth
//!   (and every walk over it) stays bounded whatever the input; content over [`MAX_CONTENT`] is read up to it.

use ego_tree::iter::Edge;
use scraper::node::Element;
use scraper::{Html, Node as HtmlNode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use crate::chat::model::Field;

/// The deepest the model nests containers (paragraphs, links, quotes, mentions).
pub const MAX_DEPTH: usize = 32;

/// The longest content read, in bytes; a longer one is read up to it ([`RichText::truncated`]). The parse's cost grows
/// with the square of the markup's depth, and a chat message is far shorter than this.
pub const MAX_CONTENT: usize = 64 * 1024;

/// The `itemtype` of a mention's span.
pub const MENTION_ITEMTYPE: &str = "http://schema.skype.com/Mention";

/// The `itemtype` of a reply's quote.
pub const REPLY_ITEMTYPE: &str = "http://schema.skype.com/Reply";

/// Elements dropped with everything in them.
const DROPPED: &[&str] = &[
    "script", "style", "template", "iframe", "frame", "frameset", "object", "embed", "noembed", "noframes", "head",
    "title", "meta", "link", "base",
];

/// Elements read as a paragraph.
const PARAGRAPHS: &[&str] = &["p", "div", "li", "h1", "h2", "h3", "h4", "h5", "h6", "pre"];

/// A message's content as rich text. It is also what a message is written from
/// ([`MessageText`](crate::chat::MessageText)), and serializes for the outbox's stored commands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RichText {
    pub nodes: Vec<Node>,
    /// The content was longer than [`MAX_CONTENT`]: only its first part was read.
    pub truncated: bool,
}

impl RichText {
    /// Plain text as one run.
    #[must_use]
    pub fn plain(text: &str) -> Self {
        let nodes = if text.is_empty() {
            Vec::new()
        } else {
            vec![Node::Text {
                text: text.to_owned(),
                marks: Marks::NONE,
            }]
        };
        Self {
            nodes,
            truncated: false,
        }
    }

    /// The text a reader sees: runs, mentions' names and images' `alt`, a line break per `br` and between paragraphs.
    #[must_use]
    pub fn plain_text(&self) -> String {
        let mut text = String::new();
        plain_text(&self.nodes, &mut text);
        text
    }
}

/// One piece of rich text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Node {
    Text {
        text: String,
        marks: Marks,
    },
    Break,
    Paragraph(Vec<Node>),
    /// A mention: its `itemid` (the index of its entry in `properties.mentions`) and the name shown.
    Mention {
        item_id: String,
        text: String,
    },
    Link {
        href: String,
        children: Vec<Node>,
    },
    Image(Image),
    Quote(Quote),
}

/// An image the content shows. Its source is kept, never fetched here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    pub src: String,
    pub alt: Option<String>,
    /// Its `itemtype`, if it has one (an AMS image's; none was ever captured).
    pub item_type: Option<String>,
}

/// A quote. A reply's names the message it quotes; a plain `blockquote`'s fields are all `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    /// The quoted message's id (the quote's `itemid`).
    pub message_id: Option<String>,
    /// The quoted message's sender (`itemprop="mri"`'s `itemid`).
    pub sender: Option<String>,
    /// The name shown for the sender.
    pub sender_name: Option<String>,
    /// The quoted message's time (`itemprop="time"`'s `itemid`).
    pub time: Option<String>,
    /// What the quote shows: the reply's `itemprop="preview"` paragraph, or a plain quote's content.
    pub body: Vec<Node>,
}

/// The marks on a text run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Marks(u8);

impl Marks {
    pub const NONE: Self = Self(0);
    pub const BOLD: Self = Self(1);
    pub const ITALIC: Self = Self(1 << 1);
    pub const UNDERLINE: Self = Self(1 << 2);
    pub const STRIKE: Self = Self(1 << 3);
    pub const CODE: Self = Self(1 << 4);

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// A thread's event (`ThreadActivity/AddMember`), its XML content read: `<addmember>` with `eventtime`, `initiator`,
/// `rosterVersion` and a `target` per member.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadActivity {
    /// The event's root element, lowercased (`addmember`).
    pub kind: String,
    pub initiator: Option<String>,
    pub targets: Vec<String>,
    /// `rosterVersion`: digits as captured, any other text kept as [`Field::Unknown`] (a JSON string).
    pub roster_version: Field<u64>,
    /// `eventtime` (ms): as `roster_version`.
    pub event_time: Field<u64>,
}

/// Parses a message's HTML content (see the module docs).
#[must_use]
pub fn parse(content: &str) -> RichText {
    let (content, truncated) = bounded(content);
    let html = Html::parse_fragment(content);
    let mut builder = Builder::new();
    for edge in html.tree.root().traverse() {
        match edge {
            Edge::Open(node) => builder.open(node.value()),
            Edge::Close(node) => builder.close(node.value()),
        }
    }
    let mut nodes = builder.root;
    drop_block_gaps(&mut nodes);
    RichText { nodes, truncated }
}

/// Drops the markup whitespace beside a block (a paragraph or a quote): text of ASCII whitespace only, right before
/// or after one, which HTML doesn't render. The service joins a read-back message's paragraphs with CRLF (the
/// `chat-shapes` captures of 2026-10-21, `docs/protocol/chat.md`), which would otherwise read as blank lines. A
/// no-break space is text, and whitespace between inline nodes stays.
fn drop_block_gaps(nodes: &mut Vec<Node>) {
    let is_block = |node: Option<&Node>| matches!(node, Some(Node::Paragraph(_) | Node::Quote(_)));
    let is_gap = |node: &Node| matches!(node, Node::Text { text, .. } if text.chars().all(|c| c.is_ascii_whitespace()));
    let keep: Vec<bool> = (0..nodes.len())
        .map(|index| {
            let before = index.checked_sub(1).and_then(|before| nodes.get(before));
            !(is_gap(&nodes[index]) && (is_block(before) || is_block(nodes.get(index + 1))))
        })
        .collect();
    let mut keep = keep.into_iter();
    nodes.retain(|_| keep.next().unwrap_or(true));
}

/// `content` up to [`MAX_CONTENT`] bytes (cut on a character boundary), and whether it was cut.
fn bounded(content: &str) -> (&str, bool) {
    if content.len() <= MAX_CONTENT {
        return (content, false);
    }
    let mut end = MAX_CONTENT;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    (&content[..end], true)
}

/// Reads a thread event's XML content (up to [`MAX_CONTENT`] bytes, as [`parse`]); `None` when it has no element.
#[must_use]
pub fn thread_activity(content: &str) -> Option<ThreadActivity> {
    let html = Html::parse_fragment(bounded(content).0);
    let mut elements = html.tree.root().descendants().filter(|node| {
        node.value()
            .as_element()
            .is_some_and(|element| element.name() != "html")
    });
    let root = elements.next()?;
    let mut activity = ThreadActivity {
        kind: root.value().as_element()?.name().to_owned(),
        ..ThreadActivity::default()
    };
    for node in root.descendants() {
        let Some(element) = node.value().as_element() else {
            continue;
        };
        let text = || {
            node.descendants()
                .filter_map(|inner| inner.value().as_text().map(|text| &**text))
                .collect::<String>()
                .trim()
                .to_owned()
        };
        match element.name() {
            "initiator" => activity.initiator = Some(text()),
            "target" => activity.targets.push(text()),
            "rosterversion" => activity.roster_version = digits(text()),
            "eventtime" => activity.event_time = digits(text()),
            _ => {}
        }
    }
    Some(activity)
}

/// An element's text as a number: digits only (as captured), anything else kept as it came.
fn digits(text: String) -> Field<u64> {
    match text.parse() {
        Ok(number) if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) => Field::Known(number),
        _ => Field::Unknown(Value::String(text)),
    }
}

/// What an open element became.
enum Frame {
    /// A container: its content is collected on the builder's container stack and made a node when it closes.
    Container,
    /// An element whose content goes to the container above (unknown markup, a mark, a leaf, a container too deep);
    /// the marks it found are restored when it closes.
    Pass { restore: Marks },
}

enum Container {
    Paragraph,
    Link(String),
    Mention(String),
    /// A quote, and whether it is a reply's.
    Quote(Quote, bool),
    /// A reply quote's sender, with the MRI its `itemid` names.
    Sender(Option<String>),
}

/// Builds the model from the parse's open and close edges. No step looks further than the open containers (at most
/// [`MAX_DEPTH`]), so deep input costs no more per element than flat input.
struct Builder {
    /// The content outside every container.
    root: Vec<Node>,
    /// One frame per open element.
    frames: Vec<Frame>,
    /// The open containers, innermost last, each with its content so far.
    containers: Vec<(Container, Vec<Node>)>,
    marks: Marks,
    /// While above 0, the edges are inside a dropped element (the count is how deep).
    skip: usize,
}

impl Builder {
    fn new() -> Self {
        Self {
            root: Vec::new(),
            frames: Vec::new(),
            containers: Vec::new(),
            marks: Marks::NONE,
            skip: 0,
        }
    }

    fn open(&mut self, node: &HtmlNode) {
        match node {
            HtmlNode::Element(_) if self.skip > 0 => self.skip += 1,
            HtmlNode::Element(element) => self.open_element(element),
            HtmlNode::Text(text) if self.skip == 0 => {
                let marks = self.marks;
                push_text(self.children(), text, marks);
            }
            _ => {}
        }
    }

    fn close(&mut self, node: &HtmlNode) {
        if !matches!(node, HtmlNode::Element(_)) {
            return;
        }
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        match self.frames.pop() {
            Some(Frame::Pass { restore }) => self.marks = restore,
            Some(Frame::Container) => {
                if let Some((kind, children)) = self.containers.pop() {
                    self.close_container(kind, children);
                }
            }
            None => {}
        }
    }

    fn open_element(&mut self, element: &Element) {
        let name = element.name();
        if DROPPED.contains(&name) {
            self.skip = 1;
            return;
        }
        if self.reply_quote().is_some() {
            match element.attr("itemprop") {
                Some("mri") => return self.open_container(Container::Sender(attr(element, "itemid"))),
                Some("time") => {
                    if let Some(quote) = self.reply_quote() {
                        quote.time = attr(element, "itemid");
                    }
                    self.skip = 1;
                    return;
                }
                _ => {}
            }
        }
        if let Some(mark) = mark(name) {
            let restore = self.marks;
            self.marks = restore.with(mark);
            self.frames.push(Frame::Pass { restore });
            return;
        }
        match container(element) {
            Some(kind) => self.open_container(kind),
            None => self.open_leaf(element),
        }
    }

    fn open_container(&mut self, kind: Container) {
        if self.containers.len() >= MAX_DEPTH {
            self.frames.push(Frame::Pass { restore: self.marks });
            return;
        }
        self.containers.push((kind, Vec::new()));
        self.frames.push(Frame::Container);
    }

    /// An element that is no container: a line break, an image, or unknown markup, whose text stays.
    fn open_leaf(&mut self, element: &Element) {
        let marks = self.marks;
        match element.name() {
            "br" => self.children().push(Node::Break),
            "img" => match image(element) {
                Some(image) => self.children().push(Node::Image(image)),
                None => {
                    if let Some(alt) = element.attr("alt") {
                        push_text(self.children(), alt, marks);
                    }
                }
            },
            _ => {}
        }
        self.frames.push(Frame::Pass { restore: marks });
    }

    fn close_container(&mut self, kind: Container, mut children: Vec<Node>) {
        drop_block_gaps(&mut children);
        let node = match kind {
            Container::Paragraph => Node::Paragraph(children),
            Container::Link(href) => Node::Link { href, children },
            Container::Mention(item_id) => {
                let mut text = String::new();
                plain_text(&children, &mut text);
                Node::Mention { item_id, text }
            }
            Container::Quote(mut quote, _) => {
                quote.body = children;
                Node::Quote(quote)
            }
            Container::Sender(mri) => {
                let mut name = String::new();
                plain_text(&children, &mut name);
                if let Some(quote) = self.reply_quote() {
                    quote.sender = mri;
                    quote.sender_name = Some(name);
                }
                return;
            }
        };
        self.children().push(node);
    }

    /// The content of the innermost open container, or the root's.
    fn children(&mut self) -> &mut Vec<Node> {
        match self.containers.last_mut() {
            Some((_, children)) => children,
            None => &mut self.root,
        }
    }

    /// The innermost open reply quote.
    fn reply_quote(&mut self) -> Option<&mut Quote> {
        self.containers.iter_mut().rev().find_map(|(kind, _)| match kind {
            Container::Quote(quote, true) => Some(quote),
            _ => None,
        })
    }
}

fn mark(name: &str) -> Option<Marks> {
    match name {
        "b" | "strong" => Some(Marks::BOLD),
        "i" | "em" => Some(Marks::ITALIC),
        "u" | "ins" => Some(Marks::UNDERLINE),
        "s" | "strike" | "del" => Some(Marks::STRIKE),
        "code" => Some(Marks::CODE),
        _ => None,
    }
}

/// The container `element` opens, if it is one.
fn container(element: &Element) -> Option<Container> {
    let name = element.name();
    if PARAGRAPHS.contains(&name) {
        return Some(Container::Paragraph);
    }
    match name {
        "a" => element
            .attr("href")
            .filter(|href| scheme(href).is_some_and(|scheme| matches!(scheme.as_str(), "http" | "https" | "mailto")))
            .map(|href| Container::Link(href.to_owned())),
        "span" if element.attr("itemtype") == Some(MENTION_ITEMTYPE) => {
            Some(Container::Mention(attr(element, "itemid").unwrap_or_default()))
        }
        "blockquote" => {
            let reply = element.attr("itemtype") == Some(REPLY_ITEMTYPE);
            let quote = Quote {
                message_id: if reply { attr(element, "itemid") } else { None },
                ..Quote::default()
            };
            Some(Container::Quote(quote, reply))
        }
        _ => None,
    }
}

/// An image with an `https` source.
fn image(element: &Element) -> Option<Image> {
    let src = element.attr("src")?;
    (scheme(src)? == "https").then(|| Image {
        src: src.to_owned(),
        alt: attr(element, "alt"),
        item_type: attr(element, "itemtype"),
    })
}

/// The scheme of an absolute URL.
fn scheme(text: &str) -> Option<String> {
    Url::parse(text.trim()).ok().map(|url| url.scheme().to_owned())
}

fn attr(element: &Element, name: &str) -> Option<String> {
    element.attr(name).map(str::to_owned)
}

/// Adds a text run, joined to the one before when their marks are the same.
fn push_text(children: &mut Vec<Node>, text: &str, marks: Marks) {
    if text.is_empty() {
        return;
    }
    if let Some(Node::Text {
        text: last,
        marks: before,
    }) = children.last_mut()
        && *before == marks
    {
        last.push_str(text);
        return;
    }
    children.push(Node::Text {
        text: text.to_owned(),
        marks,
    });
}

/// The text `nodes` show (see [`RichText::plain_text`]). Recursive, over a model at most [`MAX_DEPTH`] deep.
fn plain_text(nodes: &[Node], out: &mut String) {
    for node in nodes {
        match node {
            Node::Text { text, .. } | Node::Mention { text, .. } => out.push_str(text),
            Node::Break => out.push('\n'),
            Node::Paragraph(children) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                plain_text(children, out);
                out.push('\n');
            }
            Node::Link { children, .. } => plain_text(children, out),
            Node::Image(image) => out.push_str(image.alt.as_deref().unwrap_or_default()),
            Node::Quote(quote) => plain_text(&quote.body, out),
        }
    }
}
