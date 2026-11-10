//! The chat header's people ([`crate::chat_people`]): a 1:1 or group chat's "View and add participants" button (a
//! group chat's with its count, as Teams shows it) opens the flyout under it. A 1:1 chat's "Start a group chat" opens
//! the unnamed group chat of exactly its people when the account has one ([`Core::group_chat_with`], as New message
//! finds it), else a new group chat its first message makes ([`Shell::open_group_draft`]); the 1:1 chat stays as it
//! was. Another chat opened closes the flyout.

use super::*;
use crate::chat_list::RowKind;
use crate::chat_people::{ChatPeople, ChatPeopleEvent, ChatPeopleParts, PEOPLE_TIP, PeopleOf, Placed};
use crate::new_chat::Recipient;
use gpui_component::Selectable as _;

/// The flyout, while it is open, and what it tells.
type Open = Option<(Entity<ChatPeople>, Subscription)>;

/// The flyout while it is open, and where the header's people button was last drawn.
#[derive(Default)]
pub(super) struct State {
    pub(super) open: Open,
    button: Placed,
}

impl Shell {
    /// The header's people button for chat `id`: a 1:1 chat's, a group chat's with its count; none for any other.
    pub(super) fn people_header_button(&self, id: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let row = self.list.read(cx).row(id)?;
        let count = match row.kind {
            RowKind::OneToOne => None,
            RowKind::Group => Some(row.members).filter(|count| *count > 0),
            _ => return None,
        };
        let mut button =
            icon_button("chat-header-people", Glyph::USER_PLUS, PEOPLE_TIP).selected(self.people.open.is_some());
        if let Some(count) = count {
            button = button.label(count.to_string());
        }
        let button = button.on_click(cx.listener(|this, _, window, cx| this.toggle_people(window, cx)));
        Some(
            self.people
                .button
                .keep(selected_by("chat-header-people", button))
                .into_any_element(),
        )
    }

    /// Opens the chat shown's people, or closes them.
    pub fn toggle_people(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.people.open.take().is_some() {
            self.take_focus(window, cx);
            cx.notify();
            return;
        }
        let Main::Chat(id) = &self.main else { return };
        let Some(row) = self.list.read(cx).row(id) else { return };
        let of = match row.kind {
            RowKind::OneToOne => PeopleOf::OneToOne,
            RowKind::Group => PeopleOf::Group,
            _ => return,
        };
        let parts = ChatPeopleParts {
            core: Arc::clone(&self.core),
            trace: Arc::clone(&self.trace),
            list: self.list.clone(),
            presence: self.models.presence.clone(),
            button: self.people.button.clone(),
        };
        let chat = id.clone();
        let view = cx.new(|cx| ChatPeople::new(parts, chat, of, Vec::new(), window, cx));
        let events = cx.subscribe_in(
            &view,
            window,
            |this: &mut Self, _, event: &ChatPeopleEvent, window, cx| {
                this.on_people(event.clone(), window, cx);
            },
        );
        view.update(cx, |view, cx| view.focus(window, cx));
        self.people.open = Some((view, events));
        cx.notify();
    }

    /// The chat's people flyout, while it is open.
    #[must_use]
    pub fn people(&self) -> Option<&Entity<ChatPeople>> {
        self.people.open.as_ref().map(|(view, _)| view)
    }

    fn on_people(&mut self, event: ChatPeopleEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            ChatPeopleEvent::Close => {
                self.people.open = None;
                self.take_focus(window, cx);
            }
            ChatPeopleEvent::StartGroup(people) => {
                self.people.open = None;
                self.start_group(people, window, cx);
            }
            // a new group chat's own flyout is New message's
            ChatPeopleEvent::AddToDraft(_) => {}
        }
        cx.notify();
    }

    /// A 1:1 chat's "Start a group chat" with `people` (see the module docs).
    pub fn start_group(&mut self, people: Vec<Recipient>, window: &mut Window, cx: &mut Context<Self>) {
        let mut members: Vec<String> = people.iter().map(|person| person.mri.clone()).collect();
        members.push(self.core.me().to_owned());
        let reply = self.core.group_chat_with(members);
        cx.spawn_in(window, async move |this, cx: &mut gpui::AsyncWindowContext| {
            let found = reply.await.ok().flatten();
            let _ = this.update_in(cx, |this, window, cx| match found {
                Some(chat) => this.open_chat(chat, window, cx),
                None => this.open_group_draft(people, window, cx),
            });
        })
        .detach();
    }

    /// The flyout over the conversation pane, under the header's button, while it is the chat shown's.
    pub(super) fn people_layer(&self, id: &str, cx: &App) -> Option<AnyElement> {
        let (view, _) = self.people.open.as_ref()?;
        // drawn over the chat's own popups (the compose box's pickers), as Teams' flyout is
        (view.read(cx).chat() == id).then(|| {
            gpui::deferred(div().absolute().top(px(50.0)).right(px(12.0)).child(view.clone()))
                .with_priority(1)
                .into_any_element()
        })
    }
}
