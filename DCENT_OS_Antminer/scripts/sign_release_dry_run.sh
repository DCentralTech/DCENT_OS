#!/bin/sh
# sign_release_dry_run.sh - Wave A T1 operator rehearsal (2026-05-19)
#
# Purpose: prove the DCENT_OS Ed25519 release-signing pipeline end-to-end on
# the operator's host BEFORE they generate or expose a real signing key.
# Uses a fresh ephemeral keypair (created in a temp dir, deleted on exit) to
# build a tiny synthetic sysupgrade-<target>/ package, sign MANIFEST.json,
# and verify the result with verify_sysupgrade_signature.sh - the same
# verifier dcentrald and `dcent install` rely on.
#
# This script is intentionally:
#   - Dev-only. REFUSES to run if DCENT_RELEASE_SIGNING_KEY is set
#     (the real-key build path is build_in_docker.sh with that env var;
#     mixing them would risk a real key leaking into the rehearsal tmpdir).
#   - Self-contained. Does NOT invoke build_in_docker.sh or Docker; the
#     rehearsal is "does this host's openssl + verify script produce a
#     passing round-trip?", not "does the full Buildroot pipeline run?".
#   - Reproducible. Emits a rehearsal report with ephemeral pubkey SHA,
#     manifest SHA, package SHA, and signature length so the operator can
#     compare against their real-key run.
#
# Sister gates (do NOT skip these in a real release):
#   - scripts/ci_offline_gates.sh pre_flash_package_only_selftest — its
#     embedded ed25519 signed-package round-trip sub-check (the openssl
#     genpkey/sign/verify block; there is no standalone function named
#     signed_package_selftest). CI-bound via dcentos-offline-gates.yml.
#   - DCENT_OS_Antminer/scripts/verify_sysupgrade_signature.sh (runtime).
#   - dcentrald-api/src/ota_signature.rs::verify_sysupgrade_bundle()
#     (in-daemon at-rest check via DCENT_MANIFEST_PUBLIC_KEY_HEX pin).
#
# Usage:
#   ./DCENT_OS_Antminer/scripts/sign_release_dry_run.sh \
#     --target <board> [--out <dir>] [--keep-tmpdir]
#
# Examples:
#   ./DCENT_OS_Antminer/scripts/sign_release_dry_run.sh --target am2-s19jpro
#   ./DCENT_OS_Antminer/scripts/sign_release_dry_run.sh --target s9 --out target/audit
#
# Exit codes:
#   0   - rehearsal PASS (sign + verify round-trip + signature length checks ok)
#   2   - usage / argument error
#   3   - DCENT_RELEASE_SIGNING_KEY is set (refuse - real key path)
#   4   - required command missing (openssl / sha256sum / tar)
#   5   - sign or verify step failed
#   6   - verify_sysupgrade_signature.sh not found at the expected path

set -eu

# Resolve script + repo paths without relying on readlink -f (BSD/Mac/BusyBox).
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
DCENTOS_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
REPO_ROOT=$(CDPATH= cd -- "$DCENTOS_DIR/../.." && pwd)
VERIFY_SCRIPT="$SCRIPT_DIR/verify_sysupgrade_signature.sh"

# --- arg parse ---
TARGET=""
OUT_DIR="$REPO_ROOT/target/audit"
KEEP_TMPDIR=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --target)  shift; TARGET=${1:-}; shift || true ;;
        --out)     shift; OUT_DIR=${1:-}; shift || true ;;
        --keep-tmpdir) KEEP_TMPDIR=1; shift ;;
        -h|--help)
            grep -E '^# ' -- "$0" | sed 's/^# \{0,1\}//' | head -40
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            echo "Usage: $0 --target <board> [--out <dir>] [--keep-tmpdir]" >&2
            exit 2
            ;;
    esac
done

if [ -z "$TARGET" ]; then
    echo "ERROR: --target <board> is required (e.g. am2-s19jpro, s9, am3-s19k)" >&2
    exit 2
fi

# Sanity: target must look like a board name (alnum + hyphen). Mirrors the
# verify_sysupgrade_signature.sh regex in spirit; refuse pathy strings.
case "$TARGET" in
    /*|*/*|*..*|"") echo "ERROR: --target must be a board name, not a path: $TARGET" >&2; exit 2 ;;
esac
case "$TARGET" in
    *[!a-zA-Z0-9_-]*) echo "ERROR: --target contains invalid characters: $TARGET" >&2; exit 2 ;;
esac

# --- safety rails ---
# Refuse if the real-key env var is set. The dry-run must NEVER risk a
# real key being mistaken for the ephemeral one (e.g. by a sourced .env or
# direnv autoload). The real-key path is build_in_docker.sh, not this.
if [ -n "${DCENT_RELEASE_SIGNING_KEY:-}" ]; then
    echo "ERROR: DCENT_RELEASE_SIGNING_KEY is set." >&2
    echo "" >&2
    echo "This script is a DRY-RUN that uses an ephemeral keypair. It must not" >&2
    echo "run when a real signing key is configured, to prevent any chance of a" >&2
    echo "real key landing in the rehearsal tmpdir or being exposed to logs." >&2
    echo "" >&2
    echo "To sign with the real key, use:" >&2
    echo "  bash DCENT_OS_Antminer/scripts/build_in_docker.sh --target $TARGET" >&2
    echo "(with DCENT_RELEASE_SIGNING_KEY pointing at your real key)." >&2
    echo "" >&2
    echo "To run this rehearsal anyway, unset DCENT_RELEASE_SIGNING_KEY first:" >&2
    echo "  unset DCENT_RELEASE_SIGNING_KEY" >&2
    exit 3
fi

[ -f "$VERIFY_SCRIPT" ] || {
    echo "ERROR: verify script not found: $VERIFY_SCRIPT" >&2
    exit 6
}

# --- prerequisites ---
for cmd in openssl sha256sum tar awk sed mktemp; do
    command -v "$cmd" >/dev/null 2>&1 || {
        echo "ERROR: required command not found: $cmd" >&2
        exit 4
    }
done

# --- temp dir + trap cleanup ---
TMPDIR_REHEARSAL=$(mktemp -d 2>/dev/null || mktemp -d -t dcent-dryrun) || {
    echo "ERROR: mktemp -d failed" >&2; exit 4
}
cleanup() {
    if [ "$KEEP_TMPDIR" = "1" ]; then
        echo "[note] --keep-tmpdir set; leaving $TMPDIR_REHEARSAL in place"
    else
        rm -rf -- "$TMPDIR_REHEARSAL"
    fi
}
trap cleanup EXIT INT TERM HUP

mkdir -p -- "$OUT_DIR" || { echo "ERROR: cannot create $OUT_DIR" >&2; exit 5; }
REPORT="$OUT_DIR/sign-release-dry-run-$TARGET.txt"
: > "$REPORT" || { echo "ERROR: cannot write $REPORT" >&2; exit 5; }

START_TS=$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)

log() { printf '%s\n' "$*" | tee -a -- "$REPORT" >/dev/null; }
banner() { log "================================================================"; log "$*"; log "================================================================"; }

banner "DCENT_OS release-signing DRY-RUN rehearsal"
log "started:    $START_TS"
log "target:     $TARGET"
log "repo:       $REPO_ROOT"
log "tmpdir:     $TMPDIR_REHEARSAL"
log "verify:     $VERIFY_SCRIPT"
log "report:     $REPORT"
log ""

# --- 1. ephemeral keypair ---
banner "Step 1 - generate ephemeral Ed25519 keypair"
KEY="$TMPDIR_REHEARSAL/ephemeral.key"
PUB_PEM="$TMPDIR_REHEARSAL/ephemeral.pub"
PUB_HEX_FILE="$TMPDIR_REHEARSAL/ephemeral.pub.hex"

openssl genpkey -algorithm Ed25519 -out "$KEY" >/dev/null 2>&1 || { echo "ERROR: openssl genpkey failed" >&2; exit 5; }
openssl pkey -in "$KEY" -pubout -out "$PUB_PEM" >/dev/null 2>&1 || { echo "ERROR: openssl pkey -pubout failed" >&2; exit 5; }

# Raw 32-byte hex (the at-rest pin format consumed by DCENT_MANIFEST_PUBLIC_KEY_HEX).
# Mirrors the formula in DCENT_OS_Antminer/scripts/sign_stock_manifest.sh:24-25.
openssl pkey -in "$KEY" -pubout -outform DER 2>/dev/null \
    | xxd -p -c 64 \
    | tail -c 65 \
    | head -c 64 > "$PUB_HEX_FILE" \
    || { echo "ERROR: failed to derive raw 32-byte pubkey hex" >&2; exit 5; }
PUB_HEX=$(cat -- "$PUB_HEX_FILE")
PUB_HEX_LEN=$(printf '%s' "$PUB_HEX" | wc -c | tr -d ' ')
[ "$PUB_HEX_LEN" = "64" ] || { echo "ERROR: raw pubkey hex is $PUB_HEX_LEN chars (expected 64)" >&2; exit 5; }

PUB_PEM_SHA=$(sha256sum -- "$PUB_PEM" | awk '{print $1}')
log "ephemeral private key:      $KEY  (dev-only, deleted on exit unless --keep-tmpdir)"
log "ephemeral public key (PEM): $PUB_PEM"
log "ephemeral public key SHA256: $PUB_PEM_SHA"
log "ephemeral public key (raw 32B hex): $PUB_HEX"
log ""

# --- 2. synthetic sysupgrade package ---
banner "Step 2 - build synthetic sysupgrade-$TARGET/ package"
PKG_DIR="$TMPDIR_REHEARSAL/sysupgrade-$TARGET"
mkdir -p -- "$PKG_DIR"

# Realistic-shape fixtures. Sizes are small (kilobyte range) so the rehearsal
# is fast; the round-trip exercises the same code paths as real multi-MB
# payloads because the verifier reads sha256 + size from MANIFEST.json.
printf 'DCENT_OS sysupgrade dry-run fixture: kernel for %s\n' "$TARGET" \
    | dd if=/dev/zero of="$PKG_DIR/kernel" bs=1024 count=4 conv=notrunc 2>/dev/null || true
# Re-seed kernel with a deterministic-but-non-trivial pattern.
{ printf 'DCENT_OS-rehearsal-kernel %s\n' "$TARGET"; head -c 4096 /dev/urandom 2>/dev/null || printf 'X%.0s' $(seq 1 4096); } > "$PKG_DIR/kernel"
{ printf 'DCENT_OS-rehearsal-root %s\n' "$TARGET";   head -c 8192 /dev/urandom 2>/dev/null || printf 'Y%.0s' $(seq 1 8192); } > "$PKG_DIR/root"
cat > "$PKG_DIR/METADATA" <<EOF_META
product=DCENT_OS
package_type=sysupgrade
board=$TARGET
board_target=$TARGET
version=dry-run-rehearsal
rehearsal=true
rehearsal_started=$START_TS
EOF_META
cp -- "$PUB_PEM" "$PKG_DIR/release_ed25519.pub"

KERNEL_SIZE=$(wc -c < "$PKG_DIR/kernel" | tr -d ' ')
ROOT_SIZE=$(wc -c < "$PKG_DIR/root" | tr -d ' ')
META_SIZE=$(wc -c < "$PKG_DIR/METADATA" | tr -d ' ')
PUB_SIZE=$(wc -c < "$PKG_DIR/release_ed25519.pub" | tr -d ' ')
KERNEL_SHA=$(sha256sum -- "$PKG_DIR/kernel" | awk '{print $1}')
ROOT_SHA=$(sha256sum -- "$PKG_DIR/root" | awk '{print $1}')
META_SHA=$(sha256sum -- "$PKG_DIR/METADATA" | awk '{print $1}')
PUB_SHA=$(sha256sum -- "$PKG_DIR/release_ed25519.pub" | awk '{print $1}')

log "kernel:               size=$KERNEL_SIZE sha256=$KERNEL_SHA"
log "root:                 size=$ROOT_SIZE sha256=$ROOT_SHA"
log "METADATA:             size=$META_SIZE sha256=$META_SHA"
log "release_ed25519.pub:  size=$PUB_SIZE sha256=$PUB_SHA"
log ""

# --- 3. MANIFEST.json ---
banner "Step 3 - emit MANIFEST.json (schema mirrors ci_offline_gates.sh:250-264)"
MANIFEST="$PKG_DIR/MANIFEST.json"
cat > "$MANIFEST" <<EOF_MANIFEST
{
  "product": "DCENT_OS",
  "package_type": "sysupgrade",
  "board": "$TARGET",
  "board_target": "$TARGET",
  "version": "dry-run-rehearsal",
  "payloads": [
    { "path": "sysupgrade-$TARGET/kernel", "size": $KERNEL_SIZE, "sha256": "$KERNEL_SHA" },
    { "path": "sysupgrade-$TARGET/root", "size": $ROOT_SIZE, "sha256": "$ROOT_SHA" },
    { "path": "sysupgrade-$TARGET/METADATA", "size": $META_SIZE, "sha256": "$META_SHA" },
    { "path": "sysupgrade-$TARGET/release_ed25519.pub", "size": $PUB_SIZE, "sha256": "$PUB_SHA" }
  ]
}
EOF_MANIFEST
MANIFEST_SHA=$(sha256sum -- "$MANIFEST" | awk '{print $1}')
MANIFEST_SIZE=$(wc -c < "$MANIFEST" | tr -d ' ')
log "MANIFEST.json: size=$MANIFEST_SIZE sha256=$MANIFEST_SHA"
log ""

# Honest companion alongside MANIFEST.json (mirrors the SHA256SUMS step inside
# ci_offline_gates.sh pre_flash_package_only_selftest's signed round-trip).
(cd "$PKG_DIR" && sha256sum kernel root METADATA release_ed25519.pub > SHA256SUMS) \
    || { echo "ERROR: sha256sum SHA256SUMS failed" >&2; exit 5; }

# --- 4. sign ---
banner "Step 4 - sign MANIFEST.json with ephemeral key (raw ed25519)"
SIG="$PKG_DIR/MANIFEST.sig"
openssl pkeyutl -sign -rawin -inkey "$KEY" -in "$MANIFEST" -out "$SIG" >/dev/null 2>&1 \
    || { echo "ERROR: openssl pkeyutl -sign failed" >&2; exit 5; }
SIG_SIZE=$(wc -c < "$SIG" | tr -d ' ')
SIG_SHA=$(sha256sum -- "$SIG" | awk '{print $1}')
if [ "$SIG_SIZE" != "64" ]; then
    log "ERROR: MANIFEST.sig is $SIG_SIZE bytes (expected 64 for raw Ed25519)"
    exit 5
fi
log "MANIFEST.sig: size=$SIG_SIZE sha256=$SIG_SHA"
log ""

# --- 5. package ---
banner "Step 5 - tar the package (verify_sysupgrade_signature.sh expects exactly one sysupgrade-* dir)"
PKG="$TMPDIR_REHEARSAL/dcentos-sysupgrade-$TARGET-dryrun.tar"
(cd "$TMPDIR_REHEARSAL" && tar cf "$(basename -- "$PKG")" "sysupgrade-$TARGET") \
    || { echo "ERROR: tar packaging failed" >&2; exit 5; }
PKG_SIZE=$(wc -c < "$PKG" | tr -d ' ')
PKG_SHA=$(sha256sum -- "$PKG" | awk '{print $1}')
log "package: $PKG"
log "package: size=$PKG_SIZE sha256=$PKG_SHA"
log ""

# --- 6. verify ---
banner "Step 6 - verify via verify_sysupgrade_signature.sh (same code path as dcentrald)"
VERIFY_LOG="$TMPDIR_REHEARSAL/verify.log"
if ! sh -- "$VERIFY_SCRIPT" "$PKG" "$PUB_PEM" "$TARGET" > "$VERIFY_LOG" 2>&1; then
    log "FAIL: verify_sysupgrade_signature.sh rejected the rehearsal package"
    log "--- verify output ---"
    while IFS= read -r line; do log "  $line"; done < "$VERIFY_LOG"
    exit 5
fi
log "verify_sysupgrade_signature.sh: PASS"
log "verify output:"
while IFS= read -r line; do log "  $line"; done < "$VERIFY_LOG"
log ""

# --- 7. summary ---
banner "PASS - dry-run rehearsal complete"
END_TS=$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)
log "finished:   $END_TS"
log ""
log "Rehearsal artifacts (deleted on exit unless --keep-tmpdir):"
log "  ephemeral key:  $KEY"
log "  package:        $PKG"
log "  verify log:     $VERIFY_LOG"
log ""
log "What this proves:"
log "  - openssl + verify_sysupgrade_signature.sh round-trip works on this host."
log "  - MANIFEST.json schema (product, board, payloads sha256+size) is accepted."
log "  - Raw Ed25519 signature is exactly 64 bytes."
log "  - Tar layout (single sysupgrade-<board>/ dir) is correct."
log ""
log "What this does NOT prove:"
log "  - The full Buildroot pipeline (build_in_docker.sh) produces a package."
log "  - Your real Ed25519 key custody (HSM / Vault) is correct."
log "  - dcentrald's at-rest DCENT_MANIFEST_PUBLIC_KEY_HEX pin matches your key."
log "  - The signed package boots a real miner. That gate is Wave C (live HW)."
log ""
log "Operator next steps (T1 real-key run, from TODO-NEXT-WAVES.md T1 lines 35-51):"
log "  1. Generate your real keypair in HSM/Vault:"
log "       openssl genpkey -algorithm Ed25519 -out release_ed25519.key"
log "       openssl pkey -in release_ed25519.key -pubout -outform DER \\"
log "         | xxd -p -c 64 | tail -c 65 | head -c 64 > release_ed25519.pub.hex"
log "  2. Build the signed package:"
log "       DCENT_RELEASE_SIGNING_KEY=\$PWD/release_ed25519.key \\"
log "         DCENT_MANIFEST_PUBLIC_KEY_HEX=\$(cat release_ed25519.pub.hex) \\"
log "         bash DCENT_OS_Antminer/scripts/build_in_docker.sh --target $TARGET"
log "  3. Verify independently:"
log "       bash DCENT_OS_Antminer/scripts/verify_sysupgrade_signature.sh \\"
log "         <pkg.tar> release_ed25519.pub.pem $TARGET"
log "  4. Pin manifest SHA + package SHA into the operator inventory doc."
log "  5. \`dcent install <ip> -f <pkg.tar>\` proceeds without"
log "     \`--accept-unsigned-package-lab-only\` once both signed and"
log "     DCENT_MANIFEST_PUBLIC_KEY_HEX-pinned binaries are deployed."
log ""
log "Rehearsal report saved at: $REPORT"

exit 0
