//! The compose box under a chat's messages (M2 Task 13): what the user types becomes a message's text, and every
//! write goes out through the engine ([`Core::write`]), its outbox and the fence; the box builds no request.
//!
//! - **Text** ([`message_text`]): each line a paragraph (Shift-Enter breaks the line, and the web client sent each
//!   line as a paragraph of its own: `docs/protocol/chat.md`, "Lines and links"), blank lines at either end dropped; a
//!   URL typed stays text, as the capture's link inserted with its text went out (plain text, `properties.links`
//!   `"[]"`: `lines.json`'s `unlinked_content`), never an anchor (the web client's anchor carries a `HyperLink` entry
//!   in `links`, which isn't built here); a mention is an atomic token of the kit's text area naming its MRI,
//!   completed after `@` from the chat's members ([`Core::members`], their names from [`Core::person`]) and only from
//!   them: a mention is never read from typed text, only by the MRI the picker put in its token; the picker shows each
//!   member's presence dot (M2 Task 17: the engine is asked about the people offered each time they are shown).
//!   `teams-proto`'s [`MessageText`] renders the HTML, the mentions and the reply's quote from that one value. A send the engine
//!   refuses before it makes a row gives the box its draft back.
//! - **Keys**: Enter sends (the kit's text area hands a plain Enter on, and the box takes it as the send, or the
//!   platform would type it as a line break: M2 Task 7), Shift-Enter breaks the line; with the mention picker open, Up
//!   and Down move in it, Enter or Tab chooses, Escape closes it; Escape cancels a reply or an edit, and with nothing
//!   to cancel is handed on (the chat view moves the focus to its message list); Up in an empty box asks to edit the
//!   account's last message ([`ComposeEvent::EditLast`], which the chat view carries out).
//! - **Reply** ([`Compose::reply_to`]): the quoted message's id, sender, sender's name, time and a one-line preview of
//!   its text, sent as the captured reply's quote (`reply.json`). **Edit** ([`Compose::edit`]): the message's text
//!   loaded back into the box ([`load`]), its mentions as tokens again by their MRI, its quote kept as it was; Enter
//!   sends the edit.
//! - **Typing** ([`Core::transient`], never a stored row): sent while the box has the focus and holds text, at most once
//!   per [`TYPING_EVERY`] per chat (the web client's throttle, M2 Task 10; kept for the app, so a chat opened again
//!   doesn't start it over), never while editing, and only with the "send typing" setting on. Nothing is sent when
//!   typing stops: the web client never sent `Control/ClearTyping`.
//!
//! What the box does is traced (`compose opened` when the box is made, `sent`, `replying`, `editing`, `cancelled`, and
//! `changed` each time what it holds changes), never its text.

use std::collections::HashMap;
use std::hash::BuildHasher;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, Focusable as _, Global,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::Disableable as _;
use gpui_component::button::ButtonVariants;
use gpui_component::input::{
    Enter, Escape, IndentInline, InlineToken, InlineTokenSpan, InputContent, InputEvent, MoveDown, MoveUp, Textarea,
    TextareaState,
};
use gpui_component::{Sizable as _, Size};
use teams_core::{Action, Availability, Transient};
use teams_fence::Mri;
use teams_proto::chat::{ComposeError, Marks, MessageText, MessageType, Node, Quote, RichText, parse_html};
use teams_store::Message;

use crate::bridge::Core;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::presence::{self, Presences};
use crate::settings::SettingsModel;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

/// The compose box's key context.
pub const CONTEXT: &str = "Compose";
/// The shortest gap between two typing signals: the web client's throttle (`typingIndicator.notificationFrequency`,
/// 6 000 ms; M2 Task 10, `docs/protocol/chat.md`, "Typing").
pub const TYPING_EVERY: Duration = Duration::from_secs(6);
/// The most people the mention picker lists.
pub const PICKER_MAX: usize = 8;
/// How far back from the cursor an `@` still opens the picker, in characters.
pub const MENTION_QUERY_MAX: usize = 40;
/// The longest preview a reply's quote carries, in characters.
pub const PREVIEW_MAX: usize = 300;
/// The most rows the box grows to.
pub const MAX_ROWS: usize = 8;

/// A mention in the typed text: where its name stands (bytes) and whom it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftMention {
    pub range: Range<usize>,
    pub mri: String,
}

/// A text run with no marks.
fn text_node(text: &str) -> Node {
    Node::Text {
        text: text.to_owned(),
        marks: Marks::NONE,
    }
}

fn push_text(nodes: &mut Vec<Node>, text: &str) {
    if !text.is_empty() {
        nodes.push(text_node(text));
    }
}

/// Each line's range in `text` (bytes), its `\n` left out.
fn lines(text: &str) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (at, _) in text.match_indices('\n') {
        lines.push(start..at);
        start = at + 1;
    }
    lines.push(start..text.len());
    lines
}

/// The mentions within `line`.
fn mentions_in<'a>(mentions: &'a [DraftMention], line: &Range<usize>) -> impl Iterator<Item = &'a DraftMention> {
    let line = line.clone();
    mentions
        .iter()
        .filter(move |mention| line.start <= mention.range.start && mention.range.end <= line.end)
}

/// Whether `line` shows nothing: whitespace only, no mention.
fn blank(text: &str, line: &Range<usize>, mentions: &[DraftMention]) -> bool {
    text[line.clone()].trim().is_empty() && mentions_in(mentions, line).next().is_none()
}

/// The text typed as the rich text a message is written from, with the MRI each mention names by its `item_id`
/// (`0`, `1`, … in order): each line a paragraph, blank lines at either end dropped, mentions at their ranges, the
/// rest text (a URL too: see the module docs). No paragraph when nothing but whitespace was typed.
#[must_use]
pub fn draft_nodes(text: &str, mentions: &[DraftMention]) -> (Vec<Node>, Vec<(String, Mri)>) {
    let all = lines(text);
    let first = all.iter().position(|line| !blank(text, line, mentions));
    let last = all.iter().rposition(|line| !blank(text, line, mentions));
    let (Some(first), Some(last)) = (first, last) else {
        return (Vec::new(), Vec::new());
    };
    let mut table = Vec::new();
    let paragraphs = all[first..=last]
        .iter()
        .map(|line| Node::Paragraph(line_nodes(text, line, mentions, &mut table)))
        .collect();
    (paragraphs, table)
}

/// One line's nodes: its mentions at their ranges (each added to `table`), text in the rest.
fn line_nodes(text: &str, line: &Range<usize>, mentions: &[DraftMention], table: &mut Vec<(String, Mri)>) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut at = line.start;
    for mention in mentions_in(mentions, line) {
        if mention.range.start < at {
            continue;
        }
        push_text(&mut nodes, &text[at..mention.range.start]);
        let item_id = table.len().to_string();
        nodes.push(Node::Mention {
            item_id: item_id.clone(),
            text: text[mention.range.clone()].to_owned(),
        });
        table.push((item_id, Mri::new(mention.mri.clone())));
        at = mention.range.end;
    }
    push_text(&mut nodes, &text[at..line.end]);
    nodes
}

/// The message the text typed makes, after `quotes` (a reply's, or the quote an edited message keeps); `None` when
/// nothing but whitespace was typed.
///
/// # Errors
/// What `teams-proto` refuses in a message's text ([`ComposeError`]): a mention of anyone but a work account, a text
/// too long, …
pub fn message_text(
    text: &str,
    mentions: &[DraftMention],
    quotes: &[Node],
) -> Result<Option<MessageText>, ComposeError> {
    let (nodes, mentioned) = draft_nodes(text, mentions);
    if nodes.is_empty() {
        return Ok(None);
    }
    let mut all = quotes.to_vec();
    all.extend(nodes);
    MessageText::new(
        RichText {
            nodes: all,
            truncated: false,
        },
        mentioned,
    )
    .map(Some)
}

/// The quote a reply carries: the quoted message, its sender and the name shown for them, its time and a preview of
/// its text, as the captured reply quoted (`docs/protocol/chat.md`, "Quoted reply").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quoted {
    pub message_id: String,
    pub sender: String,
    pub sender_name: String,
    /// The quoted message's time, ms: its id, as the captured reply named it.
    pub time: String,
    /// Its text on one line, at most [`PREVIEW_MAX`] characters.
    pub preview: String,
}

impl Quoted {
    /// The quote of `message`, its sender shown as `sender_name`; `None` for a message without a sender.
    #[must_use]
    pub fn of(message: &Message, sender_name: &str) -> Option<Self> {
        let sender = message.sender.clone()?;
        let numeric = !message.id.is_empty() && message.id.bytes().all(|byte| byte.is_ascii_digit());
        let time = if numeric {
            message.id.clone()
        } else {
            message.arrival_time.to_string()
        };
        Some(Self {
            message_id: message.id.clone(),
            sender,
            sender_name: sender_name.to_owned(),
            time,
            preview: preview(message),
        })
    }

    /// The quote as rich text.
    #[must_use]
    pub fn node(&self) -> Node {
        Node::Quote(Quote {
            message_id: Some(self.message_id.clone()),
            sender: Some(self.sender.clone()),
            sender_name: Some(self.sender_name.clone()),
            time: Some(self.time.clone()),
            body: vec![text_node(&self.preview)],
        })
    }
}

/// A message's content as rich text.
#[must_use]
pub fn rich_text_of(message: &Message) -> RichText {
    if message.message_type == MessageType::RichTextHtml.as_str() {
        parse_html(&message.content)
    } else {
        RichText::plain(&message.content)
    }
}

/// A message's text on one line (what it quotes left out), at most [`PREVIEW_MAX`] characters.
fn preview(message: &Message) -> String {
    let nodes = rich_text_of(message)
        .nodes
        .into_iter()
        .filter(|node| !matches!(node, Node::Quote(_)))
        .collect();
    let plain = RichText {
        nodes,
        truncated: false,
    }
    .plain_text();
    let line = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= PREVIEW_MAX {
        return line;
    }
    let mut cut: String = line.chars().take(PREVIEW_MAX).collect();
    cut.push('…');
    cut
}

/// A mention loaded back for an edit: where its name stands, whom it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedMention {
    pub range: Range<usize>,
    pub mri: String,
    pub name: String,
}

/// A message's text loaded back into the box for an edit ([`load`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Loaded {
    pub text: String,
    pub mentions: Vec<LoadedMention>,
    /// Its quotes, kept as they were and sent again before the edited text.
    pub quotes: Vec<Node>,
}

/// Why a message can't be edited in the box.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CantEdit {
    #[error("it shows an image")]
    Image,
    #[error("it was read only in part")]
    Truncated,
}

impl Loaded {
    fn new_line(&mut self) {
        if !self.text.is_empty() {
            self.text.push('\n');
        }
    }

    fn inline<S: BuildHasher>(
        &mut self,
        nodes: &[Node],
        mentioned: &HashMap<String, String, S>,
    ) -> Result<(), CantEdit> {
        for node in nodes {
            self.node(node, mentioned)?;
        }
        Ok(())
    }

    fn node<S: BuildHasher>(&mut self, node: &Node, mentioned: &HashMap<String, String, S>) -> Result<(), CantEdit> {
        match node {
            Node::Text { text, .. } => self.text.push_str(text),
            Node::Break => self.text.push('\n'),
            Node::Mention { item_id, text } => self.mention(item_id, text, mentioned),
            Node::Link { children, .. } => self.inline(children, mentioned)?,
            Node::Paragraph(children) => {
                self.new_line();
                self.inline(children, mentioned)?;
            }
            Node::Quote(quote) => self.inline(&quote.body, mentioned)?,
            Node::Image(_) => return Err(CantEdit::Image),
        }
        Ok(())
    }

    fn mention<S: BuildHasher>(&mut self, item_id: &str, name: &str, mentioned: &HashMap<String, String, S>) {
        let start = self.text.len();
        self.text.push_str(name);
        let tokenable = !name.trim().is_empty() && !name.chars().any(char::is_control);
        if let Some(mri) = mentioned.get(item_id).filter(|_| tokenable) {
            self.mentions.push(LoadedMention {
                range: start..self.text.len(),
                mri: mri.clone(),
                name: name.to_owned(),
            });
        }
    }
}

/// A message's rich text as the box edits it: its paragraphs as lines, its mentions (those `mentioned` names by
/// `itemid`) as tokens again, a link as its text, its top-level quotes kept aside.
///
/// # Errors
/// [`CantEdit`]: the text shows an image (the box can't send one), or was read only in part.
pub fn load<S: BuildHasher>(text: &RichText, mentioned: &HashMap<String, String, S>) -> Result<Loaded, CantEdit> {
    if text.truncated {
        return Err(CantEdit::Truncated);
    }
    let mut loaded = Loaded::default();
    for node in &text.nodes {
        match node {
            Node::Quote(_) => loaded.quotes.push(node.clone()),
            other => loaded.node(other, mentioned)?,
        }
    }
    Ok(loaded)
}

/// The mention being typed before `cursor`: the `@`'s byte offset and the query after it. An `@` opens one when it
/// starts a word (at the text's start or after whitespace), on the cursor's line, at most [`MENTION_QUERY_MAX`]
/// characters back, outside a mention and with no mention between it and the cursor.
#[must_use]
pub fn mention_query<'a>(text: &'a str, cursor: usize, mentions: &[Range<usize>]) -> Option<(usize, &'a str)> {
    let before = text.get(..cursor)?;
    let (at, _) = before
        .char_indices()
        .rev()
        .take(MENTION_QUERY_MAX + 1)
        .take_while(|(_, character)| *character != '\n')
        .find(|(_, character)| *character == '@')?;
    let starts_word = before[..at].chars().next_back().is_none_or(char::is_whitespace);
    let query = &before[at + 1..];
    let crossed = mentions
        .iter()
        .any(|mention| mention.end > at && mention.start < cursor);
    (starts_word && !crossed && !query.starts_with(char::is_whitespace)).then_some((at, query))
}

/// Someone the mention picker offers: a member of the chat, by MRI, with the name shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub mri: String,
    pub name: SharedString,
}

/// The candidates whose name holds `query` (any case), at most [`PICKER_MAX`], in their order.
#[must_use]
pub fn matching(candidates: &[Candidate], query: &str) -> Vec<Candidate> {
    let query = query.to_lowercase();
    candidates
        .iter()
        .filter(|candidate| candidate.name.to_lowercase().contains(&query))
        .take(PICKER_MAX)
        .cloned()
        .collect()
}

/// When typing may be sent again: at most once per [`TYPING_EVERY`], the first at once (the web client's leading-edge
/// throttle).
#[derive(Clone, Copy, Debug, Default)]
pub struct TypingThrottle {
    last: Option<Instant>,
}

impl TypingThrottle {
    /// Whether a typing signal may go out at `now`; if so, `now` is the last one's time.
    pub fn take(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < TYPING_EVERY)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

/// The typing throttles of the app's chats, by conversation: a global, so a chat view built again (another chat opened
/// and this one again) doesn't start its chat's throttle over.
#[derive(Default)]
struct TypingSent(HashMap<String, TypingThrottle>);

impl Global for TypingSent {}

/// What the box is writing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A new message.
    New,
    /// A reply, with its quote.
    Reply(Quoted),
    /// An edit of the account's message `message_id`, which keeps `quotes`.
    Edit { message_id: String, quotes: Vec<Node> },
}

/// The mention picker while it is open: where the `@` is, the cursor it was read at, who matches, who is chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picker {
    pub at: usize,
    pub end: usize,
    pub matches: Vec<Candidate>,
    pub selected: usize,
    /// The chat's members are being read: nobody to offer yet.
    pub loading: bool,
}

/// What the box tells its chat view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposeEvent {
    /// Up in an empty box: the account's last message is to be edited.
    EditLast,
}

/// What a send took from the box: given back when the engine refuses the write.
struct Draft {
    content: InputContent,
    mode: Mode,
}

/// What a compose box is made of.
#[derive(Clone)]
pub struct ComposeParts {
    pub core: Arc<dyn Core>,
    pub settings: Entity<SettingsModel>,
    pub trace: Arc<ViewTrace>,
    /// Others' presence as the engine told it, for the mention picker's dots.
    pub presence: Entity<Presences>,
}

/// The compose box (see the module docs).
pub struct Compose {
    core: Arc<dyn Core>,
    settings: Entity<SettingsModel>,
    trace: Arc<ViewTrace>,
    presence: Entity<Presences>,
    conversation: String,
    input: Entity<TextareaState>,
    mode: Mode,
    picker: Option<Picker>,
    /// The chat's members the picker offers, once read.
    candidates: Option<Vec<Candidate>>,
    reading: Option<Task<()>>,
    /// What the last send couldn't do, shown over the box.
    problem: Option<SharedString>,
    _input_events: Subscription,
    _presence: Subscription,
}

impl EventEmitter<ComposeEvent> for Compose {}

/// The chat's members the picker may offer: work accounts but the account, by name.
async fn read_candidates(core: Arc<dyn Core>, conversation: String) -> Vec<Candidate> {
    let members = core.members(conversation).await.unwrap_or_default();
    let me = core.me().to_owned();
    let mut found = Vec::new();
    for member in members {
        if member.mri == me || !member.mri.starts_with("8:orgid:") {
            continue;
        }
        let name = core
            .person(member.mri.clone())
            .await
            .ok()
            .and_then(|person| person.name);
        found.push(Candidate {
            name: name.unwrap_or_else(|| member.mri.clone()).into(),
            mri: member.mri,
        });
    }
    found.sort_by(|one, other| one.name.cmp(&other.name));
    found
}

impl Compose {
    /// The box of `conversation`; traced as made (`compose opened`), so a live test can tell that no view it shows
    /// made one (a channel's).
    pub fn new(parts: ComposeParts, conversation: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        parts
            .trace
            .record(&TraceLine::new("compose", "opened").conversation(&conversation));
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .submit_on_enter(true)
                .soft_wrap(true)
                .auto_grow(1, MAX_ROWS)
                .placeholder("Type a message")
        });
        let input_events = cx.subscribe_in(&input, window, |this: &mut Self, _, event, window, cx| {
            if matches!(event, InputEvent::Change) {
                // what the box holds changed: traced without the text, so a test driving the keyboard can tell
                // when the app has taken in what it typed
                this.trace
                    .record(&TraceLine::new("compose", "changed").conversation(&this.conversation));
                this.refresh_picker(cx);
                this.typed(window, cx);
                // the send button follows whether there is anything to send
                cx.notify();
            }
        });
        // a presence the engine told: an open picker draws its dots again
        let presence = cx.observe(&parts.presence, |this: &mut Self, _, cx| {
            if this.picker.is_some() {
                cx.notify();
            }
        });
        Self {
            core: parts.core,
            settings: parts.settings,
            trace: parts.trace,
            presence: parts.presence,
            conversation,
            input,
            mode: Mode::New,
            picker: None,
            candidates: None,
            reading: None,
            problem: None,
            _input_events: input_events,
            _presence: presence,
        }
    }

    /// The kit's text area.
    #[must_use]
    pub fn input(&self) -> &Entity<TextareaState> {
        &self.input
    }

    #[must_use]
    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// The mention picker, while it is open.
    #[must_use]
    pub fn picker(&self) -> Option<&Picker> {
        self.picker.as_ref()
    }

    /// The presence the open picker shows beside each person it offers, in its order; empty while it is closed.
    #[must_use]
    pub fn picker_presence(&self, cx: &App) -> Vec<(String, Availability)> {
        let presence = self.presence.read(cx);
        self.picker
            .iter()
            .flat_map(|picker| &picker.matches)
            .map(|candidate| (candidate.mri.clone(), presence.of(&candidate.mri)))
            .collect()
    }

    /// What the last send couldn't do.
    #[must_use]
    pub fn problem(&self) -> Option<&SharedString> {
        self.problem.as_ref()
    }

    /// The text typed.
    #[must_use]
    pub fn value(&self, cx: &App) -> SharedString {
        self.input.read(cx).value()
    }

    /// Gives the box the focus.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Whether the box has the focus.
    #[must_use]
    pub fn focused(&self, window: &Window, cx: &App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    /// Shows `problem` over the box.
    pub fn show_problem(&mut self, problem: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.problem = Some(problem.into());
        cx.notify();
    }

    /// Starts a reply quoting `quoted`; the text typed stays.
    pub fn reply_to(&mut self, quoted: Quoted, window: &mut Window, cx: &mut Context<Self>) {
        self.trace.record(
            &TraceLine::new("compose", "replying")
                .conversation(&self.conversation)
                .messages([quoted.message_id.as_str()]),
        );
        self.mode = Mode::Reply(quoted);
        self.problem = None;
        self.focus(window, cx);
        cx.notify();
    }

    /// Starts editing the account's message `message_id`: its text loaded into the box.
    pub fn edit(&mut self, message_id: String, loaded: Loaded, window: &mut Window, cx: &mut Context<Self>) {
        self.trace.record(
            &TraceLine::new("compose", "editing")
                .conversation(&self.conversation)
                .messages([message_id.as_str()]),
        );
        let content = loaded
            .mentions
            .iter()
            .fold(InputContent::new(loaded.text.clone()), |content, mention| {
                let token = InlineToken::new(mention.mri.clone(), mention.name.clone());
                // a mention whose name the text doesn't hold as it stands stays text
                content
                    .clone()
                    .with_token(mention.range.clone(), token)
                    .unwrap_or(content)
            });
        let end = loaded.text.len();
        self.input.update(cx, |input, cx| {
            input.set_value(content, window, cx);
            input.set_selected_range(end..end, cx);
        });
        self.mode = Mode::Edit {
            message_id,
            quotes: loaded.quotes,
        };
        self.picker = None;
        self.problem = None;
        self.focus(window, cx);
        cx.notify();
    }

    /// Cancels a reply (its text stays) or an edit (its text goes).
    pub fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == Mode::New {
            return;
        }
        if matches!(self.mode, Mode::Edit { .. }) {
            self.input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.mode = Mode::New;
        self.picker = None;
        self.trace
            .record(&TraceLine::new("compose", "cancelled").conversation(&self.conversation));
        cx.notify();
    }

    /// Puts a mention of `mri`, shown as `name`, at the cursor (the picker's choice puts it over the `@` typed).
    pub fn insert_mention(&mut self, mri: &str, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.input.read(cx).cursor();
        self.place_mention(cursor..cursor, mri, name, window, cx);
    }

    fn place_mention(
        &mut self,
        range: Range<usize>,
        mri: &str,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let token = InlineToken::new(mri.to_owned(), name.to_owned());
        let placed = self.input.update(cx, |input, cx| {
            let placed = input.replace_range_with_token(range, token, window, cx);
            if placed.is_ok() {
                input.insert(" ", window, cx);
            }
            placed
        });
        self.picker = None;
        if let Err(error) = placed {
            self.problem = Some(format!("That mention can't be added: {error}").into());
        }
        cx.notify();
    }

    /// The text typed and its mentions, by the MRIs their tokens name.
    fn draft(&self, cx: &App) -> (String, Vec<DraftMention>) {
        let input = self.input.read(cx);
        let mentions = input
            .tokens()
            .iter()
            .map(|span| DraftMention {
                range: span.range(),
                mri: span.token().id().to_string(),
            })
            .collect();
        (input.value().to_string(), mentions)
    }

    /// Sends what the box holds (see the module docs); the box is emptied once the write is asked for, and given its
    /// draft back (its text, its mentions, the reply or the edit) when the engine refuses it before it makes a row,
    /// unless something was typed in it since.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (text, mentions) = self.draft(cx);
        let quotes = match &self.mode {
            Mode::New => Vec::new(),
            Mode::Reply(quoted) => vec![quoted.node()],
            Mode::Edit { quotes, .. } => quotes.clone(),
        };
        let built = match message_text(&text, &mentions, &quotes) {
            Ok(Some(built)) => built,
            Ok(None) => return,
            Err(error) => return self.show_problem(format!("This message can't be sent: {error}"), cx),
        };
        let conversation = self.conversation.clone();
        let (action, kind) = match &self.mode {
            Mode::Edit { message_id, .. } => (
                Action::Edit {
                    conversation,
                    message_id: message_id.clone(),
                    text: built,
                },
                "edit",
            ),
            Mode::Reply(_) => (
                Action::Send {
                    conversation,
                    text: built,
                },
                "reply",
            ),
            Mode::New => (
                Action::Send {
                    conversation,
                    text: built,
                },
                "message",
            ),
        };
        let draft = Draft {
            content: self.input.read(cx).content(),
            mode: self.mode.clone(),
        };
        let reply = self.core.write(action);
        self.trace.record(
            &TraceLine::new("compose", "sent")
                .conversation(&self.conversation)
                .kind(kind),
        );
        self.clear(window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = reply.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.give_back(draft, window, cx);
                    this.show_problem(format!("Not sent: {error}"), cx);
                });
            }
        })
        .detach();
    }

    /// Puts a refused send's draft back in the box, unless the box holds something new or writes something else now.
    fn give_back(&mut self, draft: Draft, window: &mut Window, cx: &mut Context<Self>) {
        let empty = {
            let input = self.input.read(cx);
            input.value().is_empty() && input.tokens().is_empty()
        };
        if !empty || self.mode != Mode::New {
            return;
        }
        let end = draft.content.text().len();
        self.input.update(cx, |input, cx| {
            input.set_value(draft.content, window, cx);
            input.set_selected_range(end..end, cx);
        });
        self.mode = draft.mode;
        cx.notify();
    }

    /// Empties the box and ends a reply or an edit.
    fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_value("", window, cx));
        self.mode = Mode::New;
        self.picker = None;
        self.problem = None;
        cx.notify();
    }

    /// Typing goes out (see the module docs) when it may.
    fn typed(&mut self, window: &Window, cx: &mut Context<Self>) {
        let wanted = self.settings.read(cx).sends_typing()
            && !matches!(self.mode, Mode::Edit { .. })
            && self.focused(window, cx)
            && self.holds_text(cx);
        if !wanted {
            return;
        }
        let now = cx.background_executor().now();
        let may = cx
            .default_global::<TypingSent>()
            .0
            .entry(self.conversation.clone())
            .or_default()
            .take(now);
        if may {
            // nothing of it is kept: how it went is the engine's (a dropped typing is never sent again)
            drop(self.core.transient(Transient::Typing {
                conversation: self.conversation.clone(),
            }));
        }
    }

    fn holds_text(&self, cx: &App) -> bool {
        let input = self.input.read(cx);
        !input.value().trim().is_empty() || !input.tokens().is_empty()
    }

    // --- the mention picker ---

    /// Reads the picker again from the text and the cursor as they stand, before a key it would take: moving the
    /// cursor changes no text, so nothing else tells the picker (an `@` the cursor left closes it).
    fn revalidate_picker(&mut self, cx: &mut Context<Self>) {
        if self.picker.is_some() {
            self.refresh_picker(cx);
        }
    }

    /// Opens, moves or closes the picker after the text changed, or before a key it would take.
    fn refresh_picker(&mut self, cx: &mut Context<Self>) {
        let (text, cursor, tokens) = {
            let input = self.input.read(cx);
            let tokens: Vec<Range<usize>> = input.tokens().iter().map(InlineTokenSpan::range).collect();
            (input.value(), input.cursor(), tokens)
        };
        let Some((at, query)) = mention_query(&text, cursor, &tokens) else {
            return self.close_picker(cx);
        };
        self.read_candidates(cx);
        let loading = self.candidates.is_none();
        let matches = self
            .candidates
            .as_deref()
            .map(|candidates| matching(candidates, query))
            .unwrap_or_default();
        if !loading && matches.is_empty() {
            return self.close_picker(cx);
        }
        // the people offered, at most `PICKER_MAX`: asked about each time they are shown (the chat list's budget,
        // `LIST_WATCHED`, leaves them room in the engine's watch)
        presence::watch(self.core.as_ref(), matches.iter().map(|candidate| &candidate.mri));
        let selected = self
            .picker
            .as_ref()
            .map_or(0, |picker| picker.selected)
            .min(matches.len().saturating_sub(1));
        self.picker = Some(Picker {
            at,
            end: cursor,
            matches,
            selected,
            loading,
        });
        cx.notify();
    }

    /// Reads the chat's members for the picker, once.
    fn read_candidates(&mut self, cx: &mut Context<Self>) {
        if self.candidates.is_some() || self.reading.is_some() {
            return;
        }
        let read = read_candidates(Arc::clone(&self.core), self.conversation.clone());
        self.reading = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let found = read.await;
            let _ = this.update(cx, |this, cx| {
                this.candidates = Some(found);
                this.reading = None;
                this.refresh_picker(cx);
            });
        }));
    }

    fn close_picker(&mut self, cx: &mut Context<Self>) {
        if self.picker.take().is_some() {
            cx.notify();
        }
    }

    /// Whether the picker offers anyone.
    fn offering(&self) -> bool {
        self.picker.as_ref().is_some_and(|picker| !picker.matches.is_empty())
    }

    fn step_picker(&mut self, step: isize, cx: &mut Context<Self>) {
        if let Some(picker) = &mut self.picker {
            let last = picker.matches.len().saturating_sub(1);
            picker.selected = picker.selected.saturating_add_signed(step).min(last);
            cx.notify();
        }
    }

    /// Puts the picker's `index`th match in place of the `@` and the query typed.
    pub fn choose(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.revalidate_picker(cx);
        let Some(picker) = self.picker.clone() else { return };
        let Some(candidate) = picker.matches.get(index) else {
            return;
        };
        self.place_mention(picker.at..picker.end, &candidate.mri, &candidate.name, window, cx);
    }

    fn choose_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.picker.as_ref().map_or(0, |picker| picker.selected);
        self.choose(selected, window, cx);
    }

    // --- keys ---

    fn capture_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        self.revalidate_picker(cx);
        if self.picker.is_some() {
            self.step_picker(-1, cx);
            cx.stop_propagation();
        } else if self.mode == Mode::New && self.input.read(cx).value().is_empty() {
            cx.emit(ComposeEvent::EditLast);
            cx.stop_propagation();
        }
    }

    fn capture_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.revalidate_picker(cx);
        if self.picker.is_some() {
            self.step_picker(1, cx);
            cx.stop_propagation();
        }
    }

    fn capture_enter(&mut self, enter: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        self.revalidate_picker(cx);
        if self.offering() && !enter.shift {
            self.choose_selected(window, cx);
            cx.stop_propagation();
        }
    }

    fn capture_tab(&mut self, _: &IndentInline, window: &mut Window, cx: &mut Context<Self>) {
        self.revalidate_picker(cx);
        if self.offering() {
            self.choose_selected(window, cx);
            cx.stop_propagation();
        }
    }

    /// The plain Enter the text area handed on: the send.
    fn on_enter(&mut self, enter: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if !enter.shift {
            self.send(window, cx);
        }
    }

    fn on_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        self.revalidate_picker(cx);
        if self.picker.is_some() {
            self.close_picker(cx);
        } else if self.mode == Mode::New {
            cx.propagate();
        } else {
            self.cancel(window, cx);
        }
    }

    // --- drawing ---

    fn mode_bar(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (title, detail) = match &self.mode {
            Mode::New => return None,
            Mode::Reply(quoted) => (format!("Replying to {}", quoted.sender_name), quoted.preview.clone()),
            Mode::Edit { .. } => ("Editing your message".to_owned(), String::new()),
        };
        let tip = if matches!(self.mode, Mode::Edit { .. }) {
            "Cancel editing"
        } else {
            "Cancel reply"
        };
        let cancel = icon_button("compose-cancel", Glyph::X, tip)
            .with_size(Size::XSmall)
            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx)));
        Some(
            div()
                .debug_selector(|| "compose-mode".to_owned())
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .border_l_2()
                .border_color(rgb(palette.brand))
                .text_xs()
                .child(div().font_weight(gpui::FontWeight::BOLD).child(title))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_color(rgb(palette.muted_foreground))
                        .child(detail),
                )
                .child(div().debug_selector(|| "compose-cancel".to_owned()).child(cancel))
                .into_any_element(),
        )
    }

    /// Send (Enter does the same), greyed while the box holds nothing to send.
    fn send_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let empty = self.input.read(cx).value().trim().is_empty();
        let tip = if matches!(self.mode, Mode::Edit { .. }) {
            "Save (Enter)"
        } else {
            "Send (Enter)"
        };
        selected_by(
            "compose-send",
            icon_button("compose-send", Glyph::SEND, tip)
                .disabled(empty)
                .when(!empty, ButtonVariants::primary)
                .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
        )
        .mb(px(2.0))
        .into_any_element()
    }

    fn picker_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.picker.as_ref()?;
        let presence = self.presence.read(cx);
        let rows = picker.matches.iter().enumerate().map(|(index, candidate)| {
            let chosen = index == picker.selected;
            div()
                .id(("mention", index))
                .debug_selector(move || format!("mention-{index}"))
                .px_2()
                .py_1()
                .rounded_md()
                .flex()
                .items_center()
                .gap_2()
                .when_chosen(chosen, palette)
                .child(presence::dot(&presence.of(&candidate.mri), palette))
                .child(candidate.name.clone())
                .on_click(cx.listener(move |this, _, window, cx| this.choose(index, window, cx)))
        });
        let loading = picker
            .loading
            .then(|| div().px_2().text_color(rgb(palette.muted_foreground)).child("…"));
        Some(
            div()
                .debug_selector(|| "mention-picker".to_owned())
                .flex()
                .flex_col()
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .text_sm()
                .children(rows)
                .children(loading)
                .into_any_element(),
        )
    }

    fn problem_view(&self, palette: Palette) -> Option<AnyElement> {
        let problem = self.problem.clone()?;
        Some(
            div()
                .debug_selector(|| "compose-problem".to_owned())
                .text_xs()
                .italic()
                .text_color(rgb(palette.foreground))
                .child(problem)
                .into_any_element(),
        )
    }
}

/// A picker row's look when it is the one chosen.
trait Chosen {
    fn when_chosen(self, chosen: bool, palette: Palette) -> Self;
}

impl Chosen for gpui::Stateful<gpui::Div> {
    fn when_chosen(self, chosen: bool, palette: Palette) -> Self {
        if chosen { self.bg(rgb(palette.selected)) } else { self }
    }
}

impl Render for Compose {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        div()
            .key_context(CONTEXT)
            .debug_selector(|| "compose".to_owned())
            .capture_action(cx.listener(Self::capture_up))
            .capture_action(cx.listener(Self::capture_down))
            .capture_action(cx.listener(Self::capture_enter))
            .capture_action(cx.listener(Self::capture_tab))
            .on_action(cx.listener(Self::on_enter))
            .on_action(cx.listener(Self::on_escape))
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .px_4()
            .pt_1()
            .pb_3()
            .children(self.picker_view(palette, cx))
            .children(self.mode_bar(palette, cx))
            .children(self.problem_view(palette))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h(px(36.0))
                            .child(Textarea::new(&self.input)),
                    )
                    .child(self.send_button(cx)),
            )
    }
}
