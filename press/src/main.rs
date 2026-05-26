use std::sync::Arc;

use tracing_subscriber::EnvFilter;

mod auth;
mod config;
mod ffmpeg;
mod jobs;
mod mp4patch;
mod routes;
mod state;

use config::Config;
use jobs::JobMap;
use state::PressState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,scribe_press=debug")))
        .init();

    let cfg = Config::from_env()?;
    let jobs = JobMap::new(cfg.tmp_dir.clone(), cfg.max_jobs);

    let state = PressState {
        cfg: Arc::new(cfg.clone()),
        jobs,
    };

    let app = routes::router(state);

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!(bind = %cfg.bind, max_jobs = cfg.max_jobs, "scribe-press listening");
    axum::serve(listener, app).await?;
    Ok(())
}
