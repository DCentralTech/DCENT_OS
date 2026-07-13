#!/usr/bin/env python3
"""Adversarial stdlib tests for immutable Git source snapshots."""

from __future__ import annotations

import importlib.util
import json
import os
import pathlib
import stat
import subprocess
import sys
import tempfile
import unittest
from typing import Optional


ROOT = pathlib.Path(__file__).resolve().parents[1]
TOOL = ROOT / "scripts/source_snapshot.py"
SPEC = importlib.util.spec_from_file_location("dcentos_source_snapshot_test", TOOL)
assert SPEC and SPEC.loader
SNAPSHOT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = SNAPSHOT
SPEC.loader.exec_module(SNAPSHOT)


def run_git(repo: pathlib.Path, *args: str, input_bytes: Optional[bytes] = None) -> bytes:
    completed = subprocess.run(("git", "-C", os.fspath(repo), *args), input=input_bytes, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if completed.returncode:
        raise AssertionError(completed.stderr.decode("utf-8", "replace"))
    return completed.stdout


class SourceSnapshotTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="dcent-source-snapshot-test-")
        self.base = pathlib.Path(self.temporary.name)
        self.repo = self.base / "repo"; self.repo.mkdir()
        run_git(self.repo, "init", "-q")
        run_git(self.repo, "config", "user.name", "Snapshot Test")
        run_git(self.repo, "config", "user.email", "snapshot@example.invalid")
        (self.repo / "plain.txt").write_bytes(b"committed\n")
        scripts = self.repo / "scripts"; scripts.mkdir()
        executable = scripts / "run.sh"; executable.write_bytes(b"#!/bin/sh\nexit 0\n")
        run_git(self.repo, "add", ".")
        run_git(self.repo, "update-index", "--chmod=+x", "scripts/run.sh")
        run_git(self.repo, "commit", "-qm", "fixture")
        self.commit = run_git(self.repo, "rev-parse", "HEAD").decode().strip()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def create(self, **kwargs):
        return SNAPSHOT.create_snapshot(self.repo, self.commit, self.base, **kwargs)

    def test_snapshot_uses_objects_not_mutated_worktree_and_preserves_modes(self) -> None:
        (self.repo / "plain.txt").write_bytes(b"live mutation\n")
        created = self.create()
        descriptor = SNAPSHOT.verify_snapshot(created.snapshot, self.commit)
        self.assertEqual((created.tree / "plain.txt").read_bytes(), b"committed\n")
        modes = {item["path"]: item["git_mode"] for item in descriptor["files"]}
        self.assertEqual(modes, {"plain.txt": "100644", "scripts/run.sh": "100755"})
        if os.name != "nt":
            self.assertEqual(stat.S_IMODE(os.lstat(created.tree / "scripts/run.sh").st_mode), 0o555)
        SNAPSHOT.destroy_snapshot(created.snapshot, created.destroy_token)

    def test_deterministic_descriptor_and_id_are_stage_independent(self) -> None:
        first = self.create(); second = self.create()
        self.assertEqual(first.snapshot_id, second.snapshot_id)
        self.assertEqual(first.snapshot.read_bytes(), second.snapshot.read_bytes())
        self.assertNotIn(first.destroy_token, first.snapshot.read_text("ascii"))
        self.assertNotIn(os.fspath(first.stage), first.snapshot.read_text("ascii"))
        SNAPSHOT.destroy_snapshot(first.snapshot, first.destroy_token)
        SNAPSHOT.destroy_snapshot(second.snapshot, second.destroy_token)

    def test_exact_commit_only_and_git_replace_is_disabled(self) -> None:
        with self.assertRaises(SNAPSHOT.SnapshotError):
            SNAPSHOT.create_snapshot(self.repo, "HEAD", self.base)
        old = self.commit
        (self.repo / "plain.txt").write_bytes(b"replacement\n")
        run_git(self.repo, "add", "plain.txt"); run_git(self.repo, "commit", "-qm", "replacement")
        replacement = run_git(self.repo, "rev-parse", "HEAD").decode().strip()
        run_git(self.repo, "replace", old, replacement)
        created = SNAPSHOT.create_snapshot(self.repo, old, self.base)
        self.assertEqual((created.tree / "plain.txt").read_bytes(), b"committed\n")
        SNAPSHOT.destroy_snapshot(created.snapshot, created.destroy_token)

    def test_mutation_after_graph_read_cannot_change_snapshot(self) -> None:
        def mutate(_: str) -> None:
            (self.repo / "plain.txt").write_bytes(b"raced live bytes\n")
            (self.repo / "untracked").write_bytes(b"not tracked")
        created = self.create(after_graph_read=mutate)
        self.assertEqual((created.tree / "plain.txt").read_bytes(), b"committed\n")
        self.assertFalse((created.tree / "untracked").exists())
        SNAPSHOT.destroy_snapshot(created.snapshot, created.destroy_token)

    def _commit_tree_entry(self, mode: str, object_type: str, oid: str, path: str) -> str:
        line = f"{mode} {object_type} {oid}\t{path}\n".encode()
        tree = run_git(self.repo, "mktree", "--missing", input_bytes=line).decode().strip()
        return run_git(self.repo, "commit-tree", tree, "-m", "synthetic", input_bytes=None).decode().strip()

    def test_symlink_and_gitlink_fail_closed(self) -> None:
        blob = run_git(self.repo, "hash-object", "-w", "--stdin", input_bytes=b"target").decode().strip()
        symlink_commit = self._commit_tree_entry("120000", "blob", blob, "link")
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "symlinks"):
            SNAPSHOT.create_snapshot(self.repo, symlink_commit, self.base)
        gitlink_commit = self._commit_tree_entry("160000", "commit", self.commit, "module")
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "Gitlinks"):
            SNAPSHOT.create_snapshot(self.repo, gitlink_commit, self.base)

    def test_tree_parser_rejects_duplicate_traversal_control_and_non_utf8(self) -> None:
        oid = bytes.fromhex("11" * 20)
        for raw in (
            b"100644 a\0" + oid + b"100644 a\0" + oid,
            b"100644 ..\0" + oid,
            b"100644 bad\nname\0" + oid,
            b"100644 \xff\0" + oid,
        ):
            with self.subTest(raw=raw):
                with self.assertRaises(SNAPSHOT.SnapshotError):
                    SNAPSHOT._parse_tree(raw, 20, "")

    def test_portable_case_collision_is_rejected(self) -> None:
        oid = bytes.fromhex("22" * 20)
        entries = SNAPSHOT._parse_tree(b"100644 A\0" + oid + b"100644 a\0" + oid, 20, "")
        self.assertEqual(len(entries), 2)  # raw Git names differ
        keys = [SNAPSHOT._portable_collision_key(item[2]) for item in entries]
        self.assertEqual(keys[0], keys[1])

    def test_tamper_extra_entry_and_wrong_capability_fail_and_leak(self) -> None:
        created = self.create()
        with self.assertRaises(SNAPSHOT.SnapshotError):
            SNAPSHOT.destroy_snapshot(created.snapshot, "0" * 64)
        os.chmod(created.stage, 0o700); (created.stage / "extra").write_bytes(b"x")
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "not exact"):
            SNAPSHOT.verify_snapshot(created.snapshot)
        with self.assertRaises(SNAPSHOT.SnapshotError):
            SNAPSHOT.destroy_snapshot(created.snapshot, created.destroy_token)
        self.assertTrue(created.stage.exists())

    def test_file_tamper_fails_and_does_not_destroy(self) -> None:
        created = self.create(); target = created.tree / "plain.txt"
        os.chmod(created.stage, 0o700); os.chmod(created.tree, 0o700); os.chmod(target, 0o600)
        target.write_bytes(b"tampered\n")
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "bytes changed"):
            SNAPSHOT.verify_snapshot(created.snapshot)
        with self.assertRaises(SNAPSHOT.SnapshotError):
            SNAPSHOT.destroy_snapshot(created.snapshot, created.destroy_token)
        self.assertTrue(created.stage.exists())

    @unittest.skipIf(not hasattr(os, "symlink"), "symlinks unavailable")
    def test_raced_symlink_never_touches_outside_target(self) -> None:
        outside = self.base / "outside"; outside.write_bytes(b"outside-safe")
        leaked = []
        def attack(stage: pathlib.Path) -> None:
            leaked.append(stage)
            victim = stage / "tree/plain.txt"; victim.unlink()
            try:
                os.symlink(outside, victim)
            except OSError as error:
                self.skipTest(f"symlink creation unavailable: {error}")
        with self.assertRaises((SNAPSHOT.SnapshotError, OSError)):
            self.create(after_materialize=attack)
        self.assertEqual(outside.read_bytes(), b"outside-safe")
        self.assertTrue(leaked and leaked[0].exists())

    def test_descriptor_tamper_is_rejected_even_with_recomputed_id(self) -> None:
        created = self.create()
        os.chmod(created.stage, 0o700); os.chmod(created.snapshot, 0o600)
        descriptor = json.loads(created.snapshot.read_bytes())
        descriptor["scope"]["claim"] = "reproducible-build-proof"
        body = dict(descriptor); body.pop("snapshot_id")
        descriptor["snapshot_id"] = SNAPSHOT.sha256_bytes(SNAPSHOT.canonical_bytes(body))
        created.snapshot.write_bytes(SNAPSHOT.canonical_bytes(descriptor))
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "scope"):
            SNAPSHOT.verify_snapshot(created.snapshot)

    def test_cli_query_round_trip_and_noncanonical_rejection(self) -> None:
        created = self.create(); raw = SNAPSHOT.canonical_bytes(created.cli_result())
        self.assertEqual(SNAPSHOT.query_result(raw, "tree"), os.fspath(created.tree))
        self.assertEqual(SNAPSHOT.query_result(raw.replace(b"\n", b"\r\n"), "files_count"), "2")
        with self.assertRaises(SNAPSHOT.SnapshotError):
            SNAPSHOT.query_result(b" " + raw, "stage")
        SNAPSHOT.destroy_snapshot(created.snapshot, created.destroy_token)

    def test_verify_against_git_reconstructs_complete_descriptor(self) -> None:
        created = self.create()
        result = SNAPSHOT.verify_against_git(self.repo, self.commit, created.snapshot)
        self.assertEqual(result["snapshot_id"], created.snapshot_id)
        self.assertEqual(result["commit_oid"], self.commit)
        self.assertEqual(
            result["descriptor_sha256"],
            SNAPSHOT.sha256_bytes(created.snapshot.read_bytes()),
        )
        raw = SNAPSHOT.canonical_bytes(result)
        self.assertEqual(
            SNAPSHOT.query_git_verification(raw, "tree"), os.fspath(created.tree)
        )
        SNAPSHOT.destroy_snapshot(created.snapshot, created.destroy_token)

    def test_forged_self_consistent_descriptor_requires_git_authentication(self) -> None:
        created = self.create()
        descriptor = json.loads(created.snapshot.read_bytes())
        forged = b"forged but locally self-consistent\n"
        item = next(value for value in descriptor["files"] if value["path"] == "plain.txt")
        item["size"] = len(forged)
        item["sha256"] = SNAPSHOT.sha256_bytes(forged)
        item["blob_oid"] = SNAPSHOT._object_digest(descriptor["object_format"], "blob", forged)
        body = dict(descriptor); body.pop("snapshot_id")
        descriptor["snapshot_id"] = SNAPSHOT.sha256_bytes(SNAPSHOT.canonical_bytes(body))
        sentinel_path = created.stage / SNAPSHOT.SENTINEL_NAME
        sentinel = json.loads(sentinel_path.read_bytes())
        sentinel["snapshot_id"] = descriptor["snapshot_id"]
        os.chmod(created.stage, 0o700); os.chmod(created.tree, 0o700)
        os.chmod(created.tree / "plain.txt", 0o600)
        os.chmod(created.snapshot, 0o600); os.chmod(sentinel_path, 0o600)
        (created.tree / "plain.txt").write_bytes(forged)
        created.snapshot.write_bytes(SNAPSHOT.canonical_bytes(descriptor))
        sentinel_path.write_bytes(SNAPSHOT.canonical_bytes(sentinel))
        SNAPSHOT._seal(created.stage, descriptor["directories"], descriptor["files"])
        SNAPSHOT.verify_snapshot(created.snapshot, self.commit)
        with self.assertRaisesRegex(SNAPSHOT.SnapshotError, "authenticated Git objects"):
            SNAPSHOT.verify_against_git(self.repo, self.commit, created.snapshot)
        SNAPSHOT.destroy_snapshot(created.snapshot, created.destroy_token)

    def test_cli_create_verify_query_destroy(self) -> None:
        created = subprocess.run((sys.executable, os.fspath(TOOL), "create", "--repo-root", os.fspath(self.repo), "--commit", self.commit, "--stage-parent", os.fspath(self.base)), stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)
        result = json.loads(created.stdout)
        query = subprocess.run((sys.executable, os.fspath(TOOL), "query-result", "--field", "snapshot"), input=created.stdout, stdout=subprocess.PIPE, check=True)
        self.assertEqual(query.stdout.decode().strip(), result["snapshot"])
        subprocess.run((sys.executable, os.fspath(TOOL), "verify", "--commit", self.commit, result["snapshot"]), check=True, stdout=subprocess.PIPE)
        verified = subprocess.run((sys.executable, os.fspath(TOOL), "verify-against-git", "--repo-root", os.fspath(self.repo), "--commit", self.commit, result["snapshot"]), check=True, stdout=subprocess.PIPE).stdout
        verified_result = json.loads(verified)
        self.assertEqual(verified_result["snapshot_id"], result["snapshot_id"])
        queried = subprocess.run((sys.executable, os.fspath(TOOL), "query-verified", "--field", "descriptor_sha256"), input=verified, check=True, stdout=subprocess.PIPE).stdout.decode().strip()
        self.assertEqual(queried, verified_result["descriptor_sha256"])
        subprocess.run((sys.executable, os.fspath(TOOL), "destroy", "--token", result["destroy_token"], result["snapshot"]), check=True)
        self.assertFalse(pathlib.Path(result["stage"]).exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
