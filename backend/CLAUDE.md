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
| `SCRIBE_AUTO_ENQUEUE` | `0` | poller auto-queues new books on discovery; `0` = manual download only (default), `1` = production auto-sync |
| `SESSION_KEY` | _ephemeral_ | 64-byte hex |
| `DEV_AUTH` | `0` | dev login fallback |
| `OIDC_ISSUER` / `OIDC_CLIENT_ID` / `OIDC_CLIENT_SECRET` / `OIDC_REDIRECT_URL` | unset | kanidm config |
