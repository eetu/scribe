//! Download + ffmpeg orchestration.
//!
//! Supports both DRM formats:
//!   * AAXC — per-book key/iv via `-audible_key` + `-audible_iv`
//!   * AAX  — account-wide activation_bytes via `-activation_bytes`
//!
//! `content_url` accepts `https://` (Audible CDN) or `file://` (local file,
//! useful for testing against an OpenAudible backlog without going through
//! the CDN). The file:// path side-steps download entirely — the file is
//! `tokio::fs::copy`'d into the job dir, identical bytes.
//!
//! Chapter embedding lives behind a TODO — task #6 generates an ffmetadata
//! file and threads `-i metadata.txt -map_metadata 1`.

use std::process::Stdio;
use std::sync::Arc;

use scribe_shared::JobEvent;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::jobs::{Drm, JobReq, JobState, Phase};

pub async fn run(state: Arc<Mutex<JobState>>, ffmpeg_bin: String) -> anyhow::Result<()> {
    let (id, req, aaxc_path, m4b_path) = {
        let s = state.lock().await;
        (s.id, s.req.clone(), s.aaxc_path(), s.m4b_path())
    };

    set_phase(&state, Phase::Downloading).await;
    let bytes = fetch(&state, &req.content_url, &aaxc_path).await?;
    tracing::info!(%id, %bytes, "fetched input");

    set_phase(&state, Phase::Converting).await;
    run_ffmpeg(&state, &ffmpeg_bin, &aaxc_path, &m4b_path, &req).await?;

    // AAX post-process: ffmpeg -c copy bakes a single-entry stts that
    // overstates the trailing AAC sample's duration (every frame
    // declared as 1024 samples even though AAX's last frame is
    // shorter). AVFoundation rejects on play. Patch mdhd/tkhd/elst
    // durations + stts in place using the source AAX's true sample
    // count. AAXC files aren't affected.
    if matches!(req.drm, Drm::Aax { .. }) {
        let aax_for_patch = aaxc_path.clone();
        let m4b_for_patch = m4b_path.clone();
        let patch_result = tokio::task::spawn_blocking(move || {
            crate::mp4patch::fix_aax_durations(&aax_for_patch, &m4b_for_patch)
        })
        .await?;
        if let Err(e) = patch_result {
            tracing::warn!(%id, error = ?e, "mp4patch failed — m4b kept as-is, may not play on AVFoundation");
        }
    }

    let m4b_bytes = tokio::fs::metadata(&m4b_path).await?.len();
    {
        let mut s = state.lock().await;
        s.m4b_bytes = m4b_bytes;
        s.phase = Phase::Ready;
        let _ = s.events.send(JobEvent::Ready);
    }
    tracing::info!(%id, %m4b_bytes, "convert complete");
    Ok(())
}

async fn set_phase(state: &Arc<Mutex<JobState>>, phase: Phase) {
    let mut s = state.lock().await;
    s.phase = phase;
}

async fn fetch(state: &Arc<Mutex<JobState>>, url: &str, dest: &std::path::Path) -> anyhow::Result<u64> {
    if let Some(local) = url.strip_prefix("file://") {
        return copy_local(state, std::path::Path::new(local), dest).await;
    }
    download_http(state, url, dest).await
}

async fn copy_local(
    state: &Arc<Mutex<JobState>>,
    src: &std::path::Path,
    dest: &std::path::Path,
) -> anyhow::Result<u64> {
    let total = tokio::fs::metadata(src).await?.len();
    let bytes = tokio::fs::copy(src, dest).await?;
    {
        let mut s = state.lock().await;
        s.aaxc_bytes = bytes;
        s.aaxc_bytes_total = Some(total);
        let _ = s.events.send(JobEvent::Downloading {
            bytes_done: bytes,
            bytes_total: Some(total),
        });
    }
    Ok(bytes)
}

async fn download_http(state: &Arc<Mutex<JobState>>, url: &str, dest: &std::path::Path) -> anyhow::Result<u64> {
    let client = reqwest::Client::builder()
        .user_agent("Audible/671 CFNetwork/1240.0.4 Darwin/20.6.0")
        .build()?;
    // Audible's CDN sometimes returns chunked responses without a
    // Content-Length header, which leaves the UI without a total and the
    // progress bar at 0%. Probe with a single-byte Range request first —
    // the 206 response carries `Content-Range: bytes 0-0/<total>` which
    // works reliably across the books that don't expose Content-Length.
    let total = probe_total(&client, url).await;
    let resp = client.get(url).send().await?.error_for_status()?;
    let total = total.or_else(|| resp.content_length());
    {
        // Stash the total so /jobs/{id} status can surface it for the
        // backend's progress polling — broadcast events go to subscribers
        // only, but status snapshots need it too.
        let mut s = state.lock().await;
        s.aaxc_bytes_total = total;
    }
    let mut stream = resp.bytes_stream();
    let mut file = tokio::fs::File::create(dest).await?;
    use futures_util::StreamExt;
    let mut bytes_done: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        bytes_done += chunk.len() as u64;
        let mut s = state.lock().await;
        s.aaxc_bytes = bytes_done;
        let _ = s.events.send(JobEvent::Downloading {
            bytes_done,
            bytes_total: total,
        });
    }
    file.flush().await?;
    Ok(bytes_done)
}

async fn probe_total(client: &reqwest::Client, url: &str) -> Option<u64> {
    let resp = client
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let cr = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)?
        .to_str()
        .ok()?;
    // `bytes 0-0/12345678` → take the trailing total.
    cr.rsplit('/').next().and_then(|s| s.parse::<u64>().ok())
}

async fn run_ffmpeg(
    state: &Arc<Mutex<JobState>>,
    ffmpeg_bin: &str,
    input: &std::path::Path,
    output: &std::path::Path,
    req: &JobReq,
) -> anyhow::Result<()> {
    let mut cmd = Command::new(ffmpeg_bin);
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);

    match &req.drm {
        Drm::Aaxc { key_hex, iv_hex } => {
            cmd.args(["-audible_key", key_hex, "-audible_iv", iv_hex]);
        }
        Drm::Aax { activation_bytes } => {
            cmd.args(["-activation_bytes", activation_bytes]);
        }
    }

    cmd.arg("-i").arg(input);
    // ffmpeg's `+faststart` rewrite of an AAX-decoded stream produces
    // a moov atom AVFoundation rejects at playback time (asset parses,
    // duration loads, then AVPlayer immediately pauses). The exact
    // delta is somewhere inside the rewritten moov — same audio
    // samples, same codec params, but reorder bugs AVFoundation cares
    // about. Re-confirmed by comparing scribe rescues against the
    // OpenAudible ffmpeg outputs of the same source: OA keeps moov at
    // the end and plays cleanly on iOS / macOS QuickTime. AAXC output
    // doesn't trip the same bug, so keep faststart there for HTTP
    // streaming friendliness.
    // Per-DRM muxer + codec strategy:
    // - AAXC: -c copy + +faststart. Audible's modern AAXC frames are
    //   clean, stts comes out correct, +faststart helps HTTP streaming.
    // - AAX: -c copy via -f ipod muxer. Re-encoding is blocked because
    //   stock ffmpeg's AAC decoder rejects Audible's older AAC with
    //   "Reserved bit set" / "ms_present = 3 is reserved" / scalable
    //   AOT extension bits in every frame, so the whole pipeline
    //   collapses. -c copy preserves audio bytes but ffmpeg's mp4
    //   muxer writes a single-entry stts (asserts every frame is 1024
    //   samples — incorrect for the trailing partial frame). The
    //   resulting m4b parses but AVFoundation rejects on play. The
    //   stts gets patched post-hoc by the scribe-press job runner
    //   reading mdat sample sizes and computing the actual last
    //   sample_duration; that step is non-destructive and only touches
    //   one atom.
    cmd.args(["-c", "copy"]);
    let mp4_format = match &req.drm {
        Drm::Aax { .. } => "ipod",
        Drm::Aaxc { .. } => {
            cmd.args(["-movflags", "+faststart"]);
            "mp4"
        }
    };
    cmd.arg("-metadata").arg(format!("title={}", req.title));
    cmd.arg("-metadata").arg(format!("artist={}", req.authors.join(", ")));
    cmd.arg("-metadata").arg(format!("album_artist={}", req.authors.join(", ")));
    // Identity tag so scribe can recognise its own files after a DB wipe
    // (sidecar JSON is the primary source, this is the lifeboat).
    cmd.arg("-metadata").arg(format!("asin={}", req.asin));
    cmd.arg("-metadata").arg(format!("source=scribe/{}", env!("CARGO_PKG_VERSION")));
    if !req.narrators.is_empty() {
        // Audiobookshelf + Apple Books convention: composer holds the narrator.
        cmd.arg("-metadata")
            .arg(format!("composer={}", req.narrators.join(", ")));
    }
    if let Some(series) = &req.series_title {
        cmd.arg("-metadata").arg(format!("album={series}"));
    }
    if let Some(seq) = &req.series_sequence {
        cmd.arg("-metadata").arg(format!("track={seq}"));
    }
    cmd.args(["-f", mp4_format]).arg(output);
    cmd.stdout(Stdio::null()).stderr(Stdio::piped()).stdin(Stdio::null());

    let mut child = cmd.spawn()?;

    // Capture stderr so a failure can be surfaced verbatim.
    let stderr_handle = child.stderr.take().map(|mut s| {
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf).await;
            buf
        })
    });

    // Poll the output file size while ffmpeg writes. Without this the
    // UI sees m4b_bytes=0 for the whole convert phase then a single jump
    // to full size at the end — bar pinned at 0% the entire time.
    let progress_state = state.clone();
    let progress_path = output.to_path_buf();
    let progress = tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            interval.tick().await;
            let Ok(meta) = tokio::fs::metadata(&progress_path).await else {
                continue;
            };
            let bytes = meta.len();
            let mut s = progress_state.lock().await;
            if !matches!(s.phase, Phase::Converting) {
                break;
            }
            s.m4b_bytes = bytes;
        }
    });

    let status = child.wait().await?;
    progress.abort();
    let stderr = match stderr_handle {
        Some(h) => h.await.unwrap_or_default(),
        None => String::new(),
    };
    if !status.success() {
        let msg = format!("ffmpeg exit {status}: {stderr}");
        {
            let mut s = state.lock().await;
            s.phase = Phase::Failed;
            s.error = Some(msg.clone());
            let _ = s.events.send(JobEvent::Failed { message: msg.clone() });
        }
        anyhow::bail!(msg);
    }
    Ok(())
}
