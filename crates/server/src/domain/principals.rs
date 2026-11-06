//! Users and groups.

use sqlx::PgPool;
use uuid::Uuid;

use super::{DomainError, Result, valid_slug};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Principal {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub display_name: String,
    pub disabled: bool,
}

async fn create(pool: &PgPool, kind: &str, name: &str, display_name: &str) -> Result<Uuid> {
    let name = name.to_ascii_lowercase();
    if !valid_slug(&name) {
        return Err(DomainError::BadName);
    }
    let id = Uuid::new_v4();
    let res = sqlx::query("INSERT INTO principals (id, kind, name, display_name) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(kind)
        .bind(&name)
        .bind(display_name)
        .execute(pool)
        .await;
    match res {
        Ok(_) => Ok(id),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(DomainError::NameTaken),
        Err(e) => Err(e.into()),
    }
}

pub async fn create_user(pool: &PgPool, name: &str, display_name: &str) -> Result<Uuid> {
    create(pool, "user", name, display_name).await
}

pub async fn create_group(pool: &PgPool, name: &str, display_name: &str) -> Result<Uuid> {
    create(pool, "group", name, display_name).await
}

pub async fn add_member(pool: &PgPool, group: Uuid, member: Uuid) -> Result<()> {
    sqlx::query(
        "INSERT INTO memberships (group_id, member_id)
         SELECT g.id, $2 FROM principals g WHERE g.id = $1 AND g.kind = 'group'
         ON CONFLICT DO NOTHING",
    )
    .bind(group)
    .bind(member)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn remove_member(pool: &PgPool, group: Uuid, member: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM memberships WHERE group_id = $1 AND member_id = $2")
        .bind(group)
        .bind(member)
        .execute(pool)
        .await?;
    Ok(())
}

/// A group's members.
pub async fn members(pool: &PgPool, group: Uuid) -> Result<Vec<Uuid>> {
    Ok(sqlx::query_scalar("SELECT member_id FROM memberships WHERE group_id = $1").bind(group).fetch_all(pool).await?)
}

pub async fn by_name(pool: &PgPool, name: &str) -> Result<Principal> {
    sqlx::query_as("SELECT id, kind, name, display_name, disabled FROM principals WHERE name = $1")
        .bind(name.to_ascii_lowercase())
        .fetch_optional(pool)
        .await?
        .ok_or(DomainError::NotFound)
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<Principal> {
    sqlx::query_as("SELECT id, kind, name, display_name, disabled FROM principals WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or(DomainError::NotFound)
}

pub async fn set_disabled(pool: &PgPool, id: Uuid, disabled: bool) -> Result<()> {
    sqlx::query("UPDATE principals SET disabled = $2 WHERE id = $1").bind(id).bind(disabled).execute(pool).await?;
    Ok(())
}
