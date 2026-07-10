#!/bin/sh
#
# revert_common.sh — shared safety contract helpers for the per-family
# revert-to-stock scripts (Phase 4C, 2026-05-15 ).
#
# Sourced from:
#   revert_to_stock_xil.sh
#   revert_to_stock_am3_aml_s19jpro.sh
#   revert_to_stock_am3_aml_t21.sh
#   revert_to_stock_cv1835.sh
#   revert_to_stock_bcb100.sh
#
# Contract enforced:
#   1. `--operator-acknowledged-data-loss` flag (HARD-required for live exec).
#   2. Family-specific env-gate (e.g. DCENT_REVERT_AUTHORIZED=1) HARD-required
#      for live exec.
#   3. `--dry-run` flag prints planned commands without executing.
#   4. `--target <ip>` for SSH-style helpers (BCB100 publishes recipe only).
#   5. `--firmware <path>` / `--ramfs <path>` / `--bootbin <path>` per family.
#   6. JSON manifest emitted under
#        DCENT_OS_Antminer/output/revert-manifests/revert-<family>-<UTC>.json
#      with timestamp + SHA256 of restored artifact (+ family + target IP +
#      mode flag (dry-run|live) + script path + env-gate name).
#
# POSIX shell only — BusyBox ash compatible. NO bashisms. NO arrays. NO `local`.
#
# The contract lives in a shared helper so the 5 family scripts can never drift
# on the load-bearing safety checks (operator ack, env-gate, dry-run). Family-
# specific code paths (NAND mtd layout, eMMC slot, rootfs window, SSH transport)
# still live in each per-family script.

# ---------------------------------------------------------------------------
# Standard state vars — every revert script initializes these before parsing.
# ---------------------------------------------------------------------------

revert_init() {
    # $1 = family slug (xil | am3_aml_s19jpro | am3_aml_t21 | cv1835 | bcb100)
    # $2 = env-gate name (DCENT_REVERT_AUTHORIZED | DCENT_CV1835_REVERT_AUTHORIZED
    #      | DCENT_BCB100_REVERT_AUTHORIZED)
    REVERT_FAMILY="$1"
    REVERT_ENV_GATE_NAME="$2"
    REVERT_OPERATOR_ACK=0
    REVERT_DRY_RUN=0
    REVERT_TARGET=""
    REVERT_FIRMWARE=""
    REVERT_EXTRA_ARGS=""
    REVERT_SCRIPT_PATH="${0}"
}

# ---------------------------------------------------------------------------
# Argument parser shared across all 5 scripts. Reads $@ from the caller via
# explicit shift — the caller passes its own $@ in.
# ---------------------------------------------------------------------------

revert_parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --operator-acknowledged-data-loss)
                REVERT_OPERATOR_ACK=1
                shift
                ;;
            --dry-run)
                REVERT_DRY_RUN=1
                shift
                ;;
            --target)
                REVERT_TARGET="${2:-}"
                if [ -z "$REVERT_TARGET" ]; then
                    echo "ERROR: --target requires an IP/hostname argument." >&2
                    return 2
                fi
                shift 2
                ;;
            --firmware|--ramfs|--bootbin|--image)
                REVERT_FIRMWARE="${2:-}"
                if [ -z "$REVERT_FIRMWARE" ]; then
                    echo "ERROR: $1 requires a path argument." >&2
                    return 2
                fi
                shift 2
                ;;
            --help|-h)
                return 10
                ;;
            *)
                # Family-specific args get appended for the caller to process.
                REVERT_EXTRA_ARGS="$REVERT_EXTRA_ARGS $1"
                shift
                ;;
        esac
    done
    return 0
}

# ---------------------------------------------------------------------------
# Safety gate. Must be called BEFORE any destructive primitive. Refuses unless
# both the ack flag AND the env-gate are set, OR --dry-run is requested.
# ---------------------------------------------------------------------------

revert_check_authorization() {
    if [ "$REVERT_DRY_RUN" = "1" ]; then
        return 0
    fi
    if [ "$REVERT_OPERATOR_ACK" != "1" ]; then
        cat >&2 <<EOF
ERROR: missing --operator-acknowledged-data-loss flag.

This script overwrites stock-firmware partitions on a live miner. The
operator must acknowledge that all DCENT_OS config + persistent state on
the unit will be destroyed.

Re-run with:
    $REVERT_SCRIPT_PATH --operator-acknowledged-data-loss [other args]

Or, to see the exact commands without executing them, use:
    $REVERT_SCRIPT_PATH --dry-run [other args]
EOF
        return 1
    fi
    # POSIX-portable env-var lookup (no bashism indirect expansion).
    env_val=$(env | grep -E "^${REVERT_ENV_GATE_NAME}=1$" || true)
    if [ -z "$env_val" ]; then
        cat >&2 <<EOF
ERROR: missing required env-gate ${REVERT_ENV_GATE_NAME}=1.

This is a load-bearing second factor on top of --operator-acknowledged-data-loss.
Live destructive reverts require the operator to explicitly set the family-
specific env-gate in the same shell that invokes the script.

Re-run with:
    ${REVERT_ENV_GATE_NAME}=1 $REVERT_SCRIPT_PATH --operator-acknowledged-data-loss [other args]
EOF
        return 1
    fi
    return 0
}

# ---------------------------------------------------------------------------
# Wrapper around `sha256sum` that returns "unavailable" on hosts without it.
# ---------------------------------------------------------------------------

revert_sha256() {
    # $1 = path
    if [ ! -f "$1" ]; then
        echo "missing"
        return 1
    fi
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" 2>/dev/null | awk '{print $1}'
        return 0
    fi
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" 2>/dev/null | awk '{print $1}'
        return 0
    fi
    echo "unavailable"
    return 1
}

# ---------------------------------------------------------------------------
# UTC timestamp in compact form for manifest filenames + JSON timestamp field.
# ---------------------------------------------------------------------------

revert_utc_stamp() {
    # YYYYMMDDTHHMMSSZ
    date -u +%Y%m%dT%H%M%SZ
}

revert_utc_iso() {
    date -u +%Y-%m-%dT%H:%M:%SZ
}

# ---------------------------------------------------------------------------
# Emit the JSON manifest under DCENT_OS_Antminer/output/revert-manifests/.
# Best-effort: if the output dir can't be created (read-only fs on the miner),
# falls back to /tmp.
# ---------------------------------------------------------------------------

revert_emit_manifest() {
    # $1 = restored artifact path (the stock image / ramfs / boot.bin)
    # $2 = result code: planned | executed | refused | published
    artifact_path="$1"
    result="$2"
    if [ -n "$artifact_path" ]; then
        artifact_sha=$(revert_sha256 "$artifact_path" 2>/dev/null || echo "missing")
    else
        artifact_sha="n/a"
    fi
    stamp=$(revert_utc_stamp)
    iso=$(revert_utc_iso)
    out_dir="${DCENT_REVERT_MANIFEST_DIR:-}"
    if [ -z "$out_dir" ]; then
        # Heuristic: if running inside the repo on a dev box, write under the
        # canonical path. Otherwise fall back to /tmp on the miner.
        # Resolve absolute path even when invoked via relative path.
        if [ -d "$(dirname "$REVERT_SCRIPT_PATH")" ]; then
            scripts_dir=$(CDPATH= cd "$(dirname "$REVERT_SCRIPT_PATH")" 2>/dev/null && pwd 2>/dev/null || echo "")
        else
            scripts_dir=""
        fi
        if [ -n "$scripts_dir" ] && [ -d "$scripts_dir/../output" ]; then
            out_dir="$scripts_dir/../output/revert-manifests"
        elif [ -n "$scripts_dir" ] && [ -d "$scripts_dir/.." ] && [ -w "$scripts_dir/.." ]; then
            # Repo dev host but `output/` not yet present — create it.
            out_dir="$scripts_dir/../output/revert-manifests"
        else
            out_dir="/tmp/revert-manifests"
        fi
    fi
    if ! mkdir -p "$out_dir" 2>/dev/null; then
        out_dir="/tmp/revert-manifests"
        mkdir -p "$out_dir" 2>/dev/null || true
    fi
    manifest_path="$out_dir/revert-${REVERT_FAMILY}-${stamp}.json"
    if [ "$REVERT_DRY_RUN" = "1" ]; then
        mode="dry-run"
    else
        mode="live"
    fi
    cat >"$manifest_path" <<EOF
{
  "schema": "dcentos.revert.manifest/1",
  "family": "${REVERT_FAMILY}",
  "timestamp_utc": "${iso}",
  "mode": "${mode}",
  "result": "${result}",
  "target": "${REVERT_TARGET}",
  "firmware_path": "${REVERT_FIRMWARE}",
  "firmware_sha256": "${artifact_sha}",
  "env_gate_required": "${REVERT_ENV_GATE_NAME}",
  "operator_acknowledged_data_loss": ${REVERT_OPERATOR_ACK},
  "script": "${REVERT_SCRIPT_PATH}"
}
EOF
    if [ -f "$manifest_path" ]; then
        echo "Manifest: $manifest_path"
    fi
}

# ---------------------------------------------------------------------------
# Run-or-print helper: in dry-run prints the command and returns 0 without
# executing. In live mode actually runs it.
# ---------------------------------------------------------------------------

revert_run() {
    if [ "$REVERT_DRY_RUN" = "1" ]; then
        echo "[dry-run] $*"
        return 0
    fi
    "$@"
}

# ---------------------------------------------------------------------------
# SSH preflight: optional reachability check before any destructive action.
# Soft-fails (warns but does not abort) to keep the dry-run path useful with
# offline / fake IPs.
# ---------------------------------------------------------------------------

revert_ssh_preflight() {
    # $1 = target IP
    target="$1"
    if [ -z "$target" ]; then
        return 0
    fi
    if ! command -v ssh >/dev/null 2>&1; then
        echo "WARN: ssh client unavailable; skipping reachability preflight." >&2
        return 0
    fi
    if [ "$REVERT_DRY_RUN" = "1" ]; then
        echo "[dry-run] ssh root@${target} 'echo reachable'"
        return 0
    fi
    if ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=no \
            "root@${target}" 'echo reachable' >/dev/null 2>&1; then
        echo "Preflight: ${target} reachable over SSH."
        return 0
    fi
    echo "WARN: ${target} not reachable over SSH at preflight." >&2
    return 0
}
