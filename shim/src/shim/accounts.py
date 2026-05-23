"""Account file persistence + account_id derivation.

Audible auth lives in `audible.Authenticator` objects. We persist them to
disk encrypted by `Authenticator.to_file(..., password=..., encryption="bytes")`
using a passphrase from env. Each account gets one file keyed by
`account_id = sha256(locale + ":" + customer_id)[:16]`.

The shim keeps a process-local cache of loaded Authenticators so each
request reuses cookies + access_token. Cache is bounded by the number of
accounts a user has — small.
"""

from __future__ import annotations

import hashlib
import logging
import threading
from pathlib import Path
from typing import Any

import audible

from .config import accounts_dir, passphrase

log = logging.getLogger(__name__)

_cache: dict[str, audible.Authenticator] = {}
_cache_lock = threading.Lock()


def derive_account_id(locale: str, customer_id: str) -> str:
    h = hashlib.sha256(f"{locale}:{customer_id}".encode()).hexdigest()
    return h[:16]


def _path_for(account_id: str) -> Path:
    return accounts_dir() / f"{account_id}.json"


def list_account_ids() -> list[str]:
    return sorted(p.stem for p in accounts_dir().glob("*.json"))


def load(account_id: str) -> audible.Authenticator:
    with _cache_lock:
        cached = _cache.get(account_id)
        if cached is not None:
            return cached
    path = _path_for(account_id)
    if not path.exists():
        raise KeyError(account_id)
    auth = audible.Authenticator.from_file(
        path,
        password=passphrase(),
        encryption="bytes",
    )
    with _cache_lock:
        _cache[account_id] = auth
    return auth


def save(account_id: str, auth: audible.Authenticator) -> None:
    path = _path_for(account_id)
    auth.to_file(
        path,
        password=passphrase(),
        encryption="bytes",
        indent=None,
    )
    with _cache_lock:
        _cache[account_id] = auth


def evict(account_id: str) -> None:
    with _cache_lock:
        _cache.pop(account_id, None)
    path = _path_for(account_id)
    if path.exists():
        path.unlink()


def summary(account_id: str) -> dict[str, Any]:
    auth = load(account_id)
    customer = auth.customer_info or {}
    email = customer.get("email", "")
    masked = _mask_email(email)
    return {
        "account_id": account_id,
        "locale": getattr(auth.locale, "country_code", None) or auth.locale_code,
        "email_masked": masked,
        "customer_name": customer.get("name"),
        "expires_at": int(auth.expires) if auth.expires else None,
        "needs_refresh": _needs_refresh(auth),
        "needs_relogin": False,  # set true elsewhere if refresh fails
    }


def _mask_email(email: str) -> str:
    if not email or "@" not in email:
        return ""
    local, _, host = email.partition("@")
    if len(local) <= 1:
        return f"{local}***@{host}"
    return f"{local[0]}***@{host}"


def _needs_refresh(auth: audible.Authenticator) -> bool:
    import time

    if not auth.expires:
        return False
    return float(auth.expires) - time.time() < 60
