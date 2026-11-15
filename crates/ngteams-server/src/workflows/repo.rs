//! The workflows' rows: connections, workflows with their owners, and runs.

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::schema::{workflow_connections, workflow_owners, workflow_runs, workflows};
use crate::db::{Db, DbError, with_conn};

/// A connector connection an account made.
#[derive(Clone, Debug, Queryable, Selectable, Insertable)]
#[diesel(table_name = workflow_connections)]
pub(super) struct Connection {
    pub name: String,
    pub owner_id: String,
    pub api: String,
    pub created_at: i64,
}

/// The account's connections, oldest first.
pub(super) async fn connections(db: &Db, owner: &str) -> Result<Vec<Connection>, DbError> {
    with_conn!(db, |conn| Ok(workflow_connections::table
        .filter(workflow_connections::owner_id.eq(owner))
        .order((workflow_connections::created_at, workflow_connections::name))
        .select(Connection::as_select())
        .load(conn)
        .await?))
}

/// Keeps `connection` unless its name is taken; the connection under that name either way.
pub(super) async fn put_connection(db: &Db, connection: &Connection) -> Result<Connection, DbError> {
    with_conn!(db, |conn| {
        diesel::insert_into(workflow_connections::table)
            .values(connection)
            .on_conflict_do_nothing()
            .execute(conn)
            .await?;
        Ok(workflow_connections::table
            .find(&connection.name)
            .select(Connection::as_select())
            .first(conn)
            .await?)
    })
}

/// A workflow as kept: its Power Automate properties (JSON) beside what the server reads of them.
#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = workflows)]
pub(super) struct Workflow {
    pub id: String,
    pub creator_id: String,
    pub display_name: String,
    pub enabled: bool,
    pub properties: String,
    /// The channel or chat it posts to.
    pub target_thread: String,
    /// Its webhook link's secret.
    pub secret: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl std::fmt::Debug for Workflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workflow")
            .field("id", &self.id)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

/// Keeps a new workflow with its creator as its owner.
pub(super) async fn insert(db: &Db, workflow: &Workflow) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::insert_into(workflows::table)
                .values(workflow)
                .execute(conn)
                .await?;
            diesel::insert_into(workflow_owners::table)
                .values((
                    workflow_owners::workflow_id.eq(&workflow.id),
                    workflow_owners::user_id.eq(&workflow.creator_id),
                ))
                .execute(conn)
                .await?;
            Ok(())
        })
        .await)
}

/// The workflow `id`.
pub(super) async fn workflow(db: &Db, id: &str) -> Result<Option<Workflow>, DbError> {
    with_conn!(db, |conn| Ok(workflows::table
        .find(id)
        .select(Workflow::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// The workflow `id` when `user` owns it.
pub(super) async fn owned(db: &Db, id: &str, user: &str) -> Result<Option<Workflow>, DbError> {
    with_conn!(db, |conn| Ok(workflows::table
        .inner_join(workflow_owners::table)
        .filter(workflows::id.eq(id))
        .filter(workflow_owners::user_id.eq(user))
        .select(Workflow::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// The workflows `user` owns, newest first.
pub(super) async fn owned_by(db: &Db, user: &str) -> Result<Vec<Workflow>, DbError> {
    with_conn!(db, |conn| Ok(workflows::table
        .inner_join(workflow_owners::table)
        .filter(workflow_owners::user_id.eq(user))
        .order((workflows::created_at.desc(), workflows::id))
        .select(Workflow::as_select())
        .load(conn)
        .await?))
}

/// Replaces what a change may change; whether the workflow was there.
pub(super) async fn update(db: &Db, workflow: &Workflow) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(diesel::update(workflows::table.find(&workflow.id))
        .set((
            workflows::display_name.eq(&workflow.display_name),
            workflows::enabled.eq(workflow.enabled),
            workflows::properties.eq(&workflow.properties),
            workflows::target_thread.eq(&workflow.target_thread),
            workflows::updated_at.eq(workflow.updated_at),
        ))
        .execute(conn)
        .await?
        > 0))
}

/// Deletes the workflow with its owners and runs; whether it was there.
pub(super) async fn delete(db: &Db, id: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::delete(workflow_runs::table.filter(workflow_runs::workflow_id.eq(id)))
                .execute(conn)
                .await?;
            diesel::delete(workflow_owners::table.filter(workflow_owners::workflow_id.eq(id)))
                .execute(conn)
                .await?;
            Ok(diesel::delete(workflows::table.find(id)).execute(conn).await? > 0)
        })
        .await)
}

/// The workflow's owners, by id.
pub(super) async fn owners(db: &Db, id: &str) -> Result<Vec<String>, DbError> {
    with_conn!(db, |conn| Ok(workflow_owners::table
        .filter(workflow_owners::workflow_id.eq(id))
        .order(workflow_owners::user_id)
        .select(workflow_owners::user_id)
        .load(conn)
        .await?))
}

/// Makes `user` an owner too.
pub(super) async fn add_owner(db: &Db, id: &str, user: &str) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::insert_into(workflow_owners::table)
            .values((workflow_owners::workflow_id.eq(id), workflow_owners::user_id.eq(user)))
            .on_conflict_do_nothing()
            .execute(conn)
            .await?;
        Ok(())
    })
}

/// Takes `user` off the owners; whether they were one.
pub(super) async fn remove_owner(db: &Db, id: &str, user: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(diesel::delete(
        workflow_owners::table
            .filter(workflow_owners::workflow_id.eq(id))
            .filter(workflow_owners::user_id.eq(user))
    )
    .execute(conn)
    .await?
        > 0))
}

/// One run of a workflow.
#[derive(Clone, Debug, Queryable, Selectable, Insertable)]
#[diesel(table_name = workflow_runs)]
pub(super) struct Run {
    pub workflow_id: String,
    pub id: String,
    pub started_at: i64,
    pub ended_at: i64,
    /// `Succeeded` or `Failed`.
    pub status: String,
    /// Why it failed: the failing action's error code.
    pub error: Option<String>,
}

/// Keeps `run`, and of the workflow's runs only the newest `kept`.
pub(super) async fn record(db: &Db, run: &Run, kept: u32) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::insert_into(workflow_runs::table)
                .values(run)
                .execute(conn)
                .await?;
            let old: Vec<String> = workflow_runs::table
                .filter(workflow_runs::workflow_id.eq(&run.workflow_id))
                .order((workflow_runs::started_at.desc(), workflow_runs::id.desc()))
                .offset(i64::from(kept))
                .select(workflow_runs::id)
                .load(conn)
                .await?;
            if !old.is_empty() {
                diesel::delete(
                    workflow_runs::table
                        .filter(workflow_runs::workflow_id.eq(&run.workflow_id))
                        .filter(workflow_runs::id.eq_any(&old)),
                )
                .execute(conn)
                .await?;
            }
            Ok(())
        })
        .await)
}

/// The workflow's runs, newest first.
pub(super) async fn runs(db: &Db, id: &str) -> Result<Vec<Run>, DbError> {
    with_conn!(db, |conn| Ok(workflow_runs::table
        .filter(workflow_runs::workflow_id.eq(id))
        .order((workflow_runs::started_at.desc(), workflow_runs::id.desc()))
        .select(Run::as_select())
        .load(conn)
        .await?))
}

/// One run of the workflow.
pub(super) async fn run(db: &Db, id: &str, run: &str) -> Result<Option<Run>, DbError> {
    with_conn!(db, |conn| Ok(workflow_runs::table
        .find((id, run))
        .select(Run::as_select())
        .first(conn)
        .await
        .optional()?))
}
