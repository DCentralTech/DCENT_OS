#!/usr/bin/env bash
#
# sd_common.sh — Shared helpers for DCENT_OS SD-card builder scripts.
#
# Extracted from the ~70% logic overlap across:
#   - build_am3_bb_sd_resident_uboot_image.sh   (AM3-BB resident-U-Boot SD)
#   - build_am3_bb_sd_vnish_bootbin_image.sh    (AM3-BB VNish boot.bin SD)
#   - build_am2_s19jpro_sd_disk_image.sh        (AM2-S19j Pro SD)
#
# Sourced from those builders. Each builder still owns its platform-specific
# logic (boot.bin path, kernel/DTB paths, uEnv.txt template, partition layout
# choices). This library only owns the genuinely shared mechanics.
#
# Public surface (functions and globals all use `sd_common` prefix / namespace):
#
#   Globals (read/write by caller):
#     SD_COMMON_CLEANUP_PATHS  — array of temp paths to rm -rf on EXIT
#     SD_COMMON_MTOOLS_SKIP_CHECK — exports MTOOLS_SKIP_CHECK=1 when set
#
#   Utility helpers:
#     sd_common::cleanup
#         Trap handler that wipes every path in SD_COMMON_CLEANUP_PATHS.
#         Caller installs the trap with `sd_common::install_cleanup_trap`.
#     sd_common::install_cleanup_trap
#         Installs trap on EXIT/INT/TERM that calls sd_common::cleanup.
#     sd_common::register_cleanup_path <path>
#         Appends <path> to SD_COMMON_CLEANUP_PATHS for later removal.
#     sd_common::need_tool <binary> <install-hint>
#         Errors out with a friendly install hint if <binary> is missing.
#     sd_common::total_bytes <file>
#         Echoes file size in bytes (0 for missing files). Cross-platform
#         stat invocation (Linux + macOS).
#     sd_common::sha256_file <file>
#         Echoes hex SHA256 of <file>. Uses sha256sum.
#     sd_common::refuse_block_device <path>
#         Exits with an error if <path> starts with /dev/ or \\.\ (Windows
#         device namespace). Used to safety-check the output directory.
#
#   FAT label handling:
#     sd_common::validate_fat_label <label-var-name>
#         Upcases the FAT label in-place and validates 1-11 chars, A-Z 0-9 _ -.
#         Pass the variable NAME, not its value (Bash nameref via printf -v).
#
#   Host tool batch:
#     sd_common::check_host_tools_basic
#         Checks for sfdisk, mkfs.vfat, mcopy, mdir, mtype, mkimage, file,
#         gzip, cpio, sha256sum. Builders that don't need all of them (e.g.
#         am2 doesn't strictly need mkimage if it doesn't wrap an initramfs)
#         can call need_tool individually.
#
#   Partition / filesystem creation:
#     sd_common::create_blank_image <out-img> <size-mb>
#         Writes a zeroed .img of the given size in MiB.
#     sd_common::write_mbr_p1_sector_aligned <img> <p1_start_sector> \
#                                            <p1_size_sectors> <type-hex>
#         Single-partition MBR with the partition bootable. Used by AM3-BB
#         (start_sector=1, FAT16 type 0x0c).
#     sd_common::write_mbr_two_part <img> <p1_off_mb> <p1_size_mb> \
#                                   <p1_type> <p2_off_mb> <p2_type>
#         Two-partition MBR with p1 bootable. Used by AM2-S19j Pro (FAT16 +
#         ext2 layout). p1_size given in MB; p2 extends to end of disk.
#     sd_common::format_fat16_partition <partition-img> <label>
#         mkfs.vfat -F 16 -n <label> <partition-img>.
#     sd_common::format_fat32_partition <partition-img> <label>
#         mkfs.vfat -F 32 -n <label> <partition-img>.
#     sd_common::copy_files_to_fat <fat-image> [<src>:<name> ...]
#         Calls mcopy for each src:name pair. Skips entries whose <src> is
#         empty or doesn't exist. Echoes "  copied <name> (<bytes> bytes)"
#         for visibility. MTOOLS_SKIP_CHECK is exported beforehand.
#     sd_common::dd_extract_partition <img> <out> <skip-sectors> <count-sectors>
#         Extracts a partition slice from <img> into <out>, sector-aligned.
#     sd_common::dd_write_partition <partition-img> <img> <seek-sectors>
#         Writes <partition-img> back into <img> at the given sector offset
#         with conv=notrunc.
#
#   Manifest emission:
#     sd_common::emit_manifest_json <out-path> <json-body>
#         Writes <json-body> verbatim to <out-path>. Pure wrapper; callers
#         build the JSON inline because per-platform fields vary.
#     sd_common::sha256_artifact <file>
#         Synonym for sd_common::sha256_file; included so manifest callers
#         have a clearly-named one-liner alongside emit_manifest_json.
#
#   Squashfs-root partition support (SD_COMMON_VERSION >= 2):
#     sd_common::verify_squashfs_file <file>
#         Errors out unless <file> starts with the squashfs v4 LE magic
#         ("hsqs"). Guards against staging a non-squashfs blob as p2.
#     sd_common::write_squashfs_root_partition <img> <squashfs> <off-mb> <size-mb>
#         Writes <squashfs> RAW into <img> at the given MiB offset after
#         verifying the magic and that it fits inside the partition. This is
#         the PROVEN DCENT_OS boot model (squashfs IS the root, mounted ro by
#         the kernel — same model .25/.109 run from NAND). Pair with bootargs
#         `root=/dev/mmcblk0p2 rootfstype=squashfs ro rootwait`.
#     sd_common::write_mbr_three_part_mb <img> <p1_off> <p1_size> <p1_type> \
#                                        <p2_off> <p2_size> <p2_type> \
#                                        <p3_off> <p3_size> <p3_type>
#         Three-partition MBR, MiB units, p1 bootable, all sizes explicit.
#         Used by AM2 squashfs-root layout (FAT16 boot + raw squashfs
#         root + small ext2 /data).
#
# Versioning: increment SD_COMMON_VERSION when the public surface changes.

SD_COMMON_VERSION=2

# Guard against double-sourcing. Builders typically `source` once; harmless
# repeated sourcing is supported via this idempotency check.
if [ "${SD_COMMON_SOURCED:-0}" = "1" ]; then
    return 0 2>/dev/null || true
fi
SD_COMMON_SOURCED=1

# Module-owned globals. Callers may read SD_COMMON_CLEANUP_PATHS directly but
# should append via sd_common::register_cleanup_path so the array is honored.
SD_COMMON_CLEANUP_PATHS=()

# ---------------------------------------------------------------------------
# Cleanup / trap
# ---------------------------------------------------------------------------
sd_common::cleanup() {
    local p
    for p in "${SD_COMMON_CLEANUP_PATHS[@]:-}"; do
        [ -n "$p" ] && rm -rf "$p"
    done
}

sd_common::install_cleanup_trap() {
    trap sd_common::cleanup EXIT INT TERM
}

sd_common::register_cleanup_path() {
    SD_COMMON_CLEANUP_PATHS+=("$1")
}

# ---------------------------------------------------------------------------
# Utility helpers
# ---------------------------------------------------------------------------
sd_common::need_tool() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "ERROR: missing host tool: $1" >&2
        echo "       Install with: $2" >&2
        exit 1
    }
}

sd_common::total_bytes() {
    if [ -f "$1" ]; then
        stat -c%s "$1" 2>/dev/null || stat -f%z "$1"
    else
        echo 0
    fi
}

sd_common::sha256_file() {
    sha256sum "$1" | awk '{print $1}'
}

# Synonym for callers that want a clearly-named "artifact hash" in manifest
# contexts. Identical behavior to sd_common::sha256_file.
sd_common::sha256_artifact() {
    sd_common::sha256_file "$1"
}

sd_common::refuse_block_device() {
    case "$1" in
        /dev/*|\\\\.\\*)
            echo "ERROR: refusing to write to a block device path: $1" >&2
            exit 1
            ;;
    esac
}

# ---------------------------------------------------------------------------
# FAT label handling
# ---------------------------------------------------------------------------
# Upcases and validates a FAT label in place. Pass the NAME of the variable
# holding the label (not the value): `sd_common::validate_fat_label BOOT_LABEL`.
sd_common::validate_fat_label() {
    local var_name="$1"
    local current
    eval "current=\${$var_name}"
    current="$(printf "%s" "$current" | tr '[:lower:]' '[:upper:]')"
    case "$current" in
        ""|*[!A-Z0-9_-]*)
            echo "ERROR: $var_name must be 1-11 chars using A-Z, 0-9, _ or -" >&2
            exit 1
            ;;
    esac
    if [ "${#current}" -gt 11 ]; then
        echo "ERROR: $var_name is too long for FAT: ${#current} > 11" >&2
        exit 1
    fi
    eval "$var_name=\$current"
}

# ---------------------------------------------------------------------------
# Host tool batch
# ---------------------------------------------------------------------------
sd_common::check_host_tools_basic() {
    sd_common::need_tool sfdisk     "sudo apt install util-linux"
    sd_common::need_tool mkfs.vfat  "sudo apt install dosfstools"
    sd_common::need_tool mcopy      "sudo apt install mtools"
    sd_common::need_tool mdir       "sudo apt install mtools"
    sd_common::need_tool mtype      "sudo apt install mtools"
    sd_common::need_tool mkimage    "sudo apt install u-boot-tools"
    sd_common::need_tool file       "sudo apt install file"
    sd_common::need_tool gzip       "sudo apt install gzip"
    sd_common::need_tool cpio       "sudo apt install cpio"
    sd_common::need_tool sha256sum  "sudo apt install coreutils"
}

# ---------------------------------------------------------------------------
# Image / partition creation
# ---------------------------------------------------------------------------
sd_common::create_blank_image() {
    local out_img="$1"
    local size_mb="$2"
    dd if=/dev/zero of="$out_img" bs=1M count="$size_mb" status=none
}

# Sector-precise blank image creator. Used by the AM3-BB VNish boot.bin
# builder where the canonical VNish layout is 102,401 sectors (50 MiB + 1
# sector), not a whole-MiB count. Writes the full image as zeros at the
# given sector count.
sd_common::create_blank_image_sectors() {
    local out_img="$1"
    local sectors="$2"
    # Allocate as zero. bs=512 keeps it simple; size-512 image fits in
    # under a second on any host.
    dd if=/dev/zero of="$out_img" bs=512 count="$sectors" status=none
}

# Single-partition sector-aligned MBR (used by AM3-BB at LBA 1, FAT16
# type 0x0c, bootable). Type is provided in hex form WITHOUT the 0x prefix,
# e.g. "0c" for FAT16 LBA, "0e" for FAT16 LBA (mixed BIOS quirks).
sd_common::write_mbr_p1_sector_aligned() {
    local img="$1"
    local p1_start_sectors="$2"
    local p1_size_sectors="$3"
    local p1_type="$4"
    sfdisk "$img" >/dev/null <<EOF
label: dos
label-id: 0x00000000
unit: sectors

start=${p1_start_sectors}, size=${p1_size_sectors}, type=${p1_type}, bootable
EOF
}

# Two-partition MBR (used by AM2-S19j Pro: FAT16 boot + ext2 rootfs).
# Offsets in MiB; p1 size in MiB; p2 extends to the end of the disk.
# Types are hex WITHOUT 0x prefix (e.g. "0e" + "83").
sd_common::write_mbr_two_part() {
    local img="$1"
    local p1_off_mb="$2"
    local p1_size_mb="$3"
    local p1_type="$4"
    local p2_off_mb="$5"
    local p2_type="$6"
    # 2048 sectors/MiB at 512B sector size.
    local p1_start_sectors=$((p1_off_mb * 2048))
    local p1_size_sectors=$((p1_size_mb * 2048))
    local p2_start_sectors=$((p2_off_mb * 2048))
    sfdisk "$img" >/dev/null <<EOF
label: dos
unit: sectors

start=${p1_start_sectors}, size=${p1_size_sectors}, type=${p1_type}, bootable
start=${p2_start_sectors}, type=${p2_type}
EOF
}

# Three-partition MBR with sector-aligned p1 (used by AM3-BB VNish boot.bin
# variant: FAT16 ANTHILLOS at LBA 1 + 2× linux scratch). All sizes/offsets
# are in absolute sectors (512-byte) to mirror the VNish v1.2.6 layout
# byte-exactly (P1 start=1 sector, NOT 1 MiB). Types are hex WITHOUT 0x
# prefix. Only p1 is bootable.
sd_common::write_mbr_three_part_sector_aligned() {
    local img="$1"
    local p1_start_sectors="$2"
    local p1_size_sectors="$3"
    local p1_type="$4"
    local p2_start_sectors="$5"
    local p2_size_sectors="$6"
    local p2_type="$7"
    local p3_start_sectors="$8"
    local p3_size_sectors="$9"
    local p3_type="${10}"
    sfdisk "$img" >/dev/null <<EOF
label: dos
unit: sectors

start=${p1_start_sectors}, size=${p1_size_sectors}, type=${p1_type}, bootable
start=${p2_start_sectors}, size=${p2_size_sectors}, type=${p2_type}
start=${p3_start_sectors}, size=${p3_size_sectors}, type=${p3_type}
EOF
}

# Three-partition MBR in MiB units (used by the AM2 squashfs-root layout:
# FAT16 boot + RAW squashfs root + small ext2 /data). p1 bootable; all
# sizes explicit so partition entries match the staged filesystems exactly.
# Types are hex WITHOUT 0x prefix (e.g. "0e" + "83" + "83").
sd_common::write_mbr_three_part_mb() {
    local img="$1"
    local p1_off_mb="$2"
    local p1_size_mb="$3"
    local p1_type="$4"
    local p2_off_mb="$5"
    local p2_size_mb="$6"
    local p2_type="$7"
    local p3_off_mb="$8"
    local p3_size_mb="$9"
    local p3_type="${10}"
    # 2048 sectors/MiB at 512B sector size.
    sfdisk "$img" >/dev/null <<EOF
label: dos
unit: sectors

start=$((p1_off_mb * 2048)), size=$((p1_size_mb * 2048)), type=${p1_type}, bootable
start=$((p2_off_mb * 2048)), size=$((p2_size_mb * 2048)), type=${p2_type}
start=$((p3_off_mb * 2048)), size=$((p3_size_mb * 2048)), type=${p3_type}
EOF
}

# Fails unless <file> begins with the squashfs v4 little-endian magic
# "hsqs" (68 73 71 73). Guards the p2 staging path against writing a
# non-squashfs blob (e.g. an ext2 image or a tarball) as the root partition.
sd_common::verify_squashfs_file() {
    local f="$1"
    local magic
    magic="$(dd if="$f" bs=4 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')"
    if [ "$magic" != "68737173" ]; then
        echo "ERROR: $f is not a squashfs v4 image (magic=$magic, want 68737173 'hsqs')" >&2
        echo "       Refusing to stage it as the raw root partition." >&2
        exit 1
    fi
}

# Writes <squashfs> RAW into <img> at <off-mb>, bounds-checked against
# <size-mb>. This is the PROVEN DCENT_OS boot model: the squashfs IS the
# root filesystem, mounted read-only by the kernel
# (root=/dev/mmcblk0p2 rootfstype=squashfs ro rootwait) — exactly how
# .25/.109 boot DCENT_OS from NAND (see
# br2_external_dcentos/board/zynq/rootfs-overlay/etc/dcentos-early-init.sh:12).
# NEVER wrap the squashfs inside an ext2 filesystem as a file: the kernel
# would mount the ext2 (which has no /sbin/init) and panic with "No init
# found" (the 2026-06-10 SD boot defect).
sd_common::write_squashfs_root_partition() {
    local img="$1"
    local squashfs="$2"
    local off_mb="$3"
    local size_mb="$4"
    [ -f "$squashfs" ] || {
        echo "ERROR: squashfs not found: $squashfs" >&2
        exit 1
    }
    sd_common::verify_squashfs_file "$squashfs"
    local sq_bytes part_bytes
    sq_bytes=$(sd_common::total_bytes "$squashfs")
    part_bytes=$((size_mb * 1024 * 1024))
    if [ "$sq_bytes" -gt "$part_bytes" ]; then
        echo "ERROR: squashfs ($sq_bytes bytes) exceeds partition (${size_mb} MiB)" >&2
        exit 1
    fi
    dd if="$squashfs" of="$img" bs=1M seek="$off_mb" conv=notrunc status=none
    echo "  wrote raw squashfs root partition at ${off_mb} MiB ($sq_bytes bytes)"
}

sd_common::format_fat16_partition() {
    local part_img="$1"
    local label="$2"
    mkfs.vfat -F 16 -n "$label" "$part_img" >/dev/null
}

sd_common::format_fat32_partition() {
    local part_img="$1"
    local label="$2"
    mkfs.vfat -F 32 -n "$label" "$part_img" >/dev/null
}

sd_common::dd_extract_partition() {
    local img="$1"
    local out="$2"
    local skip_sectors="$3"
    local count_sectors="$4"
    dd if="$img" of="$out" bs=512 skip="$skip_sectors" count="$count_sectors" status=none
}

sd_common::dd_write_partition() {
    local part_img="$1"
    local img="$2"
    local seek_sectors="$3"
    dd if="$part_img" of="$img" bs=512 seek="$seek_sectors" conv=notrunc status=none
}

# ---------------------------------------------------------------------------
# File copy into FAT image
# ---------------------------------------------------------------------------
# Copies one (src, name) pair into a FAT image via mcopy. Skips missing or
# empty <src>. Echoes a "copied <name> (<bytes>)" line for parity with the
# original builders' verbose output.
sd_common::copy_one_to_fat() {
    local image="$1"
    local src="$2"
    local name="$3"
    [ -n "$src" ] || return 0
    [ -f "$src" ] || return 0
    mcopy -i "$image" "$src" "::$name"
    echo "  copied $name ($(sd_common::total_bytes "$src") bytes)"
}

# Bulk copy: builders pass "<src>:<name>" pairs. The function is a thin
# convenience wrapper; for one-off copies builders may call copy_one_to_fat
# directly.
sd_common::copy_files_to_fat() {
    local image="$1"
    shift
    export MTOOLS_SKIP_CHECK=1
    local pair src name
    for pair in "$@"; do
        src="${pair%%:*}"
        name="${pair#*:}"
        sd_common::copy_one_to_fat "$image" "$src" "$name"
    done
}

# ---------------------------------------------------------------------------
# Manifest emission
# ---------------------------------------------------------------------------
# Writes <json-body> verbatim to <out-path>. Pure wrapper — callers build
# the JSON inline because per-platform fields vary. Intentionally minimal
# so the library doesn't grow opinions about manifest schemas.
sd_common::emit_manifest_json() {
    local out_path="$1"
    local json_body="$2"
    printf "%s\n" "$json_body" > "$out_path"
}
