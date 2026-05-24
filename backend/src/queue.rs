//! Job queue: persisted in SQLite, executed via tokio workers, observable via SSE.
//!
//! Lifecycle:
//!
//! ```text
//! queued -> fetching_voucher -> downloading -> converting -> writing_nas -> done
//!                                                                           \-> failed
//!                                                                           \-> cancelled
//! ```
//!
//! Concurrency = `SCRIBE_JOB_CONCURRENCY` (default 1 — Pi-friendly). Each
//! worker pulls one job from an mpsc queue, drives it through `pipeline::run`,
//! and emits `JobEvent`s on a per-job broadcast channel.
//!
//! Restart resume: at boot, any job whose status is not in (done, failed,
//! cancelled) is reset to `queued` and re-pushed onto the channel. This
//! covers the case where the host restarts mid-conversion — the press
//! worker keeps its tmp until DELETE, but we just start fresh.
//!
//! Retry: each job has a `retry_count` column. Transient failures (upstream
//! errors, timeouts) bump it and re-queue with backoff. Hard failures
//! (404, bad input) surface immediately.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, Mutex};
use uuid::Uuid;

use crate::error::AppError;
use crate::pipeline::{self, PipelineInput};
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Queued,
    FetchingVoucher,
    Downloading,
    Converting,
    WritingNas,
    Done,
    Failed,
    Cancelled,
}

impl Lifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Lifecycle::Queued => "queued",
            Lifecycle::FetchingVoucher => "fetching_voucher",
            Lifecycle::Downloading => "downloading",
            Lifecycle::Converting => "converting",
            Lifecycle::WritingNas => "writing_nas",
            Lifecycle::Done => "done",
            Lifecycle::Failed => "failed",
            Lifecycle::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueueEvent {
    Phase {
        phase: String,
        retry_count: u32,
    },
    /// Live progress while the press worker is busy. `bytes_total` is
    /// `None` when the upstream didn't advertise Content-Length (rare on
    /// Audible's CDN but defensive). Frontend renders a precise progress
    /// bar when total is known, indeterminate animation otherwise.
    Progress {
        phase: String,
        bytes_done: u64,
        bytes_total: Option<u64>,
    },
    Done {
        m4b_path: String,
        aaxc_path: String,
    },
    Failed {
        message: String,
    },
    Cancelled,
}

#[derive(Clone)]
pub struct Queue {
    inner: Arc<Inner>,
}

struct Inner {
    state: AppState,
    work_tx: mpsc::Sender<Uuid>,
    /// Per-job SSE broadcast channels. Reused across worker restarts within
    /// a single boot — restart-resume reaches into the DB, not memory.
    channels: Mutex<HashMap<Uuid, broadcast::Sender<QueueEvent>>>,
    /// Cancellation handles: setting to true via cancel() makes the worker
    /// abort at the next phase boundary.
    cancel_flags: Mutex<HashMap<Uuid, Arc<std::sync::atomic::AtomicBool>>>,
}

impl Queue {
    pub fn new(state: AppState) -> Self {
        let (work_tx, work_rx) = mpsc::channel::<Uuid>(256);
        let inner = Arc::new(Inner {
            state: state.clone(),
            work_tx,
            channels: Mutex::new(HashMap::new()),
            cancel_flags: Mutex::new(HashMap::new()),
        });
        // Spawn N workers.
        let concurrency = state.cfg.job_concurrency.max(1);
        let shared_rx = Arc::new(Mutex::new(work_rx));
        for worker_id in 0..concurrency {
            let inner = inner.clone();
            let rx = shared_rx.clone();
            tokio::spawn(async move {
                tracing::info!(worker_id, "queue worker started");
                loop {
                    let job_id = match rx.lock().await.recv().await {
                        Some(id) => id,
                        None => break,
                    };
                    if let Err(e) = inner.run_job(job_id).await {
                        tracing::error!(%job_id, error = ?e, "worker errored");
                    }
                    // Inter-job sleep to space out voucher fetches + CDN
                    // downloads. Makes a 200-book first-deploy backlog look
                    // closer to a human pace than a scraper burst.
                    let base = inner.state.cfg.job_interjob_delay_s;
                    if base > 0 {
                        let jitter_pct = inner.state.cfg.job_interjob_jitter_percent.min(95) as f64 / 100.0;
                        let factor = {
                            use rand::Rng;
                            1.0 + rand::rng().random_range(-jitter_pct..=jitter_pct)
                        };
                        let secs = ((base as f64) * factor).max(1.0) as u64;
                        tracing::debug!(worker_id, sleep_s = secs, "inter-job pacing");
                        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                    }
                }
            });
        }
        Self { inner }
    }

    /// Enqueue every Active book in `account_id` that doesn't already have
    /// a job row. Returns the list of newly-created job ids. Failed jobs
    /// stay parked — the user retries them by hand from the UI to avoid
    /// auto-retry storms on a single bad book.
    pub async fn enqueue_pending(&self, account_id: &str) -> Result<Vec<Uuid>, AppError> {
        let acct = account_id.to_string();
        let asins: Vec<String> = self
            .inner
            .state
            .db
            .with(move |c| {
                let mut stmt = c.prepare(
                    "SELECT b.asin FROM books b
                     LEFT JOIN jobs j ON j.asin = b.asin AND j.account_id = b.account_id
                     WHERE b.account_id = ?1
                       AND b.status = 'Active'
                       AND j.id IS NULL",
                )?;
                let v = stmt
                    .query_map([acct], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(v)
            })
            .await?;
        let mut ids = Vec::with_capacity(asins.len());
        for asin in asins {
            match self.enqueue(account_id, &asin).await {
                Ok(id) => ids.push(id),
                Err(e) => tracing::warn!(asin = %asin, error = ?e, "enqueue failed"),
            }
        }
        Ok(ids)
    }

    pub async fn enqueue(&self, account_id: &str, asin: &str) -> Result<Uuid, AppError> {
        let job_id = Uuid::new_v4();
        let now = Utc::now().timestamp();
        let aid = account_id.to_string();
        let a = asin.to_string();
        let id = job_id;
        self.inner
            .state
            .db
            .with(move |c| {
                c.execute(
                    "INSERT INTO jobs (id, asin, account_id, status, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'queued', ?4, ?4)",
                    rusqlite::params![id.to_string(), a, aid, now],
                )?;
                Ok(())
            })
            .await?;
        // best-effort send; if the queue is full, the next tick will pick it up
        // because we re-scan on startup.
        let _ = self.inner.work_tx.send(job_id).await;
        Ok(job_id)
    }

    pub async fn cancel(&self, job_id: Uuid) -> Result<bool, AppError> {
        let flags = self.inner.cancel_flags.lock().await;
        if let Some(flag) = flags.get(&job_id) {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            // Status update happens inside the worker when it notices.
            return Ok(true);
        }
        // Not in-flight — flip the DB row directly if it's queued.
        let id = job_id;
        let updated: i64 = self
            .inner
            .state
            .db
            .with(move |c| {
                let now = Utc::now().timestamp();
                let n = c.execute(
                    "UPDATE jobs SET status = 'cancelled', updated_at = ?1
                     WHERE id = ?2 AND status = 'queued'",
                    rusqlite::params![now, id.to_string()],
                )?;
                Ok(n as i64)
            })
            .await?;
        Ok(updated > 0)
    }

    pub async fn subscribe(&self, job_id: Uuid) -> Option<broadcast::Receiver<QueueEvent>> {
        let mut channels = self.inner.channels.lock().await;
        let tx = channels.entry(job_id).or_insert_with(|| broadcast::channel(32).0);
        Some(tx.subscribe())
    }

    /// Re-queue any non-terminal jobs after a restart. Call once at boot,
    /// after state is constructed and workers are spawned.
    pub async fn resume_pending(&self) -> Result<(), AppError> {
        let rows = self
            .inner
            .state
            .db
            .with(|c| {
                let mut stmt = c.prepare(
                    "SELECT id FROM jobs
                     WHERE status NOT IN ('done', 'failed', 'cancelled')",
                )?;
                let v = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(v)
            })
            .await?;
        for raw in rows {
            if let Ok(id) = Uuid::parse_str(&raw) {
                let now = Utc::now().timestamp();
                let id_s = id.to_string();
                self.inner
                    .state
                    .db
                    .with(move |c| {
                        c.execute(
                            "UPDATE jobs SET status='queued', updated_at=?1 WHERE id=?2",
                            rusqlite::params![now, id_s],
                        )?;
                        Ok(())
                    })
                    .await?;
                let _ = self.inner.work_tx.send(id).await;
                tracing::info!(%id, "resumed pending job");
            }
        }
        Ok(())
    }
}

impl Inner {
    async fn channel(&self, job_id: Uuid) -> broadcast::Sender<QueueEvent> {
        let mut channels = self.channels.lock().await;
        channels
            .entry(job_id)
            .or_insert_with(|| broadcast::channel(32).0)
            .clone()
    }

    async fn cancel_flag(&self, job_id: Uuid) -> Arc<std::sync::atomic::AtomicBool> {
        let mut guard = self.cancel_flags.lock().await;
        let flag = guard
            .entry(job_id)
            .or_insert_with(|| Arc::new(std::sync::atomic::AtomicBool::new(false)));
        flag.clone()
    }

    async fn set_phase(&self, job_id: Uuid, phase: Lifecycle, retry_count: u32) -> Result<(), AppError> {
        let id_s = job_id.to_string();
        let phase_s = phase.as_str().to_string();
        let now = Utc::now().timestamp();
        self.state
            .db
            .with(move |c| {
                c.execute(
                    "UPDATE jobs SET status = ?1, updated_at = ?2 WHERE id = ?3",
                    rusqlite::params![phase_s, now, id_s],
                )?;
                Ok(())
            })
            .await?;
        let tx = self.channel(job_id).await;
        let _ = tx.send(QueueEvent::Phase {
            phase: phase.as_str().to_string(),
            retry_count,
        });
        Ok(())
    }

    async fn job_meta(&self, job_id: Uuid) -> Result<(String, String), AppError> {
        let id_s = job_id.to_string();
        let row = self
            .state
            .db
            .with(move |c| {
                c.query_row(
                    "SELECT asin, account_id FROM jobs WHERE id = ?1",
                    rusqlite::params![id_s],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
            })
            .await?;
        Ok(row)
    }

    async fn save_outcome_done(&self, job_id: Uuid, aaxc: &str, m4b: &str) -> Result<(), AppError> {
        let id_s = job_id.to_string();
        let aaxc_s = aaxc.to_string();
        let m4b_s = m4b.to_string();
        let now = Utc::now().timestamp();
        self.state
            .db
            .with(move |c| {
                c.execute(
                    "UPDATE jobs SET status='done', updated_at=?1, m4b_path=?2, aaxc_path=?3, error=NULL WHERE id=?4",
                    rusqlite::params![now, m4b_s, aaxc_s, id_s],
                )?;
                Ok(())
            })
            .await?;
        let tx = self.channel(job_id).await;
        let _ = tx.send(QueueEvent::Done {
            m4b_path: m4b.to_string(),
            aaxc_path: aaxc.to_string(),
        });
        Ok(())
    }

    async fn save_failure(&self, job_id: Uuid, msg: &str) -> Result<(), AppError> {
        let id_s = job_id.to_string();
        let msg_s = msg.to_string();
        let now = Utc::now().timestamp();
        self.state
            .db
            .with(move |c| {
                c.execute(
                    "UPDATE jobs SET status='failed', updated_at=?1, error=?2 WHERE id=?3",
                    rusqlite::params![now, msg_s, id_s],
                )?;
                Ok(())
            })
            .await?;
        let tx = self.channel(job_id).await;
        let _ = tx.send(QueueEvent::Failed { message: msg.to_string() });
        Ok(())
    }

    async fn save_cancelled(&self, job_id: Uuid) {
        let id_s = job_id.to_string();
        let now = Utc::now().timestamp();
        let _ = self
            .state
            .db
            .with(move |c| {
                c.execute(
                    "UPDATE jobs SET status='cancelled', updated_at=?1 WHERE id=?2",
                    rusqlite::params![now, id_s],
                )?;
                Ok(())
            })
            .await;
        let tx = self.channel(job_id).await;
        let _ = tx.send(QueueEvent::Cancelled);
    }

    async fn run_job(self: &Arc<Self>, job_id: Uuid) -> Result<(), AppError> {
        let flag = self.cancel_flag(job_id).await;
        if flag.load(std::sync::atomic::Ordering::SeqCst) {
            self.save_cancelled(job_id).await;
            return Ok(());
        }

        let (asin, account_id) = self.job_meta(job_id).await?;
        self.set_phase(job_id, Lifecycle::FetchingVoucher, 0).await?;

        let max_retries = self.state.cfg.job_retry_max;
        let mut attempt: u32 = 0;
        loop {
            // Pre-attempt cancellation check.
            if flag.load(std::sync::atomic::Ordering::SeqCst) {
                self.save_cancelled(job_id).await;
                return Ok(());
            }

            self.set_phase(job_id, Lifecycle::Downloading, attempt).await?;
            let input = PipelineInput {
                account_id: account_id.clone(),
                asin: asin.clone(),
                activation_bytes_override: None,
            };

            // Hand the per-job broadcast tx into the pipeline so press's
            // polled status surfaces as live Progress events on the same
            // SSE stream the frontend is already consuming.
            let progress_tx = self.channel(job_id).await;
            match pipeline::run(&self.state, input, Some(progress_tx)).await {
                Ok(out) => {
                    self.set_phase(job_id, Lifecycle::WritingNas, attempt).await?;
                    let m4b = out.m4b_path.display().to_string();
                    let aaxc = out.aaxc_path.display().to_string();
                    self.save_outcome_done(job_id, &aaxc, &m4b).await?;
                    tracing::info!(%job_id, "job complete");
                    return Ok(());
                }
                Err(e) => {
                    let transient = matches!(&e, AppError::Upstream(_) | AppError::Internal(_));
                    if transient && attempt + 1 < max_retries {
                        attempt += 1;
                        let backoff = Duration::from_secs((1u64 << attempt.min(5)) * 5);
                        tracing::warn!(
                            %job_id,
                            attempt,
                            wait_s = backoff.as_secs(),
                            error = ?e,
                            "transient failure, retrying"
                        );
                        // Park while waiting; honour cancel.
                        tokio::select! {
                            _ = tokio::time::sleep(backoff) => {},
                            _ = wait_for_cancel(&flag) => {
                                self.save_cancelled(job_id).await;
                                return Ok(());
                            }
                        }
                        continue;
                    }
                    self.save_failure(job_id, &e.to_string()).await?;
                    return Ok(());
                }
            }
        }
    }
}

async fn wait_for_cancel(flag: &Arc<std::sync::atomic::AtomicBool>) {
    loop {
        if flag.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
