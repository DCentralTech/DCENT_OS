#!/usr/bin/env python3
"""Static honesty gate for family-level DCENT_OS docs."""

from __future__ import annotations

import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
PROJECT_DOCS = REPO_ROOT / "projects" / "dcentos" / "docs"
MATRIX = REPO_ROOT / "SUPPORT_MATRIX.md"
FAMILY_DOCS = PROJECT_DOCS / "families"
RELEASE_CHECKLISTS = FAMILY_DOCS / "RELEASE_CHECKLISTS.md"


REQUIRED_PHRASES = {
    "antminer": [
        "`beta`",
        "am1-s9",
        "am2-s19jpro-zynq",
        "management-only",
        "operator-gated",
    ],
    "esp": [
        "`beta`",
        "`experimental`",
        "signed-OTA",
        "Unknown board versions",
        "must not auto-start mining",
    ],
    "whatsminer": [
        "`unsupported`",
        "research-preview",
        "not installable",
        "no live I/O proof",
        "no accepted-share proof",
    ],
    "avalon": [
        "`experimental`",
        "Phase-1 shim",
        "Restricted Canaan components",
        "must not be redistributed",
        "No Avalon row has accepted-share proof",
    ],
    "innosilicon": [
        "`experimental`",
        "no daemon binary",
        "No customer install path exists",
        "research-only",
    ],
}


def read(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError:
        raise SystemExit(f"missing required file: {path.relative_to(REPO_ROOT)}") from None


def matrix_families() -> set[str]:
    text = read(MATRIX)
    families: set[str] = set()
    in_table = False
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if line == "<!-- support-matrix:start -->":
            in_table = True
            continue
        if line == "<!-- support-matrix:end -->":
            break
        if not in_table or not line.startswith("| "):
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if not cells or cells[0] in {"family", "---"}:
            continue
        families.add(cells[0])
    return families


def require_phrase(doc: str, family: str, phrase: str) -> None:
    if phrase not in doc:
        raise SystemExit(f"{family}.md missing required honesty phrase: {phrase}")


def main() -> int:
    expected = set(REQUIRED_PHRASES)
    actual = matrix_families()
    if actual != expected:
        raise SystemExit(
            f"family docs drift: matrix={sorted(actual)} docs={sorted(expected)}"
        )

    index = read(FAMILY_DOCS / "README.md")
    for family in sorted(expected):
        rel = f"{family}.md"
        require_phrase(index, family, f"[{rel}]({rel})")
        doc = read(FAMILY_DOCS / rel)
        require_phrase(doc, family, "Tier boundary:")
        for phrase in REQUIRED_PHRASES[family]:
            require_phrase(doc, family, phrase)

    require_phrase(
        index,
        "families/README",
        "[RELEASE_CHECKLISTS.md](RELEASE_CHECKLISTS.md)",
    )
    checklist = read(RELEASE_CHECKLISTS)
    for phrase in [
        "HOST-PROVEN",
        "BENCH-PROVEN",
        "OPERATOR-GATED",
        "No CI job may contact miners",
        "No release checklist entry can promote a SKU",
    ]:
        require_phrase(checklist, "RELEASE_CHECKLISTS", phrase)
    for family in ["Antminer", "ESP/Bitaxe", "Whatsminer", "Avalon", "Innosilicon"]:
        require_phrase(checklist, "RELEASE_CHECKLISTS", f"| {family} |")

    tiers = read(PROJECT_DOCS / "SUPPORT_TIERS.md")
    for tier in ["`stable`", "`beta`", "`experimental`", "`unsupported`", "`unknown`"]:
        require_phrase(tiers, "SUPPORT_TIERS", tier)
    require_phrase(tiers, "SUPPORT_TIERS", "Public docs must not claim a tier above")

    print(f"FAMILY_DOCS_HONESTY_OK families={len(expected)} docs={len(expected) + 3}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
