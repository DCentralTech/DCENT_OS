#!/bin/sh
# Stable compatibility entry point for Zynq AM2 S19 stock-restore requests.
#
# The former implementation guessed an unreadable selector, accepted any
# non-"a" selector as slot B, identified payloads by a four-byte prefix, and
# committed three independent environment writes without transaction/readback.
# The API already marks this profile unverified.  Preserve the installed path,
# but deny direct SSH/serial callers the authority the API correctly withholds.

set -eu

show_plan() {
    printf '%s\n' 'DCENT_OS Zynq AM2 S19 stock restore: NOT IMPLEMENTED (mutation denied).'
    printf '%s\n' 'This compatibility path performs zero storage, environment, boot-selector, or restart operations.'
    printf '%s\n' 'Use signed model-specific recovery media or an evidence-backed shared update-engine restore transaction.'
}

if [ "$#" -eq 0 ]; then
    show_plan >&2
    printf '%s\n' 'Refusing: automatic stock download and inferred-slot restore are not admitted recovery contracts.' >&2
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
    printf '%s\n' 'Refusing: image paths and hashes cannot authorize an unproven slot/environment transaction.' >&2
    exit 78
fi

if [ "$saw_information" -eq 1 ]; then
    show_plan
    exit 0
fi

show_plan >&2
exit 78
