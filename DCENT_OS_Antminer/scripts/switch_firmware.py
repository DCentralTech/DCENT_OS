#!/usr/bin/env python3
"""
Switch U-Boot firmware slot: firmware=1 (firmware1) or firmware=2 (firmware2)

*** DEPRECATED FOR THE OTA/SYSUPGRADE WRITE PATH (W24-OTA-2, 2026-05-22). ***

The am1-s9 `sysupgrade` env-flip NO LONGER calls this script — it now flips the
A/B boot-selector with `fw_setenv` (libubootenv, redundant-copy-atomic), the
same primitive the am2 path uses. Reason: this script's raw-env model wrote
BOTH redundant env copies byte-identical with the SAME 1-byte flag, and the
caller's `flash_erase /dev/mtd4 0 0` erased BOTH copies at once. That
erase-both-then-rewrite + identical-flag pattern defeats U-Boot's redundant-env
"newer copy" disambiguation and has a zero-valid-copy window — the exact failure
mode that bricked .39/.139. NEVER use this script to flip the LIVE env via raw
`flash_erase`/`nandwrite`. Use `fw_setenv` instead (see
).

This script is RETAINED only as an offline/forensic env-image patcher for
manual recovery on a unit whose libubootenv is unavailable, and it now
generates redundant copies with DISTINCT flag bytes so that even if it is used,
the two copies remain disambiguable (the newer copy gets the higher flag).
Requires the explicit `--i-understand-this-is-not-fw-setenv` acknowledgement to
run, so it cannot be invoked by accident in place of fw_setenv.

Usage:
  python3 switch_firmware.py <1|2> --i-understand-this-is-not-fw-setenv
  python3 switch_firmware.py <1|2> --with-stage --i-understand-this-is-not-fw-setenv

Reads /tmp/uboot_env.bin (dumped from mtd4), patches firmware=N, writes
/tmp/uboot_env_patched.bin covering BOTH redundant env copies.

The --with-stage flag sets upgrade_stage=0 so U-Boot auto_recovery will revert
to the previous firmware if the new one fails to boot. After a successful boot,
the S99upgrade init script clears upgrade_stage to make the new firmware permanent.
"""
import struct, sys

def crc32(data):
    """Pure Python CRC32 (same as zlib.crc32 / U-Boot crc32)."""
    table = []
    for i in range(256):
        c = i
        for _ in range(8):
            if c & 1:
                c = 0xEDB88320 ^ (c >> 1)
            else:
                c = c >> 1
        table.append(c)
    crc = 0xFFFFFFFF
    for byte in data:
        crc = table[(crc ^ byte) & 0xFF] ^ (crc >> 8)
    return crc ^ 0xFFFFFFFF

def parse_env_copy(data_128k):
    """Parse a single 128KB env copy. Returns (header_size, env_vars) or None."""
    if len(data_128k) < 8:
        return None
    stored_crc = struct.unpack("<I", data_128k[:4])[0]

    # Try 4-byte header (CRC only)
    env_data = data_128k[4:]
    header_size = 4
    calc_crc = crc32(env_data) & 0xFFFFFFFF
    if stored_crc != calc_crc:
        # Try 5-byte header (CRC + flags)
        env_data = data_128k[5:]
        calc_crc = crc32(env_data) & 0xFFFFFFFF
        if stored_crc != calc_crc:
            return None
        header_size = 5

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
            env_vars[v_str[:eq_pos]] = v_str[eq_pos+1:]

    flags = data_128k[4] if header_size == 5 else None
    return header_size, env_vars, flags

# Parse arguments
WITH_STAGE = False
TARGET_FW = None
ACK_NOT_FW_SETENV = False

for arg in sys.argv[1:]:
    if arg == "--with-stage":
        WITH_STAGE = True
    elif arg == "--i-understand-this-is-not-fw-setenv":
        ACK_NOT_FW_SETENV = True
    elif arg in ("1", "2"):
        TARGET_FW = arg
    else:
        print("Unknown argument: %s" % arg)
        sys.exit(1)

if TARGET_FW is None:
    print("Usage: python3 switch_firmware.py <1|2> --i-understand-this-is-not-fw-setenv [--with-stage]")
    print("  1 = firmware1 (mtd7)")
    print("  2 = firmware2 (mtd8)")
    print("  --with-stage = set upgrade_stage=0 for auto_recovery")
    sys.exit(1)

# W24-OTA-2: refuse to run as a stand-in for fw_setenv. The OTA/sysupgrade
# write path uses fw_setenv (libubootenv) — this script is offline/forensic
# recovery only and must never be the live env-flip mechanism.
if not ACK_NOT_FW_SETENV:
    print("REFUSING: switch_firmware.py is DEPRECATED for the env flip.")
    print("  The am1-s9 sysupgrade now flips the A/B selector with fw_setenv")
    print("  (libubootenv, redundant-copy-atomic). The raw flash_erase-both +")
    print("  identical-flag dual-write this script feeds bricked .39/.139.")
    print("  To flip the live env safely:")
    print("    fw_setenv firmware <1|2>; fw_setenv upgrade_stage 0; fw_setenv first_boot yes")
    print("  If you REALLY need offline env-image patching (no libubootenv),")
    print("  re-run with --i-understand-this-is-not-fw-setenv.")
    sys.exit(2)

ENV_FILE = "/tmp/uboot_env.bin"
COPY_SIZE = 131072  # 128KB per env copy

with open(ENV_FILE, "rb") as f:
    raw = f.read()

total_size = len(raw)
has_redundant = total_size >= COPY_SIZE * 2

# Parse copy 1 (offset 0)
copy1 = parse_env_copy(raw[:COPY_SIZE])
if copy1 is None:
    print("ERROR: Copy 1 (offset 0) CRC mismatch!")
    sys.exit(1)

header_size, env_vars, flags1 = copy1
print("Copy 1: CRC OK, header=%d bytes, flags=%s" % (header_size, hex(flags1) if flags1 is not None else "N/A"))

# Parse copy 2 if present
if has_redundant:
    copy2 = parse_env_copy(raw[COPY_SIZE:COPY_SIZE*2])
    if copy2 is not None:
        _, _, flags2 = copy2
        print("Copy 2: CRC OK, flags=%s" % (hex(flags2) if flags2 is not None else "N/A"))
    else:
        print("Copy 2: CRC invalid (will be rebuilt)")
else:
    print("Single-copy env (input < 256KB)")

print("Current firmware = %s" % env_vars.get("firmware", "<not set>"))
print("Setting firmware = %s" % TARGET_FW)

# Set firmware
env_vars["firmware"] = TARGET_FW

if WITH_STAGE:
    env_vars["upgrade_stage"] = "0"
    print("Set upgrade_stage = 0 (auto_recovery enabled)")
    if "first_boot" in env_vars:
        del env_vars["first_boot"]
        print("Removed: first_boot")
else:
    for key in ["upgrade_stage", "first_boot"]:
        if key in env_vars:
            del env_vars[key]
            print("Removed: %s" % key)

# Rebuild env data
new_env = b""
for key in sorted(env_vars.keys()):
    new_env += ("%s=%s\x00" % (key, env_vars[key])).encode("ascii")
new_env += b"\x00"

pad_size = COPY_SIZE - header_size
new_env = new_env + b"\xff" * (pad_size - len(new_env))

# New CRC
new_crc = crc32(new_env) & 0xFFFFFFFF

# Build the redundant copies.
#
# W24-OTA-2 FIX: for the 5-byte (CRC+flags) redundant env, U-Boot's
# redundant-env protocol uses the 1-byte "flags" (active/obsolete) counter to
# pick the NEWER copy when the two differ. The old code wrote BOTH copies with
# the SAME flag byte (`single_copy + single_copy`), so there was never a
# "newer vs older" distinction — combined with the caller's flash_erase-both
# this created a real zero-valid-copy / ambiguous-copy window (the .39/.139
# brick class). We now give the two copies DISTINCT flags: copy1 carries
# new_flags, copy2 carries (new_flags + 1) — so copy2 is unambiguously the
# active/newer copy and U-Boot's redundancy logic stays well-defined even if
# this offline patcher is ever used. The non-flag (5-byte) format has no
# disambiguation field, so identical copies are unavoidable there (and are
# only reached on env images that lack the flags byte).
if header_size == 5:
    new_flags = (flags1 + 1) & 0xFF if flags1 is not None else 0x01

    def _make_copy(flag_byte):
        return struct.pack("<I", new_crc) + struct.pack("B", flag_byte) + new_env

    copy1_out = _make_copy(new_flags)
    if has_redundant:
        # Distinct flag so copy2 is the unambiguous "newer/active" copy.
        copy2_out = _make_copy((new_flags + 1) & 0xFF)
        output = copy1_out + copy2_out
        if total_size > len(output):
            output += b"\xff" * (total_size - len(output))
        print("Output: %d bytes (2 redundant copies, distinct flags %s/%s)" % (
            len(output), hex(new_flags), hex((new_flags + 1) & 0xFF)))
    else:
        output = copy1_out
        print("Output: %d bytes (single copy, flag %s)" % (len(output), hex(new_flags)))
else:
    # 4-byte (CRC-only) env: no flags field exists to disambiguate copies.
    single_copy = struct.pack("<I", new_crc) + new_env
    if has_redundant:
        output = single_copy + single_copy
        if total_size > len(output):
            output += b"\xff" * (total_size - len(output))
        print("Output: %d bytes (2 redundant copies, no-flags env)" % len(output))
    else:
        output = single_copy
        print("Output: %d bytes (single copy)" % len(output))

with open("/tmp/uboot_env_patched.bin", "wb") as f:
    f.write(output)

print("Patched: /tmp/uboot_env_patched.bin")
print("Flash:   flash_erase /dev/mtd4 0 0 && nandwrite -p /dev/mtd4 /tmp/uboot_env_patched.bin")
