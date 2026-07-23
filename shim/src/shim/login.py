"""Interactive login session manager.

mkb79's `Authenticator.from_login_external` is a synchronous call that
takes a `login_url_callback(url) -> redirect_url` closure. The lib calls
the closure with the Amazon sign-in URL and blocks until the closure
returns the URL the user was redirected to.

We need to split that across two HTTP requests:

    POST /login/start  → returns Amazon URL, parks the worker thread
    POST /login/finish → unblocks the worker thread with the redirect URL,
                         worker then completes register and returns auth

This module runs the from_login_external call in a worker thread per
session. The thread blocks inside the callback on a Queue.get() until
/login/finish puts the URL onto the queue.

Sessions live in memory — restart wipes in-flight logins (acceptable).
"""

from __future__ import annotations

import logging
import queue
import threading
import time
import uuid
from dataclasses import dataclass, field
from typing import Any

import audible

from . import accounts

log = logging.getLogger(__name__)

# Sentinel placed on the redirect queue when /login/cancel is called.
_CANCEL = object()


@dataclass
class _Session:
    session_id: str
    locale: str
    with_username: bool
    oauth_url: str | None = None
    oauth_url_ready: threading.Event = field(default_factory=threading.Event)
    redirect_queue: queue.Queue = field(default_factory=lambda: queue.Queue(maxsize=1))
    done: threading.Event = field(default_factory=threading.Event)
    result: dict[str, Any] | None = None
    error: str | None = None
    thread: threading.Thread | None = None
    created_at: float = field(default_factory=time.monotonic)


_sessions: dict[str, _Session] = {}
_sessions_lock = threading.Lock()

# Stale sessions are reaped after this many seconds in-flight.
SESSION_TTL_SECONDS = 15 * 60


def _reap_stale() -> None:
    """Drop sessions that never received a /login/finish call."""
    now = time.monotonic()
    with _sessions_lock:
        dead = [sid for sid, s in _sessions.items() if now - s.created_at > SESSION_TTL_SECONDS and not s.done.is_set()]
        for sid in dead:
            sess = _sessions.pop(sid, None)
            if sess is not None:
                try:
                    sess.redirect_queue.put_nowait(_CANCEL)
                except queue.Full:
                    pass


def _worker(sess: _Session) -> None:
    """Run inside a thread. Calls audible's blocking login, parks on the queue."""

    def login_url_callback(url: str) -> str:
        sess.oauth_url = url
        sess.oauth_url_ready.set()
        item = sess.redirect_queue.get()  # blocks until /login/finish or /login/cancel
        if item is _CANCEL:
            raise RuntimeError("login cancelled")
        return item  # the pasted redirect URL

    try:
        auth = audible.Authenticator.from_login_external(
            locale=sess.locale,
            with_username=sess.with_username,
            login_url_callback=login_url_callback,
        )
        customer = auth.customer_info or {}
        customer_id = customer.get("user_id") or customer.get("id") or ""
        account_id = accounts.derive_account_id(sess.locale, customer_id)
        accounts.save(account_id, auth)
        sess.result = {
            "account_id": account_id,
            "customer_name": customer.get("name"),
            "locale": sess.locale,
        }
    except Exception as exc:
        log.exception("login failed for session %s", sess.session_id)
        sess.error = str(exc)
    finally:
        # Make sure callers stuck on oauth_url_ready unblock with an error.
        sess.oauth_url_ready.set()
        sess.done.set()


def start(locale: str, with_username: bool) -> dict[str, Any]:
    _reap_stale()
    sid = str(uuid.uuid4())
    sess = _Session(session_id=sid, locale=locale, with_username=with_username)
    sess.thread = threading.Thread(target=_worker, args=(sess,), daemon=True, name=f"login-{sid}")
    with _sessions_lock:
        _sessions[sid] = sess
    sess.thread.start()
    # Wait briefly for build_oauth_url to fire the callback. ~100 ms typically.
    if not sess.oauth_url_ready.wait(timeout=15):
        return {"error": "timeout_building_oauth_url"}
    if sess.error is not None:
        return {"error": "login_start_failed", "detail": sess.error}
    return {
        "session_id": sid,
        "open_url": sess.oauth_url,
        "instructions": (
            "Open the URL in a browser. Sign in to Amazon and approve "
            "any 2FA prompt (push notification, OTP code, CAPTCHA — "
            "whatever Amazon throws at you).\n\n"
            "Amazon then redirects you to a page that looks broken or "
            "blank — usually 'Authentication problem' or just a white "
            "screen. That's expected. The URL in your browser's address "
            "bar is the part we need: copy the entire URL (it starts "
            "with https://www.amazon.* and contains 'openid.oa2."
            "authorization_code=…') and paste it here."
        ),
    }


def finish(session_id: str, redirect_url: str) -> dict[str, Any]:
    with _sessions_lock:
        sess = _sessions.get(session_id)
    if sess is None:
        return {"error": "unknown_session"}
    if sess.done.is_set():
        if sess.error:
            return {"error": "login_failed", "detail": sess.error}
        return sess.result or {}

    try:
        sess.redirect_queue.put_nowait(redirect_url)
    except queue.Full:
        return {"error": "session_already_finished"}

    sess.done.wait(timeout=60)
    with _sessions_lock:
        _sessions.pop(session_id, None)

    if sess.error:
        return {"error": "login_failed", "detail": sess.error}
    return sess.result or {"error": "no_result"}


def cancel(session_id: str) -> dict[str, Any]:
    with _sessions_lock:
        sess = _sessions.pop(session_id, None)
    if sess is None:
        return {"cancelled": False}
    if not sess.done.is_set():
        try:
            sess.redirect_queue.put_nowait(_CANCEL)
        except queue.Full:
            pass
        sess.done.wait(timeout=5)
    return {"cancelled": True}
