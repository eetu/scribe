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

pub async fn run(
    state: &AppState,
    input: PipelineInput,
    progress_tx: Option<tokio::sync::broadcast::Sender<crate::queue::QueueEvent>>,
) -> Result<PipelineOutcome, AppError> {
    let shim = ShimClient::new(state);
    let press = PressClient::new(state);

    if !press.is_configured() {
        return Err(AppError::BadRequest("press not configured".into()));
    }

    // 1. resolve content URL + DRM
    let src = resolve_source(&shim, &input).await?;

    // Cache the voucher / activation bytes before they move into the
    // press job request — needed later for the sidecar so a future
    // reconvert can decrypt the local AAXC without re-fetching from
    // Audible (Plus revocations etc).
    let (voucher_key_hex, voucher_iv_hex, activation_bytes_hex) = match &src.drm {
        PressDrm::Aaxc { key_hex, iv_hex } => {
            (Some(key_hex.clone()), Some(iv_hex.clone()), None)
        }
        PressDrm::Aax { activation_bytes } => (None, None, Some(activation_bytes.clone())),
    };

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
    poll_until_terminal(&press, press_job_id, progress_tx.as_ref()).await?;

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
    let canonical_m4b = crate::filenaming::library_path(
        &state.cfg.library_dir,
        &state.cfg.naming.library,
        &naming_input,
    );
    let aaxc_path = crate::filenaming::original_path(
        &state.cfg.original_dir,
        &state.cfg.naming.original,
        &naming_input,
    );
    // Collision check: if the canonical path is taken by a previous download
    // of a *different* ASIN (different region of the same title, different
    // edition, etc.), suffix the new file with " (asin)" so both survive on
    // disk. Re-downloads of the same ASIN overwrite in place.
    let m4b_path = resolve_unique_m4b_path(canonical_m4b, &input.asin, &aaxc_path).await;
    // Streaming the artifacts off press is the slowest non-press phase
    // (~300 MB for a normal book over a Pi-side network mount). Pipe
    // Progress events through so the UI doesn't sit on the last
    // "converting" sample for minutes and the chip flips to "streaming".
    let aaxc_bytes = press
        .stream_to_file(
            press_job_id,
            Artifact::Aaxc,
            &aaxc_path,
            progress_tx.as_ref().map(|tx| (tx, "streaming")),
        )
        .await?;
    tracing::info!(%press_job_id, %aaxc_bytes, path = %aaxc_path.display(), "AAXC written");
    let m4b_bytes = press
        .stream_to_file(
            press_job_id,
            Artifact::M4b,
            &m4b_path,
            progress_tx.as_ref().map(|tx| (tx, "streaming")),
        )
        .await?;
    tracing::info!(%press_job_id, %m4b_bytes, path = %m4b_path.display(), "M4B written");

    // 6. write `.scribe.json` sidecar next to the AAXC — survives a DB wipe.
    // Persisted voucher/activation bytes (cached pre-move above) make a
    // future reconvert work even if Audible later revokes the title's
    // license.
    let sc = scribe_shared::Sidecar {
        asin: input.asin.clone(),
        account_id: input.account_id.clone(),
        title: src.title.clone(),
        downloaded_at: chrono::Utc::now().timestamp(),
        m4b_path: m4b_path.display().to_string(),
        aaxc_path: aaxc_path.display().to_string(),
        voucher_refresh_date: None,
        customer_name: None,
        scribe_version: env!("CARGO_PKG_VERSION").into(),
        voucher_key_hex,
        voucher_iv_hex,
        activation_bytes_hex,
        voucher_attempt_at: None,
    };
    if let Err(e) = crate::sidecar::write(&aaxc_path, &sc).await {
        tracing::warn!(asin = %input.asin, error = ?e, "sidecar write failed");
    }

    // 7. cleanup
    if let Err(e) = press.delete(press_job_id).await {
        tracing::warn!(%press_job_id, error = ?e, "press DELETE failed (tmp will age out)");
    }

    // 8. notify ABS (non-fatal)
    let _ = notify_abs(state).await;

    Ok(PipelineOutcome {
        press_job_id,
        aaxc_path,
        m4b_path,
        aaxc_bytes,
        m4b_bytes,
    })
}

/// If `canonical` is free, return it. Else read the existing scribe sidecar
/// for the path's expected counterpart (sibling `.aaxc.scribe.json`) and
/// compare ASINs:
///   - matching ASIN → same book, allow overwrite (re-download flow).
///   - mismatching ASIN or unknown ownership → suffix the m4b with the
///     new ASIN so both editions live side by side.
async fn resolve_unique_m4b_path(
    canonical: std::path::PathBuf,
    asin: &str,
    aaxc_path: &std::path::Path,
) -> std::path::PathBuf {
    if !tokio::fs::try_exists(&canonical).await.unwrap_or(false) {
        return canonical;
    }
    // Existing file. Check whether we own it under the same ASIN.
    let sidecar_path = aaxc_path.with_extension({
        let ext = aaxc_path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("aaxc");
        format!("{ext}.scribe.json")
    });
    if let Ok(raw) = tokio::fs::read_to_string(&sidecar_path).await {
        if let Ok(sc) = serde_json::from_str::<scribe_shared::Sidecar>(&raw) {
            if sc.asin == asin {
                return canonical;
            }
        }
    }
    // Append " (asin)" to stem.
    let stem = canonical
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let ext = canonical
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("m4b");
    let parent = canonical
        .parent()
        .unwrap_or(std::path::Path::new(""));
    parent.join(format!("{stem} ({asin}).{ext}"))
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

async fn poll_until_terminal(
    press: &PressClient<'_>,
    job_id: Uuid,
    progress_tx: Option<&tokio::sync::broadcast::Sender<crate::queue::QueueEvent>>,
) -> Result<(), AppError> {
    let mut sleep = Duration::from_millis(500);
    loop {
        let s = press.status(job_id).await?;
        // Fan out a Progress event on every poll regardless of phase so
        // the UI sees the byte counter advance during both downloading
        // and converting. Listeners on the broadcast channel may have
        // come and gone — `send` errors when there are zero, which is
        // fine; we just drop the event.
        if let Some(tx) = progress_tx {
            let bytes_done = match s.phase.as_str() {
                "converting" | "ready" => s.m4b_bytes,
                _ => s.aaxc_bytes,
            };
            // Press doesn't know the M4B output size mid-convert. Use the
            // AAXC input size as an approximation — lossless remux keeps
            // output ≈ input within a few percent, which is close enough
            // for a UI progress bar. The chip label is what tells the
            // user which phase we're actually in.
            let bytes_total = s.aaxc_bytes_total;
            let _ = tx.send(crate::queue::QueueEvent::Progress {
                phase: s.phase.clone(),
                bytes_done,
                bytes_total,
            });
        }
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

