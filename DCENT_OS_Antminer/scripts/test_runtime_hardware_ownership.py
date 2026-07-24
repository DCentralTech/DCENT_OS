#!/usr/bin/env python3
"""Regression tests for the normal-runtime hardware ownership boundary.

The mining runtime owns physical I2C/UART/UIO/devmem transports. Long-lived
web adapters, post-boot verification, normal REST handlers, and packaged
research tools must not create parallel owners. These tests deliberately pin
both source shape and final Buildroot composition.
"""

from __future__ import annotations

import ast
import os
import re
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RAW_I2C_COMMAND = re.compile(r"\b(?:i2cget|i2cset|i2cdetect|i2cdump|i2ctransfer)\b")

WEB_ADAPTERS = (
    ROOT / "br2_external_dcentos/board/zynq/rootfs-overlay/root/web/server.py",
    ROOT / "br2_external_dcentos/board/zynq/rootfs-overlay/root/web/ip_reporter.py",
    ROOT / "br2_external_dcentos/board/zynq/rootfs-overlay/root/web/mcp_server.py",
    ROOT / "br2_external_dcentos/board/amlogic/rootfs-overlay/root/web/server.py",
    ROOT / "br2_external_dcentos/board/amlogic/rootfs-overlay/root/web/ip_reporter.py",
    ROOT / "br2_external_dcentos/board/amlogic/rootfs-overlay/root/web/mcp_server.py",
)

S99_VERIFY_COPIES = (
    ROOT / "br2_external_dcentos/board/zynq/rootfs-overlay/etc/init.d/S99verify",
    ROOT / "br2_external_dcentos/board/amlogic/rootfs-overlay/etc/init.d/S99verify",
    ROOT
    / "br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/rootfs-overlay/etc/init.d/S99verify",
    ROOT
    / "br2_external_dcentos/board/cvitek/cv1835-s19jpro/rootfs-overlay/etc/init.d/S99verify",
)

PRUNE = (
    ROOT / "br2_external_dcentos/board/common/prune-runtime-research-tools.sh"
)
TARGET_SWITCH_FIRMWARE = (
    ROOT
    / "br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/switch_firmware.py"
)
TARGET_SWITCH_FIRMWARE_SH = TARGET_SWITCH_FIRMWARE.with_suffix(".sh")
HOST_SWITCH_FIRMWARE = ROOT / "scripts/switch_firmware.py"
HOST_SWITCH_FIRMWARE_SH = ROOT / "scripts/switch_firmware.sh"
REST_LATE = ROOT / "dcentrald/dcentrald-api/src/rest/late.rs"
REST_ROUTES = ROOT / "dcentrald/dcentrald-api/src/rest.rs"
HAL_I2C = ROOT / "dcentrald/dcentrald-hal/src/i2c.rs"
HARDWARE_INFO = ROOT / "dcentrald/dcentrald/src/runtime/hardware_info.rs"
S19J_HYBRID = ROOT / "dcentrald/dcentrald/src/s19j_hybrid_mining.rs"
AMLOGIC_HAL = ROOT / "dcentrald/dcentrald-hal/src/platform/amlogic/mod.rs"
SERIAL_MINING = ROOT / "dcentrald/dcentrald/src/serial_mining.rs"
DAEMON = ROOT / "dcentrald/dcentrald/src/daemon.rs"


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def python_function(source: str, name: str) -> str:
    tree = ast.parse(source)
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name == name:
            segment = ast.get_source_segment(source, node)
            if segment is None:
                raise AssertionError(f"could not recover Python function {name}")
            return segment
    raise AssertionError(f"Python function {name} not found")


def rust_function(source: str, name: str) -> tuple[str, int, int]:
    match = re.search(rf"\b(?:async\s+)?fn\s+{re.escape(name)}\s*\(", source)
    if match is None:
        raise AssertionError(f"Rust function {name} not found")
    start = match.start()
    brace = source.find("{", match.end())
    if brace < 0:
        raise AssertionError(f"Rust function {name} has no body")
    depth = 0
    for index in range(brace, len(source)):
        char = source[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1], start, index + 1
    raise AssertionError(f"Rust function {name} has an unterminated body")


class RuntimeHardwareOwnershipTests(unittest.TestCase):
    def test_amlogic_power_and_thermal_share_one_retained_bus1_owner(self) -> None:
        hal = read(AMLOGIC_HAL)
        serial = read(SERIAL_MINING)
        daemon = read(DAEMON)
        rest = read(REST_LATE)

        self.assertIn("pub struct AmlogicPowerThermalService", hal)
        self.assertIn(
            "spawn_i2c_service_no_register_touch_with_denylist_and_reserved_preparation",
            hal,
        )
        self.assertIn("read_lm75_temperature_register_at", hal)
        self.assertNotIn("pub fn read_board_temps(", hal)
        self.assertNotIn("pub fn enable_psu_pmbus(", hal)
        self.assertNotIn("I2cBus::open(APW_PMBUS", hal)
        self.assertNotIn("classify_with_probe", hal)
        self.assertIn("generic Platform construction is refused", hal)
        self.assertIn("pub struct AmlogicNoPicAdmission", hal)
        self.assertIn("nopic_profile_for_bosminer_toml", hal)
        self.assertIn("nopic_profile_for_dcentos_marker", hal)
        self.assertIn('nopic_profile_for_dcentos_marker("am3-aml"), None', hal)
        self.assertIsNone(re.search(r"(?m)^pub fn open_fan_controller\(\)", hal))
        self.assertIsNone(re.search(r"(?m)^pub fn spawn\(\) -> Result<Self>", hal))

        admission = serial.index("AmlogicNoPicAdmission::detect(")
        bm1366_refusal = serial.index("native BM1366 NoPic mining is refused")
        service = serial.index(".spawn_power_thermal_service()")
        fan = serial.index(".open_fan_controller()")
        watchdog = serial.index("SafetyWatchdogOwner::start_before_energizing")
        enable = serial.index(
            "tokio::task::spawn_blocking(move || power_enable_owner.enable_psu())"
        )
        self.assertLess(bm1366_refusal, admission)
        self.assertLess(admission, service)
        self.assertLess(service, fan)
        self.assertLess(fan, watchdog)
        self.assertLess(watchdog, enable)
        self.assertIn("power_thermal.terminal_fence()", serial)
        self.assertIn("power_receipt.management_fabric()", serial)
        self.assertIn("WatchdogDisarmPermit::from_evidence_set", serial)
        self.assertIn("fence.latch_terminal_safe_off();", serial)
        self.assertNotIn("amlogic::read_board_temps(", serial)
        self.assertNotIn("amlogic::AmlogicPlatform::new()", serial)

        self.assertNotIn("platform::amlogic::enable_psu()", daemon)
        self.assertIn("does not own the retained Amlogic power/thermal service", daemon)
        recovery_psu, _, _ = rust_function(rest, "post_debug_psu_control_recovery")
        self.assertNotIn("platform::amlogic::enable_psu()", recovery_psu)
        self.assertNotIn("platform::amlogic::disable_psu()", recovery_psu)
        self.assertIn("retained power/thermal owner", recovery_psu)

    def test_hybrid_eeprom_gate_stays_inside_owned_i2c_service(self) -> None:
        hal = read(HAL_I2C)
        hardware_info = read(HARDWARE_INFO)
        hybrid = read(S19J_HYBRID)

        service_reader, _, _ = rust_function(
            hardware_info,
            "read_hashboard_eeprom_prefix_via_service_for_energize_gate",
        )
        hal_reader, _, _ = rust_function(
            hal, "read_protected_hashboard_eeprom_prefix"
        )
        self.assertIn("0x50u8", service_reader)
        self.assertIn(
            "service.read_hashboard_eeprom_prefix_at(addr, deadline)", service_reader
        )
        self.assertIn("is_retryable_owned_eeprom_readiness_error", service_reader)
        self.assertIn("OwnedEepromReadinessError::Terminal", service_reader)
        self.assertIn("HalError::I2cEndpointNotReady", hardware_info)
        self.assertNotIn("std::fs", service_reader)
        self.assertNotIn("/sys/bus/i2c", service_reader)
        self.assertIn("if !self.is_write_denied(addr)", hal_reader)
        self.assertIn("fs::File::open(&path)", hal_reader)
        self.assertNotIn("set_slave", hal_reader)
        self.assertNotIn("I2C_SLAVE", hal_reader)

        run = hybrid.split("pub async fn run(&mut self)", 1)[1]
        run = run.split("fn log_am2_planned_chain_contexts", 1)[0]
        self.assertEqual(
            run.count("read_hashboard_eeprom_prefix_via_service_for_energize_gate("),
            1,
        )
        self.assertEqual(run.count("read_hashboard_eeprom_prefix_at("), 1)
        self.assertNotIn("read_bytes(0x50", run)
        self.assertNotIn("read_hashboard_eeprom_for_energize_gate(", run)
        self.assertIn("HalError::I2cEndpointNotReady", run)
        self.assertIn(
            "AM2 hashboard EEPROM bootstrap service failed terminally", run
        )
        self.assertIn("OwnedEepromReadinessError::Terminal", run)
        self.assertIn("am2-hashboard-eeprom-service-terminal", run)
        self.assertIn(
            'context("AM2 I2C service missing for serialized hashboard identity reads")',
            run,
        )

        self.assertIn("pub fn read_hashboard_eeprom_prefix_at(", hal)
        self.assertIn("I2cRequest::ReadHashboardEepromPrefix", hal)
        self.assertIn("read_protected_hashboard_eeprom_prefix", hal)
        self.assertNotIn("WriteReadProtection", hal)

    def test_web_adapters_have_no_raw_hardware_command_path(self) -> None:
        for path in WEB_ADAPTERS:
            with self.subTest(path=path):
                source = read(path)
                self.assertIsNone(RAW_I2C_COMMAND.search(source))

        for path in (WEB_ADAPTERS[0], WEB_ADAPTERS[3]):
            source = read(path)
            self.assertNotIn("_refresh_i2c_temps", source)
            self.assertNotIn("threading.Thread", source)
            self.assertNotIn("devmem", source)
            self.assertNotIn("/sys/class/gpio/export", source)
            self.assertNotIn("dcentrald_fan_cmd", source)
            self.assertNotIn("--get-fan", source)
            self.assertNotIn("--set-fan", source)

        fail_closed_tools = (
            "tool_get_fpga_registers",
            "tool_read_devmem",
            "tool_write_fpga_register",
            "tool_write_devmem",
            "tool_gpio_read",
            "tool_gpio_write",
            "tool_board_control",
            "tool_set_fan_speed",
        )
        for path in WEB_ADAPTERS[2::3]:
            source = read(path)
            for name in fail_closed_tools:
                with self.subTest(path=path, function=name):
                    body = python_function(source, name)
                    self.assertIn("_raw_hardware_unavailable", body)
                    self.assertNotIn("subprocess", body)
                    self.assertNotIn("run_cmd", body)
                    self.assertNotIn("open(", body)
            fan_read = python_function(source, "tool_get_fan_speed")
            self.assertIn("tool_live_stats", fan_read)
            self.assertIn("dcentrald /api/status", fan_read)
            self.assertNotIn("/sys/class/gpio/export", source)
            self.assertNotRegex(source, r"run_cmd\([^\n]*(?:devmem|/dev/(?:mem|uio|tty|i2c))")

        zynq_mcp = read(WEB_ADAPTERS[2])
        uart = python_function(zynq_mcp, "tool_capture_chain_uart_bytes")
        self.assertIn("_raw_hardware_unavailable", uart)
        self.assertNotIn("open(", uart)

    def test_s99_is_a_fail_closed_snapshot_consumer_and_never_commits(self) -> None:
        mutation_command = re.compile(
            r"(?m)^\s*(?:fw_setenv|nandwrite|flash_erase)(?:\s|$)"
        )
        for path in S99_VERIFY_COPIES:
            with self.subTest(path=path):
                source = read(path)
                self.assertIsNone(RAW_I2C_COMMAND.search(source))
                self.assertIsNone(mutation_command.search(source))
                self.assertNotIn('"$DAEMON" --get-fan', source)
                self.assertIn("http://127.0.0.1:8080/api/status", source)
                self.assertIn("raw fallback is prohibited", source)
                self.assertIn("S99verify is a report-only proof consumer", source)
                self.assertIn("UPGRADE_COMMIT_MARKER", source)
                self.assertIn("upgrade-commit-state", source)
                self.assertIn(
                    'emit_check V12 false "am3-aml: state.json missing', source
                )
                self.assertIn(
                    'emit_check V13 false "no daemon-owned $evt counter', source
                )

    def test_rest_normal_routes_are_safe_and_recovery_bodies_are_unmounted(self) -> None:
        source = read(REST_LATE)
        routes = read(REST_ROUTES)
        cargo = read(ROOT / "dcentrald/dcentrald-api/Cargo.toml")
        self.assertIn("recovery-tool = []", cargo)
        self.assertNotIn('Command::new("i2ctransfer")', source)
        self.assertNotIn("PsuController::open_kernel_only", source)

        normal_handlers = (
            "post_offgrid_test",
            "get_debug_registers",
            "post_debug_psu_control",
            "get_diag_fpga",
        )
        for name in normal_handlers:
            with self.subTest(handler=name):
                body, _, _ = rust_function(source, name)
                self.assertNotIn('Command::new("devmem")', body)
                self.assertNotIn("create_voltage_source", body)
                self.assertNotIn("AmlogicPsu", body)
                self.assertNotIn("ZynqPsu", body)
                self.assertIn("hardware_access_attempted", body)

        recovery_handlers = (
            "post_offgrid_test_recovery",
            "get_debug_registers_recovery",
            "post_debug_psu_control_recovery",
            "get_diag_fpga_recovery",
        )
        masked = list(source)
        for name in recovery_handlers:
            body, start, end = rust_function(source, name)
            prefix = source[max(0, start - 100) : start]
            self.assertIn('#[cfg(feature = "recovery-tool")]', prefix)
            self.assertNotIn(name, routes)
            masked[start:end] = " " * (end - start)

        normal_source = "".join(masked)
        self.assertNotIn('Command::new("devmem")', normal_source)
        self.assertNotIn("create_voltage_source", normal_source)

        fan_write, _, _ = rust_function(source, "set_fan_pwm_via_hal")
        fan_read, _, _ = rust_function(source, "read_fan_via_hal")
        self.assertIn("serialized command broker", fan_write)
        self.assertIn("runtime telemetry snapshot", fan_read)
        self.assertNotIn("FanController::open", fan_write + fan_read)

    def test_every_product_runs_prune_as_the_final_post_build_stage(self) -> None:
        configs = sorted((ROOT / "br2_external_dcentos/configs").glob("*_defconfig"))
        product_configs = [path for path in configs if "BR2_ROOTFS_OVERLAY=" in read(path)]
        self.assertTrue(product_configs)
        for path in product_configs:
            with self.subTest(path=path):
                post_build = next(
                    (
                        line
                        for line in read(path).splitlines()
                        if line.startswith("BR2_ROOTFS_POST_BUILD_SCRIPT=")
                    ),
                    "",
                )
                self.assertRegex(
                    post_build,
                    r"board/common/prune-runtime-research-tools\.sh\"$",
                )

        common = read(ROOT / "br2_external_dcentos/configs/dcentos-common.fragment")
        cv_defconfig = (
            ROOT
            / "br2_external_dcentos/configs/dcentos_cv1835_s19jpro_defconfig"
        )
        self.assertIn("# BR2_PACKAGE_I2C_TOOLS is not set", common)
        self.assertNotIn("BR2_PACKAGE_I2C_TOOLS=y", common)
        self.assertFalse(
            cv_defconfig.exists(),
            "CV1835 must not regain a Buildroot product lane before its exact toolchain and containment contract are admitted",
        )

        prune = read(PRUNE)
        self.assertIn('TARGET_DIR=$(CDPATH= cd "$TARGET_DIR" && pwd -P)', prune)
        self.assertIn("Buildroot rootfs sentinels", prune)
        self.assertIn("symlinked delete-path component", prune)
        self.assertIn('rm -rf "$TARGET_DIR/root/tools"', prune)
        self.assertIn('rm -f "$TARGET_DIR/usr/bin/dcent-shell"', prune)
        self.assertIn("root usr usr/bin usr/sbin", prune)
        self.assertIn('"$TARGET_DIR/usr/sbin/switch_firmware.py"', prune)
        self.assertIn('"$TARGET_DIR/usr/sbin/switch_firmware.sh"', prune)

    def test_raw_boot_environment_transformers_are_host_only(self) -> None:
        self.assertFalse(TARGET_SWITCH_FIRMWARE.exists())
        self.assertFalse(TARGET_SWITCH_FIRMWARE_SH.exists())
        self.assertTrue(HOST_SWITCH_FIRMWARE.is_file())
        self.assertTrue(HOST_SWITCH_FIRMWARE_SH.is_file())

        post_builds = sorted(
            (ROOT / "br2_external_dcentos/board").rglob("post-build.sh")
        )
        self.assertTrue(post_builds)
        for post_build in post_builds:
            with self.subTest(post_build=post_build):
                self.assertNotIn("switch_firmware", read(post_build))

    @unittest.skipUnless(os.name == "posix" and shutil.which("sh"), "requires POSIX sh")
    def test_prune_composes_a_safe_rootfs_and_rejects_symlink_escape(self) -> None:
        self.assertTrue(PRUNE.stat().st_mode & stat.S_IXUSR)
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            target = base / "target"
            for directory in (
                target / "etc",
                target / "bin",
                target / "usr/bin",
                target / "usr/sbin",
                target / "root/tools/__pycache__",
            ):
                directory.mkdir(parents=True, exist_ok=True)
            (target / "root/tools/future_raw_executor").write_text("raw\n")
            (target / "root/tools/__pycache__/raw.pyc").write_bytes(b"bytecode")
            (target / "usr/bin/dcent-shell").write_text("shell\n")
            (target / "usr/sbin/switch_firmware.py").write_text("raw\n")
            (target / "usr/sbin/switch_firmware.sh").write_text("raw\n")
            (target / "etc/preserved").write_text("safe\n")

            subprocess.run(["sh", str(PRUNE), str(target)], check=True)
            self.assertFalse((target / "root/tools").exists())
            self.assertFalse((target / "usr/bin/dcent-shell").exists())
            self.assertFalse((target / "usr/sbin/switch_firmware.py").exists())
            self.assertFalse((target / "usr/sbin/switch_firmware.sh").exists())
            self.assertEqual((target / "etc/preserved").read_text(), "safe\n")

            root_result = subprocess.run(
                ["sh", str(PRUNE), "/"], capture_output=True, text=True, check=False
            )
            self.assertNotEqual(root_result.returncode, 0)

            outside = base / "outside"
            outside.mkdir()
            (outside / "keeper").write_text("keep\n")
            unsafe = base / "unsafe"
            (unsafe / "etc").mkdir(parents=True)
            (unsafe / "bin").mkdir()
            (unsafe / "usr/bin").mkdir(parents=True)
            os.symlink(outside, unsafe / "root")
            escape_result = subprocess.run(
                ["sh", str(PRUNE), str(unsafe)],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertNotEqual(escape_result.returncode, 0)
            self.assertEqual((outside / "keeper").read_text(), "keep\n")

    def test_runtime_package_and_shell_do_not_reinstall_raw_tools(self) -> None:
        package = read(ROOT / "br2_external_dcentos/packages/dcentos-tools/dcentos-tools.mk")
        self.assertNotIn("$(@D)/*.py", package)
        self.assertNotIn("$(@D)/*.sh", package)
        self.assertNotIn("usr/bin/dcent-shell", package)

        shell = read(ROOT / "br2_external_dcentos/board/zynq/rootfs-overlay/etc/bash.bashrc")
        profile = read(ROOT / "br2_external_dcentos/board/zynq/rootfs-overlay/root/.profile")
        self.assertIsNone(RAW_I2C_COMMAND.search(shell + profile))
        self.assertNotIn("devmem", shell + profile)
        self.assertNotIn("/dev/tty", shell + profile)
        self.assertNotIn("/root/tools", profile)
        self.assertIn("miner-status", shell)


if __name__ == "__main__":
    unittest.main()
