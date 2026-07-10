#!/usr/bin/env bash
#
# Reproducible virtme/QEMU runner for the offline Xilinx sysupgrade proof.
#
# Run from Linux/WSL with virtme-ng installed. The guest VM runs the repo-local
# nandsim harness against the signed beta artifacts; success requires both
# target-side sysupgrade scripts to print OFFLINE_NANDSIM_PROOF_OK.

set -euo pipefail

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd)

DEFAULT_S9_PACKAGE="$PROJECT_DIR/output/beta-xil-20260617/DCENTOS_XIL1_S9_beta20260617.tar"
DEFAULT_S19JPRO_PACKAGE="$PROJECT_DIR/output/beta-xil-20260617/DCENTOS_XIL3_S19jPro_beta20260617.tar"
BETA_PUBKEY_HEX=26985575eae77d56c490ceeb9054af012eab5ae59119cd20eaa70dd7e722df83

TARGET=${DCENT_NANDSIM_TARGET:-both}
KERNEL=${DCENT_VIRTME_KERNEL:-}
MEMORY=${DCENT_VIRTME_MEMORY:-2048M}
CPUS=${DCENT_VIRTME_CPUS:-2}
S9_PACKAGE=${DCENT_NANDSIM_S9_PACKAGE:-$DEFAULT_S9_PACKAGE}
S19JPRO_PACKAGE=${DCENT_NANDSIM_S19JPRO_PACKAGE:-$DEFAULT_S19JPRO_PACKAGE}
RELEASE_KEY=${DCENT_RELEASE_PUBKEY_FILE:-}
RELEASE_KEY_HEX=${DCENT_RELEASE_PUBKEY_HEX:-$BETA_PUBKEY_HEX}
TMP_RELEASE_KEY_DIR=
PROBE_ONLY=0
DISABLE_KVM=1
REQUIRE_NANDSIM=${DCENT_REQUIRE_NANDSIM:-1}

usage() {
    cat <<'EOF'
Usage: sysupgrade_offline_virtme_nandsim_runner.sh [options]

Runs the repo-local offline nandsim proof inside a disposable virtme/QEMU VM.

Options:
  --target TARGET          both, am1-s9, am2-s19jpro, or am2-s19jpro-zynq
  --kernel PATH            Linux kernel for virtme-ng (default: /boot/vmlinuz-5.15.0-181-generic, then /boot/vmlinuz)
  --s9-package PATH        Signed S9 artifact
  --s19jpro-package PATH   Signed S19jPro artifact
  --release-key PATH       release_ed25519.pub used by the target-side verifier
                           (default: derive from the signed package and check
                           against DCENT_RELEASE_PUBKEY_HEX)
  --memory SIZE            VM memory passed to vng (default: 2048M)
  --cpus N                 VM CPU count passed to vng (default: 2)
  --probe-only             Only prove the VM has nandsim/UBI/userspace support
  --require-nandsim        Supported workflow flag; guest harnesses fail instead
                           of skip when nandsim/UBI support is unavailable
  --enable-kvm             Do not pass --disable-kvm to vng
  --disable-kvm            Pass --disable-kvm to vng (default; works in WSL)
  -h, --help               Show this help

Environment defaults:
  DCENT_VIRTME_KERNEL
  DCENT_VIRTME_MEMORY
  DCENT_VIRTME_CPUS
  DCENT_NANDSIM_TARGET
  DCENT_NANDSIM_S9_PACKAGE
  DCENT_NANDSIM_S19JPRO_PACKAGE
  DCENT_RELEASE_PUBKEY_FILE
  DCENT_RELEASE_PUBKEY_HEX
EOF
}

die() {
    echo "ERROR: $*" >&2
    exit 1
}

abs_path() {
    case "$1" in
        /*) printf '%s\n' "$1" ;;
        *) printf '%s/%s\n' "$PROJECT_DIR" "$1" ;;
    esac
}

shell_quote() {
    printf "'"
    printf "%s" "$1" | sed "s/'/'\\\\''/g"
    printf "'"
}

default_kernel() {
    local candidate
    for candidate in /boot/vmlinuz-5.15.0-181-generic /boot/vmlinuz; do
        if [ -e "$candidate" ]; then
            printf '%s\n' "$candidate"
            return 0
        fi
    done
    return 1
}

cleanup() {
    if [ -n "$TMP_RELEASE_KEY_DIR" ] && [ -d "$TMP_RELEASE_KEY_DIR" ]; then
        rm -rf "$TMP_RELEASE_KEY_DIR"
    fi
}
trap cleanup EXIT INT TERM

validate_release_key_hex() {
    case "$RELEASE_KEY_HEX" in
        *[!0123456789abcdefABCDEF]*)
            die "DCENT_RELEASE_PUBKEY_HEX contains non-hex characters"
            ;;
    esac
    local len
    len=$(printf '%s' "$RELEASE_KEY_HEX" | wc -c | tr -d ' ')
    [ "$len" = "64" ] || die "DCENT_RELEASE_PUBKEY_HEX must be exactly 64 hex characters"
    RELEASE_KEY_HEX=$(printf '%s' "$RELEASE_KEY_HEX" | tr 'A-F' 'a-f')
}

pubkey_file_hex() {
    openssl pkey -pubin -outform DER -in "$RELEASE_KEY" 2>/dev/null \
        | tail -c 32 \
        | xxd -p -c 64
}

derive_release_key() {
    local source_tar
    local member
    case "$TARGET" in
        am2-s19jpro)
            source_tar=$S19JPRO_PACKAGE
            member=sysupgrade-am2-s19j/release_ed25519.pub
            ;;
        *)
            source_tar=$S9_PACKAGE
            member=sysupgrade-am1-s9/release_ed25519.pub
            ;;
    esac
    [ -f "$source_tar" ] || die "release key derivation package not found: $source_tar; pass --release-key"
    TMP_RELEASE_KEY_DIR=$(mktemp -d "$(dirname "$source_tar")/.dcent-release-pubkey.XXXXXX" 2>/dev/null || mktemp -d)
    RELEASE_KEY="$TMP_RELEASE_KEY_DIR/release_ed25519.pub"
    tar -xOf "$source_tar" "$member" >"$RELEASE_KEY"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --target) TARGET=${2:-}; shift 2 ;;
        --kernel) KERNEL=${2:-}; shift 2 ;;
        --s9-package) S9_PACKAGE=${2:-}; shift 2 ;;
        --s19jpro-package) S19JPRO_PACKAGE=${2:-}; shift 2 ;;
        --release-key) RELEASE_KEY=${2:-}; shift 2 ;;
        --memory) MEMORY=${2:-}; shift 2 ;;
        --cpus) CPUS=${2:-}; shift 2 ;;
        --probe-only) PROBE_ONLY=1; shift ;;
        --require-nandsim) REQUIRE_NANDSIM=1; shift ;;
        --enable-kvm) DISABLE_KVM=0; shift ;;
        --disable-kvm) DISABLE_KVM=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

case "$TARGET" in
    both|am1-s9|am2-s19jpro) ;;
    am2-s19jpro-zynq) TARGET=am2-s19jpro ;;
    *) die "unsupported --target '$TARGET' (expected both, am1-s9, or am2-s19jpro)" ;;
esac

command -v vng >/dev/null 2>&1 || die "virtme-ng vng is required"
command -v sed >/dev/null 2>&1 || die "sed is required"
command -v mktemp >/dev/null 2>&1 || die "mktemp is required"
[ -f "$SCRIPT_DIR/sysupgrade_offline_nandsim_harness.sh" ] || die "missing repo-local nandsim harness"
[ -f "$SCRIPT_DIR/stage1_first_install_offline_nandsim_harness.sh" ] || die "missing repo-local first-install nandsim harness"

if [ -z "$KERNEL" ]; then
    KERNEL=$(default_kernel) || die "no default kernel found; pass --kernel"
fi
[ -f "$KERNEL" ] || die "kernel not found: $KERNEL"

S9_PACKAGE=$(abs_path "$S9_PACKAGE")
S19JPRO_PACKAGE=$(abs_path "$S19JPRO_PACKAGE")

if [ "$PROBE_ONLY" != "1" ]; then
    for cmd in dirname mktemp openssl rm tail tar tr wc xxd; do
        command -v "$cmd" >/dev/null 2>&1 || die "$cmd is required"
    done

    case "$TARGET" in
        both|am1-s9) [ -f "$S9_PACKAGE" ] || die "S9 package not found: $S9_PACKAGE" ;;
    esac
    case "$TARGET" in
        both|am2-s19jpro) [ -f "$S19JPRO_PACKAGE" ] || die "S19jPro package not found: $S19JPRO_PACKAGE" ;;
    esac

    validate_release_key_hex
    if [ -z "$RELEASE_KEY" ]; then
        derive_release_key
    fi
    [ -f "$RELEASE_KEY" ] || die "release key not found: $RELEASE_KEY"
    RELEASE_KEY_ACTUAL_HEX=$(pubkey_file_hex)
    [ "$RELEASE_KEY_ACTUAL_HEX" = "$RELEASE_KEY_HEX" ] || {
        die "release public key mismatch expected=$RELEASE_KEY_HEX actual=$RELEASE_KEY_ACTUAL_HEX"
    }
fi

PROJECT_Q=$(shell_quote "$PROJECT_DIR")
S9_PACKAGE_Q=$(shell_quote "$S9_PACKAGE")
S19JPRO_PACKAGE_Q=$(shell_quote "$S19JPRO_PACKAGE")
RELEASE_KEY_Q=$(shell_quote "$RELEASE_KEY")

VNG_ARGS=(--run "$KERNEL" --memory "$MEMORY" --cpus "$CPUS")
if [ "$DISABLE_KVM" = "1" ]; then
    VNG_ARGS+=(--disable-kvm)
fi
NANDSIM_REQUIRE_ARG=
if [ "$REQUIRE_NANDSIM" = "1" ]; then
    NANDSIM_REQUIRE_ARG=" --require-nandsim"
fi

run_guest_cmd() {
    local label=$1
    local guest_cmd=$2
    local guest_log rc
    echo "OFFLINE_NANDSIM_VIRTME_RUN target=$label kernel=$KERNEL"
    guest_log=$(mktemp "${TMPDIR:-/tmp}/dcent-virtme-${label}.XXXXXX")
    set +e
    vng "${VNG_ARGS[@]}" --exec "$guest_cmd" >"$guest_log" 2>&1
    rc=$?
    set -e
    cat "$guest_log"
    rm -f "$guest_log"
    return "$rc"
}

if [ "$PROBE_ONLY" = "1" ]; then
    GUEST_CMD="set -e; cd $PROJECT_Q; sh scripts/test_s99upgrade_failed_health_no_commit.sh; bash scripts/sysupgrade_offline_nandsim_harness.sh --probe-only$NANDSIM_REQUIRE_ARG; bash scripts/stage1_first_install_offline_nandsim_harness.sh --probe-only$NANDSIM_REQUIRE_ARG"
    run_guest_cmd "$TARGET" "$GUEST_CMD"
else
    GUEST_PREFIX="set -e; cd $PROJECT_Q; export DCENT_SYSUPGRADE_OFFLINE_CONTAINER=1"
    S99_FAILED_HEALTH_CMD="sh scripts/test_s99upgrade_failed_health_no_commit.sh"
    AM1_GUEST_CMD="$GUEST_PREFIX; $S99_FAILED_HEALTH_CMD; bash scripts/sysupgrade_offline_nandsim_harness.sh$NANDSIM_REQUIRE_ARG --target am1-s9 --package $S9_PACKAGE_Q --release-key $RELEASE_KEY_Q --workdir /tmp/dcent-nandsim-proof-am1-s9; bash scripts/sysupgrade_offline_nandsim_harness.sh$NANDSIM_REQUIRE_ARG --current-fw 1 --target am1-s9 --package $S9_PACKAGE_Q --release-key $RELEASE_KEY_Q --workdir /tmp/dcent-nandsim-proof-am1-s9-reverse; bash scripts/stage1_first_install_offline_nandsim_harness.sh$NANDSIM_REQUIRE_ARG --target am1-s9 --package $S9_PACKAGE_Q --workdir /tmp/dcent-first-install-proof-am1-s9"
    AM2_GUEST_CMD="$GUEST_PREFIX; $S99_FAILED_HEALTH_CMD; bash scripts/sysupgrade_offline_nandsim_harness.sh$NANDSIM_REQUIRE_ARG --target am2-s19jpro --package $S19JPRO_PACKAGE_Q --release-key $RELEASE_KEY_Q --workdir /tmp/dcent-nandsim-proof-am2-s19jpro; bash scripts/sysupgrade_offline_nandsim_harness.sh$NANDSIM_REQUIRE_ARG --current-fw 1 --target am2-s19jpro --package $S19JPRO_PACKAGE_Q --release-key $RELEASE_KEY_Q --workdir /tmp/dcent-nandsim-proof-am2-s19jpro-reverse; bash scripts/stage1_first_install_offline_nandsim_harness.sh$NANDSIM_REQUIRE_ARG --target am2-s19jpro --am2-package $S19JPRO_PACKAGE_Q --workdir /tmp/dcent-first-install-proof-am2-s19jpro"
    case "$TARGET" in
        both)
            run_guest_cmd am1-s9 "$AM1_GUEST_CMD"
            run_guest_cmd am2-s19jpro "$AM2_GUEST_CMD"
            ;;
        am1-s9)
            run_guest_cmd am1-s9 "$AM1_GUEST_CMD"
            ;;
        am2-s19jpro)
            run_guest_cmd am2-s19jpro "$AM2_GUEST_CMD"
            ;;
    esac
fi
