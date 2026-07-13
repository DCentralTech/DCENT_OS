#!/usr/bin/env python3
"""Unit tests for canonical release-capsule target admission."""

from __future__ import annotations

import unittest
from pathlib import Path
import subprocess

import release_capsule_target_policy as policy
import source_closure


class ReleaseCapsuleTargetPolicyTests(unittest.TestCase):
    def test_admitted_targets_have_exact_release_fields(self) -> None:
        self.assertEqual(
            policy.policy_for("s9"),
            policy.ReleaseCapsuleTargetPolicy(
                target="s9",
                cargo_variant="zynq",
                primary_artifact="dcentos-unit.tar",
                package_board="am1-s9",
                release_stem="DCENTOS_XIL1_S9",
                publication_admitted=True,
            ),
        )
        self.assertEqual(
            policy.policy_for("am2-s19jpro"),
            policy.ReleaseCapsuleTargetPolicy(
                target="am2-s19jpro",
                cargo_variant="zynq",
                primary_artifact="dcentos-sysupgrade-am2-s19jpro.tar",
                package_board="am2-s19j",
                release_stem="DCENTOS_XIL3_S19jPro",
                publication_admitted=False,
            ),
        )

    def test_admission_is_coherent_with_source_closure_policies(self) -> None:
        for target, record in policy.POLICIES.items():
            self.assertIn(target, source_closure.BUILD_TARGET_POLICIES)
            self.assertIn(target, source_closure.TARGET_BUILD_INPUTS)
            self.assertIn(target, source_closure.PREBUILT_RUST_INPUTS_BY_TARGET)
            self.assertEqual(
                source_closure.PREBUILT_RUST_VARIANT_BY_TARGET[target],
                record.cargo_variant,
            )

    def test_sd_unknown_and_empty_targets_fail_closed(self) -> None:
        for target in ("am2-s19jpro-sd", "am2-s19pro", "unknown", "", None):
            with self.subTest(target=target):
                with self.assertRaises(policy.TargetPolicyError):
                    policy.policy_for(target)

    def test_query_exposes_only_canonical_fields(self) -> None:
        self.assertEqual(policy.query("am2-s19jpro", "package_board"), "am2-s19j")
        self.assertEqual(
            policy.query("am2-s19jpro", "publication_admitted"), "false"
        )
        with self.assertRaises(policy.TargetPolicyError):
            policy.query("s9", "__dict__")

    def test_publication_admission_is_separate_from_evidence_policy(self) -> None:
        self.assertEqual(
            policy.policy_for("s9", require_publication=True).target, "s9"
        )
        with self.assertRaises(policy.TargetPolicyError):
            policy.policy_for("am2-s19jpro", require_publication=True)

    def test_release_names_are_target_bound_and_calendar_valid(self) -> None:
        for target, expected in (
            ("s9", "DCENTOS_XIL1_S9_beta20260712"),
            ("am2-s19jpro", "DCENTOS_XIL3_S19jPro_beta20260712"),
        ):
            record = policy.policy_for(target)
            self.assertEqual(policy.validate_output_name(record, expected), expected)
            helper = Path(__file__).with_name("firmware_release_name.sh")
            observed = subprocess.run(
                ["sh", str(helper), target, "beta", "20260712"],
                check=True,
                text=True,
                stdout=subprocess.PIPE,
            ).stdout.strip()
            self.assertEqual(observed, expected)
        for invalid in (
            "DCENTOS_XIL3_S19jPro_beta20260712",
            "DCENTOS_XIL1_S9_beta20261340",
            "DCENTOS_XIL1_S9_nightly20260712",
        ):
            with self.assertRaises(policy.TargetPolicyError):
                policy.validate_output_name(policy.policy_for("s9"), invalid)


if __name__ == "__main__":
    unittest.main()
