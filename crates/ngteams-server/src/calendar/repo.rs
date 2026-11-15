use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::schema::{calendar_attendees, calendar_events, meetings};
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

/// Writes a new event with its invitations. An event that takes a meeting's chat holds the meeting's row from the
/// check to the write, so of two events that name one meeting the second is refused: [`DbError::Conflict`].
pub(super) async fn insert(db: &Db, event: &Event, attendees: &[Attendee]) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            if let Some(chat) = &event.meeting_chat {
                diesel::update(meetings::table.find(chat))
                    .set(meetings::kind.eq(meetings::kind))
                    .execute(conn)
                    .await?;
                let used: bool = diesel::select(diesel::dsl::exists(
                    calendar_events::table.filter(calendar_events::meeting_chat.eq(chat)),
                ))
                .get_result(conn)
                .await?;
                if used {
                    return Err(DbError::Conflict);
                }
            }
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

/// Holds a series' row until the transaction ends: a change or a cancellation of the series, and another
/// transaction that holds it, wait for this one.
macro_rules! hold {
    ($conn:expr, $series:expr) => {
        diesel::update(calendar_events::table.find($series))
            .set(calendar_events::version.eq(calendar_events::version))
            .execute($conn)
            .await?
    };
}

/// Everyone the event `$id` invites, a series with the occurrences kept under it, as the transaction on `$conn`
/// sees it.
macro_rules! invited {
    ($conn:expr, $id:expr) => {{
        let invited: Vec<String> = calendar_attendees::table
            .inner_join(calendar_events::table.on(calendar_events::id.eq(calendar_attendees::event_id)))
            .filter(calendar_events::id.eq($id).or(calendar_events::series_id.eq($id)))
            .select(calendar_attendees::user_id)
            .distinct()
            .load($conn)
            .await?;
        invited
    }};
}

/// Makes the invitations of each of `$events` the accounts `$wanted` (each with whether it is optional), on the
/// connection of the caller's transaction: who isn't among them loses the event, who is new is invited, and with
/// `$reset` everyone is asked again.
macro_rules! sync {
    ($conn:expr, $events:expr, $wanted:expr, $reset:expr) => {{
        let (events, wanted): (&[String], &[(String, bool)]) = ($events, $wanted);
        let ids: Vec<&str> = wanted.iter().map(|(id, _)| id.as_str()).collect();
        diesel::delete(
            calendar_attendees::table
                .filter(calendar_attendees::event_id.eq_any(events))
                .filter(calendar_attendees::user_id.ne_all(&ids)),
        )
        .execute($conn)
        .await?;
        if $reset {
            diesel::update(calendar_attendees::table.filter(calendar_attendees::event_id.eq_any(events)))
                .set((
                    calendar_attendees::response.eq("None"),
                    calendar_attendees::responded_at.eq(None::<i64>),
                    calendar_attendees::removed.eq(false),
                ))
                .execute($conn)
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
                    .execute($conn)
                    .await?;
            }
        }
    }};
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

/// What a change of a series does to the occurrences kept as rows of their own under it.
pub(super) enum Kept<'a> {
    /// Its rule or its times changed: they are forgotten.
    Forgotten,
    /// The ones without a change of their own take the series' new fields.
    Given(&'a str),
}

/// Writes what can change of an event with its invitations, in one transaction that holds its row: a series
/// (`kept`) brings the occurrences kept under it along, so none is made in between from the invitations as they
/// were. The event's row goes first, as in [`cancel`]. Who lost the last invitation under it.
pub(super) async fn save_whole(
    db: &Db,
    event: &Event,
    kept: Option<Kept<'_>>,
    wanted: &[(String, bool)],
    reset: bool,
) -> Result<Vec<String>, DbError> {
    let under = calendar_events::table.filter(calendar_events::series_id.eq(&event.id));
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::update(calendar_events::table.find(&event.id))
                .set(changes(event))
                .execute(conn)
                .await?;
            let before = invited!(conn, &event.id);
            let mut rows = vec![event.id.clone()];
            match kept {
                None => {}
                Some(Kept::Forgotten) => {
                    diesel::delete(under).execute(conn).await?;
                }
                Some(Kept::Given(body)) => {
                    diesel::update(under.filter(calendar_events::changed.eq(false)))
                        .set((
                            calendar_events::body.eq(body),
                            calendar_events::version.eq(event.version),
                        ))
                        .execute(conn)
                        .await?;
                    let own: Vec<String> = under.select(calendar_events::id).load(conn).await?;
                    rows.extend(own);
                }
            }
            sync!(conn, &rows, wanted, reset);
            let still = invited!(conn, &event.id);
            Ok(before.into_iter().filter(|user| !still.contains(user)).collect())
        })
        .await)
}

/// Keeps one occurrence of a series as a row of its own, for its organizer's change of it: a new row takes the
/// series' invitations as they stand, a row there already is written over.
pub(super) async fn put_own_row(db: &Db, row: &Event) -> Result<(), DbError> {
    own_row(db, row, Existing::WrittenOver).await.map(|_| ())
}

/// As [`put_own_row`] for an occurrence of a channel's series, which is named by its start alone, unless another
/// live occurrence of the series is at that start: one its organizer gave it, or the one the series' rule puts
/// there (`own`, its id) while it isn't cancelled or moved away. The series' row is held from the check to the
/// write, so of two changes to one start the second is refused. `false`: it is taken, and nothing is written.
pub(super) async fn put_own_row_apart(db: &Db, row: &Event, own: Option<&str>) -> Result<bool, DbError> {
    own_row(db, row, Existing::Apart(own)).await
}

/// Keeps one occurrence of a series as a row of its own, for an answer or a removal of it: a row there already
/// stays as it is, since `row` is the series as its reader saw it and the organizer may have changed the
/// occurrence since. `false`, and no row, when the series itself changed or was cancelled since that read.
pub(super) async fn keep_own_row(db: &Db, row: &Event) -> Result<bool, DbError> {
    own_row(db, row, Existing::Left).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Existing<'a> {
    WrittenOver,
    Left,
    Apart(Option<&'a str>),
}

/// Whether another live occurrence of `$series` is at the start of `$row`, as the transaction on `$conn` sees it
/// ([`put_own_row_apart`]).
macro_rules! taken {
    ($conn:expr, $series:expr, $row:expr, $own:expr) => {{
        let given: i64 = calendar_events::table
            .filter(calendar_events::series_id.eq($series))
            .filter(calendar_events::changed.eq(true))
            .filter(calendar_events::cancelled.eq(false))
            .filter(calendar_events::start_ms.eq($row.start_ms))
            .filter(calendar_events::id.ne(&$row.id))
            .count()
            .get_result($conn)
            .await?;
        let mut taken = given > 0;
        if let (false, Some(own)) = (taken, $own) {
            let kept: Option<(bool, i64)> = calendar_events::table
                .find(own)
                .select((calendar_events::cancelled, calendar_events::start_ms))
                .first($conn)
                .await
                .optional()?;
            taken = kept.is_none_or(|(cancelled, start)| !cancelled && start == $row.start_ms);
        }
        taken
    }};
}

async fn own_row(db: &Db, row: &Event, existing: Existing<'_>) -> Result<bool, DbError> {
    let Some(series) = row.series_id.as_deref() else {
        return save(db, row).await.map(|()| true);
    };
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let mut row = row.clone();
            if existing == Existing::Left {
                hold!(conn, series);
                let now: Option<Event> = calendar_events::table
                    .find(series)
                    .select(Event::as_select())
                    .first(conn)
                    .await
                    .optional()?;
                let Some(now) = now.filter(|now| (now.version, now.cancelled) == (row.version, row.cancelled)) else {
                    return Ok(false);
                };
                row.meeting_chat = now.meeting_chat;
                row.post_id = now.post_id;
            }
            if let Existing::Apart(own) = existing {
                hold!(conn, series);
                if taken!(conn, series, row, own) {
                    return Ok(false);
                }
            }
            let inserted = diesel::insert_into(calendar_events::table)
                .values(&row)
                .on_conflict_do_nothing()
                .execute(conn)
                .await?;
            if inserted == 0 {
                if existing != Existing::Left {
                    diesel::update(calendar_events::table.find(&row.id))
                        .set(changes(&row))
                        .execute(conn)
                        .await?;
                }
                return Ok(true);
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
            Ok(true)
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

/// The occurrences of `series` its organizer gave a time of their own that now start at `start_ms`, two at most:
/// not a cancelled one, whose time another may since have taken.
pub(super) async fn changed_rows_at(db: &Db, series: &str, start_ms: i64) -> Result<Vec<String>, DbError> {
    with_conn!(db, |conn| Ok(calendar_events::table
        .filter(calendar_events::series_id.eq(series))
        .filter(calendar_events::changed.eq(true))
        .filter(calendar_events::cancelled.eq(false))
        .filter(calendar_events::start_ms.eq(start_ms))
        .order(calendar_events::id.asc())
        .select(calendar_events::id)
        .limit(2)
        .load(conn)
        .await?))
}

/// Makes a meeting's chat follow the invitations of its event `id` (a series with the occurrences kept under it)
/// as they now stand: of `wanted` whoever it invites is put in, of `gone` whoever it doesn't is taken out. The
/// event's row is held meanwhile, so two changes of its invitations settle the chat one after the other, each on
/// what is true by then. Whether the chat changed.
pub(super) async fn settle_chat(
    db: &Db,
    id: &str,
    chat: &str,
    wanted: &[String],
    gone: &[String],
) -> Result<bool, DbError> {
    let at = crate::chatsvc::now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            hold!(conn, id);
            let invited = invited!(conn, id);
            let wanted: Vec<&String> = wanted.iter().filter(|user| invited.contains(user)).collect();
            let gone: Vec<&String> = gone.iter().filter(|user| !invited.contains(user)).collect();
            Ok(crate::chatsvc::repo::set_members!(conn, chat, wanted, gone, at))
        })
        .await)
}

/// Everyone the event `id` invites, a series with the occurrences kept under it.
pub(super) async fn invited(db: &Db, id: &str) -> Result<Vec<String>, DbError> {
    with_conn!(db, |conn| Ok(invited!(conn, id)))
}

/// Gives the event (and the occurrences kept under it) the meeting's chat `chat`, unless it has one by then: its
/// row is held from the read to the write. The chat it now has.
pub(super) async fn give_meeting(db: &Db, id: &str, chat: &str) -> Result<String, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            hold!(conn, id);
            let had: Option<String> = calendar_events::table
                .find(id)
                .select(calendar_events::meeting_chat)
                .first(conn)
                .await?;
            if let Some(had) = had {
                return Ok(had);
            }
            diesel::update(
                calendar_events::table.filter(calendar_events::id.eq(id).or(calendar_events::series_id.eq(id))),
            )
            .set(calendar_events::meeting_chat.eq(chat))
            .execute(conn)
            .await?;
            Ok(chat.to_owned())
        })
        .await)
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

/// Cancels an event, a series with the occurrences kept under it. The series' row goes first: a row being kept
/// under it ([`keep_own_row`]) is there by the time the occurrences are reached.
pub(super) async fn cancel(db: &Db, id: &str, version: i64) -> Result<(), DbError> {
    let cancelled = (
        calendar_events::cancelled.eq(true),
        calendar_events::version.eq(version),
    );
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::update(calendar_events::table.find(id))
                .set(cancelled)
                .execute(conn)
                .await?;
            diesel::update(calendar_events::table.filter(calendar_events::series_id.eq(id)))
                .set(cancelled)
                .execute(conn)
                .await?;
            Ok(())
        })
        .await)
}

/// Makes the invitations of one occurrence kept as the row `event` under `series` the accounts `wanted`
/// ([`sync!`]), holding the series' row: the invitations under a series change one request after the other. Who
/// lost the last invitation under the series.
pub(super) async fn sync_attendees(
    db: &Db,
    series: &str,
    event: &str,
    wanted: &[(String, bool)],
    reset: bool,
) -> Result<Vec<String>, DbError> {
    let events = [event.to_owned()];
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            hold!(conn, series);
            let before = invited!(conn, series);
            sync!(conn, &events, wanted, reset);
            let still = invited!(conn, series);
            Ok(before.into_iter().filter(|user| !still.contains(user)).collect())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::{self, Actor, NewAccount};

    fn series(owner: &str) -> Event {
        Event {
            id: "series".into(),
            owner_id: owner.to_owned(),
            team_id: None,
            channel_id: None,
            post_id: None,
            series_id: None,
            original_start: None,
            start_ms: 1000,
            end_ms: 2000,
            until_ms: i64::MAX,
            recurring: true,
            changed: false,
            cancelled: false,
            meeting_chat: None,
            body: "{}".into(),
            version: 1,
        }
    }

    async fn account(db: &Db, upn: &str) -> String {
        let new = NewAccount {
            upn,
            display_name: upn,
            password: "correct horse battery",
            admin: false,
            must_change_password: false,
        };
        admin::create_user(db, Actor::CLI, &new).await.unwrap().id
    }

    /// An attendee read an occurrence before it had a row; the organizer moved it; the attendee's answer then
    /// keeps the row for it.
    #[tokio::test]
    async fn a_row_kept_for_an_answer_leaves_the_organizers_change_of_the_occurrence() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("calendar.sqlite").display());
        let db = Db::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        let (ada, bert) = (account(&db, "ada@own.test").await, account(&db, "bert@own.test").await);
        let series = series(&ada);
        let invited = Attendee {
            event_id: series.id.clone(),
            user_id: bert.clone(),
            position: 0,
            optional: false,
            response: "Accepted".into(),
            responded_at: Some(5),
            removed: false,
        };
        insert(&db, &series, &[invited]).await.unwrap();
        let seen = Event {
            id: "series.8000".into(),
            series_id: Some(series.id.clone()),
            original_start: Some(8000),
            start_ms: 8000,
            end_ms: 9000,
            until_ms: 9000,
            recurring: false,
            ..series.clone()
        };
        let moved = Event {
            start_ms: 8500,
            end_ms: 9500,
            until_ms: 9500,
            changed: true,
            cancelled: true,
            version: 2,
            ..seen.clone()
        };
        put_own_row(&db, &moved).await.unwrap();

        assert!(keep_own_row(&db, &seen).await.unwrap());
        let (kept, mine) = event(&db, &seen.id, &bert).await.unwrap().unwrap();
        assert_eq!((kept.start_ms, kept.end_ms, kept.version), (8500, 9500, 2));
        assert!(kept.changed && kept.cancelled);
        assert_eq!(
            mine.unwrap().response,
            "Accepted",
            "the series' invitations came with the row"
        );
        assert_eq!(
            changed_rows_at(&db, &series.id, 8500).await.unwrap(),
            [""; 0],
            "cancelled, it isn't the one that starts there"
        );

        // the organizer's own next change is written over it
        put_own_row(
            &db,
            &Event {
                cancelled: false,
                ..moved
            },
        )
        .await
        .unwrap();
        assert!(!event(&db, &seen.id, &bert).await.unwrap().unwrap().0.cancelled);
        assert_eq!(changed_rows_at(&db, &series.id, 8500).await.unwrap(), ["series.8000"]);
        // and a row kept for an occurrence that has none is made
        let other = Event {
            id: "series.20000".into(),
            original_start: Some(20000),
            ..seen
        };
        assert!(keep_own_row(&db, &other).await.unwrap());
        assert!(event(&db, &other.id, &bert).await.unwrap().unwrap().1.is_some());
        assert_eq!(super::invited(&db, &series.id).await.unwrap(), [bert]);
    }

    /// An attendee read an occurrence of a live series; the organizer cancelled the series; the attendee's answer
    /// then makes no live row of it.
    #[tokio::test]
    async fn a_row_isnt_kept_from_a_series_read_before_it_was_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("calendar.sqlite").display());
        let db = Db::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        let (ada, bert) = (account(&db, "ada@own.test").await, account(&db, "bert@own.test").await);
        let series = series(&ada);
        let invited = Attendee {
            event_id: series.id.clone(),
            user_id: bert.clone(),
            position: 0,
            optional: false,
            response: "None".into(),
            responded_at: None,
            removed: false,
        };
        insert(&db, &series, &[invited]).await.unwrap();
        let occurrence = |start: i64, of: &Event| Event {
            id: format!("series.{start}"),
            series_id: Some(of.id.clone()),
            original_start: Some(start),
            start_ms: start,
            end_ms: start + 1000,
            until_ms: start + 1000,
            recurring: false,
            ..of.clone()
        };
        assert!(keep_own_row(&db, &occurrence(8000, &series)).await.unwrap());

        cancel(&db, &series.id, 2).await.unwrap();
        assert!(event(&db, "series.8000", &bert).await.unwrap().unwrap().0.cancelled);
        assert!(!keep_own_row(&db, &occurrence(20000, &series)).await.unwrap());
        assert!(event(&db, "series.20000", &bert).await.unwrap().is_none());

        // read as it now is, the cancelled occurrence is kept for its removal
        let (now, _) = event(&db, &series.id, &bert).await.unwrap().unwrap();
        assert!(keep_own_row(&db, &occurrence(20000, &now)).await.unwrap());
        assert!(event(&db, "series.20000", &bert).await.unwrap().unwrap().0.cancelled);
    }

    /// A channel's occurrence is written to a start only while no other live one of its series is there.
    #[tokio::test]
    async fn a_row_isnt_put_at_the_start_another_live_one_of_its_series_is_at() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("calendar.sqlite").display());
        let db = Db::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        let series = series(&account(&db, "ada@own.test").await);
        insert(&db, &series, &[]).await.unwrap();
        let moved = |from: i64, to: i64| Event {
            id: format!("series.{from}"),
            series_id: Some(series.id.clone()),
            original_start: Some(from),
            start_ms: to,
            end_ms: to + 1000,
            until_ms: to + 1000,
            recurring: false,
            changed: true,
            version: 2,
            ..series.clone()
        };
        let start_of = async |id: &str| event(&db, id, "").await.unwrap().map(|(row, _)| row.start_ms);

        // the first to a free start is written, the second to the same start isn't
        assert!(put_own_row_apart(&db, &moved(8000, 9500), None).await.unwrap());
        assert!(!put_own_row_apart(&db, &moved(20000, 9500), None).await.unwrap());
        assert_eq!(start_of("series.20000").await, None);
        // the one that is there is written again where it is
        assert!(put_own_row_apart(&db, &moved(8000, 9500), None).await.unwrap());

        // the start the rule gives another one is taken while that one has no row, or an unchanged one
        let unchanged = Event {
            changed: false,
            ..moved(32000, 32000)
        };
        assert!(
            !put_own_row_apart(&db, &moved(20000, 32000), Some("series.32000"))
                .await
                .unwrap()
        );
        assert!(
            keep_own_row(
                &db,
                &Event {
                    version: 1,
                    ..unchanged.clone()
                }
            )
            .await
            .unwrap()
        );
        assert!(
            !put_own_row_apart(&db, &moved(20000, 32000), Some("series.32000"))
                .await
                .unwrap()
        );
        // and free once that one is moved away, or cancelled
        assert!(put_own_row_apart(&db, &moved(32000, 33000), None).await.unwrap());
        assert!(
            put_own_row_apart(&db, &moved(20000, 32000), Some("series.32000"))
                .await
                .unwrap()
        );
        // moved back to its own start, which another has since taken, it is refused
        assert!(!put_own_row_apart(&db, &moved(32000, 32000), None).await.unwrap());
        let cancelled = Event {
            cancelled: true,
            ..moved(8000, 8000)
        };
        put_own_row(&db, &cancelled).await.unwrap();
        assert!(
            put_own_row_apart(&db, &moved(20000, 8000), Some("series.8000"))
                .await
                .unwrap()
        );
        assert_eq!(start_of("series.20000").await, Some(8000));
    }

    /// Of two events that name one meeting's chat, the second isn't kept.
    #[tokio::test]
    async fn a_meeting_is_taken_by_one_event_only() {
        const CHAT: &str = "19:meeting_taken@thread.v2";
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("calendar.sqlite").display());
        let db = Db::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        let ada = account(&db, "ada@own.test").await;
        let of = |id: &str, chat: Option<&str>| Event {
            id: id.into(),
            meeting_chat: chat.map(str::to_owned),
            ..series(&ada)
        };
        insert(&db, &of("first", Some(CHAT)), &[]).await.unwrap();
        let second = insert(&db, &of("second", Some(CHAT)), &[]).await;
        assert!(matches!(second, Err(DbError::Conflict)), "{second:?}");
        assert!(event(&db, "second", &ada).await.unwrap().is_none());
        insert(&db, &of("third", None), &[]).await.unwrap();
        insert(&db, &of("fourth", Some("19:meeting_other@thread.v2")), &[])
            .await
            .unwrap();
    }

    async fn chat_of(db: &Db, id: &str, creator: &str) -> Result<(), DbError> {
        use crate::chatsvc::schema::chats;
        let chat = crate::chatsvc::repo::Chat {
            id: id.to_owned(),
            creator: creator.to_owned(),
            topic: None,
            unique_roster: false,
            created_at: 1,
            version: 1,
            roster_version: 1,
        };
        with_conn!(db, |conn| {
            diesel::insert_into(chats::table).values(&chat).execute(conn).await?;
        });
        Ok(())
    }

    async fn in_chat(db: &Db, chat: &str) -> Result<Vec<String>, DbError> {
        use crate::chatsvc::schema::chat_members;
        with_conn!(db, |conn| Ok(chat_members::table
            .filter(chat_members::chat_id.eq(chat))
            .order(chat_members::position.asc())
            .select(chat_members::user_id)
            .load(conn)
            .await?))
    }

    /// The chat takes in only who is invited by now and loses only who isn't, whatever a request saw; and the
    /// change that takes someone's last invitation is the one that names them gone.
    #[tokio::test]
    async fn a_chat_follows_the_invitations_as_they_stand() {
        const CHAT: &str = "19:meeting_sync@thread.v2";
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("calendar.sqlite").display());
        let db = Db::connect(&url, 1).await.unwrap();
        db.migrate().await.unwrap();
        let (ada, bert) = (account(&db, "ada@own.test").await, account(&db, "bert@own.test").await);
        let cora = account(&db, "cora@own.test").await;
        let series = series(&ada);
        let invited = Attendee {
            event_id: series.id.clone(),
            user_id: bert.clone(),
            position: 0,
            optional: false,
            response: "Accepted".into(),
            responded_at: Some(5),
            removed: false,
        };
        insert(&db, &series, &[invited]).await.unwrap();
        chat_of(&db, CHAT, &ada).await.unwrap();
        let (only_bert, only_cora) = ([bert.clone()], [cora.clone()]);
        let both = [(bert.clone(), false), (cora.clone(), false)];
        assert!(settle_chat(&db, &series.id, CHAT, &only_bert, &[]).await.unwrap());

        // a request that saw Bert gone and Cora invited, when neither is so
        assert!(
            !settle_chat(&db, &series.id, CHAT, &only_cora, &only_bert)
                .await
                .unwrap()
        );
        assert_eq!(in_chat(&db, CHAT).await.unwrap(), only_bert);

        // Cora on one occurrence alone, then off it: that change names her gone, and Bert keeps the series'
        let friday = Event {
            id: "series.8000".into(),
            series_id: Some(series.id.clone()),
            original_start: Some(8000),
            start_ms: 8000,
            end_ms: 9000,
            until_ms: 9000,
            recurring: false,
            ..series.clone()
        };
        put_own_row(&db, &friday).await.unwrap();
        let gone = sync_attendees(&db, &series.id, &friday.id, &both, false).await.unwrap();
        assert!(gone.is_empty());
        assert!(settle_chat(&db, &series.id, CHAT, &only_cora, &gone).await.unwrap());
        let gone = sync_attendees(&db, &series.id, &friday.id, &both[1..], false)
            .await
            .unwrap();
        assert!(gone.is_empty(), "Bert, off this occurrence, still has the series");
        let gone = sync_attendees(&db, &series.id, &friday.id, &both[..1], true)
            .await
            .unwrap();
        assert_eq!(gone, only_cora);
        let (_, mine) = event(&db, &friday.id, &bert).await.unwrap().unwrap();
        assert_eq!(mine.unwrap().response, "None", "asked again");
        assert!(settle_chat(&db, &series.id, CHAT, &only_bert, &gone).await.unwrap());
        assert_eq!(in_chat(&db, CHAT).await.unwrap(), only_bert);

        // the series changed whole brings its occurrence's invitations along in the same step
        sync_attendees(&db, &series.id, &friday.id, &both, false).await.unwrap();
        let renamed = Event {
            version: 2,
            ..series.clone()
        };
        let gone = save_whole(&db, &renamed, Some(Kept::Given("{}")), &both[..1], false)
            .await
            .unwrap();
        assert_eq!(gone, only_cora);
        assert!(event(&db, &friday.id, &cora).await.unwrap().unwrap().1.is_none());
        assert_eq!(event(&db, &friday.id, &bert).await.unwrap().unwrap().0.version, 2);

        // and one given a new rule forgets the occurrence, and whoever it alone invited
        sync_attendees(&db, &series.id, &friday.id, &both, false).await.unwrap();
        let reshaped = Event {
            version: 3,
            ..series.clone()
        };
        let gone = save_whole(&db, &reshaped, Some(Kept::Forgotten), &both[..1], true)
            .await
            .unwrap();
        assert_eq!(gone, only_cora);
        assert!(event(&db, &friday.id, &bert).await.unwrap().is_none());
        assert_eq!(super::invited(&db, &series.id).await.unwrap(), only_bert);
    }
}
