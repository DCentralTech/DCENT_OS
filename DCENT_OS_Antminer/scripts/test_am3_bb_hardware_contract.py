#!/usr/bin/env python3
"""Negative-fixture tests for the catalog-backed AM3-BB static gate."""

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import check_am3_bb_hardware_contract as contract


ROOT = Path(__file__).resolve().parent.parent


class HardwareContractGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="dcent-am3-bb-contract-")
        self.root = Path(self.temp.name)
        catalog = ROOT / contract.DEFAULT_CATALOG
        data = __import__("json").loads(catalog.read_text(encoding="utf-8"))
        paths = {
            contract.DEFAULT_CATALOG,
            contract.TARGET_TOML,
            contract.HAL,
            contract.BOOT_SETUP,
            contract.DAEMON_INIT,
            contract.POST_IMAGE,
            contract.DOCKER_BUILD,
            Path(data["legacy_reference"]["path"]),
        }
        paths.update(Path(value) for value in data["inventory"]["defconfigs"])
        paths.update(Path(value) for value in data["inventory"]["product_build_scripts"])
        paths.add(Path(data["inventory"]["dtb_contract_helper"]))
        for relative in paths:
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / relative, destination)
        fixture_references = []
        for index, reference in enumerate(data["evidence"]["lineage_references"]):
            relative = Path("fixture-evidence") / ("{}-{}".format(index, Path(reference).name))
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2((ROOT / reference).resolve(), destination)
            fixture_references.append(relative.as_posix())
        data["evidence"]["lineage_references"] = fixture_references
        (self.root / contract.DEFAULT_CATALOG).write_text(
            __import__("json").dumps(data, indent=2) + "\n", encoding="utf-8"
        )

    def tearDown(self) -> None:
        self.temp.cleanup()

    def errors(self):
        return contract.validate_repository(self.root)

    def mutate(self, relative: Path, old: str, new: str) -> None:
        path = self.root / relative
        text = path.read_text(encoding="utf-8")
        self.assertIn(old, text)
        path.write_text(text.replace(old, new, 1), encoding="utf-8")

    def rewrite_catalog(self, mutator) -> None:
        import json

        path = self.root / contract.DEFAULT_CATALOG
        data = json.loads(path.read_text(encoding="utf-8"))
        mutator(data)
        path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")

    def test_checked_in_repository_is_consistent(self) -> None:
        self.assertEqual([], self.errors())

    def test_wrong_reset_gpio_is_rejected(self) -> None:
        self.mutate(contract.TARGET_TOML, "asic_rst = [49, 60, 27, 22]", "asic_rst = [5, 4, 27, 22]")
        self.assertTrue(any("ASIC reset" in error for error in self.errors()))

    def test_missing_legacy_quarantine_is_rejected(self) -> None:
        self.mutate(
            Path("br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/am335x-s19jpro.dts"),
            "DCENT_REFERENCE_ONLY: bbctrl-s70",
            "unreviewed legacy source",
        )
        self.assertTrue(any("quarantine" in error for error in self.errors()))

    def test_generic_product_fallback_is_rejected(self) -> None:
        path = self.root / Path("scripts/build_am3_bb_s19jpro.sh")
        with path.open("a", encoding="utf-8") as handle:
            handle.write("\n# Falling back to '--target am3-bb'\n")
        self.assertTrue(any("product build target" in error for error in self.errors()))

    def test_unsafe_board_enable_default_is_rejected(self) -> None:
        self.mutate(contract.BOOT_SETUP, "gpio_set_out 59 0 0", "gpio_set_out 59 1 0")
        self.assertTrue(any("fail-safe GPIO" in error for error in self.errors()))

    def test_uncataloged_dts_is_rejected(self) -> None:
        extra = self.root / "br2_external_dcentos/board/beaglebone/new-board/new.dts"
        extra.parent.mkdir(parents=True)
        extra.write_text("/dts-v1/;\n", encoding="utf-8")
        self.assertTrue(any("DTS inventory drift" in error for error in self.errors()))

    def test_packager_cannot_bypass_shared_dtb_contract(self) -> None:
        script = Path("scripts/build_am3_bb_sd_vnish_bootbin_image.sh")
        self.mutate(
            script,
            'dcent_am3_bb_admit_carrier_dtb "$DTB_SRC" vnish-btm "$ALLOW_STALE_KERNEL"',
            '# shared carrier admission accidentally removed',
        )
        self.assertTrue(any("admission call" in error for error in self.errors()))

    def test_artifact_directory_without_dtb_is_rejected_statically(self) -> None:
        script = Path("scripts/build_am3_bb_s19jpro.sh")
        self.mutate(script, '[ -n "$DTB_SOURCE" ] || {', 'if [ -n "$DTB_SOURCE" ]; then')
        self.assertTrue(any("--artifacts requires" in error for error in self.errors()))

    def test_docker_artifact_refusal_must_dominate_docker_invocation(self) -> None:
        script = Path("scripts/build_am3_bb_s19jpro.sh")
        self.mutate(
            script,
            'if [ -n "$ARTIFACT_DIR" ]; then',
            '# Docker artifact refusal accidentally removed',
        )
        self.assertTrue(any("must dominate" in error for error in self.errors()))

    def test_packager_cannot_duplicate_carrier_marker_parser(self) -> None:
        path = self.root / "scripts/build_am3_bb_s19jpro.sh"
        with path.open("a", encoding="utf-8") as handle:
            handle.write("\n# if grep -a -q 'S19J_IO_BOARD' duplicate.dtb; then :; fi\n")
        self.assertTrue(any("centralized" in error for error in self.errors()))

    @unittest.skipUnless(shutil.which("sh"), "POSIX sh is unavailable on this host")
    def test_shared_helper_rejects_unknown_and_accepts_carrier_marker(self) -> None:
        helper = self.root / "scripts/lib/am3_bb_dtb_contract.sh"
        unknown = self.root / "unknown.dtb"
        s19j = self.root / "s19j.dtb"
        btm = self.root / "btm.dtb"
        text_marker = self.root / "marker-only.dtb"
        appended_marker = self.root / "appended-marker.dtb"

        def fake_fdt(payload: bytes) -> bytes:
            body = bytes.fromhex("d00dfeed") + b"\x00\x00\x00\x00" + (b"\x00" * 32) + payload
            return body[:4] + len(body).to_bytes(4, "big") + body[8:]

        unknown.write_bytes(fake_fdt(b"ti,am335x-bone-black\x00"))
        s19j.write_bytes(fake_fdt(b"S19J_IO_BOARD_V2_0\x00"))
        btm.write_bytes(fake_fdt(b"am335x-boneblack-btm\x00"))
        text_marker.write_bytes(b"S19J_IO_BOARD_V2_0\x00")
        appended_marker.write_bytes(fake_fdt(b"ti,am335x-bone-black\x00") + b"S19J_IO_BOARD_V2_0\x00")
        command = '. "$1"; dcent_am3_bb_admit_carrier_dtb "$2" "$3" 0'
        rejected = subprocess.run(
            ["sh", "-c", command, "contract-test", str(helper), str(unknown), "s19j-io-v2"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        accepted = subprocess.run(
            ["sh", "-c", command, "contract-test", str(helper), str(s19j), "s19j-io-v2"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertNotEqual(0, rejected.returncode)
        self.assertEqual(0, accepted.returncode)
        wrong_lineage = subprocess.run(
            ["sh", "-c", command, "contract-test", str(helper), str(btm), "s19j-io-v2"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        vnish_lineage = subprocess.run(
            ["sh", "-c", command, "contract-test", str(helper), str(btm), "vnish-btm"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertNotEqual(0, wrong_lineage.returncode)
        self.assertEqual(0, vnish_lineage.returncode)
        marker_only = subprocess.run(
            ["sh", "-c", command, "contract-test", str(helper), str(text_marker), "s19j-io-v2"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertNotEqual(0, marker_only.returncode)
        appended = subprocess.run(
            ["sh", "-c", command, "contract-test", str(helper), str(appended_marker), "s19j-io-v2"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertNotEqual(0, appended.returncode)

    @unittest.skipUnless(shutil.which("sh"), "POSIX sh is unavailable on this host")
    def test_docker_artifacts_fail_before_docker_is_invoked(self) -> None:
        fake_bin = self.root / "fake-bin"
        fake_bin.mkdir()
        sentinel = self.root / "docker-was-invoked"
        docker = fake_bin / "docker"
        docker.write_text(
            "#!/bin/sh\nprintf invoked > \"$DCENT_TEST_DOCKER_SENTINEL\"\nexit 99\n",
            encoding="utf-8",
        )
        docker.chmod(0o755)
        artifacts = self.root / "artifacts"
        artifacts.mkdir()
        output = self.root / "payload.tar"
        env = __import__("os").environ.copy()
        env["PATH"] = str(fake_bin) + __import__("os").pathsep + env.get("PATH", "")
        env["BUILDROOT_DIR"] = str(self.root / "missing-buildroot")
        env["DCENT_TEST_DOCKER_SENTINEL"] = str(sentinel)
        result = subprocess.run(
            [
                "sh",
                str(self.root / "scripts/build_am3_bb_s19jpro.sh"),
                "--output",
                str(output),
                "--artifacts",
                str(artifacts),
            ],
            cwd=str(self.root),
            env=env,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertNotEqual(0, result.returncode)
        self.assertIn(b"--artifacts is not supported by Docker packaging", result.stderr)
        self.assertFalse(sentinel.exists(), "Docker must not run before artifact refusal")

    def test_missing_catalog_key_returns_error_not_exception(self) -> None:
        self.rewrite_catalog(lambda data: data.pop("gpio"))
        self.assertTrue(any("catalog gpio" in error for error in self.errors()))

    def test_uart_array_truncation_is_rejected_before_zip(self) -> None:
        self.rewrite_catalog(lambda data: data["uart"].__setitem__("base_addresses", ["0x48022000"]))
        self.assertTrue(any("UART device/base arrays" in error for error in self.errors()))


if __name__ == "__main__":
    unittest.main()
