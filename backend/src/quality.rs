//! Pure-Rust m4b audio-quality probe — no ffmpeg dependency.
//!
//! The backend image is `FROM scratch` (no ffprobe), and we never want to
//! bloat it with ffmpeg on the 1 GB Pi. We only need the audio track's
//! sample table + codec entry, which live in `moov`; the multi-GB `mdat`
//! is skipped via seek, so probing even a 900 MB file reads ~1 MB.
//!
//! Since scribe remuxes with `-c copy` (never transcodes), the file's
//! specs equal the tier Audible delivered — this is the authoritative
//! per-book quality, and the reliable way to tell a 64 kbps edition from
//! a 128 kbps one when the same title exists twice.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use rusqlite::OptionalExtension;
use uuid::Uuid;

use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct Quality {
    pub codec: String,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub channels: u32,
}

fn be_u16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn be_u32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn be_u64(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(b[o..o + 8].try_into().unwrap())
}

/// Read just the top-level `moov` box into memory, seeking past `mdat`.
fn read_moov(path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let total = f.metadata()?.len();
    let mut pos: u64 = 0;
    loop {
        if pos + 8 > total {
            anyhow::bail!("moov box not found");
        }
        f.seek(SeekFrom::Start(pos))?;
        let mut hdr = [0u8; 8];
        f.read_exact(&mut hdr)?;
        let mut size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let mut hdr_len = 8u64;
        if size == 1 {
            let mut ext = [0u8; 8];
            f.read_exact(&mut ext)?;
            size = u64::from_be_bytes(ext);
            hdr_len = 16;
        } else if size == 0 {
            size = total - pos;
        }
        if size < hdr_len {
            anyhow::bail!("bad box size at {pos}");
        }
        if &hdr[4..8] == b"moov" {
            let payload_len = (size - hdr_len) as usize;
            let mut buf = vec![0u8; payload_len];
            f.seek(SeekFrom::Start(pos + hdr_len))?;
            f.read_exact(&mut buf)?;
            return Ok(buf);
        }
        pos += size;
    }
}

/// Find a child box by type within `buf[start..end]`. Returns the box's
/// payload range `(payload_start, box_end)` — box_end doubles as the
/// next sibling's start.
fn find(buf: &[u8], start: usize, end: usize, name: &[u8; 4]) -> Option<(usize, usize)> {
    let mut pos = start;
    while pos + 8 <= end {
        let mut size = be_u32(buf, pos) as usize;
        let mut hl = 8usize;
        if size == 1 {
            if pos + 16 > end {
                return None;
            }
            size = be_u64(buf, pos + 8) as usize;
            hl = 16;
        } else if size == 0 {
            size = end - pos;
        }
        if size < hl || pos + size > end {
            return None;
        }
        if &buf[pos + 4..pos + 8] == name {
            return Some((pos + hl, pos + size));
        }
        pos += size;
    }
    None
}

fn is_soun(buf: &[u8], trak_s: usize, trak_e: usize) -> bool {
    if let Some((m_s, m_e)) = find(buf, trak_s, trak_e, b"mdia") {
        if let Some((h_s, h_e)) = find(buf, m_s, m_e, b"hdlr") {
            // hdlr: version/flags(4) + pre_defined(4) + handler_type(4)
            return h_s + 12 <= h_e && &buf[h_s + 8..h_s + 12] == b"soun";
        }
    }
    false
}

pub fn probe(path: &Path) -> anyhow::Result<Quality> {
    let moov = read_moov(path)?;
    let end = moov.len();

    // First soun trak.
    let mut pos = 0usize;
    let (trak_s, trak_e) = loop {
        let (ps, pe) = find(&moov, pos, end, b"trak").ok_or_else(|| anyhow::anyhow!("no trak"))?;
        if is_soun(&moov, ps, pe) {
            break (ps, pe);
        }
        pos = pe;
    };

    let (mdia_s, mdia_e) = find(&moov, trak_s, trak_e, b"mdia").ok_or_else(|| anyhow::anyhow!("no mdia"))?;
    let (mdhd_s, mdhd_e) = find(&moov, mdia_s, mdia_e, b"mdhd").ok_or_else(|| anyhow::anyhow!("no mdhd"))?;
    if mdhd_e < mdhd_s + 24 {
        anyhow::bail!("short mdhd");
    }
    let version = moov[mdhd_s];
    let (timescale, duration) = if version == 0 {
        (
            be_u32(&moov, mdhd_s + 12) as u64,
            be_u32(&moov, mdhd_s + 16) as u64,
        )
    } else {
        if mdhd_e < mdhd_s + 32 {
            anyhow::bail!("short mdhd v1");
        }
        (
            be_u32(&moov, mdhd_s + 20) as u64,
            be_u64(&moov, mdhd_s + 24),
        )
    };
    if timescale == 0 || duration == 0 {
        anyhow::bail!("zero mdhd timescale/duration");
    }

    let (minf_s, minf_e) = find(&moov, mdia_s, mdia_e, b"minf").ok_or_else(|| anyhow::anyhow!("no minf"))?;
    let (stbl_s, stbl_e) = find(&moov, minf_s, minf_e, b"stbl").ok_or_else(|| anyhow::anyhow!("no stbl"))?;

    // stsd: version/flags(4) + entry_count(4) + first sample entry box.
    let (stsd_s, stsd_e) = find(&moov, stbl_s, stbl_e, b"stsd").ok_or_else(|| anyhow::anyhow!("no stsd"))?;
    let entry = stsd_s + 8;
    if entry + 36 > stsd_e {
        anyhow::bail!("short stsd entry");
    }
    // AudioSampleEntry: ...(8)+dri(2)+version/rev/vendor(8)+channels(2)
    //   +samplesize(2)+predef(2)+reserved(2)+samplerate(4, 16.16).
    let codec = String::from_utf8_lossy(&moov[entry + 4..entry + 8])
        .trim_end_matches(['\0', ' '])
        .to_string();
    let channels = be_u16(&moov, entry + 24) as u32;
    let sample_rate = be_u16(&moov, entry + 32) as u32; // integer part of 16.16

    // stsz: version/flags(4) + sample_size(4) + sample_count(4) + [sizes].
    let (stsz_s, stsz_e) = find(&moov, stbl_s, stbl_e, b"stsz").ok_or_else(|| anyhow::anyhow!("no stsz"))?;
    if stsz_e < stsz_s + 12 {
        anyhow::bail!("short stsz");
    }
    let sample_size = be_u32(&moov, stsz_s + 4);
    let count = be_u32(&moov, stsz_s + 8) as usize;
    let total_bytes: u64 = if sample_size != 0 {
        sample_size as u64 * count as u64
    } else {
        let table = stsz_s + 12;
        if table + count * 4 > stsz_e {
            anyhow::bail!("stsz table truncated");
        }
        (0..count).map(|i| be_u32(&moov, table + i * 4) as u64).sum()
    };

    let dur_sec = duration as f64 / timescale as f64;
    let bitrate_kbps = if dur_sec > 0.0 {
        ((total_bytes as f64 * 8.0) / dur_sec / 1000.0).round() as u32
    } else {
        0
    };

    Ok(Quality {
        codec,
        bitrate_kbps,
        sample_rate,
        channels,
    })
}

/// Probe `m4b_path` and persist the quality onto the job's book row.
/// Called when a job finishes; failures are logged, never fatal — a
/// quality probe must not block a completed conversion.
pub async fn capture(state: &AppState, job_id: Uuid, m4b_path: &str) {
    let jid = job_id.to_string();
    let ids: Option<(String, String)> = state
        .db
        .with(move |c| {
            c.query_row(
                "SELECT asin, account_id FROM jobs WHERE id = ?1",
                rusqlite::params![jid],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()
        })
        .await
        .unwrap_or(None);
    let Some((asin, account)) = ids else { return };
    persist(state, &asin, &account, m4b_path).await;
}

async fn persist(state: &AppState, asin: &str, account: &str, m4b_path: &str) {
    let path = m4b_path.to_string();
    let q = match tokio::task::spawn_blocking(move || probe(Path::new(&path))).await {
        Ok(Ok(q)) => q,
        Ok(Err(e)) => {
            tracing::debug!(asin, error = ?e, "quality probe failed");
            return;
        }
        Err(_) => return,
    };
    let (asin, account) = (asin.to_string(), account.to_string());
    let _ = state
        .db
        .with(move |c| {
            c.execute(
                "UPDATE books SET codec = ?1, bitrate_kbps = ?2, sample_rate = ?3, channels = ?4
                 WHERE asin = ?5 AND account_id = ?6",
                rusqlite::params![
                    q.codec,
                    q.bitrate_kbps as i64,
                    q.sample_rate as i64,
                    q.channels as i64,
                    asin,
                    account
                ],
            )
        })
        .await;
}

/// Boot pass: backfill quality for any done book that has an m4b on disk
/// but no probed bitrate yet (pre-existing library, or rows that predate
/// this feature). Trickles to keep the 1 GB Pi calm; detached + per-book
/// errors are non-fatal.
pub fn spawn_boot_backfill(state: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let rows: Vec<(String, String, String)> = match state
            .db
            .with(|c| {
                let mut stmt = c.prepare(
                    "SELECT b.asin, b.account_id, j.m4b_path
                     FROM books b
                     JOIN jobs j ON j.asin = b.asin AND j.account_id = b.account_id
                     WHERE b.bitrate_kbps IS NULL
                       AND j.status = 'done' AND j.m4b_path IS NOT NULL
                     GROUP BY b.asin, b.account_id",
                )?;
                let v = stmt
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(v)
            })
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = ?e, "quality backfill query failed");
                return;
            }
        };
        let mut done = 0usize;
        for (asin, account, m4b) in rows {
            if !tokio::fs::try_exists(&m4b).await.unwrap_or(false) {
                continue;
            }
            persist(&state, &asin, &account, &m4b).await;
            done += 1;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if done > 0 {
            tracing::info!(probed = done, "quality backfill complete");
        }
    });
}
