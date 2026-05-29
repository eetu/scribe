//! On-disk cover-image cache, keyed by asin.
//!
//! `books.cover_url` is a live Amazon CDN link — fine until Amazon pulls
//! a title (revoked Plus rotation, region delisting) and the art 404s.
//! We mirror the bytes into `cfg.covers_dir` (`/var/lib/scribe/covers`,
//! restic-backed) as `{asin}.{ext}` and serve those instead. The cache
//! is keyed by asin alone, so it survives a `books`/`jobs` wipe and a
//! reconcile-only rebuild.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;

use crate::state::AppState;

const KNOWN_EXTS: &[&str] = &["jpg", "png", "webp", "gif"];

/// Hard ceiling on a fetched cover. Real covers are well under 1 MB; this
/// is a generous bound that stops a hostile/compromised CDN from streaming
/// an unbounded body and OOMing the 1 GB Pi.
const MAX_COVER_BYTES: usize = 16 * 1024 * 1024;

/// Amazon/Audible CDN hosts a cover may legitimately come from. `cover_url`
/// is ingested from Audible's API (not user input), but proxying it
/// server-side is still an SSRF surface — restrict to https on these hosts
/// so it can't be pointed at loopback / link-local metadata / internal LAN.
const ALLOWED_COVER_HOSTS: &[&str] = &[
    "media-amazon.com",
    "ssl-images-amazon.com",
    "images-amazon.com",
    "amazon.com",
    "audible.com",
];

fn cover_host_allowed(raw: &str) -> bool {
    let Ok(u) = url::Url::parse(raw) else {
        return false;
    };
    if u.scheme() != "https" {
        return false;
    }
    let Some(host) = u.host_str() else {
        return false;
    };
    ALLOWED_COVER_HOSTS
        .iter()
        .any(|s| host == *s || host.ends_with(&format!(".{s}")))
}

fn mime_for_ext(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "image/jpeg",
    }
}

/// Sniff a sane image type from magic bytes. Returns the file extension
/// to store under. Unknown payloads are rejected (we don't cache HTML
/// error pages or empty bodies as if they were covers).
fn sniff_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 12 {
        return None;
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if bytes.starts_with(b"GIF8") {
        Some("gif")
    } else {
        None
    }
}

/// asins are Amazon's alphanumeric ids; anything else is rejected so a
/// crafted asin can't escape the covers dir.
fn sanitize_asin(asin: &str) -> Option<&str> {
    if !asin.is_empty() && asin.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Some(asin)
    } else {
        None
    }
}

/// Locate an already-cached cover for `asin`. Returns its path + mime.
pub async fn find_cached(dir: &Path, asin: &str) -> Option<(PathBuf, &'static str)> {
    let asin = sanitize_asin(asin)?;
    for ext in KNOWN_EXTS {
        let p = dir.join(format!("{asin}.{ext}"));
        if tokio::fs::try_exists(&p).await.unwrap_or(false) {
            return Some((p, mime_for_ext(ext)));
        }
    }
    None
}

/// Fetch `cover_url` and store it atomically as `{asin}.{ext}`. Removes
/// any stale cover for the asin in a different extension first so a
/// format change doesn't leave two files. Returns the stored path + mime.
pub async fn fetch_and_store(
    state: &AppState,
    asin: &str,
    cover_url: &str,
) -> anyhow::Result<(PathBuf, &'static str)> {
    let asin = sanitize_asin(asin).ok_or_else(|| anyhow::anyhow!("invalid asin"))?;
    if !cover_host_allowed(cover_url) {
        anyhow::bail!("cover_url host not allowed: {cover_url}");
    }
    let dir = &state.cfg.covers_dir;
    tokio::fs::create_dir_all(dir).await?;

    let resp = state.http.get(cover_url).send().await?.error_for_status()?;
    // Reject early on an honest-but-huge Content-Length, then cap the
    // actual read so a missing/lying length can't OOM us either.
    if let Some(len) = resp.content_length() {
        if len > MAX_COVER_BYTES as u64 {
            anyhow::bail!("cover too large: {len} bytes");
        }
    }
    let mut stream = resp.bytes_stream();
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len() + chunk.len() > MAX_COVER_BYTES {
            anyhow::bail!("cover exceeds {MAX_COVER_BYTES} bytes");
        }
        bytes.extend_from_slice(&chunk);
    }
    let ext = sniff_ext(&bytes)
        .ok_or_else(|| anyhow::anyhow!("cover payload not a known image type"))?;

    // Clear other-extension leftovers for this asin.
    for old in KNOWN_EXTS.iter().filter(|e| **e != ext) {
        let _ = tokio::fs::remove_file(dir.join(format!("{asin}.{old}"))).await;
    }

    let final_path = dir.join(format!("{asin}.{ext}"));
    let tmp = dir.join(format!(".{asin}.{ext}.tmp"));
    tokio::fs::write(&tmp, &bytes).await?;
    tokio::fs::rename(&tmp, &final_path).await?;
    Ok((final_path, mime_for_ext(ext)))
}

/// Cache the cover for `asin` if it isn't already on disk. No-op when a
/// copy exists or `cover_url` is absent. Errors are logged, not fatal —
/// a failed cover cache never blocks a conversion.
pub async fn ensure_cached(state: &AppState, asin: &str, cover_url: Option<&str>) {
    if find_cached(&state.cfg.covers_dir, asin).await.is_some() {
        return;
    }
    let Some(url) = cover_url else { return };
    if let Err(e) = fetch_and_store(state, asin, url).await {
        tracing::warn!(asin, error = ?e, "cover cache failed");
    }
}

/// Force a re-fetch of every cover a profile owns (global refresh) —
/// picks up a cover URL that rotated since the last cache. fetch_and_store
/// overwrites in place; trickled, per-cover errors non-fatal.
pub async fn recache_owned(state: &AppState, profile_id: i64) {
    let rows: Vec<(String, Option<String>)> = state
        .db
        .with(move |c| {
            let mut stmt = c.prepare(
                "SELECT b.asin, b.cover_url FROM books b
                 JOIN accounts a ON a.id = b.account_id
                 WHERE a.profile_id = ?1 AND b.cover_url IS NOT NULL",
            )?;
            let v = stmt
                .query_map([profile_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .await
        .unwrap_or_default();
    for (asin, url) in rows {
        let Some(url) = url.as_deref() else { continue };
        if let Err(e) = fetch_and_store(state, &asin, url).await {
            tracing::debug!(asin, error = ?e, "cover recache miss");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Boot-time pass: mirror every book's CDN cover to disk so the cache is
/// populated before Amazon can pull any of them. Skips already-cached
/// asins and trickles requests so a cold start on the 1 GB Pi doesn't
/// hammer the CDN. Runs detached; failures are per-cover and logged.
pub fn spawn_boot_cache(state: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let rows: Vec<(String, Option<String>)> = match state
            .db
            .with(|c| {
                let mut stmt =
                    c.prepare("SELECT asin, cover_url FROM books WHERE cover_url IS NOT NULL")?;
                let v = stmt
                    .query_map([], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(v)
            })
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = ?e, "cover backfill query failed");
                return;
            }
        };
        let mut cached = 0usize;
        for (asin, url) in rows {
            if find_cached(&state.cfg.covers_dir, &asin).await.is_some() {
                continue;
            }
            let Some(url) = url.as_deref() else { continue };
            match fetch_and_store(&state, &asin, url).await {
                Ok(_) => cached += 1,
                Err(e) => tracing::debug!(asin, error = ?e, "cover backfill miss"),
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        if cached > 0 {
            tracing::info!(cached, "cover backfill complete");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::cover_host_allowed;

    #[test]
    fn allows_amazon_audible_https() {
        assert!(cover_host_allowed(
            "https://m.media-amazon.com/images/I/abc.jpg"
        ));
        assert!(cover_host_allowed(
            "https://images-na.ssl-images-amazon.com/x.jpg"
        ));
        assert!(cover_host_allowed("https://audible.com/cover.jpg"));
    }

    #[test]
    fn rejects_non_https() {
        assert!(!cover_host_allowed("http://m.media-amazon.com/x.jpg"));
    }

    #[test]
    fn rejects_internal_and_other_hosts() {
        assert!(!cover_host_allowed("https://127.0.0.1/x.jpg"));
        assert!(!cover_host_allowed("https://169.254.169.254/latest/meta-data"));
        assert!(!cover_host_allowed("https://evil.com/x.jpg"));
        // suffix-spoof attempt: not a real amazon subdomain.
        assert!(!cover_host_allowed("https://media-amazon.com.evil.com/x.jpg"));
        assert!(!cover_host_allowed("file:///etc/passwd"));
    }
}
