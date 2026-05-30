use std::sync::Arc;

use tracing_subscriber::EnvFilter;

mod auth;
mod config;
mod ffmpeg;
mod jobs;
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

    // Aged-out sweep: every hour, drop jobs older than 24h that were never
    // DELETEd (crash / lost backend), freeing their tmp dir + in-memory
    // voucher key. Detached; runs for the process lifetime.
    {
        let jobs = state.jobs.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                tick.tick().await;
                let swept = jobs.sweep(std::time::Duration::from_secs(24 * 3600)).await;
                if swept > 0 {
                    tracing::info!(swept, "aged-out job sweep");
                }
            }
        });
    }

    let app = routes::router(state);

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!(bind = %cfg.bind, max_jobs = cfg.max_jobs, "scribe-press listening");
    axum::serve(listener, app).await?;
    Ok(())
}
