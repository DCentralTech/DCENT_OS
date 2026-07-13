#!/usr/bin/env bash
# Offline anti-regression proof for private Phase 0 -> Phase 5 binary staging.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_DRIVER="$SCRIPT_DIR/build_in_docker.sh"

python3 - "$BUILD_DRIVER" <<'PY'
from pathlib import Path
import sys

source = Path(sys.argv[1]).read_text(encoding="utf-8")
phase0 = source.split("dcent_phase0_stale_binary_guard() {", 1)[1].split(
    "dcent_phase0_stale_binary_guard_selftest()", 1
)[0]
cleanup = source.split("dcent_cleanup_failed_release_evidence() {", 1)[1].split(
    "trap dcent_cleanup_failed_release_evidence EXIT", 1
)[0]
phase5 = source.split(
    "# -------- Phase 5: stage pre-built ARM binary --------", 1
)[1].split("# -------- Phase 5b:", 1)[0]
required = source.split("dcent_required_prebuilt_binaries() {", 1)[1].split("\n}", 1)[0]
discovery_selection = source.split(
    "dcent_target_requires_dcentos_discovery() {", 1
)[1].split("\n}", 1)[0]
post_inspection = source.split('BUILD_CONTAINER_ID="$(docker image inspect', 1)[1]

# Policy validation precedes one exact export of all target-required pairs.
assert phase0.index("check-override-policy") < phase0.index("export-snapshot-set")
assert 'export_args+=(--pair "$binary_path" "${binary_path}.build-receipt.json")' in phase0
assert '--stage-parent "$BINARY_EXPORT_PARENT"' in phase0
assert 'DCENT_ALLOW_STALE_DCENTRALD is deprecated and does not bypass receipt/export validation.' in phase0
assert "proceeding without valid source-bound binary receipts" not in phase0

# Capability-authorized cleanup is installed before either private stage is made.
assert "destroy-export-snapshot-set" in cleanup
assert '--capability "$BINARY_EXPORT_CAPABILITY"' in cleanup
assert source.index("trap dcent_cleanup_failed_release_evidence EXIT") < source.index(
    'BINARY_EXPORT_PARENT="$(python3'
)
assert source.index('export-snapshot-capability-path --stage "$BINARY_EXPORT_STAGE"') < source.index(
    "# -------- Phase 5: stage pre-built ARM binary --------"
)
assert "# -------- Phase 2b: invalidate cached prebuilt binaries in volume --------" in source
assert source.count("for binary_name in $REQUIRED_BINARIES") >= 1
assert "dspic-flash" not in required
assert "pic-recovery" not in required

# Phase 5 sees only the read-only export. Every selected source path comes from
# the verified helper endpoint and the old mutable host target mount is absent.
assert '-v "${DOCKER_BINARY_EXPORT_STAGE}:/dcent-binaries:ro"' in source
assert '"${BINARY_EXPORT_MOUNT_ARGS[@]}"' in phase5
assert '${POSIX_PROJECT_DIR}/dcentrald/target:/target:ro' not in phase5
assert '"/target/' not in phase5
assert "query-export-snapshot-path" in source
assert '--field path-sha256' in source
assert "query-export-snapshot-path" not in phase5
assert "RECEIPT_HELPER" not in phase5
assert "verify-export-snapshot-set" not in phase5
assert 'source_sha256="$(sha256sum "$source_path"' in phase5
assert 'destination_sha256="$(sha256sum "$destination"' in phase5
assert 'destination digest mismatch' in phase5

# Every Docker execution after image inspection consumes the immutable ID, not
# the mutable tag whose identity was inspected earlier.
assert '"$IMAGE_NAME" bash -c' not in post_inspection
assert "docker run --rm $IMAGE_NAME" not in post_inspection
assert post_inspection.count('"$BUILD_CONTAINER_ID" bash -c') >= 10

# The generic required-set contract keeps S9/AM2 init and BB/CV discovery
# selection centralized rather than rebuilding Phase 5 special cases.
for fragment in (
    "printf '%s\\n' dcentrald",
    'dcent_target_requires_dcentos_init "$TARGET"',
    "printf '%s\\n' dcentos-init",
    'dcent_target_requires_dcentos_discovery "$TARGET"',
    "printf '%s\\n' dcentos-discovery",
):
    assert fragment in required, fragment
assert 'if [ "$TARGET" = "am3-bb" ]' not in phase5
assert "cv1835-s19jpro" in discovery_selection

# Warm-volume invalidation is all-known, not merely current-target, and refuses
# link-bearing destination components before deletion or installation.
phase2b = source.split(
    "# -------- Phase 2b: invalidate cached prebuilt binaries in volume --------", 1
)[1].split("# -------- Phase 3:", 1)[0]
known_assignment = next(
    line for line in phase2b.splitlines() if 'ALL_PREBUILT_BINARIES="' in line
)
assert set(known_assignment.split('ALL_PREBUILT_BINARIES="', 1)[1].split('"', 1)[0].split()) == {
    "dcentrald", "dcentos-init", "dcentos-discovery", "pic-recovery", "dspic-flash"
}
assert '${binary_name}.build-receipt.json' in phase2b
assert 'unsafe persistent binary staging component' in phase2b
assert 'unsafe persistent binary staging component' in phase5
PY

# Execute the actual embedded Phase 2b/5 container programs through a fake
# Docker CLI. This covers namespace refusal, all-known purge, immutable image
# selection, source/destination digest equivalence, and copy corruption.
python3 - "$BUILD_DRIVER" <<'PY'
from __future__ import annotations

import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile


source = Path(sys.argv[1]).read_text(encoding="utf-8")


def payload_between(start: str, end: str) -> str:
    section = source.split(start, 1)[1].split(end, 1)[0]
    marker = '"$BUILD_CONTAINER_ID" bash -c \'\n'
    return section.split(marker, 1)[1].rsplit("\n    '", 1)[0]


phase2_payload = payload_between(
    "# -------- Phase 2b: invalidate cached prebuilt binaries in volume --------",
    "# -------- Phase 3:",
)
phase5_payload = payload_between(
    "# -------- Phase 5: stage pre-built ARM binary --------",
    "# -------- Phase 5b:",
)
assert "python3" not in phase5_payload
assert "binary_build_receipt.py" not in phase5_payload

with tempfile.TemporaryDirectory(prefix="dcent-phase5-fake-docker-") as temporary:
    root = Path(temporary)
    fake_docker = root / "docker.py"
    log = root / "docker.log"
    fake_docker.write_text(
        """import os, subprocess, sys
args = sys.argv[1:]
assert args.pop(0) == 'run'
environment = os.environ.copy()
while args and args[0].startswith('-'):
    option = args.pop(0)
    if option == '--rm':
        continue
    if option in ('-e', '-v'):
        value = args.pop(0)
        if option == '-e':
            key, value = value.split('=', 1)
            environment[key] = value
        continue
    raise SystemExit(f'unsupported fake Docker option: {option}')
image = args.pop(0)
with open(os.environ['FAKE_DOCKER_LOG'], 'a', encoding='utf-8') as handle:
    handle.write(image + '\\n')
raise SystemExit(subprocess.run(args, env=environment).returncode)
""",
        encoding="utf-8",
    )

    def fake_run(payload: str, environment: dict[str, str]) -> subprocess.CompletedProcess[str]:
        command = [sys.executable, str(fake_docker), "run", "--rm"]
        for key, value in environment.items():
            command.extend(["-e", f"{key}={value}"])
        command.extend(["sha256:fixture-image", "bash", "-c", payload])
        host_environment = os.environ.copy()
        host_environment["FAKE_DOCKER_LOG"] = str(log)
        return subprocess.run(
            command,
            env=host_environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    volume = root / "volume"
    volume.mkdir()
    export = root / "export"
    exported_binary = export / "artifacts/0000/dcentrald"
    exported_binary.parent.mkdir(parents=True)
    exported_bytes = b"captured-generation"
    exported_binary.write_bytes(exported_bytes)
    expected_digest = hashlib.sha256(exported_bytes).hexdigest()
    phase2 = phase2_payload.replace("/build", str(volume))
    phase5 = phase5_payload.replace("/dcent-binaries", str(export)).replace(
        "/build", str(volume)
    )
    phase2_environment = {
        "BUILD_ARCH": "armv7-unknown-linux-musleabihf",
        "ALL_PREBUILT_BINARIES": "dcentrald dcentos-init dcentos-discovery pic-recovery dspic-flash",
    }
    created = fake_run(phase2, phase2_environment)
    assert created.returncode == 0, created.stderr
    release = (
        volume
        / "dcentos/dcentrald/target/armv7-unknown-linux-musleabihf/release"
    )
    for name in ("dcentrald", "dcentos-init", "dcentos-discovery"):
        (release / name).write_bytes(b"stale")
        (release / f"{name}.build-receipt.json").write_bytes(b"stale receipt")
    purged = fake_run(phase2, phase2_environment)
    assert purged.returncode == 0, purged.stderr
    assert list(release.iterdir()) == []

    phase5_environment = {
        "BUILD_ARCH": "armv7-unknown-linux-musleabihf",
        "TARGET": "unit-test-no-version-gate",
        "REQUIRED_BINARIES": "dcentrald",
        "DCENTRALD_EXPORT_PATH": "artifacts/0000/dcentrald",
        "DCENTRALD_EXPORT_SHA256": expected_digest,
        "INIT_EXPORT_PATH": "",
        "INIT_EXPORT_SHA256": "",
        "DISCOVERY_EXPORT_PATH": "",
        "DISCOVERY_EXPORT_SHA256": "",
        "DCENT_MANIFEST_PUBLIC_KEY_HEX": "",
        "DCENT_ALLOW_UNSIGNED_SYSUPGRADE": "0",
    }
    copied = fake_run(phase5, phase5_environment)
    assert copied.returncode == 0, copied.stderr
    assert (release / "dcentrald").read_bytes() == exported_bytes

    # A corrupt copy implementation cannot pass destination equivalence.
    fake_run(phase2, phase2_environment)
    fake_bin = root / "fake-bin"
    fake_bin.mkdir()
    fake_install = fake_bin / "install"
    fake_install.write_text(
        "#!/bin/sh\n[ \"$1\" = -m ] && shift 2\nprintf tampered > \"$2\"\n",
        encoding="utf-8",
    )
    fake_install.chmod(0o755)
    corrupt_environment = dict(phase5_environment)
    corrupt_environment["PATH"] = str(fake_bin) + os.pathsep + os.environ["PATH"]
    corrupt = fake_run(phase5, corrupt_environment)
    assert corrupt.returncode != 0
    assert "installed destination digest mismatch" in corrupt.stderr

    # Mutation after the host query is detected before copy.
    fake_run(phase2, phase2_environment)
    exported_binary.write_bytes(b"mutated-after-host-query")
    mutated = fake_run(phase5, phase5_environment)
    assert mutated.returncode != 0
    assert "exported source digest mismatch" in mutated.stderr
    assert not (release / "dcentrald").exists()
    exported_binary.write_bytes(exported_bytes)

    # A warm-volume parent symlink is refused without touching its target.
    bad_volume = root / "bad-volume"
    bad_volume.mkdir()
    outside = root / "outside"
    outside.mkdir()
    marker = outside / "marker"
    marker.write_bytes(b"preserve")
    try:
        (bad_volume / "dcentos").symlink_to(outside, target_is_directory=True)
    except OSError:
        pass
    else:
        bad_phase2 = phase2_payload.replace("/build", str(bad_volume))
        refused = fake_run(bad_phase2, phase2_environment)
        assert refused.returncode != 0
        assert "unsafe persistent binary staging component" in refused.stderr
        assert marker.read_bytes() == b"preserve"

    images = log.read_text(encoding="utf-8").splitlines()
    assert images and set(images) == {"sha256:fixture-image"}

# Execute the target matrix itself, not a duplicate table in Python.
functions = source.split("dcent_target_requires_dcentos_init() {", 1)[1].split(
    "dcent_expected_build_variant() {", 1
)[0]
functions = "dcent_target_requires_dcentos_init() {" + functions
for target, expected in {
    "s9": ["dcentrald", "dcentos-init"],
    "am3-bb": ["dcentrald", "dcentos-discovery"],
    "cv1835-s19jpro": ["dcentrald", "dcentos-discovery"],
    "am3-s21": ["dcentrald", "dcentos-init"],
}.items():
    result = subprocess.run(
        ["bash", "-c", functions + '\nTARGET="$1"; dcent_required_prebuilt_binaries', "_", target],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.splitlines() == expected, (target, result.stdout)

# Execute the host path-transport validator, including Windows drive syntax.
validator = "dcent_validate_docker_transport_path() {" + source.split(
    "dcent_validate_docker_transport_path() {", 1
)[1].split("\n}\n\ndcent_required_prebuilt_binaries", 1)[0] + "\n}"
for value, should_pass in (
    ("/tmp/safe-stage", True),
    (r"C:\\Users\\safe-stage", True),
    ("/tmp/unsafe:stage", False),
    ("/tmp/unsafe\nstage", False),
):
    result = subprocess.run(
        ["bash", "-c", validator + '\ndcent_validate_docker_transport_path "$1"', "_", value],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert (result.returncode == 0) is should_pass, (value, result.stderr)
PY

# Adversarial helper proofs complement the static route proof: source paths may
# be swapped after export without changing captured bytes, and path queries fail
# closed unless the complete detached set still verifies.
python3 "$SCRIPT_DIR/test_binary_build_receipt.py" \
    ReceiptFixture.test_export_after_verify_uses_captured_generation \
    ReceiptFixture.test_verified_path_query_by_name_source_and_artifact \
    ReceiptFixture.test_path_query_fully_verifies_stage_before_output

echo "binary export Phase 5 boundary: PASS"
