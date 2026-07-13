#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"

PYTHON=${PYTHON:-python3}

"$PYTHON" scripts/sync_asic_wire_python.py --check
"$PYTHON" -m pytest -q scripts/test_asic_wire_crc5.py

PYCACHE_ROOT=${TMPDIR:-/tmp}/dcentos-asic-wire-pycache-$$
export PYTHONPYCACHEPREFIX="$PYCACHE_ROOT"
"$PYTHON" -m py_compile \
    tools/asic-wire/python/dcentos_asic_wire.py \
    overlay/root/tools/dcentos_asic_wire.py \
    br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/dcentos_asic_wire.py \
    overlay/root/tools/asic_enumerator.py \
    overlay/root/tools/register_scanner.py \
    overlay/root/tools/temp_finder.py \
    overlay/root/tools/board_health.py \
    br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/asic_enumerator.py \
    br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/register_scanner.py \
    br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/temp_finder.py \
    br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/assumption_verifier.py

"$PYTHON" overlay/root/tools/asic_enumerator.py --test >/dev/null
"$PYTHON" overlay/root/tools/register_scanner.py --test >/dev/null
"$PYTHON" overlay/root/tools/temp_finder.py --test >/dev/null
"$PYTHON" overlay/root/tools/board_health.py --test >/dev/null
"$PYTHON" br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/asic_enumerator.py --test >/dev/null
"$PYTHON" br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/register_scanner.py --test >/dev/null
"$PYTHON" br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/temp_finder.py --test >/dev/null
"$PYTHON" br2_external_dcentos/board/zynq/rootfs-overlay/root/tools/assumption_verifier.py --test >/dev/null

printf '%s\n' "ASIC wire CRC5 offline gate passed"
