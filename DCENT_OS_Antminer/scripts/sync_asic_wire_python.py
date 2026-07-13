#!/usr/bin/env python3
"""Synchronize or verify target-overlay copies of the ASIC wire module."""

import argparse
import pathlib
import shutil
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE = ROOT / "tools/asic-wire/python/dcentos_asic_wire.py"
DESTINATIONS = (
    ROOT / "overlay/root/tools/dcentos_asic_wire.py",
    ROOT
    / "br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/dcentos_asic_wire.py",
)


def check():
    expected = SOURCE.read_bytes()
    drifted = []
    for destination in DESTINATIONS:
        if not destination.is_file() or destination.read_bytes() != expected:
            drifted.append(destination)
    if drifted:
        for destination in drifted:
            print("ASIC wire staging drift: {}".format(destination), file=sys.stderr)
        print("Run: python3 scripts/sync_asic_wire_python.py --write", file=sys.stderr)
        return 1
    print("ASIC wire Python staging copies match canonical source")
    return 0


def write():
    for destination in DESTINATIONS:
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(str(SOURCE), str(destination))
        print("updated {}".format(destination))
    return check()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="fail on staging drift")
    mode.add_argument("--write", action="store_true", help="refresh staging copies")
    args = parser.parse_args()
    return write() if args.write else check()


if __name__ == "__main__":
    raise SystemExit(main())
