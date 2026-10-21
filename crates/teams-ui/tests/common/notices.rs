//! A fake notification server that records what it is asked, and the made-up chats and messages the notification
//! tests (`notify.rs`, `desktop.rs`) hand the fake engine.

use std::sync::{Mutex, PoisonError};

use futures_util::future::BoxFuture;
use teams_store::{Conversation, ConversationKind, Message};
use teams_ui::notify::{Notice, NotificationServer};

/// What the fake server was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    Show { notice: Notice, replaces: u32, id: u32 },
    Close(u32),
}

/// A notification server that records every ask and answers ids from 1 up.
#[derive(Default)]
pub struct FakeServer {
    asked: Mutex<Vec<Asked>>,
}

impl FakeServer {
    pub fn asked(&self) -> Vec<Asked> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn shown(&self) -> Vec<Notice> {
        self.asked()
            .into_iter()
            .filter_map(|asked| match asked {
                Asked::Show { notice, .. } => Some(notice),
                Asked::Close(_) => None,
            })
            .collect()
    }

    pub fn closed(&self) -> Vec<u32> {
        self.asked()
            .into_iter()
            .filter_map(|asked| match asked {
                Asked::Close(id) => Some(id),
                Asked::Show { .. } => None,
            })
            .collect()
    }
}

impl NotificationServer for FakeServer {
    fn show(&self, notice: &Notice, replaces: u32) -> BoxFuture<'static, Result<u32, String>> {
        let mut asked = self.asked.lock().unwrap_or_else(PoisonError::into_inner);
        let shows = asked.iter().filter(|asked| matches!(asked, Asked::Show { .. })).count();
        let id = if replaces == 0 {
            u32::try_from(shows).unwrap() + 1
        } else {
            replaces
        };
        asked.push(Asked::Show {
            notice: notice.clone(),
            replaces,
            id,
        });
        Box::pin(async move { Ok(id) })
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        self.asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Asked::Close(id));
        Box::pin(async {})
    }
}

pub fn chat(id: &str, kind: ConversationKind, topic: Option<&str>) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: topic.map(str::to_owned),
        version: 1,
        last_activity: Some(1_000),
    }
}

pub fn message(conversation: &str, id: &str, sender: &str, content: &str, arrival: i64) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: arrival,
        compose_time: Some(arrival),
        edit_time: None,
        delete_time: None,
    }
}
