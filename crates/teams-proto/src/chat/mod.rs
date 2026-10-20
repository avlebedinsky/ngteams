//! The chat service's reads and the message model (M1 Task 10), from the shapes `docs/protocol/chat.md` records:
//!
//! - [`ChatReads`]: the conversation list, message pages (newest, around a time, and the service's own sync and
//!   paging links, followed only on the same chat base), the thread read and the member list, each a `GET` through
//!   the native transport;
//! - the model ([`Conversation`], [`Message`], [`Thread`], …): unknown fields kept, unknown kinds kept as their text,
//!   never a parse failure for what this build doesn't know;
//! - [`RichText`]: a message's HTML as a small typed model (text runs, mentions, quotes, links, images), with unknown
//!   markup kept as text and nothing active surviving.
//!
//! Received attachments are parsed, never fetched: `amsreferences` and `properties.files` are kept as they come (no
//! capture showed one), and an image's `src` is kept on [`Image`].

mod html;
mod model;
mod read;

pub use html::{
    Image, MAX_CONTENT, MAX_DEPTH, MENTION_ITEMTYPE, Marks, Node, Quote, REPLY_ITEMTYPE, RichText, ThreadActivity,
    parse as parse_html, thread_activity,
};
pub use model::{
    ConsumptionHorizon, Conversation, ConversationKind, ConversationList, Field, Malformed, MemberList, Mention,
    MentionKind, Message, MessagePage, MessageType, QuotedMessage, Reaction, ReactionUser, Thread, ThreadMember,
};
pub use read::{ChatReadError, ChatReads, MAX_PAGE_SIZE, link_endpoint};
