#!/usr/bin/env bash
#
# Offline S9 first-install stage1.sh write-path proof harness.
#
# This complements sysupgrade_offline_nandsim_harness.sh. It runs toolbox-
# generated stage1.sh scripts, not the on-device /usr/sbin/sysupgrade writer:
#   - braiinsos-am1-s9-ubi_replace
#   - stock-am1-s9-stock_phase1
#   - stock-am1-s9-stock_phase2
#
# Success prints OFFLINE_FIRST_INSTALL_PROOF_OK only after the generated
# stage1.sh has run against nandsim-backed MTD/UBI devices and this harness has
# read back the written bytes from those device nodes.
#
# This harness is intentionally S9-only. AM2 vendor-source first install has no
# authenticated, versioned capsule with source-runtime dependency closure and
# persistent-state migration. The AM2 on-device sysupgrade writer is covered by
# sysupgrade_offline_nandsim_harness.sh only as a DCENT_OS-source A/B update.

set -euo pipefail

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd)
PROJECT_DIR=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd)
ROOT_DIR=$(CDPATH='' cd "$PROJECT_DIR/../.." && pwd)
TOOLBOX_DIR="$ROOT_DIR/projects/dcent-toolbox"

TARGET=am1-s9
ROUTE=both
PACKAGE="$PROJECT_DIR/output/beta-xil-20260617/DCENTOS_XIL1_S9_beta20260617.tar"
BRAIINS_PACKAGE="$TOOLBOX_DIR/packages/braiins-os_am1-s9_ssh_22.08.1-plus.tar.gz"
WORKDIR=
PROBE_ONLY=0
REQUIRE_NANDSIM=${DCENT_REQUIRE_NANDSIM:-0}
PYTHON_BIN=${PYTHON:-python3}
NANDSIM_ID_BYTES=${DCENT_NANDSIM_ID_BYTES:-0x20,0xaa,0x00,0x15}
NANDSIM_OVERRIDESIZE=${DCENT_NANDSIM_OVERRIDESIZE:-11}
NANDSIM_ERASESIZE_HEX=00020000

# BraiinsOS S9 10-partition layout, in 128 KiB eraseblocks:
# 512k, 2560k, 2m, 2m, 512k, 512k, 22m, 95m, 95m, 36m.
BRAIINS_PARTS=${DCENT_STAGE1_BRAIINS_NANDSIM_PARTS:-4,20,16,16,4,4,176,760,760,288}
# Stock proof layout: boot chain (8 MiB), stock rootfs placeholder (20 MiB),
# kernel/rest. The stock_phase1 stage1 writes only mtd0 and mtd2.
STOCK_PARTS=${DCENT_STAGE1_STOCK_NANDSIM_PARTS:-64,160,1824}
DATA_TMPFS_MOUNTED=0

usage() {
    cat <<'EOF'
Usage: stage1_first_install_offline_nandsim_harness.sh --package TAR --workdir DIR [options]

Options:
  --target TARGET          am1-s9 (the only admitted first-install target)
  --route ROUTE            both, braiinsos-am1-s9-ubi_replace, stock-am1-s9-stock_phase1, or stock-am1-s9-stock_phase2
  --package TAR            Signed S9 beta sysupgrade tar
  --braiins-package TAR    Bundled BraiinsOS SSH package for stock_phase1
  --workdir DIR            Clean work directory
  --python PATH            Python executable used to call stage1_builder
  --probe-only             Only check kernel/tool availability
  --require-nandsim        Missing nandsim/UBI support exits 1 instead of 77
  -h, --help               Show this help
EOF
}

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
    [ "$(id -u)" = "0" ] || skip_unavailable "must run as root in a privileged disposable Linux VM"
}

load_module() {
    local module=$1
    if grep -q "^${module} " /proc/modules 2>/dev/null; then
        return 0
    fi
    modprobe "$module" >/tmp/dcent-stage1-nandsim-modprobe.out 2>&1 || {
        cat /tmp/dcent-stage1-nandsim-modprobe.out >&2 || true
        skip_unavailable "cannot load kernel module: $module"
    }
}

unload_nandsim() {
    local dev
    for dev in 1 0; do
        ubidetach -d "$dev" >/dev/null 2>&1 || true
    done
    modprobe -r nandsim >/dev/null 2>&1 || true
}

cleanup() {
    if [ "$DATA_TMPFS_MOUNTED" = "1" ]; then
        umount /data >/dev/null 2>&1 || true
    fi
    if command -v ubidetach >/dev/null 2>&1 && command -v modprobe >/dev/null 2>&1; then
        unload_nandsim
    fi
}
trap cleanup EXIT INT TERM

prepare_data_staging_root() {
    if [ ! -d /data ]; then
        mkdir -p /data 2>/dev/null || true
    fi
    if touch /data/.dcent_stage1_nandsim_probe 2>/dev/null; then
        rm -f /data/.dcent_stage1_nandsim_probe
        return 0
    fi
    mount -t tmpfs -o size=96m dcent-stage1-data /data >/tmp/dcent-stage1-data-mount.out 2>&1 || {
        cat /tmp/dcent-stage1-data-mount.out >&2 || true
        skip_unavailable "cannot provide writable /data staging root inside disposable VM"
    }
    DATA_TMPFS_MOUNTED=1
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
        skip_unavailable "nandsim must use 128KiB eraseblocks for S9 first-install proof"
    }
}

load_nandsim_parts() {
    local parts=$1
    unload_nandsim
    modprobe nandsim "id_bytes=$NANDSIM_ID_BYTES" "overridesize=$NANDSIM_OVERRIDESIZE" \
        "parts=$parts" \
        >/tmp/dcent-stage1-nandsim-modprobe.out 2>&1 || {
        cat /tmp/dcent-stage1-nandsim-modprobe.out >&2 || true
        skip_unavailable "cannot load kernel module: nandsim"
    }
    verify_nandsim_geometry
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

create_ubi_device_nodes() {
    local ubi_path name dev major minor
    for ubi_path in /sys/class/ubi/ubi*; do
        [ -d "$ubi_path" ] || continue
        name=$(basename "$ubi_path")
        dev=$(cat "$ubi_path/dev" 2>/dev/null || true)
        [ -n "$dev" ] || continue
        major=${dev%%:*}
        minor=${dev##*:}
        [ -e "/dev/$name" ] || mknod -m 660 "/dev/$name" c "$major" "$minor" 2>/dev/null || true
    done
}

ceil_div() {
    local value=$1 divisor=$2
    printf '%s\n' "$(( (value + divisor - 1) / divisor ))"
}

prepare_braiins_ubi0() {
    local rootfs_src=$1 leb_size rootfs_lebs rootfs_size
    require_nandsim_partition 4 00080000
    require_nandsim_partition 7 05f00000
    ubidetach -m 7 >/dev/null 2>&1 || true
    ubidetach -d 0 >/dev/null 2>&1 || true
    ubiformat /dev/mtd7 -y >/dev/null
    ubiattach /dev/ubi_ctrl -m 7 -d 0 >/dev/null
    leb_size=$(cat /sys/class/ubi/ubi0/eraseblock_size 2>/dev/null || true)
    case "$leb_size" in
        *[!0-9]*|"") skip_unavailable "cannot read UBI LEB size for first-install fixture" ;;
    esac
    rootfs_size=$(wc -c < "$rootfs_src" | tr -d '[:space:]')
    rootfs_lebs=$(ceil_div "$rootfs_size" "$leb_size")
    [ "$rootfs_lebs" -lt 160 ] && rootfs_lebs=160
    ubimkvol /dev/ubi0 -N kernel -s "$((64 * leb_size))" -t dynamic >/dev/null
    ubimkvol /dev/ubi0 -N rootfs -s "$((rootfs_lebs * leb_size))" -t dynamic >/dev/null
    ubimkvol /dev/ubi0 -N rootfs_data -s "$((32 * leb_size))" -t dynamic >/dev/null
    create_ubi_device_nodes
}

copy_staging() {
    local fixture=$1
    prepare_data_staging_root
    rm -rf /data/dcent_install
    mkdir -p /data/dcent_install
    cp "$fixture"/staging/* /data/dcent_install/
}

run_stage1() {
    local fixture=$1 log=$2
    copy_staging "$fixture"
    PATH="$PATH:/usr/sbin:/sbin" sh "$fixture/stage1.sh" >"$log" 2>&1 || {
        cat "$log" >&2 || true
        die "stage1.sh failed; log=$log"
    }
    grep -Fq "DCENT_STAGE:complete" "$log" || {
        cat "$log" >&2 || true
        die "stage1.sh did not reach complete stage; log=$log"
    }
    grep -E "DCENT_STOCK_PHASE[12]_READBACK_OK|DCENT_STAGE:complete" "$log" || true
}

verify_written_volume() {
    local route=$1 label=$2 dev=$3 source=$4 work=$5
    local size expected actual tmp blocks
    size=$(wc -c < "$source" | tr -d '[:space:]')
    expected=$(sha256sum "$source" | awk '{print $1}')
    tmp="$work/readback-$label.bin"
    blocks=$(( (size + 1048575) / 1048576 ))
    [ "$blocks" -gt 0 ] || blocks=1
    dd if="$dev" of="$tmp" bs=1048576 count="$blocks" >/dev/null 2>&1
    actual=$(head -c "$size" "$tmp" | sha256sum | awk '{print $1}')
    [ "$actual" = "$expected" ] || die "$label readback hash mismatch from $dev"
    echo "OFFLINE_FIRST_INSTALL_READBACK_OK target=$TARGET route=$route label=$label bytes=$size sha256=$actual device=$dev"
}

verify_mtd_region() {
    local route=$1 label=$2 mtd=$3 offset=$4 source=$5 work=$6
    local size expected actual tmp
    size=$(wc -c < "$source" | tr -d '[:space:]')
    expected=$(sha256sum "$source" | awk '{print $1}')
    tmp="$work/readback-$label.bin"
    nanddump -f "$tmp" --omitoob -s "$offset" -l "$size" "/dev/mtd$mtd" >/dev/null 2>&1
    actual=$(sha256sum "$tmp" | awk '{print $1}')
    [ "$actual" = "$expected" ] || die "$label readback hash mismatch from mtd$mtd offset=$offset"
    echo "OFFLINE_FIRST_INSTALL_READBACK_OK target=$TARGET route=$route label=$label bytes=$size sha256=$actual device=/dev/mtd$mtd offset=$offset"
}

generate_fixture() {
    local route=$1 fixture=$2
    mkdir -p "$fixture"
    ROUTE="$route" \
    FIXTURE_DIR="$fixture" \
    PACKAGE="$PACKAGE" \
    BRAIINS_PACKAGE="$BRAIINS_PACKAGE" \
    TOOLBOX_SRC="$TOOLBOX_DIR/src" \
    "$PYTHON_BIN" - <<'PY'
import hashlib
import os
import sys
from pathlib import Path

sys.path.insert(0, os.environ["TOOLBOX_SRC"])

from dcent_toolbox.core.braiins_installer import _extract_package
from dcent_toolbox.core.install_intent import build_install_intent
from dcent_toolbox.core.install_package import InstallPackage
from dcent_toolbox.core.stage1_builder import (
    InstallMethod,
    build_stage1,
    validate_stage1_safety,
)
from dcent_toolbox.core.uboot_env import build_install_env, build_stock_transition_env

route = os.environ["ROUTE"]
fixture = Path(os.environ["FIXTURE_DIR"])
staging = fixture / "staging"
expected = fixture / "expected"
staging.mkdir(parents=True, exist_ok=True)
expected.mkdir(parents=True, exist_ok=True)

package = InstallPackage.load(os.environ["PACKAGE"])

if route == "braiinsos-am1-s9-ubi_replace":
    install_intent = build_install_intent(
        install_origin="braiinsos",
        bootstrap_transport="ssh",
        install_method=InstallMethod.UBI_REPLACE.value,
        target_ip="offline-nandsim",
        model="Antminer S9",
        hostname="offline",
        mac="02:dc:00:00:00:09",
        hwid="02:dc:00:00:00:09",
        package_version=package.manifest.version,
        package_model=package.manifest.model,
        board_family=package.manifest.board_family,
        board_target=package.manifest.board_target,
        package_type=package.manifest.package_type,
    )
    script = build_stage1(
        method=InstallMethod.UBI_REPLACE,
        rootfs_sha256=package.rootfs_sha256,
        install_intent_sha256=hashlib.sha256(install_intent).hexdigest(),
        has_kernel=False,
        has_uboot_env=False,
        has_install_intent=True,
        board_family="am1",
    )
    violations = validate_stage1_safety(script, method=InstallMethod.UBI_REPLACE)
    if violations:
        raise SystemExit("stage1 safety violations: " + "; ".join(violations))
    (staging / "rootfs.squashfs").write_bytes(package.rootfs)
    (staging / "install_intent.json").write_bytes(install_intent)
    (expected / "rootfs.squashfs").write_bytes(package.rootfs)

elif route == "stock-am1-s9-stock_phase1":
    bos = _extract_package(Path(os.environ["BRAIINS_PACKAGE"]))
    names = ("boot.bin", "u-boot.img", "system.bit.gz", "fit.itb")
    env_bin = build_stock_transition_env(
        mac="02:dc:00:00:00:09",
        hwid="02:dc:00:00:00:09",
        miner_model="Antminer S9",
        bitstream_size=len(bos.firmware["system.bit.gz"]),
        kernel_size=len(bos.firmware["fit.itb"]),
    )
    files = {name: bos.firmware[name] for name in names}
    files["uboot_env.bin"] = env_bin
    boot_hashes = {name: hashlib.sha256(data).hexdigest() for name, data in files.items()}
    script = build_stage1(
        method=InstallMethod.STOCK_PHASE1,
        rootfs_sha256="",
        env_sha256=hashlib.sha256(env_bin).hexdigest(),
        has_uboot_env=True,
        boot_sha256s=boot_hashes,
        board_family="am1",
    )
    violations = validate_stage1_safety(script, method=InstallMethod.STOCK_PHASE1)
    if violations:
        raise SystemExit("stage1 safety violations: " + "; ".join(violations))
    for name, data in files.items():
        (staging / name).write_bytes(data)
        (expected / name).write_bytes(data)

elif route == "stock-am1-s9-stock_phase2":
    if package.kernel is None:
        raise SystemExit("signed S9 package is missing kernel payload")
    install_intent = build_install_intent(
        install_origin="stock",
        bootstrap_transport="ssh",
        install_method=InstallMethod.STOCK_PHASE2.value,
        target_ip="offline-nandsim",
        model="Antminer S9",
        hostname="offline",
        mac="02:dc:00:00:00:09",
        hwid="02:dc:00:00:00:09",
        package_version=package.manifest.version,
        package_model=package.manifest.model,
        board_family=package.manifest.board_family,
        board_target=package.manifest.board_target,
        package_type=package.manifest.package_type,
    )
    env_bin = build_install_env(
        firmware_slot=1,
        mac="02:dc:00:00:00:09",
        hwid="02:dc:00:00:00:09",
    )
    script = build_stage1(
        method=InstallMethod.STOCK_PHASE2,
        rootfs_sha256=package.rootfs_sha256,
        kernel_sha256=package.kernel_sha256,
        env_sha256=hashlib.sha256(env_bin).hexdigest(),
        install_intent_sha256=hashlib.sha256(install_intent).hexdigest(),
        has_kernel=True,
        has_uboot_env=True,
        has_install_intent=True,
        board_family="am1",
    )
    violations = validate_stage1_safety(script, method=InstallMethod.STOCK_PHASE2)
    if violations:
        raise SystemExit("stage1 safety violations: " + "; ".join(violations))
    files = {
        "rootfs.squashfs": package.rootfs,
        "kernel.bin": package.kernel,
        "uboot_env.bin": env_bin,
        "install_intent.json": install_intent,
    }
    for name, data in files.items():
        (staging / name).write_bytes(data)
        (expected / name).write_bytes(data)
else:
    raise SystemExit(f"unsupported route: {route}")

(fixture / "stage1.sh").write_text(script, encoding="utf-8")
(fixture / "stage1.sh").chmod(0o755)
(fixture / "fixture.sha256").write_text(
    "\n".join(
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(fixture)}"
        for path in sorted(staging.iterdir())
        if path.is_file()
    )
    + "\n",
    encoding="utf-8",
)
PY
}

run_ubi_replace() {
    local route fixture log
    route=braiinsos-am1-s9-ubi_replace
    fixture="$WORKDIR/$route"
    log="$WORKDIR/$route.log"
    generate_fixture "$route" "$fixture"
    load_nandsim_parts "$BRAIINS_PARTS"
    prepare_braiins_ubi0 "$fixture/expected/rootfs.squashfs"
    run_stage1 "$fixture" "$log"
    verify_written_volume "$route" rootfs /dev/ubi0_1 "$fixture/expected/rootfs.squashfs" "$fixture"
    echo "OFFLINE_FIRST_INSTALL_PROOF_OK target=am1-s9 route=$route"
}

run_stock_phase1() {
    local route fixture log
    route=stock-am1-s9-stock_phase1
    fixture="$WORKDIR/$route"
    log="$WORKDIR/$route.log"
    generate_fixture "$route" "$fixture"
    load_nandsim_parts "$STOCK_PARTS"
    require_nandsim_partition 0 00800000
    require_nandsim_partition 2 ""
    run_stage1 "$fixture" "$log"
    verify_mtd_region "$route" boot-bin 0 0x0 "$fixture/expected/boot.bin" "$fixture"
    verify_mtd_region "$route" u-boot-img 0 0x80000 "$fixture/expected/u-boot.img" "$fixture"
    verify_mtd_region "$route" system-bit-gz 0 0x300000 "$fixture/expected/system.bit.gz" "$fixture"
    verify_mtd_region "$route" env-primary 0 0x700000 "$fixture/expected/uboot_env.bin" "$fixture"
    verify_mtd_region "$route" env-redundant 0 0x720000 "$fixture/expected/uboot_env.bin" "$fixture"
    verify_mtd_region "$route" fit-itb 2 0x0 "$fixture/expected/fit.itb" "$fixture"
    echo "OFFLINE_FIRST_INSTALL_PROOF_OK target=am1-s9 route=$route"
}

run_stock_phase2() {
    local route fixture log
    route=stock-am1-s9-stock_phase2
    fixture="$WORKDIR/$route"
    log="$WORKDIR/$route.log"
    generate_fixture "$route" "$fixture"
    load_nandsim_parts "$BRAIINS_PARTS"
    require_nandsim_partition 4 00080000
    require_nandsim_partition 7 05f00000
    run_stage1 "$fixture" "$log"
    verify_written_volume "$route" kernel /dev/ubi0_0 "$fixture/expected/kernel.bin" "$fixture"
    verify_written_volume "$route" rootfs /dev/ubi0_1 "$fixture/expected/rootfs.squashfs" "$fixture"
    verify_mtd_region "$route" uboot-env 4 0x0 "$fixture/expected/uboot_env.bin" "$fixture"
    echo "OFFLINE_FIRST_INSTALL_PROOF_OK target=am1-s9 route=$route"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --target) TARGET=${2:-}; shift 2 ;;
        --target=*) TARGET=${1#--target=}; shift ;;
        --route) ROUTE=${2:-}; shift 2 ;;
        --route=*) ROUTE=${1#--route=}; shift ;;
        --package) PACKAGE=${2:-}; shift 2 ;;
        --package=*) PACKAGE=${1#--package=}; shift ;;
        --braiins-package) BRAIINS_PACKAGE=${2:-}; shift 2 ;;
        --braiins-package=*) BRAIINS_PACKAGE=${1#--braiins-package=}; shift ;;
        --workdir) WORKDIR=${2:-}; shift 2 ;;
        --workdir=*) WORKDIR=${1#--workdir=}; shift ;;
        --python) PYTHON_BIN=${2:-}; shift 2 ;;
        --python=*) PYTHON_BIN=${1#--python=}; shift ;;
        --probe-only) PROBE_ONLY=1; shift ;;
        --require-nandsim) REQUIRE_NANDSIM=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

[ "$TARGET" = "am1-s9" ] || die "unsupported --target '$TARGET' (expected am1-s9)"
case "$ROUTE" in
    both|braiinsos-am1-s9-ubi_replace|stock-am1-s9-stock_phase1|stock-am1-s9-stock_phase2) ;;
    *) die "unsupported --route '$ROUTE'" ;;
esac

probe_capabilities() {
    need_root
    for tool in awk basename cat chmod cp dd grep head id mkdir mknod modprobe mount rm sed sha256sum tar tr umount wc \
        flash_erase nanddump nandwrite ubidetach ubiformat ubiattach ubimkvol ubiupdatevol "$PYTHON_BIN"; do
        need_tool "$tool"
    done
    load_module ubi
    load_module ubifs
    load_nandsim_parts "$BRAIINS_PARTS"
}

probe_capabilities
if [ "$PROBE_ONLY" = "1" ]; then
    echo "FIRST_INSTALL_NANDSIM_PROBE_OK: kernel modules, userspace tools, and stage1 generator are available"
    exit 0
fi

[ -n "$WORKDIR" ] || die "--workdir is required"
if [ "${DCENT_SYSUPGRADE_OFFLINE_CONTAINER:-0}" != "1" ]; then
    skip_unavailable "refusing to mutate host /data and /dev directly; set DCENT_SYSUPGRADE_OFFLINE_CONTAINER=1 inside a disposable privileged Linux VM"
fi

rm -rf "$WORKDIR"
mkdir -p "$WORKDIR"

[ -f "$PACKAGE" ] || die "S9 package not found: $PACKAGE"
[ -f "$BRAIINS_PACKAGE" ] || die "BraiinsOS package not found: $BRAIINS_PACKAGE"

case "$ROUTE" in
    both)
        run_ubi_replace
        run_stock_phase1
        run_stock_phase2
        ;;
    braiinsos-am1-s9-ubi_replace)
        run_ubi_replace
        ;;
    stock-am1-s9-stock_phase1)
        run_stock_phase1
        ;;
    stock-am1-s9-stock_phase2)
        run_stock_phase2
        ;;
esac

unload_nandsim
