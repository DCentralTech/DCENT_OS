#!/usr/bin/env python3
"""
Patch U-Boot environment: clear upgrade_stage and first_boot
so DCENTos survives power cycles on firmware2 slot.

Usage: python3 fix_uboot_env.py
  Reads /tmp/uboot_env.bin, patches it, writes /tmp/uboot_env_patched.bin
  Then run: flash_erase /dev/mtd4 0 0 && nandwrite -p /dev/mtd4 /tmp/uboot_env_patched.bin
"""
import struct, sys

def crc32(data):
    """Pure Python CRC32 (same as crc32 / U-Boot crc32)."""
    # Build table
    table = []
    for i in range(256):
        c = i
        for _ in range(8):
            if c & 1:
                c = 0xEDB88320 ^ (c >> 1)
            else:
                c = c >> 1
        table.append(c)
    # Calculate
    crc = 0xFFFFFFFF
    for byte in data:
        crc = table[(crc ^ byte) & 0xFF] ^ (crc >> 8)
    return crc ^ 0xFFFFFFFF

ENV_FILE = "/tmp/uboot_env.bin"
ENV_SIZE = 131072  # 128KB (one erase block)

with open(ENV_FILE, "rb") as f:
    data = f.read(ENV_SIZE)

# Parse: first 4 bytes = CRC32, rest = data
stored_crc = struct.unpack("<I", data[:4])[0]
env_data = data[4:]
header_size = 4

# Verify CRC
calc_crc = crc32(env_data) & 0xFFFFFFFF
print("Stored CRC: 0x%08X" % stored_crc)
print("Calc CRC:   0x%08X" % calc_crc)
print("CRC match:  %s" % (stored_crc == calc_crc))

if stored_crc != calc_crc:
    # Try redundant format: CRC(4) + flags(1) + data
    env_data = data[5:]
    calc_crc = crc32(env_data) & 0xFFFFFFFF
    print("Trying redundant format - CRC: 0x%08X, match: %s" % (calc_crc, stored_crc == calc_crc))
    if stored_crc != calc_crc:
        print("ERROR: CRC mismatch!")
        sys.exit(1)
    header_size = 5

# Parse env vars
end_idx = env_data.find(b"\x00\x00")
if end_idx < 0:
    end_idx = len(env_data)
env_str = env_data[:end_idx+1]
vars_raw = [v for v in env_str.split(b"\x00") if v]

env_vars = {}
for v in vars_raw:
    v_str = v.decode("ascii", errors="replace")
    eq_pos = v_str.find("=")
    if eq_pos > 0:
        key = v_str[:eq_pos]
        val = v_str[eq_pos+1:]
        env_vars[key] = val

print("\nFound %d variables" % len(env_vars))
print("upgrade_stage = %s" % env_vars.get("upgrade_stage", "<not set>"))
print("first_boot = %s" % env_vars.get("first_boot", "<not set>"))
print("firmware = %s" % env_vars.get("firmware", "<not set>"))

# Remove upgrade_stage and first_boot
removed = []
if "upgrade_stage" in env_vars:
    del env_vars["upgrade_stage"]
    removed.append("upgrade_stage")
if "first_boot" in env_vars:
    del env_vars["first_boot"]
    removed.append("first_boot")

if not removed:
    print("\nNothing to change!")
    sys.exit(0)

print("\nRemoved: %s" % removed)

# Rebuild env
new_env = b""
for key in sorted(env_vars.keys()):
    entry = "%s=%s\x00" % (key, env_vars[key])
    new_env += entry.encode("ascii")
new_env += b"\x00"

# Pad
pad_size = ENV_SIZE - header_size
if len(new_env) > pad_size:
    print("ERROR: env too large!")
    sys.exit(1)
new_env = new_env + b"\xff" * (pad_size - len(new_env))

# New CRC
new_crc = crc32(new_env) & 0xFFFFFFFF
print("New CRC: 0x%08X" % new_crc)

# Build full image
if header_size == 5:
    new_data = struct.pack("<I", new_crc) + data[4:5] + new_env
else:
    new_data = struct.pack("<I", new_crc) + new_env

with open("/tmp/uboot_env_patched.bin", "wb") as f:
    f.write(new_data)

print("\nPatched env: /tmp/uboot_env_patched.bin (%d bytes)" % len(new_data))
print("Flash with: flash_erase /dev/mtd4 0 0 && nandwrite -p /dev/mtd4 /tmp/uboot_env_patched.bin")
