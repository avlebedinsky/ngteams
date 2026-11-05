//! A post's or a reply's "More options" in a channel, for the workflows that run from a message, as Teams' channel
//! menu offers them: its "..." shows while the message is under the pointer (and while its menu is open); the menu
//! lists the first workflows, then "More actions" with the rest and "Create workflows". The view writes nothing
//! itself: a choice is told to the shell ([`ChannelViewEvent::RunWorkflow`], [`ChannelViewEvent::CreateWorkflows`]).
use std::cell::Cell;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{AppContext as _, Bounds, ElementId, Entity, Pixels, anchored, deferred, px};
use gpui_component::{Selectable as _, Sizable as _, Size};

use super::{Body, ChannelView, ChannelViewEvent, Item};
use crate::chat_view::measured;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::Palette;
use crate::workflow_dialog::FlowRef;
use crate::workflow_run::MessageRef;
use crate::workflow_run::menu::{self, MessageWorkflows};
use gpui::{AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, div};

/// The size of the "..." button, as the chat's hover bar's.
const BUTTON: f32 = 28.0;

/// A message's menu: which is open (the message and its post), where its button and its "More actions" list were
/// drawn, and the workflows it offers.
#[derive(Default)]
pub(super) struct Menu {
    open: Option<(String, Option<String>)>,
    at: Rc<Cell<Option<Bounds<Pixels>>>>,
    more: bool,
    more_at: Rc<Cell<Option<Bounds<Pixels>>>>,
    workflows: Option<Entity<MessageWorkflows>>,
    /// The app's read-only mode: no workflow runs, so no menu is offered.
    pub(super) read_only: bool,
}

impl Menu {
    /// A text message's "...": shown while the message is under the pointer, or while its menu is open.
    pub(super) fn button(&self, item: &Item, root: Option<&str>, cx: &mut Context<ChannelView>) -> Option<AnyElement> {
        if self.read_only || !matches!(item.body, Body::Text(_)) {
            return None;
        }
        let open = self.open.as_ref().is_some_and(|(id, _)| *id == item.id);
        let (message, root) = (item.id.clone(), root.map(str::to_owned));
        let group = gpui::SharedString::from(format!("channel-message-{}", item.id));
        let button = icon_button(
            ElementId::Name(format!("channel-more-{}", item.id).into()),
            Glyph::MORE,
            "More options",
        )
        .with_size(Size::Size(px(BUTTON)))
        .selected(open)
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.toggle_menu(&message, root.clone(), cx);
        }));
        Some(
            selected_by(format!("channel-more-{}", item.id), button)
                .relative()
                .when(!open, |cell| cell.invisible().group_hover(group, gpui::Styled::visible))
                .children(open.then(|| measured(&self.at)))
                .into_any_element(),
        )
    }
}

impl ChannelView {
    /// Opens or closes `message`'s menu; opening reads the account's workflows that run from a message again, as
    /// Teams does.
    pub fn toggle_menu(&mut self, message: &str, root: Option<String>, cx: &mut Context<Self>) {
        self.menu.more = false;
        if self.menu.open.as_ref().is_some_and(|(id, _)| id == message) {
            self.menu.open = None;
            cx.notify();
            return;
        }
        self.menu.open = Some((message.to_owned(), root));
        let workflows = if let Some(workflows) = &self.menu.workflows {
            workflows.clone()
        } else {
            let core = std::sync::Arc::clone(&self.core);
            let workflows = cx.new(|_| MessageWorkflows::new(core));
            cx.observe(&workflows, |_, _, cx| cx.notify()).detach();
            self.menu.workflows = Some(workflows.clone());
            workflows
        };
        workflows.update(cx, MessageWorkflows::refresh);
        cx.notify();
    }

    /// The account's workflows that run from a message, as last read.
    #[must_use]
    pub fn message_workflows(&self, cx: &gpui::App) -> Vec<FlowRef> {
        self.menu
            .workflows
            .as_ref()
            .map_or_else(Vec::new, |workflows| workflows.read(cx).flows().to_vec())
    }

    fn message_ref(&self) -> Option<MessageRef> {
        let (id, root) = self.menu.open.clone()?;
        Some(MessageRef {
            conversation: self.channel.clone(),
            id,
            root,
            in_channel: true,
            team: None,
        })
    }

    /// Runs `flow` on the message whose menu is open.
    pub fn run_workflow(&mut self, flow: FlowRef, cx: &mut Context<Self>) {
        if let Some(message) = self.message_ref() {
            cx.emit(ChannelViewEvent::RunWorkflow { flow, message });
        }
        self.menu.open = None;
        self.menu.more = false;
        cx.notify();
    }

    /// "Create workflows" on the message whose menu is open.
    pub fn create_workflows(&mut self, cx: &mut Context<Self>) {
        if let Some(message) = self.message_ref() {
            cx.emit(ChannelViewEvent::CreateWorkflows(message));
        }
        self.menu.open = None;
        self.menu.more = false;
        cx.notify();
    }

    /// The open menu, under its button: the first workflows, then "More actions", whose list opens beside it.
    pub(super) fn menu_overlay(&mut self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.menu.open.is_none() {
            self.menu.at.set(None);
            return None;
        }
        let at = self.menu.at.get()?;
        let workflows = self.message_workflows(cx);
        let mut main = menu::panel("channel-message-menu", palette).on_mouse_down_out(cx.listener(
            move |this, event: &gpui::MouseDownEvent, _, cx| {
                let in_list = this.menu.more
                    && this
                        .menu
                        .more_at
                        .get()
                        .is_some_and(|list| list.contains(&event.position));
                if !at.contains(&event.position) && !in_list {
                    this.menu.open = None;
                    this.menu.more = false;
                    cx.notify();
                }
            },
        ));
        for flow in workflows.iter().take(menu::MAIN_MENU) {
            let (chosen, view) = (flow.clone(), cx.entity().downgrade());
            main = main.child(menu::workflow_row(flow, palette, move |_, _, cx| {
                let _ = view.update(cx, |this, cx| this.run_workflow(chosen.clone(), cx));
            }));
        }
        if !workflows.is_empty() {
            main = main.child(menu::separator(palette));
        }
        let view = cx.entity().downgrade();
        main = main.child(menu::more_actions_row(self.menu.more, palette, move |_, _, cx| {
            let _ = view.update(cx, |this, cx| {
                this.menu.more = !this.menu.more;
                cx.notify();
            });
        }));
        let list = self.menu.more.then(|| {
            let mut list = menu::panel("channel-more-actions-menu", palette)
                .relative()
                .child(measured(&self.menu.more_at));
            let rest: Vec<_> = workflows.iter().skip(menu::MAIN_MENU).collect();
            for flow in &rest {
                let (chosen, view) = ((*flow).clone(), cx.entity().downgrade());
                list = list.child(menu::workflow_row(flow, palette, move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.run_workflow(chosen.clone(), cx));
                }));
            }
            if !rest.is_empty() {
                list = list.child(menu::separator(palette));
            }
            let view = cx.entity().downgrade();
            list.child(menu::create_row(palette, move |_, _, cx| {
                let _ = view.update(cx, ChannelView::create_workflows);
            }))
        });
        Some(
            deferred(
                anchored()
                    .anchor(gpui::Anchor::TopRight)
                    .position(at.bottom_right())
                    .snap_to_window_with_margin(px(8.0))
                    .child(div().pt_1().flex().items_end().gap_1().children(list).child(main)),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}
