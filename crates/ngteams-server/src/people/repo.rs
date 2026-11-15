//! The directory beside the accounts: one optional row of profile fields per account, and the reads the person card
//! is made of.

use std::collections::{HashMap, HashSet};

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use crate::chatsvc::schema::chat_members;
use crate::db::schema::users;
use crate::db::users::User;
use crate::db::{Db, DbError, with_conn};

diesel::table! {
    user_profiles (user_id) {
        user_id -> Text,
        given_name -> Nullable<Text>,
        surname -> Nullable<Text>,
        job_title -> Nullable<Text>,
        department -> Nullable<Text>,
        office_location -> Nullable<Text>,
        company_name -> Nullable<Text>,
        mobile_phone -> Nullable<Text>,
        business_phone -> Nullable<Text>,
        manager_id -> Nullable<Text>,
        out_of_office -> Bool,
        picture_key -> Nullable<Text>,
        picture_size -> BigInt,
        updated_at -> BigInt,
    }
}

/// What the directory knows of an account beside its name and address.
#[derive(Clone, Debug, Default, PartialEq, Eq, Queryable, Selectable, Insertable, AsChangeset)]
#[diesel(table_name = user_profiles, treat_none_as_null = true)]
pub struct Profile {
    pub user_id: String,
    pub given_name: Option<String>,
    pub surname: Option<String>,
    pub job_title: Option<String>,
    pub department: Option<String>,
    pub office_location: Option<String>,
    pub company_name: Option<String>,
    pub mobile_phone: Option<String>,
    pub business_phone: Option<String>,
    /// The account this one reports to.
    pub manager_id: Option<String>,
    /// Whether its automatic replies are on.
    pub out_of_office: bool,
    /// Where its picture is in the file store.
    pub picture_key: Option<String>,
    pub picture_size: i64,
    pub updated_at: i64,
}

/// An account with what the directory knows of it.
#[derive(Clone, Debug)]
pub struct Person {
    pub user: User,
    pub profile: Profile,
}

/// The profile of `id`; an account without a row has an empty one.
///
/// # Errors
/// A database failure.
pub async fn profile(db: &Db, id: &str) -> Result<Profile, DbError> {
    let found: Option<Profile> = with_conn!(db, |conn| user_profiles::table
        .find(id)
        .select(Profile::as_select())
        .first(conn)
        .await
        .optional()?);
    Ok(found.unwrap_or_else(|| Profile {
        user_id: id.to_owned(),
        ..Profile::default()
    }))
}

/// Keeps `profile` in place of the account's earlier one.
///
/// # Errors
/// A database failure.
pub async fn put_profile(db: &Db, profile: &Profile) -> Result<(), DbError> {
    with_conn!(db, |conn| diesel::insert_into(user_profiles::table)
        .values(profile)
        .on_conflict(user_profiles::user_id)
        .do_update()
        .set(profile)
        .execute(conn)
        .await?);
    Ok(())
}

/// Of `ids`, the accounts whose automatic replies are on.
///
/// # Errors
/// A database failure.
pub async fn out_of_office(db: &Db, ids: &[String]) -> Result<HashSet<String>, DbError> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let found: Vec<String> = with_conn!(db, |conn| user_profiles::table
        .filter(user_profiles::user_id.eq_any(ids))
        .filter(user_profiles::out_of_office.eq(true))
        .select(user_profiles::user_id)
        .load(conn)
        .await?);
    Ok(found.into_iter().collect())
}

async fn with_profiles(db: &Db, found: Vec<User>) -> Result<Vec<Person>, DbError> {
    let ids: Vec<&str> = found.iter().map(|user| user.id.as_str()).collect();
    let mut profiles: Vec<Profile> = if ids.is_empty() {
        Vec::new()
    } else {
        with_conn!(db, |conn| user_profiles::table
            .filter(user_profiles::user_id.eq_any(&ids))
            .select(Profile::as_select())
            .load(conn)
            .await?)
    };
    Ok(found
        .into_iter()
        .map(|user| {
            let at = profiles.iter().position(|profile| profile.user_id == user.id);
            let profile = at.map_or_else(
                || Profile {
                    user_id: user.id.clone(),
                    ..Profile::default()
                },
                |at| profiles.swap_remove(at),
            );
            Person { user, profile }
        })
        .collect())
}

/// The accounts of `ids` that weren't deleted, in the order asked, each once.
///
/// # Errors
/// A database failure.
pub async fn people(db: &Db, ids: &[String]) -> Result<Vec<Person>, DbError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut found: Vec<User> = with_conn!(db, |conn| users::table
        .filter(users::id.eq_any(ids))
        .filter(users::deleted_at.is_null())
        .select(User::as_select())
        .load(conn)
        .await?);
    let place = |user: &User| ids.iter().position(|id| *id == user.id);
    found.sort_by_key(place);
    with_profiles(db, found).await
}

/// The accounts with the addresses `upns` (any case) that weren't deleted, in the order asked.
///
/// # Errors
/// A database failure.
pub async fn people_by_address(db: &Db, upns: &[String]) -> Result<Vec<Person>, DbError> {
    let upns: Vec<String> = upns.iter().map(|upn| crate::db::users::normalize_upn(upn)).collect();
    if upns.is_empty() {
        return Ok(Vec::new());
    }
    let mut found: Vec<User> = with_conn!(db, |conn| users::table
        .filter(users::upn.eq_any(&upns))
        .filter(users::deleted_at.is_null())
        .select(User::as_select())
        .load(conn)
        .await?);
    let place = |user: &User| upns.iter().position(|upn| *upn == user.upn);
    found.sort_by_key(place);
    with_profiles(db, found).await
}

/// The account `id` when it wasn't deleted.
///
/// # Errors
/// A database failure.
pub async fn person(db: &Db, id: &str) -> Result<Option<Person>, DbError> {
    Ok(people(db, &[id.to_owned()]).await?.pop())
}

/// The accounts that report to `manager` and can sign in, by name, at most `limit`.
///
/// # Errors
/// A database failure.
pub async fn directs(db: &Db, manager: &str, limit: i64) -> Result<Vec<User>, DbError> {
    let ids: Vec<String> = with_conn!(db, |conn| user_profiles::table
        .filter(user_profiles::manager_id.eq(manager))
        .select(user_profiles::user_id)
        .load(conn)
        .await?);
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    with_conn!(db, |conn| Ok(users::table
        .filter(users::id.eq_any(&ids))
        .filter(users::disabled.eq(false))
        .order((users::display_name.asc(), users::id.asc()))
        .limit(limit)
        .select(User::as_select())
        .load(conn)
        .await?))
}

/// The threads `user` is in now.
async fn seats(db: &Db, user: &str) -> Result<Vec<String>, DbError> {
    with_conn!(db, |conn| Ok(chat_members::table
        .filter(chat_members::user_id.eq(user))
        .filter(chat_members::left_at.is_null())
        .select(chat_members::chat_id)
        .load(conn)
        .await?))
}

/// The people beside `viewer` and `person` in the threads both are in now, once for each such thread.
async fn beside(db: &Db, viewer: &str, person: &str) -> Result<Vec<String>, DbError> {
    let viewers = seats(db, viewer).await?;
    let both: Vec<String> = seats(db, person)
        .await?
        .into_iter()
        .filter(|thread| viewers.contains(thread))
        .collect();
    with_conn!(db, |conn| Ok(chat_members::table
        .filter(chat_members::chat_id.eq_any(&both))
        .filter(chat_members::left_at.is_null())
        .filter(chat_members::user_id.ne(viewer))
        .filter(chat_members::user_id.ne(person))
        .select(chat_members::user_id)
        .load(conn)
        .await?))
}

/// The people in the threads both `viewer` and `person` are in now, those in the most of them first, at most
/// `limit`: whom `viewer` already sees `person` work with.
///
/// # Errors
/// A database failure.
pub async fn works_with(db: &Db, viewer: &str, person: &str, limit: usize) -> Result<Vec<User>, DbError> {
    let mut shared: HashMap<String, usize> = HashMap::new();
    for other in beside(db, viewer, person).await? {
        *shared.entry(other).or_default() += 1;
    }
    if shared.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<&str> = shared.keys().map(String::as_str).collect();
    let mut found: Vec<User> = with_conn!(db, |conn| users::table
        .filter(users::id.eq_any(&ids))
        .filter(users::disabled.eq(false))
        .select(User::as_select())
        .load(conn)
        .await?);
    found.sort_by(|a, b| {
        shared[&b.id]
            .cmp(&shared[&a.id])
            .then_with(|| a.display_name.cmp(&b.display_name))
            .then_with(|| a.id.cmp(&b.id))
    });
    found.truncate(limit);
    Ok(found)
}
