//! What a call that ended leaves in its parties' own streams (`docs/server.md`, "The call log"): an entry in each
//! party's call log (`48:calllogs`), and for each callee who never took it (who neither answered nor declined) a
//! missed call's item in the activity feed (`48:notifications`), "Missed call from <caller>". No capture shows the
//! service writing either: both take the shapes Teams' own streams were seen to hold.

use serde_json::{Value, json};

use super::state::{Call, Kind, Party, Seat, Status};
use super::wire::iso;
use crate::app::App;
use crate::chatsvc::service::{self, Activity};
use crate::db::DbError;

/// How the call went for one party, as its call log tells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Taken {
    Accepted,
    Declined,
    Missed,
}

impl Taken {
    fn of(party: &Party) -> Self {
        if party.joined.is_some() {
            Self::Accepted
        } else if party.status == Status::Declined {
            Self::Declined
        } else {
            Self::Missed
        }
    }

    /// How the call went for the party at `seat`: the caller of a group call is in it from its start, but it took
    /// the call only when some other party came in too, else it is `missed`, as the caller of a 1:1 call nobody took.
    fn in_call(call: &Call, seat: Seat) -> Self {
        let others_in = || {
            call.parties
                .iter()
                .enumerate()
                .any(|(number, party)| number != Seat::CALLER.0 && party.joined.is_some())
        };
        if seat == Seat::CALLER && call.kind == Kind::Group && !others_in() {
            return Self::Missed;
        }
        Self::of(call.party(seat))
    }

    fn name(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Missed => "missed",
        }
    }
}

/// A party as a call log entry names it; only the caller is named by name, as Teams' entries name them.
fn participant(party: &Party, tenant: &str, named: bool) -> Value {
    json!({
        "id": party.mri(), "type": "default", "displayName": named.then_some(party.name.as_str()),
        "tenantId": tenant, "alternateId": null, "applicationType": null,
    })
}

/// The call log entry of the party at `seat` (`call-log`'s JSON).
fn entry(call: &Call, seat: Seat, tenant: &str) -> Value {
    let (party, caller) = (call.party(seat), call.party(Seat::CALLER));
    let (kind, target, everyone, thread) = match call.kind {
        Kind::OneToOne => ("twoParty", Some(call.party(Seat::CALLEE)), None, None),
        Kind::Group => ("multiParty", None, Some(&call.parties), call.chat.as_deref()),
    };
    let taken = Taken::in_call(call, seat);
    let participant_id = party
        .participant
        .get("participantId")
        .and_then(Value::as_str)
        .map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned);
    json!({
        "startTime": iso(call.started_at),
        "connectTime": party.joined.as_ref().filter(|_| taken == Taken::Accepted),
        "endTime": iso(call.ended_at.unwrap_or(call.started_at)),
        "callId": call.id,
        "callDirection": if seat == Seat::CALLER { "outgoing" } else { "incoming" },
        "callType": kind,
        "callState": taken.name(),
        "originator": caller.mri(),
        "originatorParticipant": participant(caller, tenant, true),
        "target": target.map(Party::mri),
        "targetParticipant": target.map(|target| participant(target, tenant, false)),
        "participants": everyone.map(|all| all.iter().map(Party::mri).collect::<Vec<_>>()),
        "participantList": everyone.map(|all| all.iter().map(|one| participant(one, tenant, false)).collect::<Vec<_>>()),
        "threadId": thread,
        "messageId": null,
        "sessionType": "default",
        "sharedCorrelationId": call.conversation,
        "userParticipantId": participant_id,
        "callAttributes": null,
        "forwardingInfo": null,
        "transferInfo": null,
    })
}

/// A missed call's activity (`properties.activity`) for `to`, under the id `at` its feed gives it.
fn missed(call: &Call, to: &str, at: i64) -> Value {
    let caller = call.party(Seat::CALLER);
    json!({
        "activityType": "call", "activitySubtype": "missedCall",
        "activityTimestamp": iso(call.ended_at.unwrap_or(at)), "activityId": at,
        "sourceThreadId": "", "sourceMessageId": 1,
        "sourceUserId": caller.mri(), "sourceUserImDisplayName": caller.name,
        "targetUserId": format!("8:orgid:{to}"),
        "messagePreview": "", "messagePreviewTemplateOption": "previewUnavailable",
        "activityContext": {
            "callModalities": "Audio",
            "ClumpId": "nr:system:clump:Calls", "ClumpType": "Calls", "ClumpTitle": "Calls",
        },
        "sourceThreadIsPrivateChannel": "false",
    })
}

/// Writes what the ended call `call` leaves each party: its call log entry, and a missed call's activity item for
/// each callee who never took it. One not written is logged: the call itself is over either way.
pub(super) async fn tell(app: &App, call: &Call) {
    let logged = log(app, call).await;
    let fed = tell_missed(app, call).await;
    for (what, written) in [("the call log", logged), ("a missed call's activity", fed)] {
        if let Err(error) = written {
            tracing::warn!(%error, what, "a call's end wasn't written to its parties' streams");
        }
    }
}

/// Each party's call log entry.
async fn log(app: &App, call: &Call) -> Result<(), DbError> {
    let tenant = crate::db::meta::tenant(&app.db).await?;
    let caller = call.party(Seat::CALLER).mri();
    let people: Vec<String> = call.parties.iter().map(|party| party.id.clone()).collect();
    let content = format!("Call Logs for Call {}", call.id);
    let logged_for = |to: &str, at: i64| {
        let seat = call.seat_of(to).unwrap_or(Seat::CALLER);
        let properties = json!({"call-log": entry(call, seat, &tenant).to_string(), "s2spartnername": "concore_gvc"});
        service::stream_message(app, service::CALL_LOG, (at, &caller), &content, &properties)
    };
    service::tell_call_log(app, &people, &logged_for).await
}

/// A missed call's activity item for each callee who never took the call.
async fn tell_missed(app: &App, call: &Call) -> Result<(), DbError> {
    let missed_by: Vec<String> = call
        .parties
        .iter()
        .skip(1)
        .filter(|party| Taken::of(party) == Taken::Missed)
        .map(|party| party.id.clone())
        .collect();
    if missed_by.is_empty() {
        return Ok(());
    }
    let caller = call.party(Seat::CALLER).mri();
    let item = |to: &str, at: i64| {
        let properties = json!({"activity": missed(call, to, at), "isread": false, "messageUpdatePolicyValue": 1});
        service::stream_message(app, service::FEED, (at, &caller), "", &properties)
    };
    let told = Activity {
        people: &missed_by,
        thread: None,
        personal: true,
        item: &item,
    };
    service::tell_activity(app, told).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn party(id: &str, joined: bool, status: Status) -> Party {
        Party {
            id: id.to_owned(),
            name: format!("Name {id}"),
            joined: joined.then(|| "2026-11-16T10:00:00.0000000Z".to_owned()),
            status,
            ..Party::default()
        }
    }

    #[test]
    fn a_party_took_the_call_when_it_came_in_and_missed_it_unless_it_declined() {
        assert_eq!(Taken::of(&party("a", true, Status::Left)), Taken::Accepted);
        assert_eq!(
            Taken::of(&party("a", true, Status::Declined)),
            Taken::Accepted,
            "declined a ring, then joined"
        );
        assert_eq!(Taken::of(&party("b", false, Status::Declined)), Taken::Declined);
        assert_eq!(Taken::of(&party("c", false, Status::Missed)), Taken::Missed);
        assert_eq!(Taken::of(&party("d", false, Status::Ringing)), Taken::Missed);
    }
}
