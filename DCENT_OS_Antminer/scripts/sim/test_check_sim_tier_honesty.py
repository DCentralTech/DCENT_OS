#!/usr/bin/env python3
"""Regression tests for simulator tier and vector evidence boundaries."""

from __future__ import annotations

import json
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
from pathlib import Path

if __package__:
    from . import check_sim_tier_honesty, ladder_matrix
else:
    import check_sim_tier_honesty
    import ladder_matrix


SCRIPT_DIR = Path(__file__).resolve().parent


class SimTierHonestyTests(unittest.TestCase):
    @staticmethod
    def baseline_models() -> dict[str, dict[str, object]]:
        models = {
            model: {"tier": 1, "strictness": "structural"}
            for model in check_sim_tier_honesty.EXPECTED_MODELS
        }
        models["s23"] = {"tier": 1, "strictness": "scaffold"}
        return models

    def run_checker(
        self,
        *,
        claim: dict[str, object],
        header: dict[str, object] | None,
        requested_model: str | None = None,
        requested_tier: int | None = None,
        schema: object = "dcent-sim-tier-matrix-v1",
    ) -> tuple[int, str]:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            matrix = root / "model_tiers.json"
            workspace = root / "workspace"
            models = self.baseline_models()
            models["s9"] = claim
            matrix_data = {
                "schema": schema,
                "models": models,
            }
            matrix.write_text(
                json.dumps(matrix_data),
                encoding="utf-8",
            )
            if header is not None:
                vector = (
                    workspace
                    / "dcentrald-re-catalog"
                    / "vectors"
                    / "s9"
                    / "init_sequence.jsonl"
                )
                vector.parent.mkdir(parents=True)
                vector.write_text(json.dumps(header) + "\n", encoding="utf-8")

            argv = ["--matrix", str(matrix), "--workspace", str(workspace)]
            if requested_model is not None and requested_tier is not None:
                argv.extend(
                    ["--model", requested_model, "--tier", str(requested_tier)]
                )
            output = StringIO()
            with redirect_stdout(output), redirect_stderr(output):
                result = check_sim_tier_honesty.main(argv)
            return result, output.getvalue()

    @staticmethod
    def exact_header(**overrides: object) -> dict[str, object]:
        header: dict[str, object] = {
            "schema": "dcent-init-trace-v1",
            "model": "s9",
            "strictness": "exact",
        }
        header.update(overrides)
        return header

    def test_matching_t3_vector_and_request_pass(self) -> None:
        result, output = self.run_checker(
            claim={"tier": 3, "strictness": "exact"},
            header=self.exact_header(),
            requested_model="s9",
            requested_tier=3,
        )
        self.assertEqual(result, 0, output)
        self.assertIn("request=s9:T3", output)

    def test_matrix_cannot_overstate_vector_strictness(self) -> None:
        result, output = self.run_checker(
            claim={"tier": 3, "strictness": "exact"},
            header=self.exact_header(
                strictness="implementation_snapshot", maturity="experimental"
            ),
        )
        self.assertEqual(result, 1)
        self.assertIn("disagrees with vector strictness", output)
        self.assertIn("experimental vector may not claim T3", output)

    def test_matching_experimental_t2_snapshot_passes(self) -> None:
        result, output = self.run_checker(
            claim={"tier": 2, "strictness": "implementation_snapshot"},
            header=self.exact_header(
                strictness="implementation_snapshot", maturity="experimental"
            ),
            requested_model="s9",
            requested_tier=2,
        )
        self.assertEqual(result, 0, output)

    def test_t2_matrix_cannot_disagree_with_snapshot_header(self) -> None:
        result, output = self.run_checker(
            claim={"tier": 2, "strictness": "implementation_snapshot"},
            header=self.exact_header(),
        )
        self.assertEqual(result, 1)
        self.assertIn("disagrees with vector strictness", output)

    def test_vector_header_must_name_declared_model(self) -> None:
        result, output = self.run_checker(
            claim={"tier": 3, "strictness": "exact"},
            header=self.exact_header(model="s17"),
        )
        self.assertEqual(result, 1)
        self.assertIn("init vector header names model 's17'", output)

    def test_unknown_requested_model_fails_even_at_t0(self) -> None:
        result, output = self.run_checker(
            claim={"tier": 1, "strictness": "structural"},
            header=None,
            requested_model="unknown",
            requested_tier=0,
        )
        self.assertEqual(result, 1)
        self.assertIn("model is not declared", output)

    def test_request_cannot_exceed_declared_tier(self) -> None:
        result, output = self.run_checker(
            claim={"tier": 2, "strictness": "implementation_snapshot"},
            header=None,
            requested_model="s9",
            requested_tier=3,
        )
        self.assertEqual(result, 1)
        self.assertIn("requested T3 exceeds declared T2", output)

    def test_matrix_schema_is_exact(self) -> None:
        result, output = self.run_checker(
            claim={"tier": 1, "strictness": "structural"},
            header=None,
            schema="wrong-schema",
        )
        self.assertEqual(result, 1)
        self.assertIn("matrix schema must be", output)

    def test_matrix_model_inventory_is_exact(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            matrix = Path(temporary) / "model_tiers.json"
            models = self.baseline_models()
            del models["s23"]
            models["future"] = {"tier": 1, "strictness": "structural"}
            matrix.write_text(
                json.dumps(
                    {"schema": "dcent-sim-tier-matrix-v1", "models": models}
                ),
                encoding="utf-8",
            )
            _, failures = check_sim_tier_honesty.load_tier_matrix(matrix)
        self.assertIn("matrix is missing required model s23", failures)
        self.assertIn("matrix declares unexpected model future", failures)

    def test_matrix_rejects_duplicate_json_keys(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            matrix = Path(temporary) / "model_tiers.json"
            encoded = json.dumps(
                {
                    "schema": "dcent-sim-tier-matrix-v1",
                    "models": self.baseline_models(),
                }
            )
            matrix.write_text(
                encoded.replace(
                    '"schema": "dcent-sim-tier-matrix-v1"',
                    '"schema": "wrong", '
                    '"schema": "dcent-sim-tier-matrix-v1"',
                    1,
                ),
                encoding="utf-8",
            )
            _, failures = check_sim_tier_honesty.load_tier_matrix(matrix)
        self.assertTrue(
            any("duplicate JSON key 'schema'" in failure for failure in failures),
            failures,
        )

    def test_vector_header_rejects_duplicate_json_keys(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            vector = Path(temporary) / "init_sequence.jsonl"
            vector.write_text(
                '{"schema":"dcent-init-trace-v1","model":"s9",'
                '"strictness":"exact","strictness":"structural"}\n',
                encoding="utf-8",
            )
            with self.assertRaisesRegex(ValueError, "duplicate JSON key 'strictness'"):
                check_sim_tier_honesty.read_vector_header(vector)

    def test_declared_tier_is_not_coerced(self) -> None:
        for malformed_tier in (3.9, "3", True):
            with self.subTest(tier=malformed_tier):
                result, output = self.run_checker(
                    claim={"tier": malformed_tier, "strictness": "exact"},
                    header=self.exact_header(),
                )
                self.assertEqual(result, 1)
                self.assertIn("declared tier must be an integer", output)

    def test_t4_cannot_be_declared_from_static_evidence(self) -> None:
        result, output = self.run_checker(
            claim={"tier": 4, "strictness": "exact"},
            header=self.exact_header(),
            requested_model="s9",
            requested_tier=4,
        )
        self.assertEqual(result, 1)
        self.assertIn("T4 requires per-run runtime and write-path evidence", output)

    def test_strictness_must_be_a_known_string(self) -> None:
        result, output = self.run_checker(
            claim={"tier": 1, "strictness": ["exact"]},
            header=None,
        )
        self.assertEqual(result, 1)
        self.assertIn("unknown evidence strictness", output)

    def test_ladder_renderer_uses_validated_ascii_cells(self) -> None:
        output = StringIO()
        with redirect_stdout(output), redirect_stderr(output):
            result = ladder_matrix.main([])
        rendered = output.getvalue()
        self.assertEqual(result, 0, rendered)
        self.assertNotIn("—", rendered)
        self.assertIn(
            "| s19pro | PASS | PASS | PASS | - | - | "
            "implementation_snapshot |",
            rendered,
        )

    def test_ladder_renderer_rejects_coerced_tier(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            matrix = Path(temporary) / "model_tiers.json"
            models = self.baseline_models()
            models["s9"] = {"tier": 3.9, "strictness": "exact"}
            matrix.write_text(
                json.dumps(
                    {
                        "schema": "dcent-sim-tier-matrix-v1",
                        "models": models,
                    }
                ),
                encoding="utf-8",
            )
            output = StringIO()
            with redirect_stdout(output), redirect_stderr(output):
                result = ladder_matrix.main(["--matrix", str(matrix)])
        self.assertEqual(result, 1)
        self.assertIn("declared tier must be an integer", output.getvalue())

    def test_ladder_renderer_rejects_missing_t3_vector(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            matrix = root / "model_tiers.json"
            models = self.baseline_models()
            models["s9"] = {"tier": 3, "strictness": "exact"}
            matrix.write_text(
                json.dumps(
                    {"schema": "dcent-sim-tier-matrix-v1", "models": models}
                ),
                encoding="utf-8",
            )
            output = StringIO()
            with redirect_stdout(output), redirect_stderr(output):
                result = ladder_matrix.main(
                    [
                        "--matrix",
                        str(matrix),
                        "--workspace",
                        str(root / "workspace"),
                    ]
                )
        self.assertEqual(result, 1)
        self.assertIn("s9: T3 has no init_sequence.jsonl", output.getvalue())

    def test_full_proof_validates_model_before_allocating_evidence(self) -> None:
        source = (SCRIPT_DIR / "full_offline_model_proof.sh").read_text(
            encoding="utf-8"
        )
        self.assertLess(source.index('case "$MODEL" in'), source.index("mkdir -p"))
        self.assertIn('mktemp -d "$evidence_root/${MODEL}-${stamp}-XXXXXX"', source)


if __name__ == "__main__":
    unittest.main()
