#!/usr/bin/env bash
#
# Canonical dcentrald workspace test gate.
#
# The default compiles every workspace test target for the production ARMv7
# musl ABI. `--run` executes host-Linux tests. Local use runs in a reproducible
# Docker image; Linux CI passes `--native` and consumes the identical locked
# Cargo argument builder without nesting Docker.
#
# Usage:
#   bash DCENT_OS_Antminer/scripts/run_dcentrald_tests.sh
#   bash DCENT_OS_Antminer/scripts/run_dcentrald_tests.sh --run
#   bash DCENT_OS_Antminer/scripts/run_dcentrald_tests.sh --package dcentrald
#   bash DCENT_OS_Antminer/scripts/run_dcentrald_tests.sh --native
#
set -euo pipefail
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

MODE="compile"        # compile | run
BACKEND="docker"      # docker | native
PKG_ARGS=(--workspace)
while [ $# -gt 0 ]; do
    case "$1" in
        --run)
            MODE="run"
            shift
            ;;
        --native)
            BACKEND="native"
            shift
            ;;
        --package)
            [ $# -ge 2 ] || { echo "ERROR: --package requires a crate name" >&2; exit 1; }
            PKG_ARGS=(-p "$2")
            shift 2
            ;;
        --package=*)
            package="${1#--package=}"
            [ -n "$package" ] || { echo "ERROR: --package requires a crate name" >&2; exit 1; }
            PKG_ARGS=(-p "$package")
            shift
            ;;
        -h|--help)
            echo "Usage: $(basename "$0") [--run] [--native] [--package <name>]"
            echo "  (default) compile-gate workspace tests for armv7 musl"
            echo "  --run     execute tests for the host target"
            echo "  --native  run Cargo directly instead of in Docker"
            exit 0
            ;;
        *)
            echo "ERROR: unknown flag: $1 (try --help)" >&2
            exit 1
            ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
REPO_ROOT="$(cd "$PROJECT_DIR/../.." && pwd)"
TEST_DOCKERFILE="$SCRIPT_DIR/docker/Dockerfile.dcentrald-test"
DCENT_CARGO_BUILD_JOBS="${DCENT_CARGO_BUILD_JOBS:-1}"
DCENT_RUST_TOOLCHAIN="${DCENT_RUST_TOOLCHAIN:-1.90.0}"

CARGO_ARGS=(test --locked --profile ci-test)
if [ "$MODE" = "run" ]; then
    CARGO_ARGS+=(--no-fail-fast)
else
    CARGO_ARGS+=(--target armv7-unknown-linux-musleabihf --no-run)
fi
CARGO_ARGS+=("${PKG_ARGS[@]}")

if [ "$BACKEND" = "native" ]; then
    echo "=== run_dcentrald_tests: $MODE ${PKG_ARGS[*]} (native backend) ==="
    RUSTUP_BIN="$(command -v rustup 2>/dev/null || true)"
    if [ -z "$RUSTUP_BIN" ] && [ -x "$HOME/.cargo/bin/rustup" ]; then
        RUSTUP_BIN="$HOME/.cargo/bin/rustup"
    fi
    if [ -z "$RUSTUP_BIN" ]; then
        echo "ERROR: rustup is required to enforce Rust $DCENT_RUST_TOOLCHAIN in --native mode" >&2
        exit 1
    fi
    if [ "$MODE" = "compile" ]; then
        : "${CC_armv7_unknown_linux_musleabihf:?ERROR: native ARM compile requires a true armv7 musl CC wrapper}"
        : "${AR_armv7_unknown_linux_musleabihf:?ERROR: native ARM compile requires a matching archiver}"
    fi
    NATIVE_CARGO=("$RUSTUP_BIN" run "$DCENT_RUST_TOOLCHAIN" cargo)
    NATIVE_RUSTC=("$RUSTUP_BIN" run "$DCENT_RUST_TOOLCHAIN" rustc)
    : "${CARGO_TARGET_DIR:=${XDG_CACHE_HOME:-$HOME/.cache}/dcentrald-test-target-${MODE}}"
    export CARGO_TARGET_DIR
    mkdir -p "$CARGO_TARGET_DIR"
    "${NATIVE_RUSTC[@]}" --version
    "${NATIVE_CARGO[@]}" --version
    echo "CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
    (
        cd "$PROJECT_DIR/dcentrald"
        CARGO_BUILD_JOBS="$DCENT_CARGO_BUILD_JOBS" "${NATIVE_CARGO[@]}" "${CARGO_ARGS[@]}"
    )
    echo ""
    echo "run_dcentrald_tests: PASS ($MODE, native)"
    exit 0
fi

DOCKER_BIN="docker"
if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    :
elif command -v docker.exe >/dev/null 2>&1 && docker.exe info >/dev/null 2>&1; then
    DOCKER_BIN="docker.exe"
else
    echo "ERROR: Docker daemon not responding" >&2
    exit 1
fi

DOCKER_REPO_ROOT="$REPO_ROOT"
DOCKER_BUILD_CONTEXT="$PROJECT_DIR/dcentrald"
DOCKER_TEST_DOCKERFILE="$TEST_DOCKERFILE"

# Docker Desktop invoked from WSL/MSYS needs host-native paths because path
# conversion is disabled above to protect `/repo` container paths.
if command -v wslpath >/dev/null 2>&1 && grep -qi microsoft /proc/version 2>/dev/null; then
    DOCKER_REPO_ROOT="$(wslpath -w "$REPO_ROOT")"
    DOCKER_BUILD_CONTEXT="$(wslpath -w "$PROJECT_DIR/dcentrald")"
    DOCKER_TEST_DOCKERFILE="$(wslpath -w "$TEST_DOCKERFILE")"
elif command -v cygpath >/dev/null 2>&1; then
    DOCKER_REPO_ROOT="$(cygpath -w "$REPO_ROOT")"
    DOCKER_BUILD_CONTEXT="$(cygpath -w "$PROJECT_DIR/dcentrald")"
    DOCKER_TEST_DOCKERFILE="$(cygpath -w "$TEST_DOCKERFILE")"
fi

if command -v sha256sum >/dev/null 2>&1; then
    IMAGE_HASH="$(sha256sum "$TEST_DOCKERFILE" | awk '{print substr($1, 1, 16)}')"
elif command -v shasum >/dev/null 2>&1; then
    IMAGE_HASH="$(shasum -a 256 "$TEST_DOCKERFILE" | awk '{print substr($1, 1, 16)}')"
else
    echo "ERROR: sha256sum or shasum is required to identify the test image" >&2
    exit 1
fi
IMAGE="dcentrald-test-zynq:$IMAGE_HASH"

# The tag includes the checked-in Dockerfile hash, so toolchain changes cannot
# silently reuse a stale mutable image.
if ! "$DOCKER_BIN" image inspect "$IMAGE" >/dev/null 2>&1; then
    echo "--- building $IMAGE ---"
    "$DOCKER_BIN" build -t "$IMAGE" -f "$DOCKER_TEST_DOCKERFILE" "$DOCKER_BUILD_CONTEXT"
fi

# Retire superseded content-addressed test images. An image still referenced by
# a live container is retained because `docker image rm` will refuse it.
while IFS= read -r candidate; do
    [ -n "$candidate" ] || continue
    [ "$candidate" = "$IMAGE" ] && continue
    "$DOCKER_BIN" image rm "$candidate" >/dev/null 2>&1 || true
done < <("$DOCKER_BIN" images dcentrald-test-zynq --format '{{.Repository}}:{{.Tag}}')

echo "=== run_dcentrald_tests: $MODE ${PKG_ARGS[*]} (Docker backend) ==="

CONTAINER_NAME="dcentrald-test-${MODE}-$$-${RANDOM:-0}"
CARGO_HOME_VOLUME="dcentrald-test-cargo-home-rust-1-90"
TARGET_VOLUME="dcentrald-test-target-${IMAGE_HASH}-${MODE}"
cleanup_container() {
    "$DOCKER_BIN" rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
}
trap cleanup_container EXIT INT TERM HUP

# Remove stopped leftovers from a client crash/power loss before inspecting
# cache ownership. Running containers are never touched.
while IFS= read -r orphan; do
    [ -n "$orphan" ] || continue
    "$DOCKER_BIN" rm "$orphan" >/dev/null
done < <("$DOCKER_BIN" ps -a \
    --filter label=com.dcentral.dcentos.test-gate=true \
    --filter status=exited \
    --format '{{.ID}}')

# Older revisions created the same namespace without labels. Labels cannot be
# added to an existing Docker volume, so retire only exact DCENT test-cache
# names after proving no container consumes them; the volumes are disposable
# Cargo caches and will be recreated below with ownership labels.
while IFS= read -r legacy_volume; do
    [ -n "$legacy_volume" ] || continue
    legacy_label="$($DOCKER_BIN volume inspect \
        --format '{{index .Labels "com.dcentral.dcentos.test-cache"}}' \
        "$legacy_volume" 2>/dev/null || true)"
    [ -z "$legacy_label" ] || continue
    consumers="$($DOCKER_BIN ps -a --filter volume="$legacy_volume" --format '{{.ID}}')"
    if [ -z "$consumers" ]; then
        "$DOCKER_BIN" volume rm "$legacy_volume" >/dev/null
    else
        echo "WARNING: retaining unlabeled legacy cache $legacy_volume; container consumer still exists" >&2
    fi
done < <("$DOCKER_BIN" volume ls --format '{{.Name}}' | \
    awk '/^dcentrald-test-(cargo-home-rust-1-90|target-[0-9a-f]+-(compile|run))$/')

# Keep only the two target caches belonging to the current image definition.
# Volumes are namespace- and label-scoped so unrelated Docker state is never
# considered. The Cargo registry cache is intentionally shared across hashes.
while IFS= read -r volume; do
    [ -n "$volume" ] || continue
    case "$volume" in
        "dcentrald-test-target-${IMAGE_HASH}-compile"|"dcentrald-test-target-${IMAGE_HASH}-run")
            continue
            ;;
    esac
    "$DOCKER_BIN" volume rm "$volume" >/dev/null 2>&1 || true
done < <("$DOCKER_BIN" volume ls \
    --filter label=com.dcentral.dcentos.test-cache=target \
    --format '{{.Name}}')

"$DOCKER_BIN" volume create \
    --label com.dcentral.dcentos.test-cache=cargo \
    "$CARGO_HOME_VOLUME" >/dev/null
"$DOCKER_BIN" volume create \
    --label com.dcentral.dcentos.test-cache=target \
    --label "com.dcentral.dcentos.image-hash=$IMAGE_HASH" \
    --label "com.dcentral.dcentos.test-mode=$MODE" \
    "$TARGET_VOLUME" >/dev/null

# Source is mounted read-only. Cargo registry and build artifacts live in
# named volumes so Linux, ARM, and Windows products never collide in the
# workspace target directory. Creating before attaching gives cleanup a
# stable handle even when the client is interrupted.
"$DOCKER_BIN" create \
    --rm \
    --name "$CONTAINER_NAME" \
    --label com.dcentral.dcentos.test-gate=true \
    -v "$DOCKER_REPO_ROOT":/repo:ro \
    -v "$CARGO_HOME_VOLUME":/cargo \
    -v "$TARGET_VOLUME":/target \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER=rust-lld \
    -e CC_armv7_unknown_linux_musleabihf=/usr/local/bin/zig-cc-armv7-musl \
    -e AR_armv7_unknown_linux_musleabihf=/usr/local/bin/zig-ar \
    -e CARGO_BUILD_JOBS="$DCENT_CARGO_BUILD_JOBS" \
    -w /repo/DCENT_OS_Antminer/dcentrald \
    "$IMAGE" \
    cargo "${CARGO_ARGS[@]}" >/dev/null
"$DOCKER_BIN" start --attach "$CONTAINER_NAME"

echo ""
echo "run_dcentrald_tests: PASS ($MODE, Docker)"
