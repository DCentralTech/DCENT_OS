#!/usr/bin/env python3
"""Regenerate per-chip-family golden midstate fixtures for the W6.2
share-submission e2e regression test.

Output: 7 .bin files at
`DCENT_OS_Antminer/dcentrald/dcentrald-api/tests/golden_midstates/{family}.bin`,
each containing one 32-byte SHA-256 midstate computed from a
deterministic 64-byte block-header prefix.

The seed is intentionally fixed so the fixtures are stable across
runs, machines, and CI nodes. Re-run this script ONLY when the
production midstate algorithm in
`dcentrald_stratum::compute_midstate_from_prefix` changes
intentionally (rare). The test's
`derive_golden_input` helper mirrors this script's prefix
derivation; drift on either side is caught by the cross-check
assertion at the top of `share_submission_e2e_per_chip_family`.

Usage::

    python3 DCENT_OS_Antminer/scripts/regen_golden_midstates.py

Counterpart test:
    DCENT_OS_Antminer/dcentrald/dcentrald-api/tests/share_submission_e2e.rs

Counterpart CI gate:
    DCENT_OS_Antminer/scripts/ci_offline_gates.sh
    (swap_bytes_midstate_check, extended W6.2 to scan
     dcentrald-stratum/src/v1/client.rs)
"""
from __future__ import annotations

import hashlib
import os
import struct
import sys

# Stable seed — MUST match `GOLDEN_SEED` in the Rust test file.
SEED = b"DCENT_OS-W6.2-share-submission-e2e-2026-05-07"

FAMILIES = (
    "bm1387",  # S9
    "bm1397",  # BitAxe Ultra / S17
    "bm1398",  # S19 Pro
    "bm1362",  # S19j Pro Amlogic, BitAxe Supra
    "bm1366",  # S19k Pro, BitAxe Gamma
    "bm1368",  # S21, S19j XP
    "bm1370",  # BitAxe Hex, S21 Pro
)

# Default output dir, relative to repo root.
DEFAULT_OUTDIR = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..",
    "dcentrald",
    "dcentrald-api",
    "tests",
    "golden_midstates",
)


def derive_64byte_prefix(family: str) -> bytes:
    """Mirror of `derive_golden_input` in the Rust test."""
    h_version = hashlib.sha256(SEED + b"|" + family.encode() + b"|version").digest()
    version = struct.unpack("<I", h_version[:4])[0]
    prev_hash = hashlib.sha256(SEED + b"|" + family.encode() + b"|prev_hash").digest()
    merkle = hashlib.sha256(SEED + b"|" + family.encode() + b"|merkle_root").digest()
    prefix = struct.pack("<I", version) + prev_hash + merkle[:28]
    assert len(prefix) == 64
    return prefix


# Pure-Python SHA-256 single-block compression. Produces the midstate
# (32 bytes, big-endian H0..H7) without finalizing the hash. Mirrors
# `dcentrald_stratum::work::compute_midstate_from_prefix`.
_K = (
    0x428A2F98, 0x71374491, 0xB5C0FBCF, 0xE9B5DBA5, 0x3956C25B, 0x59F111F1, 0x923F82A4, 0xAB1C5ED5,
    0xD807AA98, 0x12835B01, 0x243185BE, 0x550C7DC3, 0x72BE5D74, 0x80DEB1FE, 0x9BDC06A7, 0xC19BF174,
    0xE49B69C1, 0xEFBE4786, 0x0FC19DC6, 0x240CA1CC, 0x2DE92C6F, 0x4A7484AA, 0x5CB0A9DC, 0x76F988DA,
    0x983E5152, 0xA831C66D, 0xB00327C8, 0xBF597FC7, 0xC6E00BF3, 0xD5A79147, 0x06CA6351, 0x14292967,
    0x27B70A85, 0x2E1B2138, 0x4D2C6DFC, 0x53380D13, 0x650A7354, 0x766A0ABB, 0x81C2C92E, 0x92722C85,
    0xA2BFE8A1, 0xA81A664B, 0xC24B8B70, 0xC76C51A3, 0xD192E819, 0xD6990624, 0xF40E3585, 0x106AA070,
    0x19A4C116, 0x1E376C08, 0x2748774C, 0x34B0BCB5, 0x391C0CB3, 0x4ED8AA4A, 0x5B9CCA4F, 0x682E6FF3,
    0x748F82EE, 0x78A5636F, 0x84C87814, 0x8CC70208, 0x90BEFFFA, 0xA4506CEB, 0xBEF9A3F7, 0xC67178F2,
)


def _rotr(x: int, n: int) -> int:
    return ((x >> n) | (x << (32 - n))) & 0xFFFFFFFF


def sha256_compress_one_block(prefix64: bytes) -> bytes:
    h = [0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A,
         0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19]
    w = [0] * 64
    for i in range(16):
        w[i] = struct.unpack(">I", prefix64[i * 4:(i + 1) * 4])[0]
    for i in range(16, 64):
        s0 = _rotr(w[i - 15], 7) ^ _rotr(w[i - 15], 18) ^ (w[i - 15] >> 3)
        s1 = _rotr(w[i - 2], 17) ^ _rotr(w[i - 2], 19) ^ (w[i - 2] >> 10)
        w[i] = (w[i - 16] + s0 + w[i - 7] + s1) & 0xFFFFFFFF
    a, b, c, d, e, f, g, hh = h
    for i in range(64):
        S1 = _rotr(e, 6) ^ _rotr(e, 11) ^ _rotr(e, 25)
        ch = (e & f) ^ ((~e & 0xFFFFFFFF) & g)
        t1 = (hh + S1 + ch + _K[i] + w[i]) & 0xFFFFFFFF
        S0 = _rotr(a, 2) ^ _rotr(a, 13) ^ _rotr(a, 22)
        mj = (a & b) ^ (a & c) ^ (b & c)
        t2 = (S0 + mj) & 0xFFFFFFFF
        hh = g
        g = f
        f = e
        e = (d + t1) & 0xFFFFFFFF
        d = c
        c = b
        b = a
        a = (t1 + t2) & 0xFFFFFFFF
    out = [
        (h[0] + a) & 0xFFFFFFFF,
        (h[1] + b) & 0xFFFFFFFF,
        (h[2] + c) & 0xFFFFFFFF,
        (h[3] + d) & 0xFFFFFFFF,
        (h[4] + e) & 0xFFFFFFFF,
        (h[5] + f) & 0xFFFFFFFF,
        (h[6] + g) & 0xFFFFFFFF,
        (h[7] + hh) & 0xFFFFFFFF,
    ]
    return b"".join(struct.pack(">I", x) for x in out)


def main() -> int:
    outdir = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUTDIR
    outdir = os.path.abspath(outdir)
    os.makedirs(outdir, exist_ok=True)
    print(f"Regenerating golden midstate fixtures into: {outdir}")
    print(f"Seed: {SEED!r}")
    print()
    for family in FAMILIES:
        prefix = derive_64byte_prefix(family)
        midstate = sha256_compress_one_block(prefix)
        path = os.path.join(outdir, f"{family}.bin")
        with open(path, "wb") as f:
            f.write(midstate)
        print(f"  wrote {family}.bin (32 bytes)")
        print(f"    prefix64 = {prefix.hex()}")
        print(f"    midstate = {midstate.hex()}")
    print()
    print("Done. Re-run `cargo test -p dcentrald-api --test share_submission_e2e` to verify.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
