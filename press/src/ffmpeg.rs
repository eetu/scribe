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
    let resp = client.get(url).send().await?.error_for_status()?;
    let total = resp.content_length();
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
    cmd.args(["-c", "copy", "-movflags", "+faststart"]);
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
    cmd.args(["-f", "mp4"]).arg(output);
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

    let status = child.wait().await?;
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
