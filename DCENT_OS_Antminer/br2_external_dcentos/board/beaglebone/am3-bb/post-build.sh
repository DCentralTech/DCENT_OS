#!/bin/sh
#
# DCENTos post-build script - am3-bb BeagleBone Black.
#
# Installs the fresh armv7 dcentrald binary and stamps board identity. It
# deliberately does not stage stock cgminer, daemons, monitor-ipsig, or
# uart_trans.ko.

set -e
TARGET_DIR=$1

. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/buildroot_rootfs_arch_guard.sh"

mkdir -p "${TARGET_DIR}/etc/dcentos"
mkdir -p "${TARGET_DIR}/usr/local/bin"
mkdir -p "${TARGET_DIR}/data"
mkdir -p "${TARGET_DIR}/tmp"
mkdir -p "${TARGET_DIR}/var/log"

# W1.5 (2026-05-07): pre-create /data/dcent/ with tight perms (auth.json holder).
mkdir -p "${TARGET_DIR}/data/dcent"
chmod 0700 "${TARGET_DIR}/data/dcent"
chown 0:0 "${TARGET_DIR}/data/dcent" 2>/dev/null || true

chmod +x "${TARGET_DIR}"/etc/init.d/* 2>/dev/null || true
chmod +x "${TARGET_DIR}"/etc/dcentos-early-init.sh 2>/dev/null || true
chmod +x "${TARGET_DIR}"/root/web/*.py 2>/dev/null || true
# W1.1 default-credential lockdown: SSH gate helper must be mode 0755.
chmod 0755 "${TARGET_DIR}"/usr/sbin/dcent-enable-ssh 2>/dev/null || true

dcent_ensure_armv7_busybox_init "$TARGET_DIR" "DCENTos post-build (am3-bb)"

# W5.1 (2026-05-07): install the dashboard SPA at the canonical location
# served by server.py on the other platforms. The am3-bb image is currently
# bring-up-only and does not yet ship server.py, but staging the artifact
# at /usr/share/dcentos-dashboard keeps parity so any future server.py /
# /usr/local/bin/dcentrald-served-dashboard route finds the same file.
# See DCENT_OS_Antminer/br2_external_dcentos/board/zynq/post-build.sh for
# the full rationale (W5.1 decoupling from `include_str!`).
DASHBOARD_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../dashboard/dist/index.html"
DASHBOARD_GZ_SRC="${DASHBOARD_SRC}.gz"
DASHBOARD_SHA_SRC="${DASHBOARD_SRC}.sha256"
DASHBOARD_DEST_DIR="${TARGET_DIR}/usr/share/dcentos-dashboard"
if [ -f "$DASHBOARD_SRC" ]; then
    mkdir -p "$DASHBOARD_DEST_DIR"
    cp "$DASHBOARD_SRC" "$DASHBOARD_DEST_DIR/index.html"
    chmod 644 "$DASHBOARD_DEST_DIR/index.html"
    if [ ! -f "$DASHBOARD_GZ_SRC" ] || [ "$DASHBOARD_SRC" -nt "$DASHBOARD_GZ_SRC" ]; then
        gzip -9 -c "$DASHBOARD_SRC" > "$DASHBOARD_DEST_DIR/index.html.gz"
    else
        cp "$DASHBOARD_GZ_SRC" "$DASHBOARD_DEST_DIR/index.html.gz"
    fi
    if [ ! -f "$DASHBOARD_SHA_SRC" ] || [ "$DASHBOARD_SRC" -nt "$DASHBOARD_SHA_SRC" ]; then
        sha256sum "$DASHBOARD_SRC" | awk '{print $1}' > "$DASHBOARD_DEST_DIR/index.html.sha256"
    else
        cp "$DASHBOARD_SHA_SRC" "$DASHBOARD_DEST_DIR/index.html.sha256"
    fi
    chmod 644 "$DASHBOARD_DEST_DIR/index.html.gz" "$DASHBOARD_DEST_DIR/index.html.sha256"
    DASHBOARD_SIZE=$(stat -c%s "$DASHBOARD_SRC" 2>/dev/null || stat -f%z "$DASHBOARD_SRC")
    echo "DCENTos post-build (am3-bb): staged dashboard SPA ($DASHBOARD_SIZE bytes) at /usr/share/dcentos-dashboard/index.html"
    if [ "$DASHBOARD_SIZE" -lt 100000 ]; then
        echo "DCENTos post-build (am3-bb): WARN: dashboard appears truncated ($DASHBOARD_SIZE bytes < 100 KB floor)" >&2
    fi
else
    echo "DCENTos post-build (am3-bb): WARN: dashboard not found at $DASHBOARD_SRC; skipping (am3-bb does not yet serve a dashboard)" >&2
fi

DCENTRALD_BIN="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentrald"
if [ -f "$DCENTRALD_BIN" ]; then
    STAGED_BIN="${TARGET_DIR}/usr/local/bin/dcentrald"
    cp "$DCENTRALD_BIN" "$STAGED_BIN"
    chmod 755 "$STAGED_BIN"
    DCENTRALD_SIZE=$(stat -c%s "$DCENTRALD_BIN" 2>/dev/null || stat -f%z "$DCENTRALD_BIN")
    echo "DCENTos post-build (am3-bb): installed dcentrald ($DCENTRALD_SIZE bytes)"
else
    echo "DCENTos post-build (am3-bb): ERROR: dcentrald not found at $DCENTRALD_BIN" >&2
    echo "  Build first: cargo build --release --target armv7-unknown-linux-musleabihf" >&2
    exit 1
fi

. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/dcentrald_version_gate.sh"
dcent_require_dcentrald_version_match \
    "$TARGET_DIR" \
    "${TARGET_DIR}/usr/local/bin/dcentrald" \
    "DCENTos post-build (am3-bb)" \
    "${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/Cargo.toml"

DISCOVERY_BIN="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentos-discovery"
if [ -f "$DISCOVERY_BIN" ]; then
    cp "$DISCOVERY_BIN" "${TARGET_DIR}/usr/local/bin/dcentos-discovery"
    chmod 755 "${TARGET_DIR}/usr/local/bin/dcentos-discovery"
    DISCOVERY_SIZE=$(stat -c%s "$DISCOVERY_BIN" 2>/dev/null || stat -f%z "$DISCOVERY_BIN")
    echo "DCENTos post-build (am3-bb): installed dcentos-discovery ($DISCOVERY_SIZE bytes)"
else
    echo "DCENTos post-build (am3-bb): ERROR: dcentos-discovery not found at $DISCOVERY_BIN" >&2
    echo "  Build first: cargo build --release --target armv7-unknown-linux-musleabihf --bin dcentos-discovery" >&2
    exit 1
fi

dcent_require_armv7_eabi_elf_paths \
    "$TARGET_DIR" \
    "DCENTos post-build (am3-bb)" \
    "sbin/init" \
    "usr/sbin/dropbear" \
    "usr/local/bin/dcentrald" \
    "usr/local/bin/dcentos-discovery"

if [ ! -f "${TARGET_DIR}/etc/dcentrald.toml" ]; then
    echo "DCENTos post-build (am3-bb): ERROR: /etc/dcentrald.toml missing from rootfs." >&2
    exit 1
fi

echo "am3-bb" > "${TARGET_DIR}/etc/dcentos/board_family"
echo "am3-bb" > "${TARGET_DIR}/etc/dcentos/board_target"
echo "am3-bb" > "${TARGET_DIR}/etc/dcentos/platform"
echo "management-bringup-sdcard-only" > "${TARGET_DIR}/etc/dcentos/board_status"
echo "nand-install-and-revert-disabled-pending-live-proc-mtd" > "${TARGET_DIR}/etc/dcentos/storage_status"

# Install the per-platform revert script as a disabled/status-reporting helper.
# It refuses NAND writes until dated live /proc/mtd evidence is supplied.
REVERT_BB_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/revert_to_stock_am335x_bb.sh"
if [ -f "$REVERT_BB_SRC" ]; then
    mkdir -p "${TARGET_DIR}/usr/sbin"
    cp "$REVERT_BB_SRC" "${TARGET_DIR}/usr/sbin/revert_to_stock_am335x_bb.sh"
    chmod +x "${TARGET_DIR}/usr/sbin/revert_to_stock_am335x_bb.sh" 2>/dev/null || true
    echo "DCENTos post-build (am3-bb): installed disabled revert_to_stock_am335x_bb.sh from scripts/"
else
    echo "DCENTos post-build (am3-bb): WARNING: revert_to_stock_am335x_bb.sh not found at $REVERT_BB_SRC" >&2
fi

#  W12-B: also ship the stock-Bitmain manifest (parity with
# zynq + amlogic boards). The daemon probes /etc/dcentos/ first.
MANIFEST_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../../../knowledge-base/firmware-archive/stock-bitmain-manifest.json"
if [ -f "$MANIFEST_SRC" ]; then
    mkdir -p "${TARGET_DIR}/etc/dcentos"
    cp "$MANIFEST_SRC" "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json"
    chmod 644 "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json" 2>/dev/null || true
    echo "DCENTos post-build (am3-bb): installed stock-bitmain-manifest.json"
fi

if command -v md5sum > /dev/null 2>&1; then
    md5sum "${TARGET_DIR}/usr/local/bin/dcentrald" | cut -d' ' -f1 > "${TARGET_DIR}/etc/dcentos/dcentrald.md5"
    md5sum "${TARGET_DIR}/usr/local/bin/dcentos-discovery" | cut -d' ' -f1 > "${TARGET_DIR}/etc/dcentos/dcentos-discovery.md5"
fi

find "${TARGET_DIR}" \( \
    -name 'uart_trans.ko' -o \
    -name 'monitor-ipsig' -o \
    -name 'S65monitor-ipsig' -o \
    -name 'daemons' -o \
    -name 'daemonc' -o \
    -name 'update-daemon' -o \
    -name 'S67update-daemon' -o \
    -name 'updateporc.sh' \
\) -print -quit 2>/dev/null | grep -q . && {
    echo "DCENTos post-build (am3-bb): ERROR: forbidden stock daemon/blob present in image" >&2
    exit 1
}

# W7.7 (DCENT_RE + DCENT_Security, 2026-05-07): the daemons:22322 privesc on
# stock Bitmain BB also gets a defense-in-depth netfilter drop. The init
# script (S41firewall) sits between S40network and S50dropbear so the rules
# are in place before any user-facing daemon binds. iptables is pulled in
# via BR2_PACKAGE_IPTABLES=y in the BB defconfig. See docs/THREAT_MODEL.md
# (BB-1) and .
FIREWALL_INIT="${TARGET_DIR}/etc/init.d/S41firewall"
if [ ! -x "$FIREWALL_INIT" ]; then
    echo "DCENTos post-build (am3-bb): ERROR: S41firewall missing or not executable at $FIREWALL_INIT" >&2
    echo "  Expected from board/beaglebone/am3-bb/rootfs-overlay/etc/init.d/S41firewall." >&2
    exit 1
fi
echo "DCENTos post-build (am3-bb): S41firewall hardening init script present"

echo "DCENTos post-build (am3-bb): board identity set; stock cgminer/uart_trans/monitor-ipsig/daemons omitted."

# CE-204: embed the trusted release public key when provided (same pattern as
# the zynq am2-s19jpro post-build), so a future BB install route can verify the
# signed SD-payload sidecars against the pinned key.
if [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
    if [ ! -f "${DCENT_RELEASE_PUBKEY_FILE}" ]; then
        echo "DCENTos post-build (am3-bb): ERROR: release public key not found at ${DCENT_RELEASE_PUBKEY_FILE}" >&2
        exit 1
    fi
    cp "${DCENT_RELEASE_PUBKEY_FILE}" "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    chmod 644 "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    echo "DCENTos post-build (am3-bb): embedded release public key"
elif [ "${DCENT_REQUIRE_RELEASE_KEY:-0}" = "1" ]; then
    echo "DCENTos post-build (am3-bb): ERROR: DCENT_REQUIRE_RELEASE_KEY=1 but DCENT_RELEASE_PUBKEY_FILE is not set" >&2
    exit 1
else
    echo "DCENTos post-build (am3-bb): WARNING: no release public key embedded (lab-only image)"
fi

# Production-readiness matrix §7 #1 (public-image trust boundary): release-image
# marker + first-boot SSH posture when DCENT_RELEASE_IMAGE=1. NO-OP on DEV/LAB.
. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/release_image_provision.sh"
dcent_provision_release_image "$TARGET_DIR" "am3-bb"
