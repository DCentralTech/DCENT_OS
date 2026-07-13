#!/usr/bin/env python3
"""Fail closed when AM3-BB board, boot, and legacy-DTS contracts drift."""

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any, Dict, List, Sequence


DEFAULT_CATALOG = Path("etc/board_target/am3-bb-s19jpro.hardware-contract.json")
TARGET_TOML = Path("etc/board_target/am3-bb-s19jpro.toml")
HAL = Path("dcentrald/dcentrald-hal/src/platform/beaglebone.rs")
BOOT_SETUP = Path(
    "br2_external_dcentos/board/beaglebone/am3-bb/rootfs-overlay/etc/init.d/S37board_setup"
)
DAEMON_INIT = Path(
    "br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/"
    "rootfs-overlay/etc/init.d/S82dcentrald"
)
POST_IMAGE = Path(
    "br2_external_dcentos/board/beaglebone/am3-bb-s19jpro/post-image.sh"
)
DOCKER_BUILD = Path("scripts/build_in_docker.sh")


def read_text(root: Path, relative: Path, errors: List[str]) -> str:
    path = root / relative
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except OSError as exc:
        errors.append("missing/unreadable {}: {}".format(relative.as_posix(), exc))
        return ""


def normalized(text: str) -> str:
    return " ".join(text.split())


def require_literal(text: str, value: str, label: str, errors: List[str]) -> None:
    if value not in text:
        errors.append("{}: missing {!r}".format(label, value))


def reject_literal(text: str, value: str, label: str, errors: List[str]) -> None:
    if value in text:
        errors.append("{}: forbidden {!r}".format(label, value))


def toml_string(text: str, key: str) -> str:
    match = re.search(r"(?m)^\s*{}\s*=\s*\"([^\"]+)\"".format(re.escape(key)), text)
    return match.group(1) if match else ""


def toml_int(text: str, key: str) -> int:
    match = re.search(r"(?m)^\s*{}\s*=\s*(0x[0-9a-fA-F]+|\d+)".format(re.escape(key)), text)
    return int(match.group(1), 0) if match else -1


def toml_int_array(text: str, key: str) -> List[int]:
    match = re.search(r"(?ms)^\s*{}\s*=\s*\[([^\]]*)\]".format(re.escape(key)), text)
    if not match:
        return []
    return [int(value, 0) for value in re.findall(r"0x[0-9a-fA-F]+|\d+", match.group(1))]


def relative_inventory(root: Path, pattern: str) -> List[str]:
    return sorted(path.relative_to(root).as_posix() for path in root.glob(pattern) if path.is_file())


def validate_catalog_shape(catalog: Any) -> List[str]:
    errors: List[str] = []
    if not isinstance(catalog, dict):
        return ["hardware-contract catalog root must be an object"]
    for key in ("evidence", "identity", "gpio", "uart", "inventory", "legacy_reference"):
        if not isinstance(catalog.get(key), dict):
            errors.append("hardware-contract catalog {} must be an object".format(key))
    if errors:
        return errors
    if not isinstance(catalog.get("target"), str) or not catalog["target"]:
        errors.append("hardware-contract catalog target must be a non-empty string")
    references = catalog["evidence"].get("lineage_references")
    if not isinstance(references, list) or not references or not all(isinstance(item, str) for item in references):
        errors.append("hardware-contract evidence.lineage_references must be a non-empty string array")
    for key in ("model", "compatible"):
        if not isinstance(catalog["identity"].get(key), str):
            errors.append("hardware-contract identity.{} must be a string".format(key))
    gpio = catalog["gpio"]
    for key in ("board_enable", "board_enable_safe_value", "asic_reset_safe_value"):
        if not isinstance(gpio.get(key), int):
            errors.append("hardware-contract gpio.{} must be an integer".format(key))
    if not isinstance(gpio.get("board_enable_active"), str):
        errors.append("hardware-contract gpio.board_enable_active must be a string")
    if not isinstance(gpio.get("asic_reset_active_low"), bool):
        errors.append("hardware-contract gpio.asic_reset_active_low must be a boolean")
    for key, length in (("asic_reset", 4), ("fan_tach", 4)):
        value = gpio.get(key)
        if not isinstance(value, list) or len(value) != length or not all(isinstance(item, int) for item in value):
            errors.append("hardware-contract gpio.{} must contain {} integers".format(key, length))
    uart = catalog["uart"]
    devices = uart.get("devices")
    addresses = uart.get("base_addresses")
    if not isinstance(devices, list) or not all(isinstance(item, str) for item in devices):
        errors.append("hardware-contract uart.devices must be a string array")
    if not isinstance(addresses, list) or not all(isinstance(item, str) for item in addresses):
        errors.append("hardware-contract uart.base_addresses must be a string array")
    if isinstance(devices, list) and isinstance(addresses, list):
        if len(devices) != 3 or len(addresses) != len(devices):
            errors.append("hardware-contract UART device/base arrays must have the same three entries")
    for key in ("live_pinmux", "legacy_reference_pinmux"):
        pinmux = uart.get(key)
        if not isinstance(pinmux, dict) or not pinmux:
            errors.append("hardware-contract uart.{} must be a non-empty object".format(key))
        elif not all(
            isinstance(values, list)
            and len(values) == 4
            and all(isinstance(item, str) for item in values)
            for values in pinmux.values()
        ):
            errors.append("hardware-contract uart.{} entries must contain four strings".format(key))
    inventory = catalog["inventory"]
    for key in ("dts_sources", "defconfigs", "product_build_scripts"):
        value = inventory.get(key)
        if not isinstance(value, list) or not value or not all(isinstance(item, str) for item in value):
            errors.append("hardware-contract inventory.{} must be a non-empty string array".format(key))
    if not isinstance(inventory.get("dtb_contract_helper"), str):
        errors.append("hardware-contract inventory.dtb_contract_helper must be a string")
    policies = inventory.get("dtb_policies")
    scripts = inventory.get("product_build_scripts")
    if not isinstance(policies, dict):
        errors.append("hardware-contract inventory.dtb_policies must be an object")
    elif isinstance(scripts, list):
        if set(policies) != set(scripts):
            errors.append("hardware-contract DTB policy keys must exactly match product build scripts")
        for path, policy in policies.items():
            if policy not in ("s19j-io-v2", "vnish-btm"):
                errors.append("hardware-contract DTB policy for {} is unsupported".format(path))
    legacy = catalog["legacy_reference"]
    if not isinstance(legacy.get("path"), str) or not isinstance(legacy.get("marker"), str):
        errors.append("hardware-contract legacy reference path/marker must be strings")
    legacy_gpio = legacy.get("legacy_reset_gpio")
    if not isinstance(legacy_gpio, list) or len(legacy_gpio) != 4 or not all(isinstance(item, int) for item in legacy_gpio):
        errors.append("hardware-contract legacy reset map must contain four integers")
    return errors


def validate_repository(root: Path, catalog_path: Path = DEFAULT_CATALOG) -> List[str]:
    root = root.resolve()
    errors: List[str] = []
    catalog_text = read_text(root, catalog_path, errors)
    if not catalog_text:
        return errors
    try:
        catalog: Dict[str, Any] = json.loads(catalog_text)
    except (TypeError, ValueError) as exc:
        return ["invalid hardware-contract catalog: {}".format(exc)]
    errors.extend(validate_catalog_shape(catalog))
    if catalog.get("schema_version") != 1:
        errors.append("hardware-contract catalog schema_version must be 1")
    if errors:
        return errors
    for reference in catalog["evidence"]["lineage_references"]:
        if not (root / reference).is_file():
            errors.append("hardware-contract lineage evidence is missing: {}".format(reference))

    inventory = catalog.get("inventory", {})
    expected_dts = sorted(inventory.get("dts_sources", []))
    actual_dts = sorted(
        relative_inventory(root, "br2_external_dcentos/board/beaglebone/**/*.dts")
        + relative_inventory(root, "br2_external_dcentos/board/beaglebone/**/*.dtsi")
    )
    if actual_dts != expected_dts:
        errors.append("AM3-BB DTS inventory drift: expected {}, found {}".format(expected_dts, actual_dts))
    expected_defconfigs = sorted(inventory.get("defconfigs", []))
    actual_defconfigs = relative_inventory(
        root, "br2_external_dcentos/configs/dcentos_am3_bb*_defconfig"
    )
    if actual_defconfigs != expected_defconfigs:
        errors.append(
            "AM3-BB defconfig inventory drift: expected {}, found {}".format(
                expected_defconfigs, actual_defconfigs
            )
        )

    toml = read_text(root, TARGET_TOML, errors)
    gpio = catalog["gpio"]
    identity = catalog["identity"]
    if toml_string(toml, "board_target") != catalog["target"]:
        errors.append("board-target TOML target does not match catalog")
    if toml_string(toml, "io_board") not in identity["model"]:
        errors.append("board-target TOML io_board does not match live-FDT model")
    if toml_int(toml, "board_enable") != gpio["board_enable"]:
        errors.append("board-target TOML board_enable does not match catalog")
    if toml_string(toml, "board_enable_active") != gpio["board_enable_active"]:
        errors.append("board-target TOML board-enable polarity does not match catalog")
    if toml_int_array(toml, "asic_rst") != gpio["asic_reset"]:
        errors.append("board-target TOML ASIC reset GPIOs do not match catalog")
    if toml_int_array(toml, "fan_tach") != gpio["fan_tach"]:
        errors.append("board-target TOML fan-tach GPIOs do not match catalog")
    for device, address in zip(catalog["uart"]["devices"], catalog["uart"]["base_addresses"]):
        require_literal(toml, device, "board-target UART device", errors)
        require_literal(toml.lower(), address.lower(), "board-target UART base", errors)

    hal = read_text(root, HAL, errors)
    require_literal(hal, "GPIO_BOARD_ENABLE_V2_0: u32 = 59", "HAL board-enable", errors)
    require_literal(hal, "GPIO_ASIC_RST_V2_0: [u32; 4] = [49, 60, 27, 22]", "HAL reset map", errors)
    require_literal(hal, "GPIO_FAN_TACH_V2_0: [u32; 4] = [7, 20, 110, 112]", "HAL fan-tach map", errors)
    require_literal(hal, identity["model"], "HAL exact DT model", errors)
    require_literal(hal, identity["compatible"], "HAL exact DT compatible", errors)
    for device in catalog["uart"]["devices"]:
        require_literal(hal, device, "HAL UART map", errors)

    safe_contract = "for gpio in 49 60 27 22; do gpio_set_out \"$gpio\" 1 1 done gpio_set_out 59 0 0"
    boot = normalized(read_text(root, BOOT_SETUP, errors))
    require_literal(boot, safe_contract, "S37 fail-safe GPIO contract", errors)
    require_literal(boot, "echo 10000 > \"$pwm/duty_cycle\"", "S37 10% PWM default", errors)
    daemon = normalized(read_text(root, DAEMON_INIT, errors))
    daemon_contract = "for g in 49 60 27 22; do gpio_set_out \"$g\" 1 1 done"
    require_literal(daemon, daemon_contract, "S82 reset safety override", errors)
    require_literal(daemon, "gpio_set_out 59 0 0", "S82 board-enable safety override", errors)

    legacy = catalog["legacy_reference"]
    legacy_path = Path(legacy["path"])
    dts = read_text(root, legacy_path, errors)
    require_literal(dts, legacy["marker"], "legacy DTS quarantine", errors)
    reset_offsets: List[int] = []
    for index in range(4):
        match = re.search(
            r"rst{}\s*\{{.*?gpios\s*=\s*<&gpio\d\s+(\d+)\s+1>".format(index),
            dts,
            flags=re.DOTALL,
        )
        reset_offsets.append(int(match.group(1)) if match else -1)
    if reset_offsets != legacy["legacy_reset_gpio"]:
        errors.append("legacy DTS reset topology changed without catalog review")
    for values in catalog["uart"]["legacy_reference_pinmux"].values():
        require_literal(normalized(dts).lower(), " ".join(values).lower(), "legacy DTS pinmux", errors)
    for path in expected_defconfigs + [DOCKER_BUILD.as_posix()]:
        reject_literal(
            read_text(root, Path(path), errors),
            legacy_path.name,
            "reference-only DTS must not be a build input",
            errors,
        )

    docker = read_text(root, DOCKER_BUILD, errors)
    require_literal(docker, "am3-bb-s19jpro)", "Docker product target", errors)
    require_literal(docker, 'BR_DEFCONFIG="dcentos_am3_bb_s19jpro_defconfig"', "Docker product defconfig", errors)
    helper_path = Path(inventory["dtb_contract_helper"])
    helper = read_text(root, helper_path, errors)
    require_literal(helper, "dcent_am3_bb_admit_carrier_dtb()", "shared DTB contract", errors)
    require_literal(helper, "d00dfeed", "shared DTB FDT-magic validation", errors)
    require_literal(helper, "_dcent_am3_bb_total_size", "shared DTB total-size validation", errors)
    require_literal(
        helper,
        '[ "$_dcent_am3_bb_total_size" -eq "$_dcent_am3_bb_file_size" ]',
        "shared DTB rejects bytes outside declared FDT",
        errors,
    )
    require_literal(helper, "S19J_IO_BOARD", "shared DTB carrier marker", errors)
    require_literal(helper, "am335x-boneblack-btm", "shared BTM carrier marker", errors)
    policies = inventory["dtb_policies"]
    for script_name in inventory.get("product_build_scripts", []):
        script = read_text(root, Path(script_name), errors)
        require_literal(
            script,
            '. "$SCRIPT_DIR/lib/am3_bb_dtb_contract.sh"',
            "shared carrier-DTB contract import",
            errors,
        )
        require_literal(
            script,
            'dcent_am3_bb_admit_carrier_dtb "${}" {} '.format(
                "DTB_SOURCE" if script_name.endswith("build_am3_bb_s19jpro.sh") else "DTB_SRC",
                policies[script_name],
            ),
            "shared carrier-DTB admission call",
            errors,
        )
        reject_literal(
            script,
            "grep -a -q 'S19J_IO_BOARD'",
            "carrier marker parsing is centralized",
            errors,
        )
        reject_literal(
            script,
            "grep -a -q 'am335x-boneblack-btm'",
            "BTM marker parsing is centralized",
            errors,
        )
        admission = script.find("dcent_am3_bb_admit_carrier_dtb \"$DTB_")
        legacy_check = script.find("if grep -a -q 'am335x-boneblack-btm'")
        if legacy_check >= 0 and (admission < 0 or admission > legacy_check):
            errors.append("{}: shared DTB gate must dominate legacy diagnostics".format(script_name))
    helper = read_text(root, Path("scripts/build_am3_bb_s19jpro.sh"), errors)
    reject_literal(helper, "Falling back to '--target am3-bb'", "product build target", errors)
    reject_literal(helper, 'build_in_docker.sh" --target am3-bb ', "product build target", errors)
    require_literal(
        helper,
        '[ -n "$DTB_SOURCE" ] || {',
        "--artifacts requires a carrier DTB",
        errors,
    )
    docker_branch = helper.find("elif command -v docker")
    docker_artifacts_guard = helper.find('if [ -n "$ARTIFACT_DIR" ]; then', docker_branch)
    docker_invocation = helper.find('"$SCRIPT_DIR/build_in_docker.sh"', docker_branch)
    if not (
        docker_branch >= 0
        and docker_artifacts_guard > docker_branch
        and docker_invocation > docker_artifacts_guard
    ):
        errors.append("Docker --artifacts refusal must dominate the Docker build/early-exit path")
    require_literal(
        helper,
        "--artifacts is not supported by Docker packaging",
        "Docker artifact refusal is explicit",
        errors,
    )
    post_image = read_text(root, POST_IMAGE, errors)
    require_literal(post_image, "carrier-aware DTB", "payload operator instructions", errors)
    require_literal(post_image, "checked-in am335x-s19jpro.dts is legacy reference", "payload DTS quarantine", errors)

    return errors


def main(argv: Sequence[str] = ()) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--catalog", type=Path, default=DEFAULT_CATALOG)
    args = parser.parse_args(argv)
    errors = validate_repository(args.root, args.catalog)
    if errors:
        for error in errors:
            print("ERROR: {}".format(error), file=sys.stderr)
        return 1
    print("AM3-BB hardware contract: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
