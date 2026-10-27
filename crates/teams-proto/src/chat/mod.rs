//! The chat service's reads and the message model (M1 Task 10), from the shapes `docs/protocol/chat.md` records:
//!
//! - [`ChatReads`]: the conversation list, message pages (newest, around a time, and the service's own sync and
//!   paging links, followed only on the same chat base), the thread read and the member list, each a `GET` through
//!   the native transport;
//! - the model ([`Conversation`], [`Message`], [`Thread`], …): unknown fields kept, unknown kinds kept as their text,
//!   never a parse failure for what this build doesn't know;
//! - [`RichText`]: a message's HTML as a small typed model (text runs, mentions, quotes, links, images), with unknown
//!   markup kept as text and nothing active surviving;
//! - [`MessageText`]: what a message is written from (M1 Task 12), a [`RichText`] and the MRI each of its mentions
//!   names, from which a post's or an edit's `content`, `properties.mentions` and target are all built.
//!
//! Received attachments are parsed, never fetched: `amsreferences`, `properties.files` and `properties.cards` with an
//! entry are kept as they came ([`Unshown`]: no capture showed one, so a view says the message has content it can't
//! show and never guesses at it), and an image's `src`, `itemtype`, `itemid` and size are kept on [`Image`] (a view asks the
//! engine for a pasted picture's bytes or an emoticon's, never this crate).

mod compose;
mod event;
mod html;
mod model;
mod read;

pub use compose::{ComposeError, MessageText};
pub use event::{
    CALL_MEDIA_PREFIX, CallEnd, CallStart, ChatEvent, ChatPolicy, Member, Members, Recording, RecordingContents,
    RecordingStatus, Removed, TopicUpdate, Transcript, chat_event, is_call_media,
};
pub use html::{
    AMS_IMAGE_ITEMTYPE, EMOJI_ITEMTYPE, Image, MAX_CONTENT, MAX_DEPTH, MENTION_ITEMTYPE, Marks, Node, Quote,
    REPLY_ITEMTYPE, RichText, ThreadActivity, parse as parse_html, thread_activity,
};
pub use model::{
    ConsumptionHorizon, Conversation, ConversationKind, ConversationList, Field, Malformed, MemberList, Mention,
    MentionKind, Message, MessagePage, MessageType, QuotedMessage, Reaction, ReactionUser, Thread, ThreadMember,
    Unshown, WEB_CLIENT, unshown_parts, web_chat_link,
};
pub use read::{ChatReadError, ChatReads, MAX_PAGE_SIZE, link_endpoint};
