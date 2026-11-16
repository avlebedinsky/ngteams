//! Bot invokes (`docs/protocol/bots.md`): a card's button, a message extension's command or dialog, asked of an app's
//! bot through the account's own chat service. The engine names the asker; the transport adds a Microsoft account's
//! Teams service token, and an own-server account's none. Each goes out once through the outbox, with no row: an
//! invoke may make the app act, so an unknown outcome is told, never sent again, and nothing is resent by a later run.
use teams_proto::{BotInvoke, Credentials, HttpError, InvokeAnswer, InvokeKind, Mri, Transport};

use crate::channels::Channels;
use crate::engine::{EngineError, Responder};
use crate::outbox::{CallTried, Outbox};

/// What a view asks of a bot.
#[derive(Clone, Debug, PartialEq)]
pub struct BotAsk {
    /// The bot, `28:<bot id>`.
    pub bot: Mri,
    /// Its app's id.
    pub app_id: String,
    /// The chat, or a channel post's `<channel>;messageid=<post>` chain, it acts in.
    pub conversation: Option<String>,
    pub kind: InvokeKind,
}

impl<C, T> Channels<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + 'static,
{
    pub(crate) fn bot_invoke(&self, ask: BotAsk, reply: Responder<InvokeAnswer>) {
        let outbox = std::sync::Arc::clone(&self.outbox);
        tokio::task::spawn_local(async move {
            let _ = reply.send(invoke(&outbox, ask).await);
        });
    }
}

/// `ask`, sent once.
pub(crate) async fn invoke<T: Transport<Error = HttpError>>(
    outbox: &Outbox<T>,
    ask: BotAsk,
) -> Result<InvokeAnswer, EngineError> {
    let name = ask.kind.name();
    let invoke = BotInvoke::new(ask.bot, ask.app_id, outbox.me().display_name(), ask.kind)
        .ok_or_else(|| not_reached("it names no bot and app"))?;
    let invoke = match ask.conversation {
        Some(conversation) => invoke.in_conversation(conversation),
        None => invoke,
    };
    log::info!("bot invoke {name} to {}", invoke.bot().as_str());
    match outbox.try_bot_invoke(invoke).await {
        CallTried::Answered(answer) => Ok(InvokeAnswer::read(answer.status(), answer.body())),
        CallTried::Denied(reason) => Err(not_reached(&format!("refused: {reason}"))),
        CallTried::NotSent(why) | CallTried::Uncertain(why) => Err(not_reached(&why)),
        CallTried::Throttled(_) => Err(not_reached("the service is busy")),
    }
}

fn not_reached(why: &str) -> EngineError {
    EngineError::BotNotReached { why: why.to_owned() }
}
