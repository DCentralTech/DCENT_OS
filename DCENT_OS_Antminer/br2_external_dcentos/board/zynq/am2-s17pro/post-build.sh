#!/bin/sh
#
# DCENTos post-build script — am2-s17pro (S17 / S17 Pro Zynq am2-s17 variant)
# D-Central Technologies, 2026
#
#  Phase 2E (2026-05-15) — clone of
# board/zynq/am2-s19jpro/post-build.sh adjusted for the BM1397 / S17 Pro
# hashboard family. Same Zynq armv7 Cortex-A9 toolchain, same rootfs-overlay
# conventions. Only the board identity, baked dcentrald.toml, and revert
# helper differ.
#
# ## RUNTIME-ONLY — NO COLD-BOOT PROOF ########################################
# There is NO live Antminer S17 / S17 Pro on the D-Central fleet. This script
# stamps a BEST-GUESS scaffold identity + config; it does NOT imply this image
# has ever cold-booted or mined. Do NOT claim cold-boot proof. See the board
# README.md and dcentos_am2_s17pro_zynq_defconfig header.
#
# ## UNCONFIRMED (v2 open question → R11) #####################################
# S17 / S17 Pro / T17 route through BM1397. S17+ / T17+ route through BM1396,
# which remains intentionally unregistered until live enumerate proof exists.
# Treat the distinction as an open RE question feeding the R11 handoff.
#############################################################################
#
# Overlay-on-overlay pattern (see defconfig): Buildroot applies the shared
# board/zynq/rootfs-overlay FIRST, then this board's rootfs-overlay on top.
#
# CRITICAL — :
#   The overlay MUST NOT ship a stale dcentrald binary. This script installs
#   the fresh cross-compiled binary AFTER Buildroot copies overlays.
#
# CRITICAL — :
#   board_target MUST be "am2-s17p" here. Sysupgrade tarball prefix is
#   "sysupgrade-am2-s17p/". Wrong name = silent failure = brick on flash.
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

# Make init scripts executable (from either overlay layer)
chmod +x "${TARGET_DIR}"/etc/init.d/* 2>/dev/null || true
# W1.1 default-credential lockdown: SSH gate helper must be mode 0755.
chmod 0755 "${TARGET_DIR}"/usr/sbin/dcent-enable-ssh 2>/dev/null || true

# Make early-init and persistent storage scripts executable
chmod +x "${TARGET_DIR}"/etc/dcentos-early-init.sh 2>/dev/null || true

# Make tools executable
chmod +x "${TARGET_DIR}"/root/tools/*.py 2>/dev/null || true
chmod +x "${TARGET_DIR}"/root/tools/*.sh 2>/dev/null || true
chmod +x "${TARGET_DIR}"/usr/bin/dcent-shell 2>/dev/null || true
chmod +x "${TARGET_DIR}"/usr/sbin/sysupgrade 2>/dev/null || true
chmod +x "${TARGET_DIR}"/usr/sbin/switch_firmware.py 2>/dev/null || true

# Restore-to-stock: this am2-s17pro defconfig runs its own post-build script,
# so install the PROFILE_TABLE.zynq-am2-bm1397 revert helper here. The
# revert_to_stock_s17.sh script is the am2-s17 (BM1397+) revert path; the
# dcentrald-api PROFILE_TABLE `zynq-am2-bm1397` entry points at
# /usr/sbin/revert_to_stock_s17.sh on the running miner.
REVERT_S17_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/revert_to_stock_s17.sh"
if [ -f "$REVERT_S17_SRC" ]; then
    cp "$REVERT_S17_SRC" "${TARGET_DIR}/usr/sbin/revert_to_stock_s17.sh"
    chmod +x "${TARGET_DIR}/usr/sbin/revert_to_stock_s17.sh" 2>/dev/null || true
    echo "DCENTos post-build (am2-s17pro): installed revert_to_stock_s17.sh from scripts/"
else
    echo "DCENTos post-build (am2-s17pro): WARNING: revert_to_stock_s17.sh not found at $REVERT_S17_SRC" >&2
fi

# Restore-to-stock manifest parity with the shared zynq/amlogic images.
MANIFEST_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../../../extractions/firmware-archive/stock-bitmain-manifest.json"
if [ -f "$MANIFEST_SRC" ]; then
    cp "$MANIFEST_SRC" "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json"
    chmod 644 "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json" 2>/dev/null || true
    echo "DCENTos post-build (am2-s17pro): installed stock-bitmain-manifest.json"
fi

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
    echo "DCENTos post-build (am2-s17pro): installed dashboard SPA ($DASHBOARD_SIZE bytes) at /usr/share/dcentos-dashboard/index.html"
    if [ "$DASHBOARD_SIZE" -lt 100000 ]; then
        echo "DCENTos post-build (am2-s17pro): ERROR: dashboard appears truncated ($DASHBOARD_SIZE bytes < 100 KB floor)" >&2
        echo "  Run: cd DCENT_OS_Antminer/dashboard && npm run build" >&2
        exit 1
    fi
else
    echo "DCENTos post-build (am2-s17pro): ERROR: dashboard not found at $DASHBOARD_SRC" >&2
    echo "  Run: cd DCENT_OS_Antminer/dashboard && npm run build" >&2
    exit 1
fi

# Install dcentos-init as /sbin/init if present
DCENTOS_INIT="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentos-init"
if [ -f "$DCENTOS_INIT" ]; then
    rm -f "${TARGET_DIR}/sbin/init" 2>/dev/null || true
    cp "$DCENTOS_INIT" "${TARGET_DIR}/sbin/init"
    chmod 755 "${TARGET_DIR}/sbin/init"
    echo "DCENTos post-build (am2-s17pro): installed dcentos-init as /sbin/init"
else
    echo "DCENTos post-build (am2-s17pro): WARNING: dcentos-init not found at $DCENTOS_INIT"
    echo "  BusyBox init will be used (requires CONFIG_INIT=y in busybox.config)"
fi

# -----------------------------------------------------------------------------
# Install dcentrald (fresh cross-compiled binary beats any stale overlay).
# Same toolchain as S9 / am2-s19jpro — am2 S17 Pro is Zynq armv7 (NOT aarch64).
# -----------------------------------------------------------------------------
DCENTRALD_BIN="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentrald"
if [ -f "$DCENTRALD_BIN" ]; then
    STAGED_BIN="${TARGET_DIR}/usr/local/bin/dcentrald"
    cp "$DCENTRALD_BIN" "$STAGED_BIN"
    chmod 755 "$STAGED_BIN"
    DCENTRALD_SIZE=$(stat -c%s "$DCENTRALD_BIN" 2>/dev/null || stat -f%z "$DCENTRALD_BIN")
    echo "DCENTos post-build (am2-s17pro): installed dcentrald ($DCENTRALD_SIZE bytes)"
else
    echo "DCENTos post-build (am2-s17pro): ERROR: dcentrald not found at $DCENTRALD_BIN" >&2
    echo "  Refusing to ship a stale overlay daemon. Build dcentrald first." >&2
    exit 1
fi

# -----------------------------------------------------------------------------
# Freshness audit.
# Observational only — does NOT fail the build.
# -----------------------------------------------------------------------------
STAGED_BIN="${TARGET_DIR}/usr/local/bin/dcentrald"
if [ ! -f "$STAGED_BIN" ]; then
    echo "DCENTos post-build (am2-s17pro): ERROR: dcentrald missing from $STAGED_BIN" >&2
    exit 1
fi
. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/dcentrald_version_gate.sh"
dcent_require_dcentrald_version_match \
    "$TARGET_DIR" \
    "$STAGED_BIN" \
    "DCENTos post-build (am2-s17pro)" \
    "${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/Cargo.toml"

# mtime portability: GNU coreutils (Buildroot host) supports `stat -c %Y`.
# Fall back to BSD `stat -f %m` for macOS dev boxes.
BIN_MTIME=$(stat -c %Y "$STAGED_BIN" 2>/dev/null || stat -f %m "$STAGED_BIN" 2>/dev/null || echo 0)
NOW_EPOCH=$(date +%s)
if [ "$BIN_MTIME" -gt 0 ] && [ "$NOW_EPOCH" -gt "$BIN_MTIME" ]; then
    BIN_AGE_HOURS=$(( (NOW_EPOCH - BIN_MTIME) / 3600 ))
    if [ "$BIN_AGE_HOURS" -gt 12 ]; then
        echo "DCENTos post-build (am2-s17pro): WARN: dcentrald binary is ${BIN_AGE_HOURS}h old." >&2
        echo "  If this build should include recent Rust changes, re-run cargo build first." >&2
    fi
fi
ls -l "$STAGED_BIN"

# Stamp MD5 of the staged binary so the runtime + operator tools can confirm
# which build actually shipped. Read via /etc/dcentos/dcentrald.md5.
mkdir -p "${TARGET_DIR}/etc/dcentos"
if command -v md5sum > /dev/null 2>&1; then
    md5sum "$STAGED_BIN" | cut -d' ' -f1 > "${TARGET_DIR}/etc/dcentos/dcentrald.md5"
    echo "DCENTos post-build (am2-s17pro): md5 $(cat "${TARGET_DIR}/etc/dcentos/dcentrald.md5")"
fi

# -----------------------------------------------------------------------------
# Default dcentrald.toml for am2-s17pro.
#
#  Phase 2E: the baked default is the S17 Pro scaffold
# (`configs/dcentrald_s17pro_am2_baked_default.toml`) — cloned from the
# am2-s19jpro baked default, re-pointed at BM1397 / S17 Pro, with
# mining.enabled=false (RUNTIME-ONLY: no live S17 unit, every chip value is a
# best-guess scaffold that must be confirmed on a bench unit). Same proven am2
# safety knobs as the BM1362 baked default (fan_max_pwm=30 /
# dangerous_temp_c=80 / hash_on_disconnect=false / am2_no_nonce_timeout_s=90).
#
# Always stamp this over the shared Zynq overlay default — the shared overlay
# is S9-oriented and would silently corrupt am2 if it won.
#
# voltage_mv is hard-clamped to <= 14_500 mV on am2 platforms by
# DcentraldConfig::validate() (Phase 4C, commit 747947e4).
# -----------------------------------------------------------------------------
S17_AM2_BAKED_CFG="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/configs/dcentrald_s17pro_am2_baked_default.toml"
if [ -f "$S17_AM2_BAKED_CFG" ]; then
    cp "$S17_AM2_BAKED_CFG" "${TARGET_DIR}/etc/dcentrald.toml"
    echo "DCENTos post-build (am2-s17pro): installed dcentrald_s17pro_am2_baked_default.toml (S17 Pro RUNTIME-ONLY scaffold) as /etc/dcentrald.toml"
else
    echo "DCENTos post-build (am2-s17pro): ERROR: S17 Pro baked default not found at:" >&2
    echo "  $S17_AM2_BAKED_CFG" >&2
    exit 1
fi

# Phase 4B-style safety re-verification — refuse to ship a baked
# /etc/dcentrald.toml that violates the am2 EE invariants. Same checks the
# am2-s19jpro post-build applies (same am2 control board + dsPIC + APW path).
BAKED_RUNTIME_CFG="${TARGET_DIR}/etc/dcentrald.toml"
if [ -f "$BAKED_RUNTIME_CFG" ]; then
    # voltage_mv MUST NOT exceed 14_500 (am2 chip-rail ceiling).
    BAKED_VOLTAGE_MV=$(awk '
        /^\[/ { in_mining = ($0 == "[mining]") ? 1 : 0; next }
        in_mining && /^[[:space:]]*voltage_mv[[:space:]]*=/ {
            sub(/^[^=]*=[[:space:]]*/, "", $0); sub(/[[:space:]]*#.*$/, "", $0)
            print $0; exit
        }
    ' "$BAKED_RUNTIME_CFG")
    if [ -n "$BAKED_VOLTAGE_MV" ] && [ "$BAKED_VOLTAGE_MV" -gt 14500 ] 2>/dev/null; then
        echo "DCENTos post-build (am2-s17pro): ERROR: baked dcentrald.toml has mining.voltage_mv=${BAKED_VOLTAGE_MV} > 14500 mV am2 ceiling" >&2
        echo "  am2 BHB hashboards via APW-class/dsPIC are not specified above 14.5 V chip-rail." >&2
        echo "  See feedback_eeprom_addresses_protected.md + .74 hb2 corruption incident 2026-04-29." >&2
        exit 1
    fi

    # fan_max_pwm MUST NOT exceed 30 (home-quiet ceiling).
    BAKED_FAN_MAX=$(awk '
        /^\[/ { in_thermal = ($0 == "[thermal]") ? 1 : 0; next }
        in_thermal && /^[[:space:]]*fan_max_pwm[[:space:]]*=/ {
            sub(/^[^=]*=[[:space:]]*/, "", $0); sub(/[[:space:]]*#.*$/, "", $0)
            print $0; exit
        }
    ' "$BAKED_RUNTIME_CFG")
    if [ -n "$BAKED_FAN_MAX" ] && [ "$BAKED_FAN_MAX" -gt 30 ] 2>/dev/null; then
        echo "DCENTos post-build (am2-s17pro): ERROR: baked dcentrald.toml has thermal.fan_max_pwm=${BAKED_FAN_MAX} > 30" >&2
        echo "  Home/baked-default fan ceiling is 30 (feedback_fan_max_30pwm.md / feedback_fan_never_blast.md)." >&2
        echo "  Operator may opt into a higher cap via /data/dcentrald.toml or a hacker-mode toggle." >&2
        exit 1
    fi

    echo "DCENTos post-build (am2-s17pro): baked default voltage_mv=${BAKED_VOLTAGE_MV:-unset} fan_max_pwm=${BAKED_FAN_MAX:-unset} (within am2 EE safety envelope)"
fi

# -----------------------------------------------------------------------------
# Board identity (overrides shared overlay's am1-s9 values).
# board_target MUST be "am2-s17p" so sysupgrade tarball prefix matches.
# platform string is the shared am2 Zynq marker (zynq-bm3-am2) so the shared
# S82dcentrald init script's am2 detection + --s19j-hybrid + milestone env
# vars apply (same am2 hybrid mining loop; the chip-driver layer dispatches
# BM1397 vs BM1362 at runtime via the MinerProfile / serial_chip_type).
# -----------------------------------------------------------------------------
echo "am2"            > "${TARGET_DIR}/etc/dcentos/board_family"
echo "am2-s17p"       > "${TARGET_DIR}/etc/dcentos/board_target"
echo "zynq-bm3-am2"   > "${TARGET_DIR}/etc/dcentos/platform"

# Embed the trusted release public key when provided (same pattern as
# am2-s19jpro / S9).
if [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
    if [ ! -f "${DCENT_RELEASE_PUBKEY_FILE}" ]; then
        echo "DCENTos post-build (am2-s17pro): ERROR: release public key not found at ${DCENT_RELEASE_PUBKEY_FILE}" >&2
        exit 1
    fi
    cp "${DCENT_RELEASE_PUBKEY_FILE}" "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    chmod 644 "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    echo "DCENTos post-build (am2-s17pro): embedded release public key"
elif [ "${DCENT_REQUIRE_RELEASE_KEY:-0}" = "1" ]; then
    echo "DCENTos post-build (am2-s17pro): ERROR: DCENT_REQUIRE_RELEASE_KEY=1 but DCENT_RELEASE_PUBKEY_FILE is not set" >&2
    exit 1
else
    echo "DCENTos post-build (am2-s17pro): WARNING: no release public key embedded (lab-only image)"
fi

echo "DCENTos post-build (am2-s17pro): directories, permissions, and board identity set (RUNTIME-ONLY scaffold — no live S17/S17 Pro on the fleet)."

# Production-readiness matrix §7 #1 (public-image trust boundary): release-image
# marker + first-boot SSH posture when DCENT_RELEASE_IMAGE=1. NO-OP on DEV/LAB.
. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/release_image_provision.sh"
dcent_provision_release_image "$TARGET_DIR" "am2-s17pro"
