//! Who may do what with a collection (decision 7): the owner, then the best
//! grant to the actor directly or to a group the actor belongs to. A
//! disabled actor reaches nothing.

use sqlx::PgExecutor;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    Read,
    Write,
    Admin,
    Owner,
}

impl Access {
    pub fn can_write(self) -> bool {
        self >= Access::Write
    }

    /// Rename, recolor, share or delete the collection.
    pub fn can_admin(self) -> bool {
        self >= Access::Admin
    }

    pub fn from_level(level: &str) -> Option<Self> {
        match level {
            "read" => Some(Access::Read),
            "write" => Some(Access::Write),
            "admin" => Some(Access::Admin),
            _ => None,
        }
    }
}

/// The SQL expression for an actor's access to collection `c`: 4 owner,
/// 3 admin, 2 write, 1 read, NULL none. `$actor` is bound by the caller.
pub(crate) fn level_sql(actor_param: &str) -> String {
    format!(
        "CASE WHEN EXISTS (SELECT 1 FROM principals p WHERE p.id = {a} AND p.disabled) THEN NULL
              WHEN c.owner_id = {a} THEN 4
              ELSE (SELECT max(CASE g.level WHEN 'read' THEN 1 WHEN 'write' THEN 2 WHEN 'admin' THEN 3 END)
                    FROM grants g
                    WHERE g.collection_id = c.id
                      AND (g.principal_id = {a}
                           OR g.principal_id IN (SELECT m.group_id FROM memberships m WHERE m.member_id = {a})))
         END",
        a = actor_param
    )
}

pub(crate) fn from_number(n: Option<i32>) -> Option<Access> {
    match n? {
        4 => Some(Access::Owner),
        3 => Some(Access::Admin),
        2 => Some(Access::Write),
        1 => Some(Access::Read),
        _ => None,
    }
}

/// The actor's access to one collection, or None.
pub async fn of(db: impl PgExecutor<'_>, actor: Uuid, collection: Uuid) -> sqlx::Result<Option<Access>> {
    let sql = format!("SELECT ({}) FROM collections c WHERE c.id = $2", level_sql("$1"));
    let n: Option<Option<i32>> = sqlx::query_scalar(&sql).bind(actor).bind(collection).fetch_optional(db).await?;
    Ok(n.and_then(from_number))
}
