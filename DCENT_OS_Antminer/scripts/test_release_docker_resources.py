#!/usr/bin/env python3
"""Adversarial tests for release_docker_resources.py."""

from __future__ import annotations

import ast
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("release_docker_resources.py")
SPEC = importlib.util.spec_from_file_location("release_docker_resources", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
resources = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(resources)
ri = resources.invocation


class ReleaseDockerResourceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.parent = Path(self.temporary.name) / "invocations"
        self.parent.mkdir()
        self.created = ri.create_invocation(self.parent, "release")

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

    def create_spec(self, role: str = "cargo"):
        return resources.create_volume_spec(self.created.stage, role)

    def inspect(self, role: str = "cargo"):
        spec = self.create_spec(role)
        return [
            {
                "CreatedAt": "2026-07-12T12:34:56Z",
                "Driver": "local",
                "Labels": spec["labels"],
                "Mountpoint": f"/var/lib/docker/volumes/{spec['name']}/_data",
                "Name": spec["name"],
                "Options": None,
                "Scope": "local",
            }
        ]

    @staticmethod
    def raw(value) -> bytes:
        return json.dumps(value, separators=(",", ":"), sort_keys=True).encode("utf-8")

    def run_cli(self, *arguments: str, stdin: bytes = b""):
        return subprocess.run(
            [sys.executable, str(SCRIPT), *arguments],
            input=stdin,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    def test_create_spec_has_exact_name_labels_and_declarative_argv(self) -> None:
        spec = self.create_spec("buildroot")
        verified = ri.verify_invocation(self.created.stage)
        expected_name = verified.descriptor["resources"]["docker_volumes"]["buildroot"]
        self.assertEqual(spec["name"], expected_name)
        self.assertEqual(spec["argv"][:5], ["docker", "volume", "create", "--driver", "local"])
        self.assertEqual(spec["argv"][-2:], ["--", expected_name])
        self.assertEqual(
            set(spec["labels"]),
            {
                resources.LABEL_SCHEMA,
                resources.LABEL_INVOCATION,
                resources.LABEL_ROLE,
                resources.LABEL_DESCRIPTOR,
            },
        )
        self.assertEqual(spec["labels"][resources.LABEL_ROLE], "buildroot")
        self.assertEqual(
            spec["labels"][resources.LABEL_INVOCATION], self.created.invocation_id
        )
        self.assertEqual(
            spec["labels"][resources.LABEL_DESCRIPTOR], spec["descriptor_sha256"]
        )

    def test_inspect_and_builder_tag_specs_are_exact_invocation_identities(self) -> None:
        inspect_spec = resources.inspect_volume_spec(self.created.stage, "results")
        self.assertEqual(
            inspect_spec["argv"],
            ["docker", "volume", "inspect", "--", inspect_spec["expected_name"]],
        )
        tag = resources.builder_tag_spec(self.created.stage)
        self.assertEqual(tag["tag"], f"dcentos-release-builder:{self.created.invocation_id}")
        self.assertNotIn("argv", tag)
        self.assertEqual(
            tag["removal_authority"]["exact_tag_only"], tag["tag"]
        )
        self.assertTrue(
            tag["removal_authority"]["requires_independent_retained_image_reference"]
        )

    def test_valid_inspect_produces_safe_decision(self) -> None:
        decision, inspected = resources.verify_inspect(
            self.created.stage, "cargo", self.raw(self.inspect("cargo"))
        )
        self.assertTrue(decision["allowed"])
        self.assertEqual(decision["name"], inspected["Name"])
        self.assertRegex(decision["inspect_sha256"], r"^[0-9a-f]{64}$")
        self.assertIn("filesystem-links-not-attested", decision["mountpoint_assurance"])

    def test_swapped_invocation_role_name_or_labels_are_rejected(self) -> None:
        other = ri.create_invocation(self.parent, "other")
        cases = []
        swapped_invocation = self.inspect("cargo")
        swapped_invocation[0]["Labels"][resources.LABEL_INVOCATION] = other.invocation_id
        cases.append(swapped_invocation)
        swapped_role = self.inspect("cargo")
        swapped_role[0]["Labels"][resources.LABEL_ROLE] = "results"
        cases.append(swapped_role)
        wrong_name = self.inspect("cargo")
        wrong_name[0]["Name"] = self.create_spec("results")["name"]
        cases.append(wrong_name)
        wrong_digest = self.inspect("cargo")
        wrong_digest[0]["Labels"][resources.LABEL_DESCRIPTOR] = "0" * 64
        cases.append(wrong_digest)
        extra_label = self.inspect("cargo")
        extra_label[0]["Labels"]["untrusted"] = "true"
        cases.append(extra_label)
        for value in cases:
            with self.subTest(value=value):
                with self.assertRaises(resources.DockerResourceError):
                    resources.verify_inspect(self.created.stage, "cargo", self.raw(value))

    def test_preexisting_unrelated_resource_and_two_invocations_are_isolated(self) -> None:
        other = ri.create_invocation(self.parent, "release")
        first_names = {self.create_spec(role)["name"] for role in resources.ROLES}
        second_names = {
            resources.create_volume_spec(other.stage, role)["name"]
            for role in resources.ROLES
        }
        self.assertTrue(first_names.isdisjoint(second_names))
        unrelated = self.inspect("cargo")
        unrelated[0]["Name"] = "preexisting-unrelated-volume"
        unrelated[0]["Mountpoint"] = (
            "/var/lib/docker/volumes/preexisting-unrelated-volume/_data"
        )
        with self.assertRaises(resources.DockerResourceError):
            resources.verify_inspect(self.created.stage, "cargo", self.raw(unrelated))
        other_inspect = self.inspect("cargo")
        other_spec = resources.create_volume_spec(other.stage, "cargo")
        other_inspect[0]["Name"] = other_spec["name"]
        other_inspect[0]["Labels"] = other_spec["labels"]
        other_inspect[0]["Mountpoint"] = (
            f"/var/lib/docker/volumes/{other_spec['name']}/_data"
        )
        with self.assertRaises(resources.DockerResourceError):
            resources.verify_inspect(self.created.stage, "cargo", self.raw(other_inspect))

    def test_malformed_oversized_or_non_exact_inspect_is_rejected(self) -> None:
        malformed = (
            b"not-json",
            b"{}",
            b"[]",
            self.raw([self.inspect()[0], self.inspect()[0]]),
            b'[{"Name":"first","Name":"second"}]',
            b'[{"CreatedAt":NaN}]',
            (b"[" * 2000) + (b"]" * 2000),
            b" " * (resources.MAX_INSPECT_BYTES + 1),
        )
        for raw in malformed:
            with self.subTest(length=len(raw)):
                with self.assertRaises(resources.DockerResourceError):
                    resources.verify_inspect(self.created.stage, "cargo", raw)
        for mutation in ("extra", "missing"):
            value = self.inspect()
            if mutation == "extra":
                value[0]["Status"] = {}
            else:
                del value[0]["Scope"]
            with self.assertRaises(resources.DockerResourceError):
                resources.verify_inspect(self.created.stage, "cargo", self.raw(value))

    def test_wrong_case_controls_command_injection_and_local_aliases_are_rejected(self) -> None:
        cases = []
        wrong_case = self.inspect()
        wrong_case[0]["Driver"] = "Local"
        cases.append(wrong_case)
        control = self.inspect()
        control[0]["CreatedAt"] = "now\nthen"
        cases.append(control)
        injected = self.inspect()
        injected[0]["Name"] += ";docker system prune"
        cases.append(injected)
        bind_option = self.inspect()
        bind_option[0]["Options"] = {"device": "/tmp/other", "o": "bind", "type": "none"}
        cases.append(bind_option)
        for mountpoint in (
            "relative/volumes/name/_data",
            "//var/lib/docker/volumes/name/_data",
            "/var/lib/docker/../volumes/name/_data",
            "/var/lib/docker/volumes/name/_data/",
            "C:\\docker\\volumes\\name\\_data",
        ):
            ambiguous = self.inspect()
            ambiguous[0]["Mountpoint"] = mountpoint
            cases.append(ambiguous)
        for value in cases:
            with self.subTest(value=value):
                with self.assertRaises(resources.DockerResourceError):
                    resources.verify_inspect(self.created.stage, "cargo", self.raw(value))
        with self.assertRaises(ri.InvocationError):
            ri.create_invocation(self.parent, "release;prune")

    def test_destroy_requires_verified_inspect_capability_and_explicit_cleanup_state(self) -> None:
        raw = self.raw(self.inspect("results"))
        for state in ("empty", "disposable"):
            spec = resources.destroy_volume_spec(
                self.created.stage,
                self.created.capability,
                "results",
                raw,
                state,
            )
            self.assertEqual(
                spec["argv"], ["docker", "volume", "rm", "--", spec["name"]]
            )
            self.assertEqual(spec["cleanup_state"], state)
            self.assertRegex(spec["inspect_sha256"], r"^[0-9a-f]{64}$")
        with self.assertRaises(resources.DockerResourceError):
            resources.destroy_volume_spec(
                self.created.stage,
                self.created.capability,
                "results",
                raw,
                "unknown",
            )
        other = ri.create_invocation(self.parent, "other")
        with self.assertRaises(resources.DockerResourceError):
            resources.destroy_volume_spec(
                self.created.stage, other.capability, "results", raw, "empty"
            )
        tampered = self.inspect("results")
        tampered[0]["Labels"][resources.LABEL_ROLE] = "cargo"
        with self.assertRaises(resources.DockerResourceError):
            resources.destroy_volume_spec(
                self.created.stage,
                self.created.capability,
                "results",
                self.raw(tampered),
                "disposable",
            )

    def test_cli_has_bounded_fail_closed_stdin_and_canonical_output(self) -> None:
        created = self.run_cli("create-spec", "--role", "cargo", str(self.created.stage))
        self.assertEqual(created.returncode, 0, created.stderr.decode())
        self.assertEqual(created.stdout, resources.canonical_bytes(json.loads(created.stdout)))
        rejected = self.run_cli(
            "verify-inspect",
            "--role",
            "cargo",
            str(self.created.stage),
            stdin=b"x" * (resources.MAX_INSPECT_BYTES + 1),
        )
        self.assertNotEqual(rejected.returncode, 0)
        self.assertEqual(rejected.stdout, b"")
        self.assertIn(b"exceeds", rejected.stderr)

    def test_emit_argv0_roundtrips_create_inspect_and_destroy_specs(self) -> None:
        create = resources.create_volume_spec(self.created.stage, "cargo")
        inspect = resources.inspect_volume_spec(self.created.stage, "buildroot")
        destroy = resources.destroy_volume_spec(
            self.created.stage,
            self.created.capability,
            "results",
            self.raw(self.inspect("results")),
            "empty",
        )
        for spec in (create, inspect, destroy):
            with self.subTest(operation=spec["operation"]):
                emitted = resources.emit_argv0(
                    self.created.stage, resources.canonical_bytes(spec)
                )
                self.assertTrue(emitted.endswith(b"\0"))
                self.assertEqual(
                    emitted[:-1].split(b"\0"),
                    [argument.encode("ascii") for argument in spec["argv"]],
                )
                cli = self.run_cli(
                    "emit-argv0",
                    str(self.created.stage),
                    stdin=resources.canonical_bytes(spec),
                )
                self.assertEqual(cli.returncode, 0, cli.stderr.decode())
                self.assertEqual(cli.stdout, emitted)

    def test_emit_argv0_rejects_forged_argv_options_schema_and_operation(self) -> None:
        valid = resources.create_volume_spec(self.created.stage, "cargo")
        forged = []

        argv = json.loads(resources.canonical_bytes(valid))
        argv["argv"].extend(("--opt", "device=/tmp/unrelated"))
        forged.append(argv)

        schema = json.loads(resources.canonical_bytes(valid))
        schema["schema"] = "org.example.forged.v1"
        forged.append(schema)

        operation = json.loads(resources.canonical_bytes(valid))
        operation["operation"] = "destroy-volume"
        forged.append(operation)

        options = json.loads(resources.canonical_bytes(valid))
        options["options"] = {"device": "/tmp/unrelated"}
        forged.append(options)

        inspect = resources.inspect_volume_spec(self.created.stage, "cargo")
        inspect["argv"][-1] = self.create_spec("results")["name"]
        forged.append(inspect)

        destroy = resources.destroy_volume_spec(
            self.created.stage,
            self.created.capability,
            "results",
            self.raw(self.inspect("results")),
            "disposable",
        )
        destroy["argv"].insert(-1, "--force")
        forged.append(destroy)

        for spec in forged:
            with self.subTest(spec=spec):
                with self.assertRaises(resources.DockerResourceError):
                    resources.emit_argv0(
                        self.created.stage, resources.canonical_bytes(spec)
                    )

        noncanonical = json.dumps(valid).encode("ascii")
        with self.assertRaises(resources.DockerResourceError):
            resources.emit_argv0(self.created.stage, noncanonical)
        with self.assertRaises(resources.DockerResourceError):
            resources.emit_argv0(
                self.created.stage,
                resources.canonical_bytes(resources.builder_tag_spec(self.created.stage)),
            )

    def test_query_builder_tag_is_a_strict_safe_scalar(self) -> None:
        expected = f"dcentos-release-builder:{self.created.invocation_id}\n"
        queried = self.run_cli("query-builder-tag", str(self.created.stage))
        self.assertEqual(queried.returncode, 0, queried.stderr.decode())
        self.assertEqual(queried.stdout.decode("ascii").replace("\r\n", "\n"), expected)

    @unittest.skipUnless(
        os.name == "posix" and shutil.which("bash"),
        "Bash mapfile transport proof runs on the WSL/POSIX lane",
    )
    def test_bash_mapfile_d_nul_roundtrip_preserves_argv_boundaries(self) -> None:
        spec = resources.create_volume_spec(self.created.stage, "cargo")
        spec_path = Path(self.temporary.name) / "create-spec.json"
        spec_path.write_bytes(resources.canonical_bytes(spec))
        shell = r'''
set -euo pipefail
mapfile -d '' -t argv < <(python3 "$1" emit-argv0 "$2" < "$3")
printf '%s\0' "${argv[@]}"
'''
        completed = subprocess.run(
            [
                "bash",
                "-c",
                shell,
                "release-docker-mapfile-test",
                str(SCRIPT),
                str(self.created.stage),
                str(spec_path),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr.decode())
        self.assertEqual(
            completed.stdout,
            b"".join(argument.encode("ascii") + b"\0" for argument in spec["argv"]),
        )

    def test_module_never_imports_execution_or_clock_facilities(self) -> None:
        source = SCRIPT.read_text(encoding="utf-8")
        tree = ast.parse(source)
        imported = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported.update(alias.name.split(".")[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                imported.add(node.module.split(".")[0])
        self.assertNotIn("subprocess", imported)
        self.assertNotIn("time", imported)
        self.assertFalse(hasattr(resources, "subprocess"))
        self.assertFalse(hasattr(resources, "time"))
        self.assertNotIn("os.system", source)
        self.assertNotIn("Popen", source)


if __name__ == "__main__":
    unittest.main(verbosity=2)
