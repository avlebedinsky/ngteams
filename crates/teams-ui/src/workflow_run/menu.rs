//! The workflows a message's "More options" menu offers, as Teams' does: the account's workflows that run from a
//! message (read when the menu opens), the first [`MAIN_MENU`] of them in the menu itself with the Workflows app's
//! icon, the rest under "More actions" above "Create workflows".
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AsyncApp, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div, px, rgb,
};

use crate::bridge::Core;
use crate::icons::Glyph;
use crate::theme::Palette;
use crate::workflow_dialog::FlowRef;

/// How many workflows the menu itself lists; the rest are under "More actions".
pub const MAIN_MENU: usize = 3;

/// The account's workflows that run from a message, as last read.
pub struct MessageWorkflows {
    core: Arc<dyn Core>,
    flows: Vec<FlowRef>,
    reads: u64,
}

impl MessageWorkflows {
    #[must_use]
    pub fn new(core: Arc<dyn Core>) -> Self {
        Self {
            core,
            flows: Vec::new(),
            reads: 0,
        }
    }

    /// The workflows as last read; none before a read settled or after one failed.
    #[must_use]
    pub fn flows(&self) -> &[FlowRef] {
        &self.flows
    }

    /// Reads them again, as Teams does each time a message's menu opens; only the latest read lands. Nothing is read
    /// in the read-only mode, where a run can't go.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.core.read_only() {
            return;
        }
        self.reads += 1;
        let read = self.reads;
        let core = Arc::clone(&self.core);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let flows = crate::workflow_dialog::message_workflows(&core).await;
            let _ = this.update(cx, |this, cx| {
                if this.reads == read {
                    this.flows = flows.unwrap_or_default();
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

/// A menu row: an icon (or the Workflows app's picture), its words, and its click.
pub fn row(
    id: impl Into<String>,
    leading: AnyElement,
    words: String,
    trailing: Option<AnyElement>,
    palette: Palette,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let id = id.into();
    let selector = id.clone();
    div()
        .id(ElementId::Name(id.into()))
        .debug_selector(move || selector.clone())
        .w_full()
        .h(px(32.0))
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded_md()
        .cursor_pointer()
        .text_sm()
        .text_color(rgb(palette.foreground))
        .hover(move |row| row.bg(rgb(palette.hover)))
        .on_click(on_click)
        .child(leading)
        .child(div().flex_1().min_w_0().truncate().child(words))
        .children(trailing)
        .into_any_element()
}

/// A workflow's row: the Workflows app's icon and its name.
pub fn workflow_row(
    flow: &FlowRef,
    palette: Palette,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    row(
        format!("message-workflow-{}", flow.id),
        div()
            .flex_none()
            .size(px(16.0))
            .child(gpui::img(crate::workflow_dialog::workflows_icon()).size(px(16.0)))
            .into_any_element(),
        flow.name.clone(),
        None,
        palette,
        on_click,
    )
}

/// "More actions", with its chevron; chosen while its list is open.
pub fn more_actions_row(
    open: bool,
    palette: Palette,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let row = row(
        "message-more-actions",
        div().flex_none().size(px(14.0)).into_any_element(),
        "More actions".to_owned(),
        Some(Glyph::CHEVRON_RIGHT.icon().size(px(14.0)).into_any_element()),
        palette,
        on_click,
    );
    div()
        .rounded_md()
        .when(open, |cell| cell.bg(rgb(palette.hover)))
        .child(row)
        .into_any_element()
}

/// "Create workflows", at the foot of "More actions".
pub fn create_row(
    palette: Palette,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    row(
        "message-create-workflows",
        Glyph::PLUS.icon().size(px(14.0)).into_any_element(),
        "Create workflows".to_owned(),
        None,
        palette,
        on_click,
    )
}

/// A hairline between a menu's groups.
#[must_use]
pub fn separator(palette: Palette) -> AnyElement {
    div().my_1().h(px(1.0)).bg(rgb(palette.border)).into_any_element()
}

/// A floating menu panel, as the message menu's.
#[must_use]
pub fn panel(id: &'static str, palette: Palette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .w(px(260.0))
        .p_1()
        .flex()
        .flex_col()
        .gap_0p5()
        .bg(rgb(palette.surface))
        .border_1()
        .border_color(rgb(palette.border))
        .rounded_lg()
        .shadow_lg()
        .occlude()
}
