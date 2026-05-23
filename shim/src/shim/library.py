"""GET /library wrapper.

Calls `1.0/library` with a response_groups set that's wide enough to
populate the BookMeta the Rust app needs. Shape the response to match
shim/API.md.
"""

from __future__ import annotations

from typing import Any

import audible

_RESPONSE_GROUPS = ",".join(
    [
        "product_desc",
        "product_attrs",
        "product_extended_attrs",
        "media",
        "series",
        "contributors",
        "relationships",
        "price",
    ]
)


def fetch(auth: audible.Authenticator, page: int, num_results: int, status: str | None) -> dict[str, Any]:
    params: dict[str, Any] = {
        "num_results": num_results,
        "page": page,
        "response_groups": _RESPONSE_GROUPS,
        "sort_by": "-PurchaseDate",
    }
    if status:
        params["status"] = status
    with audible.Client(auth=auth) as client:
        resp = client.get("1.0/library", **params)
    items = resp.get("items") or []
    total = resp.get("total_results") or len(items)
    return {
        "total_results": total,
        "page": page,
        "items": [_shape_book(b) for b in items],
    }


def _shape_book(b: dict[str, Any]) -> dict[str, Any]:
    series = b.get("series") or []
    series_shaped = [{"title": s.get("title"), "sequence": s.get("sequence")} for s in series]
    authors = [a.get("name") for a in (b.get("authors") or []) if a.get("name")]
    narrators = [n.get("name") for n in (b.get("narrators") or []) if n.get("name")]
    runtime_min = b.get("runtime_length_min") or 0
    cover = None
    images = b.get("product_images") or {}
    # Prefer largest available; mkb79 returns dict keyed by pixel-width strings.
    if images:
        try:
            largest = max(images.keys(), key=lambda k: int(k))
            cover = images[largest]
        except ValueError:
            cover = next(iter(images.values()), None)
    return {
        "asin": b.get("asin"),
        "title": b.get("title"),
        "subtitle": b.get("subtitle"),
        "authors": authors,
        "narrators": narrators,
        "series": series_shaped,
        "runtime_length_min": runtime_min,
        "release_date": b.get("release_date") or b.get("issue_date"),
        "publisher_name": b.get("publisher_name"),
        "language": b.get("language"),
        "cover_url": cover,
        "is_aaxc": (b.get("content_delivery_type") or "").lower() != "singlepartcontent",
        "is_aax": False,  # legacy, rare on accounts logged in via 2026 flow
        "is_listenable": b.get("is_listenable", True),
        "purchase_date": b.get("purchase_date"),
        "status": b.get("status", "Active"),
        "content_delivery_type": b.get("content_delivery_type"),
    }
