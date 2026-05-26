//! Surgical post-process for AAX-converted m4b files.
//!
//! ffmpeg's `-c copy` from an AAX source writes audio sample tables
//! that assume every AAC frame is exactly 1024 samples, including the
//! final frame. AAX's last frame is typically shorter (e.g. ~822
//! samples for All Systems Red), so the resulting m4b overstates total
//! samples in `mdhd.duration` / `tkhd.duration` / `elst.segment_duration`
//! and ships a single-entry `stts` that mismatches `mdat`. AVFoundation
//! strict-checks this and refuses to play on iOS / macOS QuickTime.
//!
//! This patcher fixes the four atoms (mdhd, tkhd, elst, stts) in place
//! using the source AAX's accurate sample count, and re-cascades the
//! growing-stts byte delta up through stbl/minf/mdia/trak/moov so the
//! container sizes stay self-consistent. Audio mdat bytes are never
//! touched.
//!
//! Inspired by OpenAudible's output shape — its Java mp4parser-based
//! muxer writes a two-entry stts straight from the source, which is
//! what AVFoundation expects.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// Patch the m4b produced by ffmpeg `-c copy` from an AAX source so
/// AVFoundation plays it. `aax_path` is the encrypted source — only
/// read to learn the true audio mdhd duration. `m4b_path` is the file
/// to mutate in place.
///
/// Returns `Ok(())` if the file was already correct (no stts overrun)
/// or after the patch lands. Errors propagate from IO + atom parse.
pub fn fix_aax_durations(aax_path: &Path, m4b_path: &Path) -> anyhow::Result<()> {
    // Step 1: read the source AAX's audio mdhd to learn (timescale,
    // true_duration_in_samples). The source is correct; the m4b
    // mirrored everything except this trailing-partial-frame fact.
    let aax_bytes = std::fs::read(aax_path)?;
    let (src_timescale, src_duration) = read_audio_mdhd(&aax_bytes)?;
    tracing::debug!(
        ?aax_path,
        timescale = src_timescale,
        duration_samples = src_duration,
        "patcher: source AAX mdhd"
    );

    // Step 2: parse the m4b's atom tree to find offsets of each atom
    // we'll modify + the container atoms whose sizes need bumping.
    let mut m4b_bytes = std::fs::read(m4b_path)?;
    let layout = locate_atoms(&m4b_bytes)?;

    // Step 3: bail out if the existing stts already encodes a partial
    // trailing sample matching the source. Idempotent re-runs cheap.
    let (existing_stts_total, existing_entry_count) =
        read_stts_summary(&m4b_bytes, layout.stts.payload_offset, layout.stts.payload_size)?;
    if existing_entry_count > 1 && existing_stts_total == src_duration {
        tracing::debug!(?m4b_path, "patcher: stts already correct, skipping");
        return Ok(());
    }

    // Step 4: build the replacement stts payload. Native AAC packs at
    // 1024 frames/sample; we keep the bulk entry at that and put the
    // remainder into a single trailing entry.
    let total_samples = existing_stts_total / 1024; // = old single-entry sample_count
    let bulk_samples = total_samples.saturating_sub(1);
    let bulk_duration = bulk_samples.saturating_mul(1024);
    let tail_duration = src_duration.saturating_sub(bulk_duration);
    if tail_duration == 0 || tail_duration > 1024 {
        anyhow::bail!(
            "patcher: implausible tail sample duration {} (bulk={} src={})",
            tail_duration,
            bulk_duration,
            src_duration
        );
    }

    let new_stts_payload = build_stts_payload(bulk_samples as u32, tail_duration as u32);
    let delta = new_stts_payload.len() as i64 - layout.stts.payload_size as i64;
    tracing::info!(
        ?m4b_path,
        bulk_samples,
        tail_duration,
        delta_bytes = delta,
        "patcher: rewriting stts"
    );

    // Step 5: rewrite the four duration fields in place. They're all
    // fixed-size — no cascade for these.
    let mvhd_ts = read_u32(&m4b_bytes, layout.mvhd_timescale_offset)? as u64;
    let new_tkhd_duration = src_duration * mvhd_ts / src_timescale as u64;
    write_u32(&mut m4b_bytes, layout.tkhd_duration_offset, new_tkhd_duration as u32);
    write_u32(&mut m4b_bytes, layout.elst_segment_duration_offset, new_tkhd_duration as u32);
    write_u32(&mut m4b_bytes, layout.mdhd_duration_offset, src_duration as u32);

    // Step 6: splice the new stts payload in. Everything after the old
    // payload shifts by `delta` bytes.
    let stts_payload_start = layout.stts.payload_offset;
    let stts_payload_end = stts_payload_start + layout.stts.payload_size;
    let mut patched = Vec::with_capacity(m4b_bytes.len() + delta.max(0) as usize);
    patched.extend_from_slice(&m4b_bytes[..stts_payload_start]);
    patched.extend_from_slice(&new_stts_payload);
    patched.extend_from_slice(&m4b_bytes[stts_payload_end..]);

    // Step 7: rewrite the size fields of stts itself + every container
    // atom above it (stbl, minf, mdia, trak, moov). Each grows by
    // `delta` bytes.
    bump_container_size(&mut patched, layout.stts.size_offset, delta);
    bump_container_size(&mut patched, layout.stbl_size_offset, delta);
    bump_container_size(&mut patched, layout.minf_size_offset, delta);
    bump_container_size(&mut patched, layout.mdia_size_offset, delta);
    bump_container_size(&mut patched, layout.trak_size_offset, delta);
    bump_container_size(&mut patched, layout.moov_size_offset, delta);

    // Step 8: atomic rewrite. Truncate-on-open + full rewrite is the
    // safest approach given the size change; a partial write would
    // leave a corrupt file.
    let mut f = OpenOptions::new().write(true).truncate(true).open(m4b_path)?;
    f.write_all(&patched)?;
    f.flush()?;
    Ok(())
}

/// Bump an atom's 32-bit size field by `delta` bytes (signed).
fn bump_container_size(buf: &mut [u8], size_offset: usize, delta: i64) {
    let current = read_u32(buf, size_offset).expect("size field in range");
    let new = (current as i64 + delta) as u32;
    write_u32(buf, size_offset, new);
}

/// Layout of the atoms we modify + their container ancestors that need
/// size cascading.
struct Layout {
    moov_size_offset: usize,
    trak_size_offset: usize,
    tkhd_duration_offset: usize,
    elst_segment_duration_offset: usize,
    mdia_size_offset: usize,
    mdhd_duration_offset: usize,
    mvhd_timescale_offset: usize,
    minf_size_offset: usize,
    stbl_size_offset: usize,
    stts: AtomSpan,
}

/// An atom's byte offsets within the file: where its size field sits
/// and where its payload starts (after the 8-byte header).
struct AtomSpan {
    size_offset: usize,
    payload_offset: usize,
    payload_size: usize,
}

fn locate_atoms(buf: &[u8]) -> anyhow::Result<Layout> {
    let moov = find_top_level_atom(buf, b"moov")?;
    let moov_payload_start = moov.size_offset + 8;
    let moov_payload_end = moov_payload_start + moov.payload_size;
    let moov_payload = &buf[moov_payload_start..moov_payload_end];

    // mvhd lives directly under moov.
    let mvhd_rel = find_child_atom(moov_payload, b"mvhd")?;
    // mvhd payload: version(1) + flags(3) + creation(4) + modification(4) + timescale(4)
    let mvhd_payload_offset = moov_payload_start + mvhd_rel.payload_offset;
    let mvhd_timescale_offset = mvhd_payload_offset + 4 + 4 + 4;

    // Find the audio trak (mdia/hdlr/handler_type == 'soun').
    let trak_rel = find_audio_trak(moov_payload)?;
    let trak_size_offset = moov_payload_start + trak_rel.size_offset;
    let trak_payload_start = moov_payload_start + trak_rel.payload_offset;
    let trak_payload_end = trak_payload_start + trak_rel.payload_size;
    let trak_payload = &buf[trak_payload_start..trak_payload_end];

    let tkhd_rel = find_child_atom(trak_payload, b"tkhd")?;
    let tkhd_payload_offset = trak_payload_start + tkhd_rel.payload_offset;
    // tkhd payload: version(1) + flags(3) + creation(4) + modification(4)
    //             + track_id(4) + reserved(4) + duration(4) ...
    let tkhd_duration_offset = tkhd_payload_offset + 4 + 4 + 4 + 4 + 4;

    let edts_rel = find_child_atom(trak_payload, b"edts")?;
    let edts_payload_offset = trak_payload_start + edts_rel.payload_offset;
    let edts_payload_end = edts_payload_offset + edts_rel.payload_size;
    let edts_payload = &buf[edts_payload_offset..edts_payload_end];
    let elst_rel = find_child_atom(edts_payload, b"elst")?;
    let elst_payload_offset = edts_payload_offset + elst_rel.payload_offset;
    // elst payload: version(1) + flags(3) + entry_count(4) + first_entry.segment_duration(4)
    let elst_segment_duration_offset = elst_payload_offset + 4 + 4;

    let mdia_rel = find_child_atom(trak_payload, b"mdia")?;
    let mdia_size_offset = trak_payload_start + mdia_rel.size_offset;
    let mdia_payload_offset = trak_payload_start + mdia_rel.payload_offset;
    let mdia_payload_end = mdia_payload_offset + mdia_rel.payload_size;
    let mdia_payload = &buf[mdia_payload_offset..mdia_payload_end];

    let mdhd_rel = find_child_atom(mdia_payload, b"mdhd")?;
    let mdhd_payload_offset = mdia_payload_offset + mdhd_rel.payload_offset;
    // mdhd payload (version 0): version(1) + flags(3) + creation(4)
    //                         + modification(4) + timescale(4) + duration(4)
    let mdhd_duration_offset = mdhd_payload_offset + 4 + 4 + 4 + 4;

    let minf_rel = find_child_atom(mdia_payload, b"minf")?;
    let minf_size_offset = mdia_payload_offset + minf_rel.size_offset;
    let minf_payload_offset = mdia_payload_offset + minf_rel.payload_offset;
    let minf_payload_end = minf_payload_offset + minf_rel.payload_size;
    let minf_payload = &buf[minf_payload_offset..minf_payload_end];

    let stbl_rel = find_child_atom(minf_payload, b"stbl")?;
    let stbl_size_offset = minf_payload_offset + stbl_rel.size_offset;
    let stbl_payload_offset = minf_payload_offset + stbl_rel.payload_offset;
    let stbl_payload_end = stbl_payload_offset + stbl_rel.payload_size;
    let stbl_payload = &buf[stbl_payload_offset..stbl_payload_end];

    let stts_rel = find_child_atom(stbl_payload, b"stts")?;
    let stts = AtomSpan {
        size_offset: stbl_payload_offset + stts_rel.size_offset,
        // stts payload begins after the 8-byte atom header AND after
        // the 4-byte version+flags field. Entries follow.
        payload_offset: stbl_payload_offset + stts_rel.payload_offset,
        payload_size: stts_rel.payload_size,
    };

    Ok(Layout {
        moov_size_offset: moov.size_offset,
        trak_size_offset,
        tkhd_duration_offset,
        elst_segment_duration_offset,
        mdia_size_offset,
        mdhd_duration_offset,
        mvhd_timescale_offset,
        minf_size_offset,
        stbl_size_offset,
        stts,
    })
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
                size_offset: pos,
                payload_offset: pos + 8,
                payload_size: size - 8,
            });
        }
        pos += size;
    }
    anyhow::bail!("atom {} not found", String::from_utf8_lossy(name))
}

/// Among moov's traks, return the one whose `mdia/hdlr/handler_type`
/// is `'soun'`.
fn find_audio_trak(moov_payload: &[u8]) -> anyhow::Result<AtomSpan> {
    let mut pos = 0;
    while pos + 8 <= moov_payload.len() {
        let size = read_u32(moov_payload, pos)? as usize;
        let atom_type = &moov_payload[pos + 4..pos + 8];
        if atom_type == b"trak" {
            let payload = &moov_payload[pos + 8..pos + size];
            if is_audio_trak(payload).unwrap_or(false) {
                return Ok(AtomSpan {
                    size_offset: pos,
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
    // hdlr payload: version+flags(4) + pre_defined(4) + handler_type(4) + ...
    if hdlr_payload.len() < 12 {
        return Ok(false);
    }
    Ok(&hdlr_payload[8..12] == b"soun")
}

/// Read the audio track's mdhd.duration + timescale from a raw
/// ISO BMFF file (used for the source AAX read).
fn read_audio_mdhd(buf: &[u8]) -> anyhow::Result<(u32, u64)> {
    let moov = find_top_level_atom(buf, b"moov")?;
    let moov_payload =
        &buf[moov.payload_offset..moov.payload_offset + moov.payload_size];
    let trak = find_audio_trak(moov_payload)?;
    let trak_payload =
        &moov_payload[trak.payload_offset..trak.payload_offset + trak.payload_size];
    let mdia = find_child_atom(trak_payload, b"mdia")?;
    let mdia_payload =
        &trak_payload[mdia.payload_offset..mdia.payload_offset + mdia.payload_size];
    let mdhd = find_child_atom(mdia_payload, b"mdhd")?;
    let mdhd_payload =
        &mdia_payload[mdhd.payload_offset..mdhd.payload_offset + mdhd.payload_size];
    // version(1) + flags(3) + ...
    let version = mdhd_payload[0];
    let (timescale, duration) = if version == 0 {
        // creation(4) + modification(4) + timescale(4) + duration(4)
        let timescale = read_u32_at(mdhd_payload, 4 + 4 + 4)?;
        let duration = read_u32_at(mdhd_payload, 4 + 4 + 4 + 4)? as u64;
        (timescale, duration)
    } else {
        // creation(8) + modification(8) + timescale(4) + duration(8)
        let timescale = read_u32_at(mdhd_payload, 4 + 8 + 8)?;
        let duration = read_u64_at(mdhd_payload, 4 + 8 + 8 + 4)?;
        (timescale, duration)
    };
    Ok((timescale, duration))
}

/// Sum the total sample-duration (= sum of count×duration) declared by
/// the existing stts. Returns (total_in_samples, entry_count).
fn read_stts_summary(
    buf: &[u8],
    payload_offset: usize,
    payload_size: usize,
) -> anyhow::Result<(u64, u32)> {
    // Payload: version+flags(4) + entry_count(4) + entries(entry_count × 8)
    let entry_count = read_u32(buf, payload_offset + 4)?;
    let entries_start = payload_offset + 8;
    let entries_end = payload_offset + payload_size;
    let mut total: u64 = 0;
    let mut pos = entries_start;
    while pos + 8 <= entries_end {
        let count = read_u32(buf, pos)? as u64;
        let dur = read_u32(buf, pos + 4)? as u64;
        total += count * dur;
        pos += 8;
    }
    Ok((total, entry_count))
}

/// Construct an stts payload with two entries: a bulk run of 1024-sample
/// frames and a single trailing partial.
fn build_stts_payload(bulk_samples: u32, tail_duration: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + 8 + 16);
    // version+flags = 0
    out.extend_from_slice(&[0u8; 4]);
    // entry_count = 2
    out.extend_from_slice(&2u32.to_be_bytes());
    // entry 1: bulk
    out.extend_from_slice(&bulk_samples.to_be_bytes());
    out.extend_from_slice(&1024u32.to_be_bytes());
    // entry 2: trailing partial
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&tail_duration.to_be_bytes());
    out
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

