#!/usr/bin/env bash
# Offline anti-orphan proof for target-scoped build-input preflights.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_DRIVER="$SCRIPT_DIR/build_in_docker.sh"
CARGO_DRIVER="$SCRIPT_DIR/build-dcentrald.sh"
NON_S9_REBUILD="$SCRIPT_DIR/rebuild_all_non_s9.sh"
SIGNING_REHEARSAL="$SCRIPT_DIR/sign_release_dry_run.sh"
SOURCE_CLOSURE="$SCRIPT_DIR/source_closure.py"
WORKFLOW="$SCRIPT_DIR/../../../.github/workflows/dcentos-image-smoke.yml"

grep -Fq 'build_input_snapshot.py" create' "$BUILD_DRIVER"
grep -Fq -- '--target "$TARGET"' "$BUILD_DRIVER"
grep -Fq 'build_input_snapshot.py" create' "$CARGO_DRIVER"
grep -Fq -- '--target cargo-workspace' "$CARGO_DRIVER"
grep -Fq -- '--build-input-manifest "$SCRIPT_DIR/build_inputs.manifest"' "$BUILD_DRIVER"
# The direct contributor path selects this manifest locally. Release capsules
# create the Cargo snapshot in the authenticated outer driver and pass only the
# verified capability into this driver, so do not require a duplicate create
# call here.
grep -Fq 'BUILD_INPUT_MANIFEST="$SCRIPT_DIR/build_inputs.manifest"' "$CARGO_DRIVER"
grep -Fq 'DCENT_CAPSULE_CARGO_BUILD_INPUT_SNAPSHOT' "$CARGO_DRIVER"
grep -Fq 'build_input_snapshot.py" destroy' "$BUILD_DRIVER"
grep -Fq 'build_input_snapshot.py" destroy' "$CARGO_DRIVER"
grep -Fq -- '--token "$BUILD_INPUT_DESTROY_TOKEN"' "$BUILD_DRIVER"
grep -Fq -- '--token "$BUILD_INPUT_DESTROY_TOKEN"' "$CARGO_DRIVER"
! grep -Eq 'DCENT_STOCK_FPGA_|STAGED_STOCK_FPGA|stock_fpga_(s9|extracted)\.bin' \
    "$CARGO_DRIVER"
grep -Fq '"${DOCKER_BUILD_INPUT_STAGE}:/dcent-inputs:ro"' "$BUILD_DRIVER"

# The builder's target case and the release-input policy must remain exactly
# exhaustive. A new builder lane cannot silently inherit an empty v3 closure.
python3 - "$SOURCE_CLOSURE" "$BUILD_DRIVER" "$WORKFLOW" <<'PY'
import importlib.util
import pathlib
import re
import sys

spec = importlib.util.spec_from_file_location("source_closure", sys.argv[1])
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)
driver = pathlib.Path(sys.argv[2]).read_text(encoding="utf-8")
workflow = pathlib.Path(sys.argv[3]).read_text(encoding="utf-8")

target_case = driver.split('case "$TARGET" in', 1)[1].split("esac", 1)[0]
driver_targets = set(
    re.findall(r"^    ([a-z0-9][a-z0-9-]*)\)$", target_case, flags=re.MULTILINE)
)
assert driver_targets == policy.BUILD_DRIVER_TARGETS, (
    sorted(driver_targets),
    sorted(policy.BUILD_DRIVER_TARGETS),
)
assert not (set(policy.BUILD_TARGET_POLICIES) & set(policy.BLOCKED_BUILD_INPUT_TARGETS))
assert set(policy.BUILD_TARGET_POLICIES) | set(policy.BLOCKED_BUILD_INPUT_TARGETS) == driver_targets
assert "cv1835-s19jpro" not in policy.BUILD_DRIVER_TARGETS
assert policy.TARGET_BUILD_INPUTS["cargo-workspace"] == ()
for target in policy.BUILD_TARGET_POLICIES:
    selected = policy.TARGET_BUILD_INPUTS[target]
    assert selected
    assert set(policy.COMMON_CARGO_BUILD_INPUTS).isdisjoint(selected), target
for target in policy.BLOCKED_BUILD_INPUT_TARGETS:
    assert target not in policy.TARGET_BUILD_INPUTS, target

assert policy.TARGET_BUILD_INPUTS["am2-s19jpro-sd"] == policy.TARGET_BUILD_INPUTS["am2-s19jpro"]
assert policy.TARGET_BUILD_INPUTS["am2-s19pro"] == policy.TARGET_BUILD_INPUTS["am2-s19jpro"]

source_closure_path = pathlib.Path(sys.argv[1]).resolve()
repo_root = source_closure_path.parents[3]
_, manifest = policy.parse_build_input_manifest(
    repo_root, str(source_closure_path.with_name("build_inputs.manifest"))
)
selected = set().union(*map(set, policy.TARGET_BUILD_INPUTS.values()))
reference_only = set(policy.REFERENCE_ONLY_BUILD_INPUTS)
separately_verified = set(policy.SEPARATELY_VERIFIED_BUILD_INPUTS)
assert selected.isdisjoint(reference_only)
assert selected.isdisjoint(separately_verified)
assert reference_only.isdisjoint(separately_verified)
assert set(manifest) == selected | reference_only | separately_verified
# The old flat S9/AM2 matrix directly invoked the inner packaging driver. The
# admitted workflow now has one S9 capsule lane whose portable verifier checks
# the closure-v4 retained set, while AM2 is an explicit unavailable job until it
# owns the same lifecycle. Keep the required S9 receipt pair pinned here rather
# than inferring authority from a mutable workflow matrix.
assert tuple(sorted(policy.PREBUILT_RUST_INPUTS_BY_TARGET["s9"])) == (
    "dcentos-init",
    "dcentrald",
)
assert "target: am2-s19jpro" not in workflow
assert "bash scripts/build_in_docker.sh" not in workflow
assert "bash scripts/build-dcentrald.sh" not in workflow

# Current board guides must not advertise the deliberately disabled mutable
# packaging lane. The target arms in the inner driver are future capsule
# recipes, not operator entrypoints.
board_docs = repo_root / "DCENT_OS_Antminer/br2_external_dcentos/board"
for readme_path in board_docs.glob("**/README.md"):
    readme = readme_path.read_text(encoding="utf-8")
    assert not re.search(
        r"(?m)^\s*(?:bash\s+)?(?:DCENT_OS_Antminer/)?scripts/build_in_docker\.sh\b",
        readme,
    ), readme_path
    assert not re.search(r"(?m)^\s*make\s+dev(?:\s|$)", readme), readme_path
    assert not re.search(r"(?m)^\s*make\s+release(?:\s|$)", readme), readme_path

# The inner packaging driver is capability-bound. Only its authenticated S9
# capsule may execute it; other build helpers may describe it but cannot use it
# as a mutable fallback.
scripts_dir = repo_root / "DCENT_OS_Antminer/scripts"
direct_inner_driver = re.compile(
    r"""^\s*(?:bash\s+)?["']?(?:\$[A-Z_]+/)?build_in_docker\.sh["']?(?:\s|$)"""
)
for wrapper_path in scripts_dir.glob("build*.sh"):
    if wrapper_path.name in {"build_in_docker.sh", "build_s9_release_capsule.sh"}:
        continue
    for line_number, line in enumerate(
        wrapper_path.read_text(encoding="utf-8").splitlines(), start=1
    ):
        assert not direct_inner_driver.search(line), (wrapper_path, line_number, line)

signing_rehearsal = (scripts_dir / "sign_release_dry_run.sh").read_text(encoding="utf-8")
assert "bash DCENT_OS_Antminer/scripts/build_in_docker.sh --target" not in signing_rehearsal
assert "make -C DCENT_OS_Antminer release RELEASE_TARGET=s9" in signing_rehearsal
assert "has no authenticated real-key packaging capsule" in signing_rehearsal

makefile = (repo_root / "DCENT_OS_Antminer/Makefile").read_text(encoding="utf-8")
assert "RELEASE_TARGET=s9|am2" not in makefile
assert "DEV_TARGET=s9|am2" not in makefile
assert "Production S9 capsule only" in makefile
assert "Image packaging disabled until a separate lab capsule exists" in makefile

xil25_runbook = (
    repo_root
    / "DCENT_OS_Antminer/docs/dev/2026-06-14-xil25-production-readiness/NAND_BAKE_RUNBOOK.md"
).read_text(encoding="utf-8")
assert "bash DCENT_OS_Antminer/scripts/build_in_docker.sh --target" not in xil25_runbook
assert "A new AM2 build requires a" in xil25_runbook
assert "target-specific capsule; none is currently admitted" in xil25_runbook

release_readme = (repo_root / "DCENT_OS_Antminer/release/README.md").read_text(
    encoding="utf-8"
)
assert "make dev` / `--lab-unsigned" not in release_readme
assert "no lab package is produced" in release_readme
assert "does not grant packaging authority" in release_readme

for required in (
    "bash scripts/build_s9_release_capsule.sh",
    "python3 scripts/portable_release_evidence.py verify",
    "portable-release-evidence.json.sig",
    ".dcent-release-set.json",
    "DCENT_PUBLISHED_RELEASE",
    "runs-on: [self-hosted, linux, x64, dcentos-restricted-inputs]",
    "provision_build_inputs.sh --source",
    "AM2 package smoke: intentionally unavailable",
):
    assert required in workflow, required
PY

# Direct packaging has no invocation/source/result authority and must stop
# before even scanning provenance or probing Docker. Target-policy assertions
# above deliberately exclude CV: it has no artifact lane to preflight.
TMPDIR_TEST="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_TEST"' EXIT
mkdir -p "$TMPDIR_TEST/bin"
cat > "$TMPDIR_TEST/bin/docker" <<EOF
#!/bin/sh
touch "$TMPDIR_TEST/docker-called"
exit 99
EOF
chmod +x "$TMPDIR_TEST/bin/docker"

if PATH="$TMPDIR_TEST/bin:$PATH" \
    bash "$BUILD_DRIVER" --target cv1835-s19jpro --lab-unsigned \
    >"$TMPDIR_TEST/stdout" 2>"$TMPDIR_TEST/stderr"; then
    echo "ERROR: CV build-input preflight accepted a target with no pinned kernel" >&2
    exit 1
fi
grep -Fq 'cv1835-s19jpro has no firmware, sysupgrade, or supported artifact build lane' \
    "$TMPDIR_TEST/stderr"
if [ -e "$TMPDIR_TEST/docker-called" ]; then
    echo "ERROR: CV refusal occurred after Docker was invoked" >&2
    exit 1
fi

# The legacy non-S9 sweep must not loop through a driver that rejects every
# direct invocation or leave partial digest evidence behind.
if bash "$NON_S9_REBUILD" >"$TMPDIR_TEST/rebuild-stdout" 2>"$TMPDIR_TEST/rebuild-stderr"; then
    echo "ERROR: non-S9 rebuild helper claimed an admitted build path" >&2
    exit 1
fi
grep -Fq 'non-S9 image rebuilding is unavailable' "$TMPDIR_TEST/rebuild-stderr"
grep -Fq 'no build, digest, partial pin, or publication files were written' \
    "$TMPDIR_TEST/rebuild-stderr"
if grep -Fq 'bash "$SCRIPT_DIR/build_in_docker.sh"' "$NON_S9_REBUILD"; then
    echo "ERROR: non-S9 inventory still invokes the disabled direct driver" >&2
    exit 1
fi
bash "$NON_S9_REBUILD" --list >"$TMPDIR_TEST/rebuild-list"
grep -Fq 'am3-s21pro|dcentos-sysupgrade-am3-s21pro.tar' \
    "$TMPDIR_TEST/rebuild-list"
grep -Fq 'am3-bb-s19jpro|dcentos-am3-bb-s19jpro-sdcard.tar' \
    "$TMPDIR_TEST/rebuild-list"

if DCENT_RELEASE_SIGNING_KEY=sentinel \
    sh "$SIGNING_REHEARSAL" --target s9 \
    >"$TMPDIR_TEST/sign-s9-stdout" 2>"$TMPDIR_TEST/sign-s9-stderr"; then
    echo "ERROR: signing rehearsal accepted a configured real key" >&2
    exit 1
fi
grep -Fq 'make -C DCENT_OS_Antminer release RELEASE_TARGET=s9' \
    "$TMPDIR_TEST/sign-s9-stderr"
if DCENT_RELEASE_SIGNING_KEY=sentinel \
    sh "$SIGNING_REHEARSAL" --target am3-s21 \
    >"$TMPDIR_TEST/sign-am3-stdout" 2>"$TMPDIR_TEST/sign-am3-stderr"; then
    echo "ERROR: signing rehearsal accepted a configured real key" >&2
    exit 1
fi
grep -Fq 'No real-key packaging command is admitted for target am3-s21' \
    "$TMPDIR_TEST/sign-am3-stderr"

echo "build-input preflight: PASS"
