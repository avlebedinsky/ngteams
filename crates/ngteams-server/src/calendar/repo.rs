use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::schema::{calendar_attendees, calendar_events};
use crate::db::schema::users;
use crate::db::users::User;
use crate::db::{Db, DbError, with_conn};

/// An event as the database keeps it. A series is one row (`recurring`); an occurrence changed, answered or
/// cancelled on its own is a row under it (`series_id`, `original_start`). `until_ms` is when a single event ends
/// and when a series' last occurrence does (`i64::MAX`: never). An event's `iCalUID` is its id, an occurrence's its
/// series'.
#[derive(Clone, Debug, Queryable, Selectable, Insertable)]
#[diesel(table_name = calendar_events)]
pub(crate) struct Event {
    pub id: String,
    pub owner_id: String,
    pub team_id: Option<String>,
    pub channel_id: Option<String>,
    pub post_id: Option<i64>,
    pub series_id: Option<String>,
    pub original_start: Option<i64>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub until_ms: i64,
    pub recurring: bool,
    pub changed: bool,
    pub cancelled: bool,
    pub meeting_chat: Option<String>,
    pub body: String,
    pub version: i64,
}

/// An account an event invites, with its answer; `removed` once it took the event off its own calendar.
#[derive(Clone, Debug, Queryable, Selectable, Insertable)]
#[diesel(table_name = calendar_attendees)]
pub(crate) struct Attendee {
    pub event_id: String,
    pub user_id: String,
    pub position: i32,
    pub optional: bool,
    pub response: String,
    pub responded_at: Option<i64>,
    pub removed: bool,
}

/// An event with the reader's own invitation, when it has one.
pub(super) type Seen = (Event, Option<Attendee>);

pub(super) async fn insert(db: &Db, event: &Event, attendees: &[Attendee]) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::insert_into(calendar_events::table)
                .values(event)
                .execute(conn)
                .await?;
            for attendee in attendees {
                diesel::insert_into(calendar_attendees::table)
                    .values(attendee)
                    .execute(conn)
                    .await?;
            }
            Ok(())
        })
        .await)
}

/// The events of `user`'s calendar that can have a part in `[from, to)`: its own that aren't cancelled, and the
/// ones it is invited to and didn't take off. A series is one event here.
pub(super) async fn window(db: &Db, user: &str, from: i64, to: i64) -> Result<Vec<Seen>, DbError> {
    with_conn!(db, |conn| Ok(calendar_events::table
        .left_join(
            calendar_attendees::table.on(calendar_attendees::event_id
                .eq(calendar_events::id)
                .and(calendar_attendees::user_id.eq(user)))
        )
        .filter(
            calendar_events::owner_id
                .eq(user)
                .and(calendar_events::team_id.is_null())
                .and(calendar_events::cancelled.eq(false))
                .or(calendar_attendees::removed.eq(false))
        )
        .filter(calendar_events::start_ms.lt(to))
        .filter(
            calendar_events::until_ms
                .gt(from)
                .or(calendar_events::start_ms.ge(from))
        )
        .order((calendar_events::start_ms.asc(), calendar_events::id.asc()))
        .select((Event::as_select(), Option::<Attendee>::as_select()))
        .load(conn)
        .await?))
}

/// The occurrences of these series kept as rows of their own: (series, the occurrence's own start).
pub(super) async fn own_rows(db: &Db, series: &[String]) -> Result<Vec<(String, i64)>, DbError> {
    let rows: Vec<(Option<String>, Option<i64>)> = with_conn!(db, |conn| calendar_events::table
        .filter(calendar_events::series_id.eq_any(series))
        .select((calendar_events::series_id, calendar_events::original_start))
        .load(conn)
        .await?);
    Ok(rows
        .into_iter()
        .filter_map(|(series, start)| series.zip(start))
        .collect())
}

/// The event `id` with `user`'s invitation to it.
pub(super) async fn event(db: &Db, id: &str, user: &str) -> Result<Option<Seen>, DbError> {
    with_conn!(db, |conn| Ok(calendar_events::table
        .left_join(
            calendar_attendees::table.on(calendar_attendees::event_id
                .eq(calendar_events::id)
                .and(calendar_attendees::user_id.eq(user)))
        )
        .filter(calendar_events::id.eq(id))
        .select((Event::as_select(), Option::<Attendee>::as_select()))
        .first(conn)
        .await
        .optional()?))
}

/// Whom the event invites, in the order they were named, each with its account.
pub(super) async fn attendees(db: &Db, event: &str) -> Result<Vec<(Attendee, User)>, DbError> {
    with_conn!(db, |conn| Ok(calendar_attendees::table
        .inner_join(users::table.on(users::id.eq(calendar_attendees::user_id)))
        .filter(calendar_attendees::event_id.eq(event))
        .order(calendar_attendees::position.asc())
        .select((Attendee::as_select(), User::as_select()))
        .load(conn)
        .await?))
}

pub(super) async fn users_by_id(db: &Db, ids: &[String]) -> Result<Vec<User>, DbError> {
    with_conn!(db, |conn| Ok(users::table
        .filter(users::id.eq_any(ids))
        .select(User::as_select())
        .load(conn)
        .await?))
}
