//! A meeting chat's tabs in its header, as browser Teams has them: Chat, Shared and Recap after the meeting's name, the
//! one shown underlined. Chat is the conversation; Shared what the chat shared ([`SharedView`]); Recap the meeting's
//! recordings and transcripts ([`RecapView`]), also opened by the meeting's recap card in the chat (View recap, its
//! Transcript chip, its recording's thumbnail). Each tab's view is made when first shown and kept while its chat is;
//! another chat opens on Chat.
//!
//! Any other chat has the same tabs while it holds calls' captions saved on this computer ([`crate::call_transcript`],
//! shown in its Recap), as Teams shows a call's recap in its chat.
//!
//! Teams' other tabs are left out: Q&A, the Meeting Whiteboard, Attendance and Breakout Rooms are apps of their own
//! (Viva Engage, Whiteboard's canvas, the organizer's reports) the app has no reach to, and "+" adds an app to the
//! meeting.

use super::*;
use crate::recap_view::RecapView;
use crate::shared_view::SharedView;
use crate::system_message;

/// A meeting chat's tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChatTab {
    #[default]
    Chat,
    Shared,
    Recap,
}

impl ChatTab {
    /// Every tab, in the header's order.
    pub const ALL: [Self; 3] = [Self::Chat, Self::Shared, Self::Recap];

    /// Its words in the header.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Shared => "Shared",
            Self::Recap => "Recap",
        }
    }

    /// The selectors' and the trace's name for it.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Shared => "shared",
            Self::Recap => "recap",
        }
    }
}

/// The tabs of the meeting chat shown: the one shown, and the views made for it.
#[derive(Default)]
pub(super) struct MeetingTabs {
    pub(super) shown: ChatTab,
    recap: Option<Entity<RecapView>>,
    shared: Option<Entity<SharedView>>,
}

impl Shell {
    /// The tab the meeting chat shown is on (Chat for any other chat).
    #[must_use]
    pub fn chat_tab(&self) -> ChatTab {
        self.meeting_tabs.shown
    }

    /// The Recap tab's view, once it was shown.
    #[must_use]
    pub fn recap_view(&self) -> Option<&Entity<RecapView>> {
        self.meeting_tabs.recap.as_ref()
    }

    /// The Shared tab's view, once it was shown.
    #[must_use]
    pub fn shared_view(&self) -> Option<&Entity<SharedView>> {
        self.meeting_tabs.shared.as_ref()
    }

    /// Another chat was opened: its tabs start on Chat, the other chat's views let go. The same chat opened again keeps
    /// its tab and its views.
    pub(super) fn reset_meeting_tabs(&mut self, chat: &str, cx: &App) {
        let tabs = &self.meeting_tabs;
        let kept = tabs
            .recap
            .as_ref()
            .is_some_and(|recap| recap.read(cx).conversation() == chat)
            || tabs
                .shared
                .as_ref()
                .is_some_and(|shared| shared.read(cx).conversation() == chat);
        if !kept {
            self.meeting_tabs = MeetingTabs::default();
        }
    }

    /// Whether the chat view itself is what the conversation pane shows of its chat (not a meeting's other tab).
    pub(super) fn chat_tab_shown(&self) -> bool {
        self.meeting_tabs.shown == ChatTab::Chat
    }

    /// The meeting chat shown was put away (another view took the pane): its recap's recording stops playing.
    pub(super) fn pause_meeting_tabs(&self, cx: &mut App) {
        if let Some(recap) = &self.meeting_tabs.recap {
            recap.update(cx, RecapView::pause);
        }
    }

    /// The meeting's name as the list now has it, for the recap's file names.
    pub(super) fn retitle_recap(&self, title: Option<SharedString>, cx: &mut App) {
        if let (Some(recap), Some(title)) = (&self.meeting_tabs.recap, title) {
            recap.update(cx, |recap, cx| recap.set_title(title, cx));
        }
    }

    /// Whether chat `id` has the tabs: a meeting's chat, or a chat holding calls' captions saved on this computer
    /// (or on another tab than Chat, so the way back stays).
    fn has_tabs(&self, id: &str, cx: &App) -> bool {
        system_message::is_meeting_chat(id)
            || self.meeting_tabs.shown != ChatTab::Chat
            || self
                .chat
                .as_ref()
                .is_some_and(|chat| chat.read(cx).conversation() == id && !chat.read(cx).saved_transcripts().is_empty())
    }

    /// Shows `tab` of the meeting chat shown, its view made when first shown.
    pub fn show_chat_tab(&mut self, tab: ChatTab, window: &mut Window, cx: &mut Context<Self>) {
        let Main::Chat(id) = &self.main else { return };
        if !self.has_tabs(id, cx) {
            return;
        }
        let id = id.clone();
        self.trace
            .record(&TraceLine::new("chat-tab", "shown").conversation(&id).kind(tab.key()));
        match &self.meeting_tabs.recap {
            None if tab == ChatTab::Recap => {
                let title = self.meeting_title(&id, cx);
                let core = Arc::clone(&self.core);
                let settings = self.settings.clone();
                self.meeting_tabs.recap =
                    Some(cx.new(|cx| RecapView::new(core, id.clone(), title, window, cx).with_settings(settings)));
            }
            // shown again: a recap that held nothing yet is read again (a recording saved since)
            Some(recap) if tab == ChatTab::Recap => recap.update(cx, RecapView::read_again_if_empty),
            // put away: its recording doesn't play on unseen
            Some(recap) => recap.update(cx, RecapView::pause),
            None => {}
        }
        if tab == ChatTab::Shared && self.meeting_tabs.shared.is_none() {
            let core = Arc::clone(&self.core);
            self.meeting_tabs.shared = Some(cx.new(|cx| SharedView::new(core, id.clone(), window, cx)));
        }
        self.meeting_tabs.shown = tab;
        if let Some(chat) = &self.chat {
            chat.update(cx, |chat, cx| chat.set_shown(tab == ChatTab::Chat, cx));
        }
        cx.notify();
    }

    /// The recap card's View recap, Transcript or thumbnail: the Recap tab, at the card's recording when it names one,
    /// showing `open` (its transcript from the top, or its recording playing).
    pub fn open_recap(
        &mut self,
        recording: Option<String>,
        open: crate::chat_view::RecapOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_chat_tab(ChatTab::Recap, window, cx);
        let Some(recap) = &self.meeting_tabs.recap else { return };
        recap.update(cx, |recap, cx| {
            if let Some(recording) = recording {
                recap.show_recording(recording, cx);
            }
            match open {
                crate::chat_view::RecapOpen::Recap => {}
                crate::chat_view::RecapOpen::Transcript => recap.show_transcript(cx),
                crate::chat_view::RecapOpen::Play => recap.play_when_shown(cx),
            }
        });
    }

    /// A call's end row's Transcript chip: the Recap tab, at the chat's call captions saved on this computer by `id`.
    pub fn open_saved_transcript(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.show_chat_tab(ChatTab::Recap, window, cx);
        if let Some(recap) = &self.meeting_tabs.recap {
            recap.update(cx, |recap, cx| recap.show_saved(id, cx));
        }
    }

    /// The chat's name as the list has it.
    fn meeting_title(&self, id: &str, cx: &App) -> SharedString {
        self.list
            .read(cx)
            .row(id)
            .map_or_else(|| SharedString::from(system_message::MEETING), |row| row.name.clone())
    }

    /// The header's tabs of a meeting chat; none for any other chat.
    pub(super) fn meeting_tab_bar(&self, id: &str, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.has_tabs(id, cx) {
            return None;
        }
        let shown = self.meeting_tabs.shown;
        let tabs = ChatTab::ALL.into_iter().map(|tab| {
            let chosen = tab == shown;
            let key = tab.key();
            div()
                .id(SharedString::from(format!("chat-tab-{key}")))
                .debug_selector(move || format!("chat-tab-{key}{}", if chosen { " shown" } else { "" }))
                .h(px(48.0))
                .px_1()
                .flex()
                .items_center()
                .cursor_pointer()
                .relative()
                .border_b_2()
                .border_color(gpui::Hsla::transparent_black())
                // the chosen tab's line in the brand gradient, growing out from its middle
                .child(crate::motion::toggle(
                    div()
                        .absolute()
                        .bottom(px(-2.0))
                        .h(px(2.0))
                        .rounded_full()
                        .bg(crate::theme::brand_gradient(palette, 90.0)),
                    SharedString::from(format!("chat-tab-{key}-line")),
                    chosen,
                    cx,
                    |line, t| {
                        let inset = gpui::relative((1.0 - t.clamp(0.0, 1.0)) / 2.0);
                        line.left(inset).right(inset)
                    },
                ))
                .text_color(rgb(if chosen {
                    palette.foreground
                } else {
                    palette.muted_foreground
                }))
                .when(chosen, |tab| tab.font_weight(gpui::FontWeight::SEMIBOLD))
                .hover(move |tab| tab.text_color(rgb(palette.foreground)))
                .child(tab.label())
                .on_click(cx.listener(move |this, _, window, cx| this.show_chat_tab(tab, window, cx)))
        });
        Some(
            div()
                .debug_selector(|| "chat-tabs".to_owned())
                .h_full()
                .flex()
                .items_center()
                .gap_4()
                .children(tabs)
                .into_any_element(),
        )
    }

    /// What the conversation pane shows under a meeting chat's header on its tab other than Chat.
    pub(super) fn meeting_tab_body(&self) -> Option<AnyElement> {
        match self.meeting_tabs.shown {
            ChatTab::Chat => None,
            ChatTab::Recap => self.meeting_tabs.recap.clone().map(IntoElement::into_any_element),
            ChatTab::Shared => self.meeting_tabs.shared.clone().map(IntoElement::into_any_element),
        }
    }
}
