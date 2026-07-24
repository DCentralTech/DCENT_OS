#!/usr/bin/env python3
"""Adversarial post-cleanup tests for portable release evidence sets."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
from typing import Any
import unittest
from unittest import mock

import build_input_snapshot
import portable_release_evidence as portable
import release_capsule_lineage
import release_invocation
import release_result_stage
import release_set_publication
import release_capsule_target_policy
import source_closure
import source_snapshot


def run(*arguments: str, cwd: Path | None = None) -> str:
    return subprocess.run(
        arguments,
        cwd=cwd,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    ).stdout.strip()


class PortableReleaseEvidenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        for tool in ("git", "openssl"):
            if shutil.which(tool) is None:
                raise unittest.SkipTest(
                    f"portable release evidence tests require {tool} on PATH"
                )

    def setUp(self) -> None:
        if shutil.which("openssl") is None:
            self.skipTest("OpenSSL executable is required for Ed25519 fixture tests")
        if shutil.which("git") is None:
            self.skipTest("Git executable is required for source projection fixtures")
        self.temporary = tempfile.TemporaryDirectory(prefix="dcent-portable-evidence-")
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self._write_fixture_repo()
        run("git", "init", "-q", cwd=self.repo)
        run("git", "config", "user.name", "portable-test", cwd=self.repo)
        run("git", "config", "user.email", "portable@test.invalid", cwd=self.repo)
        run("git", "add", ".", cwd=self.repo)
        run("git", "commit", "-q", "-m", "fixture", cwd=self.repo)
        self.commit = run("git", "rev-parse", "HEAD", cwd=self.repo)

        self.private_key = self.root / "release.pem"
        self.public_key = self.root / "release.pub"
        self.wrong_key = self.root / "wrong.pub"
        wrong_private = self.root / "wrong.pem"
        run(
            "openssl", "genpkey", "-algorithm", "Ed25519", "-out", str(self.private_key)
        )
        run(
            "openssl",
            "pkey",
            "-in",
            str(self.private_key),
            "-pubout",
            "-out",
            str(self.public_key),
        )
        run("openssl", "genpkey", "-algorithm", "Ed25519", "-out", str(wrong_private))
        run(
            "openssl",
            "pkey",
            "-in",
            str(wrong_private),
            "-pubout",
            "-out",
            str(self.wrong_key),
        )

        source_parent = self.root / "source"
        invocation_parent = self.root / "invocation"
        result_parent = self.root / "result"
        input_parent = self.root / "inputs"
        for directory in (
            source_parent,
            invocation_parent,
            result_parent,
            input_parent,
        ):
            directory.mkdir(mode=0o700)
        self.source = source_snapshot.create_snapshot(
            self.repo, self.commit, source_parent
        )
        self.invocation = release_invocation.create_invocation(invocation_parent, "s9")
        self.result = release_result_stage.create_result_stage(
            result_parent,
            self.invocation.stage,
            result_output=self.root / "result-stage-create.json",
        )
        sealed = release_result_stage.seal_result_stage(
            self.result.stage, self.result.capability, self.invocation.stage
        )
        self.cargo = build_input_snapshot.create_snapshot(
            self.repo,
            self.source.tree / "DCENT_OS_Antminer/scripts/build_inputs.manifest",
            "cargo-workspace",
            input_parent,
            selection_root=self.source.tree,
        )
        self.packaging = build_input_snapshot.create_snapshot(
            self.repo,
            self.source.tree / "DCENT_OS_Antminer/scripts/build_inputs.manifest",
            "s9",
            input_parent,
            selection_root=self.source.tree,
        )

        self.release = self.root / "DCENTOS_XIL1_S9_beta20260712"
        self.release.mkdir()
        primary = self.release / "dcentos-unit.tar"
        self._write_primary_artifact(primary, "s9", "am1-s9")
        closure = {
            "build": {"target": "s9"},
            "prebuilt_rust_inputs": {
                "packaging_artifact": primary.name,
                "entries": [],
            },
            "artifacts": [source_closure.artifact_entry(str(primary))],
        }
        (self.release / "firmware.source-closure.json").write_bytes(
            portable.canonical_bytes(closure)
        )
        self._sign(
            self.release / "firmware.source-closure.json",
            self.release / "firmware.source-closure.json.sig",
        )
        shutil.copyfile(self.source.snapshot, self.release / portable.SOURCE_NAME)
        shutil.copyfile(
            self.invocation.descriptor, self.release / portable.INVOCATION_NAME
        )
        shutil.copyfile(self.cargo.snapshot, self.release / portable.CARGO_INPUT_NAME)
        shutil.copyfile(
            self.packaging.snapshot, self.release / portable.PACKAGING_INPUT_NAME
        )
        (self.release / portable.RESULT_NAME).write_bytes(
            release_result_stage.canonical_bytes(
                release_result_stage.audit_projection(sealed)
            )
        )
        self.capsule = release_capsule_lineage.derive_release_capsule(
            self.repo,
            self.source.snapshot,
            self.commit,
            self.invocation.stage,
        )
        self._resign_and_seal()

        # The verification below is deliberately after every live authority is
        # destroyed. Restricted input bytes are never copied into the release.
        release_result_stage.destroy_result_stage(
            self.result.stage, self.result.capability, self.invocation.stage
        )
        build_input_snapshot.destroy_snapshot(
            self.cargo.snapshot, self.cargo.destroy_token
        )
        build_input_snapshot.destroy_snapshot(
            self.packaging.snapshot, self.packaging.destroy_token
        )
        release_invocation.mark_gc_eligible(
            self.invocation.stage,
            self.invocation.capability,
            "post-cleanup-portable-test",
        )
        release_invocation.destroy_invocation(
            self.invocation.stage, self.invocation.capability
        )
        source_snapshot.destroy_snapshot(
            self.source.snapshot, self.source.destroy_token
        )
        self.assertFalse(self.source.stage.exists())
        self.assertFalse(self.invocation.stage.exists())
        self.assertFalse(self.result.stage.exists())
        self.assertFalse(self.cargo.stage.exists())
        self.assertFalse(self.packaging.stage.exists())

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _write_fixture_repo(self) -> None:
        paths = {
            "": b"kernel\n",
            "": b"dtb\n",
        }
        for relative, raw in paths.items():
            path = self.repo / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(raw)
        manifest = self.repo / "DCENT_OS_Antminer/scripts/build_inputs.manifest"
        manifest.parent.mkdir(parents=True, exist_ok=True)
        manifest.write_text(
            "".join(
                f"{hashlib.sha256(raw).hexdigest()}  {relative}\n"
                for relative, raw in paths.items()
            ),
            encoding="ascii",
        )

    def _sign(self, content: Path, signature: Path) -> None:
        run(
            "openssl",
            "pkeyutl",
            "-sign",
            "-rawin",
            "-inkey",
            str(self.private_key),
            "-in",
            str(content),
            "-out",
            str(signature),
        )

    def _write_primary_artifact(
        self, output: Path, target: str, package_board: str
    ) -> None:
        package = self.root / f"package-{target}-{package_board}"
        if package.exists():
            shutil.rmtree(package)
        package.mkdir()
        payload_bytes = {
            "kernel": b"kernel fixture\n",
            "root": b"rootfs fixture\n",
            "METADATA": b"metadata fixture\n",
        }
        for name, raw in payload_bytes.items():
            (package / name).write_bytes(raw)
        shutil.copyfile(self.public_key, package / "release_ed25519.pub")

        prefix = f"sysupgrade-{package_board}"
        payload_specs = {
            "kernel": "kernel",
            "rootfs": "root",
            "metadata": "METADATA",
            "verification_key": "release_ed25519.pub",
        }
        manifest = {
            "schema": 1,
            "manifest_profile": "dcentos.sysupgrade-authority/v1",
            "product": "DCENT_OS",
            "package_type": "sysupgrade",
            "installable": True,
            "artifact_maturity": "experimental",
            "board": package_board,
            "board_target": package_board,
            "version": "test",
            "status": "release",
            "payloads": {
                kind: {
                    "path": f"{prefix}/{leaf}",
                    "size": (package / leaf).stat().st_size,
                    "sha256": hashlib.sha256((package / leaf).read_bytes()).hexdigest(),
                }
                for kind, leaf in payload_specs.items()
            },
            "provenance": {"build_target": target},
        }
        manifest_path = package / "MANIFEST.json"
        manifest_path.write_text(json.dumps(manifest, sort_keys=True) + "\n")
        self._sign(manifest_path, package / "MANIFEST.sig")
        with tarfile.open(output, "w", format=tarfile.USTAR_FORMAT) as archive:
            archive.add(package, arcname=prefix, recursive=False)
            for path in sorted(package.iterdir(), key=lambda item: item.name):
                archive.add(path, arcname=f"{prefix}/{path.name}", recursive=False)

    def _resign_and_seal(self, mutate_index=None) -> None:
        index_path = self.release / portable.INDEX_NAME
        signature_path = self.release / portable.SIGNATURE_NAME
        descriptor_path = self.release / release_set_publication.DESCRIPTOR_NAME
        for path in (index_path, signature_path, descriptor_path):
            with contextlib.suppress(FileNotFoundError):
                path.unlink()
        payload = [
            portable.evidence(path)
            for path in self.release.iterdir()
            if path.name
            not in {
                portable.INDEX_NAME,
                portable.SIGNATURE_NAME,
                release_set_publication.DESCRIPTOR_NAME,
            }
        ]
        payload.sort(key=lambda item: str(item["name"]).encode("utf-8"))
        index = {
            "schema": portable.SCHEMA,
            "target": "s9",
            "output_name": self.release.name,
            "claim": portable.CLAIM,
            "scope": {"does_not_claim": list(portable.NON_CLAIMS)},
            "release_capsule": self.capsule,
            "source_commit": self.commit,
            "source_closure": portable.evidence(
                self.release / "firmware.source-closure.json"
            ),
            "source_closure_signature": portable.evidence(
                self.release / "firmware.source-closure.json.sig"
            ),
            "projections": {
                "cargo_input": portable.evidence(
                    self.release / portable.CARGO_INPUT_NAME
                ),
                "invocation": portable.evidence(
                    self.release / portable.INVOCATION_NAME
                ),
                "packaging_input": portable.evidence(
                    self.release / portable.PACKAGING_INPUT_NAME
                ),
                "result": portable.evidence(self.release / portable.RESULT_NAME),
                "source": portable.evidence(self.release / portable.SOURCE_NAME),
            },
            "payload_files": payload,
            "signature_convention": (
                "payload_files excludes exactly portable-release-evidence.json, "
                "portable-release-evidence.json.sig, and .dcent-release-set.json; "
                "the final sealed set requires all three fixed members"
            ),
        }
        if mutate_index is not None:
            mutate_index(index)
        index_path.write_bytes(portable.canonical_bytes(index))
        self._sign(index_path, signature_path)
        files = [portable.evidence(path) for path in self.release.iterdir()]
        files.sort(key=lambda item: str(item["name"]))
        descriptor = {
            "schema": release_set_publication.STAGE_SCHEMA,
            "state": "sealed",
            "stage_id": "1" * 32,
            "capability_sha256": "2" * 64,
            "output_name": self.release.name,
            "files": files,
        }
        descriptor_path.write_bytes(release_set_publication.canonical_json(descriptor))

    def _verify(self, *, repo: Path | None = None, key: Path | None = None) -> None:
        args = argparse.Namespace(
            repo_root=str(repo or self.repo),
            public_key=str(key or self.public_key),
            release_dir=str(self.release),
        )
        with mock.patch.object(
            portable.source_closure, "verify_portable_manifest", return_value=None
        ), mock.patch.object(
            portable.source_closure,
            "validate_receipt_schema",
            side_effect=lambda value: value,
        ):
            args.command = "verify"
            portable.verify(args)

    def assert_rejected(self, action) -> None:
        with self.assertRaises((portable.PortableEvidenceError, OSError, ValueError)):
            action()

    def test_post_cleanup_exact_set_verifies_with_bounded_claims(self) -> None:
        self._verify()
        index = portable.read_canonical(
            self.release / portable.INDEX_NAME, "test evidence"
        )
        self.assertEqual(index["claim"], portable.CLAIM)
        self.assertIn(
            "build-execution-or-compiler-consumption", index["scope"]["does_not_claim"]
        )
        cargo_projection = portable.read_canonical(
            self.release / portable.CARGO_INPUT_NAME, "Cargo input projection"
        )
        self.assertEqual(cargo_projection["files"], [])

    def test_payload_tamper_is_rejected(self) -> None:
        (self.release / "dcentos-unit.tar").write_bytes(b"tampered\n")
        self.assert_rejected(self._verify)

    def test_missing_member_is_rejected(self) -> None:
        (self.release / portable.RESULT_NAME).unlink()
        self.assert_rejected(self._verify)

    def test_extra_member_is_rejected(self) -> None:
        (self.release / "unexpected.txt").write_text("extra\n", encoding="ascii")
        self.assert_rejected(self._verify)

    def test_swapped_invocation_projection_is_rejected(self) -> None:
        value = json.loads((self.release / portable.INVOCATION_NAME).read_text())
        value["invocation_id"] = "f" * 64
        (self.release / portable.INVOCATION_NAME).write_bytes(
            portable.canonical_bytes(value)
        )
        self._resign_and_seal()
        self.assert_rejected(self._verify)

    def test_noncanonical_projection_is_rejected(self) -> None:
        path = self.release / portable.INVOCATION_NAME
        path.write_bytes(path.read_bytes() + b" ")
        self._resign_and_seal()
        self.assert_rejected(self._verify)

    def test_wrong_git_commit_is_rejected(self) -> None:
        self._resign_and_seal(
            lambda index: index.__setitem__("source_commit", "f" * 40)
        )
        self.assert_rejected(self._verify)

    def test_wrong_out_of_band_key_is_rejected(self) -> None:
        self.assert_rejected(lambda: self._verify(key=self.wrong_key))

    def test_trusted_key_path_rotation_cannot_split_one_verification(self) -> None:
        with portable.pinned_public_key(self.public_key) as (snapshot, raw):
            self.public_key.write_bytes(self.wrong_key.read_bytes())
            portable.verify_signature(
                snapshot,
                self.release / portable.INDEX_NAME,
                self.release / portable.SIGNATURE_NAME,
                "pinned-key fixture",
            )
            self.assertEqual(snapshot.read_bytes(), raw)
            self.assertNotEqual(self.public_key.read_bytes(), raw)

    def test_signed_target_mismatch_is_rejected(self) -> None:
        self._resign_and_seal(
            lambda index: index.__setitem__("target", "am2-s19jpro")
        )
        self.assert_rejected(self._verify)

    def test_published_name_cannot_be_relabelled_outside_signature(self) -> None:
        renamed = self.release.with_name("DCENTOS_XIL3_S19jPro_beta20260712")
        self.release.rename(renamed)
        self.release = renamed
        descriptor_path = self.release / release_set_publication.DESCRIPTOR_NAME
        descriptor = json.loads(descriptor_path.read_text())
        descriptor["output_name"] = self.release.name
        descriptor_path.write_bytes(release_set_publication.canonical_json(descriptor))
        self.assert_rejected(self._verify)

    def test_packaging_target_mismatch_is_rejected(self) -> None:
        value = json.loads((self.release / portable.PACKAGING_INPUT_NAME).read_text())
        value["target"] = "am2-s19jpro"
        without_id = dict(value)
        without_id.pop("snapshot_id")
        value["snapshot_id"] = build_input_snapshot.sha256_bytes(
            build_input_snapshot.canonical_bytes(without_id)
        )
        (self.release / portable.PACKAGING_INPUT_NAME).write_bytes(
            portable.canonical_bytes(value)
        )
        self._resign_and_seal()
        self.assert_rejected(self._verify)

    def test_historical_v1_is_supported_only_as_s9(self) -> None:
        def historical(index):
            index["schema"] = portable.HISTORICAL_SCHEMA
            index.pop("target")
            index.pop("output_name")

        self._resign_and_seal(historical)
        self._verify()
        value = portable.validate_index(
            portable.read_canonical(self.release / portable.INDEX_NAME, "historical")
        )
        self.assertEqual(value["target"], "s9")

    def test_v1_cannot_project_am2_identity(self) -> None:
        invocation = {"logical_name": "am2-s19jpro"}
        packaging = {"target": "am2-s19jpro"}
        closure = {"build": {"target": "am2-s19jpro"}}
        with self.assertRaises(portable.PortableEvidenceError):
            portable.verify_target_bindings(
                portable.HISTORICAL_SCHEMA,
                "s9",
                invocation,
                packaging,
                closure,
            )

    def test_non_object_closure_build_is_rejected_cleanly(self) -> None:
        with self.assertRaises(portable.PortableEvidenceError):
            portable.verify_target_bindings(
                portable.SCHEMA,
                "s9",
                {"logical_name": "s9"},
                {"target": "s9"},
                {"build": "s9"},
            )

    def test_signed_manifest_json_rejects_duplicate_keys_recursively(self) -> None:
        for label, raw, duplicated_key in (
            (
                "root authority",
                b'{"product":"DCENT_OS","product":"attacker"}',
                "product",
            ),
            (
                "nested payload",
                b'{"payloads":{"kernel":{"path":"a","path":"b"}}}',
                "path",
            ),
        ):
            with self.subTest(label=label):
                with self.assertRaisesRegex(
                    portable.PortableEvidenceError,
                    rf"duplicate object key '{duplicated_key}'",
                ):
                    portable.load_unique_json(raw, "target primary artifact manifest")

    def test_manifest_payload_binding_rejects_retained_byte_mismatch(self) -> None:
        prefix = "sysupgrade-am1-s9/"
        members = [
            {"path": f"{prefix}{leaf}", "size": 1, "sha256": "0" * 64}
            for leaf in ("kernel", "root", "METADATA", "release_ed25519.pub")
        ]
        manifest = {
            "payloads": {
                kind: {"path": f"{prefix}{leaf}", "size": 1, "sha256": "0" * 64}
                for kind, leaf in {
                    "kernel": "kernel",
                    "rootfs": "root",
                    "metadata": "METADATA",
                    "verification_key": "release_ed25519.pub",
                }.items()
            }
        }
        manifest["payloads"]["rootfs"]["sha256"] = "1" * 64
        with self.assertRaisesRegex(
            portable.PortableEvidenceError,
            "rootfs payload binding disagrees with retained bytes",
        ):
            portable.verify_manifest_payload_bindings(manifest, members, prefix)

    def test_current_authority_contract_requires_exact_experimental_maturity(self) -> None:
        manifest = {
            "schema": 1,
            "manifest_profile": "dcentos.sysupgrade-authority/v1",
            "installable": True,
            "artifact_maturity": "production",
            "version": "test",
            "status": "release",
        }
        with self.assertRaisesRegex(
            portable.PortableEvidenceError,
            "typed sysupgrade authority contract",
        ):
            portable.verify_current_authority_contract(manifest)

    def test_manifest_payload_bindings_require_metadata_lowercase_digest_and_exact_path(
        self,
    ) -> None:
        prefix = "sysupgrade-am1-s9/"
        leaves = {
            "kernel": "kernel",
            "rootfs": "root",
            "metadata": "METADATA",
            "verification_key": "release_ed25519.pub",
        }
        members = [
            {"path": f"{prefix}{leaf}", "size": 1, "sha256": "a" * 64}
            for leaf in leaves.values()
        ]

        def manifest() -> dict[str, Any]:
            return {
                "payloads": {
                    kind: {
                        "path": f"{prefix}{leaf}",
                        "size": 1,
                        "sha256": "a" * 64,
                    }
                    for kind, leaf in leaves.items()
                }
            }

        missing_metadata = manifest()
        del missing_metadata["payloads"]["metadata"]
        with self.assertRaisesRegex(
            portable.PortableEvidenceError, "unsupported payload registry"
        ):
            portable.verify_manifest_payload_bindings(missing_metadata, members, prefix)

        uppercase_digest = manifest()
        uppercase_digest["payloads"]["kernel"]["sha256"] = "A" * 64
        with self.assertRaisesRegex(
            portable.PortableEvidenceError, "kernel payload binding disagrees"
        ):
            portable.verify_manifest_payload_bindings(uppercase_digest, members, prefix)

        for padding in (" leading", "trailing "):
            padded_path = manifest()
            canonical = f"{prefix}kernel"
            padded_path["payloads"]["kernel"]["path"] = (
                f" {canonical}" if padding == " leading" else f"{canonical} "
            )
            with self.subTest(padding=padding), self.assertRaisesRegex(
                portable.PortableEvidenceError, "kernel payload binding disagrees"
            ):
                portable.verify_manifest_payload_bindings(padded_path, members, prefix)

    def test_sysupgrade_archive_requires_one_canonical_directory_member(self) -> None:
        prefix = "sysupgrade-am1-s9/"

        def archive_with_directories(*names: str) -> tarfile.TarFile:
            raw = io.BytesIO()
            with tarfile.open(
                fileobj=raw, mode="w", format=tarfile.USTAR_FORMAT
            ) as archive:
                for name in names:
                    member = tarfile.TarInfo(name)
                    member.type = tarfile.DIRTYPE
                    archive.addfile(member)
            raw.seek(0)
            return tarfile.open(fileobj=raw, mode="r:*")

        with archive_with_directories(prefix) as archive:
            portable.verify_canonical_sysupgrade_directory(archive, prefix)
        for directories in ((), (prefix, prefix), ("sysupgrade-am2-s19j/",)):
            with self.subTest(directories=directories):
                with archive_with_directories(*directories) as archive:
                    with self.assertRaisesRegex(
                        portable.PortableEvidenceError,
                        "exactly one canonical sysupgrade-am1-s9/ directory member",
                    ):
                        portable.verify_canonical_sysupgrade_directory(archive, prefix)

    def test_closure_packaging_artifact_alias_is_rejected(self) -> None:
        closure_path = self.release / "firmware.source-closure.json"
        closure = json.loads(closure_path.read_text())
        closure["prebuilt_rust_inputs"]["packaging_artifact"] = "release.tar"
        closure_path.write_bytes(portable.canonical_bytes(closure))
        self._sign(closure_path, self.release / "firmware.source-closure.json.sig")
        self._resign_and_seal()
        self.assert_rejected(self._verify)

    def test_wrong_inner_package_board_is_rejected_when_outer_layers_agree(self) -> None:
        primary = self.release / "dcentos-unit.tar"
        self._write_primary_artifact(primary, "s9", "am2-s19j")
        closure_path = self.release / "firmware.source-closure.json"
        closure = json.loads(closure_path.read_text())
        closure["artifacts"] = [source_closure.artifact_entry(str(primary))]
        closure_path.write_bytes(portable.canonical_bytes(closure))
        self._sign(closure_path, self.release / "firmware.source-closure.json.sig")
        self._resign_and_seal()
        self.assert_rejected(self._verify)

    def test_am2_evidence_policy_accepts_one_coherent_inner_package(self) -> None:
        primary = self.release / "dcentos-sysupgrade-am2-s19jpro.tar"
        self._write_primary_artifact(primary, "am2-s19jpro", "am2-s19j")
        closure = {
            "build": {"target": "am2-s19jpro"},
            "prebuilt_rust_inputs": {
                "packaging_artifact": primary.name,
                "entries": [],
            },
            "artifacts": [source_closure.artifact_entry(str(primary))],
        }
        payload = [portable.evidence(primary)]
        portable.verify_target_bindings(
            portable.SCHEMA,
            "am2-s19jpro",
            {"logical_name": "am2-s19jpro"},
            {"target": "am2-s19jpro"},
            closure,
        )
        portable.verify_target_artifact(
            portable.SCHEMA,
            "am2-s19jpro",
            closure,
            self.release,
            payload,
            self.public_key,
            self.public_key.read_bytes(),
        )
        for command in ("verify-stage", "verify"):
            with self.subTest(command=command):
                with self.assertRaises(portable.PortableEvidenceError):
                    portable.enforce_verification_mode(
                        portable.SCHEMA, "am2-s19jpro", command
                    )

    def test_historical_policy_does_not_follow_mutable_current_policy(self) -> None:
        value = portable.read_canonical(
            self.release / portable.INDEX_NAME, "current index"
        )
        value["schema"] = portable.HISTORICAL_SCHEMA
        value.pop("target")
        value.pop("output_name")
        changed = release_capsule_target_policy.ReleaseCapsuleTargetPolicy(
            target="s9",
            cargo_variant="future",
            primary_artifact="future-s9.tar",
            package_board="future-s9",
            release_stem="DCENTOS_FUTURE_S9",
            publication_admitted=True,
        )
        with mock.patch.dict(
            release_capsule_target_policy.POLICIES, {"s9": changed}, clear=False
        ):
            validated = portable.validate_index(value)
        self.assertEqual(validated["target"], "s9")

    def test_v2_policy_does_not_follow_mutable_current_policy(self) -> None:
        value = portable.read_canonical(
            self.release / portable.INDEX_NAME, "current index"
        )
        changed = release_capsule_target_policy.ReleaseCapsuleTargetPolicy(
            target="s9",
            cargo_variant="future",
            primary_artifact="future-s9.tar",
            package_board="future-s9",
            release_stem="DCENTOS_FUTURE_S9",
            publication_admitted=True,
        )
        with mock.patch.dict(
            release_capsule_target_policy.POLICIES, {"s9": changed}, clear=False
        ):
            validated = portable.validate_index(value)
        self.assertEqual(validated["target"], "s9")


if __name__ == "__main__":
    unittest.main()
