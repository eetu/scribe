# shim — Audible sidecar

Thin Python FastAPI wrapper around [mkb79/audible](https://github.com/mkb79/Audible)
(actively maintained, last docs update 2026-01-12, handles current Amazon
push-approval login flow). Pure passthrough plus account-file persistence.

Runs on the Pi loopback at `127.0.0.1:3004`. No external auth on the
service itself — the only reachable client is the Rust app on the same
host. Audible credentials sit on disk under
`/var/lib/shim/accounts/<account_id>.json`, encrypted with a
passphrase from `/etc/secrets/shim.env`.

## See also

- `API.md` — frozen endpoint contract used by the Rust app.

## Source of truth for endpoint shapes

`API.md` is the contract. Python code must match it. If the underlying
mkb79 lib evolves (e.g., Amazon adds a new auth wrinkle), this shim
absorbs the change without breaking the contract.

## Layout

```
src/shim/
├── __init__.py
├── main.py            # FastAPI app, route table, dependency wiring
├── accounts.py        # file persistence, encryption, account_id derivation
├── library.py         # GET /library impl, response shaping
├── voucher.py         # GET /voucher impl, decrypts license_response
└── login.py           # /login/start + /login/finish session state machine
```

## Running

```sh
uv sync
uv run uvicorn shim.main:app --host 127.0.0.1 --port 3004
```

## Operational notes

- Single-process is fine. Audible API has no parallelism win for the
  workloads scribe drives (low QPS, sequential per account).
- A single mkb79 `audible.Client` per account is kept warm in memory so
  cookies + access_token stay live across requests.
- `expires_at - now < 60s` → call `refresh_access_token(force=True)` lazily
  inside the request path. Don't run a separate refresh timer; mkb79
  handles most of this internally.
