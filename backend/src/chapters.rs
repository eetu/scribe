//! Per-book chapter persistence.
//!
//! Chapters come from Audible — the voucher at convert time, or the
//! `/metadata` endpoint for backfilling older rescues (it works even when
//! the license is revoked). They're stored as JSON on `books.chapters_json`
//! so the read-only shelf sidecar can emit ABS `media.chapters`. Listen
//! This reads `media.chapters`; when it's empty the app falls back to
//! one-chapter-per-track, which for a single-m4b book collapses the whole
//! book into a single bogus chapter.

use std::time::Duration;

use scribe_shared::Chapter;

use crate::shim::{ChapterEntry, ShimClient};
use crate::state::AppState;

fn to_stored(entries: &[ChapterEntry]) -> Vec<Chapter> {
    entries
        .iter()
        .enumerate()
        .map(|(i, c)| Chapter {
            title: c
                .title
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| format!("Chapter {}", i + 1)),
            start_offset_ms: c.start_offset_ms,
            length_ms: c.length_ms,
        })
        .collect()
}

/// Persist chapters onto the book row as JSON. No-op on empty input.
pub async fn store(state: &AppState, account: &str, asin: &str, entries: &[ChapterEntry]) {
    if entries.is_empty() {
        return;
    }
    let Ok(json) = serde_json::to_string(&to_stored(entries)) else {
        return;
    };
    let (a, ac) = (asin.to_string(), account.to_string());
    let _ = state
        .db
        .with(move |c| {
            c.execute(
                "UPDATE books SET chapters_json = ?1 WHERE asin = ?2 AND account_id = ?3",
                rusqlite::params![json, a, ac],
            )
        })
        .await;
}

/// Force a chapter re-fetch for one book (used by per-item refresh).
pub async fn refetch(state: &AppState, account: &str, asin: &str) {
    let shim = ShimClient::new(state);
    if let Ok(md) = shim.metadata(account, asin).await {
        store(state, account, asin, &md.chapters).await;
    }
}

/// Force a chapter re-fetch across every done book a profile owns (global
/// refresh). Trickled for Audible politeness.
pub async fn refetch_owned(state: &AppState, profile_id: i64) {
    let rows: Vec<(String, String)> = state
        .db
        .with(move |c| {
            let mut stmt = c.prepare(
                "SELECT b.asin, b.account_id FROM books b
                 JOIN accounts a ON a.id = b.account_id
                 JOIN jobs j ON j.asin = b.asin AND j.account_id = b.account_id
                 WHERE a.profile_id = ?1
                   AND j.status = 'done' AND j.m4b_path IS NOT NULL
                 GROUP BY b.asin, b.account_id",
            )?;
            let v = stmt
                .query_map([profile_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .await
        .unwrap_or_default();
    let shim = ShimClient::new(state);
    for (asin, account) in rows {
        if let Ok(md) = shim.metadata(&account, &asin).await {
            store(state, &account, &asin, &md.chapters).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// One-shot backfill for already-imported books missing chapters. Fetches
/// from shim `/metadata`, trickled for Audible politeness. Gated by NULL
/// `chapters_json` so it self-limits and won't re-run once filled.
pub fn spawn_boot_backfill(state: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let rows: Vec<(String, String)> = match state
            .db
            .with(|c| {
                let mut stmt = c.prepare(
                    "SELECT b.asin, b.account_id FROM books b
                     JOIN jobs j ON j.asin = b.asin AND j.account_id = b.account_id
                     WHERE b.chapters_json IS NULL
                       AND j.status = 'done' AND j.m4b_path IS NOT NULL
                     GROUP BY b.asin, b.account_id",
                )?;
                let v = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(v)
            })
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = ?e, "chapter backfill query failed");
                return;
            }
        };
        if rows.is_empty() {
            return;
        }
        let shim = ShimClient::new(&state);
        let mut filled = 0usize;
        for (asin, account) in rows {
            match shim.metadata(&account, &asin).await {
                Ok(md) if !md.chapters.is_empty() => {
                    store(&state, &account, &asin, &md.chapters).await;
                    filled += 1;
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(asin, error = ?e, "chapter backfill fetch failed"),
            }
            // Trickle so a cold start doesn't burst Audible.
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if filled > 0 {
            tracing::info!(filled, "chapter backfill complete");
        }
    });
}
