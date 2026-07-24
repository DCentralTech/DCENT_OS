#!/usr/bin/env python3
"""Fail-closed proof that standalone controller tools are diagnostic-only.

This test intentionally checks Cargo's resolved package model as well as the
production sources.  A source-only grep would miss a reintroduced build script,
dependency, or workspace-default route that makes a historical mutation binary
part of normal release output.
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import shutil
import subprocess
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
WORKSPACE = ROOT / "dcentrald"
PACKAGE = WORKSPACE / "pic-recovery"
FABRIC_PACKAGE = WORKSPACE / "dcentrald-fabric-lease"
PIC_SOURCE = PACKAGE / "src/main.rs"
DSPIC_SOURCE = PACKAGE / "src/dspic_flash_main.rs"
BUILD_DRIVER = ROOT / "scripts/build-dcentrald.sh"
IMAGE_DRIVER = ROOT / "scripts/build_in_docker.sh"
DASHBOARDS = (
    ROOT / "br2_external_dcentos/board/zynq/rootfs-overlay/root/web/server.py",
    ROOT / "br2_external_dcentos/board/amlogic/rootfs-overlay/root/web/server.py",
)
RUST_TOOLCHAIN = "1.90.0"


def read(path: pathlib.Path) -> str:
    return path.read_text(encoding="utf-8")


def production_source(path: pathlib.Path) -> str:
    """Return code compiled outside the crate's host-only test module."""

    return read(path).split("#[cfg(test)]", 1)[0]


def without_rust_comments(source: str) -> str:
    source = re.sub(r"(?m)^\s*//.*$", "", source)
    return re.sub(r"(?s)/\*.*?\*/", "", source)


def cargo_command() -> list[str]:
    """Resolve the pinned Cargo without accepting a Windows PATH impostor."""

    configured = os.environ.get("CARGO")
    if configured:
        return [configured]
    rustup = shutil.which("rustup")
    if rustup is None:
        candidate = pathlib.Path.home() / ".cargo" / "bin" / "rustup"
        if candidate.is_file() and os.access(candidate, os.X_OK):
            rustup = str(candidate)
    if rustup is not None:
        return [rustup, "run", RUST_TOOLCHAIN, "cargo"]
    cargo = shutil.which("cargo")
    if cargo is None:
        raise RuntimeError("neither rustup nor cargo is available")
    return [cargo, f"+{RUST_TOOLCHAIN}"]


def cargo_metadata() -> dict[str, object]:
    completed = subprocess.run(
        cargo_command()
        + [
            "metadata",
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ],
        cwd=WORKSPACE,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return json.loads(completed.stdout)


class ControllerDiagnosticBoundary(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.metadata = cargo_metadata()
        cls.packages = {
            package["id"]: package for package in cls.metadata["packages"]
        }

    def package_names(self, member_ids: list[str]) -> set[str]:
        return {self.packages[member_id]["name"] for member_id in member_ids}

    def test_controller_package_is_explicitly_opt_in(self) -> None:
        members = self.package_names(self.metadata["workspace_members"])
        defaults = self.package_names(self.metadata["workspace_default_members"])
        self.assertIn("pic-recovery", members)
        self.assertEqual(members - defaults, {"pic-recovery"})

        build_commands = read(BUILD_DRIVER)
        self.assertIsNone(
            re.search(
                r"\bcargo(?:\s+\+\S+)?\s+build[^\n]*(?:--workspace|--all(?:\s|$))",
                build_commands,
            ),
            "release Cargo commands must honor workspace default-members",
        )

    def test_controller_package_depends_only_on_libc_and_the_leaf_lease(self) -> None:
        package = next(
            package
            for package in self.metadata["packages"]
            if package["name"] == "pic-recovery"
        )
        self.assertEqual(
            {
                (dependency["name"], tuple(dependency["features"]))
                for dependency in package["dependencies"]
            },
            {("dcentrald-fabric-lease", ()), ("libc", ())},
        )
        self.assertEqual(
            {(target["name"], tuple(target["kind"])) for target in package["targets"]},
            {("pic-recovery", ("bin",)), ("dspic-flash", ("bin",))},
        )
        self.assertFalse((PACKAGE / "build.rs").exists())

        lease = next(
            package
            for package in self.metadata["packages"]
            if package["name"] == "dcentrald-fabric-lease"
        )
        self.assertEqual(
            {
                (dependency["name"], tuple(dependency["features"]))
                for dependency in lease["dependencies"]
            },
            {("libc", ())},
            "the shared lease must remain a leaf instead of importing HAL mutation surfaces",
        )
        self.assertEqual(
            {(target["name"], tuple(target["kind"])) for target in lease["targets"]},
            {("dcentrald_fabric_lease", ("lib",))},
        )
        self.assertFalse((FABRIC_PACKAGE / "build.rs").exists())

    def test_pic16_inspector_has_one_fixed_read_only_device_route(self) -> None:
        source = production_source(PIC_SOURCE)
        libc_calls = set(re.findall(r"\blibc::([A-Za-z_]\w*)\s*\(", source))
        filesystem_calls = set(re.findall(r"\bfs::([A-Za-z_]\w*)\s*\(", source))
        self.assertEqual(libc_calls, {"open", "ioctl", "read", "close"})
        self.assertEqual(filesystem_calls, {"read", "read_dir", "read_to_string"})
        self.assertEqual(source.count('const I2C_PATH: &str = "/dev/i2c-0";'), 1)
        self.assertIn("libc::O_RDONLY | libc::O_CLOEXEC", source)
        self.assertIn("I2C_SLAVE", source)

        code = without_rust_comments(source)
        forbidden = {
            "forced I2C ownership": r"I2C_SLAVE_FORCE",
            "writable file descriptor": r"\bO_(?:WRONLY|RDWR|CREAT|APPEND|TRUNC)\b",
            "write syscall": r"\b(?:libc|fs)::write\s*\(",
            "generic file writer": r"\b(?:OpenOptions|File::create|write_all)\b",
            "subprocess": r"\b(?:std::process::Command|Command::new)\b",
            "embedded recovery artifact": r"\binclude_(?:bytes|str)!",
            "external source module": r"(?m)^\s*(?:pub\s+)?mod\s+[A-Za-z_]\w*\s*;",
            "mutation executor": r"\b(?:erase|program|reflash|reset_pic|jump_to_app|set_voltage|enable_dc_dc)\s*\(",
        }
        for boundary, pattern in forbidden.items():
            with self.subTest(boundary=boundary):
                self.assertIsNone(re.search(pattern, code), boundary)

        for command in (
            'command == "pic16-recover"',
            'command == "--fpga-flash"',
            'command.starts_with("pic1704-")',
            'command.starts_with("dspic-")',
        ):
            self.assertIn(command, source)

    def test_dspic_status_command_cannot_touch_hardware(self) -> None:
        source = production_source(DSPIC_SOURCE)
        for forbidden in (
            r"\blibc::",
            r"\b(?:std::)?fs::",
            r"/dev/",
            r"\b(?:std::process::Command|Command::new)\b",
            r"\b(?:std::net|TcpStream|UdpSocket)\b",
            r"\binclude_(?:bytes|str)!",
            r"(?m)^\s*(?:pub\s+)?mod\s+[A-Za-z_]\w*\s*;",
        ):
            self.assertIsNone(re.search(forbidden, source), forbidden)
        self.assertIn('eprintln!("Usage: dspic-flash status")', source)
        self.assertIn("every_historical_hardware_command_is_refused", read(DSPIC_SOURCE))

    def test_dashboards_do_not_bypass_daemon_controller_ownership(self) -> None:
        forbidden = re.compile(
            r"dspic-flash|pic-recovery|proto-probe|get_dspic_probe|"
            r"i2c(?:get|set)[^\n]*(?:0x55|0x56|0x57)",
            re.IGNORECASE,
        )
        for dashboard in DASHBOARDS:
            with self.subTest(dashboard=dashboard):
                self.assertIsNone(forbidden.search(read(dashboard)))

    def test_historical_binaries_are_purge_only(self) -> None:
        image_driver = read(IMAGE_DRIVER)
        match = re.search(r'ALL_PREBUILT_BINARIES="([^"]+)"', image_driver)
        self.assertIsNotNone(match)
        self.assertEqual(
            set(match.group(1).split()),
            {
                "dcentrald",
                "dcentos-init",
                "dcentos-discovery",
                "pic-recovery",
                "dspic-flash",
            },
        )
        required = image_driver.split(
            "dcent_required_prebuilt_binaries() {", 1
        )[1].split("\n}", 1)[0]
        self.assertNotIn("pic-recovery", required)
        self.assertNotIn("dspic-flash", required)


if __name__ == "__main__":
    unittest.main(verbosity=2)
