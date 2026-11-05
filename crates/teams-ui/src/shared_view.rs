//! A meeting chat's Shared tab, as Teams draws it: the files and links the chat shared, the newest first, filtered by
//! Teams' pills (Recent, Files, Links) and by a keyword, each row its name (and where it is), when it was shared and
//! by whom; a click opens it in the browser. What was shared is read from the chat's stored messages
//! ([`read_shared`], [`crate::recap::shared_items`]), who shared it named by the engine ([`Core::person`]).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Local;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window,
    div, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};
use teams_store::{Message, PageCursor};

use crate::bridge::Core;
use crate::clock;
use crate::icons::Glyph;
use crate::recap::{SharedFilter, SharedItem, filtered, shared_items};
use crate::theme::{self, Palette};

/// The stored messages read for what a chat shared, at most: pages of [`PAGE`].
const PAGES: usize = 20;
const PAGE: usize = 200;

/// The words the tab shows when nothing matches.
pub const NOTHING_SHARED: &str = "Nothing has been shared in this chat yet.";

/// What the chat `conversation` shared, read from its stored messages (up to [`PAGES`] pages of them).
pub fn read_shared<V: 'static>(
    core: Arc<dyn Core>,
    conversation: String,
    cx: &mut Context<V>,
) -> Task<Vec<SharedItem>> {
    let messages = read_messages(core, conversation, cx);
    cx.spawn(async move |_, _| shared_items(&messages.await))
}

/// The chat `conversation`'s stored messages, the newest first (up to [`PAGES`] pages of them).
pub fn read_messages<V: 'static>(core: Arc<dyn Core>, conversation: String, cx: &mut Context<V>) -> Task<Vec<Message>> {
    cx.spawn(async move |_, _| {
        let mut messages: Vec<Message> = Vec::new();
        let mut before: Option<PageCursor> = None;
        for _ in 0..PAGES {
            let Ok(page) = core.messages(conversation.clone(), before.clone(), PAGE).await else {
                break;
            };
            messages.extend(page.messages);
            match page.next {
                Some(next) => before = Some(next),
                None => break,
            }
        }
        messages
    })
}

/// The Shared tab (see the module docs).
pub struct SharedView {
    core: Arc<dyn Core>,
    conversation: String,
    items: Option<Vec<SharedItem>>,
    filter: SharedFilter,
    keyword: Entity<InputState>,
    names: HashMap<String, String>,
    _typed: Subscription,
    _reading: Task<()>,
}

impl SharedView {
    /// The Shared tab of the chat `conversation`; its read starts now.
    pub fn new(core: Arc<dyn Core>, conversation: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let keyword = cx.new(|cx| InputState::new(window, cx).placeholder("Filter by keyword"));
        let typed = cx.subscribe(&keyword, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let read = read_shared(Arc::clone(&core), conversation.clone(), cx);
        let reading = cx.spawn(async move |this, cx| {
            let items = read.await;
            let _ = this.update(cx, |this, cx| {
                this.name_senders(&items, cx);
                this.items = Some(items);
                cx.notify();
            });
        });
        Self {
            core,
            conversation,
            items: None,
            filter: SharedFilter::default(),
            keyword,
            names: HashMap::new(),
            _typed: typed,
            _reading: reading,
        }
    }

    /// The chat the tab is of.
    #[must_use]
    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    /// Shows the items `filter` keeps.
    pub fn set_filter(&mut self, filter: SharedFilter, cx: &mut Context<Self>) {
        self.filter = filter;
        cx.notify();
    }

    /// The items listed now, once read.
    #[must_use]
    pub fn listed(&self, cx: &App) -> Option<Vec<SharedItem>> {
        let keyword = self.keyword.read(cx).value().to_string();
        let items = self.items.as_ref()?;
        Some(filtered(items, self.filter, &keyword).into_iter().cloned().collect())
    }

    /// Asks for the name of everyone who shared something.
    fn name_senders(&mut self, items: &[SharedItem], cx: &mut Context<Self>) {
        let mut senders: Vec<String> = items.iter().filter_map(|item| item.sender.clone()).collect();
        senders.sort();
        senders.dedup();
        for mri in senders {
            let reply = self.core.person(mri.clone());
            cx.spawn(async move |this, cx| {
                let Some(name) = reply.await.ok().and_then(|person| person.name) else {
                    return;
                };
                let _ = this.update(cx, |this, cx| this.named(mri, name, cx));
            })
            .detach();
        }
    }

    fn named(&mut self, mri: String, name: String, cx: &mut Context<Self>) {
        self.names.insert(mri, name);
        cx.notify();
    }

    fn sender(&self, item: &SharedItem) -> String {
        if item.sender.as_deref() == Some(self.core.me()) {
            return "You".to_owned();
        }
        item.sender
            .as_ref()
            .and_then(|mri| self.names.get(mri).cloned())
            .or_else(|| item.sender_name.clone())
            .unwrap_or_default()
    }
}

impl Render for SharedView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let top = self.top_row(palette, cx);
        let rows = match self.listed(cx) {
            None => div()
                .debug_selector(|| "shared-loading".to_owned())
                .p_6()
                .flex()
                .justify_center()
                .text_color(rgb(palette.brand))
                .child(Glyph::LOADING.icon().size(px(24.0)))
                .into_any_element(),
            Some(items) if items.is_empty() => div()
                .debug_selector(|| "shared-empty".to_owned())
                .p_6()
                .text_color(rgb(palette.muted_foreground))
                .child(NOTHING_SHARED)
                .into_any_element(),
            Some(items) => self.table(&items, palette, cx),
        };
        div()
            .debug_selector(|| "shared-view".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .px_4()
            .child(top)
            .child(rows)
    }
}

impl SharedView {
    /// Teams' pills and the keyword's box.
    fn top_row(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let pill = |filter: SharedFilter, cx: &mut Context<Self>| {
            let chosen = self.filter == filter;
            let glyph = match filter {
                SharedFilter::Recent => Glyph::CLOCK,
                SharedFilter::Files => Glyph::FILE,
                SharedFilter::Links => Glyph::GLOBE,
            };
            let key = filter.key();
            div()
                .id(SharedString::from(format!("shared-filter-{key}")))
                .debug_selector(move || format!("shared-filter-{key}"))
                .h(px(32.0))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .rounded_full()
                .border_1()
                .cursor_pointer()
                .border_color(rgb(if chosen { palette.brand } else { palette.border }))
                .bg(rgb(if chosen { palette.selected } else { palette.surface }))
                .when(chosen, |pill| pill.font_weight(FontWeight::SEMIBOLD))
                .child(glyph.icon().size(px(14.0)))
                .child(filter.label())
                .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx)))
        };
        div()
            .h(px(64.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .child(pill(SharedFilter::Recent, cx))
            .child(pill(SharedFilter::Files, cx))
            .child(pill(SharedFilter::Links, cx))
            .child(div().flex_1())
            .child(
                div()
                    .debug_selector(|| "shared-keyword".to_owned())
                    .w(px(220.0))
                    .child(Input::new(&self.keyword)),
            )
            .into_any_element()
    }

    /// The list: its header, then a row per item.
    fn table(&self, items: &[SharedItem], palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let cycle = clock::cycle(cx);
        let now = Local::now();
        let header = div()
            .h(px(44.0))
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .child(div().w(px(24.0)))
            .child(div().flex_1().child("Name"))
            .child(div().w(px(150.0)).child("Date shared"))
            .child(div().w(px(180.0)).child("Shared by"));
        let rows: Vec<AnyElement> = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let url = item.url.clone();
                let glyph = if item.is_file() { Glyph::FILE } else { Glyph::LINK };
                div()
                    .id(SharedString::from(format!("shared-row-{index}")))
                    .debug_selector(move || format!("shared-row-{index}"))
                    .h(px(56.0))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .border_b_1()
                    .border_color(rgb(palette.border))
                    .cursor_pointer()
                    .hover(move |row| row.bg(rgb(palette.hover)))
                    .child(div().w(px(24.0)).text_color(rgb(palette.brand)).child(glyph.icon()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().child(item.name.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(rgb(palette.muted_foreground))
                                    .child(item.url.clone()),
                            ),
                    )
                    .child(
                        div()
                            .w(px(150.0))
                            .text_sm()
                            .child(clock::list_time(item.time, &now, cycle)),
                    )
                    .child(div().w(px(180.0)).text_sm().truncate().child(self.sender(item)))
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element()
            })
            .collect();
        div()
            .id("shared-table")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(header)
            .children(rows)
            .into_any_element()
    }
}
