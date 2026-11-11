//! One sync run against `/sync/v1` (decision 47):
//!
//! 0. Send the person's own views of collections changed on this phone
//!    (name, color, default reminder), so the list below already has them.
//! 1. List the collections (new ones appear, gone ones go, with any edits
//!    waiting in them reported).
//! 2. Upload every pending edit, per collection, in chunks; apply the
//!    server's merged copies, keep its conflicts to show.
//! 3. Pull every collection whose state moved: changes since the state
//!    this phone applied, batch by batch; a full sync the first time, or
//!    when the server says the state is unknown.
//!
//! Edits go up before changes come down, so the server merges them
//! against its newest copy. Any failure stops the run; what was applied so
//! far stays (each batch is one transaction with its state), and pending
//! edits stay until the server has answered for them. Android may start a
//! run twice in a row (decision 45); a second run with nothing to do sends
//! one request.

use std::collections::HashSet;

use calzone_core::sync::{self as wire, Account, Answer, Batch, Collections, Push, SignedIn, Upload};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::store::{PendingEdit, Store};
use crate::transport::{Method, Request, Response, Transport};
use crate::{Error, Result};

/// Edits per upload; the server takes up to 500.
const UPLOAD_EDITS: usize = 200;
/// About this much text per upload (contact photos are inline).
const UPLOAD_BYTES: usize = 4 * 1024 * 1024;
/// Changes per batch asked for.
const BATCH_LIMIT: usize = 500;

/// What one run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub collections: usize,
    /// Collections that went away (deleted, or no longer shared).
    pub collections_gone: usize,
    /// Edits the server took, and edits it refused.
    pub pushed: usize,
    pub rejected: usize,
    /// Objects changed or deleted on this phone by the server's changes.
    pub pulled: usize,
    /// New things to tell the person (see `Store::conflicts`).
    pub conflicts: usize,
}

#[derive(Deserialize)]
struct ServerError {
    #[serde(default)]
    error: String,
    #[serde(default)]
    code: String,
}

fn send(t: &dyn Transport, method: Method, path: String, body: Option<String>) -> Result<Response> {
    let r = t.send(&Request { method, path, body }).map_err(Error::Network)?;
    if r.status == 401 {
        return Err(Error::SignedOut);
    }
    Ok(r)
}

fn refused(r: Response) -> Error {
    let e: ServerError =
        serde_json::from_str(&r.body).unwrap_or(ServerError { error: String::new(), code: String::new() });
    let message = if e.error.is_empty() { format!("HTTP {}", r.status) } else { e.error };
    Error::Server { status: r.status, code: e.code, message }
}

fn decode<T: DeserializeOwned>(r: Response) -> Result<T> {
    if !(200..300).contains(&r.status) {
        return Err(refused(r));
    }
    serde_json::from_str(&r.body).map_err(|e| Error::Server {
        status: r.status,
        code: String::new(),
        message: format!("an answer this app does not understand: {e}"),
    })
}

fn no_content(r: Response) -> Result<()> {
    if (200..300).contains(&r.status) { Ok(()) } else { Err(refused(r)) }
}

/// Redeem a sign-in code for `server` (both from [`crate::link::parse`]).
/// The transport sends no token yet. The token comes back for the app to
/// keep (Android's account manager). Signing in again to the same account
/// on the same server keeps the store, edits waiting included (a phone
/// removed on the Devices page by mistake loses nothing); anyone else
/// starts from an empty store.
pub fn sign_in(store: &Store, t: &dyn Transport, server: &str, code: &str, device: &str) -> Result<SignedIn> {
    let body = serde_json::json!({ "code": code, "device": device }).to_string();
    let r = send(t, Method::Post, "/sync/v1/link".into(), Some(body))?;
    let signed_in: SignedIn = decode(r)?;
    let same = store.server()?.as_deref() == Some(server)
        && store.account()?.is_some_and(|a| a.name == signed_in.account.name);
    if !same {
        store.wipe()?;
    }
    store.set_server(server)?;
    store.set_account(&signed_in.account)?;
    Ok(signed_in)
}

/// The account as the server sees it now (push on or off, the names);
/// stored for the settings screen.
pub fn refresh_account(store: &Store, t: &dyn Transport) -> Result<Account> {
    let a: Account = decode(send(t, Method::Get, "/sync/v1/me".into(), None)?)?;
    store.set_account(&a)?;
    Ok(a)
}

/// Sign this phone out: the server forgets its token (best effort, so a
/// phone without network can still sign out) and the store is wiped.
/// Edits not yet on the server are lost; the app warns first.
pub fn sign_out(store: &Store, t: &dyn Transport) -> Result<()> {
    let _ = t.send(&Request { method: Method::Delete, path: "/sync/v1/me".into(), body: None });
    store.wipe()
}

/// Register this phone's UnifiedPush endpoint and WebPush keys.
pub fn register_push(t: &dyn Transport, endpoint: &str, key: &str, auth: &str) -> Result<()> {
    let body = serde_json::json!({ "endpoint": endpoint, "key": key, "auth": auth }).to_string();
    no_content(send(t, Method::Put, "/sync/v1/push".into(), Some(body))?)
}

pub fn unregister_push(t: &dyn Transport) -> Result<()> {
    no_content(send(t, Method::Delete, "/sync/v1/push".into(), None)?)
}

/// One sync run. `now` (UTC seconds) dates the conflicts it records.
pub fn sync(store: &Store, t: &dyn Transport, now: i64) -> Result<Report> {
    let before = store.conflicts()?.len();
    let mut report = Report::default();

    for (collection, mine) in store.pending_mine()? {
        let body = serde_json::to_string(&mine).expect("serializable");
        let r = send(t, Method::Put, format!("/sync/v1/collections/{collection}/mine"), Some(body))?;
        match r.status {
            200..=299 => store.mine_sent(&collection, &mine, true)?,
            // Gone, or a value the server refuses: back to the server's.
            400 | 404 => store.mine_sent(&collection, &mine, false)?,
            _ => return Err(refused(r)),
        }
    }

    let list: Collections = decode(send(t, Method::Get, "/sync/v1/collections".into(), None)?)?;
    report.collections = list.collections.len();
    report.collections_gone = store.set_collections(&list.collections, now)?;

    for collection in store.pending_collections()? {
        upload(store, t, &collection, now, &mut report)?;
    }

    for c in &list.collections {
        if store.state(&c.id)? == Some(c.state.0) {
            continue;
        }
        report.pulled += pull(store, t, &c.id)?;
    }

    report.conflicts = store.conflicts()?.len().saturating_sub(before);
    Ok(report)
}

/// Chunks of pending edits: at most [`UPLOAD_EDITS`] and about
/// [`UPLOAD_BYTES`] each, at least one edit.
fn chunks(edits: Vec<PendingEdit>) -> Vec<Vec<PendingEdit>> {
    let mut out: Vec<Vec<PendingEdit>> = Vec::new();
    let mut bytes = 0;
    for e in edits {
        let size = e.base.as_ref().map_or(0, String::len) + e.body.as_ref().map_or(0, String::len);
        let start_new = match out.last() {
            None => true,
            Some(last) => last.len() >= UPLOAD_EDITS || (bytes + size > UPLOAD_BYTES && !last.is_empty()),
        };
        if start_new {
            out.push(Vec::new());
            bytes = 0;
        }
        bytes += size;
        out.last_mut().expect("just pushed").push(e);
    }
    out
}

fn upload(store: &Store, t: &dyn Transport, collection: &str, now: i64, report: &mut Report) -> Result<()> {
    for chunk in chunks(store.pending(collection)?) {
        let up = Upload {
            changes: chunk
                .iter()
                .map(|e| Push { name: e.name.clone(), base: e.base.clone(), body: e.body.clone() })
                .collect(),
        };
        let path = format!("/sync/v1/collections/{collection}/changes");
        let r = send(t, Method::Post, path, Some(serde_json::to_string(&up).expect("serializable")))?;
        if r.status == 404 {
            // Gone between the list and now: the next run's list removes it
            // and reports the edits.
            return Ok(());
        }
        let answer: Answer = decode(r)?;
        report.pushed += answer.pushed.len();
        report.rejected += answer.rejected.len();
        store.apply_answer(collection, &chunk, &answer, now)?;
    }
    Ok(())
}

/// Pull one collection; returns how many objects changed here.
fn pull(store: &Store, t: &dyn Transport, collection: &str) -> Result<usize> {
    let mut changed = 0;
    let mut since = store.state(collection)?;
    // A full sync collects every name it saw, so what it did not see can
    // go at the end; its state is stored only then.
    let mut seen: Option<HashSet<String>> = since.is_none().then(HashSet::new);
    loop {
        let path = match since {
            Some(s) => format!("/sync/v1/collections/{collection}/changes?since={s}&limit={BATCH_LIMIT}"),
            None => format!("/sync/v1/collections/{collection}/changes?limit={BATCH_LIMIT}"),
        };
        let r = send(t, Method::Get, path, None)?;
        match r.status {
            404 => return Ok(changed),
            409 => {
                // A state the server never issued (a restored backup): start over.
                if seen.is_some() {
                    return Err(refused(r));
                }
                store.reset_state(collection)?;
                since = None;
                seen = Some(HashSet::new());
                continue;
            }
            _ => {}
        }
        let batch: Batch = decode(r)?;
        if batch.collection != collection {
            return Err(Error::Server {
                status: 200,
                code: String::new(),
                message: "a batch for another collection".into(),
            });
        }
        changed += store.apply_batch(&batch, seen.as_mut())?;
        if !batch.more {
            if let Some(seen) = &seen {
                changed += store.finish_full_sync(collection, seen, batch.state.0)?;
            }
            return Ok(changed);
        }
        since = Some(batch.state.0);
    }
}

/// The state a collection is at on this phone (for tests and settings).
pub fn state(store: &Store, collection: &str) -> Result<Option<wire::State>> {
    Ok(store.state(collection)?.map(wire::State))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploads_are_chunked() {
        let edit = |n: usize, size: usize| PendingEdit {
            name: format!("{n}.ics"),
            base: None,
            body: Some("x".repeat(size)),
            version: 1,
        };
        assert_eq!(
            chunks((0..450).map(|n| edit(n, 10)).collect()).iter().map(Vec::len).collect::<Vec<_>>(),
            [200, 200, 50]
        );
        let big = chunks(vec![edit(0, 3_000_000), edit(1, 3_000_000), edit(2, 10), edit(3, 5_000_000)]);
        assert_eq!(big.iter().map(Vec::len).collect::<Vec<_>>(), [1, 2, 1]);
        assert!(chunks(Vec::new()).is_empty());
    }
}
