//! scribe-shelf — read-only Audiobookshelf-compatible API over scribe's DB.
//!
//! Designed as a *dummy* service: no UI, no writes, no state of its own.
//! Implements just enough of ABS's REST surface for clients like
//! Listen This to browse and stream the scribe library directly,
//! bypassing the real Audiobookshelf when its folder-item ZIP behavior
//! trips up an iOS player or when you simply don't want to run ABS.
//!
//! All persistent reads go through a SQLite handle opened with
//! `SQLITE_OPEN_READ_ONLY` against scribe's database file — shelf
//! cannot mutate scribe state by construction, even if a bug tried.

pub mod abs;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod routes;
pub mod state;

use tracing_subscriber::EnvFilter;

use config::Config;
use state::ShelfState;

pub async fn run() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,scribe_shelf=debug")),
        )
        .init();

    let cfg = Config::from_env()?;
    let db = db::Db::open(&cfg.db_path)?;
    let http = reqwest::Client::builder()
        .user_agent(concat!("scribe-shelf/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let state = ShelfState { cfg: cfg.clone(), db, http };

    let app = routes::router(state);
    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!(
        bind = %cfg.bind,
        db_path = %cfg.db_path.display(),
        library_dir = %cfg.library_dir.display(),
        "scribe-shelf listening",
    );
    axum::serve(listener, app).await?;
    Ok(())
}
