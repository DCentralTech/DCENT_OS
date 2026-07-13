#!/usr/bin/env python3
"""Static cross-firmware donation/onboarding contract drift gate."""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[4]
CONTRACTS = ROOT / "docs" / "design-system" / "DCENT_DESIGN_LANGUAGE"
ESP = ROOT / "projects" / "dcentos-esp"
ANT_DONATION = (
    ROOT
    / "projects"
    / "dcentos"
    / "dcentrald"
    / "dcentrald-api"
    / "src"
    / "routes"
    / "donation.rs"
)


def fail(message: str) -> None:
    print(f"FAIL: {message}", file=sys.stderr)


def main() -> int:
    failures: list[str] = []
    onboarding = (CONTRACTS / "onboarding-contract.md").read_text(encoding="utf-8")
    expected_steps = re.findall(r"\| \d+ \| `([a-z-]+)` \|", onboarding)
    portal = (ESP / "dcentaxe" / "src" / "provisioning.rs").read_text(encoding="utf-8")
    label_to_id = {
        "Welcome": "welcome",
        "Network": "network",
        "Password": "password",
        "Mode": "mode",
        "Pool": "pool",
        "Hardware": "hardware",
        "Donation": "donation",
        "Review": "review",
    }
    emitted = [label_to_id[label] for label in re.findall(r'data-label="([^"]+)"', portal)]
    if emitted != expected_steps:
        failures.append(f"onboarding steps {emitted!r} != contract {expected_steps!r}")
    for field in [
        "ssid",
        "wifi_password",
        "owner_password",
        "owner_password_confirm",
        "mining_mode",
        "pool_url",
        "pool_port",
        "worker",
        "pool_password",
        "board_model",
        "frequency",
        "voltage",
        "donation.enabled",
        "donation.percent",
    ]:
        emitted_name = field if field != "wifi_password" else "password"
        if f'name="{emitted_name}"' not in portal:
            failures.append(f"ESP onboarding missing canonical field {field}")
    config_source = (ESP / "dcentaxe" / "src" / "config.rs").read_text(encoding="utf-8")
    if "pub mining_mode: MiningMode" not in config_source:
        failures.append("ESP onboarding mode is not persisted in DcentAxeConfig")

    ant = ANT_DONATION.read_text(encoding="utf-8")
    esp_api = (ESP / "dcentaxe" / "src" / "api.rs").read_text(encoding="utf-8")
    response_keys = {
        "pool_url",
        "pool_host",
        "worker",
        "payout_address",
        "explorer_url",
        "explorer_name",
        "verify_label",
        "trust_model",
        "disclosure",
    }
    for key in sorted(response_keys):
        if f'"{key}"' not in ant:
            failures.append(f"Antminer disclosure missing key {key}")
        if f'"{key}"' not in esp_api:
            failures.append(f"ESP disclosure missing key {key}")

    required_truth = {"donationSet", "donating", "donationPercent"}
    info_source = (
        ESP / "dcentaxe" / "src" / "api_system_info.rs"
    ).read_text(encoding="utf-8")
    combined = esp_api + info_source
    snake = {"donation_set", "donating", "donation_percent"}
    if not all(name in combined for name in snake):
        failures.append(f"ESP system info missing truth fields {sorted(required_truth)}")
    if "implemented + unit-tested; live delivery pending" not in esp_api:
        failures.append("ESP API does not disclose the locked SV2 maturity wording")

    forbidden = re.compile(r"devfee|dev fee|0\.5\s*[-–]\s*1\s*%", re.IGNORECASE)
    for path in [ESP / "README.md", ESP / ""]:
        if forbidden.search(path.read_text(encoding="utf-8")):
            failures.append(f"stale fee terminology in {path.relative_to(ROOT)}")
    if "not wired into the `dcentaxe` binary" in (ESP / "README.md").read_text(encoding="utf-8"):
        failures.append("README still claims the wired LoRa path is not wired")

    if failures:
        for message in failures:
            fail(message)
        return 1
    print(
        f"ESP_CONTRACT_PARITY_OK steps={len(expected_steps)} disclosure_keys={len(response_keys)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
