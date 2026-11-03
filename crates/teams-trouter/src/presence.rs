//! Others' presence pushed (`docs/protocol/trouter.md`, "Others' presence"; `tests/fixtures/trouter/presence-deliveries.json`):
//! once the account's endpoint subscribed to someone's presence (`teams_proto::PresenceSubscription`), UPS delivers
//! each change of theirs to the messaging stack's socket at `/v4/f/<endpoint>/unifiedPresenceService`, first a
//! snapshot of everyone a subscription named. The body is `{"presence": [{"mri", "presence": {"availability",
//! "activity", …}, …}, …]}`, `"isSnapshot": true` beside the list on a snapshot.
//!
//! A presence delivery is never answered, as no delivery but a call's is: the lab's captures show UPS's deliveries
//! going on for the whole of a run without answers (up to 49 minutes), and the ones left unanswered
//! replayed on a later connect (`"mstore":true`, [`PresenceDelivery::stored`]), as chat deliveries are.

use serde_json::Value;
use teams_proto::Mri;
use teams_proto::UserPresence;

use crate::frames::RawDelivery;

/// A presence delivery: whose presence it gives and as what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenceDelivery {
    /// The socket.io message's `id`, signed.
    pub id: i64,
    /// A stored delivery replayed on connect (`"mstore":true`): a change of up to ten minutes ago, older than what a
    /// read or the next snapshot gives.
    pub stored: bool,
    /// The snapshot that follows a subscription (`isSnapshot`).
    pub snapshot: bool,
    /// Each person's presence as given: an availability or activity of another form than text is none.
    pub presences: Vec<UserPresence>,
}

impl PresenceDelivery {
    /// The delivery `raw` as a presence delivery; `None` when its body isn't an object with a `presence` list whose
    /// every item names its `mri` as text (logged by its size only).
    #[must_use]
    pub fn from_raw(raw: &RawDelivery) -> Option<Self> {
        let presences = read_body(&raw.body);
        if presences.is_none() {
            log::debug!("trouter: a presence delivery of {} bytes isn't read", raw.body.len());
        }
        let (presences, snapshot) = presences?;
        Some(Self {
            id: raw.id,
            stored: raw.is_stored(),
            snapshot,
            presences,
        })
    }
}

/// The presences a delivery's body gives, and whether it is a snapshot.
fn read_body(body: &str) -> Option<(Vec<UserPresence>, bool)> {
    let body: Value = serde_json::from_str(body).ok()?;
    let snapshot = body.get("isSnapshot").and_then(Value::as_bool).unwrap_or(false);
    let presences = body
        .get("presence")?
        .as_array()?
        .iter()
        .map(|item| {
            let text = |key: &str| item["presence"][key].as_str().map(str::to_owned);
            Some(UserPresence {
                mri: Mri::new(item.get("mri")?.as_str()?),
                availability: text("availability"),
                activity: text("activity"),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some((presences, snapshot))
}
