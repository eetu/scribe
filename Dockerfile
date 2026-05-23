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
# Stub sources for every workspace member. Press isn't shipped in the
# backend image (it runs native on the mini), but cargo still demands
# every declared member's manifest target exists on disk for workspace
# discovery to succeed.
RUN mkdir -p backend/src press/src shared/src \
    && printf 'fn main() {}\n' > backend/src/main.rs \
    && : > backend/src/lib.rs \
    && printf 'fn main() {}\n' > press/src/main.rs \
    && : > shared/src/lib.rs \
    && xx-cargo build --release --workspace

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
# Press normally runs as a native binary on the Mac mini (built via
# cargo, deployed via launchd by the mini IaC). This image exists as a
# fallback for any future host with enough headroom — e.g. a Pi 5 or
# an x86 box — where launchd isn't an option. Skipped from the default
# CI gate unless the press image flag is flipped on.
FROM workspace-deps AS press-build
ARG TARGETPLATFORM
COPY shared/src ./shared/src
COPY press/src ./press/src
RUN touch shared/src/lib.rs press/src/main.rs \
    && xx-cargo build --release -p scribe-press

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
FROM alpine:3.23.4 AS press-runner
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

# --- Stage 6: shim runtime (Python) ---
#
# Shim is Pi-side too. Keeps mkb79/audible's Python deps isolated from the
# Rust backend so a rotting Audible auth flow swap doesn't force the Rust
# image to rebuild.
FROM python:3.13-slim AS shim-runner
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

CMD ["uv", "run", "shim"]
