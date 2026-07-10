#!/bin/bash
#
# build_sd_s19pro.sh — Build DCENT_OS lab SD boot image for Antminer S19 Pro (am2-s17)
# D-Central Technologies, 2026
#
# Creates a bootable SD card for AM2 lab bring-up and SD boot validation.
# It does NOT imply a safe public NAND install path for S19 Pro.
# Uses BraiinsOS boot chain (FSBL, U-Boot, FPGA, kernel) + DCENT_OS rootfs.
#
# *** 2026-06-10 SD BOOT DEFECT FIX (squashfs-root model) ***
# The previous revision booted the squashfs as a U-Boot ramdisk
# (root=/dev/ram0 ramdisk_size=64M rootfstype=squashfs + uInitrd) — an
# UNPROVEN model that additionally depends on CONFIG_BLK_DEV_RAM in the
# BraiinsOS kernel. The PROVEN DCENT_OS runtime model (.25/.109/.129 from
# NAND) is: the squashfs IS the root partition, mounted read-only by the
# kernel. This builder now writes the squashfs RAW as partition 2 and boots
# root=/dev/mmcblk0p2 rootfstype=squashfs ro rootwait with no ramdisk
# (`bootm kernel - fdt`). See
#
#
# Usage: runs in WSL (sudo) OR Docker-as-root (debian:bookworm-slim, no loop
#   devices). Environment-agnostic: paths auto-derive from the script location,
#   sudo is a no-op when already root, and all FAT work uses mtools (no loop
#   devices / no mount) — the same portability fix applied to build_sd_image.sh.
#
set -euo pipefail

# Auto-derive the repo root from this script's location (scripts/ -> dcentos -> projects -> ROOT),
# so it works under WSL (/mnt/c/...) AND Docker (/work) without a hardcoded path.
PROJ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
# sudo only when NOT already root (Docker-as-root has no sudo and needs none).
SUDO="sudo"; [ "$(id -u)" = "0" ] && SUDO=""
# Writable work dir: WSL $HOME if usable, else /tmp (Docker).
WORKDIR="${WORKDIR:-${HOME:-/tmp}/dcentos_sd_s19pro}"; case "$WORKDIR" in /root/*|/home/*) [ -w "$(dirname "$WORKDIR")" ] || WORKDIR=/tmp/dcentos_sd_s19pro;; esac
BRAIINS_IMG="$PROJ/knowledge-base/firmware-archive/braiins-os_am2-s17_sd.img"
DCENTOS_ROOTFS="$PROJ/DCENT_OS_Antminer/dcentos_rootfs.squashfs"
NEW_BINARY="$PROJ/DCENT_OS_Antminer/dcentrald/target/armv7-unknown-linux-musleabihf/release/dcentrald"
S19PRO_TOML="$PROJ/DCENT_OS_Antminer/dcentrald/dcentrald-s19pro.toml"
OVERLAY="$PROJ/DCENT_OS_Antminer/br2_external_dcentos/board/zynq/rootfs-overlay"
SD_IMAGE="$PROJ/DCENT_OS_Antminer/output/dcentos-s19pro-sd.img"
export MTOOLS_SKIP_CHECK=1

# Shared SD helpers (squashfs-root partition writer + magic check).
# shellcheck source=lib/sd_common.sh
. "$PROJ/DCENT_OS_Antminer/scripts/lib/sd_common.sh"

$SUDO rm -rf "$WORKDIR"
mkdir -p "$WORKDIR"/boot

# ============================================================
echo "=== Step 1: Extract FAT32 boot partition from BraiinsOS SD image (mtools, no loop) ==="
dd if="$BRAIINS_IMG" of="$WORKDIR/fat32.img" bs=512 skip=2048 count=81920 2>/dev/null
# mcopy all root files out of the FAT image — no mount / no loop device needed.
mcopy -i "$WORKDIR/fat32.img" -s -n "::*" "$WORKDIR/boot/"
for required in boot.bin u-boot.img system.bit.gz system_bm.bit.gz miner.btm miner.btm.sig fit.itb; do
    [ -f "$WORKDIR/boot/$required" ] || { echo "ERROR: Missing required boot artifact: $required"; exit 1; }
done
echo "  Boot files: $(ls "$WORKDIR/boot/")"

# ============================================================
echo ""
echo "=== Step 2: Extract kernel + DTB from FIT image ==="
python3 "$PROJ/DCENT_OS_Antminer/scripts/extract_fit.py" \
    "$WORKDIR/boot/fit.itb" "$WORKDIR"

ls -la "$WORKDIR/kernel.bin" "$WORKDIR/fdt.dtb"

# ============================================================
echo ""
echo "=== Step 3: Build DCENT_OS rootfs with new binary + fixes ==="

# Unsquash existing rootfs
$SUDO unsquashfs -d "$WORKDIR/rootfs_new" "$DCENTOS_ROOTFS"

# Replace dcentrald with a freshly cross-compiled binary IF present; otherwise keep
# the rootfs's existing dcentrald (lets the SD build + structural validation run
# without a fresh armv7 build on hand).
if [ -f "$NEW_BINARY" ]; then
    $SUDO cp "$NEW_BINARY" "$WORKDIR/rootfs_new/usr/local/bin/dcentrald"
    $SUDO chmod 755 "$WORKDIR/rootfs_new/usr/local/bin/dcentrald"
    echo "  Replaced dcentrald binary ($(stat -c%s "$NEW_BINARY") bytes)"
else
    echo "  [WARN] ============================================================"
    echo "  [WARN] $NEW_BINARY not found."
    echo "  [WARN] Keeping the shared base rootfs's EXISTING dcentrald, which may be"
    echo "  [WARN] MONTHS OLD (pass-2 NEW-1: Mar-2026 v0.8.0, predating .25/safety work)."
    echo "  [WARN] For a PRODUCTION flash, cross-compile dcentrald first so the card"
    echo "  [WARN] ships the CURRENT daemon, not stale firmware."
    echo "  [WARN] ============================================================"
fi

# Copy S19 Pro config as default
[ -f "$S19PRO_TOML" ] && { $SUDO cp "$S19PRO_TOML" "$WORKDIR/rootfs_new/etc/dcentrald.toml"; echo "  Installed S19 Pro config"; }

# Fix dropbear — enable password auth
[ -f "$OVERLAY/etc/default/dropbear" ] && { $SUDO cp "$OVERLAY/etc/default/dropbear" "$WORKDIR/rootfs_new/etc/default/dropbear"; echo "  Fixed dropbear config (password auth enabled)"; }

# Copy updated init scripts (am2-s17 platform detection)
for s in S10modules S15pic_boot S82dcentrald; do
    [ -f "$OVERLAY/etc/init.d/$s" ] && $SUDO cp "$OVERLAY/etc/init.d/$s" "$WORKDIR/rootfs_new/etc/init.d/$s"
done
echo "  Updated init scripts (am2-s17 platform detection)"

# CRITICAL (pass-2 fix P2-2): bake the am2 platform STAMPS into the rootfs. The
# shared dcentos_rootfs.squashfs base lacks them, so S82dcentrald (which keys
# IS_AM2 ONLY off these files) would land IS_AM2=0 on real am2-s17 silicon ->
# the am2 UIO-mmap persistent fan custodian is never used, falling back to the
# unreliable devmem fan path. The kernel-cmdline dcent.platform= is NOT parsed
# into these, so the stamp MUST be baked here (not passed via bootargs).
$SUDO mkdir -p "$WORKDIR/rootfs_new/etc/dcentos"
echo "zynq-bm3-am2" | $SUDO tee "$WORKDIR/rootfs_new/etc/dcentos/platform" >/dev/null
echo "am2-s17"      | $SUDO tee "$WORKDIR/rootfs_new/etc/dcentos/board_target" >/dev/null
echo "  Baked am2 platform stamps: platform=zynq-bm3-am2 board_target=am2-s17 (IS_AM2=1)"

# CRITICAL (pass-3 fix NEW-5): arch-guard the rootfs before re-squashing. The
# historical AArch64-init-in-ARMv7 PID-1 brick (PROJECT_LOG: shipped BB card
# looped every ~10s) is exactly this class — a stale cross-arch Buildroot output
# leaking an aarch64 /sbin/init into an armv7 card. Hard-fail on any non-ARMv7 PID1.
# shellcheck source=lib/buildroot_rootfs_arch_guard.sh
. "$PROJ/DCENT_OS_Antminer/scripts/lib/buildroot_rootfs_arch_guard.sh"
dcent_require_armv7_eabi_elf_paths "$WORKDIR/rootfs_new" "S19Pro rootfs" \
    sbin/init bin/busybox usr/local/bin/dcentrald
echo "  Arch guard OK: /sbin/init + busybox + dcentrald are ARMv7 EABI ELF"

# Build new squashfs
$SUDO rm -f "$WORKDIR/rootfs_dcentos.squashfs"
$SUDO mksquashfs "$WORKDIR/rootfs_new" "$WORKDIR/rootfs_dcentos.squashfs" \
    -comp xz -b 262144 -no-xattrs -noappend
echo "  New rootfs: $(stat -c%s "$WORKDIR/rootfs_dcentos.squashfs") bytes"

# ============================================================
echo ""
echo "=== Step 4: Wrap kernel as uImage (no ramdisk — squashfs is the root partition) ==="

cd "$WORKDIR"

# Wrap kernel as uImage (legacy format that old U-Boot understands)
mkimage -A arm -O linux -T kernel -C none \
    -a 0x00008000 -e 0x00008000 \
    -n "DCENT_OS Linux 4.4.92" \
    -d kernel.bin uImage
echo "  uImage: $(stat -c%s uImage) bytes"

# NOTE: no uInitrd / FIT ramdisk wrap anymore. The squashfs is written RAW
# as partition 2 and the kernel mounts it directly as the read-only root
# (the proven .25/.109 NAND runtime model).

# ============================================================
echo ""
echo "=== Step 5: Build SD card image (p1 FAT32 boot + p2 RAW squashfs root) ==="

# Layout: p1 = FAT32 boot files @ 1 MiB; p2 = RAW DCENT_OS root squashfs.
# The squashfs IS the root partition (proven .25/.109 NAND runtime model):
# the kernel mounts /dev/mmcblk0p2 read-only as squashfs. No ramdisk.
BOOT_SIZE_MB=96
P1_OFFSET_MB=1
P2_OFFSET_MB=$((P1_OFFSET_MB + BOOT_SIZE_MB))
SQUASHFS_BYTES=$(stat -c%s "$WORKDIR/rootfs_dcentos.squashfs")
P2_SIZE_MB=$(( (SQUASHFS_BYTES + 1048575) / 1048576 + 2 ))
TOTAL_SIZE_MB=$((P2_OFFSET_MB + P2_SIZE_MB + 1))

# Create empty image
dd if=/dev/zero of="$SD_IMAGE" bs=1M count=$TOTAL_SIZE_MB 2>/dev/null

# Create partition table: p1 FAT32 LBA (0x0c) bootable + p2 Linux (0x83).
sfdisk "$SD_IMAGE" >/dev/null << EOF
label: dos
unit: sectors

start=$((P1_OFFSET_MB * 2048)), size=$((BOOT_SIZE_MB * 2048)), type=c, bootable
start=$((P2_OFFSET_MB * 2048)), size=$((P2_SIZE_MB * 2048)), type=83
EOF

# Format + fill the FAT32 boot partition with mtools (NO loop device / NO mount --
# the portable method that works in Docker-as-root and WSL alike). Build it in a
# standalone temp image, then dd it into p1 of the SD image.
BOOTPART="$WORKDIR/bootpart.fat"
dd if=/dev/zero of="$BOOTPART" bs=1M count=$BOOT_SIZE_MB 2>/dev/null
mkfs.vfat -F 32 -n DCENTOS "$BOOTPART" >/dev/null 2>&1

cp "$WORKDIR/fdt.dtb" "$WORKDIR/devicetree.dtb"

# Write DCENT_OS uEnv.txt that boots our kernel + p2 squashfs root from SD
# The BraiinsOS U-Boot loads u-boot.img which then reads uEnv.txt
cat > "$WORKDIR/uEnv.txt" << 'UENV'
# DCENT_OS lab SD boot for S19 Pro (am2-s17)
# FIX (2026-06-10, SD boot defect session): squashfs IS the root partition
# (root=/dev/mmcblk0p2 rootfstype=squashfs ro), matching the proven
# .25/.109 NAND runtime model. No ramdisk (`bootm kernel - fdt`).
#
# Boot model (pass-2 correction M-2): the SD's OWN BraiinsOS BOOT.BIN (SPL) loads the
# BraiinsOS 2016.03 u-boot.img directly, whose `sdboot` runs `sd_uenvcmd` — there is NO
# stock-NAND chain-load and NO `uenvcmd` execution. All boot logic lives in sd_uenvcmd.

# CRITICAL (adversarial-pass fix 2026-06-10): a TOP-LEVEL bootargs= MUST be set
# (parity with build_am2_s19jpro_sd_disk_image.sh:512). BraiinsOS `sdboot` does
# `test -n ${bootargs} || setenv bootargs ...root=/dev/ram0...`; WITHOUT this line,
# if any clause inside sd_uenvcmd fails the kernel boots the BOS root=/dev/ram0
# default and panics. With it, the squashfs-root bootargs survive regardless.
bootargs=mem=228M console=ttyPS0,115200 root=/dev/mmcblk0p2 rootfstype=squashfs ro rootwait earlyprintk

# Memory addresses
bm_kernel_addr=0x2000000
bm_devicetree_addr=0x3000000

# (no `uenvcmd`: the resident BraiinsOS 2016.03 U-Boot runs `sd_uenvcmd`, never `uenvcmd`.
#  The old stage-1 `go 0x4000000` chain-load was DEAD code AND self-referential — re-entering
#  the running U-Boot, a latent boot-loop if ever executed. Removed, pass-2 M-2.)

# FPGA bitstream load (explicit addresses, unzip before fpga loadb)
bm_bitstream_load_addr=0x1000000
bm_bitstream_addr=0x1800000
bm_load_bitstream=load mmc 0 ${bm_bitstream_load_addr} system.bit.gz && unzip ${bm_bitstream_load_addr} ${bm_bitstream_addr} && fpga loadb 0 ${bm_bitstream_addr} ${filesize}

# Boot args: RAW squashfs root on p2, mounted read-only by the kernel
bm_set_bootargs=setenv bootargs mem=228M console=ttyPS0,115200 root=/dev/mmcblk0p2 rootfstype=squashfs ro rootwait earlyprintk

# Stage 2: Load DCENT_OS from SD (overrides BraiinsOS default NAND boot)
bm_load_sd_images=fatload mmc 0 ${bm_kernel_addr} uImage && fatload mmc 0 ${bm_devicetree_addr} devicetree.dtb
sd_uenvcmd=echo === DCENT_OS Booting from SD ===; if run bm_load_bitstream; then echo FPGA programmed; else echo FPGA load failed - booting anyway; fi; run bm_load_sd_images && run bm_set_bootargs && bootm ${bm_kernel_addr} - ${bm_devicetree_addr}
UENV

# Copy the boot chain + DCENT_OS kernel/DTB/uEnv into the FAT (mcopy, no mount).
for f in boot.bin u-boot.img system.bit.gz system_bm.bit.gz miner.btm miner.btm.sig; do
    mcopy -i "$BOOTPART" -o "$WORKDIR/boot/$f" "::$f"
done
mcopy -i "$BOOTPART" -o "$WORKDIR/uImage" "::uImage"
mcopy -i "$BOOTPART" -o "$WORKDIR/devicetree.dtb" "::devicetree.dtb"
mcopy -i "$BOOTPART" -o "$WORKDIR/uEnv.txt" "::uEnv.txt"
echo "  SD boot partition contents:"
mdir -i "$BOOTPART" :: 2>/dev/null | tail -n +4

# dd the formatted+filled FAT into p1 of the SD image (p1 starts at P1_OFFSET_MB).
dd if="$BOOTPART" of="$SD_IMAGE" bs=1M seek=$P1_OFFSET_MB conv=notrunc 2>/dev/null

# Write the DCENT_OS root squashfs RAW into partition 2 (magic-verified,
# bounds-checked). NEVER stage it as a file inside a filesystem — that was
# the "No init found" boot-loop defect.
sd_common::write_squashfs_root_partition "$SD_IMAGE" "$WORKDIR/rootfs_dcentos.squashfs" "$P2_OFFSET_MB" "$P2_SIZE_MB"

echo ""
echo "============================================"
echo "  DCENT_OS SD Card Image: LAB-ONLY"
echo "============================================"
echo "  Image: $SD_IMAGE"
echo "  Size:  $(stat -c%s "$SD_IMAGE") bytes ($(stat -c%s "$SD_IMAGE" | awk '{print int($1/1024/1024)}') MB)"
echo ""
echo "  Write to SD card:"
echo "    balenaEtcher: select dcentos-s19pro-sd.img"
echo "    Or: dd if=dcentos-s19pro-sd.img of=/dev/sdX bs=4M"
echo ""
echo "  Boot: Insert SD + power on S19 Pro (JP4 jumper to SD position)"
echo "  NOTE: This image is for AM2 SD boot validation only. Do NOT treat it as a safe NAND installer yet."
echo "  SSH:  root@<IP> password: dcentral"
echo "============================================"
