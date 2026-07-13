#!/usr/bin/env python3
"""Adversarial tests for release_signing_authority.py."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("release_signing_authority.py")
SPEC = importlib.util.spec_from_file_location("release_signing_authority", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
authority = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(authority)
invocation = authority.release_invocation


class ReleaseSigningAuthorityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.invocation_parent = self.root / "invocations"
        self.authority_parent = self.root / "authorities"
        self.keys = self.root / "keys"
        for path in (self.invocation_parent, self.authority_parent, self.keys):
            path.mkdir()
        self.private = self.keys / "private.pem"
        self.public = self.keys / "public.pem"
        self.private.write_bytes(b"PRIVATE-ED25519-TEST-AUTHORITY\n")
        self.public.write_bytes(b"PUBLIC-ED25519-TEST-AUTHORITY\n")
        if os.name == "posix":
            os.chmod(self.private, 0o600)
        self.release_invocation = invocation.create_invocation(
            self.invocation_parent, "s9"
        )

    def tearDown(self) -> None:
        for root, directories, files in os.walk(
            self.temporary.name, topdown=False, followlinks=False
        ):
            for name in files:
                path = Path(root) / name
                try:
                    if not path.is_symlink():
                        os.chmod(path, 0o600)
                except OSError:
                    pass
            for name in directories:
                path = Path(root) / name
                try:
                    if not path.is_symlink():
                        os.chmod(path, 0o700)
                except OSError:
                    pass
        self.temporary.cleanup()

    def create(self):
        return authority.create_authority(
            self.authority_parent,
            self.release_invocation.stage,
            self.private,
            self.public,
        )

    def run_cli(
        self, *arguments: str, stdin: bytes = b""
    ) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(SCRIPT), *arguments],
            input=stdin,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    def test_create_is_private_exact_invocation_bound_and_path_free(self) -> None:
        created = self.create()
        verified = authority.verify_authority(
            created.stage, self.release_invocation.stage
        )
        self.assertEqual(created.private_key.read_bytes(), self.private.read_bytes())
        self.assertEqual(created.public_key.read_bytes(), self.public.read_bytes())
        self.assertEqual(
            set(path.name for path in created.stage.iterdir()),
            {
                authority.DESCRIPTOR_NAME,
                authority.PRIVATE_KEY_NAME,
                authority.PUBLIC_KEY_NAME,
            },
        )
        self.assertEqual(
            verified.descriptor["invocation"]["invocation_id"],
            self.release_invocation.invocation_id,
        )
        descriptor_raw = created.descriptor.read_bytes()
        capability_raw = created.capability.read_bytes()
        self.assertNotIn(self.private.read_bytes(), descriptor_raw)
        self.assertNotIn(self.private.read_bytes(), capability_raw)
        self.assertNotIn(str(self.private).encode(), descriptor_raw)
        self.assertNotIn(str(self.public).encode(), descriptor_raw)
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(created.stage.stat().st_mode), 0o700)
            for path in (*created.stage.iterdir(), created.capability):
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o400)

    def test_query_result_and_cli_lifecycle_are_strict(self) -> None:
        result = self.run_cli(
            "create",
            "--stage-parent",
            str(self.authority_parent),
            "--invocation-stage",
            str(self.release_invocation.stage),
            "--private-key",
            str(self.private),
            "--public-key",
            str(self.public),
        )
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        created = json.loads(result.stdout)
        for field in authority.QUERY_FIELDS:
            queried = self.run_cli(
                "query-result", "--field", field, stdin=result.stdout
            )
            self.assertEqual(queried.returncode, 0, queried.stderr.decode())
            self.assertEqual(queried.stdout.decode().strip(), created[field])
        checked = self.run_cli(
            "verify",
            "--invocation-stage",
            str(self.release_invocation.stage),
            created["stage"],
        )
        self.assertEqual(checked.returncode, 0, checked.stderr.decode())
        destroyed = self.run_cli(
            "destroy",
            "--capability",
            created["capability"],
            created["stage"],
        )
        self.assertEqual(destroyed.returncode, 0, destroyed.stderr.decode())
        self.assertFalse(Path(created["stage"]).exists())
        self.assertFalse(Path(created["capability"]).exists())

        noncanonical = json.dumps(created).encode("ascii")
        rejected = self.run_cli("query-result", "--field", "stage", stdin=noncanonical)
        self.assertNotEqual(rejected.returncode, 0)

    def test_wrong_capability_and_invocation_cannot_authorize(self) -> None:
        first = self.create()
        other_invocation = invocation.create_invocation(self.invocation_parent, "other")
        second = authority.create_authority(
            self.authority_parent, other_invocation.stage, self.private, self.public
        )
        with self.assertRaises(authority.SigningAuthorityError):
            authority.verify_authority(first.stage, other_invocation.stage)
        with self.assertRaises(authority.SigningAuthorityError):
            authority.destroy_authority(first.stage, second.capability)
        self.assertTrue(first.stage.is_dir())
        self.assertTrue(second.stage.is_dir())

    def test_symlink_source_is_rejected(self) -> None:
        symlink = self.keys / "private-link.pem"
        try:
            symlink.symlink_to(self.private)
        except (OSError, NotImplementedError) as error:
            self.skipTest(f"symlinks unavailable: {error}")
        with self.assertRaises(authority.SigningAuthorityError):
            authority.create_authority(
                self.authority_parent,
                self.release_invocation.stage,
                symlink,
                self.public,
            )

    def test_hardlink_fifo_and_loose_private_mode_are_rejected(self) -> None:
        hardlink = self.keys / "private-hardlink.pem"
        try:
            os.link(self.private, hardlink)
        except OSError as error:
            self.skipTest(f"hardlinks unavailable: {error}")
        with self.assertRaisesRegex(
            authority.SigningAuthorityError, "exactly one filesystem link"
        ):
            self.create()
        hardlink.unlink()

        if os.name == "posix":
            fifo = self.keys / "private-fifo.pem"
            os.mkfifo(fifo)
            with self.assertRaisesRegex(
                authority.SigningAuthorityError, "regular file"
            ):
                authority.create_authority(
                    self.authority_parent,
                    self.release_invocation.stage,
                    fifo,
                    self.public,
                )
            os.chmod(self.private, 0o644)
            with self.assertRaisesRegex(
                authority.SigningAuthorityError, "group or other"
            ):
                self.create()

    @unittest.skipUnless(os.name == "posix", "pathname replacement proof is POSIX-only")
    def test_open_handle_rejects_pathname_rotation(self) -> None:
        replacement = self.keys / "replacement.pem"
        replacement.write_bytes(b"ROTATED-PRIVATE-KEY\n")
        os.chmod(replacement, 0o600)

        def rotate() -> None:
            os.replace(replacement, self.private)

        with self.assertRaisesRegex(
            authority.SigningAuthorityError,
            "pathname was replaced|exactly one filesystem link",
        ):
            authority._read_source_file(
                self.private,
                "release private key",
                private=True,
                after_open=rotate,
            )

    @unittest.skipUnless(os.name == "posix", "FIFO race proof is POSIX-only")
    def test_raced_fifo_open_never_blocks_before_handle_revalidation(self) -> None:
        code = r"""import importlib.util, os, pathlib, sys
script=pathlib.Path(sys.argv[1]); target=pathlib.Path(sys.argv[2])
sys.path.insert(0, str(script.parent))
spec=importlib.util.spec_from_file_location("rsa_fifo_child", script)
module=importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
real_open=module.os.open; raced=False
def replace_then_open(path, flags, *args, **kwargs):
    global raced
    if not raced and pathlib.Path(path) == target:
        raced=True; target.unlink(); os.mkfifo(target)
    return real_open(path, flags, *args, **kwargs)
module.os.open=replace_then_open
try:
    module._read_source_file(target, "raced private key", private=True)
except module.SigningAuthorityError:
    raise SystemExit(0)
raise SystemExit("raced FIFO unexpectedly admitted")
"""
        completed = subprocess.run(
            [sys.executable, "-c", code, str(SCRIPT), str(self.private)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=5,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr.decode())

    def test_stage_tampering_and_cleanup_escape_are_rejected(self) -> None:
        created = self.create()
        if os.name == "posix":
            os.chmod(created.private_key, 0o600)
        created.private_key.write_bytes(b"TAMPERED\n")
        if os.name == "posix":
            os.chmod(created.private_key, 0o400)
        with self.assertRaises(authority.SigningAuthorityError):
            authority.verify_authority(created.stage, self.release_invocation.stage)

        other_root = self.root / "must-survive"
        other_root.mkdir()
        marker = other_root / "marker"
        marker.write_text("retained", encoding="ascii")
        alias = self.authority_parent / "authority-alias"
        try:
            alias.symlink_to(other_root, target_is_directory=True)
        except (OSError, NotImplementedError):
            alias = None
        if alias is not None:
            with self.assertRaises(authority.SigningAuthorityError):
                authority.destroy_authority(alias, created.capability)
        self.assertEqual(marker.read_text(encoding="ascii"), "retained")

    @unittest.skipUnless(os.name == "nt", "junction rejection is Windows-only")
    def test_windows_junction_source_component_is_rejected(self) -> None:
        target = self.root / "junction-target"
        target.mkdir()
        private = target / "private.pem"
        private.write_bytes(b"PRIVATE-JUNCTION\n")
        junction = self.root / "junction"
        made = subprocess.run(
            ["cmd.exe", "/d", "/c", "mklink", "/J", str(junction), str(target)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
        )
        if made.returncode != 0:
            self.skipTest(f"junction creation unavailable: {made.stderr}")
        try:
            with self.assertRaisesRegex(
                authority.SigningAuthorityError, "reparse-point"
            ):
                authority.create_authority(
                    self.authority_parent,
                    self.release_invocation.stage,
                    junction / "private.pem",
                    self.public,
                )
        finally:
            os.rmdir(junction)
        self.assertEqual(private.read_bytes(), b"PRIVATE-JUNCTION\n")


if __name__ == "__main__":
    unittest.main(verbosity=2)
