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
│   └── NAS writer (two trees: library/ + original/)   │
│        │                                              │
│        ▼ loopback HTTP                                │
│  shim (Python, FastAPI, :3004)                       │
│   └── wraps mkb79/audible — auth, library, voucher   │
│                                                       │
│  shelf (Rust, axum, :3006) — optional                │
│   └── read-only ABS-compatible API over scribe.db    │
│       for Listen This / other ABS clients            │
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

```sh
# Pi side
cd backend && cargo run

# Mini side
cd press && cargo run

# Shim
cd shim && uv sync && uv run shim

# Frontend
cd frontend && yarn install && yarn dev

# Shelf (optional — read-only ABS-compat sidecar for external clients)
cd shelf && cargo run
```

## Deployment requirements (raspi)

scribe ships via the [raspi IaC](../raspi) tasks `scribe.py` + `scribe_shim.py`.
The container itself is straightforward; a few env vars + one firewall hole
have surprised redeploys when they were missing.

### Environment

`group_data/all.py` → `SCRIBE.env`:

| Var | Required | Notes |
|---|---|---|
| `SCRIBE_BIND` | for reconvert | Must be `0.0.0.0:3003`, not the loopback default. Press on mini fetches `/internal/aaxc/{token}` over the LAN; loopback bind blocks that. Traefik still routes public traffic through 127.0.0.1, which 0.0.0.0 naturally includes. |
| `SCRIBE_PRESS_URL` | yes | LAN URL of the mini-side press worker (`http://<mini>:3005`). |
| `SCRIBE_INTERNAL_URL` | for reconvert | LAN URL of *this* scribe instance from press's POV (`http://<raspi-lan-ip>:3003`). Used to mint one-shot `/internal/aaxc/{token}` URLs. Unset = reconvert disabled, downloads still work. |
| `SCRIBE_LIBRARY_DIR` | yes | Canonical M4B output root. ABS scans this. |
| `SCRIBE_ORIGINAL_DIR` | yes | Encrypted AAXC + sidecar JSON tree. ABS does **not** scan this. |
| `SCRIBE_AUTO_ENQUEUE` | optional | `1` auto-queues new purchases on poll; `0` is manual-only. |
| `OIDC_*` | for kanidm | Discovery + client secret + redirect URL. `DEV_AUTH=1` short-circuits for local. |

`secret_env`:

- `SCRIBE_PRESS_TOKEN` — bearer shared with mini's `scribe-press` (`api_key` field).
- `ABS_TOKEN` — audiobookshelf API token, lets scribe trigger a rescan after each job.

### Firewall

UFW on the Pi defaults to deny incoming. Reconvert requires the mini to reach
scribe directly on port 3003 (the `/internal/aaxc/{token}` route). Open it
scoped to the mini IP:

```sh
sudo ufw allow from <mini-lan-ip> to any port 3003 proto tcp comment 'scribe reconvert from mini'
```

The Pi-side `tasks/hardening.py` doesn't ship this rule yet — add it there
when you fold mini → raspi into the IaC checklist.

### NAS layout

`SCRIBE_LIBRARY_DIR` and `SCRIBE_ORIGINAL_DIR` should be **separate trees**:

- `…/audible/books/` — canonical `Author/Series/#N - Title/Title.m4b`, ABS root.
- `…/audible/originals/` — `Author/Series/Title-ASIN.aaxc` + `Title-ASIN.aaxc.scribe.json`.

The sidecar JSON is the source of truth that survives a DB wipe. It also
carries the AAXC voucher key + iv (since the reconvert feature) so a future
re-conversion can happen entirely from local files, even after Audible
revokes the title's license.

### Reconvert plumbing

When an m4b on the NAS goes missing (manual delete, ABS purge), the library
page flips the chip to `missing` and surfaces a `re-convert` button:

1. Backend mints a one-shot token, registers `(token → aaxc_path)`.
2. Submits a normal press job with `content_url = ${SCRIBE_INTERNAL_URL}/internal/aaxc/<token>`.
3. Press fetches the AAXC over LAN like any CDN URL, runs ffmpeg, stages M4B.
4. Backend streams the M4B back into the canonical path, atomic-renames `.partial → final`, revokes the token.

The flow has no extra press-side dependency — press treats the scribe URL as
just another `content_url`. The only deployment-side requirements are the
`SCRIBE_BIND`, `SCRIBE_INTERNAL_URL`, and UFW rule above.

## Shelf — external ABS-compatible read API

`scribe-shelf` is a separate, optional service that exposes a slice of the
Audiobookshelf REST API over scribe's database. iOS / desktop clients that
already speak ABS (Listen This, ABS web/iOS, etc.) can browse and stream
scribe's library directly through shelf without needing to run the real
Audiobookshelf alongside.

It is built on the same principles as `press` and `shim`:

- **Read-only by construction.** Opens `scribe.db` with
  `SQLITE_OPEN_READ_ONLY`. No writes, no schema migrations, no listening-
  progress state on the server side (CloudKit / clients own that).
- **Static bearer key auth.** `SHELF_API_KEY` env value, compared in
  constant time. Rotate by changing env + restart, no DB row to maintain.
- **Optional.** Unset `SCRIBE_SHELF_URL` on the backend side = shelf
  status is just not surfaced in the UI. The backend doesn't depend on
  shelf for any of its own work.

Endpoints implemented (the subset that Listen This consumes):

| Path | Notes |
|---|---|
| `GET /ping` | unauthenticated liveness, what scribe pings for `shelf_healthy` |
| `GET /api/me` | identity stub + library access permissions |
| `GET /api/libraries` | single library entry derived from `SHELF_LIBRARY_NAME` |
| `GET /api/libraries/{id}/items` | paginated, supports `?search=` |
| `GET /api/items/{id}?expanded=1` | metadata + single synthesized track |
| `GET /api/items/{id}/file/{ino}` | Range-aware m4b stream from `SHELF_LIBRARY_DIR` |
| `GET /api/items/{id}/cover` | proxies the Audible CDN cover URL, 24h cache |

`item_id` is `<account_id>:<asin>` so US + UK editions of the same book
stay distinct. `ino` is a stable hash of the ASIN — opaque to clients.

scribe's settings page surfaces the URL + API key as copy-able fields
when both `SCRIBE_SHELF_URL` and `SCRIBE_SHELF_API_KEY` are set on the
backend. A logged-in user can paste them into Listen This (or any
ABS-compat client) directly.

Local dev:

```sh
cd shelf && bacon       # listens on 127.0.0.1:3006 by default
```

then in Listen This: server URL `http://<host>:3006`, api key from
`shelf/.env`.

## Status

Reconvert end-to-end works. OA-style file importer (drop a local AAX into
scribe and have it appear in the library) is the next big gap.
