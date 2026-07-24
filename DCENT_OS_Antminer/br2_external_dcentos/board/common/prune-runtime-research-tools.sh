#!/bin/sh
# Remove standalone hardware and boot-state research executors from normal
# runtime images.
#
# These sources remain in the repository for offline protocol research, but
# they open I2C/UART/UIO/devmem independently and therefore cannot coexist with
# dcentrald's exclusive hardware ownership. A future repair image may install
# them behind an explicit maintenance-mode lease. Until then, every product
# applies this post-build step after all packages and rootfs overlays so a
# shared Zynq overlay or package cannot accidentally reintroduce them.

set -eu

TARGET_DIR=${1:?Buildroot TARGET_DIR argument is required}
if [ ! -d "$TARGET_DIR" ]; then
    echo "refusing non-directory TARGET_DIR: '$TARGET_DIR'" >&2
    exit 1
fi
TARGET_DIR=$(CDPATH= cd "$TARGET_DIR" && pwd -P)
case "$TARGET_DIR" in
    /|"")
        echo "refusing unsafe TARGET_DIR: '$TARGET_DIR'" >&2
        exit 1
        ;;
esac
if [ ! -d "$TARGET_DIR/etc" ] || [ ! -d "$TARGET_DIR/bin" ]; then
    echo "refusing TARGET_DIR without Buildroot rootfs sentinels: '$TARGET_DIR'" >&2
    exit 1
fi

# Never traverse an intermediate symlink while deleting. A malformed staging
# tree with root -> /somewhere or usr -> /somewhere could otherwise turn a
# bounded prune into a host-filesystem mutation outside TARGET_DIR.
for component in root usr usr/bin usr/sbin; do
    if [ -L "$TARGET_DIR/$component" ]; then
        echo "refusing TARGET_DIR with symlinked delete-path component: '$component'" >&2
        exit 1
    fi
done

# The whole directory is a research-only namespace. Removing the namespace,
# rather than a filename allowlist, also excludes ignored bytecode, editor
# artifacts, and future executors copied by Buildroot's overlay merge.
rm -rf "$TARGET_DIR/root/tools"
rm -f "$TARGET_DIR/usr/bin/dcent-shell"

# switch_firmware.{py,sh} are offline U-Boot environment-image transformers,
# not target-side recovery authorities.  Remove both names after every overlay
# and package has run so a warm Buildroot tree or future package cannot restore
# the former raw-environment patcher to a production image.
rm -f "$TARGET_DIR/usr/sbin/switch_firmware.py" \
    "$TARGET_DIR/usr/sbin/switch_firmware.sh"
