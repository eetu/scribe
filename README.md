# scribe

Self-hosted Audible library mirror. Polls your Audible accounts, downloads
purchased books, strips DRM (within your rights as the purchaser), converts
to OSS M4B with chapters + cover, and writes them to a NAS share where
audiobookshelf picks them up. Original AAXC files are stored separately as
cold-storage archive (untouched downloads from Audible).

Sibling product to [halo](../halo) and [chat](../chat). Same design tokens,
fonts, accent. Two visual divergences from the family: the wordmark reads
_the path of the righteous scribe._ (Pulp Fiction's Ezekiel 25:17 speech
with "man" swapped for "scribe", collapsing to `scribe.` on narrow
screens — same pattern as chat's `royale with chat.` → `chat.`), and the
glyph is a closed-book outline with a warm orange dot + audio ripples.

## Why it exists

OpenAudible works but is not OSS, opinionated about file layout, and
ships as a Java/SWT desktop app. scribe runs headless on the home Pi,
hands the heavy ffmpeg work to a Mac mini worker, integrates with
audiobookshelf for playback, and matches the home dashboard family
visually.

## Architecture

```
┌───────────────────── Pi (raspi) ─────────────────────┐
│                                                       │
│  scribe (Rust, axum, :3003)                          │
│   ├── React UI (Vite + Emotion + TanStack Router)    │
│   ├── SQLite (accounts, books, jobs)                 │
│   ├── OIDC (kanidm) + DEV_AUTH fallback              │
│   ├── Job orchestrator + polling loop                │
│   └── NAS writer (two trees: library/ + original/)     │
│        │                                              │
│        ▼ loopback HTTP                                │
│  shim (Python, FastAPI, :3004)                       │
│   └── wraps mkb79/audible — auth, library, voucher   │
│                                                       │
└───────────────────────────────────────────────────────┘
                       │ HTTPS + bearer
                       ▼
┌──────────────── mini (mac, Caddy) ───────────────────┐
│  scribe-press (Rust, axum, 127.0.0.1:3005)           │
│   └── ffmpeg subprocess, holds tmp AAXC + M4B,       │
│       streams both back to Pi on request             │
└───────────────────────────────────────────────────────┘
                       │
                       ▼
   NAS share (CIFS, mounted on Pi)
     ├── library/Author/Title/Title.m4b   ← audiobookshelf reads this
     └── original/Author/Title/Title.aaxc   ← cold original, ABS never sees
```

## Layout

```
scribe/
├── Cargo.toml             workspace (backend, press, shared)
├── backend/               Pi-side Rust service (UI + DB + orchestration)
├── press/                 mini-side Rust worker (ffmpeg)
├── shared/                shared types (JobSpec, BookMeta, etc.)
├── shim/                  Python sidecar wrapping mkb79/audible
├── frontend/              React + Vite + Emotion + TanStack Router
└── .claude/skills/scribe-design/  design system
```

## Job pipeline

```
1. polling loop hits shim → library list
2. diff against SQLite → new books queued
3. for each queued job:
     GET shim /accounts/{id}/books/{asin}/voucher
       → content_url + key + iv + chapters + cover_url
     POST press /jobs { content_url, key, iv, … }
       → job_id (press starts download + ffmpeg)
     wait for press SSE progress, surface to UI
     on completion:
       GET press /jobs/{id}/aaxc  → stream to original/Author/Title/Title.aaxc
       GET press /jobs/{id}/m4b   → stream to library/Author/Title/Title.m4b
       DELETE press /jobs/{id}    → press cleans tmp
       POST {ABS_URL}/api/libraries/{id}/scan  → audiobookshelf reindexes
```

Pi never holds either full file in RAM. Mini holds both briefly on SSD.

## Quick start

(Scaffold stage — most of this is stub. Run targets fill in as tasks complete.)

```sh
# Pi side
cd backend && cargo run

# Mini side
cd press && cargo run

# Shim
cd shim && uv sync && uv run shim

# Frontend
cd frontend && yarn install && yarn dev
```

## Status

Early scaffold. See task list in conversation history. Smoke test on a real
account is the milestone that unblocks rollout.
