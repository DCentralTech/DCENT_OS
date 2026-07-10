#!/bin/sh
#
# Default DCENT_OS platform identity for init scripts.

DCENTOS_PLATFORM_FILE=/etc/dcentos-platform
DCENTOS_MODE_FILE=/etc/dcentos-mode
DCENTOS_VERSION_FILE=/etc/dcentos-version

read_first_line() {
    [ -f "$1" ] || return 1
    sed -n '1{s/[[:space:]]//g;p;q;}' "$1" 2>/dev/null
}

DCENTOS_PLATFORM=$(read_first_line "$DCENTOS_PLATFORM_FILE" || echo "am3-aml")
DCENTOS_MODE=$(read_first_line "$DCENTOS_MODE_FILE" || echo "nand")
DCENTOS_VERSION=$(read_first_line "$DCENTOS_VERSION_FILE" || echo "unknown")

# Compatibility aliases for small scripts that still use BOS-era names.
BOARD_NAME=$DCENTOS_PLATFORM
BOS_MODE=$DCENTOS_MODE
BOS_VERSION=$DCENTOS_VERSION
BOS_VERSION_SUFFIX=""
BOS_SUPPORT_FACTORY_RESET=no
BOS_SUPPORT_RUN_RECOVERY=no
