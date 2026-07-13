#!/usr/bin/env python3
"""Seal and atomically publish an exact, capability-owned release directory."""

from __future__ import annotations

import argparse
import ctypes
import errno
import hashlib
import json
import os
from pathlib import Path
import secrets
import stat
import sys
import tempfile
import unicodedata
from typing import NoReturn


CAPABILITY_SCHEMA = "dcentos.release-set-capability.v1"
FILES_SCHEMA = "dcentos.release-set-files.v1"
STAGE_SCHEMA = "dcentos.release-set-stage.v1"
RESULT_SCHEMA = "dcentos.release-set-publication-result.v1"
DESCRIPTOR_NAME = ".dcent-release-set.json"
MAX_JSON_BYTES = 1024 * 1024
MAX_FILES = 4096


class ReleaseSetError(RuntimeError):
    pass


def fail(message: str) -> NoReturn:
    raise ReleaseSetError(message)


def canonical_json(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def is_reparse(metadata: os.stat_result) -> bool:
    marker = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(getattr(metadata, "st_file_attributes", 0) & marker)


def absolute(value: str | os.PathLike[str]) -> Path:
    return Path(os.path.abspath(os.fspath(value)))


def inspect_existing_components(path: Path, label: str) -> Path:
    path = absolute(path)
    current = Path(path.anchor)
    for part in path.parts[1:]:
        current /= part
        try:
            metadata = os.lstat(current)
        except OSError as error:
            fail(f"cannot inspect {label} path {current}: {error}")
        if stat.S_ISLNK(metadata.st_mode) or is_reparse(metadata):
            fail(f"{label} path contains a symlink or reparse point: {current}")
    return path


def safe_directory(
    value: str | os.PathLike[str], label: str
) -> tuple[Path, os.stat_result]:
    path = inspect_existing_components(Path(value), label)
    metadata = os.lstat(path)
    if not stat.S_ISDIR(metadata.st_mode) or is_reparse(metadata):
        fail(f"{label} must be a non-reparse directory: {path}")
    return path, metadata


def validate_flat_name(
    name: object, label: str, *, allow_descriptor: bool = False
) -> str:
    if not isinstance(name, str) or not name:
        fail(f"{label} must be a non-empty string")
    if name in (".", "..") or name != Path(name).name or "/" in name or "\\" in name:
        fail(f"{label} must be a canonical flat name")
    if name == DESCRIPTOR_NAME and not allow_descriptor:
        fail(f"{label} collides with reserved release-set metadata")
    if unicodedata.normalize("NFC", name) != name:
        fail(f"{label} must use NFC Unicode normalization")
    if len(name.encode("utf-8")) > 200:
        fail(f"{label} exceeds the 200-byte portability bound")
    if any(ord(character) < 0x20 or ord(character) == 0x7F for character in name):
        fail(f"{label} contains a control character")
    if any(character in '<>:"|?*' for character in name) or name[-1] in (" ", "."):
        fail(f"{label} is not portable across release hosts")
    return name


def validate_hex(value: object, label: str, length: int) -> str:
    if (
        not isinstance(value, str)
        or len(value) != length
        or any(character not in "0123456789abcdef" for character in value)
    ):
        fail(f"{label} must be {length} lowercase hexadecimal characters")
    return value


def validate_file_entries(files: object, label: str) -> list[dict[str, object]]:
    if not isinstance(files, list) or not files or len(files) > MAX_FILES:
        fail(f"{label} must contain 1..4096 files")
    parsed: list[dict[str, object]] = []
    portable_names: set[str] = set()
    prior = ""
    for index, entry in enumerate(files):
        if not isinstance(entry, dict) or set(entry) != {"name", "sha256", "size"}:
            fail(f"{label} file {index} has an invalid schema")
        name = validate_flat_name(entry["name"], f"{label} file {index} name")
        folded = name.casefold()
        if folded in portable_names:
            fail(f"{label} file names collide across case-insensitive hosts: {name}")
        portable_names.add(folded)
        if prior and name <= prior:
            fail(f"{label} files must be strictly sorted by name")
        prior = name
        digest = validate_hex(entry["sha256"], f"{label} file {name} digest", 64)
        size = entry["size"]
        if isinstance(size, bool) or not isinstance(size, int) or size < 0:
            fail(f"{label} file {name} size is invalid")
        parsed.append({"name": name, "sha256": digest, "size": size})
    return parsed


def read_regular_json(path: Path, label: str) -> tuple[dict[str, object], bytes]:
    path = inspect_existing_components(path, label)
    initial = os.lstat(path)
    if (
        not stat.S_ISREG(initial.st_mode)
        or is_reparse(initial)
        or getattr(initial, "st_nlink", 1) != 1
    ):
        fail(f"{label} must be a single-link non-reparse regular file")
    # O_NONBLOCK is inert for regular files and prevents an attacker from
    # wedging the verifier if the leaf is swapped to a FIFO between lstat and
    # open. fstat below remains the authority for the opened object type.
    flags = (
        os.O_RDONLY
        | getattr(os, "O_BINARY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        fail(f"cannot open {label}: {error}")
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or is_reparse(before)
            or getattr(before, "st_nlink", 1) != 1
        ):
            fail(f"{label} must be a single-link non-reparse regular file")
        if (before.st_dev, before.st_ino) != (initial.st_dev, initial.st_ino):
            fail(f"{label} changed before it could be opened")
        if before.st_size > MAX_JSON_BYTES:
            fail(f"{label} exceeds the one-MiB bound")
        raw = b""
        while len(raw) <= MAX_JSON_BYTES:
            chunk = os.read(descriptor, min(64 * 1024, MAX_JSON_BYTES + 1 - len(raw)))
            if not chunk:
                break
            raw += chunk
        after = os.fstat(descriptor)
        current = os.lstat(path)
        if (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
            before.st_ctime_ns,
        ) != (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        ) or (after.st_dev, after.st_ino) != (current.st_dev, current.st_ino):
            fail(f"{label} changed or was replaced while being read")
    finally:
        os.close(descriptor)
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"{label} is not valid JSON: {error}")
    if not isinstance(value, dict):
        fail(f"{label} must contain a JSON object")
    return value, raw


def parse_capability(path: Path) -> dict[str, object]:
    value, _ = read_regular_json(path, "capability descriptor")
    required = {
        "schema",
        "stage_id",
        "capability",
        "stage_parent",
        "stage_path",
        "stage_dev",
        "stage_ino",
    }
    if set(value) != required or value.get("schema") != CAPABILITY_SCHEMA:
        fail("capability descriptor has an invalid schema")
    stage_id = validate_hex(value["stage_id"], "stage ID", 32)
    validate_hex(value["capability"], "capability", 64)
    for field in ("stage_parent", "stage_path"):
        if (
            not isinstance(value[field], str)
            or not value[field]
            or value[field] != str(absolute(value[field]))
        ):
            fail(f"capability {field} must be an absolute canonical path")
    for field in ("stage_dev", "stage_ino"):
        if (
            isinstance(value[field], bool)
            or not isinstance(value[field], int)
            or value[field] < 0
        ):
            fail(f"capability {field} is invalid")
    parent = Path(value["stage_parent"])
    stage = Path(value["stage_path"])
    if stage.parent != parent or stage.name != f".dcent-release-set-stage-{stage_id}":
        fail("capability stage path is not its declared direct randomized child")
    return value


def validate_owned_stage(
    capability: dict[str, object],
) -> tuple[Path, Path, os.stat_result]:
    parent, _ = safe_directory(str(capability["stage_parent"]), "stage parent")
    stage = inspect_existing_components(Path(str(capability["stage_path"])), "stage")
    metadata = os.lstat(stage)
    if not stat.S_ISDIR(metadata.st_mode) or is_reparse(metadata):
        fail("owned stage must be a non-reparse directory")
    if (metadata.st_dev, metadata.st_ino) != (
        capability["stage_dev"],
        capability["stage_ino"],
    ):
        fail("owned stage identity does not match the capability")
    return parent, stage, metadata


def capability_hash(capability: dict[str, object]) -> str:
    return hashlib.sha256(str(capability["capability"]).encode("ascii")).hexdigest()


def parse_files_manifest(path: Path) -> list[dict[str, object]]:
    value, _ = read_regular_json(path, "release-set files manifest")
    if set(value) != {"schema", "files"} or value.get("schema") != FILES_SCHEMA:
        fail("release-set files manifest has an invalid schema")
    return validate_file_entries(value["files"], "release-set manifest")


def parse_stage_descriptor(
    stage: Path, capability: dict[str, object], state: str
) -> dict[str, object]:
    value, raw = read_regular_json(stage / DESCRIPTOR_NAME, "stage descriptor")
    common = {"schema", "state", "stage_id", "capability_sha256"}
    expected = common if state == "building" else common | {"output_name", "files"}
    if (
        set(value) != expected
        or value.get("schema") != STAGE_SCHEMA
        or value.get("state") != state
    ):
        fail(f"stage descriptor is not in exact {state} state")
    if value.get("stage_id") != capability["stage_id"]:
        fail("stage descriptor ID does not match the capability")
    if value.get("capability_sha256") != capability_hash(capability):
        fail("stage descriptor is not owned by the supplied capability")
    if raw != canonical_json(value):
        fail("stage descriptor is not canonically encoded")
    if state == "sealed":
        validate_flat_name(value["output_name"], "release-set output name")
        validate_file_entries(value["files"], "sealed stage")
    return value


def measure_stage_file(path: Path) -> dict[str, object]:
    initial = os.lstat(path)
    if (
        not stat.S_ISREG(initial.st_mode)
        or is_reparse(initial)
        or getattr(initial, "st_nlink", 1) != 1
    ):
        fail(f"staged file must be a single-link non-reparse regular file: {path.name}")
    # See read_regular_json: never block on a raced FIFO before fstat can
    # reject the opened object.
    flags = (
        os.O_RDONLY
        | getattr(os, "O_BINARY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        fail(f"cannot open staged file {path.name}: {error}")
    digest = hashlib.sha256()
    observed_size = 0
    try:
        before = os.fstat(descriptor)
        if (
            not stat.S_ISREG(before.st_mode)
            or is_reparse(before)
            or getattr(before, "st_nlink", 1) != 1
        ):
            fail(
                f"staged file must be a single-link non-reparse regular file: {path.name}"
            )
        if (before.st_dev, before.st_ino) != (initial.st_dev, initial.st_ino):
            fail(f"staged file changed before it could be opened: {path.name}")
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
            observed_size += len(chunk)
        after = os.fstat(descriptor)
        current = os.lstat(path)
        if (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
            before.st_ctime_ns,
        ) != (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        ) or (after.st_dev, after.st_ino) != (current.st_dev, current.st_ino):
            fail(
                f"staged file changed or was replaced while being verified: {path.name}"
            )
        if os.name == "posix":
            os.fsync(descriptor)
    finally:
        os.close(descriptor)
    return {"name": path.name, "sha256": digest.hexdigest(), "size": observed_size}


def hash_stage_file(path: Path, expected: dict[str, object]) -> None:
    observed = measure_stage_file(path)
    if observed["size"] != expected["size"] or observed["sha256"] != expected["sha256"]:
        fail(f"staged file does not match its declared size and SHA-256: {path.name}")


def validate_new_output(value: str, label: str) -> tuple[Path, Path]:
    output = absolute(value)
    validate_flat_name(output.name, f"{label} name", allow_descriptor=True)
    parent, _ = safe_directory(output.parent, f"{label} parent")
    try:
        os.lstat(output)
    except FileNotFoundError:
        pass
    except OSError as error:
        fail(f"cannot inspect {label} leaf: {error}")
    else:
        fail(f"{label} already exists: {output}")
    return output, parent


def publish_regular_file_noreplace(output: Path, content: bytes) -> None:
    temporary_fd, temporary_name = tempfile.mkstemp(
        prefix=f".{output.name}.publish.", dir=output.parent
    )
    temporary = Path(temporary_name)
    linked = False
    try:
        os.chmod(temporary, 0o644)
        with os.fdopen(temporary_fd, "wb", closefd=True) as destination:
            destination.write(content)
            destination.flush()
            os.fsync(destination.fileno())
        try:
            os.link(temporary, output, follow_symlinks=False)
        except FileExistsError:
            fail(f"manifest output appeared during publication: {output}")
        except OSError as error:
            fail(f"cannot publish manifest without replacement: {output}: {error}")
        linked = True
        os.unlink(temporary)
        final = os.lstat(output)
        if (
            not stat.S_ISREG(final.st_mode)
            or is_reparse(final)
            or getattr(final, "st_nlink", 1) != 1
            or final.st_size != len(content)
        ):
            fail("published manifest identity is invalid")
        fsync_directory(output.parent)
    except BaseException:
        if linked:
            try:
                os.unlink(output)
            except OSError:
                pass
        raise
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def inspect_stage_entries(stage: Path) -> set[str]:
    observed_names: set[str] = set()
    portable_names: set[str] = set()
    with os.scandir(stage) as entries:
        for entry in entries:
            name = validate_flat_name(
                entry.name, "staged entry name", allow_descriptor=True
            )
            if name.casefold() in portable_names:
                fail("staged entries collide across case-insensitive hosts")
            portable_names.add(name.casefold())
            # Windows DirEntry.stat() reports st_nlink=0 on some supported
            # Python versions. lstat() provides the authoritative link count.
            metadata = os.lstat(stage / name)
            if (
                not stat.S_ISREG(metadata.st_mode)
                or is_reparse(metadata)
                or getattr(metadata, "st_nlink", 1) != 1
            ):
                fail(
                    f"staged entry is not a single-link non-reparse regular file: {name}"
                )
            observed_names.add(name)
    return observed_names


def verify_exact_stage(stage: Path, descriptor: dict[str, object]) -> None:
    expected_names = {DESCRIPTOR_NAME} | {
        str(entry["name"]) for entry in descriptor["files"]
    }
    observed_names = inspect_stage_entries(stage)
    missing = sorted(expected_names - observed_names)
    extra = sorted(observed_names - expected_names)
    if missing or extra:
        fail(f"release stage is not exact (missing={missing}, extra={extra})")
    for entry in descriptor["files"]:
        hash_stage_file(stage / str(entry["name"]), entry)
    # Flush metadata bytes as well as payload bytes before directory publication.
    stage_value, _ = read_regular_json(stage / DESCRIPTOR_NAME, "stage descriptor")
    if stage_value != descriptor:
        fail("stage descriptor changed during exact-set verification")
    final_names = inspect_stage_entries(stage)
    if final_names != expected_names:
        fail("release stage changed during exact-set verification")
    if os.name == "posix":
        descriptor_fd = os.open(
            stage / DESCRIPTOR_NAME, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
        )
        try:
            os.fsync(descriptor_fd)
        finally:
            os.close(descriptor_fd)


def fsync_directory(path: Path) -> None:
    if os.name != "posix":
        return
    descriptor = os.open(
        path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    )
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def atomic_rename_noreplace(
    stage: Path, output: Path, expected_identity: tuple[int, int]
) -> None:
    if os.name == "nt":
        # Rewalk immediately before the Windows no-replace rename so a replaced
        # parent cannot silently redirect publication after initial validation.
        inspect_existing_components(stage.parent, "stage parent")
        inspect_existing_components(output.parent, "output parent")
        current = os.lstat(stage)
        if (current.st_dev, current.st_ino) != expected_identity:
            fail("owned stage changed before atomic promotion")
        try:
            os.rename(stage, output)
        except FileExistsError:
            fail(f"release-set output already exists: {output}")
        return
    if sys.platform.startswith("linux"):
        libc = ctypes.CDLL(None, use_errno=True)
        renameat2 = getattr(libc, "renameat2", None)
        if renameat2 is None:
            fail("atomic no-replace directory rename is unavailable on this host")
        renameat2.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        renameat2.restype = ctypes.c_int
        directory_flags = (
            os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
        )
        source_parent = os.open(stage.parent, directory_flags)
        output_parent = os.open(output.parent, directory_flags)
        try:
            source_status = os.fstat(source_parent)
            output_status = os.fstat(output_parent)
            if (source_status.st_dev, source_status.st_ino) != (
                os.lstat(stage.parent).st_dev,
                os.lstat(stage.parent).st_ino,
            ) or (output_status.st_dev, output_status.st_ino) != (
                os.lstat(output.parent).st_dev,
                os.lstat(output.parent).st_ino,
            ):
                fail("release-set parent changed before atomic promotion")
            current = os.stat(stage.name, dir_fd=source_parent, follow_symlinks=False)
            if (current.st_dev, current.st_ino) != expected_identity:
                fail("owned stage changed before atomic promotion")
            result = renameat2(
                source_parent,
                os.fsencode(stage.name),
                output_parent,
                os.fsencode(output.name),
                1,
            )
            if result != 0:
                error = ctypes.get_errno()
                if error in (errno.EEXIST, errno.ENOTEMPTY):
                    fail(f"release-set output already exists: {output}")
                fail(f"atomic no-replace directory rename failed: {os.strerror(error)}")
        finally:
            os.close(output_parent)
            os.close(source_parent)
        return
    fail("atomic no-replace directory rename is unsupported on this POSIX host")


def create_stage(args: argparse.Namespace) -> None:
    parent, _ = safe_directory(args.parent, "stage parent")
    for _ in range(32):
        stage_id = secrets.token_hex(16)
        capability_secret = secrets.token_hex(32)
        stage = parent / f".dcent-release-set-stage-{stage_id}"
        try:
            os.mkdir(stage, 0o700)
        except FileExistsError:
            continue
        metadata = os.lstat(stage)
        capability = {
            "schema": CAPABILITY_SCHEMA,
            "stage_id": stage_id,
            "capability": capability_secret,
            "stage_parent": str(parent),
            "stage_path": str(stage),
            "stage_dev": metadata.st_dev,
            "stage_ino": metadata.st_ino,
        }
        descriptor = {
            "schema": STAGE_SCHEMA,
            "state": "building",
            "stage_id": stage_id,
            "capability_sha256": capability_hash(capability),
        }
        descriptor_path = stage / DESCRIPTOR_NAME
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_BINARY", 0)
        fd = os.open(descriptor_path, flags, 0o600)
        try:
            os.write(fd, canonical_json(descriptor))
            os.fsync(fd)
        finally:
            os.close(fd)
        fsync_directory(stage)
        fsync_directory(parent)
        sys.stdout.buffer.write(canonical_json(capability))
        return
    fail("could not allocate a unique release-set stage")


def manifest_stage(args: argparse.Namespace) -> None:
    capability = parse_capability(Path(args.capability_file))
    _, stage, _ = validate_owned_stage(capability)
    parse_stage_descriptor(stage, capability, "building")
    output, _ = validate_new_output(args.output, "manifest output")
    try:
        common = os.path.commonpath(
            (os.path.normcase(str(stage)), os.path.normcase(str(output)))
        )
    except ValueError:
        common = ""
    if common == os.path.normcase(str(stage)):
        fail("manifest output must never be inside the owned stage")

    initial_names = inspect_stage_entries(stage)
    if DESCRIPTOR_NAME not in initial_names:
        fail("building stage descriptor is missing")
    payload_names = sorted(initial_names - {DESCRIPTOR_NAME})
    if not payload_names or len(payload_names) > MAX_FILES:
        fail("building stage must contain 1..4096 payload files")
    files = [measure_stage_file(stage / name) for name in payload_names]
    if inspect_stage_entries(stage) != initial_names:
        fail("building stage changed while its manifest was generated")
    # The descriptor is re-read after payload hashing so capability ownership
    # cannot be swapped unnoticed during manifest generation.
    parse_stage_descriptor(stage, capability, "building")
    manifest = {"schema": FILES_SCHEMA, "files": files}
    content = canonical_json(manifest)
    if len(content) > MAX_JSON_BYTES:
        fail("generated release-set manifest exceeds the one-MiB bound")
    publish_regular_file_noreplace(output, content)
    sys.stdout.buffer.write(content)


def seal_stage(args: argparse.Namespace) -> None:
    capability = parse_capability(Path(args.capability_file))
    _, stage, _ = validate_owned_stage(capability)
    parse_stage_descriptor(stage, capability, "building")
    files = parse_files_manifest(Path(args.manifest))
    output_name = validate_flat_name(args.output_name, "release-set output name")
    candidate = {
        "schema": STAGE_SCHEMA,
        "state": "sealed",
        "stage_id": capability["stage_id"],
        "capability_sha256": capability_hash(capability),
        "output_name": output_name,
        "files": files,
    }
    # Validate payloads before committing the sealed descriptor.
    building_names = {entry.name for entry in os.scandir(stage)}
    expected_building = {DESCRIPTOR_NAME} | {str(entry["name"]) for entry in files}
    if building_names != expected_building:
        fail(
            f"release stage is not exact before sealing "
            f"(missing={sorted(expected_building - building_names)}, extra={sorted(building_names - expected_building)})"
        )
    for entry in files:
        hash_stage_file(stage / str(entry["name"]), entry)
    temporary_fd, temporary_name = tempfile.mkstemp(prefix=".descriptor.", dir=stage)
    temporary = Path(temporary_name)
    try:
        os.chmod(temporary, 0o644)
        with os.fdopen(temporary_fd, "wb", closefd=True) as output:
            output.write(canonical_json(candidate))
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, stage / DESCRIPTOR_NAME)
        fsync_directory(stage)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
    descriptor = parse_stage_descriptor(stage, capability, "sealed")
    verify_exact_stage(stage, descriptor)
    sys.stdout.buffer.write(canonical_json(descriptor))


def publish(args: argparse.Namespace) -> None:
    capability = parse_capability(Path(args.capability_file))
    stage_parent, stage, stage_metadata = validate_owned_stage(capability)
    descriptor = parse_stage_descriptor(stage, capability, "sealed")
    output_parent, output_parent_metadata = safe_directory(
        args.output_parent, "output parent"
    )
    if stage_metadata.st_dev != output_parent_metadata.st_dev:
        fail("stage and output parent must be on the same filesystem")
    output = output_parent / str(descriptor["output_name"])
    try:
        os.lstat(output)
    except FileNotFoundError:
        pass
    else:
        fail(f"release-set output already exists: {output}")
    verify_exact_stage(stage, descriptor)
    fsync_directory(stage)
    fsync_directory(stage_parent)
    fsync_directory(output_parent)
    if os.environ.get("DCENT_RELEASE_SET_TEST_FAIL_BEFORE_PROMOTION") == "1":
        fail("injected failure before release-set promotion")
    descriptor_digest = hashlib.sha256(canonical_json(descriptor)).hexdigest()
    # Keep unpublished bytes private through the no-replace transition.  If a
    # destination races us, the retained diagnostic stage must not become
    # world-searchable merely because publication was attempted.
    atomic_rename_noreplace(
        stage, output, (stage_metadata.st_dev, stage_metadata.st_ino)
    )
    # The exact set is already visible as one directory entry. Normalize final
    # accessibility only after successful promotion, then durably record it.
    os.chmod(output, 0o755)
    fsync_directory(output)
    fsync_directory(stage_parent)
    fsync_directory(output_parent)
    result = {
        "schema": RESULT_SCHEMA,
        "path": str(output),
        "release_set_id": descriptor["stage_id"],
        "descriptor_sha256": descriptor_digest,
        "files": descriptor["files"],
    }
    sys.stdout.buffer.write(canonical_json(result))


def destroy_stage(args: argparse.Namespace) -> None:
    capability = parse_capability(Path(args.capability_file))
    parent, stage, _ = validate_owned_stage(capability)
    descriptor_value, _ = read_regular_json(stage / DESCRIPTOR_NAME, "stage descriptor")
    state = descriptor_value.get("state")
    if state not in ("building", "sealed"):
        fail("owned stage descriptor has an invalid cleanup state")
    parse_stage_descriptor(stage, capability, str(state))
    # Refuse recursive or link-following cleanup. A tampered stage is retained for inspection.
    names: list[str] = []
    with os.scandir(stage) as entries:
        for entry in entries:
            validate_flat_name(entry.name, "cleanup entry name", allow_descriptor=True)
            metadata = os.lstat(stage / entry.name)
            if (
                not stat.S_ISREG(metadata.st_mode)
                or is_reparse(metadata)
                or getattr(metadata, "st_nlink", 1) != 1
            ):
                fail(f"refusing to destroy stage containing unsafe entry: {entry.name}")
            names.append(entry.name)
    for name in names:
        os.unlink(stage / name)
    os.rmdir(stage)
    fsync_directory(parent)


def query(args: argparse.Namespace) -> None:
    raw = sys.stdin.buffer.read(MAX_JSON_BYTES + 1)
    if len(raw) > MAX_JSON_BYTES:
        fail("query input exceeds the one-MiB bound")
    try:
        value = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"query input is not valid JSON: {error}")
    if not isinstance(value, dict):
        fail("query input must be an object")
    schema = value.get("schema")
    if schema == CAPABILITY_SCHEMA:
        required = {
            "schema",
            "stage_id",
            "capability",
            "stage_parent",
            "stage_path",
            "stage_dev",
            "stage_ino",
        }
        if set(value) != required:
            fail("query capability has an invalid exact schema")
        stage_id = validate_hex(value["stage_id"], "query stage ID", 32)
        validate_hex(value["capability"], "query capability", 64)
        if any(
            not isinstance(value[field], str)
            or not value[field]
            or value[field] != str(absolute(value[field]))
            for field in ("stage_parent", "stage_path")
        ):
            fail("query capability paths are invalid")
        if any(
            isinstance(value[field], bool)
            or not isinstance(value[field], int)
            or value[field] < 0
            for field in ("stage_dev", "stage_ino")
        ):
            fail("query capability identity is invalid")
        stage_path = Path(str(value["stage_path"]))
        if (
            stage_path.parent != Path(str(value["stage_parent"]))
            or stage_path.name != f".dcent-release-set-stage-{stage_id}"
        ):
            fail("query capability stage relationship is invalid")
    elif schema == STAGE_SCHEMA:
        state = value.get("state")
        expected = {"schema", "state", "stage_id", "capability_sha256"}
        if state == "sealed":
            expected |= {"output_name", "files"}
        elif state != "building":
            fail("query stage state is invalid")
        if set(value) != expected:
            fail("query stage has an invalid exact schema")
        validate_hex(value["stage_id"], "query stage ID", 32)
        validate_hex(value["capability_sha256"], "query capability digest", 64)
        if state == "sealed":
            validate_flat_name(value["output_name"], "query output name")
            validate_file_entries(value["files"], "query stage")
    elif schema == RESULT_SCHEMA:
        if set(value) != {
            "schema",
            "path",
            "release_set_id",
            "descriptor_sha256",
            "files",
        }:
            fail("query publication result has an invalid exact schema")
        if (
            not isinstance(value["path"], str)
            or not value["path"]
            or value["path"] != str(absolute(value["path"]))
        ):
            fail("query publication path is invalid")
        validate_hex(value["release_set_id"], "query release-set ID", 32)
        validate_hex(value["descriptor_sha256"], "query descriptor digest", 64)
        validate_file_entries(value["files"], "query publication result")
    else:
        fail("query input schema is unsupported")
    allowed = {
        CAPABILITY_SCHEMA: {
            "stage-id": "stage_id",
            "stage-path": "stage_path",
            "capability": "capability",
        },
        STAGE_SCHEMA: {"stage-id": "stage_id", "output-name": "output_name"},
        RESULT_SCHEMA: {
            "release-set-id": "release_set_id",
            "published-path": "path",
            "descriptor-sha256": "descriptor_sha256",
        },
    }
    if args.field not in allowed[schema]:
        fail("query field is not valid for the input schema")
    field = allowed[schema][args.field]
    if (
        field not in value
        or not isinstance(value[field], (str, int))
        or isinstance(value[field], bool)
    ):
        fail("query value is absent or invalid")
    print(value[field])


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    create = commands.add_parser("create-stage")
    create.add_argument("--parent", required=True)
    create.set_defaults(function=create_stage)
    seal = commands.add_parser("seal-stage")
    seal.add_argument("--capability-file", required=True)
    seal.add_argument("--manifest", required=True)
    seal.add_argument("--output-name", required=True)
    seal.set_defaults(function=seal_stage)
    manifest = commands.add_parser("manifest-stage")
    manifest.add_argument("--capability-file", required=True)
    manifest.add_argument("--output", required=True)
    manifest.set_defaults(function=manifest_stage)
    promote = commands.add_parser("publish")
    promote.add_argument("--capability-file", required=True)
    promote.add_argument("--output-parent", required=True)
    promote.set_defaults(function=publish)
    destroy = commands.add_parser("destroy-stage")
    destroy.add_argument("--capability-file", required=True)
    destroy.set_defaults(function=destroy_stage)
    query_command = commands.add_parser("query")
    query_command.add_argument(
        "--field",
        choices=(
            "stage-id",
            "stage-path",
            "capability",
            "output-name",
            "release-set-id",
            "published-path",
            "descriptor-sha256",
        ),
        required=True,
    )
    query_command.set_defaults(function=query)
    return root


def main() -> int:
    try:
        args = parser().parse_args()
        args.function(args)
        return 0
    except (ReleaseSetError, OSError) as error:
        print(f"ERROR: release-set publication: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
