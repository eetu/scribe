# shim API contract (draft, task #1)

Python FastAPI sidecar wrapping `mkb79/audible` (≥ Jan 2026 release). Loopback `127.0.0.1:3004`. JSON in/out. No auth on the sidecar itself — only the Pi loopback can reach it. The Rust app caller is trusted.

All Audible-side state (tokens, RSA private key, cookies) lives in encrypted files on disk under `/var/lib/shim/accounts/`. Passphrase from `/etc/secrets/shim.env`.

---

## Account model

Multiple accounts. Each scoped by `account_id = sha256(region + ":" + email)[:16]`.

Auth file fields (mirrors `mkb79/audible.Authenticator.to_dict()`):

```
adp_token                       string  long-lived, device-bound, signing
device_private_key              string  RSA PEM, long-lived
access_token                    string  ~60 min
refresh_token                   string  ~1 yr
expires                         int     unix epoch when access_token dies
store_authentication_cookie     string
website_cookies                 object  Amazon session cookies
device_info                     object  marketplace + device metadata
customer_info                   object  account_id + name
locale_code                     string  us, uk, de, fr, jp, au, ca, it, in, es
with_username                   bool    legacy pre-Amazon Audible account
activation_bytes                string  deprecated, AAX-only
```

---

## Endpoints

### `GET /health`
Liveness. No deps touched.

```json
→ 200 { "ok": true, "version": "0.1.0", "audible_lib": "1.x.y" }
```

### `GET /accounts`
List local accounts.

```json
→ 200 [
  { "account_id": "ab12cd34", "locale": "us", "email_masked": "j***@example.com",
    "customer_name": "Jane Doe", "expires_at": 1748567890,
    "needs_refresh": false, "needs_relogin": false }
]
```

### `POST /login/start`
Begin interactive login. Uses `from_login_external` flow — most robust for current Amazon push-approval default.

```json
← { "locale": "us", "email": "j@example.com", "with_username": false }

→ 200 { "session_id": "uuid",
        "open_url": "https://amazon.com/ap/signin?openid....",
        "instructions": "Open URL in browser. Sign in. Approve push notification. Paste final redirect URL back." }
```

### `POST /login/finish`
User pasted the redirect URL after Amazon completed.

```json
← { "session_id": "uuid",
    "redirect_url": "https://amazon.com/ap/maplanding?openid.oa2.authorization_code=..." }

→ 200 { "account_id": "ab12cd34", "customer_name": "Jane Doe", "locale": "us" }
→ 400 { "error": "code_missing", "detail": "..." }
```

### `POST /login/cancel`
Drop session_id state. Idempotent.

### `POST /accounts/{account_id}/deregister`
Revoke device with Amazon + delete local file.

```json
→ 200 { "deregistered": true }
```

### `POST /accounts/{account_id}/refresh`
Force `refresh_access_token(force=True)`. Returns new expiry.

```json
→ 200 { "expires_at": 1748571490 }
→ 401 { "error": "refresh_failed", "needs_relogin": true }
```

### `GET /accounts/{account_id}/library`
List the account's library.

Query: `page` (default 1), `num_results` (default 50, max 1000), `status` (`Active`|`Revoked`, default both).

```json
→ 200 {
  "total_results": 412,
  "page": 1,
  "items": [
    {
      "asin": "B0...",
      "title": "Book Title",
      "subtitle": "...",
      "authors": ["Author Name"],
      "narrators": ["Narrator Name"],
      "series": [{ "title": "Series Name", "sequence": "3" }],
      "runtime_length_min": 480,
      "release_date": "2024-03-15",
      "publisher_name": "...",
      "language": "english",
      "cover_url": "https://m.media-amazon.com/.../cover.jpg",
      "is_aaxc": true,
      "is_aax": false,
      "is_listenable": true,
      "purchase_date": "2024-04-01T00:00:00Z",
      "status": "Active",
      "content_delivery_type": "MultiPartBook" | "SinglePartBook"
    }
  ]
}
```

Underlying call: `GET /1.0/library` with `response_groups=product_desc,product_attrs,media,series,product_extended_attrs,relationships,contributors,price`, `sort_by=-PurchaseDate`.

### `GET /accounts/{account_id}/books/{asin}/voucher`
Fetch AAXC license + content URL + chapters + voucher (decrypted key + iv).

Underlying: `POST /1.0/content/{asin}/licenserequest` body:
```json
{ "quality": "High",
  "consumption_type": "Download",
  "response_groups": "chapter_info,content_reference,certificate" }
```

Shim post-processes `license_response` with `AESCipher.decrypt_voucher(device_serial, customer_id, device_type, asin, encrypted)` (mkb79 does this internally) and returns:

```json
→ 200 {
  "asin": "B0...",
  "content_url": "https://download.audible.com/...AAX",
  "content_format": "mpeg",
  "codec": "mp4a.40.2",
  "key": "0123456789abcdef0123456789abcdef",   // 32 hex, AES-128 key
  "iv":  "fedcba9876543210fedcba9876543210",   // 32 hex
  "chapters": [
    { "title": "Chapter 1", "length_ms": 1234567, "start_offset_ms": 0 }
  ],
  "runtime_length_ms": 17280000,
  "cover_url": "https://.../cover.jpg",
  "refresh_date": "2026-08-21T00:00:00Z"
}
→ 410 { "error": "expired", "detail": "license refresh required" }
→ 403 { "error": "not_owned" }
```

### `GET /accounts/{account_id}/books/{asin}/metadata`
Chapter info + extended metadata (separate endpoint, useful when voucher already cached).

Underlying: `GET /1.0/content/{asin}/metadata?response_groups=chapter_info&chapter_titles_type=Flat&quality=High`.

```json
→ 200 { "chapters": [...], "runtime_length_ms": ..., "is_brand_intro_present": true }
```

### `GET /accounts/{account_id}/books/{asin}/pdf`
Returns supplemental PDF URL if attached, else 404.

```json
→ 200 { "url": "https://.../book.pdf", "filename": "book.pdf" }
→ 404
```

---

## Shared error shape

```json
{ "error": "snake_case_code", "detail": "human readable", "needs_relogin": false }
```

Status codes:
- 200 — ok
- 400 — bad input
- 401 — token invalid, includes `needs_relogin`
- 403 — owned-by-different-account or marketplace mismatch
- 404 — unknown asin / unknown account
- 410 — voucher/content expired, refresh + retry
- 502 — Amazon/Audible upstream error (passes through detail)
- 503 — shim busy / rate-limited

---

## Sequencing for scribe

```
boot:
  GET /health → ready
  GET /accounts → know what's logged in

new account flow:
  user picks region + types email
  POST /login/start → open_url
  show open_url to user, instruction copy
  user pastes redirect URL
  POST /login/finish → account_id
  GET /accounts/{id}/library → fill scribe.db

download a book:
  GET /accounts/{id}/books/{asin}/voucher
  POST to scribe-press with { content_url, key, iv, chapters, cover_url, filename }
  press streams fMP4 back
  scribe pipes to NAS

periodic:
  for each account whose `expires_at - now < 5min`:
    POST /accounts/{id}/refresh
```

---

## Not handled by shim (lives in scribe Rust)

- SQLite persistence of books / jobs
- Polling cadence for new books (default 5 min, configurable, smart-backoff overnight)
- NAS filesystem ops — TWO separate trees:
    - `SCRIBE_LIBRARY_DIR=/mnt/audiobooks/library` → `Author/Title/Title.m4b` (ABS reads this)
    - `SCRIBE_ORIGINAL_DIR=/mnt/audiobooks/original` → `Author/Title/Title.aaxc` (untouched Audible download, ABS never sees it)
- ffmpeg / streaming pipe (delegated to scribe-press)
- filename canonicalization (port from OA FileDestination.java)
- UI state, OIDC session, per-user data isolation
- worker (press) orchestration + auth
- audiobookshelf rescan trigger after M4B write (`POST {ABS_URL}/api/libraries/{id}/scan`)

---

## scribe-press contract addendum (added during task #5)

The press worker accepts both modern AAXC and legacy AAX DRM. The `drm`
field is a tagged union:

```json
{ "drm": "aaxc", "key_hex": "...", "iv_hex": "..." }
{ "drm": "aax",  "activation_bytes": "deadbeef" }
```

`activation_bytes` is the 8-hex-char account-wide secret used to
decrypt any AAX file purchased by a given Audible account. mkb79
exposes it via `auth.activation_bytes` once an account is registered.

`content_url` accepts `https://` (Audible CDN) or `file://` (local
path on the mini SSD, used for testing against the OpenAudible backlog
without going through the CDN). file:// short-circuits the download
step — same bytes copied into the job dir.

## Open questions for next task

- **Push-approval-only accounts**: confirm `from_login_external` is robust. If Amazon presents CAPTCHA mid-flow, that's still on Amazon's page in the user's browser — should "just work". Verify on real account during smoke test (task #14).
- **chapter source**: prefer chapters from licenserequest (one round-trip). Fall back to /metadata if response_groups doesn't return chapter_info for that title.
- **Refresh cadence**: 60-min access_token. Naive: refresh on every API call if `expires_at - now < 60s`. Lazy. mkb79 may already do this automatically — verify, drop our own refresh logic if so.
- **device_serial**: stored where? mkb79 derives from registration response. Verify present in `device_info` for AAXC decrypt input.
