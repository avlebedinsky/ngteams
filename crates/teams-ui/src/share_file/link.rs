//! What "Share this file" offers besides a share to chats: its "More actions" (…) with "Share outside Teams", and
//! Teams' "Link created" box that Copy link shows in the dialog's place (seen with lab accounts on 2026-11-18).
//!
//! - **Share outside Teams** opens the file's OneDrive page in the browser ([`onedrive_page`]), where SharePoint's own
//!   Share, Copy link and Manage access are. A file with no such page (an own server's) gets no "More actions".
//! - **Link created**: the check and "Link created", Close; the link in a field with Copy beside it; under them what
//!   the link lets people do, and Settings (the same OneDrive page: SharePoint sets a link there). Copy puts the link
//!   on the clipboard again; Close and Escape close the dialog.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px, rgb,
};
use gpui_component::button::Button;
use gpui_component::{FocusTrapElement as _, Selectable as _, Sizable as _};
use teams_proto::chat::onedrive_page;

use super::ShareFileDialog;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::{self, Palette};
use crate::trace::TraceLine;

/// The words of the menu and of the box, as Teams and SharePoint have them.
pub const MORE_ACTIONS: &str = "More actions";
pub const SHARE_OUTSIDE: &str = "Share outside Teams";
pub const LINK_CREATED: &str = "Link created";
pub const COPY: &str = "Copy";
pub const SETTINGS: &str = "Settings";

/// The "Link created" box's width, px, as SharePoint draws it.
const LINK_WIDTH: f32 = 480.0;

impl ShareFileDialog {
    /// The file's OneDrive page, where SharePoint shares it and sets its links; none for a file with no such page.
    #[must_use]
    pub fn outside_page(&self) -> Option<String> {
        onedrive_page(&self.file)
    }

    /// "More actions": opens or closes its menu.
    pub fn toggle_more(&mut self, cx: &mut Context<Self>) {
        self.more_open = !self.more_open;
        self.emoji = None;
        cx.notify();
    }

    /// Whether "More actions" is open.
    #[must_use]
    pub fn more_open(&self) -> bool {
        self.more_open
    }

    /// Share outside Teams (and Settings): the file's OneDrive page in the browser.
    pub fn share_outside(&mut self, cx: &mut Context<Self>) {
        self.more_open = false;
        if let Some(page) = self.outside_page() {
            self.trace.record(&TraceLine::new("share-file", "outside"));
            cx.open_url(&page);
        }
        cx.notify();
    }

    /// The header's "More actions", for a file that has its OneDrive page.
    pub(super) fn more_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.outside_page()?;
        Some(
            selected_by(
                "share-file-more",
                icon_button("share-file-more", Glyph::MORE, MORE_ACTIONS)
                    .selected(self.more_open)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_more(cx))),
            )
            .into_any_element(),
        )
    }

    /// The open menu, under "More actions", on a raised surface: "Share outside Teams".
    pub(super) fn more_menu(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.more_open {
            return None;
        }
        let row = div()
            .id("share-file-outside")
            .debug_selector(|| "share-file-outside".to_owned())
            .h(px(36.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .cursor_pointer()
            .font_weight(FontWeight::SEMIBOLD)
            .text_sm()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.share_outside(cx)))
            .child(Glyph::SHARE.icon().size(px(16.0)))
            .child(SHARE_OUTSIDE);
        Some(
            div()
                .debug_selector(|| "share-file-more-menu".to_owned())
                .absolute()
                .occlude()
                .top(px(68.0))
                .right(px(72.0))
                .p_1()
                .rounded_lg()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .shadow_lg()
                .child(row)
                .into_any_element(),
        )
    }

    /// "Link created" in the dialog's place (see the module docs).
    pub(super) fn link_created_view(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let link = SharedString::from(self.file.open_link().unwrap_or_default().to_owned());
        let close = selected_by(
            "share-file-link-close",
            icon_button("share-file-link-close", Glyph::X, "Close")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
        );
        let copied = link.clone();
        let copy = selected_by(
            "share-file-link-copy",
            Button::new("share-file-link-copy")
                .outline()
                .label(COPY)
                .on_click(move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(copied.to_string()))),
        );
        let permission = self
            .permission
            .as_ref()
            .and_then(|permission| permission.description.clone())
            .unwrap_or_default();
        div()
            .debug_selector(|| "share-file-link-created".to_owned())
            .id("share-file-dialog")
            .key_context("ShareFileDialog")
            .track_focus(&self.focus)
            .occlude()
            .w(px(LINK_WIDTH))
            .max_w_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(Self::on_key))
            .child(Self::link_title(close))
            .child(Self::link_row(link, copy, palette))
            .child(self.link_foot(permission.into(), palette, cx))
            .focus_trap("share-file-link-trap", &self.focus)
            .into_any_element()
    }

    /// "Link created" and its Close.
    fn link_title(close: impl IntoElement) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Glyph::CIRCLE_CHECK
                    .icon()
                    .size(px(18.0))
                    .text_color(rgb(theme::SUCCESS)),
            )
            .child(
                div()
                    .flex_1()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(LINK_CREATED),
            )
            .child(close)
            .into_any_element()
    }

    /// The link in its field, and Copy beside it.
    fn link_row(link: SharedString, copy: impl IntoElement, palette: Palette) -> AnyElement {
        div()
            .flex()
            .items_center()
            .child(
                div()
                    .debug_selector(|| "share-file-link-field".to_owned())
                    .flex_1()
                    .min_w_0()
                    .h(px(32.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded_l_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .text_sm()
                    .child(div().truncate().child(link)),
            )
            .child(copy)
            .into_any_element()
    }

    /// Under the link: what it lets people do, and Settings for a file that has its OneDrive page.
    fn link_foot(&self, permission: SharedString, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let settings = self.outside_page().map(|_| {
            div()
                .id("share-file-link-settings")
                .debug_selector(|| "share-file-link-settings".to_owned())
                .flex()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .text_sm()
                .text_color(rgb(palette.brand))
                .hover(gpui::Styled::underline)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.share_outside(cx)))
                .child(Glyph::SETTINGS.icon().size(px(14.0)))
                .child(SETTINGS)
        });
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .debug_selector(|| "share-file-link-permission".to_owned())
                    .text_sm()
                    .when(permission.is_empty(), gpui::Styled::invisible)
                    .child(permission),
            )
            .children(settings)
            .into_any_element()
    }
}
