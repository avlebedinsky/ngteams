//! A chat's calls' captions saved on this computer ([`crate::call_transcript`]): read with the chat, and again as they
//! change, each linked from its call's end row ([`crate::call_transcript::attach`]) by a Transcript chip marked
//! "Saved on this computer"; a click opens it in the chat's Recap tab ([`ChatViewEvent::OpenSavedTranscript`]). A
//! meeting's end whose recap card carries Teams' own transcript links none: Teams' wins. A call the chat has no end row
//! of (Teams posts none for a 1:1 call: its history is the Calls app's) gets a "Call ended" row of the app's own at the
//! transcript's end, drawn as Teams' are, with the chip; never while its call still runs, and never sent anywhere (no
//! action is offered on a system row, and a read receipt goes only to someone else's posted message).

use std::collections::{HashMap, HashSet};

use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
    Subscription, div, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::Button;
use teams_proto::chat::{ChatEvent, chat_event};
use teams_store::{CallTranscript, Message};

use super::{AVATAR, AVATAR_GAP, Body, ChatView, ChatViewEvent, MessageItem, Place};
use crate::call_transcript::{self, SAVED_HERE, TRANSCRIPT, TranscriptsChanged};
use crate::icons::{Glyph, selected_by};
use crate::theme::Palette;

/// The chat's saved transcripts and the rows linking them.
#[derive(Default)]
pub(super) struct Saved {
    transcripts: Vec<CallTranscript>,
    /// The call end rows that link a saved transcript, by the row's message id.
    links: HashMap<String, i64>,
    _changed: Option<Subscription>,
}

impl Saved {
    /// Follows the saved transcripts' changes: the view's own chat's are read again.
    pub(super) fn follow(cx: &mut Context<ChatView>) -> Self {
        let saved = call_transcript::saved(cx);
        let changed = cx.subscribe(&saved, |this: &mut ChatView, _, changed: &TranscriptsChanged, cx| {
            if changed.account == this.core.me() && changed.conversation == this.conversation {
                this.read_saved(cx);
            }
        });
        Self {
            _changed: Some(changed),
            ..Self::default()
        }
    }
}

impl ChatView {
    /// The chat's calls' captions saved on this computer, as last read.
    #[must_use]
    pub fn saved_transcripts(&self) -> &[CallTranscript] {
        &self.saved.transcripts
    }

    /// The saved transcript the row of message `id` links, when it does.
    #[must_use]
    pub fn saved_link(&self, id: &str) -> Option<i64> {
        self.saved.links.get(id).copied()
    }

    /// Reads the chat's saved transcripts (a chat's; a channel has no calls of the account's own); the rows link them.
    pub(super) fn read_saved(&mut self, cx: &mut Context<Self>) {
        if self.place != Place::Chat {
            return;
        }
        let reply = self.core.call_transcripts(self.conversation.clone());
        cx.spawn(async move |this, cx| {
            let saved = reply.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if this.saved.transcripts != saved {
                    this.saved.transcripts = saved;
                    this.rebuild(cx);
                }
            });
        })
        .detach();
    }

    /// Links the saved transcripts from the call end rows of `messages`; the rows of the app's own for those left (a
    /// call whose end the chat doesn't hold), the one of a call still running in the chat left out.
    pub(super) fn saved_rows(&mut self, messages: &[Message], cx: &App) -> Vec<Message> {
        self.link_saved(messages);
        let linked: HashSet<i64> = self.saved.links.values().copied().collect();
        let me = self.core.me().to_owned();
        let running = self
            .models
            .calls
            .read(cx)
            .call()
            .filter(|active| !matches!(active.status, teams_core::CallStatus::Ended(_)))
            .and_then(|active| call_transcript::chat_of(active, &me))
            .is_some_and(|chat| chat == self.conversation);
        let newest = self.saved.transcripts.last().map(|transcript| transcript.id);
        let mut rows = Vec::new();
        for transcript in &self.saved.transcripts {
            if linked.contains(&transcript.id) || (running && Some(transcript.id) == newest) {
                continue;
            }
            let row = saved_row(&self.conversation, &me, transcript);
            self.saved.links.insert(row.id.clone(), transcript.id);
            rows.push(row);
        }
        rows
    }

    /// Which call end rows of `messages` link which saved transcripts.
    fn link_saved(&mut self, messages: &[Message]) {
        if self.saved.transcripts.is_empty() {
            self.saved.links.clear();
            return;
        }
        let ends: Vec<(String, i64)> = messages
            .iter()
            .filter(|message| message.message_type == "Event/Call")
            .filter(|message| {
                matches!(
                    chat_event(&message.message_type, &message.content),
                    Some(ChatEvent::CallEnded(_))
                )
            })
            .map(|message| (message.id.clone(), message.arrival_time))
            .collect();
        self.saved.links = call_transcript::attach(&self.saved.transcripts, &ends);
    }

    /// The Transcript chip under a call's end row linking a saved transcript; none where Teams' recap card already
    /// carries its own transcript.
    pub(super) fn saved_chip(
        &self,
        item: &MessageItem,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let id = self.saved_link(&item.id)?;
        if let Body::MeetingEnded(end) = &item.body
            && end.recap.as_ref().is_some_and(|recap| recap.transcript)
        {
            return None;
        }
        let selector = format!("saved-transcript-{}", item.id);
        let open = Button::new(SharedString::from(selector.clone()))
            .icon(Glyph::TRANSCRIPT.icon())
            .label(TRANSCRIPT)
            .outline()
            .small()
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatViewEvent::OpenSavedTranscript(id))));
        Some(
            div()
                .w_full()
                .pl(px(AVATAR + AVATAR_GAP))
                .pb_2()
                .flex()
                .items_center()
                .gap_3()
                .child(selected_by(selector, open))
                .child(
                    div()
                        .debug_selector(|| "saved-transcript-mark".to_owned())
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_xs()
                        .text_color(rgb(palette.muted_foreground))
                        .child(Glyph::MONITOR.icon().size(px(12.0)))
                        .child(SAVED_HERE),
                )
                .into_any_element(),
        )
    }
}

/// How the rows of the app's own for saved transcripts are named: never a message id of Teams'.
pub const SAVED_ROW: &str = "saved-call-";

/// `shown` with the app's own `rows` among them by their time.
pub(super) fn with_saved<'m>(mut shown: Vec<&'m Message>, rows: &'m [Message]) -> Vec<&'m Message> {
    if !rows.is_empty() {
        shown.extend(rows);
        shown.sort_by_key(|message| message.arrival_time);
    }
    shown
}

/// The app's own "Call ended" row of `transcript`'s call in `conversation`, as Teams' call end reads (the account
/// `me` in it, its length the transcript's span).
fn saved_row(conversation: &str, me: &str, transcript: &CallTranscript) -> Message {
    let seconds = (transcript.ended_at - transcript.started_at).max(0) / 1_000;
    Message {
        conversation_id: conversation.to_owned(),
        id: format!("{SAVED_ROW}{}", transcript.id),
        version: 1,
        client_message_id: None,
        sender: Some(me.to_owned()),
        sender_name: None,
        message_type: "Event/Call".to_owned(),
        content: format!(
            "<ended/><partlist alt=\"\" count=\"1\"><part identity=\"{me}\"><name>{me}</name>\
             <duration>{seconds}</duration></part></partlist>"
        ),
        properties: None,
        arrival_time: transcript.ended_at,
        compose_time: Some(transcript.ended_at),
        edit_time: None,
        delete_time: None,
    }
}
