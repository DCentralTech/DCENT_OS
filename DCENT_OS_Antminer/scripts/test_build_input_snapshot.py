#!/usr/bin/env python3
"""Adversarial stdlib tests for external build-input snapshots."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
TOOL = ROOT / "scripts/build_input_snapshot.py"
SOURCE_CLOSURE_TOOL = ROOT / "scripts/source_closure.py"
S9_KERNEL_PATH = ""
S9_DTB_PATH = ""


def load_module(name: str, path: pathlib.Path):
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SNAPSHOT = load_module("dcentos_build_input_snapshot_test", TOOL)


def digest(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def force_remove(path: pathlib.Path) -> None:
    if not path.exists():
        return
    for directory, _dirnames, filenames in os.walk(path, topdown=False, followlinks=False):
        current = pathlib.Path(directory)
        for filename in filenames:
            child = current / filename
            try:
                os.chmod(child, 0o600)
                child.unlink()
            except FileNotFoundError:
                pass
        os.chmod(current, 0o700)
        for dirname in list(current.iterdir()):
            if dirname.is_dir() and not dirname.is_symlink():
                try:
                    dirname.rmdir()
                except OSError:
                    pass
    shutil.rmtree(path, ignore_errors=True)


class SnapshotTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="dcent-snapshot-test-")
        self.base = pathlib.Path(self.temporary.name)
        self.repo = self.base / "repo"
        self.repo.mkdir()
        self.selection_root = self.base / "selection-root"
        self.selection_root.mkdir()
        self.created = []

    def tearDown(self) -> None:
        for created in self.created:
            force_remove(created.stage)
        self.temporary.cleanup()

    def write_fixture(self, entries: dict[str, bytes]) -> pathlib.Path:
        manifest = self.repo / "scripts/build_inputs.manifest"
        manifest.parent.mkdir(parents=True, exist_ok=True)
        lines = ["# test manifest"]
        for relative, value in entries.items():
            path = self.repo.joinpath(*pathlib.PurePosixPath(relative).parts)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(value)
            lines.append(f"{digest(value)}  {relative}")
        manifest.write_text("\n".join(lines) + "\n", encoding="ascii")
        return manifest

    def write_split_fixture(self, entries: dict[str, bytes]) -> pathlib.Path:
        lines = ["# separately owned selection manifest"]
        for relative, value in entries.items():
            path = self.repo.joinpath(*pathlib.PurePosixPath(relative).parts)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(value)
            lines.append(f"{digest(value)}  {relative}")
        manifest = self.selection_root / "scripts/build_inputs.manifest"
        manifest.parent.mkdir(parents=True, exist_ok=True)
        manifest.write_text("\n".join(lines) + "\n", encoding="ascii")
        return manifest

    @staticmethod
    def s9_entries(value: bytes = b"captured S9 kernel bytes\n") -> dict[str, bytes]:
        return {
            S9_KERNEL_PATH: value,
            S9_DTB_PATH: b"captured S9 device-tree bytes\n",
        }

    @classmethod
    def supported_entries(cls) -> dict[str, bytes]:
        entries = cls.s9_entries()
        entries.update(
            {
                "": b"am2 kernel\n",
                "": b"am2 bitstream\n",
            }
        )
        return entries

    def create(self, manifest: pathlib.Path, target: str = "s9", **kwargs):
        created = SNAPSHOT.create_snapshot(
            self.repo, manifest, target, stage_parent=self.base, **kwargs
        )
        self.created.append(created)
        return created

    def destroy(self, created) -> None:
        SNAPSHOT.destroy_snapshot(created.snapshot, created.destroy_token)
        self.created.remove(created)

    def rewrite_descriptor(self, created, mutate) -> None:
        os.chmod(created.stage, 0o700)
        os.chmod(created.snapshot, 0o600)
        descriptor = json.loads(created.snapshot.read_text(encoding="ascii"))
        mutate(descriptor)
        body = dict(descriptor)
        body.pop("snapshot_id", None)
        descriptor["snapshot_id"] = digest(SNAPSHOT.canonical_bytes(body))
        created.snapshot.write_bytes(SNAPSHOT.canonical_bytes(descriptor))
        os.chmod(created.snapshot, 0o400)
        os.chmod(created.stage, 0o500)

    def test_private_canonical_descriptor_is_path_independent(self) -> None:
        manifest = self.write_fixture(self.s9_entries())
        first = self.create(manifest)
        second = self.create(manifest)
        self.assertEqual(first.snapshot.read_bytes(), second.snapshot.read_bytes())
        self.assertNotEqual(first.destroy_token, second.destroy_token)
        descriptor = SNAPSHOT.verify_snapshot(first.snapshot, "s9")
        self.assertEqual(descriptor["schema"], SNAPSHOT.SCHEMA)
        self.assertNotIn("selection_root", descriptor)
        self.assertEqual(descriptor["scope"]["claim"], SNAPSHOT.SNAPSHOT_CLAIM)
        self.assertNotIn(str(first.stage), first.snapshot.read_text(encoding="ascii"))
        evidence = SNAPSHOT.snapshot_evidence(descriptor)
        self.assertEqual(evidence["snapshot"]["snapshot_id"], first.snapshot_id)
        self.assertEqual(evidence["snapshot"]["target"], "s9")
        self.assertEqual(evidence["snapshot"]["claim"], SNAPSHOT.SNAPSHOT_CLAIM)

    def test_manifest_and_input_swaps_use_opened_handles(self) -> None:
        original = b"original manifest-pinned bytes\n"
        manifest = self.write_fixture(self.s9_entries(original))
        input_path = self.repo.joinpath(*pathlib.PurePosixPath(S9_KERNEL_PATH).parts)
        replacement_manifest = self.base / "replacement.manifest"
        replacement_manifest.write_text(
            f"{'0' * 64}  {S9_KERNEL_PATH}\n", encoding="ascii"
        )
        replacement_input = self.base / "replacement.bin"
        replacement_input.write_bytes(b"replacement bytes\n")

        def swap_manifest(_relative, _stream):
            try:
                os.replace(replacement_manifest, manifest)
            except PermissionError:
                pass

        if os.name == "nt":
            created = self.create(manifest, after_manifest_open=swap_manifest)
            self.assertEqual(
                created.stage.joinpath(
                    *pathlib.PurePosixPath(created.files[0]["staged_path"]).parts
                ).read_bytes(),
                original,
            )
            self.destroy(created)
        else:
            with self.assertRaisesRegex(
                SNAPSHOT.SnapshotError, "changed while|exactly one filesystem link"
            ):
                self.create(manifest, after_manifest_open=swap_manifest)

        manifest = self.write_fixture(self.s9_entries(original))

        def swap_input(relative, _stream):
            if relative != S9_KERNEL_PATH:
                return
            try:
                os.replace(replacement_input, input_path)
            except PermissionError:
                pass

        if os.name == "nt":
            created = self.create(manifest, after_input_open=swap_input)
            self.assertEqual(
                created.stage.joinpath(
                    *pathlib.PurePosixPath(created.files[0]["staged_path"]).parts
                ).read_bytes(),
                original,
            )
        else:
            with self.assertRaisesRegex(
                SNAPSHOT.SnapshotError, "changed while|exactly one filesystem link"
            ):
                self.create(manifest, after_input_open=swap_input)

    def test_in_place_manifest_mutation_is_blocked_or_rejected(self) -> None:
        manifest = self.write_split_fixture(self.s9_entries())
        blocked = []

        def mutate(_relative, _stream):
            try:
                manifest.write_text("mutated selection authority\n", encoding="ascii")
            except PermissionError:
                blocked.append(True)

        if os.name == "nt":
            created = self.create(
                manifest,
                selection_root=self.selection_root,
                after_manifest_open=mutate,
            )
            self.assertTrue(blocked)
            SNAPSHOT.verify_snapshot(created.snapshot)
        else:
            with self.assertRaisesRegex(
                SNAPSHOT.SnapshotError, "changed while|contains no entries"
            ):
                self.create(
                    manifest,
                    selection_root=self.selection_root,
                    after_manifest_open=mutate,
                )

    def test_split_authority_descriptor_binds_logical_manifest_not_host_paths(self) -> None:
        original = b"payload authority remains repository root\n"
        manifest = self.write_split_fixture(self.s9_entries(original))
        misleading = self.selection_root.joinpath(
            *pathlib.PurePosixPath(S9_KERNEL_PATH).parts
        )
        misleading.parent.mkdir(parents=True, exist_ok=True)
        misleading.write_bytes(b"must never be selected from selection root\n")

        created = self.create(manifest, selection_root=self.selection_root)
        descriptor = SNAPSHOT.verify_snapshot(created.snapshot, "s9")
        self.assertEqual(descriptor["schema"], SNAPSHOT.SPLIT_AUTHORITY_SCHEMA)
        self.assertEqual(
            descriptor["selection_root"], {"kind": SNAPSHOT.SELECTION_ROOT_KIND}
        )
        self.assertEqual(descriptor["manifest"]["path"], "scripts/build_inputs.manifest")
        self.assertEqual(descriptor["manifest"]["sha256"], digest(manifest.read_bytes()))
        self.assertEqual(descriptor["manifest"]["size"], len(manifest.read_bytes()))
        descriptor_text = created.snapshot.read_text(encoding="ascii")
        self.assertNotIn(str(self.repo), descriptor_text)
        self.assertNotIn(str(self.selection_root), descriptor_text)
        staged = created.stage.joinpath(
            *pathlib.PurePosixPath(created.files[0]["staged_path"]).parts
        )
        self.assertEqual(staged.read_bytes(), original)
        second_root = self.base / "other-selection-root"
        second_manifest = second_root / "scripts/build_inputs.manifest"
        second_manifest.parent.mkdir(parents=True)
        second_manifest.write_bytes(manifest.read_bytes())
        second = self.create(second_manifest, selection_root=second_root)
        self.assertEqual(created.snapshot.read_bytes(), second.snapshot.read_bytes())

    def test_split_authority_rejects_alias_and_outside_manifest(self) -> None:
        manifest = self.write_fixture(self.s9_entries())
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "must not alias"):
            self.create(manifest, selection_root=self.repo)
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "selection root"):
            self.create(manifest, selection_root=self.selection_root)

    def test_live_selection_manifest_is_not_reopened_after_snapshot(self) -> None:
        manifest = self.write_split_fixture(self.s9_entries())
        created = self.create(manifest, selection_root=self.selection_root)
        staged_digest = SNAPSHOT.verify_snapshot(created.snapshot)["manifest"]["sha256"]
        manifest.write_text("later live-tree mutation\n", encoding="ascii")
        descriptor = SNAPSHOT.verify_snapshot(created.snapshot)
        self.assertEqual(descriptor["manifest"]["sha256"], staged_digest)

    def test_manifest_and_payload_hardlinks_are_rejected(self) -> None:
        manifest = self.write_split_fixture(self.s9_entries())
        manifest_link = manifest.with_name("manifest-hardlink")
        try:
            os.link(manifest, manifest_link)
        except OSError as error:
            self.skipTest(f"hardlinks unavailable: {error}")
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "exactly one filesystem link"):
            self.create(manifest, selection_root=self.selection_root)
        manifest_link.unlink()

        payload = self.repo.joinpath(*pathlib.PurePosixPath(S9_KERNEL_PATH).parts)
        os.link(payload, payload.with_name("payload-hardlink"))
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "exactly one filesystem link"):
            self.create(manifest, selection_root=self.selection_root)

    @unittest.skipUnless(hasattr(os, "symlink"), "symlink API unavailable")
    def test_symlinked_selection_manifest_or_root_is_rejected(self) -> None:
        manifest = self.write_split_fixture(self.s9_entries())
        real_manifest = manifest.with_name("real.manifest")
        manifest.replace(real_manifest)
        try:
            manifest.symlink_to(real_manifest.name)
        except OSError as error:
            self.skipTest(f"host cannot create symlinks: {error}")
        with self.assertRaises((SNAPSHOT.SnapshotError, OSError)):
            self.create(manifest, selection_root=self.selection_root)

        root_alias = self.base / "selection-root-alias"
        try:
            root_alias.symlink_to(self.selection_root, target_is_directory=True)
        except OSError as error:
            self.skipTest(f"host cannot create directory symlinks: {error}")
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "must be a non-symlink directory"):
            self.create(real_manifest, selection_root=root_alias)

    @unittest.skipUnless(os.name == "posix", "FIFO test is POSIX-only")
    def test_special_manifest_is_rejected_without_blocking(self) -> None:
        special = self.selection_root / "special.manifest"
        os.mkfifo(special)
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "regular file"):
            self.create(special, selection_root=self.selection_root)

    @unittest.skipUnless(os.name == "posix", "FIFO test is POSIX-only")
    def test_special_payload_is_rejected_without_blocking(self) -> None:
        manifest = self.write_split_fixture(self.s9_entries())
        payload = self.repo.joinpath(*pathlib.PurePosixPath(S9_KERNEL_PATH).parts)
        payload.unlink()
        os.mkfifo(payload)
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "regular file"):
            self.create(manifest, selection_root=self.selection_root)

    @unittest.skipUnless(os.name == "nt", "junction test is Windows-only")
    def test_windows_selection_root_and_manifest_junctions_are_rejected(self) -> None:
        manifest = self.write_split_fixture(self.s9_entries())
        root_alias = self.base / "selection-root-junction"
        made_root = subprocess.run(
            ["cmd", "/c", "mklink", "/J", os.fspath(root_alias), os.fspath(self.selection_root)],
            check=False,
            capture_output=True,
            text=True,
        )
        if made_root.returncode != 0:
            self.skipTest(f"cannot create Windows junction: {made_root.stderr}")
        try:
            with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "non-symlink directory"):
                self.create(manifest, selection_root=root_alias)
        finally:
            os.rmdir(root_alias)

        real_directory = self.selection_root / "real-manifest-directory"
        real_directory.mkdir()
        real_manifest = real_directory / "build_inputs.manifest"
        manifest.replace(real_manifest)
        manifest.parent.rmdir()
        made_component = subprocess.run(
            ["cmd", "/c", "mklink", "/J", os.fspath(manifest.parent), os.fspath(real_directory)],
            check=False,
            capture_output=True,
            text=True,
        )
        if made_component.returncode != 0:
            self.skipTest(f"cannot create Windows junction: {made_component.stderr}")
        try:
            with self.assertRaisesRegex(
                SNAPSHOT.SnapshotError, "symlink or reparse-point components"
            ):
                self.create(manifest, selection_root=self.selection_root)
        finally:
            os.rmdir(manifest.parent)

    def test_in_place_mutation_is_blocked_or_fails_digest(self) -> None:
        original = b"original bytes\n"
        manifest = self.write_fixture(self.s9_entries(original))
        input_path = self.repo.joinpath(*pathlib.PurePosixPath(S9_KERNEL_PATH).parts)
        blocked = []

        def mutate(_relative, _stream):
            try:
                input_path.write_bytes(b"mutated bytes\n")
            except PermissionError:
                blocked.append(True)

        if os.name == "nt":
            created = self.create(manifest, after_input_open=mutate)
            self.assertTrue(blocked)
            SNAPSHOT.verify_snapshot(created.snapshot)
        else:
            with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "SHA256 mismatch"):
                SNAPSHOT.create_snapshot(
                    self.repo, manifest, "s9", self.base, after_input_open=mutate
                )

    @unittest.skipUnless(hasattr(os, "symlink"), "symlink API unavailable")
    def test_symlinked_source_input_is_rejected(self) -> None:
        manifest = self.write_fixture(self.s9_entries())
        input_path = self.repo.joinpath(*pathlib.PurePosixPath(S9_KERNEL_PATH).parts)
        real = input_path.with_name("real.bin")
        input_path.replace(real)
        try:
            input_path.symlink_to(real.name)
        except OSError as error:
            self.skipTest(f"host cannot create symlinks: {error}")
        with self.assertRaises((SNAPSHOT.SnapshotError, OSError)):
            SNAPSHOT.create_snapshot(self.repo, manifest, "s9", self.base)

    def test_destroy_requires_capability_and_exact_tree(self) -> None:
        manifest = self.write_fixture(self.s9_entries())
        created = self.create(manifest)
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "does not authorize"):
            SNAPSHOT.destroy_snapshot(created.snapshot, "0" * 64)
        extra = created.stage / "unlisted-extra"
        os.chmod(created.stage, 0o700)
        extra.write_bytes(b"must survive refused cleanup")
        os.chmod(created.stage, 0o500)
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "on-disk tree is not exact"):
            SNAPSHOT.destroy_snapshot(created.snapshot, created.destroy_token)
        self.assertTrue(created.stage.exists())
        self.assertTrue(extra.exists())

    def test_unlisted_hardlink_is_rejected_without_chmod_target(self) -> None:
        manifest = self.write_fixture(self.s9_entries())
        created = self.create(manifest)
        victim = self.base / "external-victim"
        victim.write_bytes(b"keep")
        os.chmod(victim, 0o444)
        before = stat.S_IMODE(victim.stat().st_mode)
        os.chmod(created.stage, 0o700)
        try:
            os.link(victim, created.stage / "unlisted-hardlink")
        except OSError as error:
            self.skipTest(f"hardlinks unavailable: {error}")
        os.chmod(created.stage, 0o500)
        with self.assertRaises(SNAPSHOT.SnapshotError):
            SNAPSHOT.destroy_snapshot(created.snapshot, created.destroy_token)
        self.assertTrue(victim.exists())
        self.assertEqual(stat.S_IMODE(victim.stat().st_mode), before)

    @unittest.skipUnless(hasattr(os, "symlink"), "symlink API unavailable")
    def test_unlisted_symlink_is_rejected_without_touching_target(self) -> None:
        manifest = self.write_fixture(self.s9_entries())
        created = self.create(manifest)
        victim = self.base / "external-target"
        victim.write_bytes(b"keep")
        before = stat.S_IMODE(victim.stat().st_mode)
        os.chmod(created.stage, 0o700)
        link = created.stage / "unlisted-symlink"
        try:
            link.symlink_to(victim)
        except OSError as error:
            self.skipTest(f"host cannot create symlinks: {error}")
        os.chmod(created.stage, 0o500)
        with self.assertRaises(SNAPSHOT.SnapshotError):
            SNAPSHOT.destroy_snapshot(created.snapshot, created.destroy_token)
        self.assertTrue(victim.exists())
        self.assertEqual(stat.S_IMODE(victim.stat().st_mode), before)

    @unittest.skipUnless(os.name == "posix", "dirfd no-follow cleanup is POSIX-only")
    def test_partial_cleanup_rejects_ancestor_symlink_without_touching_target(self) -> None:
        stage = self.base / "dcentos-build-inputs-partial"
        stage.mkdir(mode=0o700)
        outside = self.base / "outside-partial-cleanup"
        outside.mkdir()
        victim = outside / "victim"
        victim.write_bytes(b"operator-owned bytes")
        before_mode = stat.S_IMODE(victim.stat().st_mode)
        (stage / "files").symlink_to(outside, target_is_directory=True)

        SNAPSHOT._remove_partial_known_stage(stage, [stage / "files" / "victim"])

        self.assertEqual(victim.read_bytes(), b"operator-owned bytes")
        self.assertEqual(stat.S_IMODE(victim.stat().st_mode), before_mode)
        self.assertTrue(stage.exists(), "tampered partial stage must leak safely")

    def test_forged_claim_extra_keys_and_invalid_sizes_are_rejected(self) -> None:
        manifest = self.write_fixture(self.s9_entries())
        mutations = (
            lambda value: value["scope"].__setitem__("claim", "consumer_execution_proven"),
            lambda value: value.__setitem__("unvalidated_extra", True),
            lambda value: value["files"][0].__setitem__("size", True),
            lambda value: value["files"][0].__setitem__("size", -1),
            lambda value: value["files"][0].__setitem__("staged_path", value["manifest"]["staged_path"]),
        )
        for mutation in mutations:
            created = self.create(manifest)
            self.rewrite_descriptor(created, mutation)
            with self.assertRaises(SNAPSHOT.SnapshotError):
                SNAPSHOT.verify_snapshot(created.snapshot)

        split_manifest = self.write_split_fixture(self.s9_entries())
        created = self.create(split_manifest, selection_root=self.selection_root)
        self.rewrite_descriptor(
            created,
            lambda value: value["selection_root"].__setitem__("kind", "live-tree"),
        )
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "root kind"):
            SNAPSHOT.verify_snapshot(created.snapshot)

    def test_source_closure_retains_snapshot_identity_and_not_live_reopen(self) -> None:
        original = b"receipt-bound staged bytes\n"
        manifest = self.write_fixture(self.s9_entries(original))
        created = self.create(manifest)
        input_path = self.repo.joinpath(*pathlib.PurePosixPath(S9_KERNEL_PATH).parts)
        input_path.write_bytes(b"later live-tree replacement\n")
        source_closure = load_module("dcentos_source_closure_snapshot_test", SOURCE_CLOSURE_TOOL)
        evidence = source_closure.build_input_snapshot_evidence(
            str(created.snapshot), "s9"
        )
        self.assertEqual(evidence["files"][0]["sha256"], digest(original))
        self.assertEqual(evidence["snapshot"]["snapshot_id"], created.snapshot_id)
        self.assertEqual(evidence["snapshot"]["target"], "s9")
        self.assertEqual(evidence["snapshot"]["claim"], SNAPSHOT.SNAPSHOT_CLAIM)

    def test_source_closure_accepts_split_authority_snapshot_evidence(self) -> None:
        manifest = self.write_split_fixture(self.s9_entries())
        created = self.create(manifest, selection_root=self.selection_root)
        source_closure = load_module(
            "dcentos_source_closure_split_snapshot_test", SOURCE_CLOSURE_TOOL
        )
        evidence = source_closure.build_input_snapshot_evidence(
            str(created.snapshot), "s9"
        )
        self.assertEqual(
            set(evidence), {"manifest", "selection_policy", "files", "snapshot"}
        )
        self.assertEqual(evidence["manifest"]["path"], "scripts/build_inputs.manifest")
        self.assertEqual(evidence["snapshot"]["snapshot_id"], created.snapshot_id)

    def test_supported_lane_selection_is_exact(self) -> None:
        manifest = self.write_fixture(self.supported_entries())
        expected = {
            "cargo-workspace": set(),
            "s9": {"", ""},
            "am2-s19jpro": {"", ""},
            "am2-s19jpro-sd": {"", ""},
            "am2-s19pro": {"", ""},
        }
        for target, paths in expected.items():
            created = self.create(manifest, target)
            descriptor = SNAPSHOT.verify_snapshot(created.snapshot, target)
            self.assertEqual({item["path"] for item in descriptor["files"]}, paths)

    def test_cargo_workspace_selection_is_explicitly_empty(self) -> None:
        manifest = self.write_split_fixture(self.supported_entries())
        created = self.create(
            manifest, "cargo-workspace", selection_root=self.selection_root
        )
        descriptor = SNAPSHOT.verify_snapshot(created.snapshot, "cargo-workspace")
        self.assertEqual(descriptor["files"], [])
        self.assertEqual(descriptor["target"], "cargo-workspace")
        result = created.cli_result()
        raw = SNAPSHOT.canonical_bytes(result)
        self.assertEqual(
            SNAPSHOT.query_cli_result(raw, "stage", None, None),
            os.fspath(created.stage),
        )
        with self.assertRaisesRegex(
            SNAPSHOT.SnapshotError, "does not contain exactly one file"
        ):
            SNAPSHOT.query_cli_result(
                raw, None, S9_KERNEL_PATH, "sha256"
            )

    def test_cli_create_result_is_canonical_json_and_token_is_not_staged(self) -> None:
        manifest = self.write_fixture(self.s9_entries())
        stage_parent = self.base / "stage parent, with spaces"
        stage_parent.mkdir()
        process = subprocess.run(
            [
                sys.executable, os.fspath(TOOL), "create", "--repo-root", os.fspath(self.repo),
                "--build-input-manifest", os.fspath(manifest), "--target", "s9",
                "--stage-parent", os.fspath(stage_parent),
            ],
            check=True, capture_output=True, text=True,
        )
        result = json.loads(process.stdout)
        self.assertEqual(process.stdout.encode("ascii"), SNAPSHOT.canonical_bytes(result))
        self.assertNotIn(result["destroy_token"], pathlib.Path(result["snapshot"]).read_text(encoding="ascii"))
        queried_stage = subprocess.run(
            [sys.executable, os.fspath(TOOL), "query-result", "--field", "stage"],
            input=process.stdout, check=True, capture_output=True, text=True,
        ).stdout.rstrip("\n")
        self.assertEqual(queried_stage, result["stage"])
        subprocess.run(
            [sys.executable, os.fspath(TOOL), "destroy", "--token", result["destroy_token"], result["snapshot"]],
            check=True,
        )

    def test_cli_split_authority_verify_query_and_destroy(self) -> None:
        manifest = self.write_split_fixture(self.s9_entries())
        process = subprocess.run(
            [
                sys.executable,
                os.fspath(TOOL),
                "create",
                "--repo-root",
                os.fspath(self.repo),
                "--selection-root",
                os.fspath(self.selection_root),
                "--build-input-manifest",
                os.fspath(manifest),
                "--target",
                "s9",
                "--stage-parent",
                os.fspath(self.base),
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        result = json.loads(process.stdout)
        snapshot = pathlib.Path(result["snapshot"])
        descriptor = json.loads(snapshot.read_text(encoding="ascii"))
        self.assertEqual(descriptor["schema"], SNAPSHOT.SPLIT_AUTHORITY_SCHEMA)
        verified = subprocess.run(
            [
                sys.executable,
                os.fspath(TOOL),
                "verify",
                "--target",
                "s9",
                os.fspath(snapshot),
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertIn(result["snapshot_id"], verified.stdout)
        queried = subprocess.run(
            [
                sys.executable,
                os.fspath(TOOL),
                "query-result",
                "--file",
                S9_KERNEL_PATH,
                "--attribute",
                "sha256",
            ],
            input=process.stdout,
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            queried.stdout.rstrip("\n"), digest(self.s9_entries()[S9_KERNEL_PATH])
        )
        subprocess.run(
            [
                sys.executable,
                os.fspath(TOOL),
                "destroy",
                "--token",
                result["destroy_token"],
                os.fspath(snapshot),
            ],
            check=True,
        )
        self.assertFalse(pathlib.Path(result["stage"]).exists())

    def test_query_snapshot_fully_verifies_then_returns_only_safe_scalars(self) -> None:
        payload = self.s9_entries()
        manifest = self.write_split_fixture(payload)
        created = self.create(manifest, selection_root=self.selection_root)
        expected = {
            ("--field", "stage"): os.fspath(created.stage),
            ("--field", "snapshot_id"): created.snapshot_id,
            ("--field", "manifest_path"): "scripts/build_inputs.manifest",
            ("--file", S9_KERNEL_PATH, "--attribute", "sha256"): digest(
                payload[S9_KERNEL_PATH]
            ),
            ("--file", S9_KERNEL_PATH, "--attribute", "size"): str(
                len(payload[S9_KERNEL_PATH])
            ),
            (
                "--file",
                S9_KERNEL_PATH,
                "--attribute",
                "staged_path",
            ): f"files/{S9_KERNEL_PATH}",
        }
        for arguments, expected_value in expected.items():
            queried = subprocess.run(
                [
                    sys.executable,
                    os.fspath(TOOL),
                    "query-snapshot",
                    "--target",
                    "s9",
                    *arguments,
                    os.fspath(created.snapshot),
                ],
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertEqual(queried.stdout.rstrip("\n"), expected_value)

        missing_attribute = subprocess.run(
            [
                sys.executable,
                os.fspath(TOOL),
                "query-snapshot",
                "--target",
                "s9",
                "--file",
                S9_KERNEL_PATH,
                os.fspath(created.snapshot),
            ],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(missing_attribute.returncode, 0)

        descriptor = SNAPSHOT.verify_snapshot(created.snapshot, "s9")
        staged = created.stage.joinpath(
            *pathlib.PurePosixPath(descriptor["files"][0]["staged_path"]).parts
        )
        os.chmod(staged, 0o600)
        staged.write_bytes(b"changed after outer allocation\n")
        rejected = subprocess.run(
            [
                sys.executable,
                os.fspath(TOOL),
                "query-snapshot",
                "--target",
                "s9",
                "--field",
                "stage",
                os.fspath(created.snapshot),
            ],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertEqual(rejected.stdout, "")
        self.assertIn("bytes changed", rejected.stderr)

    def test_query_snapshot_rejects_legacy_single_root_snapshot(self) -> None:
        manifest = self.write_fixture(self.s9_entries())
        created = self.create(manifest)
        rejected = subprocess.run(
            [
                sys.executable,
                os.fspath(TOOL),
                "query-snapshot",
                "--target",
                "s9",
                "--field",
                "stage",
                os.fspath(created.snapshot),
            ],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertEqual(rejected.stdout, "")
        self.assertIn("split-authority v2", rejected.stderr)

    def test_unknown_target_is_fail_closed(self) -> None:
        manifest = self.write_fixture(self.s9_entries())
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "no explicit release build-input policy"):
            SNAPSHOT.create_snapshot(self.repo, manifest, "future-miner", self.base)


class IntegrationStaticTests(unittest.TestCase):
    def test_build_drivers_route_snapshots_without_legacy_cargo_input(self) -> None:
        cargo_driver = (ROOT / "scripts/build-dcentrald.sh").read_text(encoding="utf-8")
        image_driver = (ROOT / "scripts/build_in_docker.sh").read_text(encoding="utf-8")
        for driver in (cargo_driver, image_driver):
            self.assertIn('build_input_snapshot.py" create', driver)
            self.assertIn('build_input_snapshot.py" destroy', driver)
            self.assertIn("BUILD_INPUT_DESTROY_TOKEN", driver)
        self.assertNotIn("DCENT_STOCK_FPGA_", cargo_driver)
        self.assertNotIn("STAGED_STOCK_FPGA", cargo_driver)
        self.assertNotIn("stock_fpga_s9.bin", cargo_driver)
        self.assertIn('"${DOCKER_BUILD_INPUT_STAGE}:/dcent-inputs:ro"', image_driver)
        self.assertNotIn("type=bind,source=${DOCKER_BUILD_INPUT_STAGE}", image_driver)
        self.assertIn('--build-input-snapshot "$BUILD_INPUT_SNAPSHOT"', image_driver)

    def test_pic_recovery_has_no_compile_time_vendor_input(self) -> None:
        build_script = ROOT / "dcentrald/pic-recovery/build.rs"
        self.assertFalse(build_script.exists())
        cargo_manifest = (ROOT / "dcentrald/pic-recovery/Cargo.toml").read_text(
            encoding="utf-8"
        )
        self.assertNotIn("[build-dependencies]", cargo_manifest)
        for relative in ("src/main.rs", "src/dspic_flash_main.rs"):
            source = (ROOT / "dcentrald/pic-recovery" / relative).read_text(
                encoding="utf-8"
            )
            self.assertNotIn("include_bytes!", source)
            self.assertNotIn("stock_fpga_", source)


if __name__ == "__main__":
    unittest.main()
