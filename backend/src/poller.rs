//! Background polling loop.
//!
//! Audible's mobile app doesn't poll on a fixed timer — it queries the
//! library when the user opens the app. To stay below any "scraper"
//! radar we mimic that with: a base interval, a random ±jitter, and
//! active-hour gating that skips the dead-of-night entirely.
//!
//! Tuning:
//!   - `SCRIBE_POLL_INTERVAL_MIN` (default 60) — base cadence
//!   - `SCRIBE_POLL_JITTER_PERCENT` (default 50) —
//!     `next = base · (1 ± rand·jitter)`
//!   - `SCRIBE_POLL_ACTIVE_HOUR_START` / `_END` (default 7 / 23, local)
//!     — outside this window the loop sleeps until the next start
//!     (with a small wake-jitter so multiple Pis don't pile on)
//!
//! `auto_enqueue` is resolved per-profile: a profile setting overrides
//! the server-wide `SCRIBE_AUTO_ENQUEUE` default. Poll interval stays
//! global — it's a resource constraint, not a user preference.
//!
//! Skips silently if the shim is unreachable (typical during boot).
//! Doesn't retry on failure — next tick handles it.

use std::time::Duration;

use chrono::{Local, Timelike};
use rand::Rng;

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
        jitter_pct = state.cfg.poll_jitter_percent,
        active = format!(
            "{:02}:00–{:02}:00",
            state.cfg.poll_active_hour_start, state.cfg.poll_active_hour_end
        ),
        "poller started",
    );
    loop {
        let sleep = next_sleep(&state);
        tracing::debug!(sleep_s = sleep.as_secs(), "next poll");
        tokio::time::sleep(sleep).await;
        let _ = tick(&state).await;
    }
}

/// Inside active hours: base interval + uniform jitter in ±jitter_percent.
/// Outside: sleep until the next active-hour-start, ±30min jitter.
fn next_sleep(state: &AppState) -> Duration {
    let now = Local::now();
    let hour = now.hour();
    let start = state.cfg.poll_active_hour_start;
    let end = state.cfg.poll_active_hour_end;

    let in_window = if start <= end {
        hour >= start && hour < end
    } else {
        hour >= start || hour < end
    };

    if !in_window {
        let cur_min = now.hour() * 60 + now.minute();
        let start_min = start * 60;
        let base = if start_min > cur_min {
            start_min - cur_min
        } else {
            24 * 60 - cur_min + start_min
        };
        let extra = rand::rng().random_range(0..30);
        return Duration::from_secs(u64::from(base + extra) * 60);
    }

    let base_secs = state.cfg.poll_interval_min.saturating_mul(60);
    let jitter_pct = (state.cfg.poll_jitter_percent.min(95)) as f64 / 100.0;
    let factor = 1.0 + rand::rng().random_range(-jitter_pct..=jitter_pct);
    let secs = ((base_secs as f64) * factor).max(60.0) as u64;
    Duration::from_secs(secs)
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
