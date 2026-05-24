//! Background polling loop.
//!
//! Every `SCRIBE_POLL_INTERVAL_MIN` minutes, do an incremental sync for
//! every account on disk. Overnight (00:00–06:00 local) the cadence
//! widens to 30 minutes to keep Amazon happy while nothing's happening.
//!
//! `auto_enqueue` is resolved per-profile: a profile setting overrides
//! the server-wide `SCRIBE_AUTO_ENQUEUE` default. Poll interval stays
//! global — it's a resource constraint, not a user preference.
//!
//! Skips silently if the shim is unreachable (typical during boot).
//! Doesn't retry on failure — next tick handles it.

use std::time::Duration;

use chrono::Local;

use crate::profile;
use crate::state::AppState;
use crate::sync;

pub fn spawn(state: AppState) {
    tokio::spawn(async move { run(state).await });
}

async fn run(state: AppState) {
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
    let accounts = list_accounts_with_profile(state).await?;
    if accounts.is_empty() {
        tracing::trace!("no accounts in DB, poller idle");
        return Ok(());
    }
    for (acct, profile_id) in accounts {
        match sync::incremental(state, &acct).await {
            Ok(r) if r.inserted > 0 => {
                tracing::info!(account = %acct, new = r.inserted, "new books discovered");
                let auto = auto_enqueue_for(state, profile_id).await;
                if auto {
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

async fn auto_enqueue_for(state: &AppState, profile_id: i64) -> bool {
    let env_default = state.cfg.auto_enqueue_new.to_string();
    match profile::effective(state, profile_id, "auto_enqueue", env_default).await {
        Ok(s) => profile::parse_bool(&s),
        Err(_) => state.cfg.auto_enqueue_new,
    }
}

async fn list_accounts_with_profile(state: &AppState) -> anyhow::Result<Vec<(String, i64)>> {
    let rows = state
        .db
        .with(|c| {
            let mut stmt = c.prepare("SELECT id, profile_id FROM accounts WHERE profile_id IS NOT NULL")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await?;
    Ok(rows)
}
