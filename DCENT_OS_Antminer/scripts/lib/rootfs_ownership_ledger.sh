#!/bin/sh
# Emit and immediately verify the bounded final-rootfs ownership ledger.
#
# This binds the finalized Buildroot target tree, package path claims, declared
# overlays, the ordered direct post-build script definitions, and the exact
# emitted rootfs payload. It deliberately does not claim that the ledger is an
# SBOM or proves causal content origin. Direct TARGET_DIR mutations remain
# unattributed, but every declared mutator's bytes are bound into the ledger.

dcent_emit_rootfs_ownership_ledger() {
    _dcent_rootfs_artifact=$1
    _dcent_ledger_output=$2
    shift 2

    : "${TARGET_DIR:?TARGET_DIR is required for rootfs ownership ledger}"
    : "${BASE_DIR:?BASE_DIR is required for rootfs ownership ledger}"
    : "${BR2_EXTERNAL_DCENTOS_PATH:?BR2_EXTERNAL_DCENTOS_PATH is required for rootfs ownership ledger}"

    _dcent_project_root=$(CDPATH= cd "${BR2_EXTERNAL_DCENTOS_PATH}/.." && pwd)
    _dcent_analyzer="${_dcent_project_root}/scripts/rootfs_ownership_ledger.py"
    _dcent_build_dir="${BASE_DIR}/build"

    command -v python3 >/dev/null 2>&1 || {
        echo "ERROR: python3 is required for final-rootfs ownership ledger generation" >&2
        return 1
    }
    [ -f "$_dcent_analyzer" ] || {
        echo "ERROR: final-rootfs ownership analyzer missing: $_dcent_analyzer" >&2
        return 1
    }

    # Pass the caller's named evidence arguments through unchanged. This keeps
    # the helper open to any future ordered post-build mutator, overlay, or
    # hook supported by rootfs_ownership_ledger.py without another positional
    # signature migration.
    python3 "$_dcent_analyzer" \
        --target-dir "$TARGET_DIR" \
        --build-dir "$_dcent_build_dir" \
        "$@" \
        --artifact "$_dcent_rootfs_artifact" \
        --output "$_dcent_ledger_output"
    python3 "$_dcent_analyzer" \
        --target-dir "$TARGET_DIR" \
        --build-dir "$_dcent_build_dir" \
        "$@" \
        --artifact "$_dcent_rootfs_artifact" \
        --verify "$_dcent_ledger_output"
}
