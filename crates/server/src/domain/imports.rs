//! Import reports (decision 30): what an import did, per collection, and
//! every object it could not store, kept as the bytes it received. Visible
//! to the account the objects went into and to whoever started it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::types::Json;
use uuid::Uuid;

use super::{Actor, DomainError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Done,
    Failed,
    /// The server stopped while it ran; running it again converges.
    Interrupted,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Interrupted => "interrupted",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "running" => Status::Running,
            "done" => Status::Done,
            "failed" => Status::Failed,
            _ => Status::Interrupted,
        }
    }
}

/// One collection's share of an import.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionCounts {
    pub collection_id: Option<Uuid>,
    pub name: String,
    /// Whether the import created the collection.
    pub created_collection: bool,
    pub created: u32,
    pub updated: u32,
    pub unchanged: u32,
    pub failed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub collection: String,
    pub reason: String,
    pub raw: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub principal_name: String,
    pub started_by: Option<Uuid>,
    pub source: String,
    pub status: Status,
    pub message: String,
    pub summary: Vec<CollectionCounts>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl Import {
    pub fn totals(&self) -> CollectionCounts {
        let mut t = CollectionCounts::default();
        for c in &self.summary {
            t.created += c.created;
            t.updated += c.updated;
            t.unchanged += c.unchanged;
            t.failed += c.failed;
        }
        t
    }
}

type Row = (
    Uuid,
    Uuid,
    String,
    Option<Uuid>,
    String,
    String,
    String,
    Json<Vec<CollectionCounts>>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);

const SELECT: &str = "SELECT i.id, i.principal_id, p.name, i.started_by, i.source, i.status, i.message, i.summary,
                             i.created_at, i.finished_at
                      FROM imports i JOIN principals p ON p.id = i.principal_id";

fn from_row(r: Row) -> Import {
    Import {
        id: r.0,
        principal_id: r.1,
        principal_name: r.2,
        started_by: r.3,
        source: r.4,
        status: Status::parse(&r.5),
        message: r.6,
        summary: r.7.0,
        created_at: r.8,
        finished_at: r.9,
    }
}

/// Start a report. `source` must never contain a credential.
pub async fn start(pool: &PgPool, into: Uuid, started_by: Actor, source: &str) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO imports (id, principal_id, started_by, source, status) VALUES ($1, $2, $3, $4, 'running')",
    )
    .bind(id)
    .bind(into)
    .bind(started_by)
    .bind(source)
    .execute(pool)
    .await?;
    Ok(id)
}

/// Save the running counts (so the page shows progress).
pub async fn progress(pool: &PgPool, id: Uuid, summary: &[CollectionCounts]) -> Result<()> {
    sqlx::query("UPDATE imports SET summary = $2 WHERE id = $1").bind(id).bind(Json(summary)).execute(pool).await?;
    Ok(())
}

/// Keep one object that could not be stored.
pub async fn add_failure(pool: &PgPool, id: Uuid, position: i32, f: &Failure) -> Result<()> {
    sqlx::query(
        "INSERT INTO import_failures (import_id, position, collection, reason, raw) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(position)
    .bind(&f.collection)
    .bind(&f.reason)
    .bind(&f.raw)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn finish(
    pool: &PgPool,
    id: Uuid,
    status: Status,
    message: &str,
    summary: &[CollectionCounts],
) -> Result<()> {
    sqlx::query("UPDATE imports SET status = $2, message = $3, summary = $4, finished_at = now() WHERE id = $1")
        .bind(id)
        .bind(status.as_str())
        .bind(message)
        .bind(Json(summary))
        .execute(pool)
        .await?;
    Ok(())
}

/// At startup: imports that were running when the server stopped.
pub async fn mark_interrupted(pool: &PgPool) -> Result<u64> {
    let r = sqlx::query("UPDATE imports SET status = 'interrupted', finished_at = now() WHERE status = 'running'")
        .execute(pool)
        .await?;
    Ok(r.rows_affected())
}

/// One report, if the actor may see it.
pub async fn get(pool: &PgPool, actor: Actor, id: Uuid) -> Result<Import> {
    let row: Option<Row> =
        sqlx::query_as(&format!("{SELECT} WHERE i.id = $1 AND (i.principal_id = $2 OR i.started_by = $2)"))
            .bind(id)
            .bind(actor)
            .fetch_optional(pool)
            .await?;
    row.map(from_row).ok_or(DomainError::NotFound)
}

/// The actor's recent reports, newest first.
pub async fn recent(pool: &PgPool, actor: Actor, limit: i64) -> Result<Vec<Import>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT} WHERE i.principal_id = $1 OR i.started_by = $1 ORDER BY i.created_at DESC LIMIT $2"
    ))
    .bind(actor)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(from_row).collect())
}

/// The failures of a report the actor may see: position, collection, reason.
pub async fn failures(pool: &PgPool, actor: Actor, id: Uuid) -> Result<Vec<(i32, String, String)>> {
    get(pool, actor, id).await?;
    Ok(sqlx::query_as(
        "SELECT position, collection, reason FROM import_failures WHERE import_id = $1 ORDER BY position",
    )
    .bind(id)
    .fetch_all(pool)
    .await?)
}

/// The bytes of one failed object.
pub async fn failure_raw(pool: &PgPool, actor: Actor, id: Uuid, position: i32) -> Result<Vec<u8>> {
    get(pool, actor, id).await?;
    let raw: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT raw FROM import_failures WHERE import_id = $1 AND position = $2")
            .bind(id)
            .bind(position)
            .fetch_optional(pool)
            .await?;
    raw.ok_or(DomainError::NotFound)
}
