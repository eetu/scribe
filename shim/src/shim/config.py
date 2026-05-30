"""Runtime config — env vars + filesystem paths."""

from __future__ import annotations

import os
from pathlib import Path


def accounts_dir() -> Path:
    """Where encrypted account files live."""
    p = Path(os.environ.get("SHIM_DATA_DIR", "./data")) / "accounts"
    p.mkdir(parents=True, exist_ok=True)
    return p


def token() -> str | None:
    """Optional shared-secret bearer. When set, every request except
    `/health` must carry `Authorization: Bearer <token>`. Unset = the shim
    trusts its loopback binding alone (a warning is logged at startup).
    Defense-in-depth for a firewall slip / sidecar escape, since the shim
    holds Audible credentials.
    """
    return os.environ.get("SHIM_TOKEN") or None


def passphrase() -> str:
    """Passphrase used to encrypt the on-disk auth files.

    Mandatory in any non-dev deployment. The dev fallback exists so
    `uv run` works without `/etc/secrets/`. Never ship the fallback to
    the Pi — the deploy task wires it from BW.
    """
    pw = os.environ.get("SHIM_PASSPHRASE")
    if pw:
        return pw
    if os.environ.get("SHIM_DEV") == "1":
        return "dev-passphrase-do-not-ship"
    raise RuntimeError(
        "SHIM_PASSPHRASE not set. Refusing to start without an encryption secret. "
        "Set SHIM_DEV=1 only when running locally."
    )
