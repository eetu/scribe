//! Background polling loop.
//!
//! Every `SCRIBE_POLL_INTERVAL_MIN` minutes, do an incremental sync for
//! every account on disk. Overnight (00:00–06:00 local) we back off to 30
//! minutes so we don't hammer Amazon while nothing's happening.
//!
//! Skips silently if the shim is unreachable (typical during boot). Doesn't
//! retry on failure — next tick handles it.

use std::time::Duration;

use chrono::Local;

use crate::state::AppState;
use crate::sync;

pub fn spawn(state: AppState) {
    tokio::spawn(async move { run(state).await });
}

async fn run(state: AppState) {
    // Small initial delay so the HTTP server is up before we start probing.
    tokio::time::sleep(Duration::from_secs(10)).await;
    tracing::info!(
        interval_min = state.cfg.poll_interval_min,
        "poller started",
    );
    loop {
        let _ = tick(&state).await;
        tokio::time::sleep(interval(&state)).await;
    }
}

fn interval(state: &AppState) -> Duration {
    let base = state.cfg.poll_interval_min;
    let hour = Local::now().format("%H").to_string().parse::<u32>().unwrap_or(12);
    let mins = if (0..6).contains(&hour) { base.max(30) } else { base };
    Duration::from_secs(mins * 60)
}

async fn tick(state: &AppState) -> anyhow::Result<()> {
    let accounts = list_account_ids(state).await?;
    if accounts.is_empty() {
        tracing::trace!("no accounts in DB, poller idle");
        return Ok(());
    }
    for acct in accounts {
        match sync::incremental(state, &acct).await {
            Ok(r) if r.inserted > 0 => {
                tracing::info!(account = %acct, new = r.inserted, "new books discovered");
                if state.cfg.auto_enqueue_new {
                    match state.queue().enqueue_pending(&acct).await {
                        Ok(ids) => tracing::info!(account = %acct, queued = ids.len(), "auto-enqueued"),
                        Err(e) => tracing::warn!(account = %acct, error = ?e, "auto-enqueue failed"),
                    }
                }
            }
            Ok(_) => tracing::debug!(account = %acct, "no changes"),
            Err(e) => tracing::warn!(account = %acct, error = ?e, "sync failed"),
        }
    }
    Ok(())
}

async fn list_account_ids(state: &AppState) -> anyhow::Result<Vec<String>> {
    let rows = state
        .db
        .with(|c| {
            let mut stmt = c.prepare("SELECT id FROM accounts")?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?;
    Ok(rows)
}
