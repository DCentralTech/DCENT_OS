#!/usr/bin/env bash
#
# Offline sysupgrade write-path proof harness.
#
# Runs the REAL on-miner sysupgrade script against Linux MTD/UBI emulation.
# This is intentionally not a mock harness: if nandsim/UBI/libubootenv cannot
# be used, it exits 77 with SKIP_NANDSIM_UNAVAILABLE instead of reporting proof.
#
# Intended execution environment:
#   privileged disposable Linux container or VM with:
#     nandsim, ubi/ubifs kernel modules, mtd-utils, u-boot-tools
#
# Example:
#   DCENT_SYSUPGRADE_OFFLINE_CONTAINER=1 \
#     scripts/sysupgrade_offline_nandsim_harness.sh \
#       --target am2-s19jpro \
#       --package output/.../dcentos-sysupgrade-am2-s19jpro.tar \
#       --workdir /tmp/dcent-nandsim-proof

set -euo pipefail

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd)

TARGET=""
PACKAGE=""
WORKDIR=""
RELEASE_KEY=""
PROBE_ONLY=0
ALLOW_LAB_PACKAGE=0
REQUIRE_NANDSIM=${DCENT_REQUIRE_NANDSIM:-0}
NANDSIM_ID_BYTES=${DCENT_NANDSIM_ID_BYTES:-0x20,0xaa,0x00,0x15}
NANDSIM_OVERRIDESIZE=${DCENT_NANDSIM_OVERRIDESIZE:-11}
NANDSIM_PARTS=${DCENT_NANDSIM_PARTS:-1,1,1,1,4,1,1,1800}
NANDSIM_INACTIVE_MTD=${DCENT_NANDSIM_INACTIVE_MTD:-7}
NANDSIM_ERASESIZE_HEX=00020000
# CE-026 reverse A/B: the DEFAULT is forward (current-fw=2, active mtd8 ->
# inactive mtd7) and is UNCHANGED. --current-fw 1 selects a reverse layout with
# BOTH slots present (active mtd7 -> inactive mtd8), exercising the sysupgrade
# CURRENT_MTD=7 -> INACTIVE_MTD=8 branch.
CURRENT_FW=${DCENT_NANDSIM_CURRENT_FW:-2}
NANDSIM_PARTS_REVERSE=${DCENT_NANDSIM_PARTS_REVERSE:-1,1,1,1,4,1,1,900,900}

usage() {
    cat <<'EOF'
Usage: sysupgrade_offline_nandsim_harness.sh --target TARGET --package TAR --workdir DIR [options]

Targets:
  am1-s9
  am2-s19jpro

Options:
  --release-key PATH       Embedded release_ed25519.pub used by sysupgrade
  --current-fw {1,2}       A/B direction. 2 (default) = forward (active mtd8 ->
                           inactive mtd7). 1 = reverse (active mtd7 -> inactive
                           mtd8); both slots exist in the nandsim layout.
  --allow-lab-package      Permit unsigned/non-release lab packages for harness development
  --probe-only             Only check kernel/tool availability
  --require-nandsim        Missing nandsim/UBI support exits 1 instead of 77
  -h, --help               Show this help

Success prints OFFLINE_NANDSIM_PROOF_OK only after the real sysupgrade writer
has updated inactive UBI volumes and fw_printenv verifies the boot-selector flip.
The reverse direction prints a DISTINCT sentinel (direction=reverse).
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --target) TARGET=${2:-}; shift 2 ;;
        --current-fw) CURRENT_FW=${2:-}; shift 2 ;;
        --package) PACKAGE=${2:-}; shift 2 ;;
        --workdir) WORKDIR=${2:-}; shift 2 ;;
        --release-key) RELEASE_KEY=${2:-}; shift 2 ;;
        --allow-lab-package) ALLOW_LAB_PACKAGE=1; shift ;;
        --probe-only) PROBE_ONLY=1; shift ;;
        --require-nandsim) REQUIRE_NANDSIM=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

skip_unavailable() {
    echo "SKIP_NANDSIM_UNAVAILABLE: $*" >&2
    if [ "$REQUIRE_NANDSIM" = "1" ]; then
        exit 1
    fi
    exit 77
}

die() {
    echo "ERROR: $*" >&2
    exit 1
}

need_tool() {
    command -v "$1" >/dev/null 2>&1 || skip_unavailable "missing tool: $1"
}

need_root() {
    [ "$(id -u)" = "0" ] || skip_unavailable "must run as root in a privileged disposable Linux container/VM"
}

load_module() {
    local module=$1
    if grep -q "^${module} " /proc/modules 2>/dev/null; then
        return 0
    fi
    modprobe "$module" >/tmp/dcent-nandsim-modprobe.out 2>&1 || {
        cat /tmp/dcent-nandsim-modprobe.out >&2 || true
        skip_unavailable "cannot load kernel module: $module"
    }
}

verify_nandsim_geometry() {
    local erasesize
    erasesize=$(awk -v expected="$NANDSIM_ERASESIZE_HEX" '
        /NAND simulator|nandsim|NAND 256MiB/ {
            if ($3 == expected) {
                print $3;
                exit;
            }
        }
    ' /proc/mtd 2>/dev/null)
    [ "$erasesize" = "$NANDSIM_ERASESIZE_HEX" ] || {
        awk '/NAND simulator|nandsim|NAND/ {print}' /proc/mtd >&2 2>/dev/null || true
        skip_unavailable "nandsim must use 128KiB eraseblocks for Xilinx UBI layout proof"
    }
}

load_nandsim() {
    if grep -q "^nandsim " /proc/modules 2>/dev/null; then
        verify_nandsim_geometry
        return 0
    fi
    modprobe nandsim "id_bytes=$NANDSIM_ID_BYTES" "overridesize=$NANDSIM_OVERRIDESIZE" \
        "parts=$NANDSIM_PARTS" \
        >/tmp/dcent-nandsim-modprobe.out 2>&1 || {
        cat /tmp/dcent-nandsim-modprobe.out >&2 || true
        skip_unavailable "cannot load kernel module: nandsim"
    }
    verify_nandsim_geometry
}

sysupgrade_path_for_target() {
    case "$1" in
        am1-s9)
            printf '%s\n' "$PROJECT_DIR/br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/sysupgrade"
            ;;
        am2-s19jpro)
            printf '%s\n' "$PROJECT_DIR/br2_external_dcentos/board/zynq/am2-s19jpro/rootfs-overlay/usr/sbin/sysupgrade"
            ;;
        *)
            die "unsupported target '$1' (expected am1-s9 or am2-s19jpro)"
            ;;
    esac
}

coarse_board_for_target() {
    case "$1" in
        am1-s9) printf 'am1-s9\n' ;;
        am2-s19jpro) printf 'am2-s19j\n' ;;
        *) die "unsupported target '$1'" ;;
    esac
}

volume_lebs_for_target() {
    case "$1" in
        am1-s9) printf '32 134 8\n' ;;
        am2-s19jpro) printf '23 179 210\n' ;;
        *) die "unsupported target '$1'" ;;
    esac
}

ceil_div() {
    local value=$1 divisor=$2
    printf '%s\n' "$(( (value + divisor - 1) / divisor ))"
}

max_int() {
    if [ "$1" -gt "$2" ]; then
        printf '%s\n' "$1"
    else
        printf '%s\n' "$2"
    fi
}

require_nandsim_partition() {
    local mtd=$1 expected_size=${2:-} label="NAND simulator partition $1"
    if [ -n "$expected_size" ]; then
        awk -v mtd="mtd${mtd}:" -v size="$expected_size" -v erase="$NANDSIM_ERASESIZE_HEX" -v label="$label" '
            $1 == mtd && $2 == size && $3 == erase && index($0, label) { found=1 }
            END { exit found ? 0 : 1 }
        ' /proc/mtd 2>/dev/null || {
            awk -v mtd="mtd${mtd}:" '$1 == mtd { print }' /proc/mtd >&2 2>/dev/null || true
            skip_unavailable "mtd$mtd is not the expected nandsim partition '$label'"
        }
    else
        awk -v mtd="mtd${mtd}:" -v erase="$NANDSIM_ERASESIZE_HEX" -v label="$label" '
            $1 == mtd && $3 == erase && index($0, label) { found=1 }
            END { exit found ? 0 : 1 }
        ' /proc/mtd 2>/dev/null || {
            awk -v mtd="mtd${mtd}:" '$1 == mtd { print }' /proc/mtd >&2 2>/dev/null || true
            skip_unavailable "mtd$mtd is not the expected nandsim partition '$label'"
        }
    fi
    [ -c "/dev/mtd$mtd" ] || skip_unavailable "missing real MTD character device /dev/mtd$mtd for nandsim"
}

find_nandsim_mtd() {
    require_nandsim_partition 4 00080000
    if [ "${DIRECTION:-forward}" = "reverse" ]; then
        # Reverse layout has BOTH slots: assert the active slot (mtd7) too.
        require_nandsim_partition 7 ""
    fi
    require_nandsim_partition "$NANDSIM_INACTIVE_MTD" ""
    printf '%s\n' "$NANDSIM_INACTIVE_MTD"
}

create_nandsim_mtd() {
    local mtd
    load_nandsim
    mtd=$(find_nandsim_mtd)
    [ -n "$mtd" ] || skip_unavailable "nandsim loaded but no simulator MTD appeared in /proc/mtd"
    [ -e "/dev/mtd$mtd" ] || skip_unavailable "missing /dev/mtd$mtd for nandsim"
    printf '%s\n' "$mtd"
}

create_ubi_device_nodes() {
    local ubi_path name dev major minor
    for ubi_path in /sys/class/ubi/ubi1*; do
        [ -d "$ubi_path" ] || continue
        name=$(basename "$ubi_path")
        dev=$(cat "$ubi_path/dev" 2>/dev/null || true)
        [ -n "$dev" ] || continue
        major=${dev%%:*}
        minor=${dev##*:}
        [ -e "/dev/$name" ] || mknod -m 660 "/dev/$name" c "$major" "$minor" 2>/dev/null || true
    done
}

prepare_inactive_ubi() {
    local mtd=$1 target=$2 kernel_lebs=$3 rootfs_lebs=$4 data_lebs=$5 kernel_src=$6 rootfs_src=$7
    local leb_size required_kernel_lebs required_rootfs_lebs

    ubidetach -m "$mtd" >/dev/null 2>&1 || true
    ubidetach -d 1 >/dev/null 2>&1 || true
    ubiformat "/dev/mtd$mtd" -y >/dev/null
    ubiattach /dev/ubi_ctrl -m "$mtd" -d 1 >/dev/null

    leb_size=$(cat /sys/class/ubi/ubi1/eraseblock_size 2>/dev/null || true)
    case "$leb_size" in
        *[!0-9]*|"") skip_unavailable "cannot read UBI LEB size for offline fixture" ;;
    esac

    if [ "$target" = "am1-s9" ]; then
        required_kernel_lebs=$(ceil_div "$(wc -c < "$kernel_src" | tr -d '[:space:]')" "$leb_size")
        required_rootfs_lebs=$(ceil_div "$(wc -c < "$rootfs_src" | tr -d '[:space:]')" "$leb_size")
        kernel_lebs=$(max_int "$kernel_lebs" "$required_kernel_lebs")
        rootfs_lebs=$(max_int "$rootfs_lebs" "$required_rootfs_lebs")
    fi

    ubimkvol /dev/ubi1 -N kernel -s "$((kernel_lebs * leb_size))" -t dynamic >/dev/null
    ubimkvol /dev/ubi1 -N rootfs -s "$((rootfs_lebs * leb_size))" -t dynamic >/dev/null
    if [ "$data_lebs" -gt 0 ]; then
        ubimkvol /dev/ubi1 -N rootfs_data -s "$((data_lebs * leb_size))" -t dynamic >/dev/null
    fi
    create_ubi_device_nodes
    ubidetach -d 1 >/dev/null
}

extract_package_payloads() {
    local tarball=$1 outdir=$2 expected_prefix=$3
    mkdir -p "$outdir"
    tar xf "$tarball" -C "$outdir"
    local subdir="$outdir/sysupgrade-$expected_prefix"
    [ -f "$subdir/kernel" ] || die "package missing $subdir/kernel"
    [ -f "$subdir/root" ] || die "package missing $subdir/root"
    printf '%s\n' "$subdir"
}

package_manifest_version() {
    local manifest=$1
    sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest" | sed -n '1p'
}

seed_fw_env_fixture() {
    local work=$1
    local env_txt="$work/fw_env.txt"
    local env_a="$work/fw_env_a.bin"
    local env_img="$work/fw_env.bin"

    cat >"$env_txt" <<EOF
firmware=${SEED_FIRMWARE:-2}
upgrade_stage=1
first_boot=no
bootcmd=run boot_dcent
EOF
    mkenvimage -r -s 0x20000 -o "$env_a" "$env_txt" >/dev/null
    rm -f "$env_img"
    dd if=/dev/zero of="$env_img" bs=1 count=0 seek=$((0x40000)) >/dev/null 2>&1
    dd if="$env_a" of="$env_img" bs=1 seek=0 conv=notrunc >/dev/null 2>&1
    dd if="$env_a" of="$env_img" bs=1 seek=$((0x20000)) conv=notrunc >/dev/null 2>&1

    require_nandsim_partition 4 00080000
    flash_erase /dev/mtd4 0 0 >/dev/null
    nandwrite -p /dev/mtd4 "$env_img" >/dev/null

    cat >/etc/fw_env.config <<EOF
/dev/mtd4 0x00000 0x20000 0x20000
/dev/mtd4 0x20000 0x20000 0x20000
EOF
    fw_printenv firmware >/dev/null 2>&1 || skip_unavailable "fw_printenv cannot read the offline fw_env fixture"
}

verify_written_volume() {
    local label=$1 dev=$2 source=$3 work=$4
    local size expected actual tmp blocks
    size=$(wc -c < "$source" | tr -d '[:space:]')
    expected=$(sha256sum "$source" | awk '{print $1}')
    tmp="$work/readback-$label.bin"
    blocks=$(( (size + 1048575) / 1048576 ))
    [ "$blocks" -gt 0 ] || blocks=1
    dd if="$dev" of="$tmp" bs=1048576 count="$blocks" >/dev/null 2>&1
    actual=$(head -c "$size" "$tmp" | sha256sum | awk '{print $1}')
    [ "$actual" = "$expected" ] || die "$label readback hash mismatch from $dev"
}

probe_capabilities() {
    need_root
    for tool in awk basename cat chmod cp dd grep head id mkenvimage mknod modprobe sed sha256sum tar tr wc \
        flash_erase nandwrite ubidetach ubiformat ubiattach ubimkvol ubiupdatevol fw_printenv fw_setenv; do
        need_tool "$tool"
    done
    load_module ubi
    load_module ubifs
    load_nandsim
}

# --- CE-026: resolve A/B direction (default forward = today's behavior) ---
# Forward (current-fw=2) keeps every value byte-identical to the original
# harness. Reverse (current-fw=1) uses a both-slots layout, seeds firmware=1,
# points ubi.mtd at the active mtd7, targets inactive mtd8, and expects a
# post-flip firmware=2 / upgrade_stage=0.
case "$CURRENT_FW" in
    2)
        DIRECTION=forward
        SEED_FIRMWARE=2
        EXPECTED_POSTFLIP_FIRMWARE=1
        CMDLINE_UBI_MTD=8
        ;;
    1)
        DIRECTION=reverse
        NANDSIM_PARTS=$NANDSIM_PARTS_REVERSE
        NANDSIM_INACTIVE_MTD=8
        SEED_FIRMWARE=1
        EXPECTED_POSTFLIP_FIRMWARE=2
        CMDLINE_UBI_MTD=7
        ;;
    *)
        die "--current-fw must be 1 or 2 (got '$CURRENT_FW')"
        ;;
esac

probe_capabilities
if [ "$PROBE_ONLY" = "1" ]; then
    echo "NANDSIM_PROBE_OK: kernel modules and userspace tools are available"
    exit 0
fi

[ -n "$TARGET" ] || die "--target is required"
[ -n "$PACKAGE" ] || die "--package is required"
[ -n "$WORKDIR" ] || die "--workdir is required"
[ -f "$PACKAGE" ] || die "package not found: $PACKAGE"

if [ "${DCENT_SYSUPGRADE_OFFLINE_CONTAINER:-0}" != "1" ]; then
    skip_unavailable "refusing to mutate host /etc and /dev directly; set DCENT_SYSUPGRADE_OFFLINE_CONTAINER=1 inside a disposable privileged Linux container/VM"
fi

SYSUPGRADE=$(sysupgrade_path_for_target "$TARGET")
[ -f "$SYSUPGRADE" ] || die "sysupgrade script not found: $SYSUPGRADE"

BOARD=$(coarse_board_for_target "$TARGET")
read -r KERNEL_LEBS ROOTFS_LEBS DATA_LEBS < <(volume_lebs_for_target "$TARGET")

rm -rf "$WORKDIR"
mkdir -p "$WORKDIR"
CMDLINE_FILE="$WORKDIR/proc_cmdline"
BOARD_FILE="$WORKDIR/board_target"
MARKER_FILE="$WORKDIR/offline_harness.marker"
SHIM_DIR="$WORKDIR/shims"
PAYLOAD_DIR="$WORKDIR/pkg"
VERSION_FILE="$WORKDIR/dcentos-version"

printf 'console=ttyPS0 ubi.mtd=%s root=ubi0:rootfs\n' "$CMDLINE_UBI_MTD" >"$CMDLINE_FILE"
printf '%s\n' "$BOARD" >"$BOARD_FILE"
printf 'dcent-sysupgrade-offline-nandsim-harness-v1\n' >"$MARKER_FILE"
mkdir -p "$SHIM_DIR"
cat >"$SHIM_DIR/reboot" <<'EOF'
#!/bin/sh
echo "OFFLINE_HARNESS_REBOOT_SHADOWED"
exit 0
EOF
chmod 0755 "$SHIM_DIR/reboot"

PREFIX=$BOARD
PAYLOAD_SUBDIR=$(extract_package_payloads "$PACKAGE" "$PAYLOAD_DIR" "$PREFIX")
PACKAGE_VERSION=$(package_manifest_version "$PAYLOAD_SUBDIR/MANIFEST.json")
[ -n "$PACKAGE_VERSION" ] || die "package manifest missing version for offline sysupgrade fixture"
printf '%s\n' "$PACKAGE_VERSION" >"$VERSION_FILE"

MTD_NUM=$(create_nandsim_mtd)
[ "$MTD_NUM" = "$NANDSIM_INACTIVE_MTD" ] || die "nandsim inactive MTD is mtd$MTD_NUM; direction=$DIRECTION expects mtd$NANDSIM_INACTIVE_MTD from ubi.mtd=$CMDLINE_UBI_MTD"
prepare_inactive_ubi "$MTD_NUM" "$TARGET" "$KERNEL_LEBS" "$ROOTFS_LEBS" "$DATA_LEBS" \
    "$PAYLOAD_SUBDIR/kernel" "$PAYLOAD_SUBDIR/root"
seed_fw_env_fixture "$WORKDIR"

ENV_ARGS=(
    "PATH=$SHIM_DIR:$PATH"
    "DCENT_SYSUPGRADE_OFFLINE_HARNESS=1"
    "DCENT_SYSUPGRADE_OFFLINE_MARKER=$MARKER_FILE"
    "DCENT_SYSUPGRADE_PROC_CMDLINE_PATH=$CMDLINE_FILE"
    "DCENT_SYSUPGRADE_BOARD_TARGET_PATH=$BOARD_FILE"
    "DCENT_SYSUPGRADE_VERSION_PATH=$VERSION_FILE"
)
if [ -n "$RELEASE_KEY" ]; then
    ENV_ARGS+=("DCENT_SYSUPGRADE_RELEASE_PUBKEY=$RELEASE_KEY")
fi
if [ "$ALLOW_LAB_PACKAGE" = "1" ]; then
    ENV_ARGS+=("DCENT_ALLOW_UNSIGNED_SYSUPGRADE=1" "DCENT_PACKAGE_STATUS=lab")
fi

env "${ENV_ARGS[@]}" sh "$SYSUPGRADE" -f "$PACKAGE"

ubiattach /dev/ubi_ctrl -m "$MTD_NUM" -d 1 >/dev/null
create_ubi_device_nodes
verify_written_volume kernel /dev/ubi1_0 "$PAYLOAD_SUBDIR/kernel" "$WORKDIR"
verify_written_volume rootfs /dev/ubi1_1 "$PAYLOAD_SUBDIR/root" "$WORKDIR"

FW=$(fw_printenv firmware 2>/dev/null | sed -n 's/^firmware=//p')
STAGE=$(fw_printenv upgrade_stage 2>/dev/null | sed -n 's/^upgrade_stage=//p')
[ "$FW" = "$EXPECTED_POSTFLIP_FIRMWARE" ] || die "fw_printenv firmware=$FW, expected $EXPECTED_POSTFLIP_FIRMWARE after flip"
[ "$STAGE" = "0" ] || die "fw_printenv upgrade_stage=$STAGE, expected 0 after flip"

ubidetach -d 1 >/dev/null 2>&1 || true
if [ "$DIRECTION" = "reverse" ]; then
    echo "OFFLINE_NANDSIM_PROOF_OK target=$TARGET direction=reverse current_fw=1 inactive_mtd=8 mtd=$MTD_NUM package=$PACKAGE"
else
    echo "OFFLINE_NANDSIM_PROOF_OK target=$TARGET mtd=$MTD_NUM package=$PACKAGE"
fi
