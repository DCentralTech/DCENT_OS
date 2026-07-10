#!/bin/bash
# Build dcentrald for Antminer control boards
#
# Usage:
#   ./scripts/build-dcentrald.sh [target]
#
# Targets:
#   zynq      - Antminer S9/S17/S19 Zynq boards (ARMv7-A, Cortex-A9)  [default]
#   amlogic   - Antminer S19XP/S21+ Amlogic A113D boards (AArch64, Cortex-A53)
#   beaglebone - Antminer S19j BeagleBone AM335x boards (ARMv7-A, Cortex-A8)
#   cvitek    - Antminer S21/T21 CVitek CV1835 boards (AArch64, Cortex-A53)
#   native    - Build for the host machine (development/testing)
#
# Requires Docker Desktop running.
# Output: dcentrald/target/<triple>/release/dcentrald
#
# RELEASE BUILDS: export DCENT_MANIFEST_PUBLIC_KEY_HEX (64-hex ed25519
# verifying key) BEFORE running this script AND before
# scripts/build_in_docker.sh. The pin is baked into dcentrald HERE at
# cargo-build time (option_env! in dcentrald-api/src/ota_signature.rs);
# build_in_docker.sh Phase 5 then verifies the staged binary actually embeds
# the same hex and hard-fails otherwise. Exporting the pin only for
# build_in_docker.sh CANNOT retro-pin a binary that was already built here —
# this script fails fast (below) if a release context is indicated without
# the pin, instead of silently producing an unpinned (fail-open) binary.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DCENTRALD_DIR="$(cd "$SCRIPT_DIR/../dcentrald" && pwd)"
TARGET="${1:-zynq}"

# -------- Release manifest-pin fail-fast (BUG-2, 2026-07-09) --------
# Two-step release contract: DCENT_MANIFEST_PUBLIC_KEY_HEX must be exported
# for BOTH build steps (this cross-compile AND the build_in_docker.sh package
# build). If it is exported only for the package build, the binary built here
# bakes an EMPTY pin and build_in_docker.sh fails deep in Phase 5 with
# "the prebuilt dcentrald binary does NOT contain that hex string" — hours
# late. Fail fast HERE, before the long cargo build. Dev/lab builds (no
# release env indicated, or DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1) are unchanged.
_is_truthy() {
    case "${1:-}" in
        1|true|TRUE|yes|YES|y|Y) return 0 ;;
        *) return 1 ;;
    esac
}
RELEASE_CONTEXT=""
if _is_truthy "${DCENT_RELEASE_IMAGE:-0}"; then
    RELEASE_CONTEXT="DCENT_RELEASE_IMAGE=${DCENT_RELEASE_IMAGE}"
elif _is_truthy "${DCENT_REQUIRE_RELEASE_KEY:-0}"; then
    RELEASE_CONTEXT="DCENT_REQUIRE_RELEASE_KEY=${DCENT_REQUIRE_RELEASE_KEY}"
else
    case "${DCENT_PACKAGE_STATUS:-}" in
        release|production|stable)
            RELEASE_CONTEXT="DCENT_PACKAGE_STATUS=${DCENT_PACKAGE_STATUS}" ;;
    esac
fi
# A pin that is exported but EMPTY is release intent with a broken value —
# treat it as a release context rather than silently baking an unpinned
# binary (option_env! would embed nothing and the fallback is fail-open).
if [ -z "$RELEASE_CONTEXT" ] && [ -n "${DCENT_MANIFEST_PUBLIC_KEY_HEX+set}" ] \
    && [ -z "${DCENT_MANIFEST_PUBLIC_KEY_HEX}" ]; then
    RELEASE_CONTEXT="DCENT_MANIFEST_PUBLIC_KEY_HEX exported but empty"
fi
if [ -n "$RELEASE_CONTEXT" ] && [ -z "${DCENT_MANIFEST_PUBLIC_KEY_HEX:-}" ] \
    && ! _is_truthy "${DCENT_ALLOW_UNSIGNED_SYSUPGRADE:-0}"; then
    echo "" >&2
    echo "ERROR: release context indicated ($RELEASE_CONTEXT) but" >&2
    echo "       DCENT_MANIFEST_PUBLIC_KEY_HEX is empty." >&2
    echo "" >&2
    echo "  A release dcentrald bakes the stock-Bitmain manifest pubkey pin at" >&2
    echo "  COMPILE time (option_env!). Building now would produce an UNPINNED" >&2
    echo "  (fail-open) release binary, and scripts/build_in_docker.sh would" >&2
    echo "  reject it deep in Phase 5 after the long Buildroot phase." >&2
    echo "  Export the pin for BOTH build steps, then re-run:" >&2
    echo "      export DCENT_MANIFEST_PUBLIC_KEY_HEX=<hex64>" >&2
    echo "      bash scripts/build-dcentrald.sh $TARGET" >&2
    echo "      bash scripts/build_in_docker.sh --target <board>" >&2
    echo "  Dev/lab escape hatch (never for release packages):" >&2
    echo "      DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1" >&2
    exit 1
fi
if [ -n "${DCENT_MANIFEST_PUBLIC_KEY_HEX:-}" ]; then
    # Same shape gate as build_in_docker.sh: 64 hex chars = raw 32-byte
    # ed25519 verifying key. A typo here would otherwise surface only at the
    # build_in_docker.sh Phase-5 strings check, after the full cargo build.
    if ! printf '%s' "$DCENT_MANIFEST_PUBLIC_KEY_HEX" | grep -qE '^[0-9a-fA-F]{64}$'; then
        echo "ERROR: DCENT_MANIFEST_PUBLIC_KEY_HEX must be exactly 64 hex chars" >&2
        echo "       (raw 32-byte ed25519 verifying key). Got length ${#DCENT_MANIFEST_PUBLIC_KEY_HEX}." >&2
        exit 1
    fi
fi

# Map board name to Rust target triple
case "$TARGET" in
    zynq)
        # Canonical Zynq target per root  is musleabihf (static
        # musl libc — BraiinsOS rootfs is musl, glibc binaries fail
        # "not found" because the dynamic linker /lib/ld-linux-armhf.so.3
        # doesn't exist on bosminer's rootfs). musl-targeted Rust statically
        # links and runs on any Linux ARM kernel.
        TRIPLE="armv7-unknown-linux-musleabihf"
        # Install BOTH:
        #   - gcc-arm-linux-gnueabihf so cc-rs (used by transitive C-dep
        #     crates like ring) finds an ARM C compiler. Object code is
        #     ARM machine code, libc-independent at the C level.
        #   - musl-tools for musl-gcc on the HOST (some crates probe).
        CROSS_PKG="gcc-arm-linux-gnueabihf musl-tools"
        # Linker is rust-lld (bundled with rustup; statically links musl).
        CROSS_LINKER="rust-lld"
        # Cross C compiler/archiver cc-rs uses for C deps (ring/secp256k1).
        CROSS_CC="arm-linux-gnueabihf-gcc"
        CROSS_AR="arm-linux-gnueabihf-ar"
        ARCH_FLAGS="-C target-cpu=cortex-a9 -C target-feature=+vfp3"
        echo "Building for Zynq (S9/S17/S19) — ARMv7-A Cortex-A9 (musl, static)"
        ;;
    amlogic)
        # Amlogic A113D (S19j Pro AML / S19XP / S21 / S21 Pro) = AArch64
        # Cortex-A53, static musl (matches build_in_docker.sh and the
        # br2_external am3-aml post-build.sh target paths). A glibc dynamic
        # binary fails on the musl Buildroot rootfs (no dynamic linker) and
        # is not found by the post-build scripts (they read the musl path).
        TRIPLE="aarch64-unknown-linux-musl"
        # gcc-aarch64-linux-gnu = the C cross-compiler cc-rs needs for the
        # C deps (ring/secp256k1); musl-tools for host musl-gcc probes.
        CROSS_PKG="gcc-aarch64-linux-gnu musl-tools"
        # rust-lld statically links musl; no external musl gcc needed.
        CROSS_LINKER="rust-lld"
        CROSS_CC="aarch64-linux-gnu-gcc"
        CROSS_AR="aarch64-linux-gnu-ar"
        ARCH_FLAGS="-C target-cpu=cortex-a53"
        echo "Building for Amlogic A113D (S19XP/S21+) — AArch64 Cortex-A53 (musl, static)"
        ;;
    beaglebone)
        TRIPLE="armv7-unknown-linux-gnueabihf"
        CROSS_PKG="gcc-arm-linux-gnueabihf"
        CROSS_LINKER="arm-linux-gnueabihf-gcc"
        CROSS_CC="arm-linux-gnueabihf-gcc"
        CROSS_AR="arm-linux-gnueabihf-ar"
        ARCH_FLAGS="-C target-cpu=cortex-a8"
        echo "Building for BeagleBone AM335x (S19j) — ARMv7-A Cortex-A8"
        ;;
    cvitek)
        # CVitek CV1835 (S19j Pro CV1835 variant) = Cortex-A7, i.e. ARMv7
        # 32-bit — NOT AArch64. Matches build_in_docker.sh cv1835-s19jpro
        # and the cv1835 defconfig (armv7-unknown-linux-musleabihf). An
        # aarch64 binary will not execute on the 32-bit Cortex-A7.
        TRIPLE="armv7-unknown-linux-musleabihf"
        CROSS_PKG="gcc-arm-linux-gnueabihf musl-tools"
        CROSS_LINKER="rust-lld"
        CROSS_CC="arm-linux-gnueabihf-gcc"
        CROSS_AR="arm-linux-gnueabihf-ar"
        ARCH_FLAGS="-C target-cpu=cortex-a7"
        echo "Building for CVitek CV1835 (S19j Pro) — ARMv7-A Cortex-A7 (musl, static)"
        ;;
    native)
        echo "Building for host (native)"
        cd "$DCENTRALD_DIR"
        cargo build --release
        echo ""
        echo "Binary: $DCENTRALD_DIR/target/release/dcentrald"
        exit 0
        ;;
    *)
        echo "Unknown target: $TARGET"
        echo "Valid targets: zynq, amlogic, beaglebone, cvitek, native"
        exit 1
        ;;
esac

echo "Target triple: $TRIPLE"
echo ""

# Build Docker image for cross-compilation (cached after first run)
DOCKER_IMAGE="dcentrald-cross-${TARGET}"

echo "Building Docker cross-compilation image: $DOCKER_IMAGE"
docker build -t "$DOCKER_IMAGE" -f - "$DCENTRALD_DIR" <<DOCKERFILE
FROM rust:1.90-bookworm
ENV DEBIAN_FRONTEND=noninteractive
RUN dpkg --add-architecture armhf 2>/dev/null || true && \
    apt-get update && apt-get install -y \
    ${CROSS_PKG} \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add ${TRIPLE}
# Surface rust-lld (bundled with rustup) on PATH so the LINKER env var
# below can resolve it. Needed for musl static targets where we don't
# have a cross-gcc.
RUN find /usr/local/rustup -name 'rust-lld' -type f -exec ln -sf {} /usr/local/bin/rust-lld \\; || true
WORKDIR /src
DOCKERFILE

echo ""
echo "Cross-compiling dcentrald..."

# Run the build inside Docker
# Mount source as /src, set linker and rustflags via environment
# dcentrald-api depends on `dcent-schema` via `path = "../../../dcent-schema"`,
# which from `/src/dcentrald-api/Cargo.toml` resolves to `/dcent-schema`.
# Mount the sibling workspace member at that container-side absolute path.
DCENT_SCHEMA_DIR="$(cd "$DCENTRALD_DIR/../dcent-schema" && pwd)"

# dcentrald-api/src/routes/restore_to_stock.rs uses include_str!/include_bytes!
# with `"../../../../../../"`,
# which from /src/dcentrald-api/src/routes/ resolves to /
# Mount the repo-root knowledge-base/ tree at that container-side absolute path.

# Cargo per-target env var name forms for the selected triple:
#   - upper/underscore for CARGO_TARGET_<TRIPLE>_{LINKER,RUSTFLAGS}
#   - lower/underscore for cc-rs's CC_<triple>/AR_<triple>
TRIPLE_UPPER="$(echo "$TRIPLE" | tr '[:lower:]-' '[:upper:]_')"
TRIPLE_LOWER="$(echo "$TRIPLE" | tr '-' '_')"

# Per-target cross C compiler/archiver for cc-rs (ring/secp256k1 C deps).
DOCKER_ENV_ARGS=(
    -e "CC_${TRIPLE_LOWER}=${CROSS_CC}"
    -e "AR_${TRIPLE_LOWER}=${CROSS_AR}"
)
# For static musl targets we link with rust-lld; mirror that into the
# per-target RUSTFLAGS so the linker override is unambiguous. Skipped for
# gcc-linked targets (e.g. beaglebone) so their CARGO_TARGET_<T>_LINKER wins.
if [ "$CROSS_LINKER" = "rust-lld" ]; then
    DOCKER_ENV_ARGS+=( -e "CARGO_TARGET_${TRIPLE_UPPER}_RUSTFLAGS=-C linker=rust-lld" )
fi

MSYS_NO_PATHCONV=1 docker run --rm \
    -v "$(cd "$DCENTRALD_DIR" && pwd)":/src \
    -v "$DCENT_SCHEMA_DIR":/dcent-schema \
    -e "CARGO_TARGET_${TRIPLE_UPPER}_LINKER=${CROSS_LINKER}" \
    "${DOCKER_ENV_ARGS[@]}" \
    -e "RUSTFLAGS=-C link-arg=-s ${ARCH_FLAGS}" \
    -e "DCENT_MANIFEST_PUBLIC_KEY_HEX=${DCENT_MANIFEST_PUBLIC_KEY_HEX:-}" \
    -e "DCENT_MANIFEST_KEY_ID=${DCENT_MANIFEST_KEY_ID:-}" \
    "$DOCKER_IMAGE" \
    cargo build --release --target "$TRIPLE"

# Check result
BINARY="$DCENTRALD_DIR/target/$TRIPLE/release/dcentrald"
if [ -f "$BINARY" ]; then
    SIZE=$(ls -lh "$BINARY" | awk '{print $5}')
    echo ""
    echo "Build successful!"
    echo "Binary: $BINARY"
    echo "Size: $SIZE"
    echo "Target: $TRIPLE"
    echo ""
    echo "Deploy: scp $BINARY root@<miner-ip>:/usr/bin/dcentrald"
else
    echo "Build failed — binary not found at $BINARY"
    exit 1
fi
