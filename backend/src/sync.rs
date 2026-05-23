//! Library sync: shim → SQLite.
//!
//! Two flavours:
//!   * `full(state, account_id)` — walk every page until the shim runs out.
//!     Manual trigger or first-time sync of an account.
//!   * `incremental(state, account_id)` — fetch only the newest 10 (cheap
//!     ASIN diff via `purchase_date DESC`). Used by the polling loop.
//!
//! Both end by stamping `accounts.last_synced_at`.

use chrono::Utc;
use serde::Serialize;

use crate::error::AppError;
use crate::shim::{LibraryBook, ShimClient};
use crate::state::AppState;

#[derive(Debug, Default, Serialize, Clone)]
pub struct SyncReport {
    pub account_id: String,
    pub seen: u64,
    pub inserted: u64,
    pub updated: u64,
    pub finished_at_unix: i64,
}

pub async fn full(state: &AppState, account_id: &str) -> Result<SyncReport, AppError> {
    walk(state, account_id, None).await
}

pub async fn incremental(state: &AppState, account_id: &str) -> Result<SyncReport, AppError> {
    walk(state, account_id, Some(10)).await
}

async fn walk(state: &AppState, account_id: &str, limit: Option<u64>) -> Result<SyncReport, AppError> {
    let shim = ShimClient::new(state);
    let mut report = SyncReport {
        account_id: account_id.to_string(),
        ..Default::default()
    };

    let mut page: u64 = 1;
    let page_size: u64 = limit.unwrap_or(100);
    loop {
        let resp = shim.library(account_id, page, page_size, None).await?;
        if resp.items.is_empty() {
            break;
        }
        for book in &resp.items {
            let (inserted, updated) = upsert(state, account_id, book).await?;
            report.seen += 1;
            if inserted {
                report.inserted += 1;
            } else if updated {
                report.updated += 1;
            }
        }
        if limit.is_some() {
            // Incremental path: only the newest slice; one round-trip.
            break;
        }
        if (resp.items.len() as u64) < page_size {
            break;
        }
        if page * page_size >= resp.total_results {
            break;
        }
        page += 1;
    }

    let now = Utc::now().timestamp();
    let acct = account_id.to_string();
    state
        .db
        .with(move |c| {
            c.execute(
                "UPDATE accounts SET last_synced_at = ?1 WHERE id = ?2",
                rusqlite::params![now, acct],
            )?;
            Ok(())
        })
        .await?;
    report.finished_at_unix = now;
    tracing::info!(
        account = %report.account_id,
        seen = report.seen,
        inserted = report.inserted,
        updated = report.updated,
        "library sync complete",
    );
    Ok(report)
}

async fn upsert(state: &AppState, account_id: &str, book: &LibraryBook) -> Result<(bool, bool), AppError> {
    let asin = book.asin.clone();
    let acct = account_id.to_string();
    let title = book.title.clone();
    let subtitle = book.subtitle.clone();
    let authors_json = serde_json::to_string(&book.authors).unwrap_or_else(|_| "[]".into());
    let narrators_json = serde_json::to_string(&book.narrators).unwrap_or_else(|_| "[]".into());
    let series_title = book.series.first().and_then(|s| s.title.clone());
    let series_sequence = book.series.first().and_then(|s| s.sequence.clone());
    // rusqlite 0.39 dropped `u64: ToSql`. SQLite stores INTEGER as i64
    // anyway — cast at the boundary.
    let runtime_ms: Option<i64> = book.runtime_length_min.map(|m| (m * 60_000) as i64);
    let cover_url = book.cover_url.clone();
    let status = book.status.clone();
    let purchase_date = book.purchase_date.clone();
    let now = Utc::now().timestamp();

    state
        .db
        .with(move |c| {
            // Probe the current row so we can distinguish "inserted" vs "updated"
            // without relying on rusqlite's RETURNING (sqlite < 3.35 lacks it on
            // some Pi images, the bundled feature gives us 3.44 but let's stay
            // explicit).
            let prior: Option<String> = c
                .query_row(
                    "SELECT status FROM books WHERE asin = ?1 AND account_id = ?2",
                    rusqlite::params![asin, acct],
                    |r| r.get(0),
                )
                .ok();
            let exists = prior.is_some();
            c.execute(
                "INSERT INTO books (
                   asin, account_id, title, subtitle, authors_json, narrators_json,
                   series_title, series_sequence, runtime_length_ms, cover_url,
                   status, purchase_date, first_seen_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                 ON CONFLICT(asin, account_id) DO UPDATE SET
                   title = excluded.title,
                   subtitle = excluded.subtitle,
                   authors_json = excluded.authors_json,
                   narrators_json = excluded.narrators_json,
                   series_title = excluded.series_title,
                   series_sequence = excluded.series_sequence,
                   runtime_length_ms = excluded.runtime_length_ms,
                   cover_url = excluded.cover_url,
                   status = excluded.status,
                   purchase_date = excluded.purchase_date",
                rusqlite::params![
                    asin,
                    acct,
                    title,
                    subtitle,
                    authors_json,
                    narrators_json,
                    series_title,
                    series_sequence,
                    runtime_ms,
                    cover_url,
                    status,
                    purchase_date,
                    now,
                ],
            )?;
            let inserted = !exists;
            let updated = exists && prior.as_deref() != Some(status.as_str());
            Ok((inserted, updated))
        })
        .await
        .map_err(AppError::from)
}

/// Insert (or refresh) an account row that shim already owns.
/// Called from login_finish — links the shim account to the user_sub
/// currently logged in via OIDC/dev cookie.
pub async fn register_account(
    state: &AppState,
    account_id: &str,
    locale: &str,
    email_masked: &str,
    customer_name: Option<&str>,
    user_sub: &str,
) -> Result<(), AppError> {
    let aid = account_id.to_string();
    let loc = locale.to_string();
    let em = email_masked.to_string();
    let cn = customer_name.map(|s| s.to_string());
    let us = user_sub.to_string();
    state
        .db
        .with(move |c| {
            c.execute(
                "INSERT INTO accounts (id, locale, email_masked, customer_name, user_sub)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(id) DO UPDATE SET
                   locale = excluded.locale,
                   email_masked = excluded.email_masked,
                   customer_name = excluded.customer_name,
                   user_sub = excluded.user_sub",
                rusqlite::params![aid, loc, em, cn, us],
            )?;
            Ok(())
        })
        .await
        .map_err(AppError::from)
}
