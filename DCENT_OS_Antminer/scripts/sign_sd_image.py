#!/usr/bin/env python3
"""Authorize, sign, verify, and durably publish one exact SD-image signature."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import stat
import sys
from typing import NoReturn


SCRIPT_DIR = Path(__file__).resolve().parent
if os.fspath(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, os.fspath(SCRIPT_DIR))

import release_set_publication as release_io  # noqa: E402
import sign_release_receipt as exact_signer  # noqa: E402


MAX_SD_IMAGE_BYTES = 64 * 1024 * 1024 * 1024
MAX_MANIFEST_BYTES = 16 * 1024 * 1024
SHA256_RE = re.compile(r"[0-9a-f]{64}")

COMMON_ZYNQ_ARTIFACTS = frozenset(
    {
        "BOOT.bin",
        "uImage",
        "devicetree.dtb",
        "uEnv.txt",
        "bitstream",
        "rootfs",
    }
)
MANIFEST_POLICIES = {
    (
        "dcentos.am2_s19jpro_sd_image_manifest.v2",
        "am2-s19jpro-sd",
    ): COMMON_ZYNQ_ARTIFACTS,
    (
        "dcentos.am1_s9_sd_image_manifest.v1",
        "am1-s9-sd-standalone",
    ): COMMON_ZYNQ_ARTIFACTS,
    (
        "dcentos.am1_s9_sd_image_manifest.v1",
        "am1-s9-sd-piggyback",
    ): frozenset(COMMON_ZYNQ_ARTIFACTS - {"BOOT.bin"}),
}


def fail(message: str) -> NoReturn:
    raise exact_signer.SigningError(message)


def reject_duplicate_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            fail(f"SD image manifest has duplicate JSON key: {key}")
        result[key] = value
    return result


def decode_manifest(manifest: exact_signer.PinnedFile) -> dict[str, object]:
    try:
        decoded = json.loads(
            manifest.read_bytes().decode("utf-8"),
            object_pairs_hook=reject_duplicate_keys,
            parse_constant=lambda value: fail(
                f"SD image manifest contains non-JSON number: {value}"
            ),
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"SD image manifest is not canonical UTF-8 JSON: {error}")
    if not isinstance(decoded, dict):
        fail("SD image manifest root must be an object")
    return decoded


def validate_manifest_binding(
    image: exact_signer.PinnedFile,
    manifest: exact_signer.PinnedFile,
) -> None:
    data = decode_manifest(manifest)
    schema = data.get("schema")
    target = data.get("target")
    if not isinstance(schema, str) or not isinstance(target, str):
        fail("SD image manifest must declare string schema and target")
    required_artifacts = MANIFEST_POLICIES.get((schema, target))
    if required_artifacts is None:
        fail(f"unsupported SD image signing policy: schema={schema!r} target={target!r}")

    manifest_image = data.get("image")
    if not isinstance(manifest_image, str) or not manifest_image.endswith(".img"):
        fail("SD image manifest must declare its original flat .img basename")
    try:
        release_io.validate_flat_name(
            manifest_image, "SD image manifest original basename"
        )
    except release_io.ReleaseSetError as error:
        fail(str(error))
    manifest_size = data.get("image_size_bytes")
    if type(manifest_size) is not int or manifest_size != image.size:
        fail("SD image manifest size does not match the pinned image")
    image_sha256 = data.get("image_sha256")
    if (
        not isinstance(image_sha256, str)
        or SHA256_RE.fullmatch(image_sha256) is None
        or image_sha256 != image.sha256
    ):
        fail("SD image manifest SHA-256 does not match the pinned image")
    if data.get("boot_artifacts_complete") is not True:
        fail("SD image manifest does not authorize a complete boot image")
    if data.get("allow_incomplete") is True:
        fail("SD image manifest is marked allow_incomplete")

    artifacts = data.get("artifacts")
    if not isinstance(artifacts, dict):
        fail("SD image manifest artifacts must be an object")
    incomplete = sorted(
        name for name in required_artifacts if artifacts.get(name) is not True
    )
    if incomplete:
        fail(
            "SD image manifest has incomplete required artifacts: "
            + ", ".join(incomplete)
        )


def discover_manifest(image: Path, explicit: str | None) -> Path:
    if explicit is not None:
        return Path(explicit).expanduser().absolute()

    candidates: list[Path] = []
    for candidate in (
        Path(f"{image}.manifest.json"),
        image.with_suffix(".manifest.json"),
    ):
        if candidate not in candidates and (
            candidate.exists() or candidate.is_symlink()
        ):
            candidates.append(candidate)
    if not candidates:
        fail(
            "missing SD image signing manifest; expected "
            f"{image}.manifest.json or {image.with_suffix('.manifest.json')}"
        )
    if len(candidates) != 1:
        fail(
            "ambiguous SD image signing manifests: "
            + ", ".join(os.fspath(path) for path in candidates)
        )
    return candidates[0]


def signature_path(image: Path, value: str | None) -> Path:
    requested = value or f"{image}.sig"
    output, _parent = exact_signer.resolve_signature_output(requested, "SD image")
    return output


def require_unsigned_lab_state(image: Path, output: Path) -> None:
    with exact_signer.PinnedFile(
        image, "unsigned lab SD image", MAX_SD_IMAGE_BYTES
    ):
        try:
            metadata = os.lstat(output)
        except FileNotFoundError:
            return
        except OSError as error:
            fail(f"cannot inspect unsigned-lab signature path: {error}")
        kind = (
            "symlink/reparse point"
            if stat.S_ISLNK(metadata.st_mode) or exact_signer.is_reparse(metadata)
            else "existing filesystem object"
        )
        fail(
            f"unsigned lab SD image has a stale signature {kind}: {output}; "
            "remove it explicitly before continuing"
        )


def sign_sd_image(args: argparse.Namespace) -> None:
    image = Path(args.image).expanduser().absolute()
    output = signature_path(image, args.output_sig)
    if args.check_only:
        if args.key or args.pubkey or args.output_sig or args.allow_unsigned_lab:
            fail("--check-only cannot be combined with signing or unsigned-lab options")
        manifest = discover_manifest(image, args.manifest)
        with (
            exact_signer.PinnedFile(
                image, "SD image", MAX_SD_IMAGE_BYTES
            ) as pinned_image,
            exact_signer.PinnedFile(
                manifest, "SD image signing manifest", MAX_MANIFEST_BYTES
            ) as pinned_manifest,
        ):
            validate_manifest_binding(pinned_image, pinned_manifest)
            pinned_image.revalidate()
            pinned_manifest.revalidate()
        print(f"Validated SD image signing manifest: {manifest}")
        return

    private_key = args.key or os.environ.get("DCENT_RELEASE_SIGNING_KEY", "")
    public_key = args.pubkey or os.environ.get("DCENT_RELEASE_PUBKEY_FILE", "")

    if not private_key:
        if not args.allow_unsigned_lab:
            fail(
                "no private signing key was provided; use --allow-unsigned-lab "
                "only for an explicitly unsigned lab build"
            )
        if public_key:
            fail("trusted public key was provided without a private signing key")
        require_unsigned_lab_state(image, output)
        print(f"WARNING: explicitly unsigned lab SD image: {image}", file=sys.stderr)
        return

    if args.allow_unsigned_lab:
        fail("--allow-unsigned-lab cannot be combined with a private signing key")
    if not public_key:
        fail(
            "trusted release public key is required when signing; set "
            "DCENT_RELEASE_PUBKEY_FILE or pass --pubkey"
        )

    manifest = discover_manifest(image, args.manifest)
    exact_signer.sign_receipt(
        argparse.Namespace(
            receipt=os.fspath(image),
            private_key=os.fspath(Path(private_key).expanduser().absolute()),
            public_key=os.fspath(Path(public_key).expanduser().absolute()),
            signature=os.fspath(output),
            subject="SD image",
            maximum_input_bytes=MAX_SD_IMAGE_BYTES,
            manifest=os.fspath(manifest),
            manifest_label="SD image signing manifest",
            maximum_manifest_bytes=MAX_MANIFEST_BYTES,
            validate_manifest=validate_manifest_binding,
            durable_input=True,
            durable_manifest=True,
            control_prefix="dcentos-sd-image-signing-",
            success_message=f"Signed SD image: {image} -> {output}",
        )
    )


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    root.add_argument("image")
    root.add_argument("--key")
    root.add_argument("--pubkey")
    root.add_argument("--output-sig")
    root.add_argument("--manifest")
    root.add_argument("--check-only", action="store_true")
    root.add_argument(
        "--allow-unsigned-lab",
        action="store_true",
        help="succeed without signing only when the signature path is absent",
    )
    return root


def main() -> int:
    os.umask(0o077)
    try:
        sign_sd_image(parser().parse_args())
        return 0
    except (
        exact_signer.SigningError,
        OSError,
        ValueError,
        release_io.ReleaseSetError,
        release_io.DirectoryPublishError,
        release_io.PublishError,
    ) as error:
        print(f"ERROR: SD image signing: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
