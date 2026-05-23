"""Dev entrypoint for shim.

Loads `.env` from the working directory, then boots uvicorn with reload
enabled so editing any file under `src/shim/` restarts the server.
Production deploys (raspi IaC, task #12) run uvicorn directly under a
systemd unit with `EnvironmentFile=/etc/secrets/shim.env` — that path
skips this script.
"""

from __future__ import annotations

import os

from dotenv import load_dotenv
import uvicorn


def main() -> None:
    load_dotenv()  # picks up ./.env, no-op if missing
    host = os.environ.get("SHIM_HOST", "127.0.0.1")
    port = int(os.environ.get("SHIM_PORT", "3004"))
    reload = os.environ.get("SHIM_RELOAD", "1") != "0"
    uvicorn.run(
        "shim.main:app",
        host=host,
        port=port,
        reload=reload,
        reload_dirs=["src"] if reload else None,
        log_level=os.environ.get("SHIM_LOG", "info"),
    )


if __name__ == "__main__":
    main()
