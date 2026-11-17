//! The outbox of activities to bots. A chat write puts a row for each bot it concerns in its own transaction
//! ([`enqueue!`]), so an event a bot should get is never lost between the write and the send. Any node sends a row:
//! it takes the row's lease with a compare-and-set update that works on both backends, so one node sends it at a
//! time; a lease that runs out (its node died) lets another take it. Rows go out oldest first per bot and
//! conversation: a row waits while an older one of its line is pending.

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::schema::{bot_deliveries, bot_health};
use crate::db::{Db, DbError, with_conn};

/// A row waiting to be sent.
pub const PENDING: &str = "pending";
pub const SENT: &str = "sent";
pub const FAILED: &str = "failed";

/// Failed sends in a row after which a bot is paused.
pub(crate) const PAUSE_AFTER: i32 = 20;
/// How long a paused bot gets nothing, in milliseconds.
pub(crate) const PAUSE: i64 = 60_000;

#[derive(Clone, Debug, Queryable, Selectable, Insertable)]
#[diesel(table_name = bot_deliveries)]
pub struct Delivery {
    pub id: String,
    pub bot_id: String,
    pub conversation_id: String,
    pub seq: i64,
    pub activity: String,
    pub state: String,
    pub attempts: i32,
    /// 0 for the configured number (`bots.retry_delays`); 1 for what is no use later (typing).
    pub max_attempts: i32,
    pub due_at: i64,
    pub locked_until: i64,
    pub locked_by: Option<String>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Delivery {
    pub(crate) fn new(bot: &str, conversation: &str, seq: i64, activity: &serde_json::Value, once: bool) -> Self {
        let at = crate::chatsvc::now();
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            bot_id: bot.to_owned(),
            conversation_id: conversation.to_owned(),
            seq,
            activity: activity.to_string(),
            state: PENDING.to_owned(),
            attempts: 0,
            max_attempts: i32::from(once),
            due_at: at,
            locked_until: 0,
            locked_by: None,
            last_error: None,
            created_at: at,
            updated_at: at,
        }
    }
}

/// In a chat write's transaction (`$conn`): a row for each bot in `$chat` that `$event` (an
/// [`super::activity::Event`]) by `$owner` concerns. A bot's own writes and anything in a chat without a bot make
/// none. Whether one was made, so the caller wakes the senders after its commit.
macro_rules! enqueue {
    ($conn:expr, $chat:expr, $owner:expr, $event:expr, $at:expr) => {{
        use $crate::bots::schema::bot_deliveries;
        use $crate::chatsvc::schema::{chat_members, chats};
        use $crate::db::schema::users;
        let members: Vec<String> = chat_members::table
            .filter(chat_members::chat_id.eq($chat))
            .filter(chat_members::left_at.is_null())
            .select(chat_members::user_id)
            .load($conn)
            .await?;
        let bots: Vec<(String, String)> = if members.len() < 2 {
            Vec::new()
        } else {
            users::table
                .filter(users::id.eq_any(&members))
                .filter(users::kind.eq($crate::db::users::APP))
                .filter(users::id.ne($owner))
                .select((users::id, users::display_name))
                .load($conn)
                .await?
        };
        let mut queued = false;
        if !bots.is_empty() {
            let sender: Option<String> = users::table
                .find($owner)
                .filter($crate::db::users::person())
                .select(users::display_name)
                .first($conn)
                .await
                .optional()?;
            let personal: bool = chats::table
                .find($chat)
                .select(chats::unique_roster)
                .first($conn)
                .await?;
            if let Some(name) = sender {
                let event = $event;
                let place = $crate::bots::activity::Place { id: $chat, personal };
                let from = $crate::bots::activity::Sender {
                    id: $owner,
                    name: &name,
                };
                for (bot, bot_name) in &bots {
                    if !$crate::bots::outbox::concerns(&event, personal, bot) {
                        continue;
                    }
                    let to = $crate::bots::activity::Recipient {
                        id: bot,
                        name: bot_name,
                    };
                    let activity = $crate::bots::activity::pending(&event, &place, &from, &to, $at);
                    let once = matches!(event, $crate::bots::activity::Event::Typing { .. });
                    let row = $crate::bots::outbox::Delivery::new(bot, $chat, $at, &activity, once);
                    diesel::insert_into(bot_deliveries::table)
                        .values(&row)
                        .execute($conn)
                        .await?;
                    queued = true;
                }
            }
        }
        queued
    }};
}
pub(crate) use enqueue;

/// Whether a bot in a chat gets `event`: in a 1:1 chat everything; in a group chat a message that mentions it.
pub(crate) fn concerns(event: &super::activity::Event<'_>, personal: bool, bot: &str) -> bool {
    if personal {
        return true;
    }
    let super::activity::Event::Message { body, .. } = event else {
        return false;
    };
    let mention = format!("28:{bot}");
    let mentions = &body["properties"]["mentions"];
    let listed: Vec<serde_json::Value> = match mentions {
        serde_json::Value::String(text) => serde_json::from_str(text).unwrap_or_default(),
        serde_json::Value::Array(items) => items.clone(),
        _ => Vec::new(),
    };
    listed
        .iter()
        .any(|entry| entry["mri"].as_str() == Some(mention.as_str()))
}

/// Up to `limit` rows this node (`node`) now holds for `lease` milliseconds: due, not held by another node, the
/// oldest of their line, their bot not paused.
pub(crate) async fn claim(db: &Db, node: &str, lease: i64, limit: i64) -> Result<Vec<Delivery>, DbError> {
    let now = crate::chatsvc::now();
    let candidates: Vec<Delivery> = with_conn!(db, |conn| bot_deliveries::table
        .filter(bot_deliveries::state.eq(PENDING))
        .filter(bot_deliveries::due_at.le(now))
        .filter(bot_deliveries::locked_until.lt(now))
        .order((bot_deliveries::seq, bot_deliveries::id))
        .limit(limit * 4)
        .select(Delivery::as_select())
        .load(conn)
        .await?);
    let mut claimed: Vec<Delivery> = Vec::new();
    for candidate in candidates {
        if i64::try_from(claimed.len()).unwrap_or(i64::MAX) >= limit {
            break;
        }
        let same_line =
            |held: &Delivery| held.bot_id == candidate.bot_id && held.conversation_id == candidate.conversation_id;
        let waits = claimed.iter().any(same_line)
            || !head_of_line(db, &candidate).await?
            || paused(db, &candidate.bot_id, now).await?;
        if !waits && take(db, node, &candidate.id, now, lease).await? {
            let mut held = candidate;
            held.attempts += 1;
            claimed.push(held);
        }
    }
    Ok(claimed)
}

/// Takes row `id`'s lease for `node` unless another node holds it; whether it did.
async fn take(db: &Db, node: &str, id: &str, now: i64, lease: i64) -> Result<bool, DbError> {
    let taken = with_conn!(db, |conn| diesel::update(
        bot_deliveries::table
            .find(id)
            .filter(bot_deliveries::state.eq(PENDING))
            .filter(bot_deliveries::locked_until.lt(now)),
    )
    .set((
        bot_deliveries::locked_until.eq(now + lease),
        bot_deliveries::locked_by.eq(node),
        bot_deliveries::attempts.eq(bot_deliveries::attempts + 1),
        bot_deliveries::updated_at.eq(now),
    ))
    .execute(conn)
    .await?);
    Ok(taken == 1)
}

/// Whether no older row of `row`'s line is still to be sent.
async fn head_of_line(db: &Db, row: &Delivery) -> Result<bool, DbError> {
    let older: i64 = with_conn!(db, |conn| bot_deliveries::table
        .filter(bot_deliveries::bot_id.eq(&row.bot_id))
        .filter(bot_deliveries::conversation_id.eq(&row.conversation_id))
        .filter(bot_deliveries::state.eq(PENDING))
        .filter(
            bot_deliveries::seq
                .lt(row.seq)
                .or(bot_deliveries::seq.eq(row.seq).and(bot_deliveries::id.lt(&row.id))),
        )
        .count()
        .get_result(conn)
        .await?);
    Ok(older == 0)
}

async fn paused(db: &Db, bot: &str, now: i64) -> Result<bool, DbError> {
    let until: Option<i64> = with_conn!(db, |conn| bot_health::table
        .find(bot)
        .select(bot_health::paused_until)
        .first(conn)
        .await
        .optional()?);
    Ok(until.is_some_and(|until| until > now))
}

/// How a send ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Sent,
    /// Failed; tried again after the delay, if any is left.
    Retry(Option<i64>, String),
    /// Failed for good.
    Failed(String),
}

/// Records how `node`'s send of `row` ended, and the bot's run of failures. A row whose lease another node took over
/// is that node's to record.
pub(crate) async fn finish(db: &Db, node: &str, row: &Delivery, outcome: &Outcome) -> Result<(), DbError> {
    let now = crate::chatsvc::now();
    let (state, due, error) = match outcome {
        Outcome::Sent => (SENT, row.due_at, None),
        Outcome::Retry(Some(delay), error) => (PENDING, now + delay, Some(error.as_str())),
        Outcome::Retry(None, error) | Outcome::Failed(error) => (FAILED, row.due_at, Some(error.as_str())),
    };
    with_conn!(db, |conn| diesel::update(
        bot_deliveries::table
            .find(&row.id)
            .filter(bot_deliveries::locked_by.eq(node))
            .filter(bot_deliveries::state.eq(PENDING)),
    )
    .set((
        bot_deliveries::state.eq(state),
        bot_deliveries::due_at.eq(due),
        bot_deliveries::locked_until.eq(0),
        bot_deliveries::last_error.eq(error),
        bot_deliveries::updated_at.eq(now),
    ))
    .execute(conn)
    .await?);
    health(db, &row.bot_id, error, now).await
}

/// A bot's run of failed sends, as kept.
#[derive(Insertable, AsChangeset)]
#[diesel(table_name = bot_health, treat_none_as_null = true)]
struct Health<'a> {
    bot_id: &'a str,
    failures: i32,
    paused_until: i64,
    last_error: Option<String>,
    updated_at: i64,
}

/// The bot's run of failed sends: one more, or none after a success; a long run pauses it.
async fn health(db: &Db, bot: &str, error: Option<&str>, now: i64) -> Result<(), DbError> {
    let (failures, paused_until) = next_run(failures(db, bot).await?, error.is_some(), now);
    let row = Health {
        bot_id: bot,
        failures,
        paused_until,
        last_error: error.map(str::to_owned),
        updated_at: now,
    };
    with_conn!(db, |conn| diesel::insert_into(bot_health::table)
        .values(&row)
        .on_conflict(bot_health::bot_id)
        .do_update()
        .set(&row)
        .execute(conn)
        .await?);
    if error.is_none() {
        with_conn!(db, |conn| diesel::update(bot_health::table.find(bot))
            .set(bot_health::last_success.eq(now))
            .execute(conn)
            .await?);
    }
    Ok(())
}

async fn failures(db: &Db, bot: &str) -> Result<i32, DbError> {
    let failures: Option<i32> = with_conn!(db, |conn| bot_health::table
        .find(bot)
        .select(bot_health::failures)
        .first(conn)
        .await
        .optional()?);
    Ok(failures.unwrap_or(0))
}

/// A run of `failures` after one more send: its length and until when the bot is paused.
const fn next_run(failures: i32, failed: bool, now: i64) -> (i32, i64) {
    if !failed {
        (0, 0)
    } else if failures + 1 >= PAUSE_AFTER {
        (0, now + PAUSE)
    } else {
        (failures + 1, 0)
    }
}

/// The rows of a bot, oldest first (for its health and the tests).
///
/// # Errors
/// A database failure.
pub async fn deliveries(db: &Db, bot: &str) -> Result<Vec<Delivery>, DbError> {
    with_conn!(db, |conn| Ok(bot_deliveries::table
        .filter(bot_deliveries::bot_id.eq(bot))
        .order((bot_deliveries::seq, bot_deliveries::id))
        .select(Delivery::as_select())
        .load(conn)
        .await?))
}

/// Drops sent and failed rows last changed before `before`.
pub(crate) async fn purge(db: &Db, before: i64) -> Result<usize, DbError> {
    with_conn!(db, |conn| Ok(diesel::delete(
        bot_deliveries::table
            .filter(bot_deliveries::state.ne(PENDING))
            .filter(bot_deliveries::updated_at.lt(before)),
    )
    .execute(conn)
    .await?))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::bots::activity::Event;

    #[test]
    fn a_group_chat_gives_a_bot_only_the_messages_that_mention_it() {
        let named = json!({"properties": {"mentions": "[{\"mri\":\"28:b\",\"id\":0}]"}});
        let other = json!({"properties": {"mentions": [{"mri": "8:orgid:u"}]}});
        let message = |body| Event::Message { id: 1, body };
        assert!(concerns(&message(&other), true, "b"));
        assert!(concerns(&Event::Typing { at: 1 }, true, "b"));
        assert!(concerns(&message(&named), false, "b"));
        assert!(!concerns(&message(&other), false, "b"));
        assert!(!concerns(&Event::Deleted { id: 1 }, false, "b"));
    }

    #[test]
    fn a_long_run_of_failures_pauses_a_bot_and_a_success_ends_the_run() {
        assert_eq!(next_run(3, false, 10), (0, 0));
        assert_eq!(next_run(3, true, 10), (4, 0));
        assert_eq!(next_run(PAUSE_AFTER - 1, true, 10), (0, 10 + PAUSE));
    }
}
