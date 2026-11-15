//! Calls, one row each: the call as JSON beside what it is found and swept by. A row changes only from the version
//! it was read at, so of two nodes that take a step from the same row one finds the row moved and reads it again.

use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::{Value, json};

use super::state::{Call, Seat};
use crate::db::{Db, DbError, with_conn};

diesel::table! {
    calls (id) {
        id -> Text,
        conversation -> Text,
        caller_id -> Text,
        phase -> Text,
        version -> BigInt,
        due_at -> BigInt,
        created_at -> BigInt,
        state -> Text,
    }
}

#[derive(Clone, Debug, Queryable, Selectable, Insertable)]
#[diesel(table_name = calls)]
struct Row {
    id: String,
    conversation: String,
    caller_id: String,
    phase: String,
    version: i64,
    due_at: i64,
    created_at: i64,
    state: String,
}

/// A call and the version its row was read at.
#[derive(Clone, Debug)]
pub(crate) struct Kept {
    pub call: Call,
    pub version: i64,
}

impl Row {
    fn of(call: &Call, version: i64) -> Self {
        Self {
            id: call.id.clone(),
            conversation: call.conversation.clone(),
            caller_id: call.party(Seat::CALLER).id.clone(),
            phase: call.phase.name().to_owned(),
            version,
            due_at: call.due_at,
            created_at: call.started_at / 1000,
            state: serde_json::to_string(call).unwrap_or_default(),
        }
    }

    fn kept(self) -> Option<Kept> {
        let mut state: Value = serde_json::from_str(&self.state).ok()?;
        upgrade(&mut state);
        Some(Kept {
            call: serde_json::from_value(state).ok()?,
            version: self.version,
        })
    }
}

/// A call as the first controller stored it, in today's shape: its `caller` and `callee` the parties, its seats
/// named `"caller"` and `"callee"`.
fn upgrade(state: &mut Value) {
    let Some(fields) = state.as_object_mut() else { return };
    if let (Some(caller), Some(callee)) = (fields.remove("caller"), fields.remove("callee")) {
        fields.insert("parties".into(), json!([caller, callee]));
    }
    let seat = |seat: &mut Value| match seat.as_str() {
        Some("caller") => *seat = json!(0),
        Some("callee") => *seat = json!(1),
        _ => {}
    };
    for pointer in ["/negotiation/by", "/answered/by", "/held_by"] {
        if let Some(named) = state.pointer_mut(pointer) {
            seat(named);
        }
    }
}

/// Which call a request names.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Key<'a> {
    Id(&'a str),
    Conversation(&'a str),
}

pub(super) async fn insert(db: &Db, call: &Call) -> Result<(), DbError> {
    let row = Row::of(call, 1);
    with_conn!(db, |conn| {
        diesel::insert_into(calls::table)
            .values(&row)
            .execute(conn)
            .await
            .map_err(crate::db::conflict)?;
    });
    Ok(())
}

pub(super) async fn find(db: &Db, key: Key<'_>) -> Result<Option<Kept>, DbError> {
    let row: Option<Row> = with_conn!(db, |conn| match key {
        Key::Id(id) => calls::table
            .find(id)
            .select(Row::as_select())
            .first(conn)
            .await
            .optional()?,
        Key::Conversation(conversation) => calls::table
            .filter(calls::conversation.eq(conversation))
            .select(Row::as_select())
            .first(conn)
            .await
            .optional()?,
    });
    Ok(row.and_then(Row::kept))
}

/// Stores `call` over the row read at `version`; `false` when the row moved meanwhile.
pub(super) async fn store(db: &Db, call: &Call, version: i64) -> Result<bool, DbError> {
    let row = Row::of(call, version + 1);
    let changed = with_conn!(db, |conn| diesel::update(
        calls::table
            .filter(calls::id.eq(&row.id))
            .filter(calls::version.eq(version))
    )
    .set((
        calls::phase.eq(&row.phase),
        calls::version.eq(row.version),
        calls::due_at.eq(row.due_at),
        calls::state.eq(&row.state),
    ))
    .execute(conn)
    .await?);
    Ok(changed == 1)
}

/// The calls due at `now` (Unix seconds), the longest due first: their ids and whether each is over.
pub(super) async fn due(db: &Db, now: i64, limit: i64) -> Result<Vec<(String, bool)>, DbError> {
    let rows: Vec<(String, String)> = with_conn!(db, |conn| calls::table
        .filter(calls::due_at.le(now))
        .order(calls::due_at.asc())
        .limit(limit)
        .select((calls::id, calls::phase))
        .load(conn)
        .await?);
    Ok(rows.into_iter().map(|(id, phase)| (id, phase == "ended")).collect())
}

pub(super) async fn delete(db: &Db, id: &str) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::delete(calls::table.find(id)).execute(conn).await?;
    });
    Ok(())
}

/// A call that isn't over, as an operator or a test reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenCall {
    pub id: String,
    pub conversation: String,
    /// The caller's object id.
    pub caller: String,
    /// `ringing` or `connected`.
    pub phase: String,
}

/// The calls that aren't over, the oldest first.
///
/// # Errors
/// A database failure.
pub async fn open_calls(db: &Db) -> Result<Vec<OpenCall>, DbError> {
    let rows: Vec<Row> = with_conn!(db, |conn| calls::table
        .filter(calls::phase.ne("ended"))
        .order((calls::created_at.asc(), calls::id.asc()))
        .select(Row::as_select())
        .load(conn)
        .await?);
    Ok(rows
        .into_iter()
        .map(|row| OpenCall {
            id: row.id,
            conversation: row.conversation,
            caller: row.caller_id,
            phase: row.phase,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_the_first_controller_stored_reads_with_its_parties_and_seats() {
        let party = |id: &str| {
            json!({"id": id, "name": id, "participant": {}, "endpoints": [], "active": null, "muted": false,
                "state_sequence": 0, "joined": null})
        };
        let stored = json!({
            "id": "call", "conversation": "conv", "phase": "connected", "caller": party("a"), "callee": party("b"),
            "offer": {}, "started_at": 1, "connected_at": 2, "ended_at": null, "due_at": 3, "roster_sequence": 0,
            "negotiation": {"by": "callee", "number": 1, "at": 4}, "negotiations": 1,
            "answered": {"by": "caller", "number": 1}, "held_by": "caller", "chat": null,
        });
        let row = Row {
            id: "call".into(),
            conversation: "conv".into(),
            caller_id: "a".into(),
            phase: "connected".into(),
            version: 7,
            due_at: 3,
            created_at: 0,
            state: stored.to_string(),
        };
        let kept = row.kept().unwrap();
        assert_eq!(kept.version, 7);
        let call = kept.call;
        assert_eq!(call.seat_of("b"), Some(Seat::CALLEE));
        assert_eq!(call.negotiation.map(|waiting| waiting.by), Some(Seat::CALLEE));
        assert_eq!(call.answered.map(|answered| answered.by), Some(Seat::CALLER));
        assert_eq!(call.held_by, Some(Seat::CALLER));
        let mut nothing = json!("not a call");
        upgrade(&mut nothing);
        assert_eq!(nothing, json!("not a call"));
    }
}
