"""AAXC voucher fetch + decrypt.

Audible's `licenserequest` endpoint returns an `encrypted` blob containing
the per-book AES key + iv. The decryption inputs are derived from the
device fingerprint that the Authenticator owns. mkb79's
`audible.aescipher.decrypt_voucher_from_licenserequest` does it.

Two scenarios depending on lib version: helper exists, or we run the
primitives ourselves. We try the helper first.
"""

from __future__ import annotations

from typing import Any

import audible

try:  # pragma: no cover — runtime dep version skew
    from audible.aescipher import (
        decrypt_voucher_from_licenserequest,  # type: ignore[attr-defined]
    )
except ImportError:  # pragma: no cover
    decrypt_voucher_from_licenserequest = None  # type: ignore[assignment]


_BODY = {
    "quality": "High",
    "consumption_type": "Download",
    "drm_type": "Adrm",
    "response_groups": "chapter_info,content_reference,certificate",
    "supported_media_features": {
        "codecs": ["mp4a.40.2", "mp4a.40.42", "ec+3", "ac-4"],
        "drm_types": ["Adrm"],
    },
}


def fetch(auth: audible.Authenticator, asin: str) -> dict[str, Any]:
    import logging
    log = logging.getLogger(__name__)
    with audible.Client(auth=auth) as client:
        lr = client.post(f"1.0/content/{asin}/licenserequest", body=_BODY)

    content_license = (lr or {}).get("content_license") or {}
    status = content_license.get("status_code")
    if status and status != "Granted":
        message = content_license.get("message", "")
        log.warning(
            "licenserequest returned status=%s for asin=%s: %s; keys=%s",
            status,
            asin,
            message,
            sorted((lr or {}).keys()),
        )
        # Don't echo `lr` back — it's the full encrypted license response
        # (key-adjacent material). The caller only needs status/message; the
        # rest is logged above for debugging.
        return {
            "_error": "license_not_granted",
            "_detail": f"{status}: {message}" if message else status,
        }

    metadata = content_license.get("content_metadata") or {}
    content_url = ((metadata.get("content_url") or {}).get("offline_url")) or ""
    ref = metadata.get("content_reference") or {}
    codec = ref.get("content_format", "mp4a.40.2")

    chapters = _chapters_from_metadata(metadata)
    runtime_ms = (metadata.get("chapter_info") or {}).get("runtime_length_ms") or 0

    if decrypt_voucher_from_licenserequest is None:
        return {
            "_error": "decrypt_helper_missing",
            "_detail": "audible.aescipher.decrypt_voucher_from_licenserequest not importable — pin a newer mkb79/audible",
            "content_url": content_url,
            "codec": codec,
            "chapters": chapters,
            "runtime_length_ms": runtime_ms,
        }

    voucher = decrypt_voucher_from_licenserequest(auth, lr)
    key = voucher.get("key", "")
    iv = voucher.get("iv", "")

    refresh_date = content_license.get("refresh_date")

    return {
        "asin": asin,
        "content_url": content_url,
        "codec": codec,
        "key": key,
        "iv": iv,
        "chapters": chapters,
        "runtime_length_ms": runtime_ms,
        "cover_url": None,  # cover comes from library endpoint
        "refresh_date": refresh_date,
    }


def fetch_metadata(auth: audible.Authenticator, asin: str) -> dict[str, Any]:
    params = {
        "response_groups": "chapter_info",
        "quality": "High",
        "chapter_titles_type": "Flat",
    }
    with audible.Client(auth=auth) as client:
        resp = client.get(f"1.0/content/{asin}/metadata", **params)
    md = (resp or {}).get("content_metadata") or {}
    return {
        "chapters": _chapters_from_metadata(md),
        "runtime_length_ms": (md.get("chapter_info") or {}).get("runtime_length_ms") or 0,
        "is_brand_intro_present": (md.get("chapter_info") or {}).get("is_brand_intro_present", False),
    }


def _chapters_from_metadata(metadata: dict[str, Any]) -> list[dict[str, Any]]:
    chapters_in = ((metadata.get("chapter_info") or {}).get("chapters")) or []
    return [
        {
            "title": c.get("title"),
            "start_offset_ms": int(c.get("start_offset_ms", 0)),
            "length_ms": int(c.get("length_ms", 0)),
        }
        for c in chapters_in
    ]
