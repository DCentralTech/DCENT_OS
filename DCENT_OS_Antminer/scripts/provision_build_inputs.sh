#!/bin/sh
# =============================================================================
# provision_build_inputs.sh — populate a clean DCENT_OS checkout with the
# gitignored out-of-band build inputs listed in build_inputs.manifest.
# =============================================================================
#
# WHY: full firmware packaging depends on target-selected binary inputs that
# are NOT in git (extracted Bitmain/BraiinsOS boot components + the Linaro
# toolchain — not redistributable). The Rust workspace builds without them;
# release-image packaging fails closed until the required target inputs are
# provisioned. See DCENT_OS_Antminer/docs/RC_BUILD_RUNBOOK.md.
#
# USAGE:
#   sh DCENT_OS_Antminer/scripts/provision_build_inputs.sh [options]
#
# OPTIONS:
#   --source DIR   Out-of-band source directory (overrides
#                  $DCENT_BUILD_INPUTS_DIR and auto-detection).
#   --check        Verify-only mode: hash-check the inputs already present
#                  in THIS checkout against the manifest. No source dir
#                  needed, nothing is copied or modified. Exit 0 = all
#                  present + verified. (Use in CI as a preflight gate.)
#   --force        Overwrite a destination file whose hash MISMATCHES the
#                  manifest (default: fail closed and leave it untouched —
#                  a mismatched existing file may be a newer operator
#                  artifact and silently clobbering it is worse than
#                  stopping).
#   -h | --help    This help.
#
# SOURCE DIRECTORY RESOLUTION (first match wins):
#   1. --source DIR
#   2. $DCENT_BUILD_INPUTS_DIR
#   3. Auto-detect: sibling checkouts of this repo's parent directory that
#      contain the first manifest entry (e.g. the developer's main checkout
#      "DCENT Projects" next to an RC worktree).
#
# SOURCE LAYOUTS (both supported, probed in this order per entry):
#   a) MIRROR TREE — same relative paths as the repo
#      ($SRC/ ...). A full sibling
#      checkout is automatically a valid mirror-tree source.
#   b) FLAT BUNDLE — all files at the top level of $SRC by basename
#      ($SRC/kernel.bin would be AMBIGUOUS for the two kernel.bin entries,
#      so flat bundles must use the DISAMBIGUATED name
#      "<sha256-first-12-chars>_<basename>", e.g.
#      $SRC/e3d6cb698901_kernel.bin). Plain basename is accepted only when
#      it is unambiguous across the manifest (all non-kernel.bin entries).
#
# INTEGRITY (fail-closed supply-chain gate — DO NOT weaken):
#   Every copied file is SHA256-verified against build_inputs.manifest
#   AFTER the copy (copy to a temp name, hash, then atomic mv into place).
#   A source file whose hash does not match the manifest is REJECTED even
#   if it has the right name. Missing sources are reported and the script
#   exits non-zero. There is deliberately NO --skip-verify flag.
#
# IDEMPOTENT: a destination already present with the right hash is skipped.
# Re-running after a partial provision completes only what is missing.
#
# EXIT CODES: 0 = all manifest entries present + hash-verified.
#             1 = one or more entries missing/mismatched (details printed).
#             2 = usage / environment error.
#
# NOTE (toolchain): the Linaro tarball entry provisions to
# DCENT_OS_Antminer/buildroot/dl/toolchain-external-custom/ — the local cache
# location build_in_docker.sh stages from (Phase 5b re-verifies it against
# the same cee0087b... pin). DCENT_TOOLCHAIN_LOCAL_DIR support in
# build_in_docker.sh (being added separately) is the alternative way to
# point the build at an out-of-tree toolchain copy without provisioning it
# into the checkout.
#
# POSIX sh (dash/BusyBox-ash safe): no arrays, no [[ ]], no local-keyword
# reliance beyond function scope conventions.
# =============================================================================

set -u

# ---------------------------------------------------------------------------
# Locate ourselves + repo root (scripts/ is DCENT_OS_Antminer/scripts).
# ---------------------------------------------------------------------------
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)
MANIFEST="$SCRIPT_DIR/build_inputs.manifest"

if [ ! -f "$MANIFEST" ]; then
    echo "ERROR: manifest not found: $MANIFEST" >&2
    exit 2
fi
if [ ! -d "$REPO_ROOT/knowledge-base" ] || [ ! -d "$REPO_ROOT/DCENT_OS_Antminer" ]; then
    echo "ERROR: computed repo root does not look like the DCENT monorepo: $REPO_ROOT" >&2
    exit 2
fi

# ---------------------------------------------------------------------------
# Args
# ---------------------------------------------------------------------------
SRC_DIR="${DCENT_BUILD_INPUTS_DIR:-}"
CHECK_ONLY=0
FORCE=0

usage() {
    sed -n '2,64p' "$0" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        --source)
            [ $# -ge 2 ] || { echo "ERROR: --source needs a directory argument" >&2; exit 2; }
            SRC_DIR="$2"; shift 2 ;;
        --source=*)
            SRC_DIR="${1#--source=}"; shift ;;
        --check)
            CHECK_ONLY=1; shift ;;
        --force)
            FORCE=1; shift ;;
        -h|--help)
            usage; exit 0 ;;
        *)
            echo "ERROR: unknown argument: $1 (see --help)" >&2; exit 2 ;;
    esac
done

# ---------------------------------------------------------------------------
# SHA256 tool detection (GNU coreutils / BusyBox / macOS / OpenSSL fallback)
# ---------------------------------------------------------------------------
if command -v sha256sum >/dev/null 2>&1; then
    hash_file() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null 2>&1; then
    hash_file() { shasum -a 256 "$1" | awk '{print $1}'; }
elif command -v openssl >/dev/null 2>&1; then
    hash_file() { openssl dgst -sha256 -r "$1" | awk '{print $1}'; }
else
    echo "ERROR: no sha256sum/shasum/openssl available — cannot verify integrity." >&2
    echo "       This gate is mandatory (fail-closed); install coreutils." >&2
    exit 2
fi

# ---------------------------------------------------------------------------
# Source-dir auto-detection (skipped in --check mode; resolved lazily so
# --check works with no source present at all).
# ---------------------------------------------------------------------------
autodetect_source() {
    # Probe: a sibling directory of this checkout that contains the first
    # manifest entry at its mirror-tree path.
    _probe_rel=$(grep -v '^[[:space:]]*#' "$MANIFEST" | grep -v '^[[:space:]]*$' \
        | head -n 1 | sed 's/^[0-9a-fA-F]\{64\}[[:space:]]\{1,\}//')
    _parent=$(CDPATH= cd -- "$REPO_ROOT/.." && pwd)
    for _cand in "$_parent"/*/; do
        _cand=${_cand%/}
        [ "$_cand" = "$REPO_ROOT" ] && continue
        if [ -f "$_cand/$_probe_rel" ]; then
            printf '%s\n' "$_cand"
            return 0
        fi
    done
    return 1
}

if [ "$CHECK_ONLY" -eq 0 ] && [ -z "$SRC_DIR" ]; then
    if SRC_DIR=$(autodetect_source); then
        echo "Auto-detected build-inputs source: $SRC_DIR"
    else
        echo "ERROR: no build-inputs source directory." >&2
        echo "" >&2
        echo "  The out-of-band inputs in $MANIFEST" >&2
        echo "  are gitignored (non-redistributable) and must be supplied locally." >&2
        echo "  Provide them one of these ways:" >&2
        echo "    1. export DCENT_BUILD_INPUTS_DIR=/path/to/source   (mirror tree or flat bundle)" >&2
        echo "    2. $(basename "$0") --source /path/to/source" >&2
        echo "    3. place a full sibling checkout (e.g. the main 'DCENT Projects' tree)" >&2
        echo "       next to this checkout — it is auto-detected as a mirror-tree source." >&2
        echo "  Layouts: mirror tree (same relative paths) or flat bundle" >&2
        echo "  (files named <sha256-first12>_<basename> at the top level)." >&2
        exit 2
    fi
fi

if [ "$CHECK_ONLY" -eq 0 ] && [ ! -d "$SRC_DIR" ]; then
    echo "ERROR: source directory does not exist: $SRC_DIR" >&2
    exit 2
fi

# ---------------------------------------------------------------------------
# Locate one entry in the source dir. Prints the source path or nothing.
#   $1 = relative path, $2 = expected sha256
# ---------------------------------------------------------------------------
find_source_file() {
    _rel=$1
    _want=$2
    _base=$(basename "$_rel")
    _short=$(printf '%s' "$_want" | cut -c1-12)
    # a) mirror tree
    if [ -f "$SRC_DIR/$_rel" ]; then
        printf '%s\n' "$SRC_DIR/$_rel"; return 0
    fi
    # b) flat bundle, disambiguated name
    if [ -f "$SRC_DIR/${_short}_${_base}" ]; then
        printf '%s\n' "$SRC_DIR/${_short}_${_base}"; return 0
    fi
    # c) flat bundle, plain basename — only if unambiguous across the manifest
    _dups=$(grep -v '^[[:space:]]*#' "$MANIFEST" | grep -v '^[[:space:]]*$' \
        | awk '{ $1=""; sub(/^ +/,""); print }' | while IFS= read -r _p; do basename "$_p"; done \
        | grep -cx -- "$_base")
    if [ "$_dups" -eq 1 ] && [ -f "$SRC_DIR/$_base" ]; then
        printf '%s\n' "$SRC_DIR/$_base"; return 0
    fi
    return 1
}

# ---------------------------------------------------------------------------
# Main loop over manifest entries
# ---------------------------------------------------------------------------
total=0
ok_present=0
provisioned=0
failed=0
FAIL_LIST=""

# Read the manifest without a pipeline so the counters survive (no subshell).
MANIFEST_BODY=$(grep -v '^[[:space:]]*#' "$MANIFEST" | grep -v '^[[:space:]]*$')

while IFS= read -r line; do
    want=$(printf '%s' "$line" | awk '{print $1}')
    rel=$(printf '%s' "$line" | sed 's/^[0-9a-fA-F]\{64\}[[:space:]]\{1,\}//')

    # Manifest-line sanity: 64-hex hash + non-empty path.
    case "$want" in
        *[!0-9a-fA-F]*|'')
            echo "ERROR: malformed manifest line (bad hash field): $line" >&2
            failed=$((failed + 1)); FAIL_LIST="$FAIL_LIST
  MALFORMED: $line"
            continue ;;
    esac
    if [ ${#want} -ne 64 ] || [ -z "$rel" ] || [ "$rel" = "$line" ]; then
        echo "ERROR: malformed manifest line: $line" >&2
        failed=$((failed + 1)); FAIL_LIST="$FAIL_LIST
  MALFORMED: $line"
        continue
    fi

    total=$((total + 1))
    dest="$REPO_ROOT/$rel"

    # 1) Already present + verified? (idempotence / --check)
    if [ -f "$dest" ]; then
        got=$(hash_file "$dest")
        if [ "$got" = "$want" ]; then
            echo "OK        $rel"
            ok_present=$((ok_present + 1))
            continue
        fi
        if [ "$CHECK_ONLY" -eq 1 ]; then
            echo "MISMATCH  $rel"
            echo "          expected $want"
            echo "          actual   $got"
            failed=$((failed + 1)); FAIL_LIST="$FAIL_LIST
  MISMATCH:  $rel"
            continue
        fi
        if [ "$FORCE" -ne 1 ]; then
            echo "MISMATCH  $rel (present but hash differs from manifest)"
            echo "          expected $want"
            echo "          actual   $got"
            echo "          Refusing to overwrite without --force (fail-closed)."
            failed=$((failed + 1)); FAIL_LIST="$FAIL_LIST
  MISMATCH:  $rel (rerun with --force to overwrite)"
            continue
        fi
        echo "REPLACE   $rel (--force: existing file hash differs; re-provisioning)"
        # fall through to provisioning below
    elif [ "$CHECK_ONLY" -eq 1 ]; then
        echo "MISSING   $rel"
        failed=$((failed + 1)); FAIL_LIST="$FAIL_LIST
  MISSING:   $rel"
        continue
    fi

    # 2) Locate in source
    if ! src=$(find_source_file "$rel" "$want"); then
        short12=$(printf '%s' "$want" | cut -c1-12)
        echo "NO-SOURCE $rel"
        echo "          not found in $SRC_DIR (tried mirror path, ${short12}_$(basename "$rel"), plain basename)"
        failed=$((failed + 1)); FAIL_LIST="$FAIL_LIST
  NO-SOURCE: $rel"
        continue
    fi

    # 3) Copy via temp file, verify, atomic move (never leave a half-copied
    #    or unverified file at the destination path).
    mkdir -p "$(dirname "$dest")" || {
        echo "ERROR: cannot create directory for $rel" >&2
        failed=$((failed + 1)); FAIL_LIST="$FAIL_LIST
  COPY-FAIL: $rel"
        continue
    }
    tmp="$dest.provision.$$"
    if ! cp "$src" "$tmp"; then
        rm -f "$tmp"
        echo "ERROR: copy failed: $src -> $tmp" >&2
        failed=$((failed + 1)); FAIL_LIST="$FAIL_LIST
  COPY-FAIL: $rel"
        continue
    fi
    got=$(hash_file "$tmp")
    if [ "$got" != "$want" ]; then
        rm -f "$tmp"
        echo "BAD-SRC   $rel"
        echo "          source file $src FAILED SHA256 verification:"
        echo "          expected $want"
        echo "          actual   $got"
        echo "          REJECTED (supply-chain gate — source is corrupt or tampered)."
        failed=$((failed + 1)); FAIL_LIST="$FAIL_LIST
  BAD-SRC:   $rel (source hash mismatch — investigate before retrying)"
        continue
    fi
    if ! mv "$tmp" "$dest"; then
        rm -f "$tmp"
        echo "ERROR: final move failed for $rel" >&2
        failed=$((failed + 1)); FAIL_LIST="$FAIL_LIST
  COPY-FAIL: $rel"
        continue
    fi
    echo "PROVISION $rel (verified $got)"
    provisioned=$((provisioned + 1))
done <<EOF
$MANIFEST_BODY
EOF

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "==============================================================="
if [ "$CHECK_ONLY" -eq 1 ]; then
    echo "build-inputs CHECK: $ok_present/$total present + verified"
else
    echo "build-inputs provision: $((ok_present + provisioned))/$total ready" \
         "($ok_present already present, $provisioned newly provisioned)"
fi
if [ "$failed" -gt 0 ]; then
    echo "FAILED entries ($failed):$FAIL_LIST"
    echo "==============================================================="
    echo "RESULT: FAIL — the checkout is NOT buildable until every entry is provisioned." >&2
    exit 1
fi
echo "RESULT: OK — all out-of-band build inputs present and SHA256-verified."
echo "==============================================================="
exit 0
