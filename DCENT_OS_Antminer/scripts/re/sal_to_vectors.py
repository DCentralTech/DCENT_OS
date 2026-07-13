#!/usr/bin/env python3
"""Normalize Saleae UART evidence into DCENT init-trace JSONL.

The converter accepts analyzer CSV exports and documented Saleae digital
binary streams (versions 0/1), including those embedded in a .sal zip. Saleae
also stores private type-100 streams inside some .sal archives; those are
detected and rejected with an actionable message because their format is not a
public evidence contract. Exporting that capture to digital binary or analyzer
CSV in Logic 2 makes it consumable without changing provenance.
"""

from __future__ import annotations

import argparse
import bisect
import csv
import hashlib
import io
import json
import struct
import sys
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import BinaryIO, Iterable, List, Sequence, TextIO, Tuple


@dataclass(frozen=True)
class DigitalChunk:
    initial_state: int
    begin_time: float
    end_time: float
    transition_times: Tuple[float, ...]


def _read_exact(stream: BinaryIO, count: int) -> bytes:
    data = stream.read(count)
    if len(data) != count:
        raise ValueError(f"truncated Saleae stream: wanted {count} bytes, got {len(data)}")
    return data


def parse_saleae_digital(stream: BinaryIO) -> List[DigitalChunk]:
    """Parse Saleae's documented little-endian digital export v0/v1."""
    if _read_exact(stream, 8) != b"<SALEAE>":
        raise ValueError("not a Saleae binary stream")
    version, data_type = struct.unpack("<ii", _read_exact(stream, 8))
    if data_type != 0:
        if data_type == 100:
            raise ValueError(
                "private .sal digital stream type 100 is not documented; "
                "open the capture in Saleae Logic 2 and export the UART analyzer CSV "
                "or digital binary v0/v1"
            )
        raise ValueError(f"expected digital stream type 0, got {data_type}")
    if version not in (0, 1):
        raise ValueError(f"unsupported Saleae digital version {version}")

    chunk_count = struct.unpack("<Q", _read_exact(stream, 8))[0] if version == 1 else 1
    chunks: List[DigitalChunk] = []
    for _ in range(chunk_count):
        initial_state = struct.unpack("<I", _read_exact(stream, 4))[0]
        if initial_state not in (0, 1):
            raise ValueError(f"invalid initial digital state {initial_state}")
        if version == 1:
            sample_rate = struct.unpack("<d", _read_exact(stream, 8))[0]
            if sample_rate <= 0:
                raise ValueError(f"invalid sample rate {sample_rate}")
        begin_time, end_time, transition_count = struct.unpack(
            "<ddQ", _read_exact(stream, 24)
        )
        raw = _read_exact(stream, transition_count * 8)
        transitions = struct.unpack(f"<{transition_count}d", raw) if transition_count else ()
        if any(a > b for a, b in zip(transitions, transitions[1:])):
            raise ValueError("transition times are not ordered")
        chunks.append(DigitalChunk(initial_state, begin_time, end_time, tuple(transitions)))
    if stream.read(1):
        raise ValueError("unexpected trailing bytes in Saleae digital stream")
    return chunks


def decode_uart_8n1(chunks: Sequence[DigitalChunk], baud: int) -> List[Tuple[float, int]]:
    """Decode idle-high 8N1 UART bytes from transition timestamps."""
    bit_time = 1.0 / baud
    decoded: List[Tuple[float, int]] = []
    for chunk in chunks:
        transitions = chunk.transition_times

        def state_at(timestamp: float) -> int:
            toggles = bisect.bisect_right(transitions, timestamp)
            return chunk.initial_state ^ (toggles & 1)

        previous = chunk.initial_state
        next_frame_time = chunk.begin_time
        for edge in transitions:
            current = 1 - previous
            is_start = previous == 1 and current == 0 and edge >= next_frame_time
            previous = current
            if not is_start or edge + 10 * bit_time > chunk.end_time:
                continue
            value = 0
            for bit in range(8):
                value |= state_at(edge + (1.5 + bit) * bit_time) << bit
            if state_at(edge + 9.5 * bit_time) != 1:
                continue
            decoded.append((edge, value))
            next_frame_time = edge + 9.75 * bit_time
    return decoded


def parse_analyzer_csv(stream: TextIO) -> List[Tuple[float, int]]:
    reader = csv.DictReader(stream)
    if not reader.fieldnames:
        raise ValueError("analyzer CSV has no header")
    time_key = next((key for key in reader.fieldnames if "time" in key.lower()), None)
    data_key = next(
        (key for key in reader.fieldnames if key.lower() in {"data", "value", "data (hex)"}),
        None,
    )
    if time_key is None or data_key is None:
        raise ValueError(f"CSV must contain time and data columns; got {reader.fieldnames}")
    decoded: List[Tuple[float, int]] = []
    for row in reader:
        raw = (row.get(data_key) or "").strip()
        if not raw:
            continue
        value = int(raw, 0) if raw.lower().startswith("0x") else int(raw, 16)
        if not 0 <= value <= 0xFF:
            raise ValueError(f"UART analyzer value outside one byte: {raw}")
        decoded.append((float(row[time_key]), value))
    return decoded


def frames(decoded: Sequence[Tuple[float, int]], baud: int) -> Iterable[dict]:
    if not decoded:
        return
    gap_limit = 12.0 / baud
    started_at, payload = decoded[0][0], [decoded[0][1]]
    previous = decoded[0][0]
    for timestamp, value in decoded[1:]:
        if timestamp - previous > gap_limit:
            yield {"event": "uart_bytes", "timestamp_s": started_at, "bytes": payload}
            started_at, payload = timestamp, []
        payload.append(value)
        previous = timestamp
    yield {"event": "uart_bytes", "timestamp_s": started_at, "bytes": payload}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def decode_input(path: Path, channel: int, baud: int) -> List[Tuple[float, int]]:
    if path.suffix.lower() == ".csv":
        with path.open("r", encoding="utf-8-sig", newline="") as stream:
            return parse_analyzer_csv(stream)
    if path.suffix.lower() == ".bin":
        with path.open("rb") as stream:
            return decode_uart_8n1(parse_saleae_digital(stream), baud)
    if path.suffix.lower() != ".sal":
        raise ValueError("input must be .sal, Saleae digital .bin, or analyzer .csv")
    member = f"digital-{channel}.bin"
    with zipfile.ZipFile(path) as archive:
        try:
            data = archive.read(member)
        except KeyError as error:
            available = ", ".join(name for name in archive.namelist() if name.startswith("digital-"))
            raise ValueError(f"{member} is absent; available streams: {available}") from error
    return decode_uart_8n1(parse_saleae_digital(io.BytesIO(data)), baud)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path)
    parser.add_argument("--model", required=True, help="DCENT model slug")
    parser.add_argument("--channel", type=int, default=0, help="digital channel in a .sal archive")
    parser.add_argument("--baud", type=int, required=True)
    parser.add_argument("--direction", choices=("host_to_asic", "asic_to_host"), required=True)
    parser.add_argument(
        "--provenance-source",
        type=Path,
        help="original capture when INPUT is a derived Logic 2 export",
    )
    args = parser.parse_args(argv)
    try:
        decoded = decode_input(args.input, args.channel, args.baud)
        provenance_source = args.provenance_source or args.input
        header = {
            "schema": "dcent-saleae-uart-v1",
            "model": args.model,
            "strictness": "exact",
            "provenance": str(provenance_source).replace("\\", "/"),
            "source_sha256": sha256(provenance_source),
            "normalized_input_sha256": sha256(args.input),
            "channel": args.channel,
            "baud": args.baud,
            "direction": args.direction,
        }
        print(json.dumps(header, separators=(",", ":")))
        for frame in frames(decoded, args.baud):
            frame["direction"] = args.direction
            print(json.dumps(frame, separators=(",", ":")))
        return 0
    except (OSError, ValueError, zipfile.BadZipFile) as error:
        print(f"sal_to_vectors: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
