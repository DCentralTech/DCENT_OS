#!/bin/sh
#
# DCENTos post-build script — am2-s19jpro (S19j Pro AM2 Zynq lane)
# D-Central Technologies, 2026
#
# UNTESTED 2026-04-20 Phase 2 initial scaffold — first build will be in Phase 3.
#
# This is the sibling of board/zynq/post-build.sh (S9 / am1-s9). Same Zynq
# armv7 toolchain, same rootfs-overlay conventions. Only the board identity
# and default dcentrald.toml differ.
#
# Overlay-on-overlay pattern (see defconfig): Buildroot applies the shared
# board/zynq/rootfs-overlay FIRST, then this board's rootfs-overlay on top.
# So we only need to diff what's am2-specific (init script tweaks, target
# identity, default config) and let everything else fall through to shared.
#
# CRITICAL — :
#   The overlay MUST NOT ship a stale dcentrald binary. This script installs
#   the fresh cross-compiled binary AFTER Buildroot copies overlays, so the
#   compiled daemon always wins over anything checked in. Refuses to build
#   if the binary is missing.
#
# CRITICAL — :
#   board_target MUST be "am2-s19j" here. Sysupgrade tarball prefix is
#   "sysupgrade-am2-s19j/". Wrong name = silent failure = brick on flash.
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

#  restore-to-stock closure: this am2 defconfig runs its own
# post-build script, not board/zynq/post-build.sh, so install the
# PROFILE_TABLE.zynq-am2-bm1398 revert helper here as well.
REVERT_S19_AM2_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/revert_to_stock_s19_am2.sh"
if [ -f "$REVERT_S19_AM2_SRC" ]; then
    cp "$REVERT_S19_AM2_SRC" "${TARGET_DIR}/usr/sbin/revert_to_stock_s19_am2.sh"
    chmod +x "${TARGET_DIR}/usr/sbin/revert_to_stock_s19_am2.sh" 2>/dev/null || true
    echo "DCENTos post-build (am2-s19jpro): installed revert_to_stock_s19_am2.sh from scripts/"
else
    echo "DCENTos post-build (am2-s19jpro): WARNING: revert_to_stock_s19_am2.sh not found at $REVERT_S19_AM2_SRC" >&2
fi

# Restore-to-stock manifest parity with the shared zynq/amlogic images.
MANIFEST_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../../../extractions/firmware-archive/stock-bitmain-manifest.json"
if [ -f "$MANIFEST_SRC" ]; then
    cp "$MANIFEST_SRC" "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json"
    chmod 644 "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json" 2>/dev/null || true
    echo "DCENTos post-build (am2-s19jpro): installed stock-bitmain-manifest.json"
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
    echo "DCENTos post-build (am2-s19jpro): installed dashboard SPA ($DASHBOARD_SIZE bytes) at /usr/share/dcentos-dashboard/index.html"
    if [ "$DASHBOARD_SIZE" -lt 100000 ]; then
        echo "DCENTos post-build (am2-s19jpro): ERROR: dashboard appears truncated ($DASHBOARD_SIZE bytes < 100 KB floor)" >&2
        echo "  Run: cd DCENT_OS_Antminer/dashboard && npm run build" >&2
        exit 1
    fi
else
    echo "DCENTos post-build (am2-s19jpro): ERROR: dashboard not found at $DASHBOARD_SRC" >&2
    echo "  Run: cd DCENT_OS_Antminer/dashboard && npm run build" >&2
    exit 1
fi

# Install dcentos-init as /sbin/init if present
DCENTOS_INIT="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentos-init"
if [ -f "$DCENTOS_INIT" ]; then
    rm -f "${TARGET_DIR}/sbin/init" 2>/dev/null || true
    cp "$DCENTOS_INIT" "${TARGET_DIR}/sbin/init"
    chmod 755 "${TARGET_DIR}/sbin/init"
    echo "DCENTos post-build (am2-s19jpro): installed dcentos-init as /sbin/init"
    mkdir -p "${TARGET_DIR}/etc/dcentos"
    sha256sum "$DCENTOS_INIT" | cut -d' ' -f1 > "${TARGET_DIR}/etc/dcentos/dcentos-init.sha256"
    echo "DCENTos post-build (am2-s19jpro): dcentos-init sha256 $(cat "${TARGET_DIR}/etc/dcentos/dcentos-init.sha256")"
else
    echo "DCENTos post-build (am2-s19jpro): ERROR: dcentos-init not found at $DCENTOS_INIT" >&2
    echo "  Refusing to ship a release image with an unstaged PID 1. Build dcentos-init first." >&2
    exit 1
fi

# -----------------------------------------------------------------------------
# Install dcentrald (fresh cross-compiled binary beats any stale overlay).
# Same toolchain as S9 — am2 S19j Pro is Zynq armv7 (NOT aarch64 — that's the
# Amlogic S19j Pro variant handled elsewhere).
# -----------------------------------------------------------------------------
DCENTRALD_BIN="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentrald"
if [ -f "$DCENTRALD_BIN" ]; then
    STAGED_BIN="${TARGET_DIR}/usr/local/bin/dcentrald"
    cp "$DCENTRALD_BIN" "$STAGED_BIN"
    chmod 755 "$STAGED_BIN"
    DCENTRALD_SIZE=$(stat -c%s "$DCENTRALD_BIN" 2>/dev/null || stat -f%z "$DCENTRALD_BIN")
    echo "DCENTos post-build (am2-s19jpro): installed dcentrald ($DCENTRALD_SIZE bytes)"
else
    echo "DCENTos post-build (am2-s19jpro): ERROR: dcentrald not found at $DCENTRALD_BIN" >&2
    echo "  Refusing to ship a stale overlay daemon. Build dcentrald first." >&2
    exit 1
fi

# -----------------------------------------------------------------------------
# XIL .25 NAND-BAKE (operator-authorized 2026-06-14): stage the standalone
# boot-and-mine launcher + a MAC-gated seed dir so the boot-and-mine SURVIVES a
# /data wipe. The S81dcentos-xil-seed init re-seeds /data from here on the .25
# unit ONLY (MAC aa:bb:cc:dd:ee:ff). The DEFAULT /etc/dcentrald.toml stays
# management-only (the assert_first_boot_idle gate below still applies to it and
# to every non-.25 unit) — the .25 mining config lives ONLY in the seed dir and
# fires only on the .25 MAC, so F5 (fresh unit = management-only) holds fleet-wide.
# SHA auto-pin keeps the boot launcher matched to the dcentrald just built.
#
# -----------------------------------------------------------------------------
XIL25_LAUNCHER_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/dcentrald_standalone_boot_25.sh"
XIL25_SEED_CFG="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/configs/dcentrald_s19jpro_xil25_seed.toml"
XIL25_SEED_MGMT="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/configs/dcentrald_s19jpro_xil25_seed_mgmt.toml"
XIL25_SEED_ENV="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/configs/dcentrald_s19jpro_xil25_seed-env"
if [ -f "$XIL25_LAUNCHER_SRC" ] && [ -f "$XIL25_SEED_CFG" ] && [ -f "$XIL25_SEED_MGMT" ] && [ -f "$XIL25_SEED_ENV" ]; then
    XIL25_BAKED_SHA=$(sha256sum "$STAGED_BIN" | awk '{print tolower($1)}')
    XIL25_SEED_DIR="${TARGET_DIR}/etc/dcentos/xil25-seed"
    mkdir -p "$XIL25_SEED_DIR"
    # Boot launcher: install to /usr/local/bin AND the seed dir, both SHA-auto-pinned
    # to the freshly-built dcentrald (the seeder symlinks /data/dcentrald -> it).
    XIL25_BAKED_LAUNCHER="${TARGET_DIR}/usr/local/bin/dcentrald_standalone_boot.sh"
    sed "s|^EXPECTED_DCENTRALD_SHA256=.*|EXPECTED_DCENTRALD_SHA256=\"${XIL25_BAKED_SHA}\"|" \
        "$XIL25_LAUNCHER_SRC" > "$XIL25_BAKED_LAUNCHER"
    chmod 755 "$XIL25_BAKED_LAUNCHER"
    cp "$XIL25_BAKED_LAUNCHER" "${XIL25_SEED_DIR}/dcentrald_standalone_boot.sh"
    cp "$XIL25_SEED_CFG"  "${XIL25_SEED_DIR}/dcentrald.toml"
    cp "$XIL25_SEED_MGMT" "${XIL25_SEED_DIR}/dcentrald.toml.mgmt-bak"
    cp "$XIL25_SEED_ENV"  "${XIL25_SEED_DIR}/dcentrald-env"
    echo "DCENTos post-build (am2-s19jpro): XIL .25 NAND-bake staged (seed dir + auto-pinned launcher SHA=${XIL25_BAKED_SHA})"
else
    echo "DCENTos post-build (am2-s19jpro): XIL .25 seed sources absent — bake skipped (image ships management-only default; S81 seeder no-ops)" >&2
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
    echo "DCENTos post-build (am2-s19jpro): ERROR: dcentrald missing from $STAGED_BIN" >&2
    exit 1
fi
. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/dcentrald_version_gate.sh"
dcent_require_dcentrald_version_match \
    "$TARGET_DIR" \
    "$STAGED_BIN" \
    "DCENTos post-build (am2-s19jpro)" \
    "${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/Cargo.toml"

# mtime portability: GNU coreutils (Buildroot host) supports `stat -c %Y`.
# Fall back to BSD `stat -f %m` for macOS dev boxes.
BIN_MTIME=$(stat -c %Y "$STAGED_BIN" 2>/dev/null || stat -f %m "$STAGED_BIN" 2>/dev/null || echo 0)
NOW_EPOCH=$(date +%s)
if [ "$BIN_MTIME" -gt 0 ] && [ "$NOW_EPOCH" -gt "$BIN_MTIME" ]; then
    BIN_AGE_HOURS=$(( (NOW_EPOCH - BIN_MTIME) / 3600 ))
    if [ "$BIN_AGE_HOURS" -gt 12 ]; then
        echo "DCENTos post-build (am2-s19jpro): WARN: dcentrald binary is ${BIN_AGE_HOURS}h old." >&2
        echo "  If this build should include recent Rust changes, re-run cargo build first." >&2
    fi
fi
ls -l "$STAGED_BIN"

# Stamp hashes of the staged binary so the runtime + operator tools can confirm
# which build actually shipped. Read via /etc/dcentos/dcentrald.{md5,sha256}.
mkdir -p "${TARGET_DIR}/etc/dcentos"
# D7-3 (2026-06-13): the runtime + operator tools read these stamps to confirm
# WHICH build shipped before a decisive flash/run, so a silently-skipped or stale
# stamp is a correctness hazard. rm -f any prior stamp first (a partial write can
# never leave a stale hash), then HARD-FAIL if the hashers are missing on a
# production build (mirrors dcent_require_dcentrald_version_match) instead of
# silently shipping an unstamped image.
rm -f "${TARGET_DIR}/etc/dcentos/dcentrald.md5" "${TARGET_DIR}/etc/dcentos/dcentrald.sha256"
if ! command -v md5sum > /dev/null 2>&1 || ! command -v sha256sum > /dev/null 2>&1; then
    echo "DCENTos post-build (am2-s19jpro): ERROR: md5sum/sha256sum unavailable on the build host; cannot stamp dcentrald identity" >&2
    echo "  Install coreutils on the Buildroot host. The runtime/operator rely on /etc/dcentos/dcentrald.{md5,sha256} to confirm the shipped build." >&2
    exit 1
fi
md5sum "$STAGED_BIN" | cut -d' ' -f1 > "${TARGET_DIR}/etc/dcentos/dcentrald.md5"
echo "DCENTos post-build (am2-s19jpro): md5 $(cat "${TARGET_DIR}/etc/dcentos/dcentrald.md5")"
sha256sum "$STAGED_BIN" | cut -d' ' -f1 > "${TARGET_DIR}/etc/dcentos/dcentrald.sha256"
echo "DCENTos post-build (am2-s19jpro): sha256 $(cat "${TARGET_DIR}/etc/dcentos/dcentrald.sha256")"

# -----------------------------------------------------------------------------
# Default dcentrald.toml for am2-s19jpro.
#
# Phase 4B (EE Finding 5 #6 + Finding 1, 2026-05-15): the baked default is
# the milestone-derived "am2 baked default" (`configs/dcentrald_s19jpro_am2_
# baked_default.toml`) — same proven safety knobs as the XIL config that
# produced the 2026-05-15 .109 first-accepted-shares run (voltage_mv=13700
# / fan_max_pwm=30 / dangerous_temp_c=80 / hash_on_disconnect=false /
# am2_no_nonce_timeout_s=90 / frequency_mhz=525 / am2_pll_ramp=false) but
# sanitized for first-boot of an unconfigured unit:
#
#   * pool.url / pool.worker = "" (operator configures via dashboard)
#   * mining.enabled = false (idle-first until pool configured)
#   * watchdog.enabled = true (HW watchdog ON by default — workspace safety rule)
#   * donation.enabled = true (transparent 2% production policy)
#   * api.cgminer_port = 4028 (clean image owns the port)
#
# Always stamp this over the shared Zynq overlay default — the shared overlay
# is S9-oriented (9.1 V chip rail, no APW12, different PIC family) and would
# silently corrupt am2 if it won.
#
# voltage_mv is hard-clamped to <= 14_500 mV on am2 platforms by
# `DcentraldConfig::validate()` (Phase 4C, commit 747947e4). Operator-edited
# /data/dcentrald.toml is also validated against this clamp at load time.
# -----------------------------------------------------------------------------
S19J_AM2_BAKED_CFG="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/configs/dcentrald_s19jpro_am2_baked_default.toml"
S19J_AM2_LEGACY_CFG="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/dcentrald_s19jpro_am2.toml"
if [ -f "$S19J_AM2_BAKED_CFG" ]; then
    cp "$S19J_AM2_BAKED_CFG" "${TARGET_DIR}/etc/dcentrald.toml"
    echo "DCENTos post-build (am2-s19jpro): installed dcentrald_s19jpro_am2_baked_default.toml (Phase 4B milestone-safety knobs) as /etc/dcentrald.toml"
elif [ -f "$S19J_AM2_LEGACY_CFG" ]; then
    # Pre-Phase-4B legacy fallback. WARN loudly so we notice if the baked
    # default config goes missing from the source tree.
    cp "$S19J_AM2_LEGACY_CFG" "${TARGET_DIR}/etc/dcentrald.toml"
    echo "DCENTos post-build (am2-s19jpro): WARNING: baked default missing at $S19J_AM2_BAKED_CFG; falling back to legacy dcentrald_s19jpro_am2.toml" >&2
    echo "  This carries the .139-specific pool/worker — operator MUST clear pool.url/pool.worker before persistent flash." >&2
else
    echo "DCENTos post-build (am2-s19jpro): ERROR: neither baked default nor legacy am2 config found at:" >&2
    echo "  $S19J_AM2_BAKED_CFG" >&2
    echo "  $S19J_AM2_LEGACY_CFG" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# W0.1 / D7-1 (2026-06-13): build-time FIRST-BOOT AUTO-MINE guard.
# The voltage/fan/psu re-verification blocks below catch those regressions, but
# the single most important first-boot invariant — that a baked image is
# management-only (no auto-mine, no baked pool) — was left unguarded. The
# runtime F5 gate only parks the unit when [mining] enabled=false, so an
# enabled=true (or baked pool.url) regression would auto-mine on first boot with
# NO build- OR run-time catch. That is exactly the "stale gate-less daemon
# auto-mining + fan-blast" class the SD-boot pass caught. Fail the BUILD instead.
# ---------------------------------------------------------------------------
extract_toml_value() { # $1=file  $2=section (no brackets)  $3=key
    awk -v sect="[$2]" -v key="$3" '
        /^[[:space:]]*\[/ {
            hdr = $0; gsub(/[[:space:]]/, "", hdr)
            in_sect = (hdr == sect) ? 1 : 0
            next
        }
        in_sect && $0 ~ ("^[[:space:]]*" key "[[:space:]]*=") {
            sub(/^[^=]*=[[:space:]]*/, "", $0)
            sub(/[[:space:]]*#.*$/, "", $0)
            gsub(/[[:space:]]/, "", $0)
            print $0
            exit
        }
    ' "$1"
}

assert_first_boot_idle() { # $1=file  $2=human-label
    _f="$1"; _label="$2"
    [ -f "$_f" ] || return 0
    _en=$(extract_toml_value "$_f" mining enabled)
    if [ "$_en" != "false" ]; then
        echo "DCENTos post-build (am2-s19jpro): ERROR: ${_label} [mining] enabled='${_en:-unset}', expected 'false'" >&2
        echo "  A baked image MUST be management-only at first boot (idle until pool configured)." >&2
        echo "  D7-1: the runtime F5 gate only parks the unit when enabled=false — enabled=true would auto-mine uncaught." >&2
        exit 1
    fi
    _url=$(extract_toml_value "$_f" pool url)
    case "$_url" in
        ""|'""'|"''") : ;;
        *)
            echo "DCENTos post-build (am2-s19jpro): ERROR: ${_label} [pool] url is non-empty ('${_url}'); a baked image must ship NO pool" >&2
            echo "  Operator configures the pool via the wizard/dashboard before enabling mining." >&2
            exit 1
            ;;
    esac
    echo "DCENTos post-build (am2-s19jpro): ${_label} first-boot idle invariant OK ([mining] enabled=false, [pool] url empty)"
}

# Phase 4B safety re-verification — refuse to ship a baked /etc/dcentrald.toml
# that violates the EE Finding 5 invariants. This catches a future regression
# (someone hand-edits the baked TOML and pushes voltage_mv up, or removes the
# fan cap). The Rust validate() pass at runtime catches it too, but failing
# at BUILD time stops a bad image from ever leaving CI.
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
        echo "DCENTos post-build (am2-s19jpro): ERROR: baked dcentrald.toml has mining.voltage_mv=${BAKED_VOLTAGE_MV} > 14500 mV am2 ceiling" >&2
        echo "  EE Finding 5 #4: am2 BHB42xxx hashboards via APW121215a/dsPIC are not specified above 14.5 V chip-rail." >&2
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
        echo "DCENTos post-build (am2-s19jpro): ERROR: baked dcentrald.toml has thermal.fan_max_pwm=${BAKED_FAN_MAX} > 30" >&2
        echo "  Home/baked-default fan ceiling is 30 (feedback_fan_max_30pwm.md / feedback_fan_never_blast.md)." >&2
        echo "  Operator may opt into a higher cap via /data/dcentrald.toml or a hacker-mode toggle." >&2
        exit 1
    fi

    assert_first_boot_idle "$BAKED_RUNTIME_CFG" "baked dcentrald.toml"

    echo "DCENTos post-build (am2-s19jpro): baked default voltage_mv=${BAKED_VOLTAGE_MV:-unset} fan_max_pwm=${BAKED_FAN_MAX:-unset} (within EE safety envelope)"
fi

# -----------------------------------------------------------------------------
# AM2 BAKED FIRST-BOOT DEFAULT at /etc/dcentrald/xil_override.toml
# (F4, 2026-05-17 — supersedes the Phase-4A verbatim .109-runtime bake).
#
# S82dcentrald prefers /etc/dcentrald/xil_override.toml over
# /etc/dcentrald.toml whenever it exists (/data/dcentrald.toml still wins
# over both). Until F4 this baked file was a VERBATIM copy of
# dcentrald_s19jpro_xil_override.toml — the .109-SPECIFIC Loki-Mod bring-up
# runtime (pool=public-pool.io worker bc1q04…-xil, [mining] enabled=true).
# That made EVERY fresh am2 unit (not just .109) cold-boot the chain + run
# the PIC preflight at first boot. On .25 the dsPIC@0x20 answered all-0xFF,
# the s19j-hybrid path hard-bailed, the daemon (pre-F1) exited, S82dcentrald
# crash-looped 5x and permanently gave up → wizard unreachable, unit not
# re-flashable. Root cause:
#
#
# F4: bake the CONSERVATIVE mining-disabled-until-configured default
# (configs/dcentrald_s19jpro_xil_baked_default.toml). A fresh unit comes up
# management-only by *config* (the F5 gate in main.rs sees
# mining_start_enabled()==false → API/dashboard/wizard up, NO PIC preflight,
# NO cold boot, NO crash). The operator configures a pool + enables mining
# via the wizard, restarts the daemon, and the proven path runs.
#
# CRITICAL — BRICK-SAFETY INVARIANT IS PRESERVED, NOT REMOVED:
#   The baked default still ships [power.psu_override] enabled = true
#   model = "APW3" voltage_v = 12.8 (verified below exactly as before). With
#   the Loki board installed (campaign-wide operator decision —
#   ) an enabled=false
#   bake takes the Apw121215a smart path which self-disables the rail in
#   ~30 s on first cold boot. So when the operator DOES enable mining, the
#   proven .109 Loki path runs. F4 makes the .109 *pool/worker* opt-in (not
#   the silent crash-looping default); it does NOT touch the brick-safe
#   override. The G1-proven dcentrald_s19jpro_xil_override.toml is UNCHANGED
#   and remains the operator-opt-in /tmp overlay file referenced by
#    /
#   See:
#     - DCENT_OS_Antminer/dcentrald/configs/dcentrald_s19jpro_xil_baked_default.toml
#     - DCENT_OS_Antminer/dcentrald/configs/dcentrald_s19jpro_xil_override.toml
#
#
#
#
# The matching brick-safety declaration (/etc/dcentos/psu_config = "override"
# + sysupgrade MANIFEST.json psu_config_mode = "override") is stamped below /
# in post-image.sh so the toolbox XIL install gate G5 can confirm the package
# is the override (Loki-IN) bake. That stays "override" because the baked
# default keeps [power.psu_override] enabled=true.
# -----------------------------------------------------------------------------
S19J_XIL_BAKED_CFG="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/configs/dcentrald_s19jpro_xil_baked_default.toml"
if [ ! -f "$S19J_XIL_BAKED_CFG" ]; then
    echo "DCENTos post-build (am2-s19jpro): ERROR: F4 baked XIL default missing at:" >&2
    echo "  $S19J_XIL_BAKED_CFG" >&2
    echo "  Refusing to fall back to dcentrald_s19jpro_xil_override.toml; that is a .109 runtime profile, not a safe first-boot bake." >&2
    exit 1
fi
S19J_XIL_LEGACY_CFG="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/configs/dcentrald_s19jpro_xil_override.toml"
if [ -f "$S19J_XIL_BAKED_CFG" ]; then
    mkdir -p "${TARGET_DIR}/etc/dcentrald"
    cp "$S19J_XIL_BAKED_CFG" "${TARGET_DIR}/etc/dcentrald/xil_override.toml"
    chmod 644 "${TARGET_DIR}/etc/dcentrald/xil_override.toml"
    echo "DCENTos post-build (am2-s19jpro): installed dcentrald_s19jpro_xil_baked_default.toml as /etc/dcentrald/xil_override.toml (F4 conservative first-boot default: mining DISABLED until configured, [power.psu_override] enabled=true brick-safe)"
elif [ -f "$S19J_XIL_LEGACY_CFG" ]; then
    # Pre-F4 fallback. WARN loudly: this re-introduces the .25 first-boot
    # crash-loop class on any non-.109 am2 unit (F1 keeps the daemon alive
    # in management-only mode, but the unit still cold-boots + crash-restarts
    # the mining path until F1's degraded handler catches it).
    mkdir -p "${TARGET_DIR}/etc/dcentrald"
    cp "$S19J_XIL_LEGACY_CFG" "${TARGET_DIR}/etc/dcentrald/xil_override.toml"
    chmod 644 "${TARGET_DIR}/etc/dcentrald/xil_override.toml"
    echo "DCENTos post-build (am2-s19jpro): WARNING: F4 baked default missing at $S19J_XIL_BAKED_CFG; fell back to .109-runtime dcentrald_s19jpro_xil_override.toml — fresh non-.109 units will attempt a cold-boot PIC preflight at first boot (F1 keeps the daemon alive management-only, but this is NOT the intended first-boot config)" >&2
else
    echo "DCENTos post-build (am2-s19jpro): ERROR: neither F4 baked default nor legacy .109 override config found at:" >&2
    echo "  $S19J_XIL_BAKED_CFG" >&2
    echo "  $S19J_XIL_LEGACY_CFG" >&2
    exit 1
fi

# Brick-safety re-verification — the baked /etc/dcentrald/xil_override.toml
# MUST have [power.psu_override] enabled = true. With the Loki board installed
# (campaign-wide operator decision) an enabled=false bake takes the
# Apw121215a smart-PSU path and self-disables the rail in ~30 s on first cold
# boot. Fail the BUILD before a brick-class image can leave CI. This mirrors
# the existing voltage_mv / fan_max_pwm EE-envelope re-verification above.
BAKED_XIL_CFG="${TARGET_DIR}/etc/dcentrald/xil_override.toml"
BAKED_PSU_OVERRIDE_ENABLED=$(awk '
    /^[[:space:]]*\[/ {
        in_psu_override = ($0 ~ /^[[:space:]]*\[power\.psu_override\][[:space:]]*$/) ? 1 : 0
        next
    }
    in_psu_override && /^[[:space:]]*enabled[[:space:]]*=/ {
        sub(/^[^=]*=[[:space:]]*/, "", $0); sub(/[[:space:]]*#.*$/, "", $0)
        gsub(/[[:space:]]/, "", $0)
        print $0; exit
    }
' "$BAKED_XIL_CFG")
if [ "$BAKED_PSU_OVERRIDE_ENABLED" != "true" ]; then
    echo "DCENTos post-build (am2-s19jpro): ERROR: baked xil_override.toml [power.psu_override] enabled='${BAKED_PSU_OVERRIDE_ENABLED:-unset}', expected 'true'" >&2
    echo "  The .109 milestone path requires enabled=true model=APW3 voltage_v=12.8 with the Loki board installed." >&2
    echo "  enabled=false + Loki present + APW3 => Apw121215a path => ~30 s rail self-disable brick on first cold boot." >&2
    echo "  Source must be dcentrald_s19jpro_xil_override.toml (the G1-proven config), NOT dcentrald_s19jpro_xil.toml." >&2
    exit 1
fi
echo "DCENTos post-build (am2-s19jpro): baked xil_override.toml [power.psu_override] enabled=true (brick-safe milestone path)"

assert_first_boot_idle "$BAKED_XIL_CFG" "baked xil_override.toml"

# Brick-safety PSU-config declaration — toolbox XIL install gate G5 reads
# /etc/dcentos/psu_config (TARGET_PSU_CONFIG_PATH) and the sysupgrade
# MANIFEST.json psu_config_mode hint, and BLOCKS unless the operator's
# --psu-config={loki,override} matches. The baked config above is the
# override (Loki-IN milestone) path, so declare "override" here. The
# matching MANIFEST.json psu_config_mode is stamped by post-image.sh.
echo "override" > "${TARGET_DIR}/etc/dcentos/psu_config"
chmod 644 "${TARGET_DIR}/etc/dcentos/psu_config"
echo "DCENTos post-build (am2-s19jpro): stamped /etc/dcentos/psu_config = override (G5 target hint matches enabled=true bake)"

# -----------------------------------------------------------------------------
# Board identity (overrides shared overlay's am1-s9 values).
# board_target MUST be "am2-s19j" so sysupgrade tarball prefix matches.
# platform string documents the am2 Zynq variant (BM1398-era S19 Pro chain).
# -----------------------------------------------------------------------------
# D7-4 (2026-06-13): board_target MUST stay "am2-s19j" (sysupgrade tarball prefix
# — ). FINGERPRINT IMPLICATION: am2_xil_25_
# fingerprint_matches(), the wave55a recipe guard, and the dcentrald-am2-xil-env
# xil branch all key on a board_target ENDING IN "xil" (or the
# DCENT_AM2_XIL25_FINGERPRINT_OVERRIDE=1 env). So a real packaged `a lab unit` booting
# native NAND takes the .109/handoff branch (TRUST_RAIL_FALLBACK=1 — a mining-
# correctness mismatch for a cold native boot) and the forbidden-env recipe guard
# is INERT by default. This is SAFE for packaged boot (mining is opt-in; the
# standalone forbidden-var branch only fires on the xil suffix this package
# does not carry), but to give a real `a lab unit` the standalone recipe + guard
# coverage, the install step / NAND-promotion must stamp board_target ending in
# "xil" (or export the override). The  manual launcher sets the override.
echo "am2"            > "${TARGET_DIR}/etc/dcentos/board_family"
echo "am2-s19j"       > "${TARGET_DIR}/etc/dcentos/board_target"
echo "zynq-bm3-am2"   > "${TARGET_DIR}/etc/dcentos/platform"

# Embed the trusted release public key when provided (same pattern as S9).
if [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
    if [ ! -f "${DCENT_RELEASE_PUBKEY_FILE}" ]; then
        echo "DCENTos post-build (am2-s19jpro): ERROR: release public key not found at ${DCENT_RELEASE_PUBKEY_FILE}" >&2
        exit 1
    fi
    cp "${DCENT_RELEASE_PUBKEY_FILE}" "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    chmod 644 "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    echo "DCENTos post-build (am2-s19jpro): embedded release public key"
elif [ "${DCENT_REQUIRE_RELEASE_KEY:-0}" = "1" ]; then
    echo "DCENTos post-build (am2-s19jpro): ERROR: DCENT_REQUIRE_RELEASE_KEY=1 but DCENT_RELEASE_PUBKEY_FILE is not set" >&2
    exit 1
else
    echo "DCENTos post-build (am2-s19jpro): WARNING: no release public key embedded (lab-only image)"
fi

# Production-readiness matrix §7 #1 (public-image trust boundary): release-image
# marker + first-boot SSH posture when DCENT_RELEASE_IMAGE=1. NO-OP on DEV/LAB.
. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/release_image_provision.sh"
dcent_provision_release_image "$TARGET_DIR" "am2-s19jpro"

strip_sysupgrade_offline_harness() {
    _sysupgrade="${TARGET_DIR}/usr/sbin/sysupgrade"
    [ -f "$_sysupgrade" ] || return 0
    _tmp="${_sysupgrade}.strip.$$"
    awk '
        !skip && $0 ~ /^if[[:space:]]/ && index($0, "DCENT_SYSUPGRADE_OFFLINE_HARNESS") {
            skip = 1
            depth = 1
            next
        }
        skip {
            if ($0 ~ /^[[:space:]]*if[[:space:]].*;[[:space:]]*then$/) {
                depth++
            }
            if ($0 ~ /^[[:space:]]*fi[[:space:]]*$/) {
                depth--
                if (depth == 0) {
                    skip = 0
                }
            }
            next
        }
        { print }
    ' "$_sysupgrade" > "$_tmp"
    mv "$_tmp" "$_sysupgrade"
    chmod 0755 "$_sysupgrade"
    if grep -Eq 'DCENT_SYSUPGRADE_OFFLINE_HARNESS|DCENT_SYSUPGRADE_OFFLINE_MARKER|dcent-sysupgrade-offline-nandsim-harness-v1|offline harness overrides are disabled' "$_sysupgrade"; then
        echo "DCENTos post-build (am2-s19jpro): ERROR: deployed sysupgrade still contains offline harness hooks" >&2
        exit 1
    fi
    echo "DCENTos post-build (am2-s19jpro): stripped offline harness hooks from deployed sysupgrade"
}

strip_sysupgrade_offline_harness

# -----------------------------------------------------------------------------
# Defensive CRLF strip on shebang scripts (hardening guard for the shipped
# sysupgrade bug): a Windows autocrlf checkout can leave CRLF in rootfs-overlay
# scripts that no .gitattributes rule covers; a CRLF shebang (#!/bin/sh\r) makes
# the script non-executable on the device ("not found" at exec). This sweep runs
# AFTER the overlay copy + all post-build installs, normalizing every #!-script
# in the staged rootfs to LF. Binaries (no #! magic) are skipped, so it is safe.
# Belt-and-suspenders with the /br2_external_dcentos/**/usr/sbin/sysupgrade
# .gitattributes eol=lf rule — keeps the image correct even on a misconfigured
# checkout.
# -----------------------------------------------------------------------------
CRLF_FIXED=0
for d in usr/sbin usr/local/bin usr/bin etc/init.d sbin bin; do
    [ -d "${TARGET_DIR}/${d}" ] || continue
    for f in "${TARGET_DIR}/${d}"/*; do
        [ -f "$f" ] || continue
        # Only touch text scripts that begin with a shebang.
        if [ "$(head -c2 "$f" 2>/dev/null)" = "#!" ] && grep -qU "$(printf '\r')" "$f" 2>/dev/null; then
            sed -i 's/\r$//' "$f"
            CRLF_FIXED=$((CRLF_FIXED + 1))
        fi
    done
done
echo "DCENTos post-build (am2-s19jpro): CRLF-normalized $CRLF_FIXED shebang script(s) in staged rootfs"

echo "DCENTos post-build (am2-s19jpro): directories, permissions, and board identity set."
