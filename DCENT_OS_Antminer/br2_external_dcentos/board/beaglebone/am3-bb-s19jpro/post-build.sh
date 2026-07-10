#!/bin/sh
#
# DCENTos post-build script - am3-bb-s19jpro (AM335x BB S19j Pro variant).
#
# Provenance:
#   Derived verbatim-then-renamed from
#   `board/beaglebone/am3-bb/post-build.sh`. This variant exists so the
#   stock-Bitmain S19j Pro AM335x BB carrier is a first-class Buildroot
#   target distinct from the generic am3-bb base. Stamps board_target =
#   `am3-bb-s19jpro` so the runtime can route into the BM1362 + BHB42xxx
#   serial-mining path with the correct chips_per_chain default and
#   APW121215f PSU profile.
#
# Wave reference: AGENT B3 wave W10.x (2026-05-09).
#
# UART decision (B3 / per task brief): DCENT_OS does NOT port the
# reconstructed `uart_trans.ko` from PORTING_PLAN.md section 3.2. That
# reconstruction is stub-quality, depends on `bitmain_axi.ko`, and
# violates  decision #2 ("UIO approach, NOT kernel modules").
# AM335x ASIC UART traffic is routed through dcentrald userspace over the
# kernel ttyS chain devices. DevmemUart remains a lab override only.
# This script therefore enforces the same omit-list as the am3-bb base
# (no `uart_trans.ko`, no `monitor-ipsig`, no `daemons`, no
# `update-daemon`).
#
# Like the am3-bb base script, this hook installs a fresh armv7
# dcentrald binary AFTER Buildroot copies the rootfs-overlay tree.
#, dcentrald MUST NOT
# be committed into rootfs-overlay/usr/local/bin/.

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

dcent_ensure_armv7_busybox_init "$TARGET_DIR" "DCENTos post-build (am3-bb-s19jpro)"

# W5.1 (2026-05-07): install the dashboard SPA at the canonical location
# served by server.py on the other platforms. The am3-bb-s19jpro image
# tracks parity with am3-bb base; the dashboard ships as a static asset
# even though server.py is not yet bound on AM335x.
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
    echo "DCENTos post-build (am3-bb-s19jpro): staged dashboard SPA ($DASHBOARD_SIZE bytes) at /usr/share/dcentos-dashboard/index.html"
    if [ "$DASHBOARD_SIZE" -lt 100000 ]; then
        echo "DCENTos post-build (am3-bb-s19jpro): WARN: dashboard appears truncated ($DASHBOARD_SIZE bytes < 100 KB floor)" >&2
    fi
else
    echo "DCENTos post-build (am3-bb-s19jpro): WARN: dashboard not found at $DASHBOARD_SRC; skipping" >&2
fi

DCENTRALD_BIN="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentrald"
if [ -f "$DCENTRALD_BIN" ]; then
    STAGED_BIN="${TARGET_DIR}/usr/local/bin/dcentrald"
    cp "$DCENTRALD_BIN" "$STAGED_BIN"
    chmod 755 "$STAGED_BIN"
    DCENTRALD_SIZE=$(stat -c%s "$DCENTRALD_BIN" 2>/dev/null || stat -f%z "$DCENTRALD_BIN")
    echo "DCENTos post-build (am3-bb-s19jpro): installed dcentrald ($DCENTRALD_SIZE bytes)"
else
    echo "DCENTos post-build (am3-bb-s19jpro): ERROR: dcentrald not found at $DCENTRALD_BIN" >&2
    echo "  Build first: cargo build --release --target armv7-unknown-linux-musleabihf" >&2
    exit 1
fi

. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/dcentrald_version_gate.sh"
dcent_require_dcentrald_version_match \
    "$TARGET_DIR" \
    "${TARGET_DIR}/usr/local/bin/dcentrald" \
    "DCENTos post-build (am3-bb-s19jpro)" \
    "${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/Cargo.toml"

DISCOVERY_BIN="${BR2_EXTERNAL_DCENTOS_PATH}/../dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentos-discovery"
if [ -f "$DISCOVERY_BIN" ]; then
    cp "$DISCOVERY_BIN" "${TARGET_DIR}/usr/local/bin/dcentos-discovery"
    chmod 755 "${TARGET_DIR}/usr/local/bin/dcentos-discovery"
    DISCOVERY_SIZE=$(stat -c%s "$DISCOVERY_BIN" 2>/dev/null || stat -f%z "$DISCOVERY_BIN")
    echo "DCENTos post-build (am3-bb-s19jpro): installed dcentos-discovery ($DISCOVERY_SIZE bytes)"
else
    echo "DCENTos post-build (am3-bb-s19jpro): ERROR: dcentos-discovery not found at $DISCOVERY_BIN" >&2
    echo "  Build first: cargo build --release --target armv7-unknown-linux-musleabihf --bin dcentos-discovery" >&2
    exit 1
fi

dcent_require_armv7_eabi_elf_paths \
    "$TARGET_DIR" \
    "DCENTos post-build (am3-bb-s19jpro)" \
    "sbin/init" \
    "usr/sbin/dropbear" \
    "usr/local/bin/dcentrald" \
    "usr/local/bin/dcentos-discovery"

if [ ! -f "${TARGET_DIR}/etc/dcentrald.toml" ]; then
    echo "DCENTos post-build (am3-bb-s19jpro): ERROR: /etc/dcentrald.toml missing from rootfs." >&2
    exit 1
fi

echo "am3-bb" > "${TARGET_DIR}/etc/dcentos/board_family"
echo "am3-bb-s19jpro" > "${TARGET_DIR}/etc/dcentos/board_target"
echo "am3-bb-s19jpro" > "${TARGET_DIR}/etc/dcentos/platform"
echo "native-mining-sdcard-first" > "${TARGET_DIR}/etc/dcentos/board_status"
echo "nand-install-and-revert-disabled-pending-live-proc-mtd" > "${TARGET_DIR}/etc/dcentos/storage_status"

BOARD_TARGET_TOML_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../etc/board_target/am3-bb-s19jpro.toml"
BOARD_TARGET_TOML_DIR="${TARGET_DIR}/etc/dcentos/board_targets"
if [ -f "$BOARD_TARGET_TOML_SRC" ]; then
    mkdir -p "$BOARD_TARGET_TOML_DIR"
    cp "$BOARD_TARGET_TOML_SRC" "$BOARD_TARGET_TOML_DIR/am3-bb-s19jpro.toml"
    chmod 644 "$BOARD_TARGET_TOML_DIR/am3-bb-s19jpro.toml" 2>/dev/null || true
    echo "DCENTos post-build (am3-bb-s19jpro): installed board target TOML at /etc/dcentos/board_targets/am3-bb-s19jpro.toml"
else
    echo "DCENTos post-build (am3-bb-s19jpro): ERROR: board target TOML missing at $BOARD_TARGET_TOML_SRC" >&2
    exit 1
fi

if [ ! -f "${TARGET_DIR}/etc/dcentos/rescue_ssh_enabled" ]; then
    echo "DCENTos post-build (am3-bb-s19jpro): ERROR: rescue SSH marker missing" >&2
    exit 1
fi

# Production-readiness matrix §7 #4: the AM3-BB cold-boot management-only gate
# in S82dcentrald falls back to this config when the cold-boot proof marker is
# absent. It MUST ship so the daemon comes up management-only (no PSU/chain
# energize) on cold boot. If it is missing the init script fails closed (does
# not start the mining daemon), but ship-time verification keeps the install
# image in its intended management-only-by-default posture.
if [ ! -f "${TARGET_DIR}/etc/dcentrald.management-only.toml" ]; then
    echo "DCENTos post-build (am3-bb-s19jpro): ERROR: /etc/dcentrald.management-only.toml missing from rootfs (AM3-BB cold-boot management-only gate)." >&2
    exit 1
fi
echo "DCENTos post-build (am3-bb-s19jpro): AM3-BB cold-boot management-only config present (matrix §7 #4)"

# Install the per-platform revert script as a disabled/status-reporting
# helper. Like the am3-bb base, it refuses NAND writes until dated live
# /proc/mtd evidence is supplied. The s19jpro variant carries its own
# script so a future enablement can target the BHB42xxx hashboard
# subtype check independently.
REVERT_BB_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/revert_to_stock_am335x_bb_s19jpro.sh"
if [ -f "$REVERT_BB_SRC" ]; then
    mkdir -p "${TARGET_DIR}/usr/sbin"
    cp "$REVERT_BB_SRC" "${TARGET_DIR}/usr/sbin/revert_to_stock_am335x_bb_s19jpro.sh"
    chmod +x "${TARGET_DIR}/usr/sbin/revert_to_stock_am335x_bb_s19jpro.sh" 2>/dev/null || true
    echo "DCENTos post-build (am3-bb-s19jpro): installed disabled revert_to_stock_am335x_bb_s19jpro.sh from scripts/"
else
    echo "DCENTos post-build (am3-bb-s19jpro): WARNING: revert_to_stock_am335x_bb_s19jpro.sh not found at $REVERT_BB_SRC" >&2
fi

#  W12-B: also ship the stock-Bitmain manifest (parity with
# zynq + amlogic boards). The daemon probes /etc/dcentos/ first.
MANIFEST_SRC="${BR2_EXTERNAL_DCENTOS_PATH}/../../../extractions/firmware-archive/stock-bitmain-manifest.json"
if [ -f "$MANIFEST_SRC" ]; then
    mkdir -p "${TARGET_DIR}/etc/dcentos"
    cp "$MANIFEST_SRC" "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json"
    chmod 644 "${TARGET_DIR}/etc/dcentos/stock-bitmain-manifest.json" 2>/dev/null || true
    echo "DCENTos post-build (am3-bb-s19jpro): installed stock-bitmain-manifest.json"
fi

if command -v md5sum > /dev/null 2>&1; then
    md5sum "${TARGET_DIR}/usr/local/bin/dcentrald" | cut -d' ' -f1 > "${TARGET_DIR}/etc/dcentos/dcentrald.md5"
    md5sum "${TARGET_DIR}/usr/local/bin/dcentos-discovery" | cut -d' ' -f1 > "${TARGET_DIR}/etc/dcentos/dcentos-discovery.md5"
fi

# Strict no-stock-binary check (B3 decision: NEVER ship uart_trans.ko;
# DCENT_OS routes ASIC UART through dcentrald userspace over ttyS chain devices).
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
    echo "DCENTos post-build (am3-bb-s19jpro): ERROR: forbidden stock daemon/blob present in image" >&2
    exit 1
}

# W7.7: defense-in-depth netfilter drop for stock daemons:22322 privesc.
# Mirrors am3-bb base. iptables is pulled in via BR2_PACKAGE_IPTABLES=y
# in the per-product defconfig.
FIREWALL_INIT="${TARGET_DIR}/etc/init.d/S41firewall"
if [ ! -x "$FIREWALL_INIT" ]; then
    echo "DCENTos post-build (am3-bb-s19jpro): ERROR: S41firewall missing or not executable at $FIREWALL_INIT" >&2
    echo "  Expected from board/beaglebone/am3-bb-s19jpro/rootfs-overlay/etc/init.d/S41firewall." >&2
    exit 1
fi
echo "DCENTos post-build (am3-bb-s19jpro): S41firewall hardening init script present"

echo "DCENTos post-build (am3-bb-s19jpro): board identity set; stock cgminer/uart_trans/monitor-ipsig/daemons omitted; ASIC UART = dcentrald userspace ttyS path (no stock kernel module)."

# CE-204: embed the trusted release public key when provided (same pattern as
# the zynq am2-s19jpro post-build), so a future BB install route can verify the
# signed SD-payload sidecars against the pinned key.
if [ -n "${DCENT_RELEASE_PUBKEY_FILE:-}" ]; then
    if [ ! -f "${DCENT_RELEASE_PUBKEY_FILE}" ]; then
        echo "DCENTos post-build (am3-bb-s19jpro): ERROR: release public key not found at ${DCENT_RELEASE_PUBKEY_FILE}" >&2
        exit 1
    fi
    cp "${DCENT_RELEASE_PUBKEY_FILE}" "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    chmod 644 "${TARGET_DIR}/etc/dcentos/release_ed25519.pub"
    echo "DCENTos post-build (am3-bb-s19jpro): embedded release public key"
elif [ "${DCENT_REQUIRE_RELEASE_KEY:-0}" = "1" ]; then
    echo "DCENTos post-build (am3-bb-s19jpro): ERROR: DCENT_REQUIRE_RELEASE_KEY=1 but DCENT_RELEASE_PUBKEY_FILE is not set" >&2
    exit 1
else
    echo "DCENTos post-build (am3-bb-s19jpro): WARNING: no release public key embedded (lab-only image)"
fi

# Production-readiness matrix §7 #1 (public-image trust boundary): release-image
# marker + first-boot SSH posture when DCENT_RELEASE_IMAGE=1. NO-OP on DEV/LAB.
. "${BR2_EXTERNAL_DCENTOS_PATH}/../scripts/lib/release_image_provision.sh"
dcent_provision_release_image "$TARGET_DIR" "am3-bb-s19jpro"
