//! CalZone server library: the router and stores, split from `main` so
//! integration tests drive the real app.

pub mod audit;
pub mod cli;
pub mod client;
pub mod config;
pub mod dav;
pub mod domain;
pub mod export;
pub mod import;
pub mod throttle;
pub mod web;

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use sqlx::PgPool;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    /// `CALZONE_DAV`: whether the CalDAV/CardDAV door is mounted.
    pub dav: bool,
    /// `Secure` flag on the web session cookie (and https in links).
    pub cookie_secure: bool,
    /// `CALZONE_PUBLIC_URL`.
    pub public_url: Option<String>,
    /// `CALZONE_BEHIND_PROXY`: trust the right-most `X-Forwarded-For` entry.
    pub behind_proxy: bool,
    /// Login failures per client address and per account (DAV and web).
    pub throttle: Arc<throttle::Throttle>,
    /// `CALZONE_SECRET_KEY`: two-factor secrets at rest. None only in tests
    /// and CLI runs; `serve` refuses to start without it.
    pub secret_key: Option<domain::two_factor::SecretKey>,
    /// `CALZONE_TWO_FACTOR`.
    pub two_factor: config::TwoFactorPolicy,
}

impl AppState {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            dav: true,
            cookie_secure: true,
            public_url: None,
            behind_proxy: false,
            throttle: Arc::default(),
            secret_key: None,
            two_factor: config::TwoFactorPolicy::Admins,
        }
    }
}

pub fn router(state: AppState) -> Router {
    let mut app = Router::new().route("/healthz", get(healthz)).merge(web::routes(state.dav));
    if state.dav {
        app = app.merge(dav::routes());
    }
    app.layer(CompressionLayer::new()).layer(TraceLayer::new_for_http()).with_state(state)
}

/// `ok` when the database answers; `503` otherwise, so the container
/// healthcheck and Traefik see a real outage. The error itself goes to the
/// log, never to the client.
async fn healthz(State(st): State<AppState>) -> Result<&'static str, StatusCode> {
    match sqlx::query("SELECT 1").execute(&st.pool).await {
        Ok(_) => Ok("ok"),
        Err(e) => {
            tracing::warn!(error = %e, "healthz: database unreachable");
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}
