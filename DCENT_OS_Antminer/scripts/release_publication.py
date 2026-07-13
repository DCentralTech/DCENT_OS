#!/usr/bin/env python3
"""Atomically publish exact release files without following attacker-chosen paths."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import sys
import tempfile


MAX_METADATA_BYTES = 1024 * 1024


class PublicationError(RuntimeError):
    pass


def fail(message: str) -> "NoReturn":
    raise PublicationError(message)


def is_reparse(metadata: os.stat_result) -> bool:
    attributes = getattr(metadata, "st_file_attributes", 0)
    marker = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(attributes & marker)


def lexical_absolute(value: str | os.PathLike[str]) -> Path:
    return Path(os.path.abspath(os.fspath(value)))


def inspect_components(path: Path, label: str) -> Path:
    path = lexical_absolute(path)
    current = Path(path.anchor)
    for component in path.parts[1:]:
        current /= component
        try:
            metadata = os.lstat(current)
        except OSError as error:
            fail(f"cannot inspect {label} path {current}: {error}")
        if stat.S_ISLNK(metadata.st_mode) or is_reparse(metadata):
            fail(f"{label} path contains a symlink or reparse point: {current}")
    return path


def validate_output(value: str) -> tuple[Path, Path]:
    output = lexical_absolute(value)
    if output.name in ("", ".", "..") or any(
        character in output.name for character in ("/", "\\", "\0", "\r", "\n")
    ):
        fail("output must have a canonical flat basename")
    parent = inspect_components(output.parent, "output directory")
    parent_status = os.lstat(parent)
    if not stat.S_ISDIR(parent_status.st_mode) or is_reparse(parent_status):
        fail("output parent must be a non-reparse directory")
    try:
        os.lstat(output)
    except FileNotFoundError:
        pass
    except OSError as error:
        fail(f"cannot inspect output leaf: {error}")
    else:
        fail(f"output already exists: {output}")
    return output, parent


def stable_fields(metadata: os.stat_result) -> tuple[int, ...]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def same_identity(left: os.stat_result, right: os.stat_result) -> bool:
    if left.st_ino and right.st_ino:
        return (left.st_dev, left.st_ino) == (right.st_dev, right.st_ino)
    return stable_fields(left) == stable_fields(right)


def open_source(value: str) -> tuple[Path, int, os.stat_result]:
    source = inspect_components(Path(value), "source")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(source, flags)
    except OSError as error:
        fail(f"cannot open source: {source}: {error}")
    metadata = os.fstat(descriptor)
    if not stat.S_ISREG(metadata.st_mode) or is_reparse(metadata):
        os.close(descriptor)
        fail(f"source must be a non-reparse regular file: {source}")
    if getattr(metadata, "st_nlink", 1) != 1:
        os.close(descriptor)
        fail(f"source must have exactly one hard link: {source}")
    return source, descriptor, metadata


def publish(args: argparse.Namespace) -> None:
    output, parent = validate_output(args.output)
    if args.command == "copy" and args.expected_sha256 is not None:
        if (
            len(args.expected_sha256) != 64
            or any(character not in "0123456789abcdef" for character in args.expected_sha256)
        ):
            fail("expected SHA-256 must be 64 lowercase hexadecimal characters")
    source_path: Path | None = None
    source_descriptor: int | None = None
    source_before: os.stat_result | None = None
    if args.command == "copy":
        source_path, source_descriptor, source_before = open_source(args.source)

    temporary_descriptor, temporary_value = tempfile.mkstemp(
        prefix=f".{output.name}.publish.", dir=parent
    )
    temporary = Path(temporary_value)
    published = False
    digest = hashlib.sha256()
    size = 0
    try:
        os.chmod(temporary, 0o644)
        with os.fdopen(temporary_descriptor, "wb", closefd=True) as destination:
            if source_descriptor is not None:
                while True:
                    chunk = os.read(source_descriptor, 1024 * 1024)
                    if not chunk:
                        break
                    destination.write(chunk)
                    digest.update(chunk)
                    size += len(chunk)
            else:
                while True:
                    chunk = sys.stdin.buffer.read(64 * 1024)
                    if not chunk:
                        break
                    size += len(chunk)
                    if size > MAX_METADATA_BYTES:
                        fail("release metadata exceeds the one-MiB publication bound")
                    destination.write(chunk)
                    digest.update(chunk)
            destination.flush()
            os.fsync(destination.fileno())

        observed_digest = digest.hexdigest()
        if args.command == "copy" and args.expected_sha256:
            if args.expected_sha256 != observed_digest:
                fail("copied source digest does not match signed closure evidence")

        if source_descriptor is not None:
            source_after = os.fstat(source_descriptor)
            path_after = os.lstat(source_path)
            if (
                stable_fields(source_before) != stable_fields(source_after)
                or not stat.S_ISREG(path_after.st_mode)
                or is_reparse(path_after)
                or not same_identity(source_after, path_after)
                or size != source_after.st_size
            ):
                fail("source changed or was replaced while being copied")

        try:
            os.link(temporary, output, follow_symlinks=False)
        except FileExistsError:
            fail(f"output appeared during publication: {output}")
        except OSError as error:
            fail(f"cannot publish output without replacement: {output}: {error}")
        published = True
        os.unlink(temporary)
        final_status = os.lstat(output)
        if (
            not stat.S_ISREG(final_status.st_mode)
            or is_reparse(final_status)
            or getattr(final_status, "st_nlink", 1) != 1
            or final_status.st_size != size
        ):
            fail(f"published output identity is invalid: {output}")
        if os.name == "posix":
            directory = os.open(parent, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
    except BaseException:
        if published:
            try:
                os.unlink(output)
            except OSError:
                pass
        raise
    finally:
        if source_descriptor is not None:
            os.close(source_descriptor)
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass

    print(
        json.dumps(
            {"path": str(output), "sha256": observed_digest, "size": size},
            sort_keys=True,
            separators=(",", ":"),
        )
    )


def query_result(args: argparse.Namespace) -> None:
    raw = sys.stdin.buffer.read(16 * 1024 + 1)
    if len(raw) > 16 * 1024:
        fail("publication result exceeds the query bound")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"publication result is not valid JSON: {error}")
    if not isinstance(value, dict) or set(value) != {"path", "sha256", "size"}:
        fail("publication result has an invalid schema")
    if not isinstance(value["path"], str) or not value["path"]:
        fail("publication result path is invalid")
    if (
        not isinstance(value["sha256"], str)
        or len(value["sha256"]) != 64
        or any(character not in "0123456789abcdef" for character in value["sha256"])
    ):
        fail("publication result digest is invalid")
    if isinstance(value["size"], bool) or not isinstance(value["size"], int) or value["size"] < 0:
        fail("publication result size is invalid")
    print(value[args.field])


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    copy = commands.add_parser("copy")
    copy.add_argument("--source", required=True)
    copy.add_argument("--output", required=True)
    copy.add_argument("--expected-sha256")
    copy.set_defaults(function=publish)
    stdin = commands.add_parser("stdin")
    stdin.add_argument("--output", required=True)
    stdin.set_defaults(function=publish)
    query = commands.add_parser("query-result")
    query.add_argument("--field", choices=("path", "sha256", "size"), required=True)
    query.set_defaults(function=query_result)
    return root


def main() -> int:
    try:
        args = parser().parse_args()
        args.function(args)
        return 0
    except (PublicationError, OSError) as error:
        print(f"ERROR: release publication: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
