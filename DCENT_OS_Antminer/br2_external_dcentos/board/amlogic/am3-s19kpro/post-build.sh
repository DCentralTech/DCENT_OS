#!/bin/sh
#
# DCENTos post-build script — am3-s19kpro (S19K Pro Amlogic NoPic variant)
# D-Central Technologies, 2026
#
# Phase H.9 scaffold (2026-04-29) — first build will be in Phase J on .78.
#
# Sibling of board/zynq/am2-s19jpro/post-build.sh. Same overlay-on-overlay
# pattern, different toolchain (aarch64), different default config (am3-aml
# baseline already shipped under rootfs-overlay/etc/dcentrald.toml).
#
# Overlay-on-overlay (see defconfig): Buildroot applies the shared
# board/amlogic/rootfs-overlay/ FIRST, then this board's rootfs-overlay on
# top, then this script runs LAST.
#
# CRITICAL — :
#   The overlay MUST NOT ship a stale dcentrald binary. This script installs
#   the fresh cross-compiled aarch64 binary AFTER Buildroot copies overlays,
#   so the compiled daemon always wins over anything checked in. Refuses to
#   build if the binary is missing.
#
# CRITICAL — :
#   board_target MUST be "am3-s19k" here. Sysupgrade tarball prefix is
#   "sysupgrade-am3-s19k/". Wrong name = silent failure = brick on flash.
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

# W1.1 default-credential lockdown: SSH gate helper must be mode 0755.
chmod 0755 "${TARGET_DIR}"/usr/sbin/dcent-enable-ssh 2>/dev/null || true

# W1.5 (2026-05-07): pre-create /data/dcent/ with tight perms (auth.json holder).
mkdir -p "${TARGET_DIR}/data/dcent"
chmod 0700 "${TARGET_DIR}/data/dcent"
chown 0:0 "${TARGET_DIR}/data/dcent" 2>/dev/null || true

# Make init scripts executable (from either overlay layer)
chmod +x "${TARGET_DIR}"/etc/init.d/* 2>/dev/null || true

# Remote access is Dropbear SSH only. Buildroot's default BusyBox config can
# install S50telnet + telnet applets; remove them from am3 images so port 23
# is never exposed and no telnet tooling ships in the rootfs.
rm -f "${TARGET_DIR}/etc/init.d/S50telnet" "${TARGET_DIR}/usr/sbin/telnetd"
rm -f "${TARGET_DIR}/usr/sbin/in.telnetd" "${TARGET_DIR}/usr/bin/telnet"
rm -f "${TARGET_DIR}/etc/default/telnet" 2>/dev/null || true

# Make early-init and persistent storage scripts executable
chmod +x "${TARGET_DIR}"/etc/dcentos-early-init.sh 2>/dev/null || true

# Make tools executable
chmod +x "${TARGET_DIR}"/root/tools/*.py 2>/dev/null || true
chmod +x "${TARGET_DIR}"/root/tools/*.sh 2>/dev/null || true
chmod +x "${TARGET_DIR}"/usr/bin/dcent-shell 2>/dev/null || true
chmod +x "${TARGET_DIR}"/usr/sbin/sysupgrade 2>/dev/null || true
chmod +x "${TARGET_DIR}"/usr/sbin/switch_firmware.py 2>/dev/null || true

# Make web server and MCP server executable
chmod +x "${TARGET_DIR}"/root/web/server.py 2>/dev/null || true
chmod +x "${TARGET_DIR}"/root/web/mcp_server.py 2>/dev/null || true

# W5.1 (2026-05-07): install the dashboard SPA at the canonical location
# served by server.py. See DCENT_OS_Antminer/br2_external_dcentos/board/zynq/
# post-build.sh for the full rationale.
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
    echo "DCENTos post-build (am3-s19kpro): installed dashboard SPA ($DASHBOARD_SIZE bytes) at /usr/share/dcentos-dashboard/index.html"
    if [ "$DASHBOARD_SIZE" -lt 100000 ]; then
        echo "DCENTos post-build (am3-s19kpro): ERROR: dashboard appears truncated ($DASHBOARD_SIZE bytes < 100 KB floor)" >&2
        echo "  Run: cd DCENT_OS_Antminer/dashboard && npm run build" >&2
        exit 1
    fi
else
    echo "DCENTos post-build (am3-s19kpro): ERROR: dashboard not found at $DASHBOARD_SRC" >&2
    echo "  Run: cd DCENT_OS_Antminer/dashboard && npm run build" >&2
    exit 1
fi

# Install dcentos-init as /sbin/init if present (aarch64 build)
DCENTOS_INIT="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/aarch64-unknown-linux-musl/release/dcentos-init"
if [ -f "$DCENTOS_INIT" ]; then
    rm -f "${TARGET_DIR}/sbin/init" 2>/dev/null || true
    cp "$DCENTOS_INIT" "${TARGET_DIR}/sbin/init"
    chmod 755 "${TARGET_DIR}/sbin/init"
    echo "DCENTos post-build (am3-s19kpro): installed dcentos-init as /sbin/init"
else
    echo "DCENTos post-build (am3-s19kpro): WARNING: dcentos-init not found at $DCENTOS_INIT"
    echo "  BusyBox init will be used (requires CONFIG_INIT=y in busybox.config)"
fi

# -----------------------------------------------------------------------------
# Install dcentrald (fresh cross-compiled aarch64 binary beats any stale
# overlay). am3-aml = Amlogic A113D, Cortex-A53, aarch64-unknown-linux-musl.
# -----------------------------------------------------------------------------
DCENTRALD_BIN="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/aarch64-unknown-linux-musl/release/dcentrald"
if [ -f "$DCENTRALD_BIN" ]; then
    STAGED_BIN="${TARGET_DIR}/usr/local/bin/dcentrald"
    cp "$DCENTRALD_BIN" "$STAGED_BIN"
    chmod 755 "$STAGED_BIN"
    DCENTRALD_SIZE=$(stat -c%s "$DCENTRALD_BIN" 2>/dev/null || stat -f%z "$DCENTRALD_BIN")
    echo "DCENTos post-build (am3-s19kpro): installed dcentrald ($DCENTRALD_SIZE bytes)"
else
    echo "DCENTos post-build (am3-s19kpro): ERROR: dcentrald not found at $DCENTRALD_BIN" >&2
    echo "  Refusing to ship a stale overlay daemon. Build dcentrald first:" >&2
    echo "    CC_aarch64_unknown_linux_musl=zig-cc-aarch64.bat \\" >&2
    echo "    AR_aarch64_unknown_linux_musl=zig-ar-aarch64.bat \\" >&2
    echo "    cargo build --release --target aarch64-unknown-linux-musl" >&2
    exit 1
fi

# -----------------------------------------------------------------------------
# Freshness audit:
# Surface stale binaries loudly in the build log + stamp MD5 into the image
# so the runtime can report it via /api/system/info.
#
# Observational only — does NOT fail the build. A WARN in the log during CI
# is enough signal to investigate before flashing.
# -----------------------------------------------------------------------------
STAGED_BIN="${TARGET_DIR}/usr/local/bin/dcentrald"
if [ ! -f "$STAGED_BIN" ]; then
    echo "DCENTos post-build (am3-s19kpro): ERROR: dcentrald missing from $STAGED_BIN" >&2
    exit 1
fi
. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/dcentrald_version_gate.sh"
dcent_require_dcentrald_version_match \
    "$TARGET_DIR" \
    "$STAGED_BIN" \
    "DCENTos post-build (am3-s19kpro)" \
    "${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/Cargo.toml"

# mtime portability: GNU coreutils (Buildroot host) supports `stat -c %Y`.
# Fall back to BSD `stat -f %m` for macOS dev boxes.
BIN_MTIME=$(stat -c %Y "$STAGED_BIN" 2>/dev/null || stat -f %m "$STAGED_BIN" 2>/dev/null || echo 0)
NOW_EPOCH=$(date +%s)
if [ "$BIN_MTIME" -gt 0 ] && [ "$NOW_EPOCH" -gt "$BIN_MTIME" ]; then
    BIN_AGE_HOURS=$(( (NOW_EPOCH - BIN_MTIME) / 3600 ))
    if [ "$BIN_AGE_HOURS" -gt 12 ]; then
        echo "DCENTos post-build (am3-s19kpro): WARN: dcentrald binary is ${BIN_AGE_HOURS}h old." >&2
        echo "  If this build should include recent Rust changes, re-run cargo build first." >&2
    fi
fi
ls -l "$STAGED_BIN"

# Stamp MD5 of the staged binary so the runtime + operator tools can confirm
# which build actually shipped. Read via /etc/dcentos/dcentrald.md5.
mkdir -p "${TARGET_DIR}/etc/dcentos"
if command -v md5sum > /dev/null 2>&1; then
    md5sum "$STAGED_BIN" | cut -d' ' -f1 > "${TARGET_DIR}/etc/dcentos/dcentrald.md5"
    echo "DCENTos post-build (am3-s19kpro): md5 $(cat "${TARGET_DIR}/etc/dcentos/dcentrald.md5")"
fi

# -----------------------------------------------------------------------------
# Default dcentrald.toml for am3-s19kpro.
#
# Unlike am2 (which has a separate dcentrald_s19jpro_am2.toml in the dcentrald/
# directory), am3-s19kpro ships its baseline directly via the per-board
# rootfs-overlay (rootfs-overlay/etc/dcentrald.toml). The shared amlogic
# overlay does NOT define /etc/dcentrald.toml, so the per-board overlay file
# is already the authoritative copy. We only verify it landed in the image.
# -----------------------------------------------------------------------------
if [ ! -f "${TARGET_DIR}/etc/dcentrald.toml" ]; then
    echo "DCENTos post-build (am3-s19kpro): ERROR: /etc/dcentrald.toml missing from rootfs." >&2
    echo "  Expected to come from board/amlogic/am3-s19kpro/rootfs-overlay/etc/dcentrald.toml" >&2
    exit 1
fi

# -----------------------------------------------------------------------------
# Board identity (the per-board overlay already ships /etc/dcentos-platform =
# "am3-aml-s19k"; the files below mirror the am2 pattern for any consumer
# that reads board_family / board_target / platform separately).
# -----------------------------------------------------------------------------
# F-9 (Sweep-v3 PR-086): bare "am3" resolves PLATFORM=unknown in
# S99verify detect_platform() (no `am3` arm; board_target fallback
# skipped because board_family was set). Stamp the SKU-qualified
# `am3-aml-*` form (matches the `am3-aml*` classifier; same value as
# the platform file). The sweep's F-9 named only am3-s19jpro-aml +
# am3-t21; am3-s19kpro + am3-s21 had the identical latent bug.
echo "am3-aml-s19k"   > "${TARGET_DIR}/etc/dcentos/board_family"
echo "am3-s19k"       > "${TARGET_DIR}/etc/dcentos/board_target"
echo "am3-aml-s19k"   > "${TARGET_DIR}/etc/dcentos/platform"

# The AM3 revert helpers source lib/am3_geometry.sh beside /usr/sbin.
AM3_GEOMETRY_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/am3_geometry.sh"
if [ -f "$AM3_GEOMETRY_SRC" ]; then
    mkdir -p "${TARGET_DIR}/usr/sbin/lib"
    cp "$AM3_GEOMETRY_SRC" "${TARGET_DIR}/usr/sbin/lib/am3_geometry.sh"
    chmod 644 "${TARGET_DIR}/usr/sbin/lib/am3_geometry.sh" 2>/dev/null || true
    echo "DCENTos post-build (am3-s19kpro): installed am3_geometry.sh for revert helpers"
else
    echo "DCENTos post-build (am3-s19kpro): WARNING: am3_geometry.sh not found at $AM3_GEOMETRY_SRC" >&2
fi

#  W12-B: install the per-platform revert script keyed by
# PROFILE_TABLE.amlogic-a113d-bm1366.revert_script (W23 rename). Code-complete but
# `verified_revertable: false` until W21 live test on the office S19k.
REVERT_S19K_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/revert_to_stock_am3_aml_s19k.sh"
if [ -f "$REVERT_S19K_SRC" ]; then
    cp "$REVERT_S19K_SRC" "${TARGET_DIR}/usr/sbin/revert_to_stock_am3_aml_s19k.sh"
    chmod +x "${TARGET_DIR}/usr/sbin/revert_to_stock_am3_aml_s19k.sh" 2>/dev/null || true
    echo "DCENTos post-build (am3-s19kpro): installed revert_to_stock_am3_aml_s19k.sh from scripts/"
else
    echo "DCENTos post-build (am3-s19kpro): WARNING: revert_to_stock_am3_aml_s19k.sh not found at $REVERT_S19K_SRC" >&2
fi

#  W12-B: also ship the stock-Bitmain manifest (parity with
# zynq board post-build). The daemon probes /etc/dcentos/ first.
MANIFEST_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../../../knowledge-base/firmware-archive/stock-bitmain-manifest.json"
if [ -f "$MANIFEST_SRC" ]; then
    cp "$MANIFEST_SRC" "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json"
    chmod 644 "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json" 2>/dev/null || true
    echo "DCENTos post-build (am3-s19kpro): installed stock-bitmain-manifest.json"
fi

# Embed the trusted release public key when provided (same pattern as S9/am2).
if [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
    if [ ! -f "${DCENT_RELEASE_PUBKEY_FILE}" ]; then
        echo "DCENTos post-build (am3-s19kpro): ERROR: release public key not found at ${DCENT_RELEASE_PUBKEY_FILE}" >&2
        exit 1
    fi
    cp "${DCENT_RELEASE_PUBKEY_FILE}" "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    chmod 644 "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    echo "DCENTos post-build (am3-s19kpro): embedded release public key"
elif [ "${DCENT_REQUIRE_RELEASE_KEY:-0}" = "1" ]; then
    echo "DCENTos post-build (am3-s19kpro): ERROR: DCENT_REQUIRE_RELEASE_KEY=1 but DCENT_RELEASE_PUBKEY_FILE is not set" >&2
    exit 1
else
    echo "DCENTos post-build (am3-s19kpro): WARNING: no release public key embedded (lab-only image)"
fi

echo "DCENTos post-build (am3-s19kpro): directories, permissions, and board identity set."

# Production-readiness matrix §7 #1 (public-image trust boundary): release-image
# marker + first-boot SSH posture when DCENT_RELEASE_IMAGE=1. NO-OP on DEV/LAB.
. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/release_image_provision.sh"
dcent_provision_release_image "$TARGET_DIR" "am3-s19kpro"
