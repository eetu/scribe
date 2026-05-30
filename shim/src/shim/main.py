"""shim FastAPI entrypoint."""

from __future__ import annotations

import logging
from typing import Any

from fastapi import FastAPI, HTTPException, Query
from pydantic import BaseModel

from . import __version__, accounts, library, login, voucher

logging.basicConfig(level=logging.INFO)
log = logging.getLogger(__name__)

app = FastAPI(title="shim", version=__version__)


# ---------- request bodies ----------


class LoginStartIn(BaseModel):
    locale: str
    email: str | None = None
    with_username: bool = False


class LoginFinishIn(BaseModel):
    session_id: str
    redirect_url: str


class LoginCancelIn(BaseModel):
    session_id: str


# ---------- helpers ----------


def _audible_version() -> str:
    try:
        import audible

        return getattr(audible, "__version__", "unknown")
    except Exception:  # pragma: no cover
        return "missing"


def _http_error(status: int, code: str, detail: str = "", needs_relogin: bool = False) -> HTTPException:
    return HTTPException(
        status_code=status,
        detail={"error": code, "detail": detail, "needs_relogin": needs_relogin},
    )


def _safe_detail(exc: Exception) -> str:
    # Don't surface raw upstream exception text in the HTTP response —
    # mkb79/httpx messages can embed request URLs, headers, or response
    # bodies (token fragments, signed CDN URLs). The class name is enough to
    # disambiguate for the caller; the full exception is in the server log
    # via the preceding log.exception(...).
    return type(exc).__name__


def _load_or_404(account_id: str):
    try:
        return accounts.load(account_id)
    except KeyError:
        raise _http_error(404, "unknown_account", f"no account {account_id} on disk")


# ---------- routes ----------


@app.get("/health")
def health() -> dict[str, Any]:
    return {"ok": True, "version": __version__, "audible_lib": _audible_version()}


@app.get("/accounts")
def list_accounts() -> list[dict[str, Any]]:
    out = []
    for aid in accounts.list_account_ids():
        try:
            out.append(accounts.summary(aid))
        except Exception as exc:  # noqa: BLE001
            log.warning("summary failed for %s: %s", aid, exc)
    return out


@app.post("/login/start")
def login_start(body: LoginStartIn) -> dict[str, Any]:
    res = login.start(locale=body.locale.lower(), with_username=body.with_username)
    if "error" in res:
        raise _http_error(502, res["error"], res.get("detail", ""))
    return res


@app.post("/login/finish")
def login_finish(body: LoginFinishIn) -> dict[str, Any]:
    res = login.finish(session_id=body.session_id, redirect_url=body.redirect_url)
    if "error" in res:
        raise _http_error(400, res["error"], res.get("detail", ""))
    return res


@app.post("/login/cancel")
def login_cancel(body: LoginCancelIn) -> dict[str, Any]:
    return login.cancel(session_id=body.session_id)


@app.post("/accounts/{account_id}/deregister")
def deregister(account_id: str) -> dict[str, Any]:
    auth = _load_or_404(account_id)
    try:
        auth.deregister_device()
    except Exception as exc:  # noqa: BLE001
        log.exception("deregister failed for %s", account_id)
        raise _http_error(502, "deregister_failed", _safe_detail(exc))
    accounts.evict(account_id)
    return {"deregistered": True}


@app.post("/accounts/{account_id}/refresh")
def refresh(account_id: str) -> dict[str, Any]:
    auth = _load_or_404(account_id)
    try:
        auth.refresh_access_token(force=True)
    except Exception as exc:  # noqa: BLE001
        log.exception("refresh failed for %s", account_id)
        raise _http_error(401, "refresh_failed", _safe_detail(exc), needs_relogin=True)
    accounts.save(account_id, auth)
    return {"expires_at": int(auth.expires) if auth.expires else 0}


@app.get("/accounts/{account_id}/library")
def get_library(
    account_id: str,
    page: int = 1,
    num_results: int = Query(50, ge=1, le=1000),
    status: str | None = None,
) -> dict[str, Any]:
    auth = _load_or_404(account_id)
    try:
        return library.fetch(auth, page=page, num_results=num_results, status=status)
    except Exception as exc:  # noqa: BLE001
        log.exception("library fetch failed for %s", account_id)
        raise _http_error(502, "library_failed", _safe_detail(exc))


@app.get("/accounts/{account_id}/books/{asin}/voucher")
def get_voucher(account_id: str, asin: str) -> dict[str, Any]:
    auth = _load_or_404(account_id)
    try:
        res = voucher.fetch(auth, asin)
    except Exception as exc:  # noqa: BLE001
        log.exception("voucher fetch failed for %s/%s", account_id, asin)
        raise _http_error(502, "voucher_failed", _safe_detail(exc))
    if "_error" in res:
        raise _http_error(410 if res["_error"] == "license_not_granted" else 502, res["_error"], res.get("_detail", ""))
    return res


@app.get("/accounts/{account_id}/books/{asin}/metadata")
def get_metadata(account_id: str, asin: str) -> dict[str, Any]:
    auth = _load_or_404(account_id)
    try:
        return voucher.fetch_metadata(auth, asin)
    except Exception as exc:  # noqa: BLE001
        log.exception("metadata fetch failed for %s/%s", account_id, asin)
        raise _http_error(502, "metadata_failed", _safe_detail(exc))


@app.get("/accounts/{account_id}/books/{asin}/pdf")
def get_pdf(account_id: str, asin: str) -> dict[str, Any]:
    # mkb79 doesn't surface PDF directly; treat as 404 until lib helper lands.
    raise _http_error(404, "pdf_not_implemented", "Pi-side PDF retrieval lands later")
