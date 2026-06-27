# syntax=docker/dockerfile:1

# --- Cross-compilation helper ---
FROM --platform=$BUILDPLATFORM tonistiigi/xx AS xx

# --- Stage 1: Build frontend (native, output is platform-independent) ---
FROM --platform=$BUILDPLATFORM node:24-alpine AS frontend-build
ARG SCRIBE_IMAGE_TAG
ENV VITE_SCRIBE_IMAGE_TAG=$SCRIBE_IMAGE_TAG
WORKDIR /app
COPY frontend/package.json frontend/yarn.lock frontend/.yarnrc.yml* ./
RUN corepack enable && yarn install --immutable --network-timeout 1000000
COPY frontend/ .
RUN yarn build

# --- Stage 2: Build workspace dependencies (native, cross-compiled) ---
#
# Compiles all transitive deps for every workspace member using stub
# sources so the dep-build cache stays warm across binary stages.
FROM --platform=$BUILDPLATFORM rust:1-alpine AS workspace-deps
COPY --from=xx / /
RUN apk add --no-cache clang lld musl-dev curl
ARG TARGETPLATFORM
RUN xx-apk add --no-cache musl-dev gcc
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY backend/Cargo.toml backend/Cargo.toml
COPY press/Cargo.toml press/Cargo.toml
COPY shared/Cargo.toml shared/Cargo.toml
COPY shelf/Cargo.toml shelf/Cargo.toml
COPY e2e/Cargo.toml e2e/Cargo.toml
# Stub sources for every workspace member — cargo must parse all member
# manifests to load the workspace, even ones we don't ship. `e2e` is a
# test-only crate (not built here), so it just needs a stub lib to exist;
# we build only the three shipped crates so e2e's test deps stay out of the
# image dep cache. (Press isn't shipped in the backend image but is built
# here so its dep layer is warm for the optional press image.)
RUN mkdir -p backend/src press/src shared/src shelf/src e2e/src \
    && printf 'fn main() {}\n' > backend/src/main.rs \
    && : > backend/src/lib.rs \
    && printf 'fn main() {}\n' > press/src/main.rs \
    && printf 'fn main() {}\n' > shelf/src/main.rs \
    && : > shelf/src/lib.rs \
    && : > shared/src/lib.rs \
    && : > e2e/src/lib.rs \
    && xx-cargo build --release -p scribe-backend -p scribe-press -p scribe-shelf

# --- Stage 3a: Build scribe-backend ---
FROM workspace-deps AS backend-build
ARG TARGETPLATFORM
COPY shared/src ./shared/src
COPY backend/src ./backend/src
# `touch` so cargo notices the stub→real source swap. Workspace shares
# a target dir so only the changed package rebuilds.
RUN touch shared/src/lib.rs backend/src/main.rs backend/src/lib.rs \
    && xx-cargo build --release -p scribe-backend

# --- Stage 3b: Build scribe-press ---
#
# Press normally runs as a native binary on its host (built via cargo,
# deployed via launchd/systemd by the host IaC). This image exists as a
# fallback for any host where a native binary isn't an option (e.g. a
# containers-only box). Skipped from the default CI gate unless the
# press image flag is flipped on.
FROM workspace-deps AS press-build
ARG TARGETPLATFORM
COPY shared/src ./shared/src
COPY press/src ./press/src
RUN touch shared/src/lib.rs press/src/main.rs \
    && xx-cargo build --release -p scribe-press

# --- Stage 3c: Build scribe-shelf ---
#
# Optional read-only ABS-compat sidecar — share scribe's SQLite for
# external clients (Listen This, etc) without exposing scribe's UI.
FROM workspace-deps AS shelf-build
ARG TARGETPLATFORM
COPY shared/src ./shared/src
COPY shelf/src ./shelf/src
RUN touch shared/src/lib.rs shelf/src/main.rs shelf/src/lib.rs \
    && xx-cargo build --release -p scribe-shelf

# --- Stage 4: Backend runtime ---
FROM scratch AS runner
WORKDIR /app
LABEL org.opencontainers.image.description="the path of the righteous scribe — self-hosted Audible mirror"
LABEL org.opencontainers.image.source="https://github.com/eetu/scribe"

COPY --from=backend-build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=backend-build /app/target/*/release/scribe-backend ./scribe-backend
COPY --from=frontend-build /app/dist ./dist

# Sensible runtime defaults — override via -e at run time.
ENV STATIC_DIR=./dist
ENV SCRIBE_DB_PATH=/data/scribe.db
ENV SCRIBE_BIND=0.0.0.0:3003

USER 1000

EXPOSE 3003

CMD ["./scribe-backend"]

# --- Stage 5: press runtime (Rust ffmpeg worker) ---
#
# ffmpeg comes from the distro. Alpine ships a recent enough ffmpeg for
# `-audible_key` / `-activation_bytes`, and stays tiny — image lands
# around 80MB vs. Debian's 400MB. Bind to 0.0.0.0 by default since the
# image is expected to sit behind a reverse proxy with bearer auth.
FROM alpine:3.24.1 AS press-runner
WORKDIR /app
LABEL org.opencontainers.image.description="scribe-press — DRM strip + remux worker for scribe"
LABEL org.opencontainers.image.source="https://github.com/eetu/scribe"

RUN apk add --no-cache ffmpeg ca-certificates

COPY --from=press-build /app/target/*/release/scribe-press ./scribe-press

ENV PRESS_BIND=0.0.0.0:3005
ENV PRESS_TMP_DIR=/tmp/scribe-press

USER 1000

EXPOSE 3005

CMD ["./scribe-press"]

# --- Stage 6: shelf runtime (Rust, scratch) ---
#
# Read-only ABS-compatible sidecar. Mounts scribe.db read-only and the
# library tree read-only — no writable surface inside the container.
FROM scratch AS shelf-runner
WORKDIR /app
LABEL org.opencontainers.image.description="scribe-shelf — read-only ABS-compatible view of scribe's library"
LABEL org.opencontainers.image.source="https://github.com/eetu/scribe"

COPY --from=shelf-build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=shelf-build /app/target/*/release/scribe-shelf ./scribe-shelf

ENV SHELF_BIND=0.0.0.0:3006
ENV SHELF_DB_PATH=/data/scribe.db
ENV SHELF_LIBRARY_DIR=/library

USER 1000

EXPOSE 3006

CMD ["./scribe-shelf"]

# --- Stage 7: shim runtime (Python) ---
#
# Shim is Pi-side too. Keeps mkb79/audible's Python deps isolated from the
# Rust backend so a rotting Audible auth flow swap doesn't force the Rust
# image to rebuild.
FROM python:3.15.0b3-slim AS shim-runner
WORKDIR /app
LABEL org.opencontainers.image.description="scribe-shim — Audible auth + library + voucher sidecar"
LABEL org.opencontainers.image.source="https://github.com/eetu/scribe"

# uv for fast resolve + frozen sync.
COPY --from=ghcr.io/astral-sh/uv:latest /uv /uvx /usr/local/bin/

COPY shim/pyproject.toml shim/uv.lock ./
RUN uv sync --frozen --no-dev --no-install-project

COPY shim/src ./src
RUN uv sync --frozen --no-dev

ENV PATH="/app/.venv/bin:$PATH"
ENV SHIM_DATA_DIR=/data
ENV SHIM_HOST=0.0.0.0
ENV SHIM_PORT=3004
ENV SHIM_RELOAD=0

USER 1000

EXPOSE 3004

# Call the entry-point binary baked into the venv directly. `uv run` would
# probe its own cache dir at startup (and fail Permission-denied at /app
# /.cache/uv when running as USER 1000) — we already locked the venv at
# build time so there's nothing left for uv to do at runtime.
CMD ["/app/.venv/bin/shim"]
