use std::net::SocketAddr;

use anyhow::Result;
use calzone_server::config::Config;
use calzone_server::{AppState, MIGRATOR, cli, router};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();
    let cfg = Config::from_env()?;
    let pool = PgPoolOptions::new().max_connections(10).connect(&cfg.database_url).await?;
    MIGRATOR.run(&pool).await?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] | ["serve"] => {
            let Some(secret_key) = cfg.secret_key else {
                anyhow::bail!(
                    "CALZONE_SECRET_KEY is required (it encrypts two-factor secrets); \
                     make one with: head -c 32 /dev/urandom | base64"
                );
            };
            let state = AppState {
                secret_key: Some(secret_key),
                two_factor: cfg.two_factor,
                dav: cfg.dav,
                cookie_secure: cfg.cookie_secure,
                public_url: cfg.public_url.clone(),
                behind_proxy: cfg.behind_proxy,
                ..AppState::new(pool)
            };
            let stale = calzone_server::domain::imports::mark_interrupted(&state.pool).await?;
            if stale > 0 {
                tracing::warn!(count = stale, "imports interrupted by the last shutdown; running them again converges");
            }
            tracing::info!(dav = state.dav, "CalDAV/CardDAV door {}", if state.dav { "on" } else { "off" });
            let app = router(state);
            let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
            tracing::info!(bind = %cfg.bind, "listening");
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                .with_graceful_shutdown(shutdown())
                .await?;
        }
        command => println!("{}", cli::run(&pool, command).await?),
    }
    Ok(())
}

/// SIGTERM (`docker compose down`, the rolling backup) or Ctrl-C. Without
/// SIGTERM the server, PID 1 in its container, would ignore the stop and be
/// killed after the grace period. Running imports are dropped and marked
/// interrupted at the next start; running one again converges.
async fn shutdown() {
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term => {}
    }
    tracing::info!("shutting down");
}
