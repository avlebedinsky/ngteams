use std::collections::HashMap;
use std::sync::Arc;

use teams_core::{Action, Reply};
use teams_proto::Mri;
use teams_proto::chat::MessageText;
use teams_store::{OutboxEntry, OutboxKind, OutboxState};

use super::Target;
use crate::bridge::Core;
use crate::sending::{chat_id, settled_row};

pub(super) async fn refresh(core: &dyn Core, targets: &[Target], retries: &mut HashMap<String, OutboxEntry>) -> bool {
    for target in targets {
        if let Some(previous) = retries.get_mut(target.key()) {
            let Ok(Some(row)) = core.outbox_entry(previous.id).await else {
                return false;
            };
            *previous = row;
        }
    }
    true
}

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
        let row = if previous.state == OutboxState::Sent {
            previous
        } else {
            retry(core.as_ref(), previous.id, cx).await?
        };
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

async fn retry(core: &dyn Core, id: i64, cx: &gpui::AsyncApp) -> Option<OutboxEntry> {
    let row = core.outbox_entry(id).await.ok()??;
    let reply = if row.state == OutboxState::Failed {
        core.resend(id)
    } else {
        Reply::ready(Ok(row))
    };
    if let Some(row) = settled_row(core, reply, cx.background_executor()).await {
        return Some(row);
    }
    let row = core.outbox_entry(id).await.ok()??;
    settled_row(core, Reply::ready(Ok(row)), cx.background_executor()).await
}
