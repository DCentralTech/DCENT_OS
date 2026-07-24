#!/bin/sh
# Stable compatibility entry point for the legacy SD-to-NAND installer.
#
# The former implementation admitted boards heuristically, could mutate media
# during --dry-run, did not stop after several failed writers, and maintained a
# raw whole-environment fallback outside the shared update transaction.  Keep
# the pathname for operator and automation compatibility, but grant it no
# storage, boot-environment, or reboot authority.

set -eu

show_plan() {
    printf '%s\n' 'DCENT_OS SD-to-NAND install: NOT IMPLEMENTED (mutation denied).'
    printf '%s\n' 'This compatibility path performs zero storage, environment, boot-selector, or restart operations.'
    printf '%s\n' 'Use a signed model-specific installer backed by the shared update engine, or keep the verified SD boot media installed.'
}

if [ "$#" -eq 0 ]; then
    show_plan >&2
    printf '%s\n' 'Refusing: the legacy installer has no exact admitted hardware/update descriptor.' >&2
    exit 78
fi

saw_information=0
saw_mutation=0
unknown_argument=''

while [ "$#" -gt 0 ]; do
    case "$1" in
        --help|-h|--plan|--dry-run)
            saw_information=1
            ;;
        --yes|-y|--preserve-env|--force-unsafe|--slot=*)
            saw_mutation=1
            ;;
        --slot)
            saw_mutation=1
            if [ "$#" -gt 1 ]; then
                shift
            fi
            ;;
        --*)
            unknown_argument=$1
            break
            ;;
        *)
            saw_mutation=1
            ;;
    esac
    shift
done

if [ -n "$unknown_argument" ]; then
    printf 'ERROR: unknown argument: %s\n' "$unknown_argument" >&2
    show_plan >&2
    exit 2
fi

if [ "$saw_mutation" -eq 1 ]; then
    show_plan >&2
    printf '%s\n' 'Refusing: legacy install-shaped arguments cannot authorize NAND or boot-environment mutation.' >&2
    exit 78
fi

if [ "$saw_information" -eq 1 ]; then
    show_plan
    exit 0
fi

show_plan >&2
exit 78
