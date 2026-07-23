# scribe task runner. `just` with no args lists recipes.
# Yarn = the repo-vendored release (yarnPath in frontend/.yarnrc.yml), run via
# node — no global yarn / corepack needed, and it auto-tracks `yarn set version`.
yarn := "node " + justfile_directory() / "frontend" / `awk '/^yarnPath:/{print $2}' frontend/.yarnrc.yml`

default:
    @just --list

# Install all deps: frontend (yarn) + the Python shim (uv).
install:
    cd frontend && {{yarn}} install
    cd shim && uv sync

# Dev the whole stack in parallel; one Ctrl-C tears it all down.
dev:
    #!/usr/bin/env bash
    # Services: shim 127.0.0.1:3004 (uv) · backend :3003 (DEV_AUTH, backend/.env)
    #           press :3005 · frontend :5173 (Vite → proxies /api,/auth,/status)
    # Teardown kills each child + its grandchildren; never `kill 0` (hits `just`).
    set -euo pipefail
    pids=""
    cleanup() {
        trap - INT TERM EXIT
        for p in $pids; do
            pkill -P "$p" 2>/dev/null || true
            kill "$p" 2>/dev/null || true
        done
    }
    trap cleanup INT TERM EXIT
    ( cd shim && exec uv run uvicorn shim.main:app --host 127.0.0.1 --port 3004 ) &
    pids="$pids $!"
    ( cd backend && DEV_AUTH=1 exec cargo run -p scribe-backend ) &
    pids="$pids $!"
    ( cd press && exec cargo run -p scribe-press ) &
    pids="$pids $!"
    ( cd frontend && exec {{yarn}} dev ) &
    pids="$pids $!"
    wait

# Build the SPA then the release binaries.
build:
    cd frontend && {{yarn}} build
    cargo build --release --workspace

# Lint + format + typecheck everything (what the pre-commit hook runs).
check:
    cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all -- --check
    cd frontend && {{yarn}} validate

# Run all tests (rust workspace; the SPA has no unit suite).
test:
    cargo test --workspace
