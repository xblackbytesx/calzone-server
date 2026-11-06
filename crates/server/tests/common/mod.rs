//! Shared harness: a fresh database per test (dropped at the end) and the
//! real router.
#![allow(dead_code)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use calzone_server::{AppState, MIGRATOR, router};
use http_body_util::BodyExt as _;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt as _;
use uuid::Uuid;

pub struct Db {
    pub pool: PgPool,
    admin: PgPool,
    name: String,
}

impl Db {
    /// None (and a SKIP line) when `CALZONE_TEST_DATABASE_URL` is unset.
    pub async fn new() -> Option<Db> {
        let Ok(base) = std::env::var("CALZONE_TEST_DATABASE_URL") else {
            eprintln!("SKIP: CALZONE_TEST_DATABASE_URL unset");
            return None;
        };
        let admin = PgPoolOptions::new().max_connections(1).connect(&base).await.unwrap();
        let name = format!("calzone_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {name}")).execute(&admin).await.unwrap();
        let url = format!("{}/{name}", base.rsplit_once('/').unwrap().0);
        let pool = PgPoolOptions::new().max_connections(8).connect(&url).await.unwrap();
        MIGRATOR.run(&pool).await.unwrap();
        Some(Db { pool, admin, name })
    }

    pub fn state(&self) -> AppState {
        // Two-factor is optional here so page tests need no authenticator;
        // tests/web_pg.rs covers the policies themselves.
        AppState {
            cookie_secure: false,
            secret_key: Some([42; 32]),
            two_factor: calzone_server::config::TwoFactorPolicy::Optional,
            ..AppState::new(self.pool.clone())
        }
    }

    pub fn app(&self) -> Router {
        router(self.state())
    }

    /// Drop the database. Call at the end of every test.
    pub async fn drop(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name)).execute(&self.admin).await.unwrap();
    }
}

/// One request through the router: status and body as text.
pub async fn get(app: &Router, path: &str) -> (StatusCode, String) {
    let res = app.clone().oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: String,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
    }
}

/// Any method, optional Basic credentials, extra headers and a body.
pub async fn send(
    app: &Router,
    method: &str,
    path: &str,
    auth: Option<(&str, &str)>,
    headers: &[(&str, &str)],
    body: &str,
) -> Reply {
    use base64::Engine as _;
    let mut req = Request::builder().method(method).uri(path);
    if let Some((u, p)) = auth {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"));
        req = req.header("authorization", format!("Basic {token}"));
    }
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app.clone().oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    Reply { status, headers, body: String::from_utf8_lossy(&body).into_owned() }
}

/// Any method with a byte body; the reply's body as bytes.
pub async fn send_bytes(
    app: &Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app.clone().oneshot(req.body(Body::from(body)).unwrap()).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, body)
}

/// The real app on a TCP port (for clients that make HTTP requests, like
/// the Baikal importer); its base URL.
pub async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
    });
    format!("http://{addr}")
}
