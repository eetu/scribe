# press — ffmpeg worker

axum 0.8 service. Receives JobSpecs from scribe over LAN (or loopback when
co-located), downloads AAXC from Audible CDN to local disk, decrypts +
remuxes to M4B (fragmented MP4) via ffmpeg subprocess, exposes both
artifacts to scribe over HTTP, then cleans up on DELETE.

Runs on any LAN host — a separate box for headroom, or right on the Pi
alongside scribe. The ffmpeg step is a lossless remux (`-c copy`), not a
re-encode, so it's light enough that a Pi handles it; a dedicated host is
an optional throughput choice, not a requirement.

Stateless across restarts — any in-flight job on boot is lost and must be
re-enqueued by scribe.

## Routes

| Method | Path | Notes |
|---|---|---|
| GET | `/health` | liveness |
| POST | `/jobs` | body: `shared::JobSpec` → `{ job_id }`. Starts work. |
| GET | `/jobs/{id}` | status snapshot |
| GET | `/jobs/{id}/sse` | progress events stream |
| GET | `/jobs/{id}/aaxc` | byte-stream raw AAXC (Content-Type: application/octet-stream) |
| GET | `/jobs/{id}/m4b` | byte-stream final fMP4 (Content-Type: audio/mp4) |
| DELETE | `/jobs/{id}` | remove tmp files + state |

All non-`/health` routes require `Authorization: Bearer <PRESS_TOKEN>`.

## ffmpeg invocation

```
ffmpeg -hide_banner -loglevel error -nostdin \
       -audible_key  $HEX_KEY \
       -audible_iv   $HEX_IV \
       -i $TMPDIR/$JOB.aaxc \
       -c copy \
       -movflags +frag_keyframe+empty_moov+default_base_moof \
       -metadata title="$TITLE" \
       -metadata artist="$AUTHOR" \
       -metadata album="$SERIES_TITLE" \
       -metadata track="$SERIES_SEQ" \
       -f mp4 \
       $TMPDIR/$JOB.m4b
```

(Cover + chapter embedding done via a follow-on pass; see ffmpeg.rs.)

## Concurrency

`PRESS_MAX_JOBS` (default 2). tokio semaphore around the ffmpeg
subprocess. Remux saturates a core and CDN downloads benefit from
sequencing, so keep this low on the Pi; raise it on a beefier host.

## Tmp layout

`PRESS_TMP_DIR` (default `/var/folders/.../scribe-press/`) — one
subdirectory per job: `{job_id}/raw.aaxc`, `{job_id}/out.m4b`,
`{job_id}/cover.jpg`. Deleted on DELETE; aged-out after 24h by background
sweep (in case scribe never DELETEs).

## Environment

| Var | Default | Purpose |
|---|---|---|
| `PRESS_BIND` | `127.0.0.1:3005` | listen address |
| `PRESS_TOKEN` | _required_ | bearer expected on Authorization header |
| `PRESS_TMP_DIR` | `/var/folders/.../scribe-press/` | scratch root |
| `PRESS_MAX_JOBS` | `2` | concurrent jobs |
| `FFMPEG_BIN` | `ffmpeg` | path to ffmpeg binary |
