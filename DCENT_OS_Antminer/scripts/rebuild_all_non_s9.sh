#!/usr/bin/env bash
#
# rebuild_all_non_s9.sh -- 2 (DCENT_DevOps) orchestrator that rebuilds
# every non-S9 sysupgrade tarball in one pass and pins the resulting SHA-256
# digests to `output/sha256_pins.txt` so downstream installers can detect
# silent overlay/binary drift.
#
# This wrapper is intentionally NOT a build engine. It calls
# `bash scripts/build_in_docker.sh --target <T>` once per target and reuses
# the existing Docker pipeline (toolchain cache, rsync staging, post-image
# board scripts, sysupgrade packaging). All hardware safety, package signing,
# and validator gates inside `build_in_docker.sh` keep firing untouched.
#
# Targets covered (S9 deliberately excluded -- production
# readiness matrix; S9 has its own well-trodden release loop and rebuilding
# it accidentally on a non-S9 sweep is a foot-gun):
#
#   am2-s19jpro  -> output/dcentos-sysupgrade-am2-s19jpro.tar
#   am3-s19kpro  -> output/dcentos-sysupgrade-am3-s19kpro.tar
#   am3-s21      -> output/dcentos-sysupgrade-am3-s21.tar
#   am3-bb       -> output/dcentos-am3-bb-sdcard.tar    (SD-card payload)
#
# Usage:
#
#   bash DCENT_OS_Antminer/scripts/rebuild_all_non_s9.sh
#
# Optional flags are forwarded transparently to `build_in_docker.sh` so the
# operator can bake a release pubkey in or run unsigned lab builds without
# editing this script:
#
#   bash DCENT_OS_Antminer/scripts/rebuild_all_non_s9.sh --lab-unsigned
#   DCENT_RELEASE_SIGNING_KEY=/path/to/key.pem \
#   DCENT_MANIFEST_PUBLIC_KEY_HEX=$(cat ...) \
#       bash DCENT_OS_Antminer/scripts/rebuild_all_non_s9.sh
#
# Exit codes:
#   0  all four targets built and pinned cleanly
#   1  one or more targets failed (script names which) OR the per-build
#      tarball cannot be located/sha256summed afterwards
#
# What gets written:
#   output/sha256_pins.txt   one line per artifact, format documented inline
#                            (see write_pins_header below). Atomically
#                            replaced via tmp+mv so a partial run never
#                            leaves a half-written pin file in place.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
OUTPUT_DIR="$PROJECT_DIR/output"
PINS_FILE="$OUTPUT_DIR/sha256_pins.txt"
PINS_TMP="$OUTPUT_DIR/.sha256_pins.txt.tmp"

mkdir -p "$OUTPUT_DIR"

# Targets in deliberate order: Zynq am2 first (oldest, most-tested), then
# the two am3-aml aarch64 variants that share a post-image script family,
# then the am3-bb SD-card-only payload last. Reordering changes the pins
# file ordering -- ci_offline_gates.sh stale-tarball gate is order-agnostic
# so the order is for human readability only.
TARGETS=(am2-s19jpro am3-s19kpro am3-s21 am3-bb)

declare -A TARBALL_FOR_TARGET=(
    [am2-s19jpro]="dcentos-sysupgrade-am2-s19jpro.tar"
    [am3-s19kpro]="dcentos-sysupgrade-am3-s19kpro.tar"
    [am3-s21]="dcentos-sysupgrade-am3-s21.tar"
    [am3-bb]="dcentos-am3-bb-sdcard.tar"
)

current_commit() {
    # Git is the source of truth for "which dcentos commit produced this
    # tarball". If we're outside a git tree (e.g. unpacked source tarball),
    # degrade to "unknown" rather than silently lying about provenance.
    if git -C "$PROJECT_DIR" rev-parse --short=12 HEAD >/dev/null 2>&1; then
        git -C "$PROJECT_DIR" rev-parse --short=12 HEAD
    else
        printf '%s' "unknown"
    fi
}

write_pins_header() {
    # Document the format inline so future agents can read sha256_pins.txt
    # standalone. Comment lines start with `#`. Each pin line is exactly:
    #
    #     <sha256>  <filename>  <commit>
    #
    # The two-space gap matches GNU coreutils `sha256sum` so a downstream
    # `sha256sum -c` ignoring the third column still verifies the digest.
    local commit
    commit="$(current_commit)"
    {
        printf '# DCENT_OS sysupgrade SHA-256 pins (rebuild_all_non_s9.sh)\n'
        printf '# Generated: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf '# Commit:    %s\n' "$commit"
        printf '# Format:    <sha256>  <filename>  <commit>\n'
        printf '# Verify:    awk "{print \$1, \$2}" sha256_pins.txt | sha256sum -c -\n'
        printf '#\n'
    } > "$PINS_TMP"
}

append_pin() {
    local target="$1"
    local tarball
    tarball="${TARBALL_FOR_TARGET[$target]:-}"
    if [ -z "$tarball" ]; then
        echo "ERROR: no tarball mapping for target $target" >&2
        return 1
    fi
    local artifact="$OUTPUT_DIR/$tarball"
    if [ ! -f "$artifact" ]; then
        echo "ERROR: expected artifact missing after build: $artifact" >&2
        return 1
    fi
    local sha
    sha="$(sha256sum "$artifact" | awk '{print $1}')"
    if [ -z "$sha" ]; then
        echo "ERROR: sha256sum produced empty digest for $artifact" >&2
        return 1
    fi
    printf '%s  %s  %s\n' "$sha" "$tarball" "$(current_commit)" >> "$PINS_TMP"
    printf '  pinned %s -> %s\n' "$tarball" "$sha"
}

run_build() {
    local target="$1"
    shift
    echo ""
    echo "==================================================================="
    echo "rebuild_all_non_s9: target=$target"
    echo "==================================================================="
    if ! bash "$SCRIPT_DIR/build_in_docker.sh" --target "$target" "$@"; then
        echo "ERROR: build_in_docker.sh failed for target=$target" >&2
        return 1
    fi
    return 0
}

main() {
    write_pins_header
    local failed=()
    local target
    for target in "${TARGETS[@]}"; do
        if run_build "$target" "$@"; then
            if ! append_pin "$target"; then
                failed+=("$target (sha256 pin failed)")
            fi
        else
            failed+=("$target")
        fi
    done

    if [ "${#failed[@]}" -gt 0 ]; then
        echo "" >&2
        echo "==================================================================="
        echo "rebuild_all_non_s9 FAILED on:" >&2
        local f
        for f in "${failed[@]}"; do
            printf '  - %s\n' "$f" >&2
        done
        echo "==================================================================="
        # Even on partial failure, persist whatever we did pin so the operator
        # can diff against a previous run instead of losing all evidence.
        mv "$PINS_TMP" "$PINS_FILE.partial"
        echo "Partial pins written to: $PINS_FILE.partial" >&2
        exit 1
    fi

    mv "$PINS_TMP" "$PINS_FILE"
    echo ""
    echo "==================================================================="
    echo "rebuild_all_non_s9 SUCCESS"
    echo "  pins file: $PINS_FILE"
    echo "==================================================================="
    cat "$PINS_FILE"
}

main "$@"
