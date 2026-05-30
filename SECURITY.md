# Security

scribe is a self-hosted, single-household Audible mirror. It runs on a home
Pi behind kanidm OIDC and a reverse proxy — it is **not** a public,
multi-tenant service. The threat model is: keep a household member's Audible
credentials and library isolated from the rest of the LAN and from other
household users, and survive an honest operator misconfiguration without
silently leaking secrets.

## Trust boundaries

- **Audible credentials live only in the shim.** RSA device key, refresh
  token, and Amazon cookies sit in `shim`'s on-disk account files, AES-
  encrypted with `SHIM_PASSPHRASE`. The Rust app never parses them — it
  talks to the shim over loopback and keeps working from cached library
  data if the shim is down. A shim compromise/restart fails new fetches,
  not the whole app.

- **Per-user isolation.** Each kanidm `sub` maps to one `profile`; Audible
  accounts, books, and jobs are scoped through `accounts.profile_id`. Every
  data query joins through it — no admin role, no shared state. Cross-user
  reads were audited and closed (e.g. `book_cover` is auth-gated and
  ownership-scoped).

- **Sessions.** A signed cookie (`SESSION_KEY`, HMAC) carries `sub|email`;
  no server-side session store. The backend **fails closed**: it refuses to
  boot with `DEV_AUTH=0` and no `SESSION_KEY`, because a predictable key
  would let anyone forge a session. Under `DEV_AUTH=1` a random per-boot key
  is used (sessions drop on restart). Cookies are `Secure` + `HttpOnly` +
  `SameSite=Lax` in prod.

- **OIDC.** Discovery is lazy + self-healing (kanidm may boot after scribe);
  state/nonce/PKCE are validated; the OIDC HTTP client disables redirects
  (SSRF guard). While the issuer is unreachable, `/auth/login` returns a
  retryable 503 rather than downgrading to `DEV_AUTH`.

- **Inter-service auth.**
  - `press` (ffmpeg worker): `PRESS_TOKEN` bearer on every route but
    `/health`. `file://` content URLs are rejected unless
    `PRESS_ALLOW_FILE_URL=1` (dev-only).
  - `shim`: loopback bind is the primary control; an optional `SHIM_TOKEN`
    bearer (sent by the backend via `SCRIBE_SHIM_TOKEN`) is required on every
    route but `/health` — defense-in-depth for a firewall slip / sidecar
    escape.
  - `shelf` (read-only ABS-compat): `SHELF_API_KEY` bearer. The `?token=`
    query form is accepted **only** on the audio-stream route (AVFoundation
    can't set a header on a media URL); all JSON/metadata routes are
    header-only so the key stays out of access logs.

- **Two NAS trees.** `SCRIBE_LIBRARY_DIR` (canonical M4B, scanned by
  audiobookshelf) and `SCRIBE_ORIGINAL_DIR` (untouched downloads + voucher
  sidecars, never scanned). Author/title path segments from Audible metadata
  are sanitized before they hit the filesystem.

- **Outbound fetches.** Cover proxying (backend + shelf) is restricted to
  `https` on an Amazon/Audible host allowlist with a size cap; all clients
  use verified TLS (rustls / httpx defaults).

## Secrets

All secrets are injected at runtime via env (never baked into images or
committed): `SESSION_KEY`, `SHIM_PASSPHRASE`, `SHIM_TOKEN`, `PRESS_TOKEN`,
`SHELF_API_KEY`, `SCRIBE_SHIM_TOKEN`, `OIDC_CLIENT_SECRET`, `ABS_TOKEN`. The
`.env.example` files hold placeholders only; `.env`, `*.db*`, and the shim
account/data dirs are gitignored. Containers run as a non-root UID.

## Accepted risks

These are known and intentionally not "fixed" — documented so the choice is
deliberate, not forgotten.

### 1. ffmpeg decryption key visible in the process table (press host)

The per-book AES key + IV are passed to ffmpeg as command-line args
(`-audible_key <hex> -audible_iv <hex>`), so they're visible in
`ps auxww` / `/proc/<pid>/cmdline` to any local user on the **press host**
for the duration of a conversion.

- **Why accepted:** ffmpeg has no env-var or file alternative for these
  inputs. The press host is a trusted single-tenant box on the LAN; the key
  is per-book and ephemeral (the job is swept within 24h, and the key is
  useless without the matching AAXC).
- **Revisit if:** press ever runs on a shared/multi-user host, or ffmpeg
  gains a non-argv way to pass the key.

### 2. Dev passphrase under `SHIM_DEV=1`

With `SHIM_DEV=1`, shim account files are encrypted with a built-in constant
passphrase (`dev-passphrase-do-not-ship`) so `uv run shim` works without a
configured secret. Files written in that mode are effectively plaintext to
anyone with the source.

- **Why accepted:** it's a local-dev convenience; production sets
  `SHIM_PASSPHRASE` (from the secret store) and never `SHIM_DEV`.
- **Operator action:** never set `SHIM_DEV=1` on the Pi. If a dev box ever
  linked a *real* Audible account, treat those `data/accounts/*.json` as
  compromised — deregister + re-link under a real passphrase.

## Out of scope

Multi-tenant hardening, rate-limiting against a hostile internal user, and
playback (handled by audiobookshelf). DRM is stripped only within the
purchaser's rights.

## Reporting

This is a personal project. Flag an issue privately to the maintainer rather
than opening a public issue with exploit detail.
