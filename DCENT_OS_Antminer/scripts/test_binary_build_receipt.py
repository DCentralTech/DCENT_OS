#!/usr/bin/env python3
"""Adversarial offline tests for post-build snapshot receipts."""

from __future__ import annotations

import hashlib
import json
import importlib.util
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("binary_build_receipt.py")
SOURCE_SNAPSHOT_SCRIPT = Path(__file__).with_name("source_snapshot.py")
INVOCATION_SCRIPT = Path(__file__).with_name("release_invocation.py")
BUILD_INPUT_SNAPSHOT_SCRIPT = Path(__file__).with_name("build_input_snapshot.py")
BUILD_DRIVER = Path(__file__).with_name("build-dcentrald.sh")
MODULE_SPEC = importlib.util.spec_from_file_location("binary_build_receipt", SCRIPT)
assert MODULE_SPEC is not None and MODULE_SPEC.loader is not None
RECEIPT_MODULE = importlib.util.module_from_spec(MODULE_SPEC)
MODULE_SPEC.loader.exec_module(RECEIPT_MODULE)
TARGET = "armv7-unknown-linux-musleabihf"
PROFILE = "release"
VARIANT = "zynq"


class BuildDriverIdentityTests(unittest.TestCase):
    def test_cross_builder_executes_and_records_immutable_image_identity(self) -> None:
        driver = BUILD_DRIVER.read_text(encoding="utf-8")
        self.assertIn(
            "DOCKER_IMAGE_ID=\"$(\"$DOCKER_BIN\" image inspect --format '{{.Id}}' \"$DOCKER_IMAGE\")\"",
            driver,
        )
        self.assertIn('DOCKER_BIN="${DCENT_DOCKER_BIN:-}"', driver)
        self.assertIn('DOCKER_SPEC_ARGV[0]="$DOCKER_BIN"', driver)
        self.assertIn("'^sha256:[0-9a-f]{64}$'", driver)
        self.assertIn('"$DOCKER_IMAGE_ID" \\\n    bash -c', driver)
        self.assertNotIn('"$DOCKER_IMAGE" \\\n    bash -c', driver)
        self.assertIn("builder_base_reference=$DCENT_BUILDER_BASE_REFERENCE", driver)
        self.assertIn("builder_image_id=$DCENT_BUILDER_IMAGE_ID", driver)
        self.assertIn("DCENT_BUILDER_[^=]*", driver)

    def test_release_cross_builder_base_requires_manifest_digest(self) -> None:
        driver = BUILD_DRIVER.read_text(encoding="utf-8")
        self.assertIn("DCENT_RUST_BUILDER_BASE", driver)
        self.assertIn("@sha256:[0-9a-f]{64}", driver)
        self.assertIn("mutable Docker tags are development-only", driver)
        packaging = BUILD_DRIVER.with_name("build_in_docker.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn("export_args+=(--require-immutable-builder)", packaging)


class ReceiptFixture(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="dcent-receipt-test-")
        self.root = Path(self.temporary.name)
        self.workspace = self.root / "DCENT_OS_Antminer/dcentrald"
        self.result_root = self.workspace / "target"
        self.release = self.workspace / f"target/{TARGET}/{PROFILE}"
        self.metadata = self.workspace / f"target/release-inventory/{TARGET}.metadata.json"
        self.toolchain = self.workspace / f"target/release-inventory/{TARGET}.toolchain.txt"
        self.compile_environment = (
            self.workspace / f"target/release-inventory/{TARGET}.compile-env.txt"
        )
        self.binary = self.release / "dcentrald"
        self.receipt = self.binary.with_name("dcentrald.build-receipt.json")
        self.stage_parent = self.workspace / "target/export-stages"
        (self.workspace / "src").mkdir(parents=True)
        (self.root / "projects/dcent-schema/src").mkdir(parents=True)
        (self.root / "DCENT_OS_Antminer/configs").mkdir(parents=True)
        (self.root / "DCENT_OS_Antminer/scripts/hw-acceptance").mkdir(parents=True)
        (self.root / "DCENT_OS_Antminer/docs/architecture").mkdir(parents=True)
        (self.root / "").mkdir(parents=True)
        self.release.mkdir(parents=True)
        self.stage_parent.mkdir(parents=True)
        self.metadata.parent.mkdir(parents=True)

        (self.root / ".gitignore").write_text("**/target/\n", encoding="utf-8")
        (self.workspace / "Cargo.toml").write_text(
            "[package]\nname='fixture'\nversion='0.1.0'\n", encoding="utf-8"
        )
        (self.workspace / "Cargo.lock").write_text("# fixture lock\n", encoding="utf-8")
        self.source = self.workspace / "src/main.rs"
        self.source.write_text("fn main() {}\n", encoding="utf-8")
        (self.root / "projects/dcent-schema/src/lib.rs").write_text(
            "pub const SCHEMA: u8 = 1;\n", encoding="utf-8"
        )
        (self.root / "DCENT_OS_Antminer/configs/baked.toml").write_text(
            "mode='fixture'\n", encoding="utf-8"
        )
        (self.root / "DCENT_OS_Antminer/scripts/hw-acceptance/skus.conf").write_text(
            "fixture|fixture-chain|1\n", encoding="utf-8"
        )
        (self.root / "DCENT_OS_Antminer/docs/architecture/install_matrix.tsv").write_text(
            "model\tcontrol_board\nfixture\tfixture-board\n", encoding="utf-8"
        )
        (
            self.root
            / "DCENT_OS_Antminer/docs/architecture/hardware_enablement_matrix.json"
        ).write_text('{"schema":1,"targets":[]}\n', encoding="utf-8")
        (self.root / "").write_text(
            '{"fixture":true}\n', encoding="utf-8"
        )
        (
            self.root
            / ""
        ).write_bytes(b"fixture-signature")
        (self.root / "DCENT_OS_Antminer/scripts/build-dcentrald.sh").write_text(
            "#!/bin/sh\n# fixture build driver\n", encoding="utf-8"
        )
        (self.root / "DCENT_OS_Antminer/scripts/binary_build_receipt.py").write_text(
            "# fixture receipt generator\n", encoding="utf-8"
        )
        self.external_root = self.root.parent / f"{self.root.name}-external-inputs"
        self.external_s9_kernel = (
            self.external_root / ""
        )
        self.external_s9_kernel.parent.mkdir(parents=True)
        self.external_s9_kernel.write_bytes(b"fixture-s9-kernel")
        self.external_s9_kernel_sha256 = hashlib.sha256(
            self.external_s9_kernel.read_bytes()
        ).hexdigest()
        (self.root / "DCENT_OS_Antminer/scripts/build_inputs.manifest").write_text(
            f"{self.external_s9_kernel_sha256}  "
            "\n",
            encoding="utf-8",
        )
        (self.root / "DCENT_OS_Antminer/dcentrald/dcentrald_s21xp.toml").write_text(
            "[miner]\nmodel='fixture'\n", encoding="utf-8"
        )
        self.binary.write_bytes(b"fixture-elf")
        self.metadata.write_text('{"packages":[],"version":1}\n', encoding="utf-8")
        self.toolchain.write_text(
            "rustc 1.90.0 (fixture)\nbinary: rustc\nhost: fixture\ncargo 1.90.0 (fixture)\n",
            encoding="utf-8",
        )
        self.compile_environment.write_text(
            "CARGO_BUILD_PROFILE=release\n"
            "DCENT_BUILDER_KIND=docker-cross\n"
            "DCENT_BUILDER_BASE_REFERENCE=rust@sha256:1111111111111111111111111111111111111111111111111111111111111111\n"
            "DCENT_BUILDER_IMAGE_ID=sha256:2222222222222222222222222222222222222222222222222222222222222222\n"
            "DCENT_BUILDER_PACKAGE_RESOLUTION=fixture-not-reproducible\n"
            "RUSTFLAGS=-C target-cpu=cortex-a9\n",
            encoding="utf-8",
        )

        self.git("init", "-q")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("config", "user.name", "Receipt Fixture")
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        self.source_commit = self.git("rev-parse", "HEAD").stdout.strip()
        capsule_parent = self.root / "capsule-control"
        capsule_parent.mkdir()
        snapshot_result = subprocess.run(
            [
                sys.executable,
                str(SOURCE_SNAPSHOT_SCRIPT),
                "create",
                "--repo-root",
                str(self.root),
                "--commit",
                self.source_commit,
                "--stage-parent",
                str(capsule_parent),
            ],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.snapshot_result = json.loads(snapshot_result.stdout)
        self.source_snapshot = Path(self.snapshot_result["snapshot"])
        self.source_snapshot_tree = Path(self.snapshot_result["tree"])
        build_input_result = subprocess.run(
            [
                sys.executable,
                str(BUILD_INPUT_SNAPSHOT_SCRIPT),
                "create",
                "--repo-root",
                str(self.external_root),
                "--selection-root",
                str(self.source_snapshot_tree),
                "--build-input-manifest",
                str(
                    self.source_snapshot_tree
                    / "DCENT_OS_Antminer/scripts/build_inputs.manifest"
                ),
                "--target",
                "cargo-workspace",
                "--stage-parent",
                str(capsule_parent),
            ],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.build_input_result = json.loads(build_input_result.stdout)
        self.build_input_snapshot = Path(self.build_input_result["snapshot"])
        invocation_result = subprocess.run(
            [
                sys.executable,
                str(INVOCATION_SCRIPT),
                "create",
                "--stage-parent",
                str(capsule_parent),
                "--name",
                "receipt-test",
            ],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.invocation_result = json.loads(invocation_result.stdout)
        self.release_invocation = Path(self.invocation_result["stage"])
        self.run_receipt("create", check=True)

    def tearDown(self) -> None:
        self.temporary.cleanup()
        shutil.rmtree(self.external_root, ignore_errors=True)

    def git(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", "-C", str(self.root), *args],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def run_receipt(
        self,
        command: str,
        *,
        target: str = TARGET,
        profile: str = PROFILE,
        source_commit: str | None = None,
        release_invocation: Path | None = None,
        check: bool = False,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                command,
                *self.context_arguments(
                    target=target,
                    profile=profile,
                    source_commit=source_commit,
                    release_invocation=release_invocation,
                ),
                "--binary",
                str(self.binary),
            ],
            check=check,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def context_arguments(
        self,
        *,
        target: str = TARGET,
        profile: str = PROFILE,
        source_commit: str | None = None,
        release_invocation: Path | None = None,
    ) -> list[str]:
        return [
            "--git-object-repo",
            str(self.root),
            "--source-snapshot",
            str(self.source_snapshot),
            "--source-commit",
            source_commit or self.source_commit,
            "--source-workspace",
            "DCENT_OS_Antminer/dcentrald",
            "--release-invocation",
            str(release_invocation or self.release_invocation),
            "--result-root",
            str(self.result_root),
            "--build-input-snapshot",
            str(self.build_input_snapshot),
            "--target",
            target,
            "--profile",
            profile,
            "--build-variant",
            VARIANT,
            "--metadata",
            str(self.metadata),
            "--toolchain-context",
            str(self.toolchain),
            "--compile-environment",
            str(self.compile_environment),
        ]

    def export_arguments(
        self,
        pairs: list[tuple[Path, Path]] | None = None,
        *,
        require_immutable_builder: bool = False,
    ) -> list[str]:
        arguments = [
            "export-snapshot-set",
            *self.context_arguments(),
            "--stage-parent",
            str(self.stage_parent),
        ]
        if require_immutable_builder:
            arguments.append("--require-immutable-builder")
        for binary, receipt in pairs or [(self.binary, self.receipt)]:
            arguments.extend(["--pair", str(binary), str(receipt)])
        return arguments

    def run_export(
        self,
        pairs: list[tuple[Path, Path]] | None = None,
        *,
        require_immutable_builder: bool = False,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                *self.export_arguments(
                    pairs, require_immutable_builder=require_immutable_builder
                ),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def run_stage_command(
        self,
        command: str,
        stage: Path,
        *,
        capability: Path | None = None,
    ) -> subprocess.CompletedProcess[str]:
        arguments = [sys.executable, str(SCRIPT), command, "--stage", str(stage)]
        if command == "destroy-export-snapshot-set":
            arguments.extend(
                [
                    "--capability",
                    str(capability or self.capability_path(stage)),
                ]
            )
        return subprocess.run(
            arguments,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def capability_path(self, stage: Path) -> Path:
        return (
            stage.parent
            / ".dcent-export-capabilities"
            / f"{stage.name}.destroy-capability.json"
        )

    def exported_paths(self, stage: Path) -> tuple[Path, Path, Path]:
        descriptor_path = stage / "export-set.json"
        descriptor = json.loads(descriptor_path.read_text(encoding="utf-8"))
        pair = descriptor["artifacts"][0]
        binary = stage.joinpath(*pair["binary"]["export_path"].split("/"))
        receipt = stage.joinpath(*pair["receipt"]["export_path"].split("/"))
        return descriptor_path, binary, receipt

    def run_path_query(
        self, stage: Path, *arguments: str
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "query-export-snapshot-path",
                "--stage",
                str(stage),
                *arguments,
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def run_retain(
        self, stage: Path, output_dir: Path, prefix: str = "release.tar"
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "retain-export-snapshot-set",
                "--stage",
                str(stage),
                "--output-dir",
                str(output_dir),
                "--artifact-prefix",
                prefix,
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def assert_verify_fails(self, expected: str | None = None, **kwargs: str) -> None:
        result = self.run_receipt("verify", **kwargs)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        if expected:
            self.assertIn(expected, result.stderr)

    def replace_with_symlink(
        self, path: Path, target: Path, *, is_directory: bool = False
    ) -> None:
        try:
            os.symlink(target, path, target_is_directory=is_directory)
        except (NotImplementedError, OSError) as error:
            self.skipTest(f"symlink creation is unavailable: {error}")

    def create_windows_junction(self, path: Path, target: Path) -> None:
        if os.name != "nt":
            self.skipTest("NTFS junction regression is Windows-only")
        result = subprocess.run(
            ["cmd.exe", "/d", "/c", "mklink", "/J", str(path), str(target)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if result.returncode != 0:
            self.skipTest(f"junction creation is unavailable: {result.stderr}")

    def create_second_invocation(self, name: str = "receipt-other") -> Path:
        result = subprocess.run(
            [
                sys.executable,
                str(INVOCATION_SCRIPT),
                "create",
                "--stage-parent",
                str(self.release_invocation.parent),
                "--name",
                name,
            ],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        return Path(json.loads(result.stdout)["stage"])

    def test_valid_matching_receipt_passes(self) -> None:
        result = self.run_receipt("verify")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("verified post-build snapshot receipt", result.stdout)

    def test_receipt_contains_no_host_absolute_paths(self) -> None:
        raw = self.receipt.read_text(encoding="utf-8")
        self.assertNotIn(str(self.root), raw)
        parsed = json.loads(raw)
        self.assertEqual(parsed["binary"]["path"], f"{TARGET}/{PROFILE}/dcentrald")
        self.assertEqual(
            parsed["cargo_metadata"]["path"],
            f"release-inventory/{TARGET}.metadata.json",
        )

    def test_swapped_invocation_is_rejected_by_exact_capsule_equality(self) -> None:
        other = self.create_second_invocation()
        rejected = self.run_receipt("verify", release_invocation=other)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("release_capsule", rejected.stderr)

    def test_forged_snapshot_descriptor_is_rejected_before_receipt_comparison(self) -> None:
        descriptor = self.source_snapshot
        os.chmod(descriptor, stat.S_IREAD | stat.S_IWRITE)
        value = json.loads(descriptor.read_text(encoding="utf-8"))
        value["snapshot_id"] = "0" * 64
        descriptor.write_text(
            json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
            encoding="utf-8",
        )
        rejected = self.run_receipt("verify")
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("source snapshot verification failed", rejected.stderr)

    def test_result_evidence_path_escape_is_rejected(self) -> None:
        outside = self.root / "outside-metadata.json"
        outside.write_text('{}\n', encoding="utf-8")
        arguments = self.context_arguments()
        metadata_index = arguments.index("--metadata") + 1
        arguments[metadata_index] = str(outside)
        rejected = subprocess.run(
            [sys.executable, str(SCRIPT), "verify", *arguments, "--binary", str(self.binary)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("must be inside repo root", rejected.stderr)

    def test_live_tree_mutation_cannot_change_authenticated_snapshot_receipt(self) -> None:
        self.source.write_text("fn main() { println!(\"changed\"); }\n", encoding="utf-8")
        os.utime(self.binary, None)
        verified = self.run_receipt("verify")
        self.assertEqual(verified.returncode, 0, verified.stderr)

    def test_copied_receipt_with_modified_binary_fails(self) -> None:
        self.binary.write_bytes(b"different-fixture-elf")
        self.assert_verify_fails("binary")

    def test_sibling_live_tree_mutation_does_not_replace_snapshot_bytes(self) -> None:
        schema = self.root / "projects/dcent-schema/src/lib.rs"
        schema.write_text("pub const SCHEMA: u8 = 2;\n", encoding="utf-8")
        verified = self.run_receipt("verify")
        self.assertEqual(verified.returncode, 0, verified.stderr)

    def test_wrong_target_and_profile_fail(self) -> None:
        self.assert_verify_fails("target_triple", target="aarch64-unknown-linux-musl")
        self.assert_verify_fails("profile", profile="debug")

    def test_live_head_change_does_not_change_exact_snapshot_commit(self) -> None:
        marker = self.root / "unrelated.txt"
        marker.write_text("commit-only identity change\n", encoding="utf-8")
        self.git("add", "unrelated.txt")
        self.git("commit", "-qm", "identity change")
        verified = self.run_receipt("verify")
        self.assertEqual(verified.returncode, 0, verified.stderr)
        self.assert_verify_fails("source snapshot", source_commit=self.git("rev-parse", "HEAD").stdout.strip())

    def test_missing_receipt_fails(self) -> None:
        self.binary.with_name("dcentrald.build-receipt.json").unlink()
        self.assert_verify_fails("required build receipt is missing")

    def test_metadata_digest_drift_fails(self) -> None:
        self.metadata.write_text('{"packages":["changed"],"version":1}\n', encoding="utf-8")
        self.assert_verify_fails("cargo_metadata")

    def test_toolchain_and_compile_environment_drift_fail(self) -> None:
        self.toolchain.write_text("rustc 1.91.0 (wrong)\n", encoding="utf-8")
        self.assert_verify_fails("toolchain_context")
        self.toolchain.write_text(
            "rustc 1.90.0 (fixture)\nbinary: rustc\nhost: fixture\ncargo 1.90.0 (fixture)\n",
            encoding="utf-8",
        )
        self.compile_environment.write_text(
            "CARGO_BUILD_PROFILE=release\n"
            "DCENT_BUILDER_KIND=docker-cross\n"
            "DCENT_BUILDER_BASE_REFERENCE=rust@sha256:1111111111111111111111111111111111111111111111111111111111111111\n"
            "DCENT_BUILDER_IMAGE_ID=sha256:2222222222222222222222222222222222222222222222222222222222222222\n"
            "DCENT_BUILDER_PACKAGE_RESOLUTION=fixture-not-reproducible\n"
            "RUSTFLAGS=-C target-cpu=cortex-a7\n",
            encoding="utf-8",
        )
        self.assert_verify_fails("compile_environment")

    def test_receipt_is_deterministic(self) -> None:
        receipt = self.binary.with_name("dcentrald.build-receipt.json")
        first = receipt.read_bytes()
        self.run_receipt("create", check=True)
        self.assertEqual(first, receipt.read_bytes())
        parsed = json.loads(first)
        self.assertEqual(parsed["schema_version"], 4)
        self.assertEqual(
            set(parsed["release_capsule"]),
            {
                "schema",
                "release_invocation_descriptor_sha256",
                "release_invocation_id",
                "source_snapshot_id",
                "source_snapshot_descriptor_sha256",
            },
        )
        self.assertEqual(
            parsed["claim"],
            "declared-release-capsule-and-post-build-snapshot-consistency-"
            "not-build-causality-or-reproducibility-proof",
        )
        self.assertEqual(parsed["git"]["source_kind"], "exact-git-object-snapshot")
        self.assertEqual(
            set(parsed["build_inputs"]),
            {"claim", "evidence", "selection_authority"},
        )
        self.assertEqual(
            parsed["build_inputs"]["selection_authority"],
            "manifest-from-same-git-authenticated-release-capsule-source-snapshot",
        )
        self.assertEqual(parsed["build_inputs"]["evidence"]["files"], [])

    def test_build_input_snapshot_manifest_integrity_is_required(self) -> None:
        descriptor = json.loads(self.build_input_snapshot.read_text(encoding="utf-8"))
        staged_manifest = (
            self.build_input_snapshot.parent / descriptor["manifest"]["staged_path"]
        )
        os.chmod(staged_manifest, 0o600)
        staged_manifest.write_bytes(b"tampered-captured-manifest")
        rejected = self.run_receipt("verify")
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("build-input snapshot", rejected.stderr)

    def test_unselected_external_input_mutation_does_not_change_captured_receipt(self) -> None:
        before = self.receipt.read_bytes()
        self.external_s9_kernel.write_bytes(b"mutated-after-snapshot")
        self.run_receipt("create", check=True)
        self.assertEqual(self.receipt.read_bytes(), before)

    def test_compile_environment_rejects_removed_stock_fpga_authority(self) -> None:
        self.compile_environment.write_text(
            self.compile_environment.read_text(encoding="utf-8")
            + f"DCENT_STOCK_FPGA_SHA256={'0' * 64}\n",
            encoding="utf-8",
        )
        rejected = self.run_receipt("create")
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("removed stock FPGA authority", rejected.stderr)

    def test_release_export_requires_semantic_immutable_builder_identity(self) -> None:
        accepted = self.run_export(require_immutable_builder=True)
        self.assertEqual(accepted.returncode, 0, accepted.stderr)

        for key, value, expected in (
            (
                "DCENT_BUILDER_BASE_REFERENCE",
                "rust:1.90-bookworm",
                "immutable builder base digest",
            ),
            ("DCENT_BUILDER_IMAGE_ID", "mutable-tag", "immutable builder image ID"),
            ("DCENT_BUILDER_KIND", "native-host", "docker-cross"),
        ):
            with self.subTest(key=key):
                lines = self.compile_environment.read_text(encoding="utf-8").splitlines()
                rewritten = [
                    f"{key}={value}" if line.startswith(f"{key}=") else line
                    for line in lines
                ]
                self.compile_environment.write_text(
                    "\n".join(rewritten) + "\n", encoding="utf-8"
                )
                self.run_receipt("create", check=True)
                rejected = self.run_export(require_immutable_builder=True)
                self.assertNotEqual(rejected.returncode, 0)
                self.assertIn(expected, rejected.stderr)
                self.compile_environment.write_text(
                    "CARGO_BUILD_PROFILE=release\n"
                    "DCENT_BUILDER_KIND=docker-cross\n"
                    "DCENT_BUILDER_BASE_REFERENCE=rust@sha256:"
                    + "1" * 64
                    + "\nDCENT_BUILDER_IMAGE_ID=sha256:"
                    + "2" * 64
                    + "\nDCENT_BUILDER_PACKAGE_RESOLUTION=fixture-not-reproducible\n"
                    "RUSTFLAGS=-C target-cpu=cortex-a9\n",
                    encoding="utf-8",
                )
                self.run_receipt("create", check=True)

    def test_export_rejects_receipt_from_another_verified_invocation(self) -> None:
        first_receipt = self.receipt.read_bytes()
        other = self.create_second_invocation("receipt-mixed")
        self.run_receipt("create", release_invocation=other, check=True)
        rejected = self.run_export()
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("release_capsule", rejected.stderr)
        self.receipt.write_bytes(first_receipt)

    def test_export_rejects_malformed_schema_v4_fields(self) -> None:
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        receipt["host_temporary_path"] = str(self.root)
        self.receipt.write_bytes(RECEIPT_MODULE.canonical_json_bytes(receipt))
        rejected = self.run_export()
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("exact schema-v4 receipt fields", rejected.stderr)

    def test_export_rejects_forged_raw_capsule_identifier(self) -> None:
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        receipt["release_capsule"]["release_invocation_id"] = "A" * 64
        self.receipt.write_bytes(RECEIPT_MODULE.canonical_json_bytes(receipt))
        rejected = self.run_export()
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("invalid release_capsule", rejected.stderr)

    def test_schema_v3_is_inspectable_but_not_admitted_to_new_export(self) -> None:
        receipt = json.loads(self.receipt.read_text(encoding="utf-8"))
        receipt["schema_version"] = 3
        receipt.pop("release_capsule")
        receipt["claim"] = RECEIPT_MODULE.HISTORICAL_RECEIPT_CLAIM_V3
        self.receipt.write_bytes(RECEIPT_MODULE.canonical_json_bytes(receipt))
        inspected = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "inspect-receipt",
                "--receipt",
                str(self.receipt),
                "--binary",
                str(self.binary),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertEqual(inspected.returncode, 0, inspected.stderr)
        rejected = self.run_export()
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("schema-v4", rejected.stderr)

    def test_exact_non_rust_compile_inputs_are_in_inventory(self) -> None:
        receipt = json.loads(
            self.binary.with_name("dcentrald.build-receipt.json").read_text(encoding="utf-8")
        )
        paths = {entry["path"] for entry in receipt["source_inventory"]}
        self.assertIn("DCENT_OS_Antminer/scripts/hw-acceptance/skus.conf", paths)
        self.assertIn("DCENT_OS_Antminer/docs/architecture/install_matrix.tsv", paths)
        self.assertIn(
            "DCENT_OS_Antminer/docs/architecture/hardware_enablement_matrix.json",
            paths,
        )
        for relative in RECEIPT_MODULE.BAKED_INPUTS:
            self.assertIn(relative, paths)
        self.assertIn("DCENT_OS_Antminer/dcentrald/dcentrald_s21xp.toml", paths)

    def test_live_deletion_of_required_inputs_does_not_change_snapshot(self) -> None:
        for relative in RECEIPT_MODULE.REQUIRED_SOURCE_INPUTS:
            with self.subTest(relative=relative):
                path = self.root / relative
                content = path.read_bytes()
                path.unlink()
                verified = self.run_receipt("verify")
                self.assertEqual(verified.returncode, 0, verified.stderr)
                path.write_bytes(content)

    def test_symlinked_binary_is_rejected(self) -> None:
        target = self.root / "binary-target"
        self.binary.replace(target)
        self.replace_with_symlink(self.binary, target)
        self.assert_verify_fails("symlink")

    def test_symlinked_metadata_is_rejected(self) -> None:
        target = self.root / "metadata-target"
        self.metadata.replace(target)
        self.replace_with_symlink(self.metadata, target)
        self.assert_verify_fails("symlink")

    def test_symlinked_context_files_are_rejected(self) -> None:
        for path in (self.toolchain, self.compile_environment):
            with self.subTest(path=path.name):
                target = self.root / f"{path.name}-target"
                path.replace(target)
                self.replace_with_symlink(path, target)
                self.assert_verify_fails("symlink")
                path.unlink()
                target.replace(path)

    def test_live_symlinked_source_file_is_not_a_receipt_input(self) -> None:
        target = self.root / "main-rs-target"
        self.source.replace(target)
        self.replace_with_symlink(self.source, target)
        verified = self.run_receipt("verify")
        self.assertEqual(verified.returncode, 0, verified.stderr)

    def test_live_symlinked_source_directory_is_not_a_receipt_input(self) -> None:
        source_directory = self.workspace / "src"
        target = self.root / "source-directory-target"
        source_directory.replace(target)
        self.replace_with_symlink(source_directory, target, is_directory=True)
        verified = self.run_receipt("verify")
        self.assertEqual(verified.returncode, 0, verified.stderr)

    def test_live_symlinked_source_root_is_not_a_receipt_input(self) -> None:
        source_root = self.root / "projects/dcent-schema"
        target = self.root / "schema-root-target"
        source_root.replace(target)
        self.replace_with_symlink(source_root, target, is_directory=True)
        verified = self.run_receipt("verify")
        self.assertEqual(verified.returncode, 0, verified.stderr)

    def test_live_windows_junction_is_not_a_receipt_input(self) -> None:
        target = self.root / "junction-source-target"
        target.mkdir()
        (target / "outside.rs").write_text("outside\n", encoding="utf-8")
        junction = self.workspace / "src/junction"
        self.create_windows_junction(junction, target)
        try:
            verified = self.run_receipt("verify")
            self.assertEqual(verified.returncode, 0, verified.stderr)
        finally:
            os.rmdir(junction)

    def test_open_file_replacement_race_is_rejected(self) -> None:
        victim = self.root / "race-victim"
        replacement = self.root / "race-replacement"
        victim.write_bytes(b"original")
        replacement.write_bytes(b"replacement")

        def replace_after_open(path: Path, _descriptor: int) -> None:
            if path == victim:
                os.replace(replacement, victim)

        previous = RECEIPT_MODULE._AFTER_OPEN_HOOK
        RECEIPT_MODULE._AFTER_OPEN_HOOK = replace_after_open
        try:
            with self.assertRaises(RECEIPT_MODULE.ReceiptError):
                RECEIPT_MODULE.read_regular_snapshot(victim, "race fixture")
        finally:
            RECEIPT_MODULE._AFTER_OPEN_HOOK = previous

    def test_private_export_set_round_trip_and_modes(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertEqual(len(result.stdout.splitlines()), 1)
        stage = Path(result.stdout.strip())
        self.assertTrue(stage.is_dir())
        capability = self.capability_path(stage)
        self.assertTrue(capability.is_file())
        self.assertEqual(capability.parent.parent, stage.parent)
        self.assertNotEqual(capability.parent, stage)
        descriptor, exported_binary, exported_receipt = self.exported_paths(stage)
        self.assertEqual(exported_binary.read_bytes(), self.binary.read_bytes())
        self.assertEqual(exported_receipt.read_bytes(), self.receipt.read_bytes())
        parsed = json.loads(descriptor.read_text(encoding="utf-8"))
        expected_descriptor = (
            json.dumps(parsed, sort_keys=True, separators=(",", ":")) + "\n"
        ).encode("utf-8")
        self.assertEqual(descriptor.read_bytes(), expected_descriptor)
        if os.name == "posix":
            self.assertEqual(stat.S_IMODE(stage.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE((stage / "artifacts").stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE(capability.parent.stat().st_mode), 0o700)
            for path in (descriptor, exported_binary, exported_receipt):
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o400)
            self.assertEqual(stat.S_IMODE(capability.stat().st_mode), 0o400)
        for path in (descriptor, exported_binary, exported_receipt, capability):
            self.assertEqual(path.stat().st_nlink, 1)

        verified = self.run_stage_command("verify-export-snapshot-set", stage)
        self.assertEqual(verified.returncode, 0, verified.stderr)
        self.assertEqual(verified.stdout, f"{stage}\n")
        queried = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "export-snapshot-capability-path",
                "--stage",
                str(stage),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertEqual(queried.returncode, 0, queried.stderr)
        self.assertEqual(queried.stdout, f"{capability}\n")

    def test_export_descriptor_and_bytes_are_deterministic(self) -> None:
        first_result = self.run_export()
        second_result = self.run_export()
        self.assertEqual(first_result.returncode, 0, first_result.stderr)
        self.assertEqual(second_result.returncode, 0, second_result.stderr)
        first = Path(first_result.stdout.strip())
        second = Path(second_result.stdout.strip())
        self.assertNotEqual(first, second)
        first_paths = self.exported_paths(first)
        second_paths = self.exported_paths(second)
        self.assertEqual(
            [path.read_bytes() for path in first_paths],
            [path.read_bytes() for path in second_paths],
        )

    def test_retain_export_set_copies_exact_bytes_and_is_deterministic(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        output_one = self.root / "retained-one"
        output_two = self.root / "retained-two"
        output_one.mkdir()
        output_two.mkdir()

        first = self.run_retain(stage, output_one)
        second = self.run_retain(stage, output_two)
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(first.stdout, second.stdout)
        parsed = json.loads(first.stdout)
        self.assertEqual(
            first.stdout,
            json.dumps(parsed, sort_keys=True, separators=(",", ":")) + "\n",
        )
        self.assertEqual([item["name"] for item in parsed["artifacts"]], ["dcentrald"])
        _, exported_binary, exported_receipt = self.exported_paths(stage)
        expected = {
            "release.tar.prebuilt-rust.dcentrald.bin": exported_binary.read_bytes(),
            "release.tar.prebuilt-rust.dcentrald.build-receipt.json": (
                exported_receipt.read_bytes()
            ),
        }
        for name, raw in expected.items():
            self.assertEqual((output_one / name).read_bytes(), raw)
            self.assertEqual((output_two / name).read_bytes(), raw)
            self.assertEqual((output_one / name).stat().st_nlink, 1)
        self.assertEqual(
            sorted(path.name for path in output_one.iterdir()), sorted(expected)
        )

    def test_retain_refuses_existing_output_without_overwrite(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        output = self.root / "retained-existing"
        output.mkdir()
        collision = output / "release.tar.prebuilt-rust.dcentrald.bin"
        collision.write_bytes(b"operator-owned-existing-output")

        retained = self.run_retain(stage, output)
        self.assertNotEqual(retained.returncode, 0)
        self.assertEqual(collision.read_bytes(), b"operator-owned-existing-output")
        self.assertEqual(sorted(path.name for path in output.iterdir()), [collision.name])

    def test_atomic_retain_publication_does_not_overwrite_raced_output(self) -> None:
        output = self.root / "retained-race"
        output.mkdir()
        destination = output / "release.tar.prebuilt-rust.dcentrald.bin"
        real_publish = RECEIPT_MODULE.publish_staged_file

        def race_destination(
            source: Path,
            target: Path,
            *,
            require_directory_sync: bool = False,
            require_staged_cleanup: bool = False,
            expected_staged_identity: tuple[int, int] | None = None,
        ) -> tuple[str, str]:
            self.assertTrue(require_directory_sync)
            self.assertTrue(require_staged_cleanup)
            self.assertEqual(
                expected_staged_identity,
                (source.stat().st_dev, source.stat().st_ino),
            )
            Path(target).write_bytes(b"raced-operator-output")
            return real_publish(
                source,
                target,
                require_directory_sync=require_directory_sync,
                require_staged_cleanup=require_staged_cleanup,
                expected_staged_identity=expected_staged_identity,
            )

        with mock.patch.object(
            RECEIPT_MODULE,
            "publish_staged_file",
            side_effect=race_destination,
        ):
            with self.assertRaises(RECEIPT_MODULE.ReceiptError):
                RECEIPT_MODULE._atomic_raw_write_new(destination, b"retained-bytes")
        self.assertEqual(destination.read_bytes(), b"raced-operator-output")
        self.assertEqual(sorted(path.name for path in output.iterdir()), [destination.name])

    def test_atomic_retain_does_not_touch_reoccupied_staging_after_commit(self) -> None:
        output = self.root / "retained-postcommit-staging"
        output.mkdir()
        destination = output / "release.tar.prebuilt-rust.dcentrald.bin"

        def publish_then_reoccupy(
            staging: Path,
            target: Path,
            *,
            require_directory_sync: bool = False,
            require_staged_cleanup: bool = False,
            expected_staged_identity: tuple[int, int] | None = None,
        ) -> tuple[str, str]:
            self.assertTrue(require_directory_sync)
            self.assertTrue(require_staged_cleanup)
            self.assertEqual(
                expected_staged_identity,
                (staging.stat().st_dev, staging.stat().st_ino),
            )
            content = staging.read_bytes()
            staging.unlink()
            target.write_bytes(content)
            staging.mkdir()
            return "pass", "removed"

        with mock.patch.object(
            RECEIPT_MODULE,
            "publish_staged_file",
            side_effect=publish_then_reoccupy,
        ):
            RECEIPT_MODULE._atomic_raw_write_new(destination, b"retained-bytes")
        self.assertEqual(destination.read_bytes(), b"retained-bytes")
        pending = list(output.glob("*.publication-pending.*"))
        self.assertEqual(len(pending), 1)
        self.assertTrue(pending[0].is_dir())

    @unittest.skipUnless(os.name == "posix", "POSIX signal delivery regression")
    def test_retain_signal_after_partial_publication_rolls_back_exact_set(
        self,
    ) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        output = self.root / "retained-partial-signal"
        output.mkdir()
        code = f"""
import importlib.util
import os
from pathlib import Path
import signal
import sys

script = Path({str(SCRIPT)!r})
sys.path.insert(0, str(script.parent))
spec = importlib.util.spec_from_file_location("binary_receipt_signal_test", script)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
real_publish = module.publish_staged_file
publish_calls = 0

def signal_after_first_commit(*args, **kwargs):
    global publish_calls
    result = real_publish(*args, **kwargs)
    publish_calls += 1
    if publish_calls == 1:
        os.kill(os.getpid(), signal.SIGTERM)
    return result

module.publish_staged_file = signal_after_first_commit
sys.argv = [
    str(script),
    "retain-export-snapshot-set",
    "--stage",
    {str(stage)!r},
    "--output-dir",
    {str(output)!r},
    "--artifact-prefix",
    "release.tar",
]
raise SystemExit(module.main())
"""
        retained = subprocess.run(
            [sys.executable, "-c", code],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.assertNotEqual(retained.returncode, 0)
        self.assertIn("before durable retained export-set publication", retained.stderr)
        self.assertEqual(list(output.iterdir()), [])

    @unittest.skipUnless(os.name == "posix", "POSIX signal delivery regression")
    def test_retain_signal_after_set_completion_cannot_revoke_commit(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        output = self.root / "retained-postcommit-signal"
        output.mkdir()
        code = f"""
import importlib.util
import os
from pathlib import Path
import signal
import sys

script = Path({str(SCRIPT)!r})
sys.path.insert(0, str(script.parent))
spec = importlib.util.spec_from_file_location("binary_receipt_signal_test", script)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
real_retain = module.retain_export_snapshot_set

def signal_after_set(*args, **kwargs):
    result = real_retain(*args, **kwargs)
    os.kill(os.getpid(), signal.SIGTERM)
    return result

module.retain_export_snapshot_set = signal_after_set
sys.argv = [
    str(script),
    "retain-export-snapshot-set",
    "--stage",
    {str(stage)!r},
    "--output-dir",
    {str(output)!r},
    "--artifact-prefix",
    "release.tar",
]
raise SystemExit(module.main())
"""
        retained = subprocess.run(
            [sys.executable, "-c", code],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.assertEqual(retained.returncode, 0, retained.stderr)
        self.assertIn("ignored signal", retained.stderr)
        self.assertEqual(len(json.loads(retained.stdout)["artifacts"]), 1)
        self.assertEqual(
            sorted(path.name for path in output.iterdir()),
            [
                "release.tar.prebuilt-rust.dcentrald.bin",
                "release.tar.prebuilt-rust.dcentrald.build-receipt.json",
            ],
        )

    def test_retain_rollback_reports_cleanup_fault_and_still_syncs(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        output = self.root / "retained-rollback-fault"
        output.mkdir()
        first_destination = output / "release.tar.prebuilt-rust.dcentrald.bin"
        real_publish = RECEIPT_MODULE._atomic_raw_write_new
        publish_calls = 0

        def publish_then_fail(path: Path, raw: bytes) -> tuple[int, int]:
            nonlocal publish_calls
            publish_calls += 1
            if publish_calls == 2:
                raise RECEIPT_MODULE.ReceiptError("injected second-file failure")
            return real_publish(path, raw)

        real_unlink = Path.unlink

        def reject_first_rollback(path: Path, *args: object, **kwargs: object) -> None:
            if path == first_destination:
                raise PermissionError("injected rollback unlink failure")
            real_unlink(path, *args, **kwargs)

        with (
            mock.patch.object(
                RECEIPT_MODULE,
                "_atomic_raw_write_new",
                side_effect=publish_then_fail,
            ),
            mock.patch.object(Path, "unlink", new=reject_first_rollback),
            mock.patch.object(RECEIPT_MODULE, "sync_directory") as sync,
        ):
            with self.assertRaisesRegex(
                RECEIPT_MODULE.ReceiptError, "rollback was incomplete"
            ):
                RECEIPT_MODULE.retain_export_snapshot_set(
                    stage, output, "release.tar"
                )
        self.assertEqual(first_destination.read_bytes(), self.binary.read_bytes())
        sync.assert_called_once_with(output)

    def test_retain_reporting_survives_closed_result_consumer(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        output = self.root / "retained-closed-consumer"
        output.mkdir()
        read_descriptor, write_descriptor = os.pipe()
        os.close(read_descriptor)
        try:
            retained = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "retain-export-snapshot-set",
                    "--stage",
                    str(stage),
                    "--output-dir",
                    str(output),
                    "--artifact-prefix",
                    "release.tar",
                ],
                text=True,
                stdout=write_descriptor,
                stderr=subprocess.PIPE,
            )
        finally:
            os.close(write_descriptor)
        self.assertEqual(retained.returncode, 0, retained.stderr)
        self.assertEqual(len(list(output.iterdir())), 2)

    def test_retain_rejects_symlinked_output_directory_and_leaf(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        real_output = self.root / "real-retained-output"
        real_output.mkdir()
        linked_output = self.root / "linked-retained-output"
        self.replace_with_symlink(linked_output, real_output, is_directory=True)
        retained = self.run_retain(stage, linked_output)
        self.assertNotEqual(retained.returncode, 0)
        self.assertEqual(list(real_output.iterdir()), [])

        outside = self.root / "outside-retained-leaf"
        outside.write_bytes(b"outside-operator-bytes")
        leaf = real_output / "release.tar.prebuilt-rust.dcentrald.bin"
        self.replace_with_symlink(leaf, outside)
        retained = self.run_retain(stage, real_output)
        self.assertNotEqual(retained.returncode, 0)
        self.assertEqual(outside.read_bytes(), b"outside-operator-bytes")
        self.assertTrue(leaf.is_symlink())

    def test_retain_rolls_back_partial_set_and_preserves_collision(self) -> None:
        second_binary = self.release / "dcentos-init"
        second_binary.write_bytes(b"second-fixture-elf")
        created = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "create",
                *self.context_arguments(),
                "--binary",
                str(self.binary),
                "--binary",
                str(second_binary),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertEqual(created.returncode, 0, created.stderr)
        second_receipt = second_binary.with_name(
            second_binary.name + ".build-receipt.json"
        )
        result = self.run_export(
            [(second_binary, second_receipt), (self.binary, self.receipt)]
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        descriptor = json.loads((stage / "export-set.json").read_text(encoding="utf-8"))
        ordered_names = [
            Path(pair["binary"]["source_path"]).name
            for pair in descriptor["artifacts"]
        ]
        self.assertEqual(ordered_names, ["dcentos-init", "dcentrald"])
        output = self.root / "retained-partial"
        output.mkdir()
        collision = output / "release.tar.prebuilt-rust.dcentrald.bin"
        collision.write_bytes(b"preserve-me")

        retained = self.run_retain(stage, output)
        self.assertNotEqual(retained.returncode, 0)
        self.assertEqual(collision.read_bytes(), b"preserve-me")
        self.assertEqual(sorted(path.name for path in output.iterdir()), [collision.name])

    def test_retain_rejects_tampered_missing_and_extra_stage_entries(self) -> None:
        mutations = ("tampered", "missing", "extra")
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                result = self.run_export()
                self.assertEqual(result.returncode, 0, result.stderr)
                stage = Path(result.stdout.strip())
                _, exported_binary, exported_receipt = self.exported_paths(stage)
                if mutation == "tampered":
                    os.chmod(exported_binary, 0o600)
                    exported_binary.write_bytes(b"tampered")
                elif mutation == "missing":
                    exported_receipt.unlink()
                else:
                    extra = stage / "undeclared"
                    extra.write_bytes(b"extra")
                    os.chmod(extra, 0o400)
                output = self.root / f"retained-{mutation}"
                output.mkdir()
                retained = self.run_retain(stage, output)
                self.assertNotEqual(retained.returncode, 0)
                self.assertEqual(list(output.iterdir()), [])

    def test_verified_path_query_by_name_source_and_artifact(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        descriptor = json.loads(
            (stage / "export-set.json").read_text(encoding="utf-8")
        )
        pair = descriptor["artifacts"][0]
        by_name = self.run_path_query(stage, "--binary-name", "dcentrald")
        self.assertEqual(by_name.returncode, 0, by_name.stderr)
        self.assertEqual(by_name.stderr, "")
        self.assertEqual(by_name.stdout, pair["binary"]["export_path"] + "\n")
        by_source = self.run_path_query(
            stage, "--source-path", pair["binary"]["source_path"]
        )
        self.assertEqual(by_source.returncode, 0, by_source.stderr)
        self.assertEqual(by_source.stdout, pair["binary"]["export_path"] + "\n")
        receipt = self.run_path_query(
            stage,
            "--binary-name",
            "dcentrald",
            "--artifact",
            "receipt",
        )
        self.assertEqual(receipt.returncode, 0, receipt.stderr)
        self.assertEqual(receipt.stdout, pair["receipt"]["export_path"] + "\n")
        digest = self.run_path_query(
            stage,
            "--binary-name",
            "dcentrald",
            "--artifact",
            "binary",
            "--field",
            "sha256",
        )
        self.assertEqual(digest.returncode, 0, digest.stderr)
        self.assertEqual(digest.stdout, pair["binary"]["sha256"] + "\n")
        record = self.run_path_query(
            stage,
            "--binary-name",
            "dcentrald",
            "--artifact",
            "binary",
            "--field",
            "path-sha256",
        )
        self.assertEqual(record.returncode, 0, record.stderr)
        self.assertEqual(
            record.stdout,
            f"{pair['binary']['export_path']} {pair['binary']['sha256']}\n",
        )
        self.assertNotIn("capability", by_name.stdout)

    def test_path_query_rejects_zero_ambiguous_and_unsafe_selectors(self) -> None:
        duplicate_directory = self.release / "duplicate"
        duplicate_directory.mkdir()
        duplicate_binary = duplicate_directory / "dcentrald"
        duplicate_binary.write_bytes(b"duplicate-name-fixture")
        created = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "create",
                *self.context_arguments(),
                "--binary",
                str(self.binary),
                "--binary",
                str(duplicate_binary),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertEqual(created.returncode, 0, created.stderr)
        duplicate_receipt = duplicate_binary.with_name(
            duplicate_binary.name + ".build-receipt.json"
        )
        exported = self.run_export(
            [(self.binary, self.receipt), (duplicate_binary, duplicate_receipt)]
        )
        self.assertEqual(exported.returncode, 0, exported.stderr)
        stage = Path(exported.stdout.strip())

        ambiguous = self.run_path_query(stage, "--binary-name", "dcentrald")
        self.assertNotEqual(ambiguous.returncode, 0)
        self.assertIn("matched 2 pairs", ambiguous.stderr)
        missing = self.run_path_query(stage, "--binary-name", "not-present")
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("matched 0 pairs", missing.stderr)

        unsafe_names = (
            "../dcentrald",
            ".hidden",
            "bad/name",
            "bad\\name",
            "bad\nname",
            "bad\x00name",
        )
        for value in unsafe_names:
            with self.subTest(binary_name=value):
                with self.assertRaises(RECEIPT_MODULE.ReceiptError):
                    RECEIPT_MODULE.query_export_snapshot_path(
                        stage,
                        binary_name=value,
                        source_path=None,
                        artifact="binary",
                    )
        for value in ("../unsafe", "unsafe\npath", "unsafe\x00path", "unsafe\\path"):
            with self.subTest(source_path=value):
                with self.assertRaises(RECEIPT_MODULE.ReceiptError):
                    RECEIPT_MODULE.query_export_snapshot_path(
                        stage,
                        binary_name=None,
                        source_path=value,
                        artifact="binary",
                    )

    def test_path_query_fully_verifies_stage_before_output(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        _, exported_binary, _ = self.exported_paths(stage)
        os.chmod(exported_binary, 0o600)
        exported_binary.write_bytes(b"tampered-before-query")
        queried = self.run_path_query(stage, "--binary-name", "dcentrald")
        self.assertNotEqual(queried.returncode, 0)
        self.assertEqual(queried.stdout, "")

    def test_export_set_canonicalizes_multiple_declared_pairs(self) -> None:
        second_binary = self.release / "dcentos-init"
        second_binary.write_bytes(b"second-fixture-elf")
        created = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "create",
                *self.context_arguments(),
                "--binary",
                str(self.binary),
                "--binary",
                str(second_binary),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertEqual(created.returncode, 0, created.stderr)
        second_receipt = second_binary.with_name(
            second_binary.name + ".build-receipt.json"
        )
        result = self.run_export(
            [(second_binary, second_receipt), (self.binary, self.receipt)]
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        descriptor = json.loads(
            (stage / "export-set.json").read_text(encoding="utf-8")
        )
        source_paths = [
            pair["binary"]["source_path"] for pair in descriptor["artifacts"]
        ]
        self.assertEqual(source_paths, sorted(source_paths))
        verified = self.run_stage_command("verify-export-snapshot-set", stage)
        self.assertEqual(verified.returncode, 0, verified.stderr)

    def test_export_rejects_duplicate_and_missing_pairs(self) -> None:
        duplicate = self.run_export(
            [(self.binary, self.receipt), (self.binary, self.receipt)]
        )
        self.assertNotEqual(duplicate.returncode, 0)
        self.assertIn("duplicate declared export path", duplicate.stderr)

        self.receipt.unlink()
        missing = self.run_export()
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("export receipt is missing", missing.stderr)

    def test_export_rejects_symlinked_receipt_pair(self) -> None:
        target = self.root / "receipt-target"
        self.receipt.replace(target)
        self.replace_with_symlink(self.receipt, target)
        result = self.run_export()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("symlink", result.stderr)

    def test_export_rejects_symlinked_capability_directory(self) -> None:
        capability_directory = self.stage_parent / ".dcent-export-capabilities"
        target = self.root / "outside-capability-directory"
        target.mkdir()
        self.replace_with_symlink(
            capability_directory, target, is_directory=True
        )
        result = self.run_export()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("symlink", result.stderr)

    @unittest.skipUnless(os.name == "posix", "exact capability modes are POSIX-only")
    def test_export_does_not_repair_discovered_capability_directory_mode(self) -> None:
        first = self.run_export()
        self.assertEqual(first.returncode, 0, first.stderr)
        stage = Path(first.stdout.strip())
        capability_directory = self.capability_path(stage).parent
        os.chmod(capability_directory, 0o755)
        second = self.run_export()
        self.assertNotEqual(second.returncode, 0)
        self.assertIn("expected exactly 0700", second.stderr)
        self.assertEqual(stat.S_IMODE(capability_directory.stat().st_mode), 0o755)

    def test_export_swap_after_open_is_rejected_before_staging(self) -> None:
        replacement = self.release / "replacement-binary"
        replacement.write_bytes(b"replacement-generation")

        def replace_binary_after_open(path: Path, _descriptor: int) -> None:
            if path == self.binary:
                os.replace(replacement, self.binary)

        args = RECEIPT_MODULE.parser().parse_args(self.export_arguments())
        previous = RECEIPT_MODULE._AFTER_OPEN_HOOK
        RECEIPT_MODULE._AFTER_OPEN_HOOK = replace_binary_after_open
        try:
            with self.assertRaises(RECEIPT_MODULE.ReceiptError):
                RECEIPT_MODULE.export_snapshot_set(args)
        finally:
            RECEIPT_MODULE._AFTER_OPEN_HOOK = previous
        self.assertEqual(list(self.stage_parent.iterdir()), [])

    def test_export_after_verify_uses_captured_generation(self) -> None:
        original_binary = self.binary.read_bytes()
        original_receipt = self.receipt.read_bytes()
        replacement_binary = self.release / "replacement-binary"
        replacement_receipt = self.release / "replacement-receipt"
        replacement_binary.write_bytes(b"replacement-generation")
        replacement_receipt.write_bytes(b"not-the-captured-receipt")

        def replace_originals_after_verify() -> None:
            os.replace(replacement_binary, self.binary)
            os.replace(replacement_receipt, self.receipt)

        args = RECEIPT_MODULE.parser().parse_args(self.export_arguments())
        previous = RECEIPT_MODULE._AFTER_EXPORT_VERIFY_HOOK
        RECEIPT_MODULE._AFTER_EXPORT_VERIFY_HOOK = replace_originals_after_verify
        try:
            stage = RECEIPT_MODULE.export_snapshot_set(args)
        finally:
            RECEIPT_MODULE._AFTER_EXPORT_VERIFY_HOOK = previous
        _, exported_binary, exported_receipt = self.exported_paths(stage)
        self.assertEqual(exported_binary.read_bytes(), original_binary)
        self.assertEqual(exported_receipt.read_bytes(), original_receipt)
        RECEIPT_MODULE.verify_export_snapshot_set(stage)

    def test_verify_rejects_symlinked_exported_file(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        _, exported_binary, _ = self.exported_paths(stage)
        target = self.root / "exported-binary-target"
        exported_binary.replace(target)
        self.replace_with_symlink(exported_binary, target)
        verified = self.run_stage_command("verify-export-snapshot-set", stage)
        self.assertNotEqual(verified.returncode, 0)
        self.assertIn("symlink", verified.stderr)

    def test_verify_rejects_windows_junction_in_export_stage(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        target = self.root / "junction-export-target"
        target.mkdir()
        (target / "outside.bin").write_bytes(b"outside")
        junction = stage / "junction"
        self.create_windows_junction(junction, target)
        try:
            verified = self.run_stage_command("verify-export-snapshot-set", stage)
            self.assertNotEqual(verified.returncode, 0)
            self.assertIn("reparse point", verified.stderr)
        finally:
            os.rmdir(junction)

    def test_verify_and_destroy_fail_closed_on_tamper(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        _, exported_binary, _ = self.exported_paths(stage)
        os.chmod(exported_binary, 0o600)
        exported_binary.write_bytes(b"tampered")
        verified = self.run_stage_command("verify-export-snapshot-set", stage)
        self.assertNotEqual(verified.returncode, 0)
        destroyed = self.run_stage_command("destroy-export-snapshot-set", stage)
        self.assertNotEqual(destroyed.returncode, 0)
        self.assertTrue(stage.exists())

    def test_verify_rejects_undeclared_stage_entry(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        extra = stage / "undeclared"
        extra.write_bytes(b"not declared")
        os.chmod(extra, 0o400)
        verified = self.run_stage_command("verify-export-snapshot-set", stage)
        self.assertNotEqual(verified.returncode, 0)
        self.assertIn("does not exactly match", verified.stderr)
        destroyed = self.run_stage_command("destroy-export-snapshot-set", stage)
        self.assertNotEqual(destroyed.returncode, 0)
        self.assertTrue(stage.exists())

    def test_outside_hardlink_quarantines_stage_without_chmod(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        _, exported_binary, _ = self.exported_paths(stage)
        outside = self.root / "outside-hardlink"
        try:
            os.link(exported_binary, outside)
        except (NotImplementedError, OSError) as error:
            self.skipTest(f"hardlink creation is unavailable: {error}")
        before_mode = stat.S_IMODE(outside.stat().st_mode)
        before_bytes = outside.read_bytes()
        verified = self.run_stage_command("verify-export-snapshot-set", stage)
        self.assertNotEqual(verified.returncode, 0)
        self.assertIn("hard links", verified.stderr)
        destroyed = self.run_stage_command("destroy-export-snapshot-set", stage)
        self.assertNotEqual(destroyed.returncode, 0)
        self.assertTrue(stage.exists())
        self.assertEqual(outside.read_bytes(), before_bytes)
        self.assertEqual(stat.S_IMODE(outside.stat().st_mode), before_mode)

    def test_hardlinked_capability_cannot_authorize_destruction(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        capability = self.capability_path(stage)
        outside = self.root / "outside-capability-hardlink"
        try:
            os.link(capability, outside)
        except (NotImplementedError, OSError) as error:
            self.skipTest(f"hardlink creation is unavailable: {error}")
        destroyed = self.run_stage_command("destroy-export-snapshot-set", stage)
        self.assertNotEqual(destroyed.returncode, 0)
        self.assertIn("hard links", destroyed.stderr)
        self.assertTrue(stage.exists())
        self.assertTrue(capability.exists())

    def test_forged_stage_cannot_reuse_another_stage_capability(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        forged = stage.parent / "forged-export-stage"
        shutil.copytree(stage, forged, copy_function=shutil.copy2)
        original_capability = self.capability_path(stage)
        destroyed = self.run_stage_command(
            "destroy-export-snapshot-set",
            forged,
            capability=original_capability,
        )
        self.assertNotEqual(destroyed.returncode, 0)
        self.assertIn("not the out-of-stage capability bound", destroyed.stderr)
        self.assertTrue(forged.exists())
        self.assertTrue(stage.exists())

    def test_destroy_removes_only_a_verified_export_set(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        capability = self.capability_path(stage)
        destroyed = self.run_stage_command("destroy-export-snapshot-set", stage)
        self.assertEqual(destroyed.returncode, 0, destroyed.stderr)
        self.assertEqual(destroyed.stdout, "")
        self.assertFalse(stage.exists())
        self.assertFalse(capability.exists())

    def test_destroy_cli_requires_out_of_stage_capability(self) -> None:
        result = self.run_export()
        self.assertEqual(result.returncode, 0, result.stderr)
        stage = Path(result.stdout.strip())
        destroyed = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "destroy-export-snapshot-set",
                "--stage",
                str(stage),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertNotEqual(destroyed.returncode, 0)
        self.assertIn("--capability", destroyed.stderr)
        self.assertTrue(stage.exists())
        self.assertTrue(self.capability_path(stage).exists())


class OverridePolicyTests(unittest.TestCase):
    def policy(self, **values: str) -> subprocess.CompletedProcess[str]:
        command = [sys.executable, str(SCRIPT), "check-override-policy"]
        for key, value in values.items():
            command.extend(["--" + key.replace("_", "-"), value])
        return subprocess.run(
            command,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def test_release_provenance_with_override_fails(self) -> None:
        result = self.policy(allow_stale="1", release_provenance="1", package_status="lab")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("forbidden in release", result.stderr)

    def test_release_status_and_release_image_with_override_fail(self) -> None:
        for values in (
            {"allow_stale": "1", "package_status": "release"},
            {"allow_stale": "1", "package_status": "lab", "release_image": "1"},
        ):
            with self.subTest(values=values):
                result = self.policy(**values)
                self.assertNotEqual(result.returncode, 0)

    def test_non_release_lab_override_warns_without_bypass(self) -> None:
        result = self.policy(allow_stale="1", package_status="lab_signed")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("deprecated compatibility signal", result.stderr)
        self.assertIn("does not bypass snapshot/export validation", result.stderr)
        self.assertIn("or authorize release claims", result.stderr)
        self.assertIn("remove it from callers", result.stderr)
        self.assertNotIn("permits receipt bypass", result.stderr)


if __name__ == "__main__":
    unittest.main()
