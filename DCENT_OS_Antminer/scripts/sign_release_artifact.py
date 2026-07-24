#!/usr/bin/env python3
"""Sign one exact release artifact through the durable no-replace signer."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import sys


SCRIPT_DIR = Path(__file__).resolve().parent
if os.fspath(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, os.fspath(SCRIPT_DIR))

import release_set_publication as release_io  # noqa: E402
import sign_release_receipt as exact_signer  # noqa: E402


MAX_RELEASE_ARTIFACT_BYTES = 64 * 1024 * 1024 * 1024


def fail(message: str) -> None:
    raise exact_signer.SigningError(message)


def sign_artifact(args: argparse.Namespace) -> None:
    artifact = Path(args.artifact).expanduser().absolute()
    output = Path(args.output_sig or f"{artifact}.sig").expanduser().absolute()
    private_key = args.key or os.environ.get("DCENT_RELEASE_SIGNING_KEY", "")
    public_key = args.pubkey or os.environ.get("DCENT_RELEASE_PUBKEY_FILE", "")
    if not private_key:
        fail(
            "release artifact signing requires a private key; set "
            "DCENT_RELEASE_SIGNING_KEY or pass --key"
        )
    if not public_key:
        fail(
            "release artifact signing requires a trusted public key; set "
            "DCENT_RELEASE_PUBKEY_FILE or pass --pubkey"
        )

    exact_signer.sign_receipt(
        argparse.Namespace(
            receipt=os.fspath(artifact),
            private_key=os.fspath(Path(private_key).expanduser().absolute()),
            public_key=os.fspath(Path(public_key).expanduser().absolute()),
            signature=os.fspath(output),
            subject="release artifact",
            maximum_input_bytes=MAX_RELEASE_ARTIFACT_BYTES,
            durable_input=True,
            control_prefix="dcentos-release-artifact-signing-",
            success_message=f"Signed release artifact: {artifact} -> {output}",
        )
    )


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("artifact")
    result.add_argument("--key")
    result.add_argument("--pubkey")
    result.add_argument("--output-sig")
    return result


def main() -> int:
    os.umask(0o077)
    try:
        sign_artifact(parser().parse_args())
        return 0
    except (
        exact_signer.SigningError,
        OSError,
        ValueError,
        release_io.ReleaseSetError,
        release_io.DirectoryPublishError,
        release_io.PublishError,
    ) as error:
        print(f"ERROR: release artifact signing: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
