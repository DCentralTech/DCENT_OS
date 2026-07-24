#!/usr/bin/env python3
"""Create and durably publish one deterministic GNU-tar release envelope."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import sys
import tempfile
from typing import NoReturn


SCRIPT_DIR = Path(__file__).resolve().parent
if os.fspath(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, os.fspath(SCRIPT_DIR))

from atomic_publish_file import (  # noqa: E402
    PublishError,
    atomic_publish,
    report_after_commit,
    warn_after_commit,
)
from release_publication import (  # noqa: E402
    inspect_components,
    is_reparse,
    validate_output,
)


class ArchiveError(RuntimeError):
    """The archive staging or publication boundary failed closed."""


class ArchiveSignal(ArchiveError):
    """Termination was requested before the archive became authoritative."""


def fail(message: str) -> NoReturn:
    raise ArchiveError(message)


def stable_state(metadata: os.stat_result) -> tuple[int, ...]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_mode,
        getattr(metadata, "st_nlink", 1),
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


class SignalGuard:
    """Forward termination to GNU tar and defer it across the commit boundary."""

    def __init__(self) -> None:
        self.child: subprocess.Popen[bytes] | None = None
        self.committed = False
        self.pending: int | None = None
        self.previous: dict[int, object] = {}

    def _handler(self, signum: int, _frame: object) -> None:
        self.pending = signum
        child = self.child
        if child is not None and child.poll() is None:
            try:
                child.terminate()
            except OSError:
                pass

    def __enter__(self) -> SignalGuard:
        handled = {
            value
            for name in ("SIGINT", "SIGTERM", "SIGHUP", "SIGBREAK")
            if (value := getattr(signal, name, None)) is not None
        }
        for signum in handled:
            try:
                self.previous[signum] = signal.getsignal(signum)
                signal.signal(signum, self._handler)
            except (AttributeError, OSError, ValueError):
                self.previous.pop(signum, None)
        return self

    def refuse_pending(self) -> None:
        if self.pending is not None:
            raise ArchiveSignal(
                f"received signal {self.pending} before archive publication"
            )

    def mark_committed(self) -> None:
        self.committed = True

    def __exit__(self, kind: object, _value: object, _traceback: object) -> None:
        for signum, handler in self.previous.items():
            try:
                signal.signal(signum, handler)
            except (OSError, ValueError):
                pass
        if self.committed and self.pending is not None:
            warn_after_commit(
                f"WARNING: ignored signal {self.pending} after durable "
                "release-envelope commit"
            )
        elif kind is None and self.pending is not None:
            raise ArchiveSignal(
                f"received signal {self.pending} before archive publication"
            )


def validate_epoch(value: str) -> str:
    if not value or any(character not in "0123456789" for character in value):
        fail("source-date epoch must be an unsigned integer")
    return value


def validate_top(value: str) -> str:
    if value in ("", ".", "..") or any(
        character in value for character in ("/", "\\", "\0", "\r", "\n")
    ):
        fail("archive root must be one canonical flat basename")
    return value


def chmod_member(path: Path, mode: int) -> None:
    if os.name == "nt":
        os.chmod(path, mode)
    else:
        os.chmod(path, mode, follow_symlinks=False)


def tar_path(path: Path) -> str:
    value = os.fspath(path)
    return value.replace("\\", "/") if os.name == "nt" else value


def normalize_and_snapshot(root: Path) -> dict[str, tuple[int, ...]]:
    snapshot: dict[str, tuple[int, ...]] = {}
    pending = [root]
    while pending:
        current = pending.pop()
        relative = current.relative_to(root.parent).as_posix()
        try:
            metadata = current.lstat()
        except OSError as error:
            fail(f"cannot inspect archive member {relative}: {error}")
        if stat.S_ISLNK(metadata.st_mode) or is_reparse(metadata):
            fail(f"archive member is a symlink or reparse point: {relative}")
        if stat.S_ISDIR(metadata.st_mode):
            try:
                chmod_member(current, 0o755)
                entries = list(os.scandir(current))
            except OSError as error:
                fail(f"cannot normalize or enumerate archive directory {relative}: {error}")
            for entry in reversed(sorted(entries, key=lambda item: item.name)):
                if entry.name in ("", ".", "..") or "/" in entry.name or "\\" in entry.name:
                    fail(f"archive member has an unsafe name below {relative}")
                pending.append(current / entry.name)
        elif stat.S_ISREG(metadata.st_mode):
            if getattr(metadata, "st_nlink", 1) != 1:
                fail(f"multiply-linked archive member is forbidden: {relative}")
            try:
                chmod_member(current, 0o644)
            except OSError as error:
                fail(f"cannot normalize archive member {relative}: {error}")
        else:
            fail(f"unsupported archive member type: {relative}")
        try:
            normalized = current.lstat()
        except OSError as error:
            fail(f"cannot reinspect archive member {relative}: {error}")
        snapshot[relative] = stable_state(normalized)
    return snapshot


def snapshot_without_mutation(root: Path) -> dict[str, tuple[int, ...]]:
    snapshot: dict[str, tuple[int, ...]] = {}
    pending = [root]
    while pending:
        current = pending.pop()
        relative = current.relative_to(root.parent).as_posix()
        try:
            metadata = current.lstat()
        except OSError as error:
            fail(f"cannot reinspect archive member {relative}: {error}")
        if stat.S_ISLNK(metadata.st_mode) or is_reparse(metadata):
            fail(f"archive member became a symlink or reparse point: {relative}")
        if stat.S_ISDIR(metadata.st_mode):
            try:
                entries = list(os.scandir(current))
            except OSError as error:
                fail(f"cannot re-enumerate archive directory {relative}: {error}")
            for entry in reversed(sorted(entries, key=lambda item: item.name)):
                pending.append(current / entry.name)
        elif not stat.S_ISREG(metadata.st_mode):
            fail(f"archive member changed to an unsupported type: {relative}")
        snapshot[relative] = stable_state(metadata)
    return snapshot


def require_gnu_tar(executable: str) -> None:
    try:
        result = subprocess.run(
            [executable, "--version"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
    except OSError as error:
        fail(f"GNU tar is required: {error}")
    if result.returncode != 0 or not result.stdout.startswith(b"tar (GNU tar)"):
        fail("GNU tar is required")


def create_archive(args: argparse.Namespace) -> None:
    epoch = validate_epoch(args.source_date_epoch)
    top = validate_top(args.top)
    output, parent = validate_output(args.output)
    base = inspect_components(Path(args.base), "archive base")
    root = inspect_components(base / top, "archive root")
    try:
        root_status = root.lstat()
    except OSError as error:
        fail(f"archive root is unavailable: {error}")
    if not stat.S_ISDIR(root_status.st_mode) or is_reparse(root_status):
        fail(f"archive root must be a non-reparse directory: {root}")
    require_gnu_tar(args.tar)

    before = normalize_and_snapshot(root)
    temporary_descriptor, temporary_value = tempfile.mkstemp(
        prefix=f".{output.name}.archive-pending.",
        dir=parent,
    )
    temporary = Path(temporary_value)
    temporary_status = os.fstat(temporary_descriptor)
    temporary_identity = (temporary_status.st_dev, temporary_status.st_ino)
    committed = False
    os.close(temporary_descriptor)
    try:
        os.chmod(temporary, 0o644)
        command = [
            args.tar,
            "--sort=name",
            "--format=ustar",
            f"--mtime=@{epoch}",
            "--owner=0",
            "--group=0",
            "--numeric-owner",
            "--mode=u+rwX,go+rX,go-w",
            "--force-local",
            "-cf",
            tar_path(temporary),
            "-C",
            tar_path(base),
            top,
        ]
        with SignalGuard() as guard:
            try:
                guard.child = subprocess.Popen(command)
            except OSError as error:
                fail(f"cannot start GNU tar: {error}")
            return_code = guard.child.wait()
            guard.child = None
            guard.refuse_pending()
            if return_code != 0:
                fail(f"GNU tar failed with exit status {return_code}")

            try:
                with temporary.open("r+b") as staged:
                    os.fsync(staged.fileno())
                    final_status = os.fstat(staged.fileno())
            except OSError as error:
                fail(f"cannot flush staged archive bytes: {error}")
            if (
                (final_status.st_dev, final_status.st_ino) != temporary_identity
                or not stat.S_ISREG(final_status.st_mode)
                or getattr(final_status, "st_nlink", 1) != 1
            ):
                fail("staged archive identity changed before publication")
            if snapshot_without_mutation(root) != before:
                fail("archive source tree changed while GNU tar was reading it")
            guard.refuse_pending()
            try:
                atomic_publish(
                    temporary,
                    output,
                    require_directory_sync=True,
                    require_staged_cleanup=True,
                    expected_staged_identity=temporary_identity,
                )
            except PublishError as error:
                fail(f"cannot durably publish archive without replacement: {error}")
            committed = True
            guard.mark_committed()
        report_after_commit(
            (
                json.dumps(
                    {"path": str(output), "size": final_status.st_size},
                    sort_keys=True,
                    separators=(",", ":"),
                ),
            )
        )
    finally:
        if not committed:
            try:
                current = temporary.lstat()
                if (current.st_dev, current.st_ino) == temporary_identity:
                    temporary.unlink()
            except OSError:
                pass


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--output", required=True)
    result.add_argument("--base", required=True)
    result.add_argument("--top", required=True)
    result.add_argument("--source-date-epoch", required=True)
    result.add_argument("--tar", required=True)
    result.set_defaults(function=create_archive)
    return result


def main() -> int:
    try:
        args = parser().parse_args()
        args.function(args)
        return 0
    except (ArchiveError, OSError) as error:
        print(f"ERROR: release envelope archive: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
