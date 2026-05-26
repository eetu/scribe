//! Surgical post-process for AAX-converted m4b files.
//!
//! ffmpeg's `-c copy` from an AAX source preserves the audio sample
//! table verbatim (mdhd.duration / stts) — those are *deliberately
//! overstated* in AAX: every AAC frame including the trailing partial
//! is declared as 1024 samples. The intentional safety net is the
//! **edit list**: `elst.segment_duration` (along with mvhd.duration
//! and tkhd.duration) caps actual playback to the real length, so
//! decoders stop before reading into the trailing-garbage frame.
//!
//! ffmpeg loses this nuance on remux: it copies the (overstated)
//! mdhd.duration straight through and writes mvhd/tkhd/elst from it.
//! Result: edit list no longer truncates, playback runs past the
//! valid audio, AVFoundation pauses immediately on play.
//!
//! Fix: re-derive mvhd/tkhd/elst from the source AAX (which holds the
//! true duration in its own edit list) and patch them in the output.
//! mdhd.duration and stts stay as-is — they intentionally overstate;
//! the edit list is the corrective layer above them.
//!
//! Audio mdat bytes are never touched. Idempotent re-runs detect the
//! already-correct values and skip.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// Patch the m4b's edit-list-side duration atoms (mvhd, audio tkhd,
/// audio elst) using the source AAX's authoritative playback length.
/// Leaves mdhd / stts / mdat untouched.
pub fn fix_aax_durations(aax_path: &Path, m4b_path: &Path) -> anyhow::Result<()> {
    let aax_bytes = std::fs::read(aax_path)?;
    let (src_mvhd_timescale, src_mvhd_duration) = read_mvhd(&aax_bytes)?;
    let true_seconds = src_mvhd_duration as f64 / src_mvhd_timescale as f64;
    tracing::debug!(
        ?aax_path,
        src_mvhd_timescale,
        src_mvhd_duration,
        true_seconds,
        "patcher: source AAX playback duration"
    );

    let mut m4b_bytes = std::fs::read(m4b_path)?;
    let layout = locate_atoms(&m4b_bytes)?;

    let out_mvhd_timescale = read_u32(&m4b_bytes, layout.mvhd_timescale_offset)? as u64;
    let new_mvhd_duration =
        (src_mvhd_duration as u128 * out_mvhd_timescale as u128 / src_mvhd_timescale as u128)
            as u32;

    let current_mvhd_duration = read_u32(&m4b_bytes, layout.mvhd_duration_offset)?;
    if current_mvhd_duration == new_mvhd_duration {
        tracing::debug!(?m4b_path, "patcher: durations already match source, skipping");
        return Ok(());
    }
    tracing::info!(
        ?m4b_path,
        current = current_mvhd_duration,
        replacement = new_mvhd_duration,
        delta_ms = current_mvhd_duration as i64 - new_mvhd_duration as i64,
        "patcher: rewriting mvhd/tkhd/elst durations"
    );

    write_u32(&mut m4b_bytes, layout.mvhd_duration_offset, new_mvhd_duration);
    write_u32(&mut m4b_bytes, layout.tkhd_duration_offset, new_mvhd_duration);
    write_u32(
        &mut m4b_bytes,
        layout.elst_segment_duration_offset,
        new_mvhd_duration,
    );

    let mut f = OpenOptions::new().write(true).truncate(true).open(m4b_path)?;
    f.write_all(&m4b_bytes)?;
    f.flush()?;
    Ok(())
}

/// Byte offsets we mutate. No container-size cascade is needed because
/// every field we touch is fixed-size.
struct Layout {
    mvhd_timescale_offset: usize,
    mvhd_duration_offset: usize,
    tkhd_duration_offset: usize,
    elst_segment_duration_offset: usize,
}

struct AtomSpan {
    payload_offset: usize,
    payload_size: usize,
}

fn locate_atoms(buf: &[u8]) -> anyhow::Result<Layout> {
    let moov = find_top_level_atom(buf, b"moov")?;
    let moov_payload_start = moov.payload_offset;
    let moov_payload_end = moov_payload_start + moov.payload_size;
    let moov_payload = &buf[moov_payload_start..moov_payload_end];

    let mvhd_rel = find_child_atom(moov_payload, b"mvhd")?;
    let mvhd_payload_offset = moov_payload_start + mvhd_rel.payload_offset;
    // mvhd v0: version(1) + flags(3) + creation(4) + modification(4)
    //        + timescale(4) + duration(4) + ...
    let version = buf[mvhd_payload_offset];
    if version != 0 {
        anyhow::bail!("mvhd version {} not yet supported", version);
    }
    let mvhd_timescale_offset = mvhd_payload_offset + 4 + 4 + 4;
    let mvhd_duration_offset = mvhd_timescale_offset + 4;

    let trak_rel = find_audio_trak(moov_payload)?;
    let trak_payload_start = moov_payload_start + trak_rel.payload_offset;
    let trak_payload_end = trak_payload_start + trak_rel.payload_size;
    let trak_payload = &buf[trak_payload_start..trak_payload_end];

    let tkhd_rel = find_child_atom(trak_payload, b"tkhd")?;
    let tkhd_payload_offset = trak_payload_start + tkhd_rel.payload_offset;
    let tkhd_version = buf[tkhd_payload_offset];
    if tkhd_version != 0 {
        anyhow::bail!("tkhd version {} not yet supported", tkhd_version);
    }
    // tkhd v0: version+flags(4) + creation(4) + modification(4)
    //        + track_id(4) + reserved(4) + duration(4) + ...
    let tkhd_duration_offset = tkhd_payload_offset + 4 + 4 + 4 + 4 + 4;

    let edts_rel = find_child_atom(trak_payload, b"edts")?;
    let edts_payload_offset = trak_payload_start + edts_rel.payload_offset;
    let edts_payload_end = edts_payload_offset + edts_rel.payload_size;
    let edts_payload = &buf[edts_payload_offset..edts_payload_end];
    let elst_rel = find_child_atom(edts_payload, b"elst")?;
    let elst_payload_offset = edts_payload_offset + elst_rel.payload_offset;
    // elst v0: version+flags(4) + entry_count(4) + first_entry.segment_duration(4)
    let elst_segment_duration_offset = elst_payload_offset + 4 + 4;

    Ok(Layout {
        mvhd_timescale_offset,
        mvhd_duration_offset,
        tkhd_duration_offset,
        elst_segment_duration_offset,
    })
}

/// Read mvhd.timescale + mvhd.duration from a raw ISO BMFF buffer.
fn read_mvhd(buf: &[u8]) -> anyhow::Result<(u32, u64)> {
    let moov = find_top_level_atom(buf, b"moov")?;
    let moov_payload = &buf[moov.payload_offset..moov.payload_offset + moov.payload_size];
    let mvhd = find_child_atom(moov_payload, b"mvhd")?;
    let mvhd_payload =
        &moov_payload[mvhd.payload_offset..mvhd.payload_offset + mvhd.payload_size];
    let version = mvhd_payload[0];
    let (timescale, duration) = if version == 0 {
        let timescale = read_u32_at(mvhd_payload, 4 + 4 + 4)?;
        let duration = read_u32_at(mvhd_payload, 4 + 4 + 4 + 4)? as u64;
        (timescale, duration)
    } else {
        let timescale = read_u32_at(mvhd_payload, 4 + 8 + 8)?;
        let duration = read_u64_at(mvhd_payload, 4 + 8 + 8 + 4)?;
        (timescale, duration)
    };
    Ok((timescale, duration))
}

fn find_top_level_atom(buf: &[u8], name: &[u8; 4]) -> anyhow::Result<AtomSpan> {
    find_atom_in(buf, name, 0, buf.len())
}

fn find_child_atom(payload: &[u8], name: &[u8; 4]) -> anyhow::Result<AtomSpan> {
    find_atom_in(payload, name, 0, payload.len())
}

fn find_atom_in(
    buf: &[u8],
    name: &[u8; 4],
    start: usize,
    end: usize,
) -> anyhow::Result<AtomSpan> {
    let mut pos = start;
    while pos + 8 <= end {
        let size = read_u32(buf, pos)? as usize;
        if size < 8 || pos + size > end {
            anyhow::bail!(
                "atom size out of range: name={:?} pos={} size={} end={}",
                String::from_utf8_lossy(&buf[pos + 4..pos + 8]),
                pos,
                size,
                end
            );
        }
        let atom_type = &buf[pos + 4..pos + 8];
        if atom_type == name {
            return Ok(AtomSpan {
                payload_offset: pos + 8,
                payload_size: size - 8,
            });
        }
        pos += size;
    }
    anyhow::bail!("atom {} not found", String::from_utf8_lossy(name))
}

/// Among moov's traks, return the one whose mdia/hdlr handler_type is
/// 'soun'. Source AAX has multiple traks (audio, chapters, metadata);
/// the audio one is the only one we care about.
fn find_audio_trak(moov_payload: &[u8]) -> anyhow::Result<AtomSpan> {
    let mut pos = 0;
    while pos + 8 <= moov_payload.len() {
        let size = read_u32(moov_payload, pos)? as usize;
        let atom_type = &moov_payload[pos + 4..pos + 8];
        if atom_type == b"trak" {
            let payload = &moov_payload[pos + 8..pos + size];
            if is_audio_trak(payload).unwrap_or(false) {
                return Ok(AtomSpan {
                    payload_offset: pos + 8,
                    payload_size: size - 8,
                });
            }
        }
        pos += size;
    }
    anyhow::bail!("no audio trak in moov")
}

fn is_audio_trak(trak_payload: &[u8]) -> anyhow::Result<bool> {
    let mdia = find_child_atom(trak_payload, b"mdia")?;
    let mdia_payload = &trak_payload[mdia.payload_offset..mdia.payload_offset + mdia.payload_size];
    let hdlr = find_child_atom(mdia_payload, b"hdlr")?;
    let hdlr_payload =
        &mdia_payload[hdlr.payload_offset..hdlr.payload_offset + hdlr.payload_size];
    if hdlr_payload.len() < 12 {
        return Ok(false);
    }
    Ok(&hdlr_payload[8..12] == b"soun")
}

fn read_u32(buf: &[u8], offset: usize) -> anyhow::Result<u32> {
    let slice = buf
        .get(offset..offset + 4)
        .ok_or_else(|| anyhow::anyhow!("read_u32 out of range at {offset}"))?;
    Ok(u32::from_be_bytes(slice.try_into().unwrap()))
}

fn read_u32_at(buf: &[u8], offset: usize) -> anyhow::Result<u32> {
    read_u32(buf, offset)
}

fn read_u64_at(buf: &[u8], offset: usize) -> anyhow::Result<u64> {
    let slice = buf
        .get(offset..offset + 8)
        .ok_or_else(|| anyhow::anyhow!("read_u64 out of range at {offset}"))?;
    Ok(u64::from_be_bytes(slice.try_into().unwrap()))
}

fn write_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}
