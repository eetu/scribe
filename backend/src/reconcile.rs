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

use rusqlite::OptionalExtension;
use uuid::Uuid;

use crate::sidecar;
use crate::state::AppState;

#[derive(Debug, Default)]
pub struct ReconcileReport {
    pub sidecars_seen: usize,
    pub jobs_inserted: usize,
    pub jobs_promoted: usize,
    pub jobs_already: usize,
    pub errors: usize,
}

enum Outcome {
    Inserted,
    Promoted,
    Already,
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
            Ok(Outcome::Inserted) => report.jobs_inserted += 1,
            Ok(Outcome::Promoted) => report.jobs_promoted += 1,
            Ok(Outcome::Already) => report.jobs_already += 1,
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
            promoted = report.jobs_promoted,
            already = report.jobs_already,
            errors = report.errors,
            "reconcile pass complete"
        );
    }
    Ok(report)
}

async fn reconcile_one(state: &AppState, sidecar_path: &Path) -> anyhow::Result<Outcome> {
    let sc = sidecar::read(sidecar_path).await?;
    let asin = sc.asin.clone();
    let account = sc.account_id.clone();
    let m4b = sc.m4b_path.clone();
    let aaxc = sc.aaxc_path.clone();

    // Tombstoned by an explicit user removal — the leftover sidecar must
    // not resurrect the book. The audio + voucher files are intentionally
    // left on disk; only scribe's tracking was removed.
    let tombstoned: bool = state
        .db
        .with({
            let asin = asin.clone();
            let account = account.clone();
            move |c| {
                c.query_row(
                    "SELECT 1 FROM removed_books WHERE asin = ?1 AND account_id = ?2",
                    rusqlite::params![asin, account],
                    |_| Ok(()),
                )
                .optional()
                .map(|o| o.is_some())
            }
        })
        .await?;
    if tombstoned {
        return Ok(Outcome::Already);
    }

    // Physical presence of the library m4b is ground truth: if the file
    // is on disk the book is playable regardless of how it got there
    // (normal convert, or a hand-placed copy — e.g. an OpenAudible
    // rescue of a title Audible has since revoked the voucher for).
    let m4b_present = tokio::fs::try_exists(&m4b).await.unwrap_or(false);

    let exists: i64 = state
        .db
        .with({
            let asin = asin.clone();
            let account = account.clone();
            move |c| {
                c.query_row(
                    "SELECT COUNT(*) FROM jobs WHERE asin = ?1 AND account_id = ?2",
                    rusqlite::params![asin, account],
                    |r| r.get(0),
                )
            }
        })
        .await?;

    if exists > 0 {
        // A row already exists. Normally we leave it — a failed/cancelled
        // status encodes a real signal (license denial, ffmpeg error)
        // that a stale sidecar shouldn't silently overwrite. The one
        // exception is when the m4b is physically present: the file
        // existing *is* the resolution, so promote any non-done row to
        // done (covers dropping a working copy into the library by hand).
        if !m4b_present {
            return Ok(Outcome::Already);
        }
        let now = crate::util::now_iso();
        let promoted = state
            .db
            .with({
                let asin = asin.clone();
                let account = account.clone();
                let m4b = m4b.clone();
                let aaxc = aaxc.clone();
                move |c| {
                    c.execute(
                        "UPDATE jobs SET status = 'done', m4b_path = ?3, aaxc_path = ?4, updated_at = ?5
                         WHERE asin = ?1 AND account_id = ?2 AND status != 'done'",
                        rusqlite::params![asin, account, m4b, aaxc, now],
                    )
                }
            })
            .await?;
        return Ok(if promoted > 0 {
            Outcome::Promoted
        } else {
            Outcome::Already
        });
    }

    let id = Uuid::new_v4().to_string();
    let now = crate::util::now_iso();
    let downloaded_at = sc.downloaded_at;
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
    Ok(Outcome::Inserted)
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
