//! The phone's store: one SQLite file (decision 44's shape).
//!
//! - `collections`: what the person can see, as the server listed it, and
//!   the change-log state this phone has applied (`NULL`: never synced).
//! - `objects`: the full text as the person sees it (their own reminders
//!   in it), its revision, and a projection (kind, UID, summary, UTC
//!   bounds) for queries. Local edits show here at once.
//! - `pending`: edits not yet on the server, each with the copy it started
//!   from (`base`, `NULL` for new) and the phone's copy (`body`, `NULL` for
//!   deleted). `version` grows with every further edit, so an answer for
//!   an older version never overwrites a newer edit.
//! - `conflicts`: what the person should hear about: a merge that kept the
//!   server's value, an edit the server refused, a collection that went
//!   away with edits in it.
//! - `local_changes`: every change that came from the server, in order,
//!   for the system bridge to mirror into Android (step 5). The bridge
//!   reads past its last position and then forgets what it has applied.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use calzone_core::lines::Document;
use calzone_core::merge;
use calzone_core::object::{self, Bounds};
use calzone_core::sync::{self as wire, Account, Change, Pushed, Rejected};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

const SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE collections (
    id    TEXT PRIMARY KEY,
    info  TEXT NOT NULL,
    state INTEGER
);
CREATE TABLE objects (
    collection TEXT NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    revision   INTEGER NOT NULL,
    body       TEXT NOT NULL,
    kind       TEXT NOT NULL,
    uid        TEXT NOT NULL,
    summary    TEXT,
    starts     INTEGER,
    ends       INTEGER,
    recurring  INTEGER NOT NULL,
    undated    INTEGER NOT NULL,
    PRIMARY KEY (collection, name)
);
CREATE INDEX objects_span ON objects (starts, ends);
CREATE TABLE pending (
    collection TEXT NOT NULL REFERENCES collections (id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    base       TEXT,
    body       TEXT,
    version    INTEGER NOT NULL,
    PRIMARY KEY (collection, name)
);
CREATE TABLE conflicts (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    collection TEXT NOT NULL,
    name       TEXT NOT NULL,
    at         INTEGER NOT NULL,
    detail     TEXT NOT NULL
);
CREATE TABLE local_changes (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    collection TEXT NOT NULL,
    name       TEXT,
    deleted    INTEGER NOT NULL
);
";

/// A collection on this phone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCollection {
    pub info: wire::Collection,
    /// The change-log state applied here; `None`: never synced.
    pub state: Option<i64>,
    /// Edits not yet on the server.
    pub pending: u32,
}

/// One object's projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub collection: String,
    pub name: String,
    pub revision: i64,
    pub kind: String,
    pub uid: String,
    pub summary: Option<String>,
    /// UTC seconds, widened like the server's (decision 9); `None` end is
    /// open-ended.
    pub starts: Option<i64>,
    pub ends: Option<i64>,
    pub recurring: bool,
    pub undated: bool,
    /// An edit to it is waiting for the server.
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredObject {
    pub name: String,
    /// The server revision it is based on; 0 for an object new on the phone.
    pub revision: i64,
    pub body: String,
}

/// Something to tell the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub id: i64,
    pub collection: String,
    pub name: String,
    /// UTC seconds.
    pub at: i64,
    pub detail: ConflictDetail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConflictDetail {
    /// The server merged the edit and kept its own value where both sides
    /// changed the same thing (decision 46).
    Merged { conflicts: Vec<merge::Conflict> },
    /// The server refused the edit; the phone went back to the server's
    /// copy. `body` is the phone's text that was not taken.
    Rejected { reason: wire::Reason, message: String, body: Option<String> },
    /// The collection is gone (deleted, or no longer shared) while this
    /// edit waited; `body` is the phone's text.
    CollectionGone { body: Option<String> },
}

/// A change the bridge must mirror. `name` `None` is the collection itself
/// (its name, color or access changed, or with `deleted` it is gone).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalChange {
    pub seq: i64,
    pub collection: String,
    pub name: Option<String>,
    pub deleted: bool,
}

/// A pending edit as the engine sends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingEdit {
    pub name: String,
    pub base: Option<String>,
    pub body: Option<String>,
    pub version: i64,
}

/// The projection of one body, from the core.
struct Projection {
    kind: String,
    uid: String,
    summary: Option<String>,
    starts: Option<i64>,
    ends: Option<i64>,
    recurring: bool,
    undated: bool,
}

fn project(addressbook: bool, name: &str, body: &str) -> std::result::Result<Projection, String> {
    let doc = Document::parse(body).map_err(|e| e.to_string())?;
    let is_card = doc.components.first().is_some_and(|c| c.name == "VCARD");
    if addressbook != is_card {
        return Err("wrong kind of object for this collection".into());
    }
    if is_card {
        let info = object::inspect_card(&doc).map_err(|e| e.to_string())?;
        return Ok(Projection {
            kind: "VCARD".into(),
            uid: info.uid.unwrap_or_else(|| format!("name:{name}")),
            summary: info.formatted_name,
            starts: None,
            ends: None,
            recurring: false,
            undated: true,
        });
    }
    let info = object::inspect_calendar(&doc, body).map_err(|e| e.to_string())?;
    let (undated, starts, ends) = match info.bounds {
        Bounds::Undated => (true, None, None),
        Bounds::Span { start, end } => (false, Some(start.and_utc().timestamp()), end.map(|e| e.and_utc().timestamp())),
    };
    Ok(Projection {
        kind: info.kind.component_name().into(),
        uid: info.uid,
        summary: info.summary,
        starts,
        ends,
        recurring: info.recurring,
        undated,
    })
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        Self::init(Connection::open(path)?)
    }

    /// For tests: a store that lives as long as this value.
    pub fn in_memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store> {
        conn.pragma_update(None, "foreign_keys", true)?;
        // In-memory databases answer "memory"; that is fine.
        let _: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        match version {
            0 => {
                conn.execute_batch(SCHEMA)?;
                conn.pragma_update(None, "user_version", 1)?;
            }
            1 => {}
            v => return Err(Error::Input(format!("this store was written by a newer CalZone (schema {v})"))),
        }
        Ok(Store { conn: Mutex::new(conn) })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock leaves SQLite consistent (every
        // write is a transaction), so a poisoned lock is still usable.
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    // --- the account ---

    pub fn account(&self) -> Result<Option<Account>> {
        let v: Option<String> =
            self.lock().query_row("SELECT value FROM meta WHERE key = 'account'", [], |r| r.get(0)).optional()?;
        Ok(v.and_then(|v| serde_json::from_str(&v).ok()))
    }

    pub fn set_account(&self, account: &Account) -> Result<()> {
        let json = serde_json::to_string(account).expect("serializable");
        self.lock().execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('account', ?1)", params![json])?;
        Ok(())
    }

    /// The server this store belongs to (`https://calzone.example.com`).
    pub fn server(&self) -> Result<Option<String>> {
        Ok(self.lock().query_row("SELECT value FROM meta WHERE key = 'server'", [], |r| r.get(0)).optional()?)
    }

    pub fn set_server(&self, server: &str) -> Result<()> {
        self.lock().execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('server', ?1)", params![server])?;
        Ok(())
    }

    /// Forget everything: signing out, or signing in as someone else.
    pub fn wipe(&self) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for t in ["pending", "objects", "conflicts", "local_changes", "collections", "meta"] {
            tx.execute(&format!("DELETE FROM {t}"), [])?;
        }
        tx.commit()?;
        Ok(())
    }

    // --- reading ---

    pub fn collections(&self) -> Result<Vec<StoredCollection>> {
        let conn = self.lock();
        let mut st = conn.prepare(
            "SELECT c.info, c.state, (SELECT count(*) FROM pending p WHERE p.collection = c.id)
             FROM collections c ORDER BY c.rowid",
        )?;
        let rows =
            st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, u32>(2)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (info, state, pending) = row?;
            let info = serde_json::from_str(&info).map_err(|e| Error::Input(e.to_string()))?;
            out.push(StoredCollection { info, state, pending });
        }
        Ok(out)
    }

    pub fn object(&self, collection: &str, name: &str) -> Result<Option<StoredObject>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT name, revision, body FROM objects WHERE collection = ?1 AND name = ?2",
                params![collection, name],
                |r| Ok(StoredObject { name: r.get(0)?, revision: r.get(1)?, body: r.get(2)? }),
            )
            .optional()?)
    }

    /// Every object of a collection, by name.
    pub fn objects(&self, collection: &str) -> Result<Vec<ObjectMeta>> {
        self.metas("o.collection = ?1", params![collection])
    }

    /// Calendar objects that may have an occurrence in `[from, to)` (UTC
    /// seconds), across collections: candidates for the core to expand,
    /// never missing one.
    pub fn in_range(&self, from: i64, to: i64) -> Result<Vec<ObjectMeta>> {
        self.metas(
            "o.kind <> 'VCARD' AND (o.undated OR (o.starts < ?2 AND (o.ends IS NULL OR o.ends > ?1 OR (o.ends = o.starts AND o.starts >= ?1))))",
            params![from, to],
        )
    }

    fn metas(&self, filter: &str, p: impl rusqlite::Params) -> Result<Vec<ObjectMeta>> {
        let conn = self.lock();
        let sql = format!(
            "SELECT o.collection, o.name, o.revision, o.kind, o.uid, o.summary, o.starts, o.ends, o.recurring, o.undated,
                    EXISTS (SELECT 1 FROM pending p WHERE p.collection = o.collection AND p.name = o.name)
             FROM objects o WHERE {filter} ORDER BY o.collection, o.name"
        );
        let mut st = conn.prepare(&sql)?;
        let rows = st.query_map(p, |r| {
            Ok(ObjectMeta {
                collection: r.get(0)?,
                name: r.get(1)?,
                revision: r.get(2)?,
                kind: r.get(3)?,
                uid: r.get(4)?,
                summary: r.get(5)?,
                starts: r.get(6)?,
                ends: r.get(7)?,
                recurring: r.get(8)?,
                undated: r.get(9)?,
                pending: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn conflicts(&self) -> Result<Vec<Conflict>> {
        let conn = self.lock();
        let mut st = conn.prepare("SELECT id, collection, name, at, detail FROM conflicts ORDER BY id")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, collection, name, at, detail) = row?;
            if let Ok(detail) = serde_json::from_str(&detail) {
                out.push(Conflict { id, collection, name, at, detail });
            }
        }
        Ok(out)
    }

    /// The person has seen it.
    pub fn dismiss_conflict(&self, id: i64) -> Result<()> {
        self.lock().execute("DELETE FROM conflicts WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Changes from the server after `after`, oldest first, at most `limit`.
    pub fn local_changes(&self, after: i64, limit: u32) -> Result<Vec<LocalChange>> {
        let conn = self.lock();
        let mut st = conn
            .prepare("SELECT seq, collection, name, deleted FROM local_changes WHERE seq > ?1 ORDER BY seq LIMIT ?2")?;
        let rows = st.query_map(params![after, limit], |r| {
            Ok(LocalChange { seq: r.get(0)?, collection: r.get(1)?, name: r.get(2)?, deleted: r.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The bridge has applied everything up to `seq`.
    pub fn forget_local_changes(&self, upto: i64) -> Result<()> {
        self.lock().execute("DELETE FROM local_changes WHERE seq <= ?1", params![upto])?;
        Ok(())
    }

    // --- the person's edits ---

    /// An edit on this phone (the bridge, later the app's own screens):
    /// `body` is the new full text, `None` deletes. It shows in the store
    /// at once and waits as a pending edit. The text must parse and suit
    /// the collection; whether the person may make it is the server's call.
    pub fn edit(&self, collection: &str, name: &str, body: Option<&str>) -> Result<()> {
        if name.is_empty() || name.len() > 255 || name.contains('/') || name.chars().any(char::is_control) {
            return Err(Error::Input(format!("{name:?} cannot name an object")));
        }
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let addressbook = collection_kind(&tx, collection)?;
        let projection = match body {
            Some(b) => Some(project(addressbook, name, b).map_err(Error::Input)?),
            None => None,
        };
        let current: Option<(String, i64)> = tx
            .query_row(
                "SELECT body, revision FROM objects WHERE collection = ?1 AND name = ?2",
                params![collection, name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let pending: Option<(Option<String>, i64)> = tx
            .query_row(
                "SELECT base, version FROM pending WHERE collection = ?1 AND name = ?2",
                params![collection, name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if current.as_ref().map(|c| c.0.as_str()) == body {
            return Ok(());
        }
        match pending {
            // Made here and deleted here before the server heard of it.
            Some((None, _)) if body.is_none() => {
                tx.execute("DELETE FROM pending WHERE collection = ?1 AND name = ?2", params![collection, name])?;
            }
            Some((_, version)) => {
                tx.execute(
                    "UPDATE pending SET body = ?3, version = ?4 WHERE collection = ?1 AND name = ?2",
                    params![collection, name, body, version + 1],
                )?;
            }
            None => {
                tx.execute(
                    "INSERT INTO pending (collection, name, base, body, version) VALUES (?1, ?2, ?3, ?4, 1)",
                    params![collection, name, current.as_ref().map(|c| c.0.as_str()), body],
                )?;
            }
        }
        match (body, projection) {
            (Some(b), Some(p)) => {
                let revision = current.map_or(0, |c| c.1);
                write_object(&tx, collection, name, revision, b, &p)?;
            }
            _ => {
                tx.execute("DELETE FROM objects WHERE collection = ?1 AND name = ?2", params![collection, name])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // --- what the engine does ---

    /// Take the server's collection list. New ones are added (never
    /// synced), changed ones updated, and ones that are gone removed with
    /// their objects; their pending edits become conflicts. Returns how
    /// many went away.
    pub(crate) fn set_collections(&self, list: &[wire::Collection], now: i64) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let known: Vec<(String, String)> = {
            let mut st = tx.prepare("SELECT id, info FROM collections")?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let listed: HashSet<&str> = list.iter().map(|c| c.id.as_str()).collect();
        let mut gone = 0;
        for (id, _) in known.iter().filter(|(id, _)| !listed.contains(id.as_str())) {
            let edits: Vec<(String, Option<String>)> = {
                let mut st = tx.prepare("SELECT name, body FROM pending WHERE collection = ?1")?;
                st.query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
            };
            for (name, body) in edits {
                add_conflict(&tx, id, &name, now, &ConflictDetail::CollectionGone { body })?;
            }
            tx.execute("DELETE FROM collections WHERE id = ?1", params![id])?;
            tx.execute("DELETE FROM local_changes WHERE collection = ?1", params![id])?;
            local_change(&tx, id, None, true)?;
            gone += 1;
        }
        for c in list {
            let info = serde_json::to_string(c).expect("serializable");
            // The state is not the list's to change: it is what this phone
            // has applied.
            let before = known.iter().find(|(id, _)| *id == c.id).map(|(_, i)| i.as_str());
            if before == Some(info.as_str()) {
                continue;
            }
            tx.execute(
                "INSERT INTO collections (id, info, state) VALUES (?1, ?2, NULL)
                 ON CONFLICT (id) DO UPDATE SET info = excluded.info",
                params![c.id, info],
            )?;
            // Only the state moving is not news for the bridge.
            let same_but_state = before
                .and_then(|b| serde_json::from_str::<wire::Collection>(b).ok())
                .is_some_and(|b| wire::Collection { state: c.state, ..b } == *c);
            if !same_but_state {
                local_change(&tx, &c.id, None, false)?;
            }
        }
        tx.commit()?;
        Ok(gone)
    }

    pub(crate) fn state(&self, collection: &str) -> Result<Option<i64>> {
        Ok(self
            .lock()
            .query_row("SELECT state FROM collections WHERE id = ?1", params![collection], |r| r.get(0))
            .optional()?
            .flatten())
    }

    /// Start over: the next pull is a full sync.
    pub(crate) fn reset_state(&self, collection: &str) -> Result<()> {
        self.lock().execute("UPDATE collections SET state = NULL WHERE id = ?1", params![collection])?;
        Ok(())
    }

    /// Collections with edits waiting.
    pub(crate) fn pending_collections(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut st = conn.prepare("SELECT DISTINCT collection FROM pending ORDER BY collection")?;
        Ok(st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
    }

    pub(crate) fn pending(&self, collection: &str) -> Result<Vec<PendingEdit>> {
        let conn = self.lock();
        let mut st =
            conn.prepare("SELECT name, base, body, version FROM pending WHERE collection = ?1 ORDER BY name")?;
        let rows = st.query_map(params![collection], |r| {
            Ok(PendingEdit { name: r.get(0)?, base: r.get(1)?, body: r.get(2)?, version: r.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Apply the server's answer to edits sent at `sent` versions. An edit
    /// that is still the one sent is done: the merged copy replaces it. An
    /// edit the person changed again meanwhile is rebased onto the merged
    /// copy with the core's merge and waits for the next run. A refused
    /// edit goes back to the server's copy it started from.
    pub(crate) fn apply_answer(
        &self,
        collection: &str,
        sent: &[PendingEdit],
        answer: &wire::Answer,
        now: i64,
    ) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let addressbook = collection_kind(&tx, collection)?;
        for p in &answer.pushed {
            let Some(was) = sent.iter().find(|s| s.name == p.name) else { continue };
            apply_pushed(&tx, addressbook, collection, was, p)?;
            if !p.conflicts.is_empty() {
                add_conflict(
                    &tx,
                    collection,
                    &p.name,
                    now,
                    &ConflictDetail::Merged { conflicts: p.conflicts.clone() },
                )?;
            }
        }
        for r in &answer.rejected {
            let Some(was) = sent.iter().find(|s| s.name == r.name) else { continue };
            apply_rejected(&tx, addressbook, collection, was, r, now)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Apply one batch of the server's changes. An object with an edit
    /// waiting is left alone: the edit goes up next run and the server
    /// merges it. With `seen`, this is part of a full sync: the names are
    /// collected for [`Store::finish_full_sync`] and the state is not moved
    /// until it is done. Otherwise the batch's state is stored with it.
    pub(crate) fn apply_batch(&self, batch: &wire::Batch, seen: Option<&mut HashSet<String>>) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let addressbook = collection_kind(&tx, &batch.collection)?;
        let mut applied = 0;
        let full = seen.is_some();
        for c in &batch.changes {
            if pending_version(&tx, &batch.collection, &c.name)?.is_some() {
                continue;
            }
            applied += apply_change(&tx, addressbook, &batch.collection, c)? as usize;
        }
        if let Some(seen) = seen {
            seen.extend(batch.changes.iter().filter(|c| c.body.is_some()).map(|c| c.name.clone()));
        }
        if !full {
            tx.execute("UPDATE collections SET state = ?2 WHERE id = ?1", params![batch.collection, batch.state.0])?;
        }
        tx.commit()?;
        Ok(applied)
    }

    /// The end of a full sync: whatever the server did not list is gone
    /// (unless an edit to it is waiting), and the state is stored.
    pub(crate) fn finish_full_sync(&self, collection: &str, seen: &HashSet<String>, state: i64) -> Result<usize> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let names: Vec<String> = {
            let mut st = tx.prepare(
                "SELECT o.name FROM objects o WHERE o.collection = ?1
                   AND NOT EXISTS (SELECT 1 FROM pending p WHERE p.collection = o.collection AND p.name = o.name)",
            )?;
            st.query_map(params![collection], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let mut removed = 0;
        for name in names.iter().filter(|n| !seen.contains(*n)) {
            tx.execute("DELETE FROM objects WHERE collection = ?1 AND name = ?2", params![collection, name])?;
            local_change(&tx, collection, Some(name), true)?;
            removed += 1;
        }
        tx.execute("UPDATE collections SET state = ?2 WHERE id = ?1", params![collection, state])?;
        tx.commit()?;
        Ok(removed)
    }
}

fn collection_kind(tx: &Transaction<'_>, collection: &str) -> Result<bool> {
    let info: Option<String> =
        tx.query_row("SELECT info FROM collections WHERE id = ?1", params![collection], |r| r.get(0)).optional()?;
    let info = info.ok_or_else(|| Error::Input(format!("no collection {collection} on this phone")))?;
    let c: wire::Collection = serde_json::from_str(&info).map_err(|e| Error::Input(e.to_string()))?;
    Ok(c.kind == "addressbook")
}

fn pending_version(tx: &Transaction<'_>, collection: &str, name: &str) -> Result<Option<i64>> {
    Ok(tx
        .query_row("SELECT version FROM pending WHERE collection = ?1 AND name = ?2", params![collection, name], |r| {
            r.get(0)
        })
        .optional()?)
}

fn write_object(
    tx: &Transaction<'_>,
    collection: &str,
    name: &str,
    revision: i64,
    body: &str,
    p: &Projection,
) -> Result<()> {
    tx.execute(
        "INSERT INTO objects (collection, name, revision, body, kind, uid, summary, starts, ends, recurring, undated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (collection, name) DO UPDATE SET
            revision = excluded.revision, body = excluded.body, kind = excluded.kind, uid = excluded.uid,
            summary = excluded.summary, starts = excluded.starts, ends = excluded.ends,
            recurring = excluded.recurring, undated = excluded.undated",
        params![collection, name, revision, body, p.kind, p.uid, p.summary, p.starts, p.ends, p.recurring, p.undated],
    )?;
    Ok(())
}

/// A copy from the server: stored with its projection. The server only
/// stores what parses; should one ever not, it is kept as an undated
/// object rather than dropped (decision 5).
fn store_server_copy(
    tx: &Transaction<'_>,
    addressbook: bool,
    collection: &str,
    name: &str,
    revision: i64,
    body: &str,
) -> Result<()> {
    let p = project(addressbook, name, body).unwrap_or(Projection {
        kind: if addressbook { "VCARD".into() } else { "VEVENT".into() },
        uid: format!("name:{name}"),
        summary: None,
        starts: None,
        ends: None,
        recurring: false,
        undated: true,
    });
    write_object(tx, collection, name, revision, body, &p)
}

/// Returns whether anything changed.
fn apply_change(tx: &Transaction<'_>, addressbook: bool, collection: &str, c: &Change) -> Result<bool> {
    let current: Option<(i64, String)> = tx
        .query_row(
            "SELECT revision, body FROM objects WHERE collection = ?1 AND name = ?2",
            params![collection, c.name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match &c.body {
        Some(body) => {
            if current.as_ref().is_some_and(|(rev, b)| *rev == c.revision.0 && b == body) {
                return Ok(false);
            }
            store_server_copy(tx, addressbook, collection, &c.name, c.revision.0, body)?;
            local_change(tx, collection, Some(&c.name), false)?;
        }
        None => {
            if current.is_none() {
                return Ok(false);
            }
            tx.execute("DELETE FROM objects WHERE collection = ?1 AND name = ?2", params![collection, c.name])?;
            local_change(tx, collection, Some(&c.name), true)?;
        }
    }
    Ok(true)
}

fn apply_pushed(
    tx: &Transaction<'_>,
    addressbook: bool,
    collection: &str,
    was: &PendingEdit,
    p: &Pushed,
) -> Result<()> {
    let now = pending_version(tx, collection, &p.name)?;
    match now {
        Some(v) if v == was.version => {
            tx.execute("DELETE FROM pending WHERE collection = ?1 AND name = ?2", params![collection, p.name])?;
            match &p.body {
                Some(b) => store_server_copy(tx, addressbook, collection, &p.name, p.revision.0, b)?,
                None => {
                    tx.execute("DELETE FROM objects WHERE collection = ?1 AND name = ?2", params![collection, p.name])?;
                }
            }
            // The merged copy may differ from what the bridge wrote.
            local_change(tx, collection, Some(&p.name), p.body.is_none())?;
        }
        Some(_) => {
            // Edited again while the answer was on its way: rebase the
            // newer edit onto the merged copy.
            let newer: Option<String> = tx.query_row(
                "SELECT body FROM pending WHERE collection = ?1 AND name = ?2",
                params![collection, p.name],
                |r| r.get(0),
            )?;
            let rebased =
                merge::merge(was.body.as_deref(), newer.as_deref(), p.body.as_deref()).map(|m| m.text).unwrap_or(newer);
            tx.execute(
                "UPDATE pending SET base = ?3, body = ?4 WHERE collection = ?1 AND name = ?2",
                params![collection, p.name, p.body, rebased],
            )?;
            match &rebased {
                Some(b) => {
                    let proj = project(addressbook, &p.name, b).map_err(Error::Input)?;
                    write_object(tx, collection, &p.name, p.revision.0, b, &proj)?;
                }
                None => {
                    tx.execute("DELETE FROM objects WHERE collection = ?1 AND name = ?2", params![collection, p.name])?;
                }
            }
            local_change(tx, collection, Some(&p.name), rebased.is_none())?;
        }
        // Gone meanwhile (the collection was reset); nothing to do.
        None => {}
    }
    Ok(())
}

fn apply_rejected(
    tx: &Transaction<'_>,
    addressbook: bool,
    collection: &str,
    was: &PendingEdit,
    r: &Rejected,
    now: i64,
) -> Result<()> {
    let detail = ConflictDetail::Rejected { reason: r.reason, message: r.message.clone(), body: was.body.clone() };
    add_conflict(tx, collection, &r.name, now, &detail)?;
    if pending_version(tx, collection, &r.name)? != Some(was.version) {
        // Edited again meanwhile: that edit gets its own answer.
        return Ok(());
    }
    tx.execute("DELETE FROM pending WHERE collection = ?1 AND name = ?2", params![collection, r.name])?;
    // Back to the server's copy the edit started from.
    match &was.base {
        Some(base) => {
            let revision: i64 = tx
                .query_row(
                    "SELECT revision FROM objects WHERE collection = ?1 AND name = ?2",
                    params![collection, r.name],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            store_server_copy(tx, addressbook, collection, &r.name, revision, base)?;
            local_change(tx, collection, Some(&r.name), false)?;
        }
        None => {
            tx.execute("DELETE FROM objects WHERE collection = ?1 AND name = ?2", params![collection, r.name])?;
            local_change(tx, collection, Some(&r.name), true)?;
        }
    }
    Ok(())
}

fn add_conflict(tx: &Transaction<'_>, collection: &str, name: &str, at: i64, d: &ConflictDetail) -> Result<()> {
    tx.execute(
        "INSERT INTO conflicts (collection, name, at, detail) VALUES (?1, ?2, ?3, ?4)",
        params![collection, name, at, serde_json::to_string(d).expect("serializable")],
    )?;
    Ok(())
}

fn local_change(tx: &Transaction<'_>, collection: &str, name: Option<&str>, deleted: bool) -> Result<()> {
    tx.execute(
        "INSERT INTO local_changes (collection, name, deleted) VALUES (?1, ?2, ?3)",
        params![collection, name, deleted],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use calzone_core::sync::{Answer, Batch, Reason, State};

    use super::*;

    fn collection(id: &str, kind: &str, state: i64) -> wire::Collection {
        wire::Collection {
            id: id.into(),
            kind: kind.into(),
            owner: "anna".into(),
            owner_name: "Anna".into(),
            name: id.into(),
            description: String::new(),
            color: None,
            components: Vec::new(),
            access: wire::Access::Owner,
            read_only: false,
            subscribed: false,
            state: State(state),
            mine: wire::Mine::default(),
        }
    }

    fn event(uid: &str, summary: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART:20260310T090000Z\r\n\
             DTEND:20260310T100000Z\r\nSUMMARY:{summary}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    fn batch(changes: Vec<Change>, state: i64) -> Batch {
        Batch { collection: "cal".into(), since: State(0), state: State(state), changes, more: false }
    }

    fn change(name: &str, rev: i64, body: Option<String>) -> Change {
        Change { name: name.into(), revision: State(rev), body }
    }

    fn store() -> Store {
        let s = Store::in_memory().unwrap();
        s.set_collections(&[collection("cal", "calendar", 2)], 0).unwrap();
        s.apply_batch(
            &batch(vec![change("a.ics", 1, Some(event("a", "A"))), change("b.ics", 2, Some(event("b", "B")))], 2),
            None,
        )
        .unwrap();
        s
    }

    #[test]
    fn server_copies_land_with_their_projection() {
        let s = store();
        assert_eq!(s.state("cal").unwrap(), Some(2));
        let metas = s.objects("cal").unwrap();
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].summary.as_deref(), Some("A"));
        assert_eq!(metas[0].starts, Some(1_773_133_200));
        assert_eq!(s.in_range(1_773_100_000, 1_773_200_000).unwrap().len(), 2);
        assert!(s.in_range(0, 1_000).unwrap().is_empty());
        let log: Vec<_> = s.local_changes(0, 100).unwrap().into_iter().map(|c| (c.name, c.deleted)).collect();
        assert_eq!(log, [(None, false), (Some("a.ics".into()), false), (Some("b.ics".into()), false)]);
        // The same batch again changes nothing.
        let again = s.apply_batch(&batch(vec![change("a.ics", 1, Some(event("a", "A")))], 2), None).unwrap();
        assert_eq!(again, 0);
    }

    #[test]
    fn edits_wait_with_the_copy_they_started_from() {
        let s = store();
        s.edit("cal", "a.ics", Some(&event("a", "A2"))).unwrap();
        s.edit("cal", "a.ics", Some(&event("a", "A3"))).unwrap();
        s.edit("cal", "new.ics", Some(&event("new", "N"))).unwrap();
        s.edit("cal", "b.ics", None).unwrap();
        let p = s.pending("cal").unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(
            (p[0].base.as_deref(), p[0].body.as_deref(), p[0].version),
            (Some(event("a", "A").as_str()), Some(event("a", "A3").as_str()), 2)
        );
        assert_eq!((p[1].base.as_deref(), p[1].body.as_deref()), (Some(event("b", "B").as_str()), None));
        assert_eq!(p[2].base, None);
        assert_eq!(s.object("cal", "a.ics").unwrap().unwrap().body, event("a", "A3"), "shows at once");
        assert!(s.object("cal", "b.ics").unwrap().is_none());
        // Made and deleted here: never mind.
        s.edit("cal", "new.ics", None).unwrap();
        assert_eq!(s.pending("cal").unwrap().len(), 2);
        assert!(matches!(s.edit("cal", "x.ics", Some("not a calendar")), Err(Error::Input(_))));
        assert!(matches!(s.edit("cal", "x.vcf", Some("BEGIN:VCARD\r\nFN:x\r\nEND:VCARD\r\n")), Err(Error::Input(_))));
        assert!(matches!(s.edit("nope", "x.ics", None), Err(Error::Input(_))));
        // A server change to an object with an edit waiting is left alone.
        s.apply_batch(&batch(vec![change("a.ics", 5, Some(event("a", "server")))], 5), None).unwrap();
        assert_eq!(s.object("cal", "a.ics").unwrap().unwrap().body, event("a", "A3"));
    }

    #[test]
    fn answers_replace_rebase_or_restore() {
        let s = store();
        s.edit("cal", "a.ics", Some(&event("a", "A2"))).unwrap();
        s.edit("cal", "b.ics", Some(&event("b", "B2"))).unwrap();
        let sent = s.pending("cal").unwrap();
        // While the upload is on its way, a.ics is edited again.
        s.edit("cal", "a.ics", Some(&event("a", "A2").replace("END:VEVENT", "LOCATION:Here\r\nEND:VEVENT"))).unwrap();
        let merged_a = event("a", "A2").replace("END:VEVENT", "DESCRIPTION:from the web\r\nEND:VEVENT");
        let answer = Answer {
            pushed: vec![Pushed {
                name: "a.ics".into(),
                revision: State(7),
                body: Some(merged_a.clone()),
                conflicts: Vec::new(),
            }],
            rejected: vec![Rejected { name: "b.ics".into(), reason: Reason::ReadOnly, message: "read-only".into() }],
        };
        s.apply_answer("cal", &sent, &answer, 100).unwrap();
        // a: the newer edit rebased onto the merged copy, still waiting.
        let p = s.pending("cal").unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].base.as_deref(), Some(merged_a.as_str()));
        let body = p[0].body.clone().unwrap();
        assert!(body.contains("LOCATION:Here") && body.contains("DESCRIPTION:from the web"), "{body}");
        assert_eq!(s.object("cal", "a.ics").unwrap().unwrap().revision, 7);
        // b: refused, back to the server's copy, reported.
        assert_eq!(s.object("cal", "b.ics").unwrap().unwrap().body, event("b", "B"));
        let c = s.conflicts().unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].at, 100);
        assert!(
            matches!(&c[0].detail, ConflictDetail::Rejected { reason: Reason::ReadOnly, body: Some(b), .. } if b.contains("B2"))
        );
        s.dismiss_conflict(c[0].id).unwrap();
        assert!(s.conflicts().unwrap().is_empty());
        // The second answer for a: done.
        let sent = s.pending("cal").unwrap();
        let answer = Answer {
            pushed: vec![Pushed {
                name: "a.ics".into(),
                revision: State(8),
                body: Some(body.clone()),
                conflicts: Vec::new(),
            }],
            rejected: Vec::new(),
        };
        s.apply_answer("cal", &sent, &answer, 101).unwrap();
        assert!(s.pending("cal").unwrap().is_empty());
        assert_eq!(s.object("cal", "a.ics").unwrap().unwrap().revision, 8);
    }

    #[test]
    fn a_full_sync_removes_what_the_server_no_longer_has() {
        let s = store();
        s.edit("cal", "b.ics", Some(&event("b", "B2"))).unwrap();
        s.reset_state("cal").unwrap();
        let mut seen = HashSet::new();
        s.apply_batch(&batch(vec![change("c.ics", 9, Some(event("c", "C")))], 9), Some(&mut seen)).unwrap();
        assert_eq!(s.state("cal").unwrap(), None, "not stored until the full sync is done");
        assert_eq!(s.finish_full_sync("cal", &seen, 9).unwrap(), 1);
        let names: Vec<_> = s.objects("cal").unwrap().into_iter().map(|o| o.name).collect();
        assert_eq!(names, ["b.ics", "c.ics"], "a.ics went; b.ics has an edit waiting");
        assert_eq!(s.state("cal").unwrap(), Some(9));
    }

    #[test]
    fn a_collection_that_goes_takes_its_edits_into_conflicts() {
        let s = store();
        s.edit("cal", "a.ics", Some(&event("a", "A2"))).unwrap();
        let after = s.local_changes(0, 100).unwrap().last().unwrap().seq;
        assert_eq!(s.set_collections(&[collection("book", "addressbook", 0)], 50).unwrap(), 1);
        assert!(s.objects("cal").unwrap().is_empty() && s.pending_collections().unwrap().is_empty());
        assert!(matches!(&s.conflicts().unwrap()[0].detail, ConflictDetail::CollectionGone { body: Some(_) }));
        let log = s.local_changes(after, 100).unwrap();
        assert!(log.iter().any(|c| c.collection == "cal" && c.name.is_none() && c.deleted));
        assert!(log.iter().any(|c| c.collection == "book" && c.name.is_none() && !c.deleted));
        // Only the state moving is not news.
        s.forget_local_changes(i64::MAX).unwrap();
        s.set_collections(&[collection("book", "addressbook", 3)], 51).unwrap();
        assert!(s.local_changes(0, 100).unwrap().is_empty());
        s.wipe().unwrap();
        assert!(s.collections().unwrap().is_empty());
    }
}
