//! Reconvert pipeline — rebuild an M4B from a locally-stored AAXC.
//!
//! Triggered when a user deletes / loses an m4b but the encrypted
//! source is still on the NAS. We skip the Audible CDN entirely:
//!   1. Load the job + sidecar (asin, account_id, aaxc_path, voucher)
//!   2. If the sidecar lacks a voucher, fetch one live from shim. Plus
//!      revocations surface here as `LicenseDenied` and abort fast.
//!   3. Mint a one-shot token mapping → aaxc_path, register in
//!      `AppState::aaxc_tokens`
//!   4. Submit a normal press job with `content_url` =
//!      `<SCRIBE_INTERNAL_URL>/internal/aaxc/<token>` — press fetches
//!      that like any CDN URL, no press-side code change needed
//!   5. Poll press, stream the resulting m4b back into the existing
//!      canonical path
//!   6. Revoke the token, press DELETE, notify ABS
//!
//! Reuses the queue's per-job broadcast channel so existing SSE
//! subscribers see phase/progress events on the same job_id they were
//! already watching.

use std::path::PathBuf;
use std::time::Duration;

use uuid::Uuid;

use crate::error::AppError;
use crate::press::{notify_abs, Artifact, PressClient, PressDrm, PressJobReq};
use crate::queue::Lifecycle;
use crate::sidecar;
use crate::state::AppState;

/// Kicks off a reconvert in a background tokio task and returns. The
/// queue's per-job channel + DB rows are the only state surface — the
/// frontend sees normal phase/progress events on the existing
/// `/api/jobs/{id}/sse` subscription.
pub async fn kick_off(state: AppState, job_id: Uuid) -> Result<(), AppError> {
    let cfg_url = state
        .cfg
        .internal_url
        .clone()
        .ok_or_else(|| AppError::BadRequest("SCRIBE_INTERNAL_URL not configured".into()))?;
    if !PressClient::new(&state).is_configured() {
        return Err(AppError::BadRequest("press not configured".into()));
    }

    let (asin, account_id, aaxc_path, prior_status) = job_meta(&state, job_id).await?;
    if prior_status == "queued"
        || prior_status == "fetching_voucher"
        || prior_status == "downloading"
        || prior_status == "converting"
        || prior_status == "streaming"
    {
        return Err(AppError::BadRequest(
            "job already in progress — cancel before reconvert".into(),
        ));
    }
    let aaxc_path =
        aaxc_path.ok_or_else(|| AppError::BadRequest("job has no aaxc_path on file".into()))?;
    if !std::path::Path::new(&aaxc_path).is_file() {
        return Err(AppError::BadRequest(
            "stored aaxc file is missing — reconvert needs the encrypted source".into(),
        ));
    }

    tokio::spawn(async move {
        if let Err(e) = run(&state, job_id, &asin, &account_id, &aaxc_path, &cfg_url).await {
            tracing::warn!(%job_id, error = ?e, "reconvert failed");
            state.queue().save_failure(job_id, &e.to_string()).await;
        }
    });
    Ok(())
}

async fn run(
    state: &AppState,
    job_id: Uuid,
    asin: &str,
    account_id: &str,
    aaxc_path: &str,
    internal_url: &str,
) -> Result<(), AppError> {
    state
        .queue()
        .set_phase(job_id, Lifecycle::FetchingVoucher, 0)
        .await?;

    // Read the sidecar to pull stored voucher / activation bytes +
    // metadata. Falls back to a live shim fetch if the sidecar pre-dates
    // voucher persistence — that path also surfaces a LicenseDenied
    // immediately for Plus-revoked titles instead of starting press for
    // a job that can't finish.
    let drm = resolve_drm(state, account_id, asin, aaxc_path).await?;
    let (title, authors, narrators, series_title, series_sequence) =
        book_meta(state, account_id, asin).await?;

    // Mint a one-shot token mapping to the AAXC path. Anyone on the LAN
    // who guesses the UUID could fetch the encrypted file, but without
    // the matching voucher (which lives only in the press job body)
    // they get unplayable bytes.
    let token = Uuid::new_v4().to_string();
    state
        .aaxc_tokens
        .insert(token.clone(), PathBuf::from(aaxc_path))
        .await;
    let cleanup = TokenGuard {
        store: state.aaxc_tokens.clone(),
        token: token.clone(),
    };
    let content_url = format!(
        "{}/internal/aaxc/{}",
        internal_url.trim_end_matches('/'),
        token
    );

    let press = PressClient::new(state);
    let job_req = PressJobReq {
        content_url,
        drm,
        asin: asin.to_string(),
        title,
        authors,
        narrators,
        series_title,
        series_sequence,
        cover_url: None,
    };
    state
        .queue()
        .set_phase(job_id, Lifecycle::Downloading, 0)
        .await?;
    let press_job_id = press.submit(&job_req).await?;
    tracing::info!(%press_job_id, %asin, "reconvert press job submitted");

    // Byte-level progress during reconvert would require a public
    // broadcaster on Queue; for v1 we lean on the coarse Phase
    // transitions (Downloading → Streaming → Done) — enough to drive
    // the UI chip, just no live byte counter.
    poll_until_ready(&press, press_job_id).await?;

    // Stream the m4b back into the existing canonical path. Reuse
    // PressClient::stream_to_file which already handles partial-file
    // atomic rename + progress emission.
    state
        .queue()
        .set_phase(job_id, Lifecycle::Streaming, 0)
        .await?;
    let m4b_path = current_m4b_path(state, job_id).await?;
    let dest = std::path::Path::new(&m4b_path);
    press
        .stream_to_file(press_job_id, Artifact::M4b, dest, None)
        .await?;

    // Cleanup. Token revocation happens automatically via TokenGuard
    // drop.
    if let Err(e) = press.delete(press_job_id).await {
        tracing::warn!(%press_job_id, error = ?e, "press DELETE failed after reconvert (tmp will age out)");
    }
    let _ = notify_abs(state).await;
    state
        .queue()
        .save_outcome_done(job_id, aaxc_path, &m4b_path)
        .await?;
    drop(cleanup);
    tracing::info!(%job_id, "reconvert complete");
    Ok(())
}

async fn resolve_drm(
    state: &AppState,
    account_id: &str,
    asin: &str,
    aaxc_path: &str,
) -> Result<PressDrm, AppError> {
    let sc_path = sidecar::sidecar_path_for(std::path::Path::new(aaxc_path));
    let cached = sidecar::read(&sc_path).await.ok();

    if let Some(sc) = &cached {
        if let (Some(k), Some(v)) = (&sc.voucher_key_hex, &sc.voucher_iv_hex) {
            return Ok(PressDrm::Aaxc {
                key_hex: k.clone(),
                iv_hex: v.clone(),
            });
        }
        if let Some(ab) = &sc.activation_bytes_hex {
            return Ok(PressDrm::Aax {
                activation_bytes: ab.clone(),
            });
        }
    }

    // Sidecar missing or pre-voucher-persistence — fall back to live
    // shim fetch. Persist what we get for next time.
    let shim = crate::shim::ShimClient::new(state);
    let voucher = shim.voucher(account_id, asin).await?;
    if let Some(mut sc) = cached {
        sc.voucher_key_hex = Some(voucher.key.clone());
        sc.voucher_iv_hex = Some(voucher.iv.clone());
        sc.voucher_attempt_at = None;
        let _ = sidecar::write(std::path::Path::new(aaxc_path), &sc).await;
    }
    Ok(PressDrm::Aaxc {
        key_hex: voucher.key,
        iv_hex: voucher.iv,
    })
}

/// Title, authors, narrators, series_title, series_sequence — the
/// metadata press needs to write the right id3/ffmpeg tags during
/// reconvert. Pulled from the books table since the sidecar only
/// has the title.
type BookMeta = (
    String,
    Vec<String>,
    Vec<String>,
    Option<String>,
    Option<String>,
);
type BookRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

async fn book_meta(state: &AppState, account_id: &str, asin: &str) -> Result<BookMeta, AppError> {
    let asin = asin.to_string();
    let account_id = account_id.to_string();
    let row: Option<BookRow> = state
        .db
        .with(move |c| {
            c.query_row(
                "SELECT title, authors_json, narrators_json, series_title, series_sequence
                 FROM books WHERE asin = ?1 AND account_id = ?2",
                rusqlite::params![asin, account_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .map(Some)
            .or_else(|e| {
                if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                    Ok(None)
                } else {
                    Err(e)
                }
            })
        })
        .await?;
    let (title, authors_json, narrators_json, series_title, series_sequence) =
        row.ok_or(AppError::NotFound)?;
    let authors: Vec<String> = serde_json::from_str(&authors_json).unwrap_or_default();
    let narrators: Vec<String> = serde_json::from_str(&narrators_json).unwrap_or_default();
    Ok((title, authors, narrators, series_title, series_sequence))
}

async fn job_meta(
    state: &AppState,
    job_id: Uuid,
) -> Result<(String, String, Option<String>, String), AppError> {
    let jid = job_id.to_string();
    let row: Option<(String, String, Option<String>, String)> = state
        .db
        .with(move |c| {
            c.query_row(
                "SELECT asin, account_id, aaxc_path, status FROM jobs WHERE id = ?1",
                rusqlite::params![jid],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .map(Some)
            .or_else(|e| {
                if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                    Ok(None)
                } else {
                    Err(e)
                }
            })
        })
        .await?;
    row.ok_or(AppError::NotFound)
}

async fn current_m4b_path(state: &AppState, job_id: Uuid) -> Result<String, AppError> {
    let jid = job_id.to_string();
    let row: Option<Option<String>> = state
        .db
        .with(move |c| {
            c.query_row(
                "SELECT m4b_path FROM jobs WHERE id = ?1",
                rusqlite::params![jid],
                |r| r.get::<_, Option<String>>(0),
            )
            .map(Some)
            .or_else(|e| {
                if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                    Ok(None)
                } else {
                    Err(e)
                }
            })
        })
        .await?;
    row.flatten()
        .ok_or_else(|| AppError::BadRequest("job has no m4b_path".into()))
}

async fn poll_until_ready(press: &PressClient<'_>, job_id: Uuid) -> Result<(), AppError> {
    let mut sleep = Duration::from_millis(500);
    loop {
        let s = press.status(job_id).await?;
        match s.phase.as_str() {
            "ready" => return Ok(()),
            "failed" => {
                return Err(AppError::Upstream(
                    s.error.unwrap_or_else(|| "press job failed".into()),
                ))
            }
            _ => {
                tracing::debug!(%job_id, phase = %s.phase, "reconvert polling press");
                tokio::time::sleep(sleep).await;
                sleep = (sleep * 2).min(Duration::from_secs(5));
            }
        }
    }
}

/// RAII guard that revokes the one-shot AAXC token when the surrounding
/// reconvert task drops, whether by success or error.
struct TokenGuard {
    store: crate::state::AaxcTokenStore,
    token: String,
}

impl Drop for TokenGuard {
    fn drop(&mut self) {
        let store = self.store.clone();
        let token = std::mem::take(&mut self.token);
        tokio::spawn(async move {
            store.revoke(&token).await;
        });
    }
}

