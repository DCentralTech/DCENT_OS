#!/bin/sh
#
# DCENTos post-build script - cv1835-s19jpro (Cvitek CV1835 Cortex-A7 carrier
# of the Antminer S19j Pro family).
#
# This is a runtime-only image. The CV1835 carrier is not on any DCENT_OS
# bench unit yet, so the script ships every safety gate enabled by default
# and emits an explicit storage_status that disables eMMC sysupgrade until
# 3 successful round-trip flashes are proven on a real board.
#
# Mirrors the am3-bb post-build pattern: stages dcentrald + dcentos-discovery,
# stamps board identity, stages init scripts and stock-bitmain-manifest, but
# never copies stock cgminer / bmminer / FileParser / daemonc / uart_trans.ko
# even if the build pipeline has them sitting next door.

set -e
TARGET_DIR=$1

mkdir -p "${TARGET_DIR}/etc/dcentos"
mkdir -p "${TARGET_DIR}/usr/local/bin"
mkdir -p "${TARGET_DIR}/usr/sbin"
mkdir -p "${TARGET_DIR}/data"
mkdir -p "${TARGET_DIR}/data/dcent"
mkdir -p "${TARGET_DIR}/config"
mkdir -p "${TARGET_DIR}/tmp"
mkdir -p "${TARGET_DIR}/var/log"

chmod 0700 "${TARGET_DIR}/data/dcent"
chown 0:0 "${TARGET_DIR}/data/dcent" 2>/dev/null || true

chmod +x "${TARGET_DIR}"/etc/init.d/* 2>/dev/null || true

# Stage dcentrald binary. CV1835 = Cortex-A7 = armv7-unknown-linux-musleabihf
# (same toolchain target as Zynq/AM335x; only the cpu tuning differs at link
# time).
DCENTRALD_BIN="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentrald"
if [ -f "$DCENTRALD_BIN" ]; then
    STAGED_BIN="${TARGET_DIR}/usr/local/bin/dcentrald"
    cp "$DCENTRALD_BIN" "$STAGED_BIN"
    chmod 755 "$STAGED_BIN"
    DCENTRALD_SIZE=$(stat -c%s "$DCENTRALD_BIN" 2>/dev/null || stat -f%z "$DCENTRALD_BIN")
    echo "DCENTos post-build (cv1835-s19jpro): installed dcentrald ($DCENTRALD_SIZE bytes)"
else
    echo "DCENTos post-build (cv1835-s19jpro): ERROR: dcentrald not found at $DCENTRALD_BIN" >&2
    echo "  Build first: cargo build --release --target armv7-unknown-linux-musleabihf" >&2
    exit 1
fi

DISCOVERY_BIN="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentos-discovery"
if [ -f "$DISCOVERY_BIN" ]; then
    cp "$DISCOVERY_BIN" "${TARGET_DIR}/usr/local/bin/dcentos-discovery"
    chmod 755 "${TARGET_DIR}/usr/local/bin/dcentos-discovery"
else
    echo "DCENTos post-build (cv1835-s19jpro): ERROR: dcentos-discovery not found at $DISCOVERY_BIN" >&2
    echo "  Refusing to ship CV1835 discovery from an undeclared or stale volume artifact." >&2
    exit 1
fi

# Board identity files (also pre-staged via rootfs-overlay/etc/dcentos/, but
# re-write here so a stale overlay can't drift product identity).
echo "cv1835"           > "${TARGET_DIR}/etc/dcentos/board_family"
echo "cv1835-s19jpro"   > "${TARGET_DIR}/etc/dcentos/board_target"
echo "cv1835-s19jpro"   > "${TARGET_DIR}/etc/dcentos/platform"
echo "runtime-only-no-fleet-unit"                                  > "${TARGET_DIR}/etc/dcentos/board_status"
echo "emmc-sysupgrade-disabled-pending-3-roundtrip-bench-proof"    > "${TARGET_DIR}/etc/dcentos/storage_status"

# Bake the factory kernel image into /config so U-Boot bootcount fallback
# can restore it (see uboot-bootcmd.txt for the boot-side logic). We don't
# yet have a built kernel.bin sitting in the buildroot output, so this hook
# stages whatever the operator drops at $FACTORY_KERNEL_SRC and otherwise
# leaves a placeholder marker so revert_to_stock_cv1835.sh can detect the
# missing-evidence state.
FACTORY_KERNEL_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../../../knowledge-base/firmware-archive/cv1835/factory_kernel.bin"
FACTORY_KERNEL_DST="${TARGET_DIR}/config/factory_kernel.bin"
if [ -f "$FACTORY_KERNEL_SRC" ]; then
    cp "$FACTORY_KERNEL_SRC" "$FACTORY_KERNEL_DST"
    chmod 0444 "$FACTORY_KERNEL_DST"
    FK_SIZE=$(stat -c%s "$FACTORY_KERNEL_SRC" 2>/dev/null || stat -f%z "$FACTORY_KERNEL_SRC")
    echo "DCENTos post-build (cv1835-s19jpro): staged /config/factory_kernel.bin ($FK_SIZE bytes)"
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$FACTORY_KERNEL_DST" | cut -d' ' -f1 > "${TARGET_DIR}/config/factory_kernel.bin.sha256"
    fi
else
    cat > "${TARGET_DIR}/config/factory_kernel.bin.MISSING" <<'EOF'
factory_kernel.bin is not staged in this build.

CV1835 sysupgrade safety (safe_sysupgrade_cv_emmc.sh + U-Boot bootcount
recovery) requires a known-good factory kernel sitting at
/config/factory_kernel.bin so U-Boot can restore it after 3 failed boots.

Drop the verified BMU-extracted CVCtrl kernel at:
  knowledge-base/firmware-archive/cv1835/factory_kernel.bin

then rebuild. Until that file exists, eMMC sysupgrade stays gated by
DCENT_CV1835_EMMC_PROVEN=1 + 3 round-trip bench proof.
EOF
fi

# fw_env.config is shipped via rootfs-overlay; ensure it landed.
if [ ! -f "${TARGET_DIR}/etc/fw_env.config" ]; then
    echo "DCENTos post-build (cv1835-s19jpro): ERROR: /etc/fw_env.config missing from overlay" >&2
    exit 1
fi

# /etc/dcentrald.toml: CV1835 has no per-board baked config yet (no live unit
# to derive sane defaults). Inherit the workspace default — cold-boot will
# come up in observability mode, mining stays disabled until /data/dcentrald.toml
# is staged from a verified-good config.
if [ ! -f "${TARGET_DIR}/etc/dcentrald.toml" ]; then
    echo "DCENTos post-build (cv1835-s19jpro): ERROR: /etc/dcentrald.toml missing from rootfs." >&2
    exit 1
fi

# Stage the per-platform revert script as a status-gate helper. Like
# revert_to_stock_am335x_bb.sh it refuses to run any destructive eMMC
# write until DCENT_CV1835_EMMC_PROVEN=1.
REVERT_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/revert_to_stock_cv1835.sh"
if [ -f "$REVERT_SRC" ]; then
    cp "$REVERT_SRC" "${TARGET_DIR}/usr/sbin/revert_to_stock_cv1835.sh"
    chmod 0755 "${TARGET_DIR}/usr/sbin/revert_to_stock_cv1835.sh" 2>/dev/null || true
fi

# Stage the safe_sysupgrade variant that targets eMMC.
SAFE_SU_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/safe_sysupgrade_cv_emmc.sh"
if [ -f "$SAFE_SU_SRC" ]; then
    cp "$SAFE_SU_SRC" "${TARGET_DIR}/usr/sbin/safe_sysupgrade_cv_emmc.sh"
    chmod 0755 "${TARGET_DIR}/usr/sbin/safe_sysupgrade_cv_emmc.sh" 2>/dev/null || true
fi

# Stage stock-bitmain-manifest for restore-to-stock parity (same as am3-bb /
# zynq / amlogic boards).
MANIFEST_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../../../knowledge-base/firmware-archive/stock-bitmain-manifest.json"
if [ -f "$MANIFEST_SRC" ]; then
    cp "$MANIFEST_SRC" "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json"
    chmod 0644 "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json" 2>/dev/null || true
fi

# Refuse to ship stock blobs.
find "${TARGET_DIR}" \( \
    -name 'uart_trans.ko' -o \
    -name 'cv183x_base.ko' -o \
    -name 'cv183x_pwm.ko' -o \
    -name 'monitor-ipsig' -o \
    -name 'S65monitor-ipsig' -o \
    -name 'daemons' -o \
    -name 'daemonc' -o \
    -name 'update-daemon' -o \
    -name 'S67update-daemon' -o \
    -name 'updateporc.sh' -o \
    -name 'FileParser' \
\) -print -quit 2>/dev/null | grep -q . && {
    echo "DCENTos post-build (cv1835-s19jpro): ERROR: forbidden stock blob present in image" >&2
    exit 1
}

if command -v md5sum > /dev/null 2>&1; then
    md5sum "${TARGET_DIR}/usr/local/bin/dcentrald" | cut -d' ' -f1 > "${TARGET_DIR}/etc/dcentos/dcentrald.md5"
fi

echo "DCENTos post-build (cv1835-s19jpro): board identity stamped; stock cv183x_*.ko / FileParser / daemonc omitted."

# Embed the trusted release public key when provided (mirrors zynq post-build).
# safe_sysupgrade_cv_emmc.sh verifies MANIFEST.sig against this PINNED key at
# /etc/dcentos/release_ed25519.pub before any eMMC backup/write (CE-091/CE-287),
# so a runtime image that accepts signed CV1835 sysupgrade packages must carry
# it. Absent it, only a non-release lab package plus the explicit unsigned
# override can flash.
if [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
    if [ ! -f "${DCENT_RELEASE_PUBKEY_FILE}" ]; then
        echo "DCENTos post-build (cv1835-s19jpro): ERROR: release public key not found at ${DCENT_RELEASE_PUBKEY_FILE}" >&2
        exit 1
    fi
    cp "${DCENT_RELEASE_PUBKEY_FILE}" "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    chmod 644 "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    echo "DCENTos post-build (cv1835-s19jpro): embedded release public key"
elif [ "${DCENT_REQUIRE_RELEASE_KEY:-0}" = "1" ]; then
    echo "DCENTos post-build (cv1835-s19jpro): ERROR: DCENT_REQUIRE_RELEASE_KEY=1 but DCENT_RELEASE_PUBKEY_FILE is not set" >&2
    exit 1
else
    echo "DCENTos post-build (cv1835-s19jpro): WARNING: no release public key embedded (lab-only image)"
fi

# Production-readiness matrix §7 #1 (public-image trust boundary): release-image
# marker + first-boot SSH posture when DCENT_RELEASE_IMAGE=1. NO-OP on DEV/LAB.
. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/release_image_provision.sh"
dcent_provision_release_image "$TARGET_DIR" "cv1835-s19jpro"
