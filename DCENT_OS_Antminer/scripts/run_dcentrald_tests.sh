#!/usr/bin/env bash
#
# run_dcentrald_tests.sh — the workspace test gate that was MISSING.
#
# Wave-A / SB-2 / SB-3 (2026-05-28): DCENT_OS had NO command that compiled or
# ran the dcentrald workspace tests. `make release` → build_in_docker.sh runs
# only `cargo build --release` (skips #[cfg(test)] + integration tests), and
# ci_offline_gates.sh is static-only. As a result SB-3 (a test with a broken
# 4-up include_str! path) shipped uncompiled for weeks and silently broke
# `cargo build --workspace --tests`, which is the proximate reason the SB-1
# fingerprint/guard drift was never caught. This script closes that gap.
#
# By DEFAULT it COMPILE-GATES every workspace test target for the real Zynq
# musl target (cargo test --no-run), which is exactly what would have caught
# SB-3. It runs inside the cached `dcentrald-cross-zynq` Docker image with the
# WHOLE repo mounted, because several integration tests `include_str!` sibling
# files under ../../../scripts and ../../../dcent-schema that only resolve in
# the full-repo layout (a dcentrald-only mount fails them — a real gotcha).
#
# To actually EXECUTE the tests (not just compile), pass --run: that builds +
# runs for the container HOST target (x86_64-linux), where test binaries can
# execute. A future Linux CI runner / pre-push hook should call `--run`.
#
# Usage (from a bash shell with Docker Desktop running):
#   bash DCENT_OS_Antminer/scripts/run_dcentrald_tests.sh            # compile-gate (armv7, --no-run)
#   bash DCENT_OS_Antminer/scripts/run_dcentrald_tests.sh --run      # compile + RUN (host x86_64)
#   bash DCENT_OS_Antminer/scripts/run_dcentrald_tests.sh --package dcentrald
#
set -euo pipefail
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

MODE="compile"        # compile | run
PKG_ARGS="--workspace"
while [ $# -gt 0 ]; do
    case "$1" in
        --run) MODE="run"; shift ;;
        --package) PKG_ARGS="-p $2"; shift 2 ;;
        --package=*) PKG_ARGS="-p ${1#--package=}"; shift ;;
        -h|--help)
            echo "Usage: $(basename "$0") [--run] [--package <name>]"
            echo "  (default) compile-gate all workspace tests for armv7 musl (cargo test --no-run)"
            echo "  --run     compile + execute tests on the container host target (x86_64-linux)"
            exit 0 ;;
        *) echo "ERROR: unknown flag: $1 (try --help)" >&2; exit 1 ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"               # DCENT_OS_Antminer
REPO_ROOT="$(cd "$PROJECT_DIR/../.." && pwd)"        # repo root (full-repo mount needed for include_str!)
IMAGE="dcentrald-cross-zynq"
DOCKER_BIN="docker"
DOCKER_REPO_ROOT="$REPO_ROOT"
DOCKER_BUILD_CONTEXT="$PROJECT_DIR/dcentrald"

if command -v "$DOCKER_BIN" >/dev/null 2>&1 && "$DOCKER_BIN" info >/dev/null 2>&1; then
    :
elif command -v docker.exe >/dev/null 2>&1 && docker.exe info >/dev/null 2>&1; then
    DOCKER_BIN="docker.exe"
    if command -v wslpath >/dev/null 2>&1; then
        DOCKER_REPO_ROOT="$(wslpath -w "$REPO_ROOT")"
        DOCKER_BUILD_CONTEXT="$(wslpath -w "$PROJECT_DIR/dcentrald")"
    fi
else
    echo "ERROR: Docker daemon not responding" >&2
    exit 1
fi

# Build the cross image if missing (mirrors build-dcentrald.sh's Dockerfile).
if ! "$DOCKER_BIN" image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "--- building $IMAGE (one-time) ---"
    "$DOCKER_BIN" build -t "$IMAGE" -f - "$DOCKER_BUILD_CONTEXT" <<'DOCKERFILE'
FROM rust:1.90-bookworm
ENV DEBIAN_FRONTEND=noninteractive
RUN dpkg --add-architecture armhf 2>/dev/null || true && \
    apt-get update && apt-get install -y gcc-arm-linux-gnueabihf musl-tools && rm -rf /var/lib/apt/lists/*
RUN rustup target add armv7-unknown-linux-musleabihf
RUN find /usr/local/rustup -name 'rust-lld' -type f -exec ln -sf {} /usr/local/bin/rust-lld \; || true
WORKDIR /src
DOCKERFILE
fi

if [ "$MODE" = "run" ]; then
    echo "=== run_dcentrald_tests: EXECUTE $PKG_ARGS on host target (x86_64-linux) ==="
    CARGO_CMD="cargo test $PKG_ARGS --no-fail-fast"
else
    echo "=== run_dcentrald_tests: COMPILE-GATE $PKG_ARGS for armv7-musl (--no-run) ==="
    CARGO_CMD="cargo test --target armv7-unknown-linux-musleabihf --no-run $PKG_ARGS"
fi

# Whole repo mounted at /repo so ../../../{scripts,dcent-schema} + knowledge-base
# include_str! paths resolve exactly as in the real source tree.
"$DOCKER_BIN" run --rm \
    -v "$DOCKER_REPO_ROOT":/repo \
    -e CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER=rust-lld \
    -e CC_armv7_unknown_linux_musleabihf=arm-linux-gnueabihf-gcc \
    -e AR_armv7_unknown_linux_musleabihf=arm-linux-gnueabihf-ar \
    -e "CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS=-C linker=rust-lld" \
    -e "RUSTFLAGS=-C target-cpu=cortex-a9 -C target-feature=+vfp3" \
    -w /repo/DCENT_OS_Antminer/dcentrald \
    "$IMAGE" \
    sh -c "$CARGO_CMD"

echo ""
echo "run_dcentrald_tests: PASS ($MODE)"
