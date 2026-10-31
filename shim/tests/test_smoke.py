"""Smoke and unit tests for the Python Audible shim."""

from __future__ import annotations

from unittest.mock import MagicMock, patch

import audible
import pytest
from fastapi.testclient import TestClient

from shim import accounts, library, voucher
from shim.main import app


@pytest.fixture
def client():
    return TestClient(app)


def test_health_reports_audible_version(client):
    resp = client.get("/health")
    assert resp.status_code == 200
    data = resp.json()
    assert data["ok"] is True
    assert data["version"] == "0.1.0"
    assert data["audible_lib"] == audible.__version__
    assert data["audible_lib"] not in ("unknown", "missing")


def test_auth_middleware(client, monkeypatch):
    monkeypatch.setattr("shim.main._SHIM_TOKEN", "secret-test-token")

    # /health is exempt
    resp = client.get("/health")
    assert resp.status_code == 200

    # /accounts requires bearer
    resp = client.get("/accounts")
    assert resp.status_code == 401
    assert resp.json() == {"error": "unauthorized"}

    resp = client.get("/accounts", headers={"Authorization": "Bearer wrong-token"})
    assert resp.status_code == 401

    resp = client.get("/accounts", headers={"Authorization": "Bearer secret-test-token"})
    assert resp.status_code == 200


def test_decrypt_voucher_helper_available():
    assert voucher.decrypt_voucher_from_licenserequest is not None
    assert callable(voucher.decrypt_voucher_from_licenserequest)


def test_account_id_derivation_and_traversal_guards(tmp_path, monkeypatch):
    monkeypatch.setenv("SHIM_DATA_DIR", str(tmp_path))
    monkeypatch.setenv("SHIM_PASSPHRASE", "test-passphrase-12345")

    aid = accounts.derive_account_id("us", "A2RZZD2X2R33TT")
    assert len(aid) == 16
    assert accounts._ACCOUNT_ID_RE.fullmatch(aid)

    # Path traversal attempts must raise KeyError
    with pytest.raises(KeyError):
        accounts._path_for("../escaped")
    with pytest.raises(KeyError):
        accounts._path_for("not-hex-id-1234")


def test_library_shape_book():
    raw_audible_item = {
        "asin": "B0BN6Q88BF",
        "title": "City of Last Chances",
        "subtitle": "A Novel",
        "authors": [{"name": "Adrian Tchaikovsky"}],
        "narrators": [{"name": "David Thorpe"}],
        "series": [{"title": "Ilmar", "sequence": "1"}],
        "runtime_length_min": 1150,
        "release_date": "2022-12-08",
        "publisher_name": "Head of Zeus",
        "language": "English",
        "product_images": {
            "500": "https://m.media-amazon.com/images/I/500.jpg",
            "1200": "https://m.media-amazon.com/images/I/1200.jpg",
        },
        "content_delivery_type": "MultiPartContent",
        "is_listenable": True,
        "purchase_date": "2026-10-31T07:00:00Z",
        "status": "Active",
    }
    shaped = library._shape_book(raw_audible_item)
    assert shaped["asin"] == "B0BN6Q88BF"
    assert shaped["title"] == "City of Last Chances"
    assert shaped["authors"] == ["Adrian Tchaikovsky"]
    assert shaped["narrators"] == ["David Thorpe"]
    assert shaped["series"] == [{"title": "Ilmar", "sequence": "1"}]
    assert shaped["cover_url"] == "https://m.media-amazon.com/images/I/1200.jpg"
    assert shaped["is_aaxc"] is True
    assert shaped["is_aax"] is False
    assert shaped["is_listenable"] is True


def test_voucher_fetch_denied():
    mock_client = MagicMock()
    mock_client.__enter__.return_value = mock_client
    mock_client.post.return_value = {
        "content_license": {
            "status_code": "Denied",
            "message": "License not granted to customer [A123] for asin [B0BN6Q88BF]",
        }
    }
    with patch("audible.Client", return_value=mock_client):
        res = voucher.fetch(MagicMock(), "B0BN6Q88BF")
        assert res["_error"] == "license_not_granted"
        assert "Denied" in res["_detail"]
        assert "License not granted" in res["_detail"]


def test_voucher_fetch_granted():
    mock_client = MagicMock()
    mock_client.__enter__.return_value = mock_client
    mock_client.post.return_value = {
        "content_license": {
            "status_code": "Granted",
            "refresh_date": "2026-11-01T00:00:00Z",
            "content_metadata": {
                "content_url": {"offline_url": "https://cdn.audible.com/aaxc/B0BN6Q88BF.aaxc"},
                "content_reference": {"content_format": "mp4a.40.2"},
                "chapter_info": {
                    "runtime_length_ms": 69000000,
                    "chapters": [
                        {"title": "Chapter 1", "start_offset_ms": 0, "length_ms": 300000},
                        {"title": "Chapter 2", "start_offset_ms": 300000, "length_ms": 400000},
                    ],
                },
            },
        }
    }
    with (
        patch("audible.Client", return_value=mock_client),
        patch(
            "shim.voucher.decrypt_voucher_from_licenserequest",
            return_value={"key": "0123456789abcdef0123456789abcdef", "iv": "abcdef0123456789abcdef0123456789"},
        ),
    ):
        res = voucher.fetch(MagicMock(), "B0BN6Q88BF")
        assert res["asin"] == "B0BN6Q88BF"
        assert res["key"] == "0123456789abcdef0123456789abcdef"
        assert res["iv"] == "abcdef0123456789abcdef0123456789"
        assert res["content_url"] == "https://cdn.audible.com/aaxc/B0BN6Q88BF.aaxc"
        assert res["codec"] == "mp4a.40.2"
        assert len(res["chapters"]) == 2
        assert res["chapters"][0]["title"] == "Chapter 1"
        assert res["refresh_date"] == "2026-11-01T00:00:00Z"
