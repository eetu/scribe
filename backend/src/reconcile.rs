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
//! A recorded path — in the sidecar or in `jobs` — is a memory of where a
//! file was, not a claim about where it is. `SCRIBE_LIBRARY_DIR` and
//! `SCRIBE_ORIGINAL_DIR` are the present truth: when a recorded path no
//! longer exists, reconcile looks for the same file under the configured
//! root (see [`reroot`]), and when that finds it, repairs both the
//! sidecar and any matching `jobs` row to point at the resolved path — so
//! a share remounted under a different root heals instead of reporting
//! the whole library missing.
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
    pub paths_rerooted: usize,
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
            Ok((outcome, path_rerooted)) => {
                match outcome {
                    Outcome::Inserted => report.jobs_inserted += 1,
                    Outcome::Promoted => report.jobs_promoted += 1,
                    Outcome::Already => report.jobs_already += 1,
                }
                if path_rerooted {
                    report.paths_rerooted += 1;
                }
            }
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
            rerooted = report.paths_rerooted,
            errors = report.errors,
            "reconcile pass complete"
        );
    }
    Ok(report)
}

async fn reconcile_one(state: &AppState, sidecar_path: &Path) -> anyhow::Result<(Outcome, bool)> {
    let sc = sidecar::read(sidecar_path).await?;
    let asin = sc.asin.clone();
    let account = sc.account_id.clone();

    let recorded_aaxc = PathBuf::from(&sc.aaxc_path);
    let recorded_m4b = PathBuf::from(&sc.m4b_path);

    // The recorded aaxc path first, then the location its own sidecar says
    // it lives at (the sidecar's suffix inverted), then a generic re-root
    // under the configured tree.
    let aaxc = if recorded_aaxc.is_file() {
        recorded_aaxc.clone()
    } else if let Some(from_sidecar) = sidecar::aaxc_path_for(sidecar_path).filter(|p| p.is_file())
    {
        from_sidecar
    } else if let Some(found) = reroot(&recorded_aaxc, &state.cfg.original_dir) {
        found
    } else {
        recorded_aaxc.clone()
    };

    let m4b = if recorded_m4b.is_file() {
        recorded_m4b.clone()
    } else if let Some(found) = reroot(&recorded_m4b, &state.cfg.library_dir) {
        found
    } else {
        recorded_m4b.clone()
    };

    let path_rerooted = aaxc != recorded_aaxc || m4b != recorded_m4b;
    if path_rerooted {
        let mut resolved = sc.clone();
        resolved.aaxc_path = aaxc.display().to_string();
        resolved.m4b_path = m4b.display().to_string();
        let write_target =
            sidecar::aaxc_path_for(sidecar_path).unwrap_or_else(|| sidecar_path.to_path_buf());
        if let Err(e) = sidecar::write(&write_target, &resolved).await {
            tracing::warn!(path = %sidecar_path.display(), error = ?e, "sidecar reroot rewrite failed");
        }
    }

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
        return Ok((Outcome::Already, path_rerooted));
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
        // A stale `jobs` row keeps whatever path it was written with even
        // when its status never changes (already `done`, or a genuine
        // failure this pass shouldn't touch), so the resolved path is
        // written back on its own, independent of the promotion below.
        if path_rerooted {
            let now = crate::util::now_iso();
            let m4b_str = m4b.display().to_string();
            let aaxc_str = aaxc.display().to_string();
            state
                .db
                .with({
                    let asin = asin.clone();
                    let account = account.clone();
                    move |c| {
                        c.execute(
                            "UPDATE jobs SET m4b_path = ?3, aaxc_path = ?4, updated_at = ?5
                             WHERE asin = ?1 AND account_id = ?2
                               AND (m4b_path IS NOT ?3 OR aaxc_path IS NOT ?4)",
                            rusqlite::params![asin, account, m4b_str, aaxc_str, now],
                        )
                    }
                })
                .await?;
        }
        // A row already exists. Normally we leave it — a failed/cancelled
        // status encodes a real signal (license denial, ffmpeg error)
        // that a stale sidecar shouldn't silently overwrite. The one
        // exception is when the m4b is physically present: the file
        // existing *is* the resolution, so promote any non-done row to
        // done (covers dropping a working copy into the library by hand).
        if !m4b_present {
            return Ok((Outcome::Already, path_rerooted));
        }
        let now = crate::util::now_iso();
        let m4b_str = m4b.display().to_string();
        let aaxc_str = aaxc.display().to_string();
        let promoted = state
            .db
            .with({
                let asin = asin.clone();
                let account = account.clone();
                move |c| {
                    c.execute(
                        "UPDATE jobs SET status = 'done', m4b_path = ?3, aaxc_path = ?4, updated_at = ?5
                         WHERE asin = ?1 AND account_id = ?2 AND status != 'done'",
                        rusqlite::params![asin, account, m4b_str, aaxc_str, now],
                    )
                }
            })
            .await?;
        return Ok((
            if promoted > 0 {
                Outcome::Promoted
            } else {
                Outcome::Already
            },
            path_rerooted,
        ));
    }

    let id = Uuid::new_v4().to_string();
    let now = crate::util::now_iso();
    let downloaded_at = sc.downloaded_at;
    let m4b_str = m4b.display().to_string();
    let aaxc_str = aaxc.display().to_string();
    state
        .db
        .with(move |c| {
            c.execute(
                "INSERT INTO jobs (id, asin, account_id, status, created_at, updated_at, m4b_path, aaxc_path)
                 VALUES (?1, ?2, ?3, 'done', ?4, ?5, ?6, ?7)",
                rusqlite::params![id, asin, account, downloaded_at, now, m4b_str, aaxc_str],
            )?;
            Ok(())
        })
        .await?;
    Ok((Outcome::Inserted, path_rerooted))
}

/// A recorded path as it is now: itself when it exists, otherwise the same
/// file under `root` — found by trying the path's own tail, longest first.
///
/// A recorded path is a memory of where a file was; a share getting
/// remounted under a different root leaves everything below the old mount
/// point unchanged, so the longest surviving tail is tried first. The
/// shortest (and last) attempt is `root` joined with the bare filename —
/// still the same file, identified by name alone as the final fallback.
/// Every candidate is stat'd with [`Path::is_file`], so a directory of the
/// same name is never mistaken for a match. At most `n - 1` stat calls for
/// an `n`-component path.
pub fn reroot(recorded: &Path, root: &Path) -> Option<PathBuf> {
    if recorded.is_file() {
        return Some(recorded.to_path_buf());
    }
    let tail: Vec<&std::ffi::OsStr> = recorded
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    for k in 1..tail.len() {
        let mut candidate = root.to_path_buf();
        for seg in tail[k..].iter().copied() {
            candidate.push(seg);
        }
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::reroot;
    use super::AppState;

    /// A minimal `AppState` over a throwaway on-disk DB — no queue, no
    /// OIDC, no network. Enough to drive `scan`/`reconcile_one`, which
    /// touch only `state.cfg` and `state.db`.
    async fn test_state(tmp: &std::path::Path) -> AppState {
        let cfg = crate::config::Config {
            bind: "127.0.0.1:0".into(),
            db_path: tmp.join("scribe.db"),
            session_key_hex: "00".repeat(64),
            dev_auth: true,
            shim_url: "http://127.0.0.1:0".into(),
            shim_token: None,
            press_url: None,
            press_token: None,
            shelf_url: None,
            shelf_api_key: None,
            library_dir: tmp.join("books"),
            original_dir: tmp.join("originals"),
            covers_dir: tmp.join("covers"),
            internal_url: None,
            poll_interval_min: 60,
            poll_jitter_percent: 50,
            poll_active_hour_start: 7,
            poll_active_hour_end: 23,
            job_concurrency: 1,
            job_retry_max: 3,
            job_interjob_delay_s: 60,
            job_interjob_jitter_percent: 50,
            auto_enqueue_new: false,
            naming: crate::filenaming::Templates {
                library: crate::filenaming::Templates::DEFAULT_LIBRARY.into(),
                original: crate::filenaming::Templates::DEFAULT_ORIGINAL.into(),
            },
            abs_url: None,
            abs_token: None,
            abs_library_id: None,
            oidc: None,
        };
        let db = crate::db::Db::open(&cfg.db_path).expect("open db");
        AppState {
            cfg: std::sync::Arc::new(cfg),
            db,
            http: reqwest::Client::new(),
            cookie_key: crate::auth::cookie_key(&"00".repeat(64)),
            queue: std::sync::Arc::new(std::sync::OnceLock::new()),
            oidc: std::sync::Arc::new(crate::oidc::OidcLazy::new(None)),
            aaxc_tokens: crate::state::AaxcTokenStore::default(),
        }
    }

    #[tokio::test]
    async fn scan_reroots_a_moved_share_and_repairs_row_and_sidecar() {
        let dir = tempfile::tempdir().expect("tempdir");
        let originals = dir.path().join("originals");
        let books = dir.path().join("books");
        let aaxc_real = originals.join("A/T/T-ASIN.aaxc");
        let m4b_real = books.join("A/T/T.m4b");
        std::fs::create_dir_all(aaxc_real.parent().unwrap()).expect("mkdir originals");
        std::fs::create_dir_all(m4b_real.parent().unwrap()).expect("mkdir books");
        std::fs::write(&aaxc_real, b"aaxc bytes").expect("write aaxc");
        std::fs::write(&m4b_real, b"m4b bytes").expect("write m4b");

        let sc = scribe_shared::Sidecar {
            asin: "ASIN".into(),
            account_id: "acct".into(),
            title: "T".into(),
            downloaded_at: 0,
            m4b_path: "/old/mount/books/A/T/T.m4b".into(),
            aaxc_path: "/old/mount/originals/A/T/T-ASIN.aaxc".into(),
            voucher_refresh_date: None,
            customer_name: None,
            scribe_version: "test".into(),
            voucher_key_hex: None,
            voucher_iv_hex: None,
            activation_bytes_hex: None,
            voucher_attempt_at: None,
        };
        let sc_path = crate::sidecar::sidecar_path_for(&aaxc_real);
        std::fs::write(&sc_path, serde_json::to_vec_pretty(&sc).expect("serialize"))
            .expect("write sidecar");

        let state = test_state(dir.path()).await;
        let report = super::scan(&state).await.expect("scan");
        assert_eq!(report.paths_rerooted, 1);
        assert_eq!(report.jobs_inserted, 1);

        let (db_m4b, db_aaxc): (String, String) = state
            .db
            .with(|c| {
                c.query_row(
                    "SELECT m4b_path, aaxc_path FROM jobs WHERE asin = 'ASIN' AND account_id = 'acct'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
            })
            .await
            .expect("query jobs");
        assert_eq!(db_m4b, m4b_real.display().to_string());
        assert_eq!(db_aaxc, aaxc_real.display().to_string());

        let rewritten: scribe_shared::Sidecar =
            serde_json::from_slice(&std::fs::read(&sc_path).expect("read sidecar"))
                .expect("parse sidecar");
        assert_eq!(rewritten.m4b_path, m4b_real.display().to_string());
        assert_eq!(rewritten.aaxc_path, aaxc_real.display().to_string());
    }

    #[test]
    fn identical_path_wins_when_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("book.m4b");
        std::fs::write(&file, b"x").expect("write");
        // `root` is irrelevant here — the recorded path itself exists.
        let root = dir.path().join("unused-root");
        assert_eq!(reroot(&file, &root), Some(file));
    }

    #[test]
    fn rerooted_when_the_old_prefix_differs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("books/A/T/T.m4b");
        std::fs::create_dir_all(real.parent().unwrap()).expect("mkdir");
        std::fs::write(&real, b"x").expect("write");
        let recorded = std::path::Path::new("/mnt/audiobooks/audible/books/A/T/T.m4b");
        assert_eq!(reroot(recorded, dir.path()), Some(real));
    }

    #[test]
    fn none_when_the_file_exists_nowhere() {
        let dir = tempfile::tempdir().expect("tempdir");
        let recorded = std::path::Path::new("/mnt/audiobooks/audible/books/A/T/T.m4b");
        assert_eq!(reroot(recorded, dir.path()), None);
    }

    #[test]
    fn a_directory_of_the_same_name_is_not_a_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("T.m4b")).expect("mkdir");
        let recorded = std::path::Path::new("/mnt/audiobooks/T.m4b");
        assert_eq!(reroot(recorded, dir.path()), None);
    }

    #[test]
    fn last_attempt_is_root_joined_with_the_bare_filename() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("T.m4b");
        std::fs::write(&real, b"x").expect("write");
        let recorded = std::path::Path::new("/mnt/audiobooks/audible/books/A/T/T.m4b");
        assert_eq!(reroot(recorded, dir.path()), Some(real));
    }
}
