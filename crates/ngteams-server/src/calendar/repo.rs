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

/// Writes what can change of the event `event.id`.
pub(super) async fn save(db: &Db, event: &Event) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::update(calendar_events::table.find(&event.id))
            .set(changes(event))
            .execute(conn)
            .await?;
    });
    Ok(())
}

type Changes<'a> = (
    diesel::dsl::Eq<calendar_events::start_ms, i64>,
    diesel::dsl::Eq<calendar_events::end_ms, i64>,
    diesel::dsl::Eq<calendar_events::until_ms, i64>,
    diesel::dsl::Eq<calendar_events::recurring, bool>,
    diesel::dsl::Eq<calendar_events::changed, bool>,
    diesel::dsl::Eq<calendar_events::cancelled, bool>,
    diesel::dsl::Eq<calendar_events::body, &'a str>,
    diesel::dsl::Eq<calendar_events::version, i64>,
);

fn changes(event: &Event) -> Changes<'_> {
    (
        calendar_events::start_ms.eq(event.start_ms),
        calendar_events::end_ms.eq(event.end_ms),
        calendar_events::until_ms.eq(event.until_ms),
        calendar_events::recurring.eq(event.recurring),
        calendar_events::changed.eq(event.changed),
        calendar_events::cancelled.eq(event.cancelled),
        calendar_events::body.eq(event.body.as_str()),
        calendar_events::version.eq(event.version),
    )
}

/// Deletes an event, a series with the occurrences kept under it.
pub(super) async fn delete(db: &Db, id: &str) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::delete(calendar_events::table.find(id)).execute(conn).await?;
    });
    Ok(())
}

/// Forgets the occurrences kept as rows of their own under `series`: its rule or its times changed.
pub(super) async fn drop_own_rows(db: &Db, series: &str) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::delete(calendar_events::table.filter(calendar_events::series_id.eq(series)))
            .execute(conn)
            .await?;
    });
    Ok(())
}

/// Gives the series' new fields to its occurrences kept as rows without a change of their own.
pub(super) async fn refresh_own_rows(db: &Db, series: &str, body: &str, version: i64) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::update(
            calendar_events::table
                .filter(calendar_events::series_id.eq(series))
                .filter(calendar_events::changed.eq(false)),
        )
        .set((calendar_events::body.eq(body), calendar_events::version.eq(version)))
        .execute(conn)
        .await?;
    });
    Ok(())
}

/// Keeps one occurrence of a series as a row of its own: a new row takes the series' invitations as they stand,
/// a row there already is written over.
pub(super) async fn put_own_row(db: &Db, row: &Event) -> Result<(), DbError> {
    let Some(series) = row.series_id.as_deref() else {
        return save(db, row).await;
    };
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let inserted = diesel::insert_into(calendar_events::table)
                .values(row)
                .on_conflict_do_nothing()
                .execute(conn)
                .await?;
            if inserted == 0 {
                diesel::update(calendar_events::table.find(&row.id))
                    .set(changes(row))
                    .execute(conn)
                    .await?;
                return Ok(());
            }
            let invited: Vec<Attendee> = calendar_attendees::table
                .filter(calendar_attendees::event_id.eq(series))
                .select(Attendee::as_select())
                .load(conn)
                .await?;
            for mut attendee in invited {
                attendee.event_id.clone_from(&row.id);
                diesel::insert_into(calendar_attendees::table)
                    .values(&attendee)
                    .execute(conn)
                    .await?;
            }
            Ok(())
        })
        .await)
}

/// The ids of the occurrences kept as rows of their own under `series`.
pub(super) async fn own_row_ids(db: &Db, series: &str) -> Result<Vec<String>, DbError> {
    with_conn!(db, |conn| Ok(calendar_events::table
        .filter(calendar_events::series_id.eq(series))
        .select(calendar_events::id)
        .load(conn)
        .await?))
}

/// Whether an event already takes place in the meeting chat `chat`.
pub(super) async fn uses_chat(db: &Db, chat: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(diesel::select(diesel::dsl::exists(
        calendar_events::table.filter(calendar_events::meeting_chat.eq(chat))
    ))
    .get_result(conn)
    .await?))
}

/// Gives the event (and the occurrences kept under it) its meeting's chat.
pub(super) async fn set_meeting(db: &Db, id: &str, chat: &str) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::update(calendar_events::table.filter(calendar_events::id.eq(id).or(calendar_events::series_id.eq(id))))
            .set(calendar_events::meeting_chat.eq(chat))
            .execute(conn)
            .await?;
    });
    Ok(())
}

/// Remembers the message that told the event (and the occurrences kept under it).
pub(super) async fn set_post(db: &Db, id: &str, post: i64) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::update(calendar_events::table.filter(calendar_events::id.eq(id).or(calendar_events::series_id.eq(id))))
            .set(calendar_events::post_id.eq(post))
            .execute(conn)
            .await?;
    });
    Ok(())
}

/// Cancels an event, a series with the occurrences kept under it.
pub(super) async fn cancel(db: &Db, id: &str, version: i64) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::update(calendar_events::table.filter(calendar_events::id.eq(id).or(calendar_events::series_id.eq(id))))
            .set((
                calendar_events::cancelled.eq(true),
                calendar_events::version.eq(version),
            ))
            .execute(conn)
            .await?;
    });
    Ok(())
}

/// Makes the invitations of each of `events` the accounts `wanted` (each with whether it is optional): who isn't
/// among them loses the event, who is new is invited, and with `reset` everyone is asked again.
pub(super) async fn sync_attendees(
    db: &Db,
    events: &[String],
    wanted: &[(String, bool)],
    reset: bool,
) -> Result<(), DbError> {
    let ids: Vec<&str> = wanted.iter().map(|(id, _)| id.as_str()).collect();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::delete(
                calendar_attendees::table
                    .filter(calendar_attendees::event_id.eq_any(events))
                    .filter(calendar_attendees::user_id.ne_all(&ids)),
            )
            .execute(conn)
            .await?;
            if reset {
                diesel::update(calendar_attendees::table.filter(calendar_attendees::event_id.eq_any(events)))
                    .set((
                        calendar_attendees::response.eq("None"),
                        calendar_attendees::responded_at.eq(None::<i64>),
                        calendar_attendees::removed.eq(false),
                    ))
                    .execute(conn)
                    .await?;
            }
            for event in events {
                for (position, (user, optional)) in wanted.iter().enumerate() {
                    let row = Attendee {
                        event_id: event.clone(),
                        user_id: user.clone(),
                        position: i32::try_from(position).unwrap_or(i32::MAX),
                        optional: *optional,
                        response: "None".into(),
                        responded_at: None,
                        removed: false,
                    };
                    diesel::insert_into(calendar_attendees::table)
                        .values(&row)
                        .on_conflict((calendar_attendees::event_id, calendar_attendees::user_id))
                        .do_update()
                        .set((
                            calendar_attendees::position.eq(row.position),
                            calendar_attendees::optional.eq(row.optional),
                        ))
                        .execute(conn)
                        .await?;
                }
            }
            Ok(())
        })
        .await)
}

/// Sets `user`'s answer to each of `events`.
pub(super) async fn set_response(
    db: &Db,
    events: &[String],
    user: &str,
    response: &str,
    at: i64,
) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::update(
            calendar_attendees::table
                .filter(calendar_attendees::event_id.eq_any(events))
                .filter(calendar_attendees::user_id.eq(user)),
        )
        .set((
            calendar_attendees::response.eq(response),
            calendar_attendees::responded_at.eq(at),
        ))
        .execute(conn)
        .await?;
    });
    Ok(())
}

/// Takes each of `events` off `user`'s calendar.
pub(super) async fn set_removed(db: &Db, events: &[String], user: &str) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::update(
            calendar_attendees::table
                .filter(calendar_attendees::event_id.eq_any(events))
                .filter(calendar_attendees::user_id.eq(user)),
        )
        .set(calendar_attendees::removed.eq(true))
        .execute(conn)
        .await?;
    });
    Ok(())
}
