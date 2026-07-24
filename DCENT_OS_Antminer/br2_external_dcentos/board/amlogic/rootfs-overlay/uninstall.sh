#!/bin/sh
# Stable compatibility entry point for Amlogic stock-restore requests.
#
# The previously shipped implementation copied a LuxOS S19k procedure: corrupt
# a single-copy /dev/nand_env, pivot_root, wipe /mnt/nvdata, and hard reboot.
# That observation does not establish a safe DCENT_OS restore transaction for
# every Amlogic model, bootloader default, or rootfs layout. Keep the pathname
# for operator/API compatibility, but grant it no mutation authority.

set -eu

show_plan() {
    printf '%s\n' 'DCENT_OS Amlogic stock restore: NOT IMPLEMENTED (mutation denied).'
    printf '%s\n' 'This compatibility path performs zero environment, storage, rootfs, or reboot operations.'
    printf '%s\n' 'Use a signed model-specific SD/USB recovery image or an evidence-backed typed serial/offline recovery procedure.'
}

case "${1-}" in
    ''|--help|-h|--dry-run|--plan)
        show_plan
        exit 0
        ;;
    --confirm-uninstall)
        show_plan >&2
        printf '%s\n' 'Refusing: the captured LuxOS bad-CRC procedure is not a DCENT_OS recovery contract.' >&2
        exit 78
        ;;
    *)
        printf 'ERROR: unknown argument: %s\n' "$1" >&2
        show_plan >&2
        exit 2
        ;;
esac
