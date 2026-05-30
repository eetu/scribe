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

use crate::error::AppError;
use crate::profile;
use crate::shim::ShimClient;
use crate::sidecar;
use crate::state::AppState;
use crate::sync;

/// How many done-job sidecars to backfill per tick. Kept tiny so the
/// opportunistic voucher catch-up doesn't look like a scraper sweep —
/// at 2/tick × ~60min jitter, ~100 books take 24-48h to cover.
const VOUCHER_BACKFILL_PER_TICK: usize = 2;
/// Cooldown for a sidecar whose previous voucher fetch came back 410.
/// Revoked titles don't come back; recheck weekly just in case Audible
/// reissues a license.
const VOUCHER_RETRY_COOLDOWN_S: i64 = 7 * 24 * 3600;

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

/// Is `hour` (0–23) inside the active polling window `[start, end)`?
///
/// - `start < end`  → normal same-day window, e.g. 7..23.
/// - `start > end`  → wraps midnight, e.g. 22..6.
/// - `start == end` → no quiet hours: active 24/7. (The old `start <= end`
///   branch made an equal pair `hour >= s && hour < s`, i.e. *never* active,
///   silently collapsing a "24h" config to one poll a day.)
fn in_active_window(hour: u32, start: u32, end: u32) -> bool {
    if start == end {
        true
    } else if start < end {
        hour >= start && hour < end
    } else {
        hour >= start || hour < end
    }
}

/// Inside active hours: base interval + uniform jitter in ±jitter_percent.
/// Outside: sleep until the next active-hour-start, ±30min jitter.
fn next_sleep(state: &AppState) -> Duration {
    let now = Local::now();
    let hour = now.hour();
    let start = state.cfg.poll_active_hour_start;
    let end = state.cfg.poll_active_hour_end;

    let in_window = in_active_window(hour, start, end);

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
    backfill_vouchers(state).await;
    Ok(())
}

/// Opportunistic voucher backfill — fetch keys for legacy sidecars that
/// pre-date the persistence feature. Capped per tick to stay invisible.
async fn backfill_vouchers(state: &AppState) {
    let candidates = match find_sidecars_needing_voucher(state, VOUCHER_BACKFILL_PER_TICK).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = ?e, "voucher backfill scan failed");
            return;
        }
    };
    if candidates.is_empty() {
        return;
    }
    let shim = ShimClient::new(state);
    let now = chrono::Utc::now().timestamp();
    for (aaxc_path, account_id, asin) in candidates {
        match shim.voucher(&account_id, &asin).await {
            Ok(v) => {
                if let Err(e) = stamp_voucher(&aaxc_path, |sc| {
                    sc.voucher_key_hex = Some(v.key.clone());
                    sc.voucher_iv_hex = Some(v.iv.clone());
                    sc.voucher_attempt_at = None;
                })
                .await
                {
                    tracing::warn!(asin = %asin, error = ?e, "sidecar voucher write failed");
                } else {
                    tracing::info!(asin = %asin, "voucher backfilled");
                }
            }
            Err(AppError::LicenseDenied(_)) => {
                if let Err(e) = stamp_voucher(&aaxc_path, |sc| {
                    sc.voucher_attempt_at = Some(now);
                })
                .await
                {
                    tracing::warn!(asin = %asin, error = ?e, "sidecar attempt stamp failed");
                } else {
                    tracing::debug!(asin = %asin, "voucher backfill denied, cooldown set");
                }
            }
            Err(e) => {
                tracing::debug!(asin = %asin, error = ?e, "voucher backfill transient error");
            }
        }
    }
}

async fn stamp_voucher<F>(aaxc_path: &str, mutate: F) -> Result<(), AppError>
where
    F: FnOnce(&mut scribe_shared::Sidecar),
{
    let path = sidecar::sidecar_path_for(std::path::Path::new(aaxc_path));
    let mut sc = sidecar::read(&path).await?;
    mutate(&mut sc);
    sidecar::write(std::path::Path::new(aaxc_path), &sc).await
}

async fn find_sidecars_needing_voucher(
    state: &AppState,
    limit: usize,
) -> Result<Vec<(String, String, String)>, AppError> {
    // Pull recent done-jobs that recorded an AAXC path. Filtering for
    // "needs voucher" happens after the file read — cheaper than
    // walking the originals tree blind, and the DB already knows the
    // accountship + asin for free.
    let rows: Vec<(String, String, String)> = state
        .db
        .with(|c| {
            let mut stmt = c.prepare(
                "SELECT aaxc_path, account_id, asin
                 FROM jobs
                 WHERE status = 'done' AND aaxc_path IS NOT NULL
                 ORDER BY updated_at DESC",
            )?;
            let collected = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(collected)
        })
        .await?;
    let now = chrono::Utc::now().timestamp();
    let mut out = Vec::new();
    for (aaxc_path, account_id, asin) in rows {
        if out.len() >= limit {
            break;
        }
        let sc_path = sidecar::sidecar_path_for(std::path::Path::new(&aaxc_path));
        let sc = match sidecar::read(&sc_path).await {
            Ok(s) => s,
            Err(_) => continue,
        };
        if sc.voucher_key_hex.is_some() {
            continue;
        }
        if let Some(prev) = sc.voucher_attempt_at {
            if now - prev < VOUCHER_RETRY_COOLDOWN_S {
                continue;
            }
        }
        out.push((aaxc_path, account_id, asin));
    }
    Ok(out)
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

#[cfg(test)]
mod tests {
    use super::in_active_window;

    #[test]
    fn normal_window_excludes_end_hour() {
        // 07:00–23:00: active 7..=22, quiet at 23 and before 7.
        assert!(!in_active_window(6, 7, 23));
        assert!(in_active_window(7, 7, 23));
        assert!(in_active_window(22, 7, 23));
        assert!(!in_active_window(23, 7, 23));
        assert!(!in_active_window(0, 7, 23));
    }

    #[test]
    fn wrapping_window_spans_midnight() {
        // 22:00–06:00.
        assert!(in_active_window(22, 22, 6));
        assert!(in_active_window(23, 22, 6));
        assert!(in_active_window(0, 22, 6));
        assert!(in_active_window(5, 22, 6));
        assert!(!in_active_window(6, 22, 6));
        assert!(!in_active_window(12, 22, 6));
    }

    #[test]
    fn equal_start_end_is_always_active() {
        // start == end → 24/7, no quiet hours (regression: used to be never).
        for h in 0..24 {
            assert!(in_active_window(h, 0, 0), "hour {h} should be active");
            assert!(in_active_window(h, 9, 9), "hour {h} should be active");
        }
    }
}
