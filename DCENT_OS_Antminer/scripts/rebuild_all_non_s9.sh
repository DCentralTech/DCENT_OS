#!/usr/bin/env bash
#
# rebuild_all_non_s9.sh -- fail-closed non-S9 artifact inventory.
#
# Non-S9 target recipes exist inside build_in_docker.sh, but no authenticated
# outer capsule currently owns their source, result, signing, and publication
# lifecycles. The direct packaging lane is intentionally disabled. This
# compatibility entrypoint therefore MUST NOT call the inner driver or write
# digest files: doing so would create partial output that looks like build
# evidence even though every target is unadmitted.
#
# Usage:
#
#   bash DCENT_OS_Antminer/scripts/rebuild_all_non_s9.sh --list
#
# `--list` prints the dormant target-to-artifact contracts. Any rebuild
# invocation fails before filesystem or Docker mutation until a target-specific
# capsule is implemented.

set -euo pipefail

TARGETS=(
    am2-s19jpro
    am2-s19pro
    am2-s17pro
    am3-s19kpro
    am3-s21
    am3-s21pro
    am3-s21xp
    am3-s19jpro-aml
    am3-t21
    am3-bb
    am3-bb-s19jpro
)

declare -A TARBALL_FOR_TARGET=(
    [am2-s19jpro]="dcentos-sysupgrade-am2-s19jpro.tar"
    [am2-s19pro]="dcentos-sysupgrade-am2-s19pro.tar"
    [am2-s17pro]="dcentos-sysupgrade-am2-s17pro.tar"
    [am3-s19kpro]="dcentos-sysupgrade-am3-s19kpro.tar"
    [am3-s21]="dcentos-sysupgrade-am3-s21.tar"
    [am3-s21pro]="dcentos-sysupgrade-am3-s21pro.tar"
    [am3-s21xp]="dcentos-sysupgrade-am3-s21xp.tar"
    [am3-s19jpro-aml]="dcentos-sysupgrade-am3-s19jpro-aml.tar"
    [am3-t21]="dcentos-sysupgrade-am3-t21.tar"
    [am3-bb]="dcentos-am3-bb-sdcard.tar"
    [am3-bb-s19jpro]="dcentos-am3-bb-s19jpro-sdcard.tar"
)

usage() {
    cat <<'EOF'
Usage: rebuild_all_non_s9.sh [--list]

Non-S9 image rebuilding is unavailable until target-specific authenticated
capsules are implemented. --list prints the dormant artifact contracts without
building or writing files.
EOF
}

list_targets() {
    local target
    printf '%s\n' "TARGET|EXPECTED ARTIFACT (NOT CURRENTLY BUILDABLE)"
    for target in "${TARGETS[@]}"; do
        printf '%s|%s\n' "$target" "${TARBALL_FOR_TARGET[$target]}"
    done
}

case "${1:-}" in
    --list)
        if [ "$#" -ne 1 ]; then
            usage >&2
            exit 64
        fi
        list_targets
        exit 0
        ;;
    -h|--help)
        usage
        exit 0
        ;;
    "")
        ;;
    *)
        echo "ERROR: rebuild_all_non_s9.sh accepts only --list; packaging flags are not admitted" >&2
        usage >&2
        exit 64
        ;;
esac

echo "ERROR: non-S9 image rebuilding is unavailable: no target-specific authenticated capsule exists" >&2
echo "       no build, digest, partial pin, or publication files were written" >&2
echo "       inspect the dormant contracts with: $0 --list" >&2
exit 2
