# backend — Pi-side scribe service

axum 0.8 service running on the raspi. Hosts the React UI, owns SQLite
state, talks to shim (loopback :3004) for Audible-side work, and
orchestrates scribe-press (mini, HTTPS+bearer) for ffmpeg.

## Routes

| Method | Path | Notes |
|---|---|---|
| GET | `/status` | upstream health (shim + press), version |
| GET | `/auth/login` | OIDC start (or DEV_AUTH cookie set) |
| GET | `/auth/callback` | OIDC return |
| POST | `/auth/logout` | clear session |
| GET | `/api/me` | session probe |
| GET | `/api/accounts` | proxy GET shim /accounts |
| POST | `/api/accounts/login/start` | proxy POST shim /login/start |
| POST | `/api/accounts/login/finish` | proxy POST shim /login/finish |
| GET | `/api/library` | union of all accounts' books, joined with job state |
| POST | `/api/library/sync` | manual refresh trigger |
| GET | `/api/jobs` | recent jobs + active queue |
| POST | `/api/jobs` | enqueue download of a specific asin |
| GET | `/api/jobs/{id}/sse` | per-job progress stream |
| POST | `/api/jobs/{id}/cancel` | stop / dequeue |
| GET | `/api/reorg/preview` | walk NAS, propose renames |
| POST | `/api/reorg/commit` | apply selected renames |

## SQLite schema (initial)

```sql
CREATE TABLE accounts (
  id TEXT PRIMARY KEY,            -- shim's account_id
  locale TEXT NOT NULL,
  email_masked TEXT NOT NULL,
  customer_name TEXT,
  last_synced_at INTEGER,
  user_sub TEXT NOT NULL          -- OIDC subject — per-user isolation
);

CREATE TABLE books (
  asin TEXT NOT NULL,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  title TEXT NOT NULL,
  subtitle TEXT,
  authors_json TEXT NOT NULL,
  narrators_json TEXT NOT NULL,
  series_title TEXT,
  series_sequence TEXT,
  runtime_length_ms INTEGER,
  cover_url TEXT,
  status TEXT NOT NULL,            -- Active | Revoked
  purchase_date TEXT,
  first_seen_at INTEGER NOT NULL,
  PRIMARY KEY (asin, account_id)
);

CREATE TABLE jobs (
  id TEXT PRIMARY KEY,             -- uuid
  asin TEXT NOT NULL,
  account_id TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  error TEXT,
  m4b_path TEXT,
  aaxc_path TEXT
);
```

## Polling loop

Mimics human library opens, not a scraper timer. Each tick computes the
next sleep as `base · (1 ± jitter)` where base = `SCRIBE_POLL_INTERVAL_MIN`
(default 60) and jitter = `SCRIBE_POLL_JITTER_PERCENT / 100` (default 0.5).
Outside the active window — `SCRIBE_POLL_ACTIVE_HOUR_START` to
`SCRIBE_POLL_ACTIVE_HOUR_END` (default 07:00–23:00 local) — the loop
sleeps until the next start with a random 0–30 min wake jitter so multiple
hosts don't pile on at the same minute.

Each tick: for each account, GET shim
`/library?num_results=10&sort_by=-PurchaseDate`, compare against
`MAX(purchase_date) WHERE account_id=?`, enqueue any new ASINs as jobs
when the owner profile's effective `auto_enqueue` is true. Manual
`/api/library/sync` does a full paginated walk and bypasses the timer.

## First deploy

Linking an Audible account runs `sync::full` in the background and
populates `books` for every Active title. **No jobs are auto-queued
for the existing backlog** — the poller's incremental tick fires
auto-enqueue only when it observes a *new* book purchase, and after
the full sync `MAX(purchase_date)` already covers everything visible
right now. Two choices for the backlog:

- **"download all"** button on the library page — one confirm dialog,
  queues every Active book that has no job row. Worker takes it from
  there at the throttled pace below.
- **Per-book download** — same code path, scoped to one ASIN.

After that, any future Audible purchase triggers auto-enqueue on the
next poll tick (provided `SCRIBE_AUTO_ENQUEUE=1`). When it fires it
catches the new book *and* any older books still missing a job, so
forgetting to "download all" on day one isn't terminal — buying
anything new sweeps the backlog in.

A 51 GB library is roughly 100–200 books; at the default inter-job
floor (30–90 s jitter) and ~1–3 min per book, expect the backlog to
trickle over **6–24 hours**. Worker concurrency stays at 1 so it never
bursts. Throughput is intentionally low so each voucher fetch + CDN
download looks like a deliberate user action rather than a scraper
window. Raise `SCRIBE_JOB_CONCURRENCY` and/or zero
`SCRIBE_JOB_INTERJOB_DELAY_S` if you need it faster on a private
deploy behind a stable Amazon session.

## Auth model

kanidm is the bouncer. Anyone kanidm admits gets a scribe profile
auto-created on first call to `/api/me`. No role distinction, no
admin/user split, no closed-registration env. Cookie payload is
`sub|email` signed with `SESSION_KEY`. Sessions survive restarts as
long as `SESSION_KEY` stays put; the v1 schema doesn't persist
sessions anywhere else.

OIDC discovery is **lazy + retried**, not done at boot. kanidm may boot
concurrently with scribe; a one-shot boot discovery that failed would
leave auth wedged until a manual restart (and `/status` would still
return 200). Instead the first `/auth/login` and every `/status` poll
route through `OidcLazy::ctx`, which discovers + caches on demand and
re-attempts while the issuer is down — so it self-heals once kanidm is
up, no restart needed. `/status` reports `oidc_configured` + `oidc_ready`
(it stays 200 either way; a 503 there would just make the orchestrator
kill a container that can recover on its own). While configured but not
yet reachable, `/auth/login` returns a retryable 503 in prod rather than
silently downgrading to DEV_AUTH.

Profile model holds one row per OIDC sub. Audible accounts hang off
`accounts.profile_id` — same profile can own multiple regions of the
same Audible identity, and per-account isolation in queries always
joins through `accounts.profile_id`.

## Future: multi-library

v1 hardwires the output paths (`SCRIBE_LIBRARY_DIR`, `SCRIBE_ORIGINAL_DIR`)
via env, so one deployment serves exactly one library. The household
multi-user case — every kanidm user gets their own audiobookshelf
library on the NAS — is a real roadmap item:

- new `libraries` table: `id, name, library_path, original_path,
  filename_template_m4b, filename_template_original`
- `profile.library_id` FK pinning each profile to a library
- IaC seeds rows (one per kanidm person) + creates the directories
- UI gains a library-create flow for new kanidm users to claim a
  library on first login
- per-library audiobookshelf scan path (one `Volume=...` per library
  in the ABS quadlet) so each user's library is its own ABS root

Not v1. Document so the next pass doesn't reach for the role/admin
machinery that was already torn out.

## Environment

| Var | Default | Purpose |
|---|---|---|
| `SCRIBE_DB_PATH` | `scribe.db` | SQLite file |
| `SCRIBE_SHIM_URL` | `http://127.0.0.1:3004` | sidecar |
| `SCRIBE_PRESS_URL` | unset | mini-side worker base URL |
| `SCRIBE_PRESS_TOKEN` | unset | bearer for press auth |
| `SCRIBE_LIBRARY_DIR` | `/mnt/audiobooks/library` | M4B output root |
| `SCRIBE_ORIGINAL_DIR` | `/mnt/audiobooks/original` | untouched AAXC/AAX downloads from Audible |
| `SCRIBE_POLL_INTERVAL_MIN` | `60` | base poll cadence in minutes |
| `SCRIBE_POLL_JITTER_PERCENT` | `50` | uniform ± randomness on each interval |
| `SCRIBE_POLL_ACTIVE_HOUR_START` | `7` | poll window start (local hour 0-23) |
| `SCRIBE_POLL_ACTIVE_HOUR_END` | `23` | poll window end (local hour 0-23) |
| `ABS_URL` | unset | audiobookshelf base URL |
| `ABS_TOKEN` | unset | ABS API token |
| `ABS_LIBRARY_ID` | unset | ABS library id to rescan |
| `SCRIBE_FILENAME_TEMPLATE_M4B` | `{author?}/{series_title?}/[#{series_num} - ]{title}/{title}.m4b` | M4B path template — see filenaming.rs |
| `SCRIBE_FILENAME_TEMPLATE_ORIGINAL` | `{author?}/{series_title?}/{title}-{asin}.aaxc` | original-file path template |
| `SCRIBE_JOB_CONCURRENCY` | `1` | parallel ffmpeg jobs (raise on mini, leave at 1 on Pi) |
| `SCRIBE_JOB_RETRY_MAX` | `3` | transient-failure retry cap |
| `SCRIBE_AUTO_ENQUEUE` | `0` | poller auto-queues new books on discovery; `0` = manual download only (default), `1` = production auto-sync. Cold-start with `1`: see "First deploy" below — every book in the linked accounts gets queued. |
| `SCRIBE_JOB_INTERJOB_DELAY_S` | `60` | seconds the worker sleeps between jobs (mid-window inter-job pacing) |
| `SCRIBE_JOB_INTERJOB_JITTER_PERCENT` | `50` | uniform ± randomness on each inter-job sleep |
| `SESSION_KEY` | _required if `DEV_AUTH=0`_ | 64-byte hex (`openssl rand -hex 64`). Boot fails without it in prod; random per-boot key under `DEV_AUTH=1` |
| `DEV_AUTH` | `0` | dev login fallback |
| `OIDC_ISSUER` / `OIDC_CLIENT_ID` / `OIDC_CLIENT_SECRET` / `OIDC_REDIRECT_URL` | unset | kanidm config |
