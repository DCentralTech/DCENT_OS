#!/usr/bin/env bash
# Offline fixture and adversarial tests for Buildroot legal-info evidence.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GENERATOR="$SCRIPT_DIR/buildroot_legal_inventory.py"
TMPDIR_TEST="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_TEST"' EXIT
LEGAL="$TMPDIR_TEST/legal-info"
ARTIFACTS="$TMPDIR_TEST/artifacts"
mkdir -p "$LEGAL/sources" "$LEGAL/host-sources" "$LEGAL/licenses/busybox" \
    "$LEGAL/host-licenses/host-make" "$ARTIFACTS"

printf 'PACKAGE,VERSION,LICENSE,LICENSE FILES,SOURCE ARCHIVE,SOURCE SITE\nBusyBox,1.36.1,GPL-2.0\n' \
    > "$LEGAL/manifest.csv"
printf 'PACKAGE,VERSION,LICENSE,LICENSE FILES,SOURCE ARCHIVE,SOURCE SITE\nhost-make,4.4.1,GPL-3.0\n' \
    > "$LEGAL/host-manifest.csv"
printf 'BR2_PACKAGE_BUSYBOX=y\n' > "$LEGAL/buildroot.config"
printf 'Buildroot legal-info fixture\n' > "$LEGAL/README"
printf 'source archive bytes\n' > "$LEGAL/sources/busybox-1.36.1.tar.bz2"
printf 'host source archive bytes\n' > "$LEGAL/host-sources/make-4.4.1.tar.gz"
printf 'target license bytes\n' > "$LEGAL/licenses/busybox/COPYING"
printf 'host license bytes\n' > "$LEGAL/host-licenses/host-make/COPYING"
printf 'fixture legal-info checksum ledger\n' > "$LEGAL/legal-info.sha256"
printf 'firmware bytes\n' > "$ARTIFACTS/dcentos-test.tar"

COMMON_ARGS=(
    --legal-info-dir "$LEGAL"
    --buildroot-repository https://github.com/buildroot/buildroot.git
    --buildroot-commit 7c8edc1b402efcd7bba2dabfe0b3be877adaed7a
    --target am1-s9
    --arch armv7-unknown-linux-musleabihf
    --artifact "$ARTIFACTS/dcentos-test.tar"
    --source-date-epoch 1700000000
)

expect_failure() {
    local label="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        echo "ERROR: Buildroot legal inventory accepted invalid input: $label" >&2
        exit 1
    fi
}

python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" --output "$ARTIFACTS/one.json" >/dev/null
touch -t 202501020304 "$LEGAL/manifest.csv" "$LEGAL/sources/busybox-1.36.1.tar.bz2" \
    "$ARTIFACTS/dcentos-test.tar"
python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" --output "$ARTIFACTS/two.json" >/dev/null
cmp "$ARTIFACTS/one.json" "$ARTIFACTS/two.json"
python3 "$GENERATOR" verify --artifact-dir "$ARTIFACTS" --legal-info-dir "$LEGAL" \
    "$ARTIFACTS/one.json" >/dev/null
python3 "$GENERATOR" verify --artifact-dir "$ARTIFACTS" "$ARTIFACTS/one.json" \
    | grep -Fq 'materials=not-reinspected'

ln -s "$TMPDIR_TEST" "$TMPDIR_TEST/legal-info-parent-link"
expect_failure symlinked-legal-info-ancestor python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --legal-info-dir "$TMPDIR_TEST/legal-info-parent-link/legal-info" \
    --output "$ARTIFACTS/symlink-root.json"
rm "$TMPDIR_TEST/legal-info-parent-link"

ln -s "$ARTIFACTS" "$TMPDIR_TEST/artifact-parent-link"
expect_failure symlinked-artifact-ancestor python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --artifact "$TMPDIR_TEST/artifact-parent-link/dcentos-test.tar" \
    --output "$ARTIFACTS/symlink-artifact.json"
rm "$TMPDIR_TEST/artifact-parent-link"

ln -s "$ARTIFACTS" "$TMPDIR_TEST/output-parent-link"
expect_failure symlinked-output-ancestor python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --output "$TMPDIR_TEST/output-parent-link/symlink-output.json"
rm "$TMPDIR_TEST/output-parent-link"

ln -s "$ARTIFACTS" "$TMPDIR_TEST/artifacts-link"
expect_failure symlinked-artifact-directory python3 "$GENERATOR" verify \
    --artifact-dir "$TMPDIR_TEST/artifacts-link" "$ARTIFACTS/one.json"
rm "$TMPDIR_TEST/artifacts-link"

python3 - "$ARTIFACTS/one.json" <<'PY'
import json
import pathlib
import sys

inventory = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
assert inventory["schema"] == "org.dcentral.dcentos.buildroot-legal-inventory.v1"
assert inventory["scope"]["is_sbom"] is False
assert inventory["scope"]["spdx_conformance"] == "not_claimed"
assert inventory["scope"]["cyclonedx_conformance"] == "not_claimed"
assert inventory["scope"]["license_compliance"] == "not_assessed"
assert inventory["scope"]["vulnerability_analysis"] == "not_performed"
assert inventory["legal_info"]["source_archive_file_count"] == 1
assert inventory["legal_info"]["host_source_archive_file_count"] == 1
assert inventory["legal_info"]["target_license_file_count"] == 1
assert inventory["legal_info"]["host_license_file_count"] == 1
assert [entry["path"] for entry in inventory["legal_info"]["files"]] == sorted(
    entry["path"] for entry in inventory["legal_info"]["files"]
)
PY

printf 'artifact mutation\n' >> "$ARTIFACTS/dcentos-test.tar"
expect_failure changed-artifact python3 "$GENERATOR" verify \
    --artifact-dir "$ARTIFACTS" "$ARTIFACTS/one.json"
printf 'firmware bytes\n' > "$ARTIFACTS/dcentos-test.tar"

printf 'source mutation\n' >> "$LEGAL/sources/busybox-1.36.1.tar.bz2"
expect_failure changed-material python3 "$GENERATOR" verify \
    --artifact-dir "$ARTIFACTS" --legal-info-dir "$LEGAL" "$ARTIFACTS/one.json"
printf 'source archive bytes\n' > "$LEGAL/sources/busybox-1.36.1.tar.bz2"

mv "$LEGAL/sources/busybox-1.36.1.tar.bz2" "$TMPDIR_TEST/source-archive"
expect_failure missing-source-evidence python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --output "$ARTIFACTS/missing-source.json"
mv "$TMPDIR_TEST/source-archive" "$LEGAL/sources/busybox-1.36.1.tar.bz2"

ln -s ../../manifest.csv "$LEGAL/licenses/busybox/linked-license"
expect_failure symlink-evidence python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --output "$ARTIFACTS/symlink.json"
rm "$LEGAL/licenses/busybox/linked-license"

expect_failure mutable-buildroot-ref python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --buildroot-commit master --output "$ARTIFACTS/mutable.json"
expect_failure noncanonical-repository python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --buildroot-repository https://example.invalid/buildroot.git --output "$ARTIFACTS/repo.json"

cp "$LEGAL/manifest.csv" "$TMPDIR_TEST/manifest.csv"
python3 - "$LEGAL/manifest.csv" <<'PY'
import pathlib
import sys

pathlib.Path(sys.argv[1]).write_bytes(b"X" * (64 * 1024 + 1))
PY
expect_failure unbounded-manifest-header python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --output "$ARTIFACTS/huge-header.json"
mv "$TMPDIR_TEST/manifest.csv" "$LEGAL/manifest.csv"

truncate -s $((8 * 1024 * 1024 * 1024)) "$LEGAL/sources/aggregate-overflow.tar"
expect_failure unbounded-aggregate-material python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --output "$ARTIFACTS/aggregate-overflow.json"
rm "$LEGAL/sources/aggregate-overflow.tar"

python3 - "$ARTIFACTS/one.json" "$ARTIFACTS/tampered.json" <<'PY'
import json
import pathlib
import sys

source = pathlib.Path(sys.argv[1])
inventory = json.loads(source.read_text(encoding="ascii"))
inventory["scope"]["is_sbom"] = True
pathlib.Path(sys.argv[2]).write_text(
    json.dumps(inventory, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
PY
expect_failure overclaim python3 "$GENERATOR" verify \
    --artifact-dir "$ARTIFACTS" "$ARTIFACTS/tampered.json"

python3 - "$ARTIFACTS/one.json" "$ARTIFACTS/widened.json" <<'PY'
import json
import pathlib
import sys

inventory = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
inventory["unexpected"] = "schema widening"
pathlib.Path(sys.argv[2]).write_text(
    json.dumps(inventory, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
PY
expect_failure widened-schema python3 "$GENERATOR" verify \
    --artifact-dir "$ARTIFACTS" "$ARTIFACTS/widened.json"

python3 - "$ARTIFACTS/one.json" "$ARTIFACTS/string-epoch.json" <<'PY'
import json
import pathlib
import sys

inventory = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
inventory["source_date_epoch"] = str(inventory["source_date_epoch"])
pathlib.Path(sys.argv[2]).write_text(
    json.dumps(inventory, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
PY
expect_failure coerced-epoch python3 "$GENERATOR" verify \
    --artifact-dir "$ARTIFACTS" "$ARTIFACTS/string-epoch.json"

truncate -s $((64 * 1024 * 1024 + 1)) "$ARTIFACTS/oversized-inventory.json"
expect_failure unbounded-inventory-read python3 "$GENERATOR" verify \
    --artifact-dir "$ARTIFACTS" "$ARTIFACTS/oversized-inventory.json"

python3 - "$ARTIFACTS/one.json" "$ARTIFACTS/unsafe-path.json" <<'PY'
import hashlib
import json
import pathlib
import sys

inventory = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
entry = inventory["legal_info"]["files"][0]
entry["path"] = "../escape"
digest = hashlib.sha256()
for item in inventory["legal_info"]["files"]:
    digest.update(item["path"].encode("utf-8"))
    digest.update(b"\0")
    digest.update(item["sha256"].encode("ascii"))
    digest.update(b"\0")
    digest.update(str(item["size"]).encode("ascii"))
    digest.update(b"\n")
inventory["legal_info"]["sha256"] = digest.hexdigest()
pathlib.Path(sys.argv[2]).write_text(
    json.dumps(inventory, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
PY
expect_failure unsafe-path python3 "$GENERATOR" verify \
    --artifact-dir "$ARTIFACTS" "$ARTIFACTS/unsafe-path.json"

python3 - "$ARTIFACTS/one.json" "$ARTIFACTS/oversized-material.json" <<'PY'
import hashlib
import json
import pathlib
import sys

inventory = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
inventory["legal_info"]["files"][0]["size"] = 8 * 1024 * 1024 * 1024 + 1
digest = hashlib.sha256()
for item in inventory["legal_info"]["files"]:
    digest.update(item["path"].encode("utf-8"))
    digest.update(b"\0")
    digest.update(item["sha256"].encode("ascii"))
    digest.update(b"\0")
    digest.update(str(item["size"]).encode("ascii"))
    digest.update(b"\n")
inventory["legal_info"]["sha256"] = digest.hexdigest()
pathlib.Path(sys.argv[2]).write_text(
    json.dumps(inventory, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
PY
expect_failure oversized-material-claim python3 "$GENERATOR" verify \
    --artifact-dir "$ARTIFACTS" "$ARTIFACTS/oversized-material.json"

python3 - "$ARTIFACTS/one.json" "$ARTIFACTS/missing-manifest.json" <<'PY'
import hashlib
import json
import pathlib
import sys

inventory = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
files = [entry for entry in inventory["legal_info"]["files"] if entry["path"] != "manifest.csv"]
inventory["legal_info"]["files"] = files
inventory["legal_info"]["file_count"] = len(files)
digest = hashlib.sha256()
for item in files:
    digest.update(item["path"].encode("utf-8"))
    digest.update(b"\0")
    digest.update(item["sha256"].encode("ascii"))
    digest.update(b"\0")
    digest.update(str(item["size"]).encode("ascii"))
    digest.update(b"\n")
inventory["legal_info"]["sha256"] = digest.hexdigest()
pathlib.Path(sys.argv[2]).write_text(
    json.dumps(inventory, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
PY
expect_failure missing-manifest-claim python3 "$GENERATOR" verify \
    --artifact-dir "$ARTIFACTS" "$ARTIFACTS/missing-manifest.json"

# Anti-orphan the production path: release builds delete stale legal-info,
# regenerate it after Buildroot, re-inspect it while still in the build volume,
# bind the inventory in the signed source-closure receipt, and publish its name.
grep -Fq 'rm -rf buildroot/output/legal-info' "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'BR2_EXTERNAL=/build/dcentos/br2_external_dcentos legal-info' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'buildroot_legal_inventory.py generate' "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq -- '--legal-info-dir /build/dcentos/buildroot/output/legal-info' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq -- '--artifact "$BUILDROOT_LEGAL_INVENTORY_PATH"' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'buildroot_legal_inventory=$(basename "$BUILDROOT_LEGAL_INVENTORY_PATH")' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'dcent_cleanup_failed_release_evidence' "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq '"$OUTPUT_DIR/${TARBALL_NAME}.buildroot-legal.json"' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq '"$OUTPUT_DIR/${TARBALL_NAME}.source-closure.json.sig"' \
    "$SCRIPT_DIR/build_in_docker.sh"

echo "Buildroot legal inventory: PASS"
