//! A chat's bot cards and suggested actions, behind `NGTEAMS_BOTS=1` ([`crate::bots_flag`]):
//!
//! - **Cards**: each card message keeps its bot state ([`CardBot`]) while the view lives: its answers, its buttons'
//!   asks and what the bot answered ([`crate::bot_card`]). A card whose `refresh` names the account runs it by itself
//!   when it is first drawn; any card with a `refresh` offers "Refresh card" in its message's "More options".
//! - **Suggested actions**: when the newest message of a 1:1 chat is a bot's and suggests actions, they stand as
//!   buttons over the compose box, as Teams shows them; a click posts the user's answer (an `imBack`) or asks the bot.
//!   They go once anyone writes again.
//!
//! None of it is offered in read-only mode.
use std::cell::RefCell;
use std::collections::HashMap;

use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonRounded};
use teams_proto::chat::card::{ActionKind, Card};

use super::{Body, ChatView, MessageItem, Place, writes_to};
use crate::bot_card::{CardBot, Target};
use crate::card_view::{BotHook, CardState};
use crate::icons::Glyph;
use crate::theme::Palette;

/// "Refresh card" in a message's "More options".
pub const REFRESH_CARD: &str = "Refresh card";

/// What a chat's cards show besides what they say ([`ChatView`]'s, per message).
pub(super) struct HeldCards {
    /// Toggled elements, opened cards.
    pub(super) states: HashMap<String, CardState>,
    /// Each card message's bot state, made as it is first drawn; none while the bots aren't shown.
    bots: Option<RefCell<HashMap<String, Entity<CardBot>>>>,
}

impl HeldCards {
    pub(super) fn new(cx: &App) -> Self {
        Self {
            states: HashMap::new(),
            bots: crate::bots_flag::bots_on(cx).then(RefCell::default),
        }
    }

    /// Whether the bots' cards and suggested actions are shown (`NGTEAMS_BOTS=1`, [`crate::bots_flag`]).
    pub(super) const fn shows_bots(&self) -> bool {
        self.bots.is_some()
    }
}

impl ChatView {
    /// Whether this chat's cards take answers and ask their bot: the bots are shown and the account's cloud serves
    /// bot invokes (`Capabilities::apps`); else the cards are read and drawn as without the flag.
    pub(super) fn asks_bots(&self) -> bool {
        self.held_cards.shows_bots() && self.core.capabilities().apps
    }

    /// Where a message's bot is asked and its user's answers go: a channel's post's chain, else the view's own place.
    fn bot_conversation(&self, message: &str) -> String {
        match &self.place {
            Place::Channel => teams_proto::reply_chain_id(&self.conversation, self.root_of(message))
                .unwrap_or_else(|| self.conversation.clone()),
            place => writes_to(&self.conversation, place),
        }
    }

    /// `item`'s bot state, made when first asked for; a card that names the account then refreshes by itself.
    pub(super) fn card_bot(&self, item: &MessageItem, cx: &mut Context<Self>) -> Option<Entity<CardBot>> {
        if !self.asks_bots() {
            return None;
        }
        let bots = self.held_cards.bots.as_ref()?;
        if let Some(bot) = bots.borrow().get(&item.id) {
            return Some(bot.clone());
        }
        let target = Target::of_sender(item.sender.as_deref(), &self.bot_conversation(&item.id));
        let core = std::sync::Arc::clone(&self.core);
        let id = item.id.clone();
        let bot = cx.new(|_| CardBot::new(core, id, target));
        // what its bot answered shows at once
        cx.observe(&bot, |_, _, cx| cx.notify()).detach();
        bots.borrow_mut().insert(item.id.clone(), bot.clone());
        if !self.core.read_only() {
            let me = self.core.me().to_owned();
            for (at, card) in cards_of(item).iter().enumerate() {
                if let Some(refresh) = card
                    .refresh
                    .as_ref()
                    .filter(|refresh| CardBot::refreshes_for(refresh, &me))
                {
                    let refresh = refresh.clone();
                    bot.update(cx, |bot, cx| bot.refresh(at, &refresh, true, cx));
                }
            }
        }
        Some(bot)
    }

    /// What `item`'s cards are drawn with besides their state: its bot state, while the bots are shown.
    pub(super) fn bot_hook(&self, item: &MessageItem, cx: &mut Context<Self>) -> Option<BotHook> {
        Some(BotHook {
            bot: self.card_bot(item, cx)?,
            read_only: self.core.read_only(),
        })
    }

    /// The `refresh` of `message`'s cards, run by hand: each card that has one.
    pub fn refresh_card(&mut self, message: &str, cx: &mut Context<Self>) {
        self.options = None;
        let Some(item) = self.message_items().find(|item| item.id == message).cloned() else {
            return;
        };
        let Some(bot) = self.card_bot(&item, cx) else {
            return;
        };
        for (at, card) in cards_of(&item).iter().enumerate() {
            if let Some(refresh) = &card.refresh {
                bot.update(cx, |bot, cx| bot.refresh(at, refresh, false, cx));
            }
        }
        cx.notify();
    }

    /// What a click on button `action` of card `card` of `message` does when it asks the bot: its card's answers
    /// checked, then the bot asked. A button that doesn't ask the bot does nothing here.
    pub fn press_card_action(&mut self, message: &str, card: usize, action: usize, cx: &mut Context<Self>) {
        let Some(item) = self.message_items().find(|item| item.id == message).cloned() else {
            return;
        };
        let Some(shown) = cards_of(&item).get(card) else {
            return;
        };
        let Some(ActionKind::Submit(pressed)) = shown.actions.get(action).map(|action| &action.kind) else {
            return;
        };
        let mut inputs = Vec::new();
        crate::bot_card::card_inputs(shown, false, &mut inputs);
        let Some(bot) = self.card_bot(&item, cx) else {
            return;
        };
        let key = format!("c{card}a{action}");
        bot.update(cx, |bot, cx| bot.press(card, &key, pressed, &inputs, cx));
    }

    /// "Refresh card" for the more-options menu of `message`, when the bots are shown, the app writes and a card of
    /// the message has a `refresh`.
    pub(super) fn refresh_entry(&self, message: &str, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let item = self.message_items().find(|item| item.id == message)?;
        let offered = self.asks_bots()
            && !self.core.read_only()
            && !item.actions().is_empty()
            && cards_of(item).iter().any(|card| card.refresh.is_some());
        if !offered {
            return None;
        }
        let message = message.to_owned();
        Some(
            div()
                .id("action-refresh-card")
                .debug_selector(|| "action-refresh-card".to_owned())
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
                .on_click(cx.listener(move |this, _, _, cx| this.refresh_card(&message, cx)))
                .child(Glyph::REFRESH.icon().size(px(14.0)))
                .child(div().flex_1().child(REFRESH_CARD))
                .into_any_element(),
        )
    }

    /// The newest message's suggested actions over the compose box (see the module docs).
    pub(super) fn suggested_actions(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.asks_bots() || self.core.read_only() || teams_proto::one_to_one_pair(&self.conversation).is_none() {
            return None;
        }
        let item = self.message_items().last()?;
        let Body::Cards(cards) = &item.body else {
            return None;
        };
        let from_bot = item.sender.as_deref().is_some_and(|sender| sender.starts_with("28:"));
        if !from_bot || cards.0.suggested.is_empty() {
            return None;
        }
        let bot = self.card_bot(item, cx)?;
        let buttons = cards.0.suggested.iter().enumerate().filter_map(|(at, action)| {
            let ActionKind::Submit(sent) = &action.kind else {
                return None;
            };
            let (bot, sent, key) = (bot.clone(), sent.clone(), format!("suggested-{at}"));
            let id = SharedString::from(key.clone());
            let selector = key.clone();
            Some(
                div().debug_selector(move || selector).child(
                    Button::new(ElementId::Name(id))
                        .label(action.title.clone())
                        .outline()
                        .rounded(ButtonRounded::Large)
                        .small()
                        .on_click(move |_, _, cx| {
                            bot.update(cx, |bot, cx| bot.press(0, &key, &sent, &[], cx));
                        }),
                ),
            )
        });
        Some(
            div()
                .id("suggested-actions")
                .debug_selector(|| "suggested-actions".to_owned())
                .w_full()
                .px_4()
                .pb_2()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
                .text_color(rgb(palette.foreground))
                .children(buttons)
                .into_any_element(),
        )
    }
}

/// `item`'s cards, when it is a card message.
fn cards_of(item: &MessageItem) -> &[Card] {
    match &item.body {
        Body::Cards(cards) => &cards.0.cards,
        _ => &[],
    }
}
