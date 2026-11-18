use std::sync::Arc;

use teams_core::Action;
use teams_proto::Mri;
use teams_proto::chat::MessageText;
use teams_store::{OutboxEntry, OutboxKind, OutboxState};

use super::Target;
use crate::bridge::Core;
use crate::sending::{chat_id, settled_row};

pub(super) async fn send_to(
    core: &Arc<dyn Core>,
    target: &Target,
    chats: &[(String, String)],
    text: MessageText,
    previous: Option<OutboxEntry>,
    cx: &mut gpui::AsyncApp,
) -> Option<OutboxEntry> {
    if core.read_only() {
        return None;
    }
    let created = if let Some(previous) = previous {
        let kind = previous.kind;
        let row = retry(core.as_ref(), previous, cx).await;
        if kind == OutboxKind::Send || row.state != OutboxState::Sent {
            return Some(row);
        }
        chat_id(row)
    } else {
        None
    };
    let conversation = match target {
        Target::Chat(row) => row.id.clone(),
        Target::Channel { id, .. } => id.clone(),
        Target::Person { mri, .. } => {
            if let Some(chat) = created.or_else(|| {
                chats
                    .iter()
                    .find(|(partner, _)| partner == mri)
                    .map(|(_, chat)| chat.clone())
            }) {
                chat
            } else {
                let reply = core.write(Action::CreateChat {
                    others: vec![Mri::new(mri)],
                    topic: None,
                });
                let row = settled_row(core.as_ref(), reply, cx.background_executor()).await?;
                if row.state != OutboxState::Sent {
                    return Some(row);
                }
                chat_id(row)?
            }
        }
    };
    let reply = core.write(Action::Send { conversation, text });
    settled_row(core.as_ref(), reply, cx.background_executor()).await
}

async fn retry(core: &dyn Core, previous: OutboxEntry, cx: &gpui::AsyncApp) -> OutboxEntry {
    if previous.state != OutboxState::Failed {
        return previous;
    }
    settled_row(core, core.resend(previous.id), cx.background_executor())
        .await
        .unwrap_or(previous)
}
