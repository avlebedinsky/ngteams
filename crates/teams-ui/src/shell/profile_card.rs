//! The window's profile cards ([`crate::profile_card`]): the views' asks ([`CardAsk`], from the models' `cards`) that
//! came from this shell's window open a card here, one at a time.
//!
//! - **Hover**: the pointer resting [`HOVER_OPEN`] on a face or a name opens its compact card beside it (under it, or
//!   over it near the window's foot; left of it near the window's right edge); the card closes [`HOVER_CLOSE`] after the
//!   pointer left both, unless it came back to either. A card opened by a click, or one pressed or typed in since, stays
//!   until Escape or a press outside.
//! - **The full card** opens over the dimmed window from a click on a face, or from the compact card's links; no hover
//!   opens a card over it.
//! - **What the card asks**: Chat opens the 1:1 chat in its place, or New message to them alone (a new chat its first
//!   message makes); a call asks the calls for their 1:1 chat's call ([`Shell::ask_call`], refused as any call is while
//!   another is up), and a video call asks the camera on once that call's media is up, as the user asked for video.

use super::*;
use crate::new_chat::Recipient;
use crate::profile_card::{
    CardAsk, CardOf, CardTab, Click, Form, HOVER_CLOSE, HOVER_OPEN, ProfileCard, ProfileCardEvent, ProfileCardParts,
};

/// The card open, with where it opened from.
struct Open {
    view: Entity<ProfileCard>,
    _events: Subscription,
    anchor: gpui::Bounds<gpui::Pixels>,
    /// Opened by a click: the pointer leaving doesn't close it.
    sticky: bool,
}

/// The window's card and the pointer's way over faces and names.
#[derive(Default)]
pub(super) struct State {
    open: Option<Open>,
    /// A face or a name the pointer rests on, until its card opens.
    waiting: Option<(String, Task<()>)>,
    /// The card closing, unless the pointer comes back.
    closing: Option<Task<()>>,
    /// The face or name the pointer is on, by the person's MRI.
    on_anchor: Option<String>,
    /// The pointer is on the card.
    on_card: bool,
    /// The 1:1 chat whose call was asked as a video call: its camera goes on once its media is up.
    video: Option<String>,
    _follows: Vec<Subscription>,
}

impl State {
    /// Following the asks of `window` and the calls (for a video call's camera).
    pub(super) fn following(follows: Vec<Subscription>) -> Self {
        Self {
            _follows: follows,
            ..Self::default()
        }
    }
}

impl Shell {
    /// The window's cards, following the asks from its views and the calls; the title bar's search handed its
    /// translations (`previews`) and where a person found asks for their card.
    pub(super) fn card_host(
        models: &Models,
        (search, previews): (&Entity<GlobalSearch>, Entity<crate::chat_translate::Previews>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> State {
        search.update(cx, |search, cx| {
            search.set_previews(previews, cx);
            search.set_cards(models.cards.clone());
        });
        let id = window.window_handle().window_id();
        State::following(vec![
            cx.subscribe_in(
                &models.cards,
                window,
                move |this: &mut Self, _, ask: &CardAsk, window, cx| {
                    if ask.window() == id {
                        this.on_card_ask(ask.clone(), window, cx);
                    }
                },
            ),
            cx.observe(&models.calls, |this: &mut Self, _, cx| this.camera_for_video(cx)),
        ])
    }

    fn on_card_ask(&mut self, ask: CardAsk, window: &mut Window, cx: &mut Context<Self>) {
        match ask {
            CardAsk::Hover { of, anchor, .. } => self.card_hovered(of, anchor, window, cx),
            CardAsk::Left { mri, .. } => {
                if self.cards.on_anchor.as_ref() == Some(&mri) {
                    self.cards.on_anchor = None;
                }
                if self.cards.waiting.as_ref().is_some_and(|(waiting, _)| *waiting == mri) {
                    self.cards.waiting = None;
                }
                self.close_card_later(window, cx);
            }
            CardAsk::Open { of, anchor, form, .. } => {
                self.cards.waiting = None;
                self.open_card(of, (form, CardTab::Overview), anchor, window, cx);
                if let Some(open) = &mut self.cards.open {
                    open.sticky = true;
                }
            }
        }
    }

    /// The pointer came onto a face or a name: its compact card opens if it rests there, one open stays.
    fn card_hovered(
        &mut self,
        of: CardOf,
        anchor: gpui::Bounds<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cards.on_anchor = Some(of.mri.clone());
        if let Some(open) = &self.cards.open
            && (open.view.read(cx).of().mri == of.mri || open.view.read(cx).form() == Form::Full)
        {
            self.cards.closing = None;
            return;
        }
        let mri = of.mri.clone();
        let wait = cx.spawn_in(window, async move |this, cx: &mut gpui::AsyncWindowContext| {
            cx.background_executor().timer(HOVER_OPEN).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.cards.waiting = None;
                if this.cards.on_anchor.as_ref() == Some(&of.mri) {
                    this.open_card(of, (Form::Compact, CardTab::Overview), anchor, window, cx);
                }
            });
        });
        self.cards.waiting = Some((mri, wait));
    }

    /// Closes a compact card opened by hover a moment after the pointer left it and what it opened from, unless it
    /// comes back to either.
    fn close_card_later(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = &self.cards.open else { return };
        if open.sticky || open.view.read(cx).form() == Form::Full {
            return;
        }
        self.cards.closing = Some(
            cx.spawn_in(window, async move |this, cx: &mut gpui::AsyncWindowContext| {
                cx.background_executor().timer(HOVER_CLOSE).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.cards.closing = None;
                    if !this.cards.on_card && this.cards.on_anchor.is_none() {
                        this.close_card(window, cx);
                    }
                });
            }),
        );
    }

    /// Opens `of`'s card in `form` (the full card on `tab`) from `anchor`, in place of any card open; nothing for
    /// someone without a card.
    pub fn open_card(
        &mut self,
        of: CardOf,
        (form, tab): (Form, CardTab),
        anchor: gpui::Bounds<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !teams_core::profile_card::has_card(&of.mri) {
            return;
        }
        let parts = ProfileCardParts {
            core: Arc::clone(&self.core),
            models: self.models.clone(),
            trace: Arc::clone(&self.trace),
            list: self.list.clone(),
        };
        let view = cx.new(|cx| ProfileCard::new(parts, of, form, tab, window, cx));
        let events = cx.subscribe_in(
            &view,
            window,
            |this: &mut Self, _, event: &ProfileCardEvent, window, cx| {
                this.on_card(event.clone(), window, cx);
            },
        );
        view.update(cx, |view, cx| view.focus(window, cx));
        self.cards.closing = None;
        self.cards.on_card = false;
        self.cards.open = Some(Open {
            view,
            _events: events,
            anchor,
            sticky: form == Form::Full,
        });
        cx.notify();
    }

    /// The card open, if any.
    #[must_use]
    pub fn profile_card(&self) -> Option<&Entity<ProfileCard>> {
        self.cards.open.as_ref().map(|open| &open.view)
    }

    /// Closes the card open, the keyboard back to the shell's place.
    pub fn close_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cards.closing = None;
        self.cards.on_card = false;
        if self.cards.open.take().is_some() {
            self.take_focus(window, cx);
            cx.notify();
        }
    }

    fn on_card(&mut self, event: ProfileCardEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            ProfileCardEvent::Close => self.close_card(window, cx),
            ProfileCardEvent::OpenChat(chat) => {
                self.close_card(window, cx);
                self.open_chat(chat, window, cx);
            }
            ProfileCardEvent::StartChat(person) => {
                self.close_card(window, cx);
                self.open_new_chat_with(person, window, cx);
            }
            ProfileCardEvent::Call { chat, video } => {
                self.close_card(window, cx);
                self.cards.video = video.then(|| chat.clone());
                self.ask_call(Ask::Call { chat }, cx);
            }
            ProfileCardEvent::Expand(tab) => {
                let Some(open) = &self.cards.open else { return };
                let (of, anchor) = (open.view.read(cx).of().clone(), open.anchor);
                self.open_card(of, (Form::Full, tab), anchor, window, cx);
            }
            ProfileCardEvent::Engaged => {
                if let Some(open) = &mut self.cards.open {
                    open.sticky = true;
                }
                self.cards.closing = None;
            }
            ProfileCardEvent::Hovered(on) => {
                self.cards.on_card = on;
                if on {
                    self.cards.closing = None;
                } else {
                    self.close_card_later(window, cx);
                }
            }
        }
    }

    /// Opens the chat with `person` alone: their 1:1 chat, else New message to them (a new chat its first message
    /// makes; none in the read-only mode).
    pub fn open_chat_with(&mut self, person: Recipient, window: &mut Window, cx: &mut Context<Self>) {
        let chat = crate::profile_card::chat_with(self.list.read(cx), self.core.me(), &person.mri);
        match chat {
            Some(chat) => self.open_chat(chat, window, cx),
            None => self.open_new_chat_with(person, window, cx),
        }
    }

    /// A video call's camera, turned on once its call's media is up; dropped once that call is gone or another came.
    fn camera_for_video(&mut self, cx: &mut Context<Self>) {
        let Some(chat) = self.cards.video.clone() else { return };
        let calls = self.models.calls.read(cx);
        match calls.call() {
            Some(call) if call.chat.as_deref() == Some(chat.as_str()) => {
                if call.connected_at.is_none() {
                    return;
                }
                let off = !call.camera;
                self.cards.video = None;
                if off {
                    self.ask_call(Ask::ToggleCamera, cx);
                }
            }
            Some(_) => self.cards.video = None,
            None if !calls.busy() => self.cards.video = None,
            None => {}
        }
    }

    /// The card open: the compact one beside what it opened from, the full one over the dimmed window.
    pub(super) fn card_layer(&self, window: &Window, cx: &App) -> Option<AnyElement> {
        let open = self.cards.open.as_ref()?;
        let view = open.view.clone();
        if view.read(cx).form() == Form::Full {
            return Some(
                gpui::deferred(
                    div()
                        .id("profile-card-backdrop")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .occlude()
                        .bg(gpui::rgba(0x0000_0080))
                        .child(view),
                )
                .with_priority(3)
                .into_any_element(),
            );
        }
        let (position, corner) = compact_place(open.anchor, window.viewport_size());
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(position)
                    .anchor(corner)
                    .snap_to_window_with_margin(px(8.0))
                    .child(view),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }
}

impl Shell {
    /// A 1:1 chat's header (chat `id`): its face opens the partner's full card on a click, its name their compact
    /// card on a click, either their compact card on a rest, as Teams' header does; none for any other chat.
    pub(super) fn partner_header(
        &self,
        id: &str,
        name: &SharedString,
        palette: Palette,
        cx: &App,
    ) -> Option<gpui::Div> {
        let row = self.list.read(cx).row(id)?;
        if row.kind != crate::chat_list::RowKind::OneToOne {
            return None;
        }
        let mri = row
            .partner
            .clone()
            .or_else(|| crate::chat_list::one_to_one_partner(id, self.core.me()))?;
        let of = CardOf::new(mri, row.named.then(|| row.name.clone())).with_picture(row.picture.clone());
        let asks = &self.models.cards;
        let face = people::avatar(row.named.then_some(&row.name), row.picture.as_ref(), Size::Small);
        let lead = crate::profile_card::anchor("profile-card-from-header-avatar", of.clone(), Click::Full, asks, face);
        let title = crate::profile_card::anchor(
            "profile-card-from-header-name",
            of,
            Click::Compact,
            asks,
            super::pane_title(name.clone()),
        );
        Some(super::pane_header_titled(Some(lead), title, palette))
    }
}

/// Where the compact card goes beside `anchor` in a window of `viewport`: under it, over it when it wouldn't fit under,
/// left of it near the window's right edge.
fn compact_place(
    anchor: gpui::Bounds<gpui::Pixels>,
    viewport: gpui::Size<gpui::Pixels>,
) -> (gpui::Point<gpui::Pixels>, gpui::Anchor) {
    use crate::profile_card::{COMPACT_MOST, COMPACT_WIDTH};
    let gap = px(4.0);
    if anchor.right() + px(COMPACT_WIDTH) > viewport.width && anchor.left() > px(COMPACT_WIDTH) {
        return (gpui::point(anchor.left() - gap, anchor.top()), gpui::Anchor::TopRight);
    }
    if anchor.bottom() + px(COMPACT_MOST) + gap > viewport.height {
        (gpui::point(anchor.left(), anchor.top() - gap), gpui::Anchor::BottomLeft)
    } else {
        (gpui::point(anchor.left(), anchor.bottom() + gap), gpui::Anchor::TopLeft)
    }
}
