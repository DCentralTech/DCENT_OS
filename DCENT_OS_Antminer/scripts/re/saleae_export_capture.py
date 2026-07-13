#!/usr/bin/env python3
"""Export a saved .sal capture through Logic 2's supported automation API."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path
from typing import Sequence


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--port", type=int, default=10430)
    parser.add_argument("--channels", type=int, nargs="+")
    args = parser.parse_args(argv)
    args.output.mkdir(parents=True, exist_ok=True)
    try:
        from saleae import automation

        with automation.Manager.connect(port=args.port) as manager:
            with manager.load_capture(str(args.capture.resolve())) as capture:
                capture.export_raw_data_binary(
                    directory=str(args.output.resolve()),
                    digital_channels=args.channels,
                )
        return 0
    except (ImportError, OSError, RuntimeError) as error:
        print(f"saleae_export_capture: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
