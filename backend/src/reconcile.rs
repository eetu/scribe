//! Filesystem-driven recovery: replay sidecars into the DB so a wiped
//! `scribe.db` (or a fresh deploy on a NAS that already holds books)
//! finds the existing files instead of re-downloading.
//!
//! Scope:
//!   - Walks `original_dir` for `*.scribe.json`.
//!   - For each sidecar, ensures a `jobs` row exists with status=done
//!     pointing at the recorded paths.
//!   - Does NOT touch the `books` table — that's owned by library sync.
//!     A reconciled-job-with-no-book pair still flags the right book as
//!     "already have it" once a library sync runs.
//!
//! Idempotent: re-running is a no-op when nothing changed.

use std::path::{Path, PathBuf};

use chrono::Utc;
use uuid::Uuid;

use crate::sidecar;
use crate::state::AppState;

#[derive(Debug, Default)]
pub struct ReconcileReport {
    pub sidecars_seen: usize,
    pub jobs_inserted: usize,
    pub jobs_already: usize,
    pub errors: usize,
}

pub async fn scan(state: &AppState) -> anyhow::Result<ReconcileReport> {
    let root = state.cfg.original_dir.clone();
    if !root.exists() {
        tracing::debug!(path = %root.display(), "original_dir missing, skipping reconcile");
        return Ok(ReconcileReport::default());
    }
    let paths = tokio::task::spawn_blocking(move || walk_sidecars(&root)).await??;
    let mut report = ReconcileReport::default();
    for path in paths {
        report.sidecars_seen += 1;
        match reconcile_one(state, &path).await {
            Ok(true) => report.jobs_inserted += 1,
            Ok(false) => report.jobs_already += 1,
            Err(e) => {
                report.errors += 1;
                tracing::warn!(path = %path.display(), error = ?e, "reconcile failed");
            }
        }
    }
    if report.sidecars_seen > 0 {
        tracing::info!(
            seen = report.sidecars_seen,
            inserted = report.jobs_inserted,
            already = report.jobs_already,
            errors = report.errors,
            "reconcile pass complete"
        );
    }
    Ok(report)
}

async fn reconcile_one(state: &AppState, sidecar_path: &Path) -> anyhow::Result<bool> {
    let sc = sidecar::read(sidecar_path).await?;
    let asin = sc.asin.clone();
    let account = sc.account_id.clone();
    let exists: i64 = state
        .db
        .with({
            let asin = asin.clone();
            let account = account.clone();
            move |c| {
                // Any existing job row blocks insert — including failed or
                // cancelled ones. A failed-state row means something
                // notable happened (license denial, ffmpeg error, manual
                // intervention) and silently replacing it with a fresh
                // "done" row from a stale sidecar would erase that
                // signal. Real DB-wipe recovery starts from zero rows
                // anyway, so this stays correct for the originally
                // intended use case.
                c.query_row(
                    "SELECT COUNT(*) FROM jobs WHERE asin = ?1 AND account_id = ?2",
                    rusqlite::params![asin, account],
                    |r| r.get(0),
                )
            }
        })
        .await?;
    if exists > 0 {
        return Ok(false);
    }
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    let downloaded_at = sc.downloaded_at;
    let m4b = sc.m4b_path.clone();
    let aaxc = sc.aaxc_path.clone();
    state
        .db
        .with(move |c| {
            c.execute(
                "INSERT INTO jobs (id, asin, account_id, status, created_at, updated_at, m4b_path, aaxc_path)
                 VALUES (?1, ?2, ?3, 'done', ?4, ?5, ?6, ?7)",
                rusqlite::params![id, asin, account, downloaded_at, now, m4b, aaxc],
            )?;
            Ok(())
        })
        .await?;
    Ok(true)
}

fn walk_sidecars(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk_dir(root, &mut out)?;
    Ok(out)
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let p = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk_dir(&p, out)?;
        } else if ft.is_file()
            && p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(".scribe.json"))
                .unwrap_or(false)
        {
            out.push(p);
        }
    }
    Ok(())
}

pub fn spawn_boot_scan(state: AppState) {
    tokio::spawn(async move {
        // Small delay so the rest of boot logs first.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if let Err(e) = scan(&state).await {
            tracing::warn!(error = ?e, "reconcile boot scan failed");
        }
    });
}
