#!/usr/bin/env python3
"""Cross-platform durable local-file and directory primitives."""

from __future__ import annotations

import argparse
import os
import stat
import sys
import tempfile
from pathlib import Path
from typing import Sequence


class DurableIoError(ValueError):
    """A requested durability boundary is unsafe or unavailable."""


def _windows_directory_handle_path(path: Path) -> str:
    """Return an anchored Win32 directory path without drive-relative rewrite."""
    return str(path)


def _fsync_directory_windows(path: Path) -> None:
    """Flush one Windows directory handle opened with write authority."""
    import ctypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    create_file = kernel32.CreateFileW
    create_file.argtypes = (
        ctypes.c_wchar_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
        ctypes.c_uint32,
        ctypes.c_uint32,
        ctypes.c_void_p,
    )
    create_file.restype = ctypes.c_void_p
    flush_file_buffers = kernel32.FlushFileBuffers
    flush_file_buffers.argtypes = (ctypes.c_void_p,)
    flush_file_buffers.restype = ctypes.c_int
    close_handle = kernel32.CloseHandle
    close_handle.argtypes = (ctypes.c_void_p,)
    close_handle.restype = ctypes.c_int

    generic_write = 0x40000000
    share_read_write_delete = 0x00000001 | 0x00000002 | 0x00000004
    open_existing = 3
    file_flag_backup_semantics = 0x02000000
    invalid_handle = ctypes.c_void_p(-1).value
    handle = create_file(
        _windows_directory_handle_path(path),
        generic_write,
        share_read_write_delete,
        None,
        open_existing,
        file_flag_backup_semantics,
        None,
    )
    if handle == invalid_handle:
        raise ctypes.WinError(ctypes.get_last_error())
    flush_error: OSError | None = None
    try:
        if not flush_file_buffers(handle):
            flush_error = ctypes.WinError(ctypes.get_last_error())
            raise flush_error
    finally:
        if not close_handle(handle) and flush_error is None:
            raise ctypes.WinError(ctypes.get_last_error())


def fsync_directory(path: Path) -> None:
    """Persist directory-entry changes or fail when flushing is unavailable."""
    if os.name == "nt":
        _fsync_directory_windows(path)
        return
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def mkdir_durable(
    path: Path,
    *,
    mode: int = 0o777,
    parents: bool = True,
    exist_ok: bool = True,
) -> None:
    """Create a directory and persist every newly created ancestor entry."""
    missing: list[Path] = []
    cursor = path
    while not cursor.exists():
        missing.append(cursor)
        parent = cursor.parent
        if parent == cursor:
            break
        cursor = parent

    path.mkdir(mode=mode, parents=parents, exist_ok=exist_ok)
    if not missing:
        return

    for directory in reversed(missing):
        fsync_directory(directory)
        fsync_directory(directory.parent)


def fsync_file(path: Path) -> None:
    """Flush one regular non-symlink file's content and metadata."""
    try:
        mode = path.lstat().st_mode
    except OSError as error:
        raise DurableIoError(f"file is unavailable: {path}: {error}") from error
    if not stat.S_ISREG(mode):
        raise DurableIoError(f"file must be a regular non-symlink: {path}")
    open_mode = "r+b" if os.name == "nt" else "rb"
    try:
        with path.open(open_mode) as handle:
            os.fsync(handle.fileno())
    except OSError as error:
        raise DurableIoError(f"cannot flush file {path}: {error}") from error


def parse_mode(value: str) -> int:
    try:
        mode = int(value, 8)
    except ValueError as error:
        raise argparse.ArgumentTypeError("mode must be an octal integer") from error
    if mode < 0 or mode > 0o777:
        raise argparse.ArgumentTypeError("mode must be between 000 and 777")
    return mode


def run_self_test() -> int:
    tests = 0
    with tempfile.TemporaryDirectory(prefix="dcent-durable-io-") as temporary:
        root = Path(temporary)
        nested = root / "one" / "two"
        mkdir_durable(nested, mode=0o700, parents=True, exist_ok=False)
        assert nested.is_dir()
        tests += 1

        evidence = nested / "evidence.txt"
        evidence.write_text("durable evidence\n", encoding="utf-8")
        fsync_file(evidence)
        assert evidence.read_text(encoding="utf-8") == "durable evidence\n"
        tests += 1

        fsync_directory(nested)
        tests += 1

    print(f"durable file I/O self-test passed: {tests} scenarios")
    return 0


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    subparsers = parser.add_subparsers(dest="command")

    mkdir_parser = subparsers.add_parser("mkdir")
    mkdir_parser.add_argument("path", type=Path)
    mkdir_parser.add_argument("--mode", type=parse_mode, default=0o777)
    mkdir_parser.add_argument("--parents", action="store_true")
    mkdir_parser.add_argument("--exist-ok", action="store_true")

    files_parser = subparsers.add_parser("fsync-files")
    files_parser.add_argument("paths", nargs="+", type=Path)

    directories_parser = subparsers.add_parser("fsync-directories")
    directories_parser.add_argument("paths", nargs="+", type=Path)

    args = parser.parse_args(argv)
    if args.self_test:
        if args.command is not None:
            parser.error("--self-test cannot be combined with a command")
    elif args.command is None:
        parser.error("a command is required")
    return args


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    if args.self_test:
        return run_self_test()
    try:
        if args.command == "mkdir":
            mkdir_durable(
                args.path,
                mode=args.mode,
                parents=args.parents,
                exist_ok=args.exist_ok,
            )
        elif args.command == "fsync-files":
            for path in args.paths:
                fsync_file(path)
        elif args.command == "fsync-directories":
            for path in args.paths:
                fsync_directory(path)
        else:  # pragma: no cover - argparse owns command admission.
            raise DurableIoError(f"unsupported command: {args.command}")
    except (DurableIoError, OSError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        print("durable_io=fail", file=sys.stderr)
        return 1
    print("durable_io=pass")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
