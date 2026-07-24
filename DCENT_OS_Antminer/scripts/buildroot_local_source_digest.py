#!/usr/bin/env python3
"""Digest every byte and output-relevant mode in a staged BR2_EXTERNAL tree."""

from __future__ import annotations

import argparse
import hashlib
import os
import stat
import sys
from pathlib import Path


SCHEMA = b"dcentos-buildroot-local-source-v1\0"


class DigestError(ValueError):
    """The source tree cannot be represented by the canonical digest."""


def _field(hasher: "hashlib._Hash", value: bytes) -> None:
    hasher.update(len(value).to_bytes(8, "big"))
    hasher.update(value)


def digest_tree(root: Path) -> str:
    if root.is_symlink():
        raise DigestError(f"source root must not be a symlink: {root}")
    root = root.resolve(strict=True)
    if not root.is_dir():
        raise DigestError(f"source root is not a directory: {root}")

    entries: list[tuple[str, bytes, Path, os.stat_result]] = []
    for directory, dirnames, filenames in os.walk(root, followlinks=False):
        directory_path = Path(directory)
        for name in dirnames:
            path = directory_path / name
            if path.is_symlink():
                raise DigestError(f"symlinked source directory is unsupported: {path}")
            metadata = path.stat(follow_symlinks=False)
            if not stat.S_ISDIR(metadata.st_mode):
                raise DigestError(f"non-directory source entry is unsupported: {path}")
            relative = path.relative_to(root).as_posix()
            entries.append((relative, b"directory", path, metadata))
        for name in filenames:
            path = directory_path / name
            if path.is_symlink():
                raise DigestError(f"symlinked source file is unsupported: {path}")
            metadata = path.stat(follow_symlinks=False)
            if not stat.S_ISREG(metadata.st_mode):
                raise DigestError(f"non-regular source entry is unsupported: {path}")
            relative = path.relative_to(root).as_posix()
            if not relative or relative.startswith("/") or "\x00" in relative:
                raise DigestError(f"unsafe relative source path: {relative!r}")
            entries.append((relative, b"file", path, metadata))

    hasher = hashlib.sha256()
    hasher.update(SCHEMA)
    for relative, entry_type, path, metadata in sorted(
        entries, key=lambda item: item[0].encode()
    ):
        _field(hasher, relative.encode("utf-8"))
        _field(hasher, entry_type)
        _field(hasher, f"{stat.S_IMODE(metadata.st_mode):04o}".encode("ascii"))
        data = path.read_bytes() if entry_type == b"file" else b""
        after = path.stat(follow_symlinks=False)
        before_identity = (
            metadata.st_dev,
            metadata.st_ino,
            metadata.st_mode,
            metadata.st_size,
            metadata.st_mtime_ns,
        )
        after_identity = (
            after.st_dev,
            after.st_ino,
            after.st_mode,
            after.st_size,
            after.st_mtime_ns,
        )
        if (entry_type == b"file" and len(data) != metadata.st_size) or (
            before_identity != after_identity
        ):
            raise DigestError(f"source changed while hashing: {path}")
        _field(hasher, data)
    return hasher.hexdigest()


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="emit the canonical SHA-256 of one staged BR2_EXTERNAL tree"
    )
    parser.add_argument("root", type=Path, help="BR2_EXTERNAL source directory")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    try:
        print(digest_tree(args.root))
    except (DigestError, OSError, UnicodeError) as error:
        print(f"buildroot-local-source-digest: ERROR: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
