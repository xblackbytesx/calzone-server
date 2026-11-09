//! The sync API door, `/sync/v1` (decisions 15 and 47): JSON for the
//! CalZone app. A phone signs in once with a code from the Devices page
//! and then sends `Authorization: Bearer czd_...`. Mounted whatever
//! `CALZONE_DAV` says: the app never needs the DAV door.
//!
//! - `POST /sync/v1/link`: redeem a sign-in code (no token yet).
//! - `GET /sync/v1/me`, `DELETE /sync/v1/me`: the account; sign this phone out.
//! - `PUT /sync/v1/push`, `DELETE /sync/v1/push`: this phone's UnifiedPush endpoint.
//! - `GET /sync/v1/collections`: what the person can see, with their view.
//! - `GET /sync/v1/collections/{id}/changes?since=N&limit=M`: a batch.
//! - `POST /sync/v1/collections/{id}/changes`: the phone's edits, merged.
//!
//! Errors are `{"error": "...", "code": "..."}`; the app branches on the
//! code only.

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, Query, State};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use calzone_core::sync::{Account, Answer, Batch, Collections, SignedIn, Upload};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::domain::{self, DomainError, accounts, devices};
use crate::push::{self, EndpointError};
use crate::webpush::DeviceKeys;

/// At most this many edits in one upload; the phone sends more in turns.
pub const MAX_UPLOAD: usize = 500;
/// Uploads carry whole objects, contact photos included.
const BODY_LIMIT: usize = 32 * 1024 * 1024;
/// Changes per batch unless the phone asks for fewer (or more, up to 1000).
const DEFAULT_LIMIT: usize = 200;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/sync/v1/link", post(claim))
        .route("/sync/v1/me", get(me).delete(sign_out))
        .route("/sync/v1/push", put(register_push).delete(unregister_push))
        .route("/sync/v1/collections", get(collections))
        .route("/sync/v1/collections/{id}/changes", get(changes).post(upload))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

#[derive(Debug)]
pub struct SyncError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl SyncError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        SyncError { status, code, message: message.into() }
    }

    fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "sign in again: this phone's token is not valid")
    }
}

impl From<DomainError> for SyncError {
    fn from(e: DomainError) -> Self {
        match e {
            // Unseen and missing look the same (AGENTS.md invariants).
            DomainError::NotFound => Self::new(StatusCode::NOT_FOUND, "not_found", "not found"),
            DomainError::BadSyncToken => {
                Self::new(StatusCode::CONFLICT, "bad_state", "this state was never issued here: sync from the start")
            }
            DomainError::Db(e) => {
                tracing::error!(error = %e, "sync: database error");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "server", "something went wrong on the server")
            }
            other => Self::bad(other.to_string()),
        }
    }
}

impl From<JsonRejection> for SyncError {
    fn from(e: JsonRejection) -> Self {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            return Self::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "send fewer changes at a time");
        }
        Self::bad(e.body_text())
    }
}

impl IntoResponse for SyncError {
    fn into_response(self) -> Response {
        let mut r = (self.status, Json(json!({ "error": self.message, "code": self.code }))).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            r.headers_mut().insert("www-authenticate", HeaderValue::from_static("Bearer"));
        }
        r
    }
}

type Reply<T> = Result<Json<T>, SyncError>;

/// A signed-in phone: a device token whose person is enabled.
pub struct Phone(pub devices::SignedIn);

impl Phone {
    fn actor(&self) -> Uuid {
        self.0.principal
    }
}

impl FromRequestParts<AppState> for Phone {
    type Rejection = SyncError;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(SyncError::unauthorized)?;
        match devices::verify(&st.pool, token.trim()).await? {
            Some(s) => Ok(Phone(s)),
            None => Err(SyncError::unauthorized()),
        }
    }
}

async fn account(st: &AppState, s: devices::SignedIn) -> Result<Account, SyncError> {
    let a = accounts::get(&st.pool, s.principal).await?;
    let d = devices::get(&st.pool, s.principal, s.device).await?;
    Ok(Account {
        name: a.name.clone(),
        display_name: a.label().to_string(),
        device: d.id.to_string(),
        device_label: d.label,
        push_hosts: st.push.hosts.clone(),
        push: d.push,
    })
}

#[derive(Deserialize)]
struct Claim {
    code: String,
    /// The phone's name for the Devices page.
    device: String,
}

/// Redeem a sign-in code for a device token.
async fn claim(State(st): State<AppState>, body: Result<Json<Claim>, JsonRejection>) -> Reply<SignedIn> {
    let Json(c) = body?;
    let Some((s, token)) = devices::claim(&st.pool, &c.code, &c.device).await? else {
        return Err(SyncError::new(
            StatusCode::FORBIDDEN,
            "link_used",
            "this code is used up or has expired: show a new one on the Devices page",
        ));
    };
    tracing::info!(device = %s.device, "sync: a phone signed in");
    Ok(Json(SignedIn { token, account: account(&st, s).await? }))
}

async fn me(State(st): State<AppState>, phone: Phone) -> Reply<Account> {
    Ok(Json(account(&st, phone.0).await?))
}

/// Sign this phone out: its token stops working and its pushes stop.
async fn sign_out(State(st): State<AppState>, phone: Phone) -> Result<StatusCode, SyncError> {
    devices::remove(&st.pool, phone.0.principal, phone.0.device).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct PushEndpoint {
    /// The UnifiedPush endpoint the distributor gave the app.
    endpoint: String,
    /// The connector's WebPush public key and auth secret (base64url).
    key: String,
    auth: String,
}

/// Register (or replace) this phone's push endpoint.
async fn register_push(
    State(st): State<AppState>,
    phone: Phone,
    body: Result<Json<PushEndpoint>, JsonRejection>,
) -> Result<StatusCode, SyncError> {
    let Json(p) = body?;
    let refused = |e: EndpointError| {
        let code = if e == EndpointError::Off { "push_off" } else { "bad_endpoint" };
        SyncError::new(StatusCode::BAD_REQUEST, code, e.to_string())
    };
    let url = push::check_endpoint(&st.push, &p.endpoint).map_err(refused)?;
    DeviceKeys::parse(&p.key, &p.auth).map_err(|e| refused(e.into()))?;
    devices::set_push(&st.pool, phone.0.device, &url, p.key.trim(), p.auth.trim()).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unregister_push(State(st): State<AppState>, phone: Phone) -> Result<StatusCode, SyncError> {
    devices::clear_push(&st.pool, phone.0.device, None).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn collections(State(st): State<AppState>, phone: Phone) -> Reply<Collections> {
    Ok(Json(domain::sync::collections(&st.pool, phone.actor()).await?))
}

fn collection_id(id: &str) -> Result<Uuid, SyncError> {
    id.parse().map_err(|_| DomainError::NotFound.into())
}

#[derive(Deserialize)]
struct Since {
    since: Option<i64>,
    limit: Option<usize>,
}

/// The changes since a state; no `since` is a full sync.
async fn changes(
    State(st): State<AppState>,
    phone: Phone,
    Path(id): Path<String>,
    q: Result<Query<Since>, axum::extract::rejection::QueryRejection>,
) -> Reply<Batch> {
    let Query(q) = q.map_err(|e| SyncError::bad(e.body_text()))?;
    let id = collection_id(&id)?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    Ok(Json(domain::sync::changes(&st.pool, phone.actor(), id, q.since, limit).await?))
}

/// The phone's edits to one collection: each merged with the current copy.
async fn upload(
    State(st): State<AppState>,
    phone: Phone,
    Path(id): Path<String>,
    body: Result<Json<Upload>, JsonRejection>,
) -> Reply<Answer> {
    let Json(up) = body?;
    let id = collection_id(&id)?;
    if up.changes.len() > MAX_UPLOAD {
        return Err(SyncError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!("at most {MAX_UPLOAD} changes at a time"),
        ));
    }
    let mut names = std::collections::HashSet::new();
    if !up.changes.iter().all(|c| names.insert(c.name.as_str())) {
        return Err(SyncError::bad("each object at most once per upload"));
    }
    let answer = domain::sync::upload(&st.pool, phone.actor(), id, &up.changes).await?;
    if !answer.rejected.is_empty() {
        tracing::info!(rejected = answer.rejected.len(), "sync: edits refused");
    }
    Ok(Json(answer))
}
