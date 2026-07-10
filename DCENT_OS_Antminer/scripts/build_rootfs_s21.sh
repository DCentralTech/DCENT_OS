#!/bin/bash
# build_rootfs_s21.sh — Build DCENT_OS rootfs for S21 (Amlogic A113D)
#
# LAB-ONLY rootfs repack for Amlogic bring-up.
#
# Extracts the BraiinsOS rootfs from a backup uImage, removes bosminer,
# injects dcentrald + init scripts, validates everything, and produces
# a candidate uImage for backup-first lab flashing.
#
# This script does NOT make standalone Amlogic flashing public-release safe.
# Recovery and cold-boot validation still gate that path.
#
# Requirements: WSL or Linux with cpio, gzip, mkimage, fakeroot
# Usage: ./build_rootfs_s21.sh <braiins_backup.uimage> <dcentrald_binary> [output.uimage]
#
# D-Central Technologies, 2026

set -euo pipefail

ROOTFS_WINDOW_BYTES=$((0x2800000))

# --- Arguments ---
BOS_UIMAGE="${1:?Usage: $0 <braiins_backup.uimage> <dcentrald_binary> [output.uimage]}"
DCENTRALD_BIN="${2:?Usage: $0 <braiins_backup.uimage> <dcentrald_binary> [output.uimage]}"
OUTPUT="${3:-dcentos_s21.uimage}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
WORKDIR="$(mktemp -d)"
STAGING="$WORKDIR/staging"

echo "=== DCENT_OS S21 Rootfs Builder ==="
echo "BOS backup: $BOS_UIMAGE"
echo "dcentrald:  $DCENTRALD_BIN"
echo "Output:     $OUTPUT"
echo "Work dir:   $WORKDIR"
echo ""

# --- Step 1: Validate inputs ---
echo "[1/7] Validating inputs..."
[ -f "$BOS_UIMAGE" ] || { echo "ERROR: BOS uImage not found: $BOS_UIMAGE"; exit 1; }
[ -f "$DCENTRALD_BIN" ] || { echo "ERROR: dcentrald binary not found: $DCENTRALD_BIN"; exit 1; }

# Check uImage magic
MAGIC=$(dd if="$BOS_UIMAGE" bs=1 count=4 2>/dev/null | od -A n -t x1 | tr -d ' ')
[ "$MAGIC" = "27051956" ] || { echo "ERROR: Not a uImage (magic: $MAGIC, expected: 27051956)"; exit 1; }
echo "  uImage magic: OK"

# --- Step 2: Extract BraiinsOS rootfs ---
echo "[2/7] Extracting BraiinsOS rootfs..."
mkdir -p "$STAGING"
tail -c +65 "$BOS_UIMAGE" > "$WORKDIR/rootfs_payload.gz"
gunzip -c "$WORKDIR/rootfs_payload.gz" > "$WORKDIR/rootfs.cpio"
[ -s "$WORKDIR/rootfs.cpio" ] || { echo "ERROR: Extracted rootfs.cpio is empty"; rm -rf "$WORKDIR"; exit 1; }
cd "$STAGING"
sudo cpio -id --no-absolute-filenames < "$WORKDIR/rootfs.cpio" 2>/dev/null
[ -x init ] || { echo "ERROR: Extracted rootfs is missing executable /init"; rm -rf "$WORKDIR"; exit 1; }
echo "  Extracted: $(du -sh . | cut -f1), $(find . -type f | wc -l) files"

# --- Step 3: Remove bosminer ---
echo "[3/7] Removing bosminer..."
REMOVED=0
for f in usr/bin/bosminer usr/bin/bos-tools usr/bin/boser; do
    if [ -f "$f" ]; then
        SIZE=$(stat -c%s "$f" 2>/dev/null || stat -f%z "$f" 2>/dev/null || echo 0)
        rm -f "$f"
        REMOVED=$((REMOVED + SIZE))
    fi
done
echo "  Removed: $((REMOVED / 1024 / 1024)) MB"

# --- Step 4: Inject dcentrald ---
echo "[4/7] Injecting dcentrald..."
sudo mkdir -p usr/local/bin etc/dcentrald etc/init.d
sudo cp "$DCENTRALD_BIN" usr/local/bin/dcentrald
sudo chmod 755 usr/local/bin/dcentrald

# Copy S21 config
if [ -f "$PROJECT_DIR/dcentrald/dcentrald_s21.toml" ]; then
    sudo cp "$PROJECT_DIR/dcentrald/dcentrald_s21.toml" etc/dcentrald/dcentrald.toml
fi

# Copy Amlogic init script
if [ -f "$PROJECT_DIR/br2_external_dcentos/board/amlogic/rootfs-overlay/etc/init.d/S82dcentrald" ]; then
    sudo cp "$PROJECT_DIR/br2_external_dcentos/board/amlogic/rootfs-overlay/etc/init.d/S82dcentrald" etc/init.d/S82dcentrald
    sudo chmod 755 etc/init.d/S82dcentrald
fi

# Neutralize bosminer init scripts (overwrite with no-ops, don't delete)
for f in etc/init.d/S97bosminer-detect etc/init.d/S98boser etc/init.d/S99bosminer; do
    if [ -f "$f" ]; then
        echo '#!/bin/sh' | sudo tee "$f" > /dev/null
        echo '# Disabled by DCENT_OS' | sudo tee -a "$f" > /dev/null
        echo 'exit 0' | sudo tee -a "$f" > /dev/null
        sudo chmod 755 "$f"
    fi
done
echo "  dcentrald: $(du -sh usr/local/bin/dcentrald | cut -f1)"

# Set hostname
echo "dcent-s21" | sudo tee etc/hostname > /dev/null

# Passwordless root
sudo sed -i 's/^root:[^:]*:/root::/' etc/shadow 2>/dev/null || true

# --- Step 5: Pre-flash checklist (7 automated checks) ---
echo "[5/7] Running pre-flash checklist..."
FAIL=0

# Check 1: armhf glibc linker exists
if [ -f lib/ld-linux-armhf.so.3 ] || [ -L lib/ld-linux-armhf.so.3 ]; then
    echo "  [PASS] armhf glibc linker: lib/ld-linux-armhf.so.3"
else
    echo "  [FAIL] armhf glibc linker MISSING — rootfs will kernel panic!"
    FAIL=1
fi

# Check 2: BusyBox is 32-bit ARM
BB_TYPE=$(file bin/busybox 2>/dev/null || echo "unknown")
if echo "$BB_TYPE" | grep -q "32-bit.*ARM"; then
    echo "  [PASS] BusyBox: 32-bit ARM"
else
    echo "  [FAIL] BusyBox is NOT 32-bit ARM: $BB_TYPE"
    FAIL=1
fi

# Check 3: /init exists and is executable
if [ -x init ]; then
    echo "  [PASS] /init exists and executable"
else
    echo "  [FAIL] /init missing or not executable"
    FAIL=1
fi

# Check 4: dcentrald is statically linked aarch64
DC_TYPE=$(file usr/local/bin/dcentrald 2>/dev/null || echo "unknown")
if echo "$DC_TYPE" | grep -q "64-bit.*aarch64"; then
    echo "  [PASS] dcentrald: 64-bit aarch64"
else
    echo "  [FAIL] dcentrald is NOT aarch64: $DC_TYPE"
    FAIL=1
fi
if echo "$DC_TYPE" | grep -q "statically linked"; then
    echo "  [PASS] dcentrald: statically linked"
else
    echo "  [WARN] dcentrald may be dynamically linked (check manually)"
fi

# Check 5: No hardcoded wallet in config
if grep -q "bc1q04lzw" etc/dcentrald/dcentrald.toml 2>/dev/null; then
    echo "  [FAIL] Hardcoded wallet address in config!"
    FAIL=1
else
    echo "  [PASS] No hardcoded wallet in config"
fi

# Check 6: S82dcentrald init script has fan_safety_override
if grep -q "fan_safety_override" etc/init.d/S82dcentrald 2>/dev/null; then
    echo "  [PASS] S82dcentrald has fan safety override"
else
    echo "  [WARN] S82dcentrald missing fan_safety_override"
fi

# Check 7: Bosminer init scripts neutralized
if head -1 etc/init.d/S99bosminer 2>/dev/null | grep -q "#!/bin/sh" && \
   grep -q "exit 0" etc/init.d/S99bosminer 2>/dev/null; then
    echo "  [PASS] S99bosminer neutralized"
else
    echo "  [WARN] S99bosminer may still be active"
fi

if [ $FAIL -ne 0 ]; then
    echo ""
    echo "*** PRE-FLASH CHECKLIST FAILED — aborting! ***"
    echo "Fix the issues above and re-run."
    rm -rf "$WORKDIR"
    exit 1
fi

# --- Step 6: Pack rootfs ---
echo "[6/7] Packing rootfs (fakeroot + cpio + gzip)..."
cd "$STAGING"
sudo chown -R 0:0 . 2>/dev/null || true
find . | fakeroot cpio -o -H newc 2>/dev/null | gzip -9 > "$WORKDIR/rootfs.cpio.gz"
echo "  CPIO: $(du -sh "$WORKDIR/rootfs.cpio.gz" | cut -f1)"

# --- Step 7: Wrap in uImage ---
echo "[7/7] Creating uImage..."
mkimage -A arm64 -T ramdisk -C gzip -n "DCENT_OS S21" \
    -d "$WORKDIR/rootfs.cpio.gz" "$WORKDIR/dcentos.uimage"

# Copy to output
cp "$WORKDIR/dcentos.uimage" "$OUTPUT"
OUTPUT_SIZE=$(stat -c%s "$OUTPUT" 2>/dev/null || stat -f%z "$OUTPUT" 2>/dev/null || echo 0)
if [ "$OUTPUT_SIZE" -gt "$ROOTFS_WINDOW_BYTES" ]; then
    echo "ERROR: Output uImage is too large for the Amlogic rootfs window: $OUTPUT_SIZE > $ROOTFS_WINDOW_BYTES"
    rm -rf "$WORKDIR"
    exit 1
fi
OUTPUT_SHA256=$(sha256sum "$OUTPUT" | awk '{print $1}')
echo ""
echo "=== BUILD COMPLETE ==="
echo "Output: $OUTPUT ($(du -sh "$OUTPUT" | cut -f1))"
echo "SHA256: $OUTPUT_SHA256"
echo ""
echo "Guarded lab workflow:"
echo "  - Do not paste raw flash_erase/nandwrite commands from this legacy builder."
echo "  - Prefer scripts/build_amlogic_native_install.sh for validated native images."
echo "  - Flash only through a model-calibrated helper that validates package/uImage"
echo "    magic, mtd geometry, upload SHA256, readback SHA256, and recovery access."
echo "  - Expected image SHA256 for a guarded readback comparison: $OUTPUT_SHA256"

# Cleanup
rm -rf "$WORKDIR"
