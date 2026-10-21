//! The chat list: the account's chats by last activity, each row with its avatar, name, last message's first line,
//! time and unread count; the connection state above it.
//!
//! - **From the store only.** The list asks the engine's store ([`Core::conversations`], then per chat its members,
//!   newest messages, unread count and people's names) when it opens and again each time the models say a chat
//!   changed ([`crate::Chats::revision`]): a delivery, a catch-up, a read horizon, an outbox row. A row is built again
//!   only when its chat changed since it was built; every row again after a catch-up, which may have brought the
//!   names of people in other chats.
//! - **Which chats**: 1:1, group and meeting chats. A team's channels belong to the Teams rail (Task 16) and the
//!   account's own streams (`48:notes`, `48:notifications`, …) aren't chats: neither is listed.
//! - **Names** (until Task 12 reads profiles): a 1:1 chat by the other member's newest `imdisplayname` in the store,
//!   else their MRI; a group or meeting chat by its topic, else its other members' names (or MRIs). The avatar shows
//!   the initials of a name the store gave.
//! - **Unread** is the store's count after the account's read horizon (`unread_count`).
//!
//! Opening a chat (a click, or the switcher) is told as [`ChatListEvent::Opened`]; the shell shows it. What the list
//! shows is traced (`chat-list shown`: the chats' ids in order, their kinds and unread counts, never a name or text).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Datelike as _, Local, TimeZone};
use gpui::{
    AnyElement, App, AsyncApp, Context, ElementId, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    WeakEntity, Window, div, px, rgb, uniform_list,
};
use gpui_component::avatar::Avatar;
use gpui_component::{Sizable as _, Size};
use teams_proto::chat::parse_html;
use teams_store::{Conversation, ConversationKind, Member, Message};

use crate::bridge::Core;
use crate::models::Models;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};
use crate::view::status_text;

/// The most chats the list shows.
pub const LIST_LIMIT: usize = 500;
/// How many of a chat's newest messages are looked through for its preview line.
pub const PREVIEW_PAGE: usize = 20;
/// The longest preview line, in characters.
pub const PREVIEW_CHARS: usize = 120;
/// The most members a group chat without a topic is named by.
pub const NAMED_MEMBERS: usize = 4;
/// A row's height, px.
pub const ROW_HEIGHT: f32 = 56.0;
/// What a deleted message's preview says.
pub const DELETED: &str = "This message was deleted";

/// What kind of chat a row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    OneToOne,
    Group,
    Meeting,
    /// A kind this build doesn't name.
    Other,
}

impl RowKind {
    #[must_use]
    pub fn of(kind: &ConversationKind) -> Self {
        match kind {
            ConversationKind::OneToOne => Self::OneToOne,
            ConversationKind::Group => Self::Group,
            ConversationKind::Meeting => Self::Meeting,
            ConversationKind::Channel | ConversationKind::Other(_) => Self::Other,
        }
    }

    /// The trace's name for it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::OneToOne => "one-to-one",
            Self::Group => "group",
            Self::Meeting => "meeting",
            Self::Other => "other",
        }
    }
}

/// Whether the chat list shows a conversation: not a channel (the Teams rail's), not one of the account's own
/// streams.
#[must_use]
pub fn listed(conversation: &Conversation) -> bool {
    !conversation.id.starts_with("48:") && conversation.kind != ConversationKind::Channel
}

/// One row of the list, as built from the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatRow {
    pub id: String,
    pub kind: RowKind,
    /// What the row is called (see the module docs).
    pub name: SharedString,
    /// Whether `name` came from the store's names or the chat's topic, not from MRIs.
    pub named: bool,
    /// The last message's first line.
    pub preview: SharedString,
    /// Milliseconds.
    pub last_activity: Option<i64>,
    pub unread: u64,
}

/// The other member of a 1:1 chat its id names (`19:<oid>_<oid>@unq.gbl.spaces`), when one of the two is `me`.
#[must_use]
pub fn one_to_one_partner(id: &str, me: &str) -> Option<String> {
    let pair = id.strip_prefix("19:")?.strip_suffix("@unq.gbl.spaces")?;
    let (first, second) = pair.split_once('_')?;
    let mine = me.strip_prefix("8:orgid:")?;
    let other = match (first == mine, second == mine) {
        (true, false) => second,
        (false, true) => first,
        _ => return None,
    };
    Some(format!("8:orgid:{other}"))
}

/// The chat's members other than `me`; a 1:1 chat without stored members gives the one its id names.
#[must_use]
pub fn others(conversation: &Conversation, members: &[Member], me: &str) -> Vec<String> {
    let mut others: Vec<String> = members
        .iter()
        .map(|member| member.mri.clone())
        .filter(|mri| mri != me)
        .collect();
    if others.is_empty() && conversation.kind == ConversationKind::OneToOne {
        others.extend(one_to_one_partner(&conversation.id, me));
    }
    others
}

/// A message the list previews: text, deleted or not.
fn is_text(message: &Message) -> bool {
    message.message_type == "Text" || message.message_type.starts_with("RichText")
}

/// The first line of `text` with something in it, at most [`PREVIEW_CHARS`] characters.
fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    if line.chars().count() <= PREVIEW_CHARS {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(PREVIEW_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// The preview line of a chat from its newest messages (newest first): the newest text message's first line, read
/// from its sanitized rich text ("You: " before the account's own); empty when there is none.
#[must_use]
pub fn preview(page: &[Message], me: &str) -> SharedString {
    let Some(message) = page.iter().find(|message| is_text(message)) else {
        return SharedString::default();
    };
    let text = if message.delete_time.is_some() {
        DELETED.to_owned()
    } else {
        first_line(&parse_html(&message.content).plain_text())
    };
    if message.sender.as_deref() == Some(me) {
        format!("You: {text}").into()
    } else {
        text.into()
    }
}

/// A row's time: the time of day today, the weekday within the week, else the day (and the year, if not this one).
#[must_use]
pub fn format_time<Tz: TimeZone>(ms: i64, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(at) = DateTime::from_timestamp_millis(ms) else {
        return String::new();
    };
    let at = at.with_timezone(&now.timezone());
    let (day, today) = (at.date_naive(), now.date_naive());
    let days = (today - day).num_days();
    let format = if days == 0 {
        "%H:%M"
    } else if (1..7).contains(&days) {
        "%a"
    } else if at.year() == now.year() {
        "%-d %b"
    } else {
        "%-d %b %Y"
    };
    at.format(format).to_string()
}

/// What a chat is called and whether that is a name (see the module docs).
async fn name_of(core: &dyn Core, conversation: &Conversation, members: &[Member]) -> (SharedString, bool) {
    let topic = conversation
        .topic
        .as_deref()
        .map(str::trim)
        .filter(|topic| !topic.is_empty());
    if let Some(topic) = topic
        && conversation.kind != ConversationKind::OneToOne
    {
        return (topic.to_owned().into(), true);
    }
    let others = others(conversation, members, core.me());
    if others.is_empty() {
        return ("Chat".into(), false);
    }
    let mut shown = Vec::new();
    let mut all_named = true;
    for mri in others.iter().take(NAMED_MEMBERS) {
        if let Ok(Some(name)) = core.display_name(mri.clone()).await {
            shown.push(name);
        } else {
            shown.push(mri.clone());
            all_named = false;
        }
    }
    let mut name = shown.join(", ");
    if others.len() > NAMED_MEMBERS {
        name = format!("{name} +{}", others.len() - NAMED_MEMBERS);
    }
    (name.into(), all_named)
}

/// A chat's row, from the store. A query that fails leaves its part empty: the row is still shown.
pub async fn build_row(core: &dyn Core, conversation: &Conversation) -> ChatRow {
    let id = conversation.id.clone();
    let members = core.members(id.clone()).await.unwrap_or_default();
    let page = core
        .messages(id.clone(), None, PREVIEW_PAGE)
        .await
        .map(|page| page.messages)
        .unwrap_or_default();
    let unread = core.unread_count(id.clone()).await.unwrap_or(0);
    let (name, named) = name_of(core, conversation, &members).await;
    ChatRow {
        kind: RowKind::of(&conversation.kind),
        name,
        named,
        preview: preview(&page, core.me()),
        last_activity: conversation.last_activity,
        unread,
        id,
    }
}

/// What the chat list tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatListEvent {
    /// A chat was opened from the list.
    Opened(String),
}

/// A row built, and what it was built from.
#[derive(Clone, Debug)]
struct Cached {
    conversation: Conversation,
    /// The models' revision when it was built: a later change of its chat builds it again.
    at: u64,
    row: ChatRow,
}

/// A row for a load to keep, or to build.
enum Planned {
    Keep(ChatRow),
    Build(Conversation),
}

/// The chat list view (see the module docs).
pub struct ChatList {
    core: Arc<dyn Core>,
    models: Models,
    trace: Arc<ViewTrace>,
    rows: Vec<ChatRow>,
    cache: HashMap<String, Cached>,
    /// Moves when the cache is dropped: a load that started before keeps none of its rows.
    generation: u64,
    selected: Option<String>,
    loading: Option<Task<()>>,
    /// A change came while a load ran: another follows it.
    dirty: bool,
    /// Loads that ended.
    loads: u64,
    catch_ups: u64,
    traced: Option<Vec<(String, RowKind, u64)>>,
    _observing: [Subscription; 2],
}

impl EventEmitter<ChatListEvent> for ChatList {}

impl ChatList {
    /// The list over the store, loaded at once; `selected` is highlighted.
    pub fn new(
        core: Arc<dyn Core>,
        models: Models,
        trace: Arc<ViewTrace>,
        selected: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let chats = cx.observe(&models.chats, |this: &mut Self, _, cx| this.reload(cx));
        let status = cx.observe(&models.status, |this: &mut Self, status, cx| {
            let catch_ups = status.read(cx).catch_ups;
            if catch_ups != this.catch_ups {
                // a catch-up may have brought names of people in other chats
                this.catch_ups = catch_ups;
                this.cache.clear();
                this.generation += 1;
                this.reload(cx);
            }
            cx.notify();
        });
        let catch_ups = models.status.read(cx).catch_ups;
        let mut list = Self {
            core,
            models,
            trace,
            rows: Vec::new(),
            cache: HashMap::new(),
            generation: 0,
            selected,
            loading: None,
            dirty: false,
            loads: 0,
            catch_ups,
            traced: None,
            _observing: [chats, status],
        };
        list.reload(cx);
        list
    }

    /// The rows shown, the latest activity first.
    #[must_use]
    pub fn rows(&self) -> &[ChatRow] {
        &self.rows
    }

    /// A shown row, by its chat's id.
    #[must_use]
    pub fn row(&self, id: &str) -> Option<&ChatRow> {
        self.rows.iter().find(|row| row.id == id)
    }

    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// How many loads from the store have ended.
    #[must_use]
    pub fn loads(&self) -> u64 {
        self.loads
    }

    /// Whether a load is running.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.loading.is_some()
    }

    /// Highlights a chat (the one the shell shows), telling nobody.
    pub fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if self.selected != id {
            self.selected = id;
            cx.notify();
        }
    }

    /// Opens a chat: highlighted, and told to the shell.
    pub fn open(&mut self, id: String, cx: &mut Context<Self>) {
        self.select(Some(id.clone()), cx);
        cx.emit(ChatListEvent::Opened(id));
    }

    /// Reads the list from the store again (or once the running load ends).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.loading.is_some() {
            self.dirty = true;
            return;
        }
        self.dirty = false;
        let core = Arc::clone(&self.core);
        let generation = self.generation;
        self.loading = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            load(core, generation, this, cx).await;
        }));
    }

    /// Which rows a load keeps and which it builds, of the conversations the store listed.
    fn plan(&self, listed: Vec<Conversation>, cx: &App) -> Vec<Planned> {
        let chats = self.models.chats.read(cx);
        listed
            .into_iter()
            .filter(self::listed)
            .map(|conversation| match self.cache.get(&conversation.id) {
                Some(cached)
                    if cached.at >= chats.changed_at(&conversation.id) && cached.conversation == conversation =>
                {
                    Planned::Keep(cached.row.clone())
                }
                _ => Planned::Build(conversation),
            })
            .collect()
    }

    /// Keeps the rows a load built, as of the models' revision `at`.
    fn keep_built(&mut self, rows: &[ChatRow], built: Vec<Conversation>, at: u64) {
        for conversation in built {
            let Some(row) = rows.iter().find(|row| row.id == conversation.id) else {
                continue;
            };
            let cached = Cached {
                row: row.clone(),
                at,
                conversation,
            };
            self.cache.insert(cached.row.id.clone(), cached);
        }
    }

    /// A load ended: its rows shown, the ones it built kept (unless the cache was dropped since it started).
    fn loaded(
        &mut self,
        generation: u64,
        at: u64,
        rows: Option<(Vec<ChatRow>, Vec<Conversation>)>,
        cx: &mut Context<Self>,
    ) {
        self.loading = None;
        self.loads += 1;
        if let Some((rows, built)) = rows {
            if generation == self.generation {
                self.keep_built(&rows, built, at);
            }
            self.cache.retain(|id, _| rows.iter().any(|row| &row.id == id));
            self.rows = rows;
            self.trace_rows();
        }
        cx.notify();
        if self.dirty {
            self.reload(cx);
        }
    }

    /// Traces what the list shows, when it changed.
    fn trace_rows(&mut self) {
        let shown: Vec<(String, RowKind, u64)> = self
            .rows
            .iter()
            .map(|row| (row.id.clone(), row.kind, row.unread))
            .collect();
        if self.traced.as_ref() == Some(&shown) {
            return;
        }
        let line = TraceLine::new("chat-list", "shown")
            .conversations(shown.iter().map(|(id, _, _)| id.as_str()))
            .unread(shown.iter().map(|(_, _, unread)| *unread));
        let line = shown.iter().fold(line, |line, (_, kind, _)| line.kind(kind.name()));
        self.trace.record(&line);
        self.traced = Some(shown);
    }

    /// The row at `index` of the list; its test selector `chat-row-<index>`.
    fn render_row(&self, index: usize, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.rows.get(index) else {
            return div().into_any_element();
        };
        let selected = self.selected.as_deref() == Some(row.id.as_str());
        let id = row.id.clone();
        let background = if selected { palette.selected } else { palette.surface };
        div()
            .id(ElementId::Name(row.id.clone().into()))
            .debug_selector(move || format!("chat-row-{index}"))
            .w_full()
            .h(px(ROW_HEIGHT))
            .overflow_hidden()
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .bg(rgb(background))
            .hover(move |style| style.bg(rgb(if selected { palette.selected } else { palette.hover })))
            .on_click(cx.listener(move |this, _, _, cx| this.open(id.clone(), cx)))
            .child(avatar(row))
            .child(row_text(row, palette))
            .into_any_element()
    }
}

/// The row's avatar: the initials of a name the store gave, else none.
fn avatar(row: &ChatRow) -> Avatar {
    let avatar = Avatar::new().with_size(Size::Small);
    if row.named {
        avatar.name(row.name.clone())
    } else {
        avatar
    }
}

/// The row's name and time over its preview and unread count.
fn row_text(row: &ChatRow, palette: Palette) -> impl IntoElement {
    let time = row
        .last_activity
        .map(|ms| format_time(ms, &Local::now()))
        .unwrap_or_default();
    let name = div().flex_1().min_w_0().truncate().child(row.name.clone());
    let name = if row.unread > 0 {
        name.font_weight(gpui::FontWeight::BOLD)
    } else {
        name
    };
    let top = div()
        .flex()
        .gap_2()
        .child(name)
        .child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(time));
    let preview = div()
        .flex_1()
        .min_w_0()
        .truncate()
        .text_sm()
        .text_color(rgb(palette.muted_foreground))
        .child(row.preview.clone());
    let mut bottom = div().flex().gap_2().items_center().child(preview);
    if row.unread > 0 {
        bottom = bottom.child(
            div()
                .px_1()
                .rounded_md()
                .text_xs()
                .bg(rgb(palette.brand))
                .text_color(rgb(palette.on_brand))
                .child(row.unread.to_string()),
        );
    }
    div().flex_1().min_w_0().flex().flex_col().child(top).child(bottom)
}

/// One load: the conversation list, then each row kept or built, then shown.
async fn load(core: Arc<dyn Core>, generation: u64, this: WeakEntity<ChatList>, cx: &mut AsyncApp) {
    let listed = core.conversations(LIST_LIMIT).await;
    let Ok(planned) = this.update(cx, |list, cx| {
        let at = list.models.chats.read(cx).revision();
        (listed.map(|listed| list.plan(listed, cx)), at)
    }) else {
        // the list is gone
        return;
    };
    let (planned, at) = planned;
    let rows = match planned {
        Ok(planned) => Some(build_rows(&*core, planned).await),
        // the engine stopped: the rows shown stay
        Err(_) => None,
    };
    let _ = this.update(cx, |list, cx| list.loaded(generation, at, rows, cx));
}

/// The rows of a load, in its order, and the conversations it built rows for.
async fn build_rows(core: &dyn Core, planned: Vec<Planned>) -> (Vec<ChatRow>, Vec<Conversation>) {
    let mut rows = Vec::with_capacity(planned.len());
    let mut built = Vec::new();
    for item in planned {
        match item {
            Planned::Keep(row) => rows.push(row),
            Planned::Build(conversation) => {
                rows.push(build_row(core, &conversation).await);
                built.push(conversation);
            }
        }
    }
    (rows, built)
}

impl Render for ChatList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let status = status_text(self.models.status.read(cx));
        let header = div()
            .h(px(48.0))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(div().font_weight(gpui::FontWeight::BOLD).child("Chat"))
            .child(
                div()
                    .debug_selector(|| "connection".to_owned())
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(status),
            );
        let rows = uniform_list(
            "chat-rows",
            self.rows.len(),
            cx.processor(move |list: &mut Self, range: std::ops::Range<usize>, _window, cx| {
                range
                    .map(|index| list.render_row(index, palette, cx))
                    .collect::<Vec<AnyElement>>()
            }),
        )
        .flex_1()
        .w_full();
        div()
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .bg(rgb(palette.surface))
            .child(header)
            .child(rows)
    }
}
