#!/usr/bin/env python3
"""Sign exact pinned receipt bytes and durably publish one Ed25519 signature."""

from __future__ import annotations

import argparse
from contextlib import ExitStack
import hashlib
import os
from pathlib import Path
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
from typing import NoReturn


SCRIPT_DIR = Path(__file__).resolve().parent
if os.fspath(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, os.fspath(SCRIPT_DIR))

import release_set_publication as release_io  # noqa: E402


MAX_RECEIPT_BYTES = 16 * 1024 * 1024
MAX_KEY_BYTES = 1024 * 1024


class SigningError(RuntimeError):
    """The exact signing or publication boundary failed closed."""


class SigningSignal(SigningError):
    """Termination requested before a signature became authoritative."""

    def __init__(self, signum: int) -> None:
        self.signum = signum
        super().__init__(f"received signal {signum} before signature publication")


class SigningSignalGuard:
    """Defer termination until scratch is retired or publication is committed."""

    def __init__(self) -> None:
        self.committed = False
        self.pending: int | None = None
        self.previous: dict[int, object] = {}

    def _handler(self, signum: int, _frame: object) -> None:
        self.pending = signum

    def __enter__(self) -> SigningSignalGuard:
        handled_signals = {
            value
            for name in ("SIGINT", "SIGTERM", "SIGHUP", "SIGBREAK")
            if (value := getattr(signal, name, None)) is not None
        }
        for signum in handled_signals:
            try:
                self.previous[signum] = signal.getsignal(signum)
                signal.signal(signum, self._handler)
            except (AttributeError, OSError, ValueError):
                self.previous.pop(signum, None)
        return self

    def refuse_pending_before_commit(self) -> None:
        if self.pending is not None:
            raise SigningSignal(self.pending)

    def mark_committed(self) -> None:
        self.committed = True

    def __exit__(self, kind: object, _value: object, _traceback: object) -> None:
        for signum, handler in self.previous.items():
            try:
                signal.signal(signum, handler)
            except (OSError, ValueError):
                pass
        if self.committed and self.pending is not None:
            release_io.warn_after_commit(
                f"WARNING: ignored signal {self.pending} after durable "
                "release-receipt signature commit"
            )
        elif kind is None and self.pending is not None:
            raise SigningSignal(self.pending)


def fail(message: str) -> NoReturn:
    raise SigningError(message)


def is_reparse(metadata: os.stat_result) -> bool:
    flag = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(getattr(metadata, "st_file_attributes", 0) & flag)


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


class PinnedFile:
    """Keep one exact regular-file identity open across an OpenSSL operation."""

    def __init__(
        self,
        path: Path,
        label: str,
        maximum_size: int,
        *,
        private_key: bool = False,
        durable: bool = False,
    ) -> None:
        self.path = path
        self.label = label
        self.maximum_size = maximum_size
        self.private_key = private_key
        self.durable = durable
        self.descriptor = -1
        self._windows_handle: int | None = None
        self._close_windows = None
        self.initial: os.stat_result | None = None
        self.sha256 = ""
        self.size = 0
        self._open()

    def _open(self) -> None:
        try:
            initial = os.lstat(self.path)
        except FileNotFoundError:
            fail(f"{self.label} is missing: {self.path}")
        if (
            not stat.S_ISREG(initial.st_mode)
            or is_reparse(initial)
            or getattr(initial, "st_nlink", 1) != 1
        ):
            fail(
                f"{self.label} must be a single-link non-reparse regular file: "
                f"{self.path}"
            )
        if initial.st_size <= 0 or initial.st_size > self.maximum_size:
            fail(f"{self.label} has an invalid size: {initial.st_size}")
        if self.private_key and os.name == "posix" and stat.S_IMODE(initial.st_mode) & 0o077:
            fail(f"private signing key must not grant group/other permissions: {self.path}")
        if self.private_key and os.name == "nt":
            try:
                release_io.require_private_windows_acl(
                    self.path,
                    "private signing key",
                    require_protected=False,
                )
            except release_io.ReleaseSetError as error:
                fail(str(error))

        if os.name == "nt":
            try:
                self._windows_handle, self._close_windows = (
                    release_io.open_pinned_windows_file(
                        self.path,
                        (initial.st_dev, initial.st_ino),
                        self.label,
                        write_data=self.durable,
                        share_write=False,
                        request_delete_access=False,
                    )
                )
            except (OSError, release_io.ReleaseSetError) as error:
                fail(f"cannot exclusively pin {self.label}: {error}")

        flags = (
            os.O_RDONLY
            | getattr(os, "O_BINARY", 0)
            | getattr(os, "O_NOFOLLOW", 0)
            | getattr(os, "O_NONBLOCK", 0)
        )
        try:
            self.descriptor = os.open(self.path, flags)
            opened = os.fstat(self.descriptor)
            if (
                not stat.S_ISREG(opened.st_mode)
                or is_reparse(opened)
                or getattr(opened, "st_nlink", 1) != 1
                or stable_state(opened) != stable_state(initial)
            ):
                fail(f"{self.label} changed before it could be opened")
            self.initial = initial
            self.sha256, self.size = self._hash_twice()
            self.revalidate()
        except BaseException:
            self.close()
            raise

    def _hash_once(self) -> tuple[str, int]:
        os.lseek(self.descriptor, 0, os.SEEK_SET)
        digest = hashlib.sha256()
        observed = 0
        while True:
            chunk = os.read(self.descriptor, 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
            observed += len(chunk)
            if observed > self.maximum_size:
                fail(f"{self.label} exceeded its size bound while being read")
        return digest.hexdigest(), observed

    def _hash_twice(self) -> tuple[str, int]:
        first = self._hash_once()
        second = self._hash_once()
        if first != second:
            fail(f"{self.label} changed while its bytes were pinned")
        return first

    def revalidate(self) -> None:
        if self.initial is None or self.descriptor < 0:
            fail(f"{self.label} pin is not active")
        digest, size = self._hash_twice()
        opened = os.fstat(self.descriptor)
        try:
            current = os.lstat(self.path)
        except FileNotFoundError:
            fail(f"{self.label} pathname disappeared while pinned")
        if (
            digest != self.sha256
            or size != self.size
            or stable_state(opened) != stable_state(self.initial)
            or stable_state(current) != stable_state(self.initial)
        ):
            fail(f"{self.label} changed while the signature was prepared")
        if self.private_key and os.name == "nt":
            try:
                release_io.require_private_windows_acl(
                    self.path,
                    "private signing key",
                    require_protected=False,
                )
            except release_io.ReleaseSetError as error:
                fail(str(error))

    def openssl_path(self) -> str:
        if os.name == "nt":
            return os.fspath(self.path)
        proc_path = Path(f"/proc/self/fd/{self.descriptor}")
        if proc_path.exists():
            return os.fspath(proc_path)
        return f"/dev/fd/{self.descriptor}"

    def pass_fds(self) -> tuple[int, ...]:
        return (self.descriptor,) if os.name == "posix" else ()

    def read_bytes(self) -> bytes:
        os.lseek(self.descriptor, 0, os.SEEK_SET)
        chunks: list[bytes] = []
        observed = 0
        while True:
            chunk = os.read(self.descriptor, min(64 * 1024, self.maximum_size + 1))
            if not chunk:
                break
            chunks.append(chunk)
            observed += len(chunk)
            if observed > self.maximum_size:
                fail(f"{self.label} exceeded its size bound while being copied")
        content = b"".join(chunks)
        if (
            len(content) != self.size
            or hashlib.sha256(content).hexdigest() != self.sha256
        ):
            fail(f"{self.label} bytes changed while being copied")
        return content

    def flush(self) -> None:
        if os.name == "nt":
            if self._windows_handle is None:
                fail(f"{self.label} Windows pin is not active")
            release_io.flush_windows_file_handle(self._windows_handle)
        else:
            os.fsync(self.descriptor)

    def close(self) -> None:
        if self.descriptor >= 0:
            try:
                os.close(self.descriptor)
            finally:
                self.descriptor = -1
        if self._close_windows is not None:
            self._close_windows()
            self._close_windows = None
            self._windows_handle = None

    def __enter__(self) -> PinnedFile:
        return self

    def __exit__(self, *_unused: object) -> None:
        self.close()


def run_openssl(
    openssl: str,
    arguments: list[str],
    pinned: tuple[PinnedFile, ...],
    failure_message: str,
) -> None:
    pass_fds = tuple(descriptor for item in pinned for descriptor in item.pass_fds())
    for item in pinned:
        os.lseek(item.descriptor, 0, os.SEEK_SET)
    options: dict[str, object] = {
        "stdout": subprocess.DEVNULL,
        "stderr": subprocess.PIPE,
        "check": False,
    }
    if os.name == "posix":
        options["pass_fds"] = pass_fds
    result = subprocess.run([openssl, *arguments], **options)
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", "replace").strip()
        fail(f"{failure_message}: {detail or 'OpenSSL returned failure'}")


def resolve_signature_output(
    value: str, subject: str = "release receipt"
) -> tuple[Path, Path]:
    output = Path(os.path.abspath(os.fspath(Path(value).expanduser())))
    try:
        release_io.validate_flat_name(
            output.name,
            f"{subject} signature output name",
            allow_descriptor=True,
        )
        parent, _ = release_io.safe_directory(
            output.parent, f"{subject} signature output parent"
        )
    except release_io.ReleaseSetError as error:
        fail(str(error))
    return output, parent


def reconcile_published_signature(
    path: Path,
    expected: bytes,
    openssl: str,
    receipt: PinnedFile,
    public_key: PinnedFile,
) -> None:
    """Accept only the exact deterministic signature already made authoritative."""

    subject = receipt.label
    with PinnedFile(
        path, f"published {subject} signature", 64, durable=True
    ) as published:
        if published.initial is None:
            fail(f"published {subject} signature pin is incomplete")
        if os.name == "posix" and stat.S_IMODE(published.initial.st_mode) != 0o644:
            fail(f"published {subject} signature must use canonical mode 0644")
        if os.name == "nt":
            try:
                release_io.require_public_windows_acl(
                    path, f"published {subject} signature"
                )
            except release_io.ReleaseSetError as error:
                fail(str(error))
        if published.size != 64 or published.read_bytes() != expected:
            fail(f"existing {subject} signature disagrees with verified bytes")
        run_openssl(
            openssl,
            [
                "pkeyutl",
                "-verify",
                "-rawin",
                "-pubin",
                "-inkey",
                public_key.openssl_path(),
                "-sigfile",
                published.openssl_path(),
                "-in",
                receipt.openssl_path(),
            ],
            (receipt, public_key, published),
            f"published {subject} signature verification failed",
        )
        receipt.revalidate()
        public_key.revalidate()
        published.revalidate()
        published.flush()
        published.revalidate()
        release_io.fsync_directory(path.parent)
        if os.name == "nt":
            try:
                release_io.require_public_windows_acl(
                    path, f"published {subject} signature"
                )
            except release_io.ReleaseSetError as error:
                fail(str(error))


def retire_signing_control(control: Path, pending_signature: Path) -> None:
    if pending_signature.exists() or pending_signature.is_symlink():
        pending_signature.unlink()
    control.rmdir()


def sign_receipt(args: argparse.Namespace) -> None:
    subject = getattr(args, "subject", "release receipt")
    maximum_input_bytes = getattr(
        args, "maximum_input_bytes", MAX_RECEIPT_BYTES
    )
    manifest_argument = getattr(args, "manifest", None)
    manifest_label = getattr(args, "manifest_label", f"{subject} manifest")
    maximum_manifest_bytes = getattr(
        args, "maximum_manifest_bytes", MAX_RECEIPT_BYTES
    )
    validate_manifest = getattr(args, "validate_manifest", None)
    durable_input = bool(getattr(args, "durable_input", False))
    durable_manifest = bool(getattr(args, "durable_manifest", False))
    control_prefix = getattr(
        args, "control_prefix", "dcentos-receipt-signing-"
    )

    openssl = shutil.which("openssl")
    if openssl is None:
        fail(f"openssl is required for {subject} signing")

    receipt_path = Path(args.receipt).expanduser().absolute()
    private_key_path = Path(args.private_key).expanduser().absolute()
    public_key_path = Path(args.public_key).expanduser().absolute()
    requested_signature = args.signature or f"{receipt_path}.sig"
    signature_path, _signature_parent = resolve_signature_output(
        requested_signature, subject
    )
    manifest_path = (
        Path(manifest_argument).expanduser().absolute()
        if manifest_argument is not None
        else None
    )

    with SigningSignalGuard() as termination:
        control = Path(tempfile.mkdtemp(prefix=control_prefix))
        pending_signature = control / "signature.pending"
        committed = False
        try:
            termination.refuse_pending_before_commit()
            os.chmod(control, 0o700)
            if os.name == "nt":
                release_io.set_windows_directory_acl(
                    control, release_io.WINDOWS_PRIVATE_DIRECTORY_SDDL
                )
                release_io.require_private_windows_acl(
                    control, "receipt signing control"
                )
            termination.refuse_pending_before_commit()
            with ExitStack() as pinned_stack:
                receipt = pinned_stack.enter_context(
                    PinnedFile(
                        receipt_path,
                        subject,
                        maximum_input_bytes,
                        durable=durable_input,
                    )
                )
                private_key = pinned_stack.enter_context(
                    PinnedFile(
                        private_key_path,
                        "private signing key",
                        MAX_KEY_BYTES,
                        private_key=True,
                    )
                )
                public_key = pinned_stack.enter_context(
                    PinnedFile(
                        public_key_path, "trusted public key", MAX_KEY_BYTES
                    )
                )
                manifest = (
                    pinned_stack.enter_context(
                        PinnedFile(
                            manifest_path,
                            manifest_label,
                            maximum_manifest_bytes,
                            durable=durable_manifest,
                        )
                    )
                    if manifest_path is not None
                    else None
                )

                def revalidate_precommit() -> None:
                    termination.refuse_pending_before_commit()
                    receipt.revalidate()
                    private_key.revalidate()
                    public_key.revalidate()
                    if manifest is not None:
                        manifest.revalidate()
                    if validate_manifest is not None:
                        if manifest is None:
                            fail(f"{manifest_label} pin is required")
                        validate_manifest(receipt, manifest)
                    durable_parents: set[Path] = set()
                    if durable_input:
                        receipt.flush()
                        durable_parents.add(receipt.path.parent)
                    if durable_manifest:
                        if manifest is None:
                            fail(f"{manifest_label} durability pin is required")
                        manifest.flush()
                        durable_parents.add(manifest.path.parent)
                    for durable_parent in durable_parents:
                        release_io.fsync_directory(durable_parent)
                    if durable_input or durable_manifest:
                        receipt.revalidate()
                        private_key.revalidate()
                        public_key.revalidate()
                        if manifest is not None:
                            manifest.revalidate()
                        if validate_manifest is not None:
                            validate_manifest(receipt, manifest)
                    termination.refuse_pending_before_commit()

                revalidate_precommit()
                run_openssl(
                    openssl,
                    [
                        "pkeyutl",
                        "-sign",
                        "-rawin",
                        "-inkey",
                        private_key.openssl_path(),
                        "-in",
                        receipt.openssl_path(),
                        "-out",
                        os.fspath(pending_signature),
                    ],
                    (receipt, private_key),
                    f"{subject} signing failed",
                )
                termination.refuse_pending_before_commit()
                with PinnedFile(
                    pending_signature, "prepared release signature", 64
                ) as prepared_signature:
                    if prepared_signature.size != 64:
                        fail("Ed25519 release signature must be exactly 64 bytes")
                    run_openssl(
                        openssl,
                        [
                            "pkeyutl",
                            "-verify",
                            "-rawin",
                            "-pubin",
                            "-inkey",
                            public_key.openssl_path(),
                            "-sigfile",
                            prepared_signature.openssl_path(),
                            "-in",
                            receipt.openssl_path(),
                        ],
                        (receipt, public_key, prepared_signature),
                        f"{subject} signature does not verify against the "
                        "trusted public key",
                    )
                    termination.refuse_pending_before_commit()
                    receipt.revalidate()
                    private_key.revalidate()
                    public_key.revalidate()
                    if manifest is not None:
                        manifest.revalidate()
                    if validate_manifest is not None:
                        validate_manifest(receipt, manifest)
                    prepared_signature.revalidate()
                    signature_bytes = prepared_signature.read_bytes()
                    if len(signature_bytes) != 64:
                        fail("prepared Ed25519 signature length changed")

                termination.refuse_pending_before_commit()
                try:
                    os.lstat(signature_path)
                except FileNotFoundError:
                    output_exists = False
                except OSError as error:
                    fail(f"cannot inspect {subject} signature output: {error}")
                else:
                    output_exists = True

                if output_exists:
                    revalidate_precommit()
                    reconcile_published_signature(
                        signature_path,
                        signature_bytes,
                        openssl,
                        receipt,
                        public_key,
                    )
                    revalidate_precommit()
                else:
                    try:
                        release_io.publish_regular_file_noreplace(
                            signature_path,
                            signature_bytes,
                            mode=0o644,
                            before_commit=revalidate_precommit,
                        )
                    except release_io.PublicationCollision as publication_error:
                        try:
                            reconcile_published_signature(
                                signature_path,
                                signature_bytes,
                                openssl,
                                receipt,
                                public_key,
                            )
                        except SigningError as reconciliation_error:
                            fail(
                                f"{publication_error}; competing output "
                                f"reconciliation failed: {reconciliation_error}"
                            )
                        revalidate_precommit()
                committed = True
                termination.mark_committed()
                reconcile_published_signature(
                    signature_path,
                    signature_bytes,
                    openssl,
                    receipt,
                    public_key,
                )
                if manifest is not None:
                    manifest.revalidate()
                if validate_manifest is not None:
                    validate_manifest(receipt, manifest)
        finally:
            try:
                retire_signing_control(control, pending_signature)
            except OSError as error:
                message = f"cannot retire receipt signing control: {error}"
                if committed:
                    release_io.warn_after_commit(f"WARNING: {message}")
                elif sys.exc_info()[0] is None:
                    fail(message)
                else:
                    print(f"ERROR: {message}", file=sys.stderr)

        if not committed:
            fail(f"{subject} signature was not published")
        success_message = getattr(
            args,
            "success_message",
            f"Signed release receipt: {receipt_path} -> {signature_path}",
        )
        release_io.report_after_commit([success_message])


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("receipt")
    root.add_argument("private_key")
    root.add_argument("public_key")
    root.add_argument("signature", nargs="?")
    return root


def main() -> int:
    os.umask(0o077)
    try:
        sign_receipt(parser().parse_args())
        return 0
    except (
        SigningError,
        OSError,
        ValueError,
        release_io.ReleaseSetError,
        release_io.DirectoryPublishError,
        release_io.PublishError,
    ) as error:
        print(f"ERROR: release receipt signing: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
