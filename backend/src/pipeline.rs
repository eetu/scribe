//! End-to-end download pipeline.
//!
//! Pulls a book through the full chain:
//!   1. shim → voucher (or activation_bytes for AAX)
//!   2. press → submit job
//!   3. poll press status until ready/failed
//!   4. stream press → backup NAS (raw AAX/AAXC)
//!   5. stream press → library NAS (decrypted M4B)
//!   6. DELETE press job (cleans tmp on mini)
//!   7. notify ABS to rescan
//!
//! Filenaming uses a placeholder pattern in this skeleton; task #9 ports
//! OA's FileDestination canonicalization.

use std::path::PathBuf;
use std::time::Duration;

use uuid::Uuid;

use crate::error::AppError;
use crate::press::{notify_abs, Artifact, PressClient, PressDrm, PressJobReq};
use crate::shim::ShimClient;
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct PipelineInput {
    pub account_id: String,
    pub asin: String,
    /// For legacy AAX accounts/titles, the user-provided 8-hex activation_bytes.
    /// For AAXC titles, leave None — the shim's voucher endpoint supplies key+iv.
    pub activation_bytes_override: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PipelineOutcome {
    pub press_job_id: Uuid,
    pub aaxc_path: PathBuf,
    pub m4b_path: PathBuf,
    pub aaxc_bytes: u64,
    pub m4b_bytes: u64,
}

pub async fn run(state: &AppState, input: PipelineInput) -> Result<PipelineOutcome, AppError> {
    let shim = ShimClient::new(state);
    let press = PressClient::new(state);

    if !press.is_configured() {
        return Err(AppError::BadRequest("press not configured".into()));
    }

    // 1. resolve content URL + DRM
    let src = resolve_source(&shim, &input).await?;

    // 2. submit to press
    let job_req = PressJobReq {
        content_url: src.content_url,
        drm: src.drm,
        asin: input.asin.clone(),
        title: src.title.clone(),
        authors: src.authors.clone(),
        narrators: src.narrators.clone(),
        series_title: src.series_title.clone(),
        series_sequence: src.series_sequence.clone(),
        cover_url: src.cover_url.clone(),
    };
    let press_job_id = press.submit(&job_req).await?;
    tracing::info!(%press_job_id, asin = %input.asin, "press job submitted");

    // 3. poll until ready or failed
    poll_until_terminal(&press, press_job_id).await?;

    // 4 + 5. stream both artifacts to NAS using the configured naming templates.
    let naming_input = crate::filenaming::NamingInput {
        asin: &input.asin,
        title: &src.title,
        subtitle: src.subtitle.as_deref(),
        authors: &src.authors,
        narrators: &src.narrators,
        series_title: src.series_title.as_deref(),
        series_sequence: src.series_sequence.as_deref(),
        release_date: src.release_date.as_deref(),
    };
    let m4b_path = crate::filenaming::library_path(
        &state.cfg.library_dir,
        &state.cfg.naming.library,
        &naming_input,
    );
    let aaxc_path = crate::filenaming::original_path(
        &state.cfg.original_dir,
        &state.cfg.naming.original,
        &naming_input,
    );
    let aaxc_bytes = press
        .stream_to_file(press_job_id, Artifact::Aaxc, &aaxc_path)
        .await?;
    tracing::info!(%press_job_id, %aaxc_bytes, path = %aaxc_path.display(), "AAXC written");
    let m4b_bytes = press
        .stream_to_file(press_job_id, Artifact::M4b, &m4b_path)
        .await?;
    tracing::info!(%press_job_id, %m4b_bytes, path = %m4b_path.display(), "M4B written");

    // 6. cleanup
    if let Err(e) = press.delete(press_job_id).await {
        tracing::warn!(%press_job_id, error = ?e, "press DELETE failed (tmp will age out)");
    }

    // 7. notify ABS (non-fatal)
    let _ = notify_abs(state).await;

    Ok(PipelineOutcome {
        press_job_id,
        aaxc_path,
        m4b_path,
        aaxc_bytes,
        m4b_bytes,
    })
}

struct ResolvedSource {
    content_url: String,
    drm: PressDrm,
    title: String,
    authors: Vec<String>,
    series_title: Option<String>,
    series_sequence: Option<String>,
    cover_url: Option<String>,
    subtitle: Option<String>,
    narrators: Vec<String>,
    release_date: Option<String>,
}

async fn resolve_source(
    shim: &ShimClient<'_>,
    input: &PipelineInput,
) -> Result<ResolvedSource, AppError> {
    // AAX path: caller supplies activation_bytes + we'd resolve content_url via shim too
    // once shim grows an AAX download URL endpoint. For now, AAX-only flow goes through
    // a separate caller (reorg, manual upload) that already has a file:// URL.
    if let Some(ab) = &input.activation_bytes_override {
        return Err(AppError::BadRequest(format!(
            "AAX flow needs both activation_bytes ({ab}) and a content_url — not wired through shim yet"
        )));
    }

    let voucher = shim.voucher(&input.account_id, &input.asin).await?;
    // Look up cover + minimal meta from library. Caller could pass these but
    // re-querying keeps callers simple and the load is one row.
    let lib = shim.library(&input.account_id, 1, 1000, None).await?;
    let book = lib
        .items
        .into_iter()
        .find(|b| b.asin == input.asin)
        .ok_or(AppError::NotFound)?;

    let drm = PressDrm::Aaxc {
        key_hex: voucher.key,
        iv_hex: voucher.iv,
    };
    let series_title = book.series.first().and_then(|s| s.title.clone());
    let series_sequence = book.series.first().and_then(|s| s.sequence.clone());

    Ok(ResolvedSource {
        content_url: voucher.content_url,
        drm,
        title: book.title,
        authors: book.authors,
        series_title,
        series_sequence,
        cover_url: voucher.cover_url.or(book.cover_url),
        subtitle: book.subtitle,
        narrators: book.narrators,
        release_date: book.release_date,
    })
}

async fn poll_until_terminal(press: &PressClient<'_>, job_id: Uuid) -> Result<(), AppError> {
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
                tracing::debug!(%job_id, phase = %s.phase, aaxc = s.aaxc_bytes, "polling press");
                tokio::time::sleep(sleep).await;
                // Exponential-ish backoff capped at 5s — books take minutes,
                // we don't need millisecond polling once the job is in flight.
                sleep = (sleep * 2).min(Duration::from_secs(5));
            }
        }
    }
}

