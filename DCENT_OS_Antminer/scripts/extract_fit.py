#!/usr/bin/env python3
"""Extract kernel, DTB, and ramdisk from a BraiinsOS FIT image.

The FIT image is a flattened device tree with inline binary data.
We find components by searching for known magic bytes:
  - ARM zImage: 0x016f2818 at offset +0x24
  - DTB: 0xd00dfeed
  - gzip: 0x1f8b08

Usage: python3 extract_fit.py <fit.itb> <output_dir>
"""
import struct
import sys
import os

def find_all(data, pattern, start=0, limit=10):
    """Find all occurrences of pattern in data."""
    results = []
    pos = start
    while len(results) < limit:
        idx = data.find(pattern, pos)
        if idx == -1:
            break
        results.append(idx)
        pos = idx + len(pattern)
    return results

def main():
    if len(sys.argv) < 3:
        print(f"Usage: {sys.argv[0]} <fit.itb> <output_dir>")
        sys.exit(1)

    fit_path = sys.argv[1]
    out_dir = sys.argv[2]

    with open(fit_path, "rb") as f:
        data = f.read()

    print(f"  FIT image: {len(data)} bytes ({len(data)//1024//1024} MB)")

    # Find ARM zImage magic (0x016f2818 at offset +0x24 from zImage start)
    zimage_magic = struct.pack("<I", 0x016f2818)
    magic_positions = find_all(data, zimage_magic)

    if not magic_positions:
        print("ERROR: No ARM zImage magic found")
        sys.exit(1)

    kernel_start = magic_positions[0] - 0x24
    # zImage size is at offset +0x2C from start
    kernel_size = struct.unpack_from("<I", data, kernel_start + 0x2C)[0]
    print(f"  Kernel: offset=0x{kernel_start:X}, size={kernel_size} ({kernel_size//1024} KB)")

    kernel_path = os.path.join(out_dir, "kernel.bin")
    with open(kernel_path, "wb") as f:
        f.write(data[kernel_start:kernel_start + kernel_size])
    print(f"  -> {kernel_path}")

    # Find DTB (magic 0xd00dfeed) AFTER the kernel
    dtb_magic = b"\xd0\x0d\xfe\xed"
    dtb_positions = find_all(data, dtb_magic, start=kernel_start + kernel_size)

    if dtb_positions:
        dtb_start = dtb_positions[0]
        dtb_size = struct.unpack_from(">I", data, dtb_start + 4)[0]
        # Sanity check: DTB should be < 1MB
        if dtb_size < 1024 * 1024:
            print(f"  DTB: offset=0x{dtb_start:X}, size={dtb_size} ({dtb_size//1024} KB)")
            dtb_path = os.path.join(out_dir, "fdt.dtb")
            with open(dtb_path, "wb") as f:
                f.write(data[dtb_start:dtb_start + dtb_size])
            print(f"  -> {dtb_path}")
            search_after_dtb = dtb_start + dtb_size
        else:
            print(f"  DTB at 0x{dtb_start:X} has suspicious size {dtb_size}, skipping")
            search_after_dtb = kernel_start + kernel_size
    else:
        print("  No DTB found after kernel")
        search_after_dtb = kernel_start + kernel_size

    # Find ramdisk (gzip magic 0x1f8b08) AFTER DTB
    gzip_magic = b"\x1f\x8b\x08"
    gz_positions = find_all(data, gzip_magic, start=search_after_dtb)

    if gz_positions:
        rd_start = gz_positions[0]
        # Ramdisk extends to near end of FIT (minus FDT trailing structure)
        # Find the end by looking for FDT_END tag (0x00000009) near the end
        rd_end = len(data)
        # Trim trailing zeros/padding
        while rd_end > rd_start and data[rd_end-1:rd_end] == b"\x00":
            rd_end -= 1
        rd_size = rd_end - rd_start
        print(f"  Ramdisk: offset=0x{rd_start:X}, size={rd_size} ({rd_size//1024//1024} MB)")
        rd_path = os.path.join(out_dir, "ramdisk_orig.gz")
        with open(rd_path, "wb") as f:
            f.write(data[rd_start:rd_end])
        print(f"  -> {rd_path}")
    else:
        print("  No ramdisk found")

    print("  Extraction complete")

if __name__ == "__main__":
    main()
