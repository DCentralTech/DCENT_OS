#!/usr/bin/env bash
# Static test: pure-Python three-part MiB MBR writer in sd_common.sh
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/sd_common.sh"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
IMG="$TMP/disk.img"
dd if=/dev/zero of="$IMG" bs=1M count=16 status=none 2>/dev/null \
  || dd if=/dev/zero of="$IMG" bs=1M count=16 2>/dev/null

# Layout mirrors AM2-ish: p1@1MiB/4MiB type 0e bootable, p2@5/4 type 83, p3@9/4 type 83
sd_common::write_mbr_three_part_mb "$IMG" 1 4 0e 5 4 83 9 4 83

python3 - <<PY
import struct
with open("$IMG", "rb") as f:
    mbr = f.read(512)
assert mbr[0x1FE:0x200] == b"\x55\xaa", "missing 0x55AA"
entries = []
for i in range(4):
    off = 0x1BE + i * 16
    status, chs1, ptype, chs2, start, size = struct.unpack("<B3sB3sII", mbr[off:off+16])
    entries.append((status, ptype, start, size))
assert entries[0] == (0x80, 0x0E, 2048, 8192), entries[0]
assert entries[1] == (0x00, 0x83, 5 * 2048, 4 * 2048), entries[1]
assert entries[2] == (0x00, 0x83, 9 * 2048, 4 * 2048), entries[2]
assert entries[3] == (0x00, 0x00, 0, 0), entries[3]
print("test_sd_common_mbr_static: PASS")
PY
