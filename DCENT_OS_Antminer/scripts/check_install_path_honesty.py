#!/usr/bin/env python3
"""Static honesty gate for Antminer install-path claims (SD + Toolbox).

Pins the 2026-07-09 install-path release wave:

* install matrix + CE-410 checklist exist and use required vocabulary
* INSTALL docs do not claim AM2 SD production is supported
* S9 SD is described as boot-only / keep-card (not NAND install by default)
* PUBLIC_INSTALL_ARCHITECTURE no longer lists Amlogic as an unconditional
  official public install vector

Exit 0 on success; non-zero with printed failures otherwise.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
WAVE = REPO_ROOT / "docs" / "dev" / "2026-07-09-install-path-release"
INSTALL_DOCS = REPO_ROOT / "projects" / "dcentos" / "docs" / "INSTALL"
DCENTOS_DOCS = REPO_ROOT / "projects" / "dcentos" / "docs"

# Toolbox single source for method classification + the Homer-facing copy the
# 3-method honesty check scans.
TOOLBOX_SRC = REPO_ROOT / "projects" / "dcent-toolbox" / "src" / "dcent_toolbox"
INSTALL_METHODS_PY = TOOLBOX_SRC / "core" / "install_methods.py"
INSTALL_UX_PY = TOOLBOX_SRC / "core" / "install_ux.py"
UART_CMD_PY = TOOLBOX_SRC / "cli" / "commands" / "uart.py"

# Forbidden generic install claims — mirrors
# ``install_methods.FORBIDDEN_GENERIC_CLAIMS``. Any of these in user-facing
# installer copy overclaims support without naming the exact model + control
# board + method. Kept here (not imported) so the firmware CI gate has no
# cross-project import dependency; drift is caught by asserting each phrase also
# appears in ``install_methods.py``.
FORBIDDEN_GENERIC_CLAIMS = (
    "install works",
    "installation works",
    "sd install works for s19j pro",
    "sd card install works",
    "toolbox flashes s11",
    "toolbox flashes s15",
    "toolbox flashes s23",
    "any s19j pro",
    "works on all antminers",
    "works on any antminer",
    "one-click install for all",
)


def check_three_method_honesty(failures: list[str]) -> None:
    """Pin the OTA/SD/UART 3-method model + scan user-facing copy for overclaims."""
    matrix = read(WAVE / "INSTALL_PATH_MATRIX.md") if (
        WAVE / "INSTALL_PATH_MATRIX.md"
    ).is_file() else ""
    for phrase in (
        "Network install (OTA/SSH)",
        "SD card",
        "UART rescue",
        "install_methods.py",   # machine twin reference
        "Not Applicable",       # honest structural-impossibility label
    ):
        if phrase not in matrix:
            failures.append(f"INSTALL_PATH_MATRIX.md missing 3-method phrase: {phrase!r}")

    if not INSTALL_METHODS_PY.is_file():
        failures.append("missing toolbox single source: core/install_methods.py")
        return
    methods_src = read(INSTALL_METHODS_PY)
    for token in ("FORBIDDEN_GENERIC_CLAIMS", "network-ota", "sd-card", "uart-rescue"):
        if token not in methods_src:
            failures.append(f"install_methods.py missing required token: {token!r}")
    # Drift guard: the guard's forbidden list must match install_methods'.
    for phrase in FORBIDDEN_GENERIC_CLAIMS:
        if phrase not in methods_src.lower():
            failures.append(
                f"install_methods.FORBIDDEN_GENERIC_CLAIMS drift: missing {phrase!r}"
            )

    # Scan the Homer-facing copy (installer UX + UART command) for overclaims.
    for path in (INSTALL_UX_PY, UART_CMD_PY):
        if not path.is_file():
            failures.append(f"missing user-facing module: {path.relative_to(REPO_ROOT)}")
            continue
        text = read(path).lower()
        for phrase in FORBIDDEN_GENERIC_CLAIMS:
            if phrase in text:
                failures.append(
                    f"{path.name} contains forbidden generic install claim: {phrase!r}"
                )


def read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise SystemExit(f"missing required file: {path}") from exc


def main() -> int:
    failures: list[str] = []

    required_wave_files = [
        WAVE / "README.md",
        WAVE / "INSTALL_PATH_MATRIX.md",
        WAVE / "SD_CARD_STATUS.md",
        WAVE / "CE410_AM2_SD_ARTIFACT_CHECKLIST.md",
        WAVE / "TOOLBOX_INSTALL_STATUS.md",
        WAVE / "HARDWARE_VALIDATION_CHECKLIST.md",
        WAVE / "INSTALL_READINESS_VERDICT.md",
    ]
    for path in required_wave_files:
        if not path.is_file():
            failures.append(f"missing wave deliverable: {path.relative_to(REPO_ROOT)}")

    matrix = read(WAVE / "INSTALL_PATH_MATRIX.md") if (WAVE / "INSTALL_PATH_MATRIX.md").is_file() else ""
    for phrase in (
        "Driver readiness",
        "Blocked by Missing Signed Artifact",
        "am1-s9",
        "am2-s19jpro-zynq",
        "CE-410",
        "CE-026",
        "S9",
        "S11",
        "S15",
        "S17",
        "S19",
        "S19j Pro",
        "S19k Pro",
        "S21",
        "S23",
        "Detect-Only",
        "Public Beta Supported",
    ):
        if phrase not in matrix:
            failures.append(f"INSTALL_PATH_MATRIX.md missing required phrase: {phrase!r}")

    # AM2 SD must not be "Public Beta Supported" without cold-boot evidence language.
    # Host may ship complete+signed Experimental; never silent Supported.
    if re.search(r"AM2 SD.*Public Beta Supported", matrix, re.I | re.S):
        if "cold-boot" not in matrix.lower() and "unproven" not in matrix.lower():
            failures.append(
                "matrix must not claim AM2 SD Supported without cold-boot/unproven residual text"
            )
    # Closeout GO guard must exist
    go_guard = REPO_ROOT / "projects" / "dcentos" / "scripts" / "check_install_path_go_guard.py"
    if not go_guard.is_file():
        failures.append("missing check_install_path_go_guard.py")
    closeout = REPO_ROOT / "docs" / "dev" / "2026-07-09-install-path-production-closeout" / "FINAL_GO_NO_GO.md"
    if closeout.is_file():
        final_text = read(closeout)
        if re.search(r"Aggregate public install GO:\s*\*?\*?YES", final_text, re.I):
            failures.append("FINAL_GO_NO_GO must not claim aggregate YES without capstone program complete")

    ce410 = read(WAVE / "CE410_AM2_SD_ARTIFACT_CHECKLIST.md") if (WAVE / "CE410_AM2_SD_ARTIFACT_CHECKLIST.md").is_file() else ""
    for phrase in (
        "boot_artifacts_complete",
        "BOOT.bin",
        "uImage",
        "devicetree.dtb",
        "uEnv.txt",
        "bitstream",
        "rootfs",
        "test_sd_signing_gate_static.sh",
        "Blocked by Missing Signed Artifact",
    ):
        if phrase not in ce410:
            failures.append(f"CE410 checklist missing required phrase: {phrase!r}")

    install_readme = read(INSTALL_DOCS / "README.md")
    if "Preferred methods" not in install_readme and "Preferred methods (priority order)" not in install_readme:
        failures.append("INSTALL/README.md must list preferred methods (SD then Toolbox)")
    if "keep card" not in install_readme.lower() and "Keep the card" not in install_readme:
        failures.append("INSTALL/README.md must state S9 SD keep-card / boot-only posture")
    # Must not claim AM2 SD production Supported/NAND, and must not stale-claim
    # "Blocked by Missing Signed Artifact" after host complete+signed package exists.
    if re.search(r"AM2 SD.*Blocked by Missing Signed Artifact", install_readme, re.I):
        failures.append(
            "INSTALL/README.md stale: AM2 SD host package exists — use Experimental/cold-boot language"
        )
    bad_am2_sd = re.search(
        r"S19j Pro.*SD.*(works|ready|Supported).*install",
        install_readme,
        re.I | re.S,
    )
    if bad_am2_sd and "Experimental" not in install_readme and "cold-boot" not in install_readme.lower():
        failures.append("INSTALL/README.md appears to claim S19j Pro SD install works")

    s19j = read(INSTALL_DOCS / "S19J_PRO_XILINX.md")
    if "Blocked by Missing Artifact" not in s19j and "clean unit" not in s19j.lower() and "live" not in s19j.lower():
        failures.append("INSTALL/S19J_PRO_XILINX.md must state live install residual / blocked facets")
    if "Experimental" not in s19j and "cold-boot" not in s19j.lower():
        failures.append("INSTALL/S19J_PRO_XILINX.md must classify AM2 SD as Experimental or cold-boot residual")
    if "upload_accepted" not in s19j:
        failures.append("INSTALL/S19J_PRO_XILINX.md must document TARGET_SYSUPGRADE upload_accepted honesty")

    # Verdict must not stale-claim missing signed AM2 SD package (line-scoped).
    verdict = WAVE / "INSTALL_READINESS_VERDICT.md"
    if verdict.is_file():
        vtext = read(verdict)
        for line in vtext.splitlines():
            # Table: | Complete signed AM2 SD ... | Present? | HTTPS? |
            if re.search(r"Complete signed AM2 SD", line, re.I):
                # Present-locally cell must be Yes (HTTPS column may be No).
                if not re.search(r"Complete signed AM2 SD[^|]*\|\s*\*\*Yes\*\*", line, re.I):
                    if re.search(r"Complete signed AM2 SD[^|]*\|\s*\*\*No\*\*", line, re.I):
                        failures.append(
                            "INSTALL_READINESS_VERDICT stale: complete signed AM2 SD Present=No"
                        )
                    elif re.search(r"Complete signed AM2 SD[^|]*\|\s*No\s*\|", line, re.I):
                        failures.append(
                            "INSTALL_READINESS_VERDICT stale: complete signed AM2 SD Present=No"
                        )
            if re.search(r"AM2 SD production", line, re.I) and re.search(
                r"Blocked by Missing Signed Artifact", line, re.I
            ):
                failures.append(
                    "INSTALL_READINESS_VERDICT stale: AM2 SD is Experimental host complete+signed"
                )
            if re.search(r"^\| AM2 \|", line) and re.search(
                r"Blocked by Missing Signed Artifact", line, re.I
            ):
                failures.append(
                    "INSTALL_READINESS_VERDICT SD-card table still marks AM2 missing-artifact"
                )


    s9 = read(INSTALL_DOCS / "S9_XILINX.md")
    if "not NAND" not in s9 and "not “SD install to NAND”" not in s9 and "not \"SD install to NAND\"" not in s9:
        if "does not write NAND" not in s9 and "SD-boot only" not in s9:
            failures.append("INSTALL/S9_XILINX.md must keep SD-boot ≠ NAND install language")

    arch = read(DCENTOS_DOCS / "PUBLIC_INSTALL_ARCHITECTURE.md")
    if "Install-path honesty" not in arch and "install-path" not in arch.lower():
        failures.append("PUBLIC_INSTALL_ARCHITECTURE.md must reference install-path honesty / matrix")
    # Amlogic must not be listed as unconditional official public primary.
    if re.search(
        r"Primary path for supported Zynq and Amlogic releases",
        arch,
    ):
        failures.append(
            "PUBLIC_INSTALL_ARCHITECTURE.md still claims Amlogic as unconditional official public vector"
        )
    if "upload_accepted" not in arch:
        failures.append("PUBLIC_INSTALL_ARCHITECTURE.md must mention AM2 upload_accepted / no auto-reboot")

    pointer = read(DCENTOS_DOCS / "ANTMINER_PRODUCTION_READINESS_MATRIX.md")
    if "2026-07-09-install-path-release" not in pointer:
        failures.append("ANTMINER_PRODUCTION_READINESS_MATRIX.md must point at install-path wave")

    # 3-method (OTA/SD/UART) honesty + forbidden-generic-claim scan.
    check_three_method_honesty(failures)

    if failures:
        print("check_install_path_honesty: FAIL")
        for item in failures:
            print(f"  - {item}")
        return 1

    print("check_install_path_honesty: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
