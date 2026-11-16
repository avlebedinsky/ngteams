//! A bot's messages and a bot's chat, as Teams shows them: a bot sender's avatar is its app's icon on a tile
//! ([`crate::bot_face`]) and its name the one its message carries, else its app's; a 1:1 chat with a bot that holds no
//! message yet shows the app's intro where the messages go (its tile, name, maker and what it does), as a new
//! conversation with a person says it is new.

use std::sync::Arc;

use gpui::{
    AnyElement, AsyncApp, Context, InteractiveElement as _, IntoElement as _, ParentElement as _, Styled as _, div, px, rgb,
};

use super::ChatView;
use crate::bot_face::{self, BotApp};
use crate::people::Shown;
use crate::theme::Palette;

/// The intro's tile, px.
const INTRO_TILE: f32 = 72.0;

/// What the intro says under the app's own words.
pub const INTRO_HINT: &str = "Send a message to start chatting.";

/// A bot as the view names it: by its app, with its icon.
pub(super) fn shown(app: &BotApp) -> Shown {
    Shown {
        name: Some(app.name().to_owned().into()),
        from: None,
        picture: app.picture.clone(),
    }
}

impl ChatView {
    /// Finds whether the view's chat is a 1:1 chat with a bot, and its app, for the intro.
    pub(super) fn ask_chat_bot(&self, cx: &mut Context<Self>) {
        if !self.conversation.ends_with("@unq.gbl.spaces") {
            return;
        }
        let core = Arc::clone(&self.core);
        let id = self.conversation.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let members = core.members(id.clone()).await.unwrap_or_default();
            let Some(bot) = crate::chat_list::one_to_one_bot(core.as_ref(), &id, &members, &[]).await else {
                return;
            };
            // the Workflows app's own Chat tab stays as it is
            if crate::workflows_app::is_workflows_bot(&bot) {
                return;
            }
            let app = bot_face::bot_app(core.as_ref(), &bot).await;
            let _ = this.update(cx, |view, cx| {
                view.chat_bot = Some((bot, app));
                cx.notify();
            });
        })
        .detach();
    }

    /// The bot this 1:1 chat is with, once found.
    #[must_use]
    pub fn chat_bot(&self) -> Option<&str> {
        self.chat_bot.as_ref().map(|(bot, _)| bot.as_str())
    }

    /// The intro of a bot's chat while it holds no message.
    pub(super) fn bot_intro(&self, palette: Palette) -> Option<AnyElement> {
        if !self.is_empty() {
            return None;
        }
        let (_, app) = self.chat_bot.as_ref()?;
        let name = app.as_ref().map_or(crate::chat_list::BOT, BotApp::name).to_owned();
        let maker = app
            .as_ref()
            .map(|app| app.app.app.developer.clone())
            .filter(|maker| !maker.is_empty());
        let about = app
            .as_ref()
            .map(|app| app.app.app.description.clone())
            .filter(|about| !about.is_empty());
        let words = div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.foreground))
                    .child(name),
            )
            .children(maker.map(|maker| div().text_sm().text_color(rgb(palette.muted_foreground)).child(maker)))
            .children(about.map(|about| {
                div()
                    .pt_2()
                    .max_w(px(420.0))
                    .text_sm()
                    .text_center()
                    .text_color(rgb(palette.foreground))
                    .child(about)
            }))
            .child(
                div()
                    .pt_3()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(INTRO_HINT),
            );
        Some(
            div()
                .debug_selector(|| "chat-bot-intro".to_owned())
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .px_6()
                .child(bot_face::tile(app.as_ref(), INTRO_TILE, palette))
                .child(words)
                .into_any_element(),
        )
    }
}
