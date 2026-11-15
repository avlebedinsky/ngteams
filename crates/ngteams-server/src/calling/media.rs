//! A group call's media node, as signaling reaches it: [`MediaNodes`] picks a node for a new call's room and makes,
//! answers and drops its legs there. Signaling records only what the node answered, so each operation calls the node
//! first (a leg's requests repeat safely by its id) and then makes the step that records it.
//!
//! Nothing in the server implements it yet: [`Mixers`] stays empty and a group call's start is answered 501 until the
//! media node serves. The e2e harness installs a node of its own that answers fixed SDP.

use std::fmt::Debug;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Where a call's room is: the node, how it is reached, and the node's life it was made in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaHome {
    pub node: String,
    pub url: String,
    pub epoch: u64,
}

/// A party's media sources in the call, as signaling numbered them: what the roster names and the node sends as.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sources {
    pub audio: u32,
    pub video: u32,
    pub sharing: u32,
    pub data: u32,
}

/// An offer or an answer, as a call's requests carry it (`mediaContent`'s `blob` and `contentType`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sdp {
    pub blob: String,
    #[serde(rename = "contentType")]
    pub content_type: String,
}

/// Why a media node didn't do what it was asked.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// No node takes a new room, or the node is full.
    #[error("no media node takes it")]
    Unavailable,
    /// The room isn't there any more (the node restarted).
    #[error("the call's room is gone")]
    Gone,
    #[error("the media node failed: {0}")]
    Failed(String),
}

/// The media nodes a signaling node reaches.
#[async_trait]
pub trait MediaNodes: Send + Sync + Debug {
    /// The node a new call's room goes to.
    async fn pick(&self) -> Result<MediaHome, MediaError>;

    /// Makes the leg `leg` of the call `call`'s room (and the room): with `offer` (a caller's, a joiner's) it answers
    /// it; without (a callee's) it makes the node's offer. Asked again, it answers the same.
    async fn put_leg(
        &self,
        home: &MediaHome,
        call: &str,
        leg: &str,
        sources: Sources,
        offer: Option<&Sdp>,
    ) -> Result<Sdp, MediaError>;

    /// A callee's answer to the node's offer.
    async fn answer_leg(&self, home: &MediaHome, call: &str, leg: &str, answer: &Sdp) -> Result<(), MediaError>;

    /// A party's new offer on its leg: the node's answer.
    async fn renegotiate_leg(&self, home: &MediaHome, call: &str, leg: &str, offer: &Sdp) -> Result<Sdp, MediaError>;

    /// The party left, declined or wasn't reached.
    async fn delete_leg(&self, home: &MediaHome, call: &str, leg: &str) -> Result<(), MediaError>;

    /// The call ended.
    async fn delete_room(&self, home: &MediaHome, call: &str) -> Result<(), MediaError>;
}

/// The media nodes this node reaches, once installed (at most once): none, and a group call's start isn't served.
#[derive(Debug, Default)]
pub struct Mixers(OnceLock<Arc<dyn MediaNodes>>);

impl Mixers {
    /// Installs `nodes`; `false` when some were installed already.
    pub fn install(&self, nodes: Arc<dyn MediaNodes>) -> bool {
        self.0.set(nodes).is_ok()
    }

    pub(crate) fn get(&self) -> Option<&Arc<dyn MediaNodes>> {
        self.0.get()
    }
}

/// Drops the call `call`'s room; one not dropped is logged (the node drops a room whose call is over by itself).
pub(crate) async fn drop_room(nodes: &dyn MediaNodes, home: &MediaHome, call: &str) {
    if let Err(error) = nodes.delete_room(home, call).await {
        tracing::warn!(%error, "a call's media room wasn't dropped");
    }
}

/// Drops the leg `leg` of the call `call`'s room; one not dropped is logged.
pub(crate) async fn drop_leg(nodes: &dyn MediaNodes, home: &MediaHome, call: &str, leg: &str) {
    if let Err(error) = nodes.delete_leg(home, call, leg).await {
        tracing::warn!(%error, "a call's media leg wasn't dropped");
    }
}
