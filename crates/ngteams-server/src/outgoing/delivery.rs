//! The senders: every API node takes due rows of the outbox and posts each to its webhook once. A row goes from
//! `pending` to `sending` by a compare-and-set update, so one node sends it; the node that still holds it when the
//! request ends records the end, also by compare-and-set, and only then posts the reply. Thus a mention gets one
//! request and one reply at most. A row whose node died stays `sending` until its lease runs out: the sweep then
//! fails it with Teams' apology and sends nothing again.

use std::sync::Arc;
use std::time::Duration;

use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::activity;
use super::schema::outgoing_webhook_deliveries as rows;
use crate::app::App;
use crate::db::{Db, DbError, with_conn};

/// The fan-out topic a post that queued a mention wakes the senders on.
const WAKE: &str = "webhooks:wake";
/// How often a sender looks for due rows without a wake-up.
const POLL: Duration = Duration::from_secs(1);
/// The most rows one node sends at once.
const BATCH: i64 = 32;
/// How long a node holds a row beyond the request's own limit.
const LEASE_MARGIN: i64 = 10_000;
/// A row not sent within this many milliseconds of its message isn't sent, and gets no reply: Teams asks while the
/// person waits, and only stopped senders (webhooks turned off) leave a row so long.
const STALE: i64 = 60_000;

pub(crate) const PENDING: &str = "pending";
const SENDING: &str = "sending";
const DONE: &str = "done";
const FAILED: &str = "failed";

/// What Teams posts as the webhook when its service failed or didn't answer in time.
pub(crate) const APOLOGY: &str = "Sorry, there was a problem encountered with your request";

#[derive(Clone, Debug, Queryable, Selectable, Insertable)]
#[diesel(table_name = rows)]
pub(crate) struct Delivery {
    pub id: String,
    pub webhook_id: String,
    pub chat_id: String,
    pub root_id: i64,
    pub activity: String,
    pub state: String,
    pub locked_by: Option<String>,
    pub locked_until: i64,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Delivery {
    pub(crate) fn new(webhook: &str, chat: &str, root: i64, activity: &Value) -> Self {
        let at = crate::chatsvc::now();
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            webhook_id: webhook.to_owned(),
            chat_id: chat.to_owned(),
            root_id: root,
            activity: activity.to_string(),
            state: PENDING.to_owned(),
            locked_by: None,
            locked_until: 0,
            last_error: None,
            created_at: at,
            updated_at: at,
        }
    }
}

/// Wakes every node's sender.
pub(crate) async fn wake(app: &App) {
    if app.fanout.publish(WAKE, bytes::Bytes::new()).await.is_err() {
        tracing::warn!("outgoing webhook wake-up unavailable; the senders' poll will find it");
    }
}

/// The sender of a node: started with its API, stopped before the node exits.
pub struct Senders {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Senders {
    /// Starts to send what the outbox holds.
    #[must_use]
    pub fn start(app: Arc<App>) -> Self {
        let (stop, stopping) = watch::channel(false);
        let task = tokio::spawn(keep_delivering(app, stopping));
        Self { stop, task }
    }

    /// A planned stop: the sender takes no more rows; the requests under way end (each in its limit) and their
    /// replies are posted, for at most `within`. A row dropped after that fails at the sweep, unsent again.
    pub async fn stop(mut self, within: Duration) {
        let _ = self.stop.send(true);
        if tokio::time::timeout(within, &mut self.task).await.is_err() {
            tracing::warn!("outgoing webhook requests under way didn't end in time; the sweep fails them");
        }
    }
}

impl Drop for Senders {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn keep_delivering(app: Arc<App>, mut stopping: watch::Receiver<bool>) {
    let node = uuid::Uuid::new_v4().simple().to_string();
    let mut wake = None;
    loop {
        if wake.is_none() {
            // a closed fan-out closes every subscription: subscribe again, and poll meanwhile
            wake = app.fanout.subscribe(WAKE).await.ok();
        }
        sweep(&app).await;
        while !*stopping.borrow() && round(&app, &node).await > 0 {}
        if *stopping.borrow() {
            return;
        }
        let woken = async {
            match wake.as_mut() {
                Some(subscription) => !matches!(subscription.recv().await, Err(crate::fanout::Missed::Closed)),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            open = woken => {
                if !open {
                    wake = None;
                }
            }
            () = tokio::time::sleep(POLL) => {}
            _ = stopping.wait_for(|stopped| *stopped) => return,
        }
    }
}

/// One round: the rows this node took, each sent; how many.
async fn round(app: &App, node: &str) -> usize {
    let lease = timeout_ms(app) + LEASE_MARGIN;
    let claimed = match claim(&app.db, node, lease).await {
        Ok(claimed) => claimed,
        Err(error) => {
            tracing::warn!(%error, "outgoing webhook deliveries couldn't be taken");
            return 0;
        }
    };
    let count = claimed.len();
    futures_util::future::join_all(claimed.into_iter().map(|row| send(app, node, row))).await;
    count
}

fn timeout_ms(app: &App) -> i64 {
    i64::try_from(app.config.outgoing_webhooks.timeout_ms).unwrap_or(i64::MAX / 4)
}

/// Up to [`BATCH`] pending rows, oldest first, that this node took.
async fn claim(db: &Db, node: &str, lease: i64) -> Result<Vec<Delivery>, DbError> {
    let now = crate::chatsvc::now();
    let candidates: Vec<Delivery> = with_conn!(db, |conn| rows::table
        .filter(rows::state.eq(PENDING))
        .order((rows::created_at.asc(), rows::id.asc()))
        .limit(BATCH)
        .select(Delivery::as_select())
        .load(conn)
        .await?);
    let mut claimed = Vec::new();
    for candidate in candidates {
        if take(db, &candidate.id, PENDING, (SENDING, Some(node), now + lease), now).await? {
            claimed.push(candidate);
        }
    }
    Ok(claimed)
}

/// Moves row `id` from state `from` to `to` (its state, holder and lease) unless another node moved it first;
/// whether this call did.
async fn take(db: &Db, id: &str, from: &str, to: (&str, Option<&str>, i64), now: i64) -> Result<bool, DbError> {
    let (state, holder, until) = to;
    let taken = with_conn!(db, |conn| diesel::update(
        rows::table.find(id).filter(rows::state.eq(from))
    )
    .set((
        rows::state.eq(state),
        rows::locked_by.eq(holder),
        rows::locked_until.eq(until),
        rows::updated_at.eq(now),
    ))
    .execute(conn)
    .await?);
    Ok(taken == 1)
}

/// How a request ended.
#[derive(Debug)]
enum Outcome {
    /// The answer: an activity, or none.
    Answered(Option<Value>),
    Failed(String),
    /// Not sent: it waited too long (the senders were off), and nobody waits for its reply now.
    Stale,
}

async fn send(app: &App, node: &str, row: Delivery) {
    let outcome = if crate::chatsvc::now() - row.created_at > STALE {
        Outcome::Stale
    } else {
        attempt(app, &row).await
    };
    if ended(app, node, &row, &outcome).await {
        reply(app, &row, outcome).await;
    }
}

/// Records how `node`'s send of `row` ended; whether the node still held it, so its reply is its own to post.
async fn ended(app: &App, node: &str, row: &Delivery, outcome: &Outcome) -> bool {
    let (state, error) = match outcome {
        Outcome::Answered(_) => (DONE, None),
        Outcome::Failed(error) => (FAILED, Some(error.as_str())),
        Outcome::Stale => (FAILED, Some("the mention waited too long to be sent")),
    };
    let held = finish(&app.db, node, &row.id, state, error)
        .await
        .unwrap_or_else(|error| unkept(&error));
    if held && let Some(error) = error {
        tracing::info!(webhook = %row.webhook_id, %error, "a request to an outgoing webhook failed");
    }
    held
}

fn unkept(error: &DbError) -> bool {
    tracing::warn!(%error, "an outgoing webhook delivery's end wasn't kept; the sweep fails it");
    false
}

/// Records the end of `node`'s send of row `id`; whether the node still held it.
async fn finish(db: &Db, node: &str, id: &str, state: &str, error: Option<&str>) -> Result<bool, DbError> {
    let now = crate::chatsvc::now();
    let done = with_conn!(db, |conn| diesel::update(
        rows::table
            .find(id)
            .filter(rows::state.eq(SENDING))
            .filter(rows::locked_by.eq(node)),
    )
    .set((
        rows::state.eq(state),
        rows::locked_until.eq(0),
        rows::last_error.eq(error),
        rows::updated_at.eq(now),
    ))
    .execute(conn)
    .await?);
    Ok(done == 1)
}

async fn attempt(app: &App, row: &Delivery) -> Outcome {
    let hook = match super::registry::hook(&app.db, &row.webhook_id).await {
        Ok(Some(hook)) => hook,
        Ok(None) => return Outcome::Answered(None),
        Err(error) => return Outcome::Failed(error.to_string()),
    };
    let Ok(mut activity) = serde_json::from_str::<Value>(&row.activity) else {
        return Outcome::Failed("the stored activity is unreadable".to_owned());
    };
    activity::addressed(
        &mut activity,
        &crate::apps::service_url(&app.config),
        app.issuer.tenant(),
    );
    let body = activity.to_string().into_bytes();
    let authorization = activity::sign(&hook.key(), &body);
    let limit = Duration::from_millis(app.config.outgoing_webhooks.timeout_ms);
    match app
        .outbound
        .post_signed(&hook.callback_url, &authorization, body, limit)
        .await
    {
        Ok(answer) if (200..300).contains(&answer.status) => read(&answer.body),
        Ok(answer) => Outcome::Failed(format!("the webhook answered {}", answer.status)),
        Err(error) => Outcome::Failed(error.to_string()),
    }
}

/// An answer's body: nothing, or an activity.
fn read(body: &[u8]) -> Outcome {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Outcome::Answered(None);
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(answer) if answer.is_object() => Outcome::Answered(Some(answer)),
        _ => Outcome::Failed("the answer isn't an activity".to_owned()),
    }
}

/// Posts what `outcome` makes in the thread, as the webhook with its name now.
async fn reply(app: &App, row: &Delivery, outcome: Outcome) {
    let made = replies(&outcome, &app.config.origin());
    if made.is_empty() {
        return;
    }
    if let Err(error) = post_replies(app, row, made).await {
        tracing::warn!(%error, "an outgoing webhook's reply wasn't posted");
    }
}

/// The messages `outcome` makes: the answer's, Teams' apology, or none.
fn replies(outcome: &Outcome, origin: &str) -> Vec<(&'static str, String)> {
    use crate::bots::activity::{Refused, messages};
    match outcome {
        Outcome::Answered(None) | Outcome::Stale => Vec::new(),
        Outcome::Answered(Some(answer)) => match messages(&activity::kept(answer.clone()), origin) {
            Ok(made) => made.into_iter().map(|made| (made.kind, made.content)).collect(),
            Err(Refused::Empty) => Vec::new(),
            Err(Refused::Attachment) => vec![apology()],
        },
        Outcome::Failed(_) => vec![apology()],
    }
}

async fn post_replies(app: &App, row: &Delivery, made: Vec<(&'static str, String)>) -> Result<(), DbError> {
    let Some(hook) = super::registry::hook(&app.db, &row.webhook_id).await? else {
        return Ok(());
    };
    for (kind, content) in made {
        let post = crate::chatsvc::service::AppPost {
            thread: &row.chat_id,
            app: &row.webhook_id,
            name: &hook.name,
            kind,
            content: &content,
            properties: serde_json::Map::new(),
        };
        crate::chatsvc::service::reply_as_app(app, post, row.root_id).await?;
    }
    Ok(())
}

fn apology() -> (&'static str, String) {
    (crate::bots::activity::HTML, format!("<p>{APOLOGY}</p>"))
}

/// Fails the rows whose node died while it sent them (their lease ran out), with Teams' apology; nothing is sent
/// again.
async fn sweep(app: &App) {
    let now = crate::chatsvc::now();
    let stuck: Vec<Delivery> = match stuck(&app.db, now).await {
        Ok(stuck) => stuck,
        Err(error) => {
            tracing::warn!(%error, "outgoing webhook deliveries couldn't be swept");
            return;
        }
    };
    for row in stuck {
        fail_stuck(app, &row, now).await;
    }
}

async fn fail_stuck(app: &App, row: &Delivery, now: i64) {
    match take(&app.db, &row.id, SENDING, (FAILED, None, 0), now).await {
        Ok(true) => reply(app, row, Outcome::Failed(String::new())).await,
        Ok(false) => {}
        Err(error) => tracing::warn!(%error, "an outgoing webhook delivery couldn't be swept"),
    }
}

async fn stuck(db: &Db, now: i64) -> Result<Vec<Delivery>, DbError> {
    with_conn!(db, |conn| Ok(rows::table
        .filter(rows::state.eq(SENDING))
        .filter(rows::locked_until.lt(now))
        .limit(BATCH)
        .select(Delivery::as_select())
        .load(conn)
        .await?))
}

/// Drops sent and failed rows last changed before `before`.
pub(crate) async fn purge(db: &Db, before: i64) -> Result<usize, DbError> {
    with_conn!(db, |conn| Ok(diesel::delete(
        rows::table
            .filter(rows::state.eq_any([DONE, FAILED]))
            .filter(rows::updated_at.lt(before)),
    )
    .execute(conn)
    .await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_answer_is_none_and_one_that_is_no_activity_fails() {
        assert!(matches!(read(b""), Outcome::Answered(None)));
        assert!(matches!(read(b" \n"), Outcome::Answered(None)));
        assert!(matches!(read(br#"{"text":"x"}"#), Outcome::Answered(Some(_))));
        assert!(matches!(read(b"[1]"), Outcome::Failed(_)));
        assert!(matches!(read(b"nope"), Outcome::Failed(_)));
    }

    async fn sqlite() -> Result<(tempfile::TempDir, Db), DbError> {
        use diesel_async::SimpleAsyncConnection as _;
        let dir = tempfile::tempdir().map_err(|error| DbError::Pool(error.to_string()))?;
        let db = Db::connect(&format!("sqlite://{}", dir.path().join("server.sqlite").display()), 1).await?;
        // the rows' webhooks aren't registered: this tests only the outbox's own updates
        with_conn!(db, |conn| conn.batch_execute("PRAGMA foreign_keys = OFF").await?);
        Ok((dir, db))
    }

    async fn put(db: &Db, row: &Delivery) -> Result<(), DbError> {
        with_conn!(db, |conn| diesel::insert_into(rows::table)
            .values(row)
            .execute(conn)
            .await?);
        Ok(())
    }

    async fn state(db: &Db, id: &str) -> Result<String, DbError> {
        with_conn!(db, |conn| Ok(rows::table
            .find(id)
            .select(rows::state)
            .first(conn)
            .await?))
    }

    #[tokio::test]
    async fn one_node_takes_a_row_and_only_the_holder_ends_it() -> Result<(), DbError> {
        let (_dir, db) = sqlite().await?;
        let row = Delivery::new("w", "19:c@thread.tacv2", 5, &serde_json::json!({"type": "message"}));
        put(&db, &row).await?;
        let first = claim(&db, "a", 60_000).await?;
        let second = claim(&db, "b", 60_000).await?;
        assert_eq!((first.len(), second.len()), (1, 0));
        assert!(!finish(&db, "b", &row.id, DONE, None).await?);
        assert!(finish(&db, "a", &row.id, DONE, None).await?);
        assert_eq!(state(&db, &row.id).await?, DONE);
        // a lease that ran out is swept to failed, never pending again
        let lost = Delivery::new("w", "19:c@thread.tacv2", 6, &serde_json::json!({}));
        put(&db, &lost).await?;
        claim(&db, "a", -1).await?;
        let now = crate::chatsvc::now();
        assert_eq!(stuck(&db, now).await?.len(), 1);
        assert!(take(&db, &lost.id, SENDING, (FAILED, None, 0), now).await?);
        assert!(!take(&db, &lost.id, SENDING, (FAILED, None, 0), now).await?);
        assert!(!finish(&db, "a", &lost.id, DONE, None).await?);
        assert_eq!(state(&db, &lost.id).await?, FAILED);
        assert_eq!(purge(&db, now + 1).await?, 2);
        Ok(())
    }
}
