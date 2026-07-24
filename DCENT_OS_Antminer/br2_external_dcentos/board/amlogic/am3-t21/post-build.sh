#!/bin/sh
#
# DCENTos post-build script — am3-t21 (T21 Amlogic NoPic variant)
# D-Central Technologies, Phase 4B (2026-05-15)
#
# Sibling of board/amlogic/am3-s21/post-build.sh. Same overlay-on-overlay
# pattern, same aarch64 toolchain, same shared `board/amlogic/rootfs-overlay/`
# base layer. T21 is the lower-tier S21 sibling on the same A113D carrier
# (BM1368 + NoPic TAS5782M); product identity is the only meaningful
# difference vs S21.
#
# Overlay-on-overlay (see defconfig): Buildroot applies the shared
# board/amlogic/rootfs-overlay/ FIRST, then this board's rootfs-overlay on
# top, then this script runs LAST.
#
# CRITICAL — :
#   The overlay MUST NOT ship a stale dcentrald binary. This script installs
#   the fresh cross-compiled aarch64 binary AFTER Buildroot copies overlays,
#   so the compiled daemon always wins over anything checked in.
#
# CRITICAL — :
#   board_target MUST be "am3-t21" here. Sysupgrade tarball prefix is
#   "sysupgrade-am3-t21/". Wrong name = silent failure on flash.
#
set -e
TARGET_DIR=$1

mkdir -p "${TARGET_DIR}/etc"
mkdir -p "${TARGET_DIR}/proc"
mkdir -p "${TARGET_DIR}/sys"
mkdir -p "${TARGET_DIR}/tmp"
mkdir -p "${TARGET_DIR}/root"
mkdir -p "${TARGET_DIR}/root/tools"
mkdir -p "${TARGET_DIR}/root/.ssh"
mkdir -p "${TARGET_DIR}/lib/modules"
mkdir -p "${TARGET_DIR}/usr/local/bin"
mkdir -p "${TARGET_DIR}/var/log"
mkdir -p "${TARGET_DIR}/data"
mkdir -p "${TARGET_DIR}/etc/dcentos"

# W1.5 (2026-05-07): pre-create /data/dcent/ with tight perms (auth.json holder).
mkdir -p "${TARGET_DIR}/data/dcent"
chmod 0700 "${TARGET_DIR}/data/dcent"
chown 0:0 "${TARGET_DIR}/data/dcent" 2>/dev/null || true

chmod +x "${TARGET_DIR}"/etc/init.d/* 2>/dev/null || true

# SSH/dashboard/MCP only. Do not ship BusyBox telnet on am3 images.
rm -f "${TARGET_DIR}/etc/init.d/S50telnet" "${TARGET_DIR}/usr/sbin/telnetd"
rm -f "${TARGET_DIR}/usr/sbin/in.telnetd" "${TARGET_DIR}/usr/bin/telnet"
rm -f "${TARGET_DIR}/etc/default/telnet" 2>/dev/null || true

chmod +x "${TARGET_DIR}"/etc/dcentos-early-init.sh 2>/dev/null || true
chmod +x "${TARGET_DIR}"/root/tools/*.py 2>/dev/null || true
chmod +x "${TARGET_DIR}"/root/tools/*.sh 2>/dev/null || true
chmod +x "${TARGET_DIR}"/usr/bin/dcent-shell 2>/dev/null || true
chmod +x "${TARGET_DIR}"/usr/sbin/sysupgrade 2>/dev/null || true
# W1.1 default-credential lockdown: SSH gate helper must be mode 0755.
chmod 0755 "${TARGET_DIR}"/usr/sbin/dcent-enable-ssh 2>/dev/null || true
chmod +x "${TARGET_DIR}"/root/web/server.py 2>/dev/null || true
chmod +x "${TARGET_DIR}"/root/web/mcp_server.py 2>/dev/null || true

# W5.1 (2026-05-07): install the dashboard SPA at the canonical location.
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
    echo "DCENTos post-build (am3-t21): installed dashboard SPA ($DASHBOARD_SIZE bytes) at /usr/share/dcentos-dashboard/index.html"
    if [ "$DASHBOARD_SIZE" -lt 100000 ]; then
        echo "DCENTos post-build (am3-t21): ERROR: dashboard appears truncated ($DASHBOARD_SIZE bytes < 100 KB floor)" >&2
        echo "  Run: cd DCENT_OS_Antminer/dashboard && npm run build" >&2
        exit 1
    fi
else
    echo "DCENTos post-build (am3-t21): ERROR: dashboard not found at $DASHBOARD_SRC" >&2
    echo "  Run: cd DCENT_OS_Antminer/dashboard && npm run build" >&2
    exit 1
fi

DCENTOS_INIT="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/aarch64-unknown-linux-musl/release/dcentos-init"
if [ -f "$DCENTOS_INIT" ]; then
    rm -f "${TARGET_DIR}/sbin/init" 2>/dev/null || true
    cp "$DCENTOS_INIT" "${TARGET_DIR}/sbin/init"
    chmod 755 "${TARGET_DIR}/sbin/init"
    echo "DCENTos post-build (am3-t21): installed dcentos-init as /sbin/init"
else
    echo "DCENTos post-build (am3-t21): WARNING: dcentos-init not found at $DCENTOS_INIT"
fi

DCENTRALD_BIN="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/aarch64-unknown-linux-musl/release/dcentrald"
if [ -f "$DCENTRALD_BIN" ]; then
    STAGED_BIN="${TARGET_DIR}/usr/local/bin/dcentrald"
    cp "$DCENTRALD_BIN" "$STAGED_BIN"
    chmod 755 "$STAGED_BIN"
    DCENTRALD_SIZE=$(stat -c%s "$DCENTRALD_BIN" 2>/dev/null || stat -f%z "$DCENTRALD_BIN")
    echo "DCENTos post-build (am3-t21): installed dcentrald ($DCENTRALD_SIZE bytes)"
else
    echo "DCENTos post-build (am3-t21): ERROR: dcentrald not found at $DCENTRALD_BIN" >&2
    exit 1
fi

STAGED_BIN="${TARGET_DIR}/usr/local/bin/dcentrald"
if [ ! -f "$STAGED_BIN" ]; then
    echo "DCENTos post-build (am3-t21): ERROR: dcentrald missing from $STAGED_BIN" >&2
    exit 1
fi
. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/dcentrald_version_gate.sh"
dcent_require_dcentrald_version_match \
    "$TARGET_DIR" \
    "$STAGED_BIN" \
    "DCENTos post-build (am3-t21)" \
    "${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/Cargo.toml"

if command -v md5sum > /dev/null 2>&1; then
    md5sum "$STAGED_BIN" | cut -d' ' -f1 > "${TARGET_DIR}/etc/dcentos/dcentrald.md5"
    echo "DCENTos post-build (am3-t21): md5 $(cat "${TARGET_DIR}/etc/dcentos/dcentrald.md5")"
fi

if [ ! -f "${TARGET_DIR}/etc/dcentrald.toml" ]; then
    echo "DCENTos post-build (am3-t21): ERROR: /etc/dcentrald.toml missing from rootfs." >&2
    echo "  Expected board/amlogic/am3-t21/rootfs-overlay/etc/dcentrald.toml" >&2
    exit 1
fi

# F-9 (Sweep-v3 PR-086): bare "am3" resolves PLATFORM=unknown in
# S99verify detect_platform() (no `am3` arm; board_target fallback
# skipped because board_family was set). Stamp the SKU-qualified
# `am3-aml-*` form (matches the `am3-aml*` classifier; same value as
# the platform file). Closes the per-family V5/V6/V7/V11/V12/V13
# cold-boot-proof override gap for am3-t21.
echo "am3-aml-t21"  > "${TARGET_DIR}/etc/dcentos/board_family"
echo "am3-t21"      > "${TARGET_DIR}/etc/dcentos/board_target"
echo "am3-aml-t21"  > "${TARGET_DIR}/etc/dcentos/platform"

# The AM3 revert helpers source lib/am3_geometry.sh beside /usr/sbin.
AM3_GEOMETRY_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/am3_geometry.sh"
if [ -f "$AM3_GEOMETRY_SRC" ]; then
    mkdir -p "${TARGET_DIR}/usr/sbin/lib"
    cp "$AM3_GEOMETRY_SRC" "${TARGET_DIR}/usr/sbin/lib/am3_geometry.sh"
    chmod 644 "${TARGET_DIR}/usr/sbin/lib/am3_geometry.sh" 2>/dev/null || true
    echo "DCENTos post-build (am3-t21): installed am3_geometry.sh for revert helpers"
else
    echo "DCENTos post-build (am3-t21): WARNING: am3_geometry.sh not found at $AM3_GEOMETRY_SRC" >&2
fi

# Phase 4C deliverable. The script may not exist yet when Phase 4B
# scaffolding lands; warn but do not fail. Once Phase 4C ships
# revert_to_stock_am3_aml_t21.sh, this becomes a hard install.
REVERT_T21_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/revert_to_stock_am3_aml_t21.sh"
if [ -f "$REVERT_T21_SRC" ]; then
    cp "$REVERT_T21_SRC" "${TARGET_DIR}/usr/sbin/revert_to_stock_am3_aml_t21.sh"
    chmod +x "${TARGET_DIR}/usr/sbin/revert_to_stock_am3_aml_t21.sh" 2>/dev/null || true
    echo "DCENTos post-build (am3-t21): installed revert_to_stock_am3_aml_t21.sh from scripts/"
else
    echo "DCENTos post-build (am3-t21): WARNING: revert_to_stock_am3_aml_t21.sh not found (Phase 4C pending) at $REVERT_T21_SRC" >&2
fi

# Stock-Bitmain manifest (parity with zynq board post-build).
MANIFEST_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../../../knowledge-base/firmware-archive/stock-bitmain-manifest.json"
if [ -f "$MANIFEST_SRC" ]; then
    cp "$MANIFEST_SRC" "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json"
    chmod 644 "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json" 2>/dev/null || true
    echo "DCENTos post-build (am3-t21): installed stock-bitmain-manifest.json"
fi

if [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
    if [ ! -f "${DCENT_RELEASE_PUBKEY_FILE}" ]; then
        echo "DCENTos post-build (am3-t21): ERROR: release public key not found at ${DCENT_RELEASE_PUBKEY_FILE}" >&2
        exit 1
    fi
    cp "${DCENT_RELEASE_PUBKEY_FILE}" "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    chmod 644 "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
elif [ "${DCENT_REQUIRE_RELEASE_KEY:-0}" = "1" ]; then
    echo "DCENTos post-build (am3-t21): ERROR: DCENT_REQUIRE_RELEASE_KEY=1 but DCENT_RELEASE_PUBKEY_FILE is not set" >&2
    exit 1
else
    echo "DCENTos post-build (am3-t21): WARNING: no release public key embedded (lab-only image)"
fi

echo "DCENTos post-build (am3-t21): directories, permissions, and board identity set."

# Production-readiness matrix §7 #1 (public-image trust boundary): release-image
# marker + first-boot SSH posture when DCENT_RELEASE_IMAGE=1. NO-OP on DEV/LAB.
. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/release_image_provision.sh"
dcent_provision_release_image "$TARGET_DIR" "am3-t21"
