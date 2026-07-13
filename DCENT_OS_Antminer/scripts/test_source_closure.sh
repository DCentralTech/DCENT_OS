#!/usr/bin/env bash
# Offline contract test for deterministic partial source-closure receipts.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GENERATOR="$SCRIPT_DIR/source_closure.py"
SOURCE_SNAPSHOT_HELPER="$SCRIPT_DIR/source_snapshot.py"
INVOCATION_HELPER="$SCRIPT_DIR/release_invocation.py"
BUILD_INPUT_SNAPSHOT_HELPER="$SCRIPT_DIR/build_input_snapshot.py"
RESULT_STAGE_HELPER="$SCRIPT_DIR/release_result_stage.py"
RELEASE_SET_HELPER="$SCRIPT_DIR/release_set_publication.py"
PORTABLE_EVIDENCE_HELPER="$SCRIPT_DIR/portable_release_evidence.py"

for tool in python3 git tar sha256sum openssl; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "ERROR: source-closure test requires $tool" >&2
        exit 1
    }
done

expect_failure() {
    local label="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        echo "ERROR: source closure accepted invalid input: $label" >&2
        exit 1
    fi
}

TMPDIR_TEST="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_TEST"' EXIT
REPO="$TMPDIR_TEST/repo"
ARTIFACT_DIR="$TMPDIR_TEST/artifacts"
mkdir -p \
    "$REPO/dcentrald" \
    "$REPO/scripts" \
    "$REPO/DCENT_OS_Antminer/scripts" \
    "$REPO/knowledge-base/extractions/s9" \
    "$REPO/knowledge-base/extractions/s19j" \
    "$REPO/DCENT_OS_Antminer/br2_external_dcentos/configs" \
    "$REPO/DCENT_OS_Antminer/br2_external_dcentos/board/example" \
    "$ARTIFACT_DIR/stage/sysupgrade-test"

printf 'lockfile fixture\n' > "$REPO/dcentrald/Cargo.lock"
cat > "$REPO/scripts/build-dcentrald.sh" <<'EOF'
#!/bin/sh
cargo build --release --locked --target armv7-unknown-linux-musleabihf
EOF
printf 'BR2_TARGET_GENERIC_HOSTNAME="dcentos"\n' > \
    "$REPO/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment"
printf 'BR2_arm=y\n' > "$REPO/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig"
printf 'BR2_arm=y\n' > "$REPO/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_am2_s19jpro_defconfig"
printf 'BR2_arm=y\n' > "$REPO/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_am2_s19pro_defconfig"
printf 'overlay fixture\n' > "$REPO/DCENT_OS_Antminer/br2_external_dcentos/board/example/overlay.txt"
printf 'FROM scratch\n' > "$REPO/Dockerfile.build"
printf 's9 kernel fixture\n' > "$REPO/knowledge-base/extractions/s9/kernel.bin"
printf 's9 dtb fixture\n' > "$REPO/knowledge-base/extractions/s9/s9_devicetree.dtb"
printf 'am2 kernel fixture\n' > "$REPO/knowledge-base/extractions/s19j/kernel.bin"
printf 'am2 bitstream fixture\n' > "$REPO/knowledge-base/extractions/s19j/fpga_bitstream.bit"
cat > "$REPO/.gitignore" <<'EOF'
knowledge-base/extractions/
EOF
write_build_input_manifest() {
    : > "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"
    for input in \
        knowledge-base/extractions/s9/kernel.bin \
        knowledge-base/extractions/s9/s9_devicetree.dtb \
        knowledge-base/extractions/s19j/kernel.bin \
        knowledge-base/extractions/s19j/fpga_bitstream.bit; do
        printf '%s  %s\n' "$(sha256sum "$REPO/$input" | awk '{print $1}')" "$input" \
            >> "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"
    done
}
write_build_input_manifest

git init -q "$REPO"
git -C "$REPO" config user.name source-closure-test
git -C "$REPO" config user.email source-closure-test.invalid
git -C "$REPO" add .
GIT_AUTHOR_DATE='2023-11-14T22:13:20Z' \
GIT_COMMITTER_DATE='2023-11-14T22:13:20Z' \
    git -C "$REPO" commit -q -m fixture
COMMIT="$(git -C "$REPO" rev-parse HEAD)"

mkdir -p "$TMPDIR_TEST/source-snapshots" "$TMPDIR_TEST/invocations" \
    "$TMPDIR_TEST/build-input-snapshots" "$TMPDIR_TEST/result-stages"
SOURCE_SNAPSHOT_RESULT="$(python3 "$SOURCE_SNAPSHOT_HELPER" create \
    --repo-root "$REPO" --commit "$COMMIT" \
    --stage-parent "$TMPDIR_TEST/source-snapshots")"
SOURCE_SNAPSHOT="$(printf '%s\n' "$SOURCE_SNAPSHOT_RESULT" | \
    python3 "$SOURCE_SNAPSHOT_HELPER" query-result --field snapshot)"
SOURCE_TREE="$(printf '%s\n' "$SOURCE_SNAPSHOT_RESULT" | \
    python3 "$SOURCE_SNAPSHOT_HELPER" query-result --field tree)"
SOURCE_SNAPSHOT_ID="$(printf '%s\n' "$SOURCE_SNAPSHOT_RESULT" | \
    python3 "$SOURCE_SNAPSHOT_HELPER" query-result --field snapshot_id)"
SOURCE_DESTROY_TOKEN="$(printf '%s\n' "$SOURCE_SNAPSHOT_RESULT" | \
    python3 "$SOURCE_SNAPSHOT_HELPER" query-result --field destroy_token)"
SOURCE_SNAPSHOT_DESCRIPTOR_SHA256="$(sha256sum "$SOURCE_SNAPSHOT" | awk '{print $1}')"

INVOCATION_RESULT="$(python3 "$INVOCATION_HELPER" create \
    --stage-parent "$TMPDIR_TEST/invocations" --name s9)"
RELEASE_INVOCATION="$(printf '%s\n' "$INVOCATION_RESULT" | \
    python3 "$INVOCATION_HELPER" query-result --field stage)"
RELEASE_INVOCATION_ID="$(printf '%s\n' "$INVOCATION_RESULT" | \
    python3 "$INVOCATION_HELPER" query-result --field invocation_id)"
RELEASE_INVOCATION_CAPABILITY="$(printf '%s\n' "$INVOCATION_RESULT" | \
    python3 "$INVOCATION_HELPER" query-result --field capability)"
RELEASE_INVOCATION_DESCRIPTOR_SHA256="$(sha256sum \
    "$RELEASE_INVOCATION/invocation.json" | awk '{print $1}')"
RESULT_STAGE_RESULT="$(python3 "$RESULT_STAGE_HELPER" create \
    --stage-parent "$TMPDIR_TEST/result-stages" \
    --invocation-stage "$RELEASE_INVOCATION")"
RESULT_STAGE="$(printf '%s\n' "$RESULT_STAGE_RESULT" | \
    python3 "$RESULT_STAGE_HELPER" query-result --field stage)"
RESULT_CAPABILITY="$(printf '%s\n' "$RESULT_STAGE_RESULT" | \
    python3 "$RESULT_STAGE_HELPER" query-result --field capability)"
python3 "$RESULT_STAGE_HELPER" seal --capability "$RESULT_CAPABILITY" \
    --invocation-stage "$RELEASE_INVOCATION" "$RESULT_STAGE" >/dev/null

BUILD_INPUT_SNAPSHOT_RESULT="$(python3 "$BUILD_INPUT_SNAPSHOT_HELPER" create \
    --repo-root "$REPO" \
    --selection-root "$SOURCE_TREE" \
    --build-input-manifest "$SOURCE_TREE/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target s9 --stage-parent "$TMPDIR_TEST/build-input-snapshots")"
BUILD_INPUT_SNAPSHOT="$(printf '%s\n' "$BUILD_INPUT_SNAPSHOT_RESULT" | \
    python3 "$BUILD_INPUT_SNAPSHOT_HELPER" query-result --field snapshot)"
BUILD_INPUT_DESTROY_TOKEN="$(printf '%s\n' "$BUILD_INPUT_SNAPSHOT_RESULT" | \
    python3 "$BUILD_INPUT_SNAPSHOT_HELPER" query-result --field destroy_token)"
CARGO_BUILD_INPUT_SNAPSHOT_RESULT="$(python3 "$BUILD_INPUT_SNAPSHOT_HELPER" create \
    --repo-root "$REPO" --selection-root "$SOURCE_TREE" \
    --build-input-manifest "$SOURCE_TREE/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target cargo-workspace --stage-parent "$TMPDIR_TEST/build-input-snapshots")"
CARGO_BUILD_INPUT_SNAPSHOT="$(printf '%s\n' "$CARGO_BUILD_INPUT_SNAPSHOT_RESULT" | \
    python3 "$BUILD_INPUT_SNAPSHOT_HELPER" query-result --field snapshot)"
CARGO_BUILD_INPUT_DESTROY_TOKEN="$(printf '%s\n' "$CARGO_BUILD_INPUT_SNAPSHOT_RESULT" | \
    python3 "$BUILD_INPUT_SNAPSHOT_HELPER" query-result --field destroy_token)"

printf 'kernel fixture\n' > "$ARTIFACT_DIR/stage/sysupgrade-test/kernel"
printf 'root fixture\n' > "$ARTIFACT_DIR/stage/sysupgrade-test/root"
tar --sort=name --format=ustar --mtime='@1700000000' \
    --owner=0 --group=0 --numeric-owner \
    -cf "$ARTIFACT_DIR/release.tar" -C "$ARTIFACT_DIR/stage" sysupgrade-test

for binary_name in dcentrald dcentos-init; do
    printf 'prebuilt fixture: %s\n' "$binary_name" > \
        "$ARTIFACT_DIR/release.tar.prebuilt-rust.${binary_name}.bin"
    python3 - \
        "$ARTIFACT_DIR/release.tar.prebuilt-rust.${binary_name}.bin" \
        "$ARTIFACT_DIR/release.tar.prebuilt-rust.${binary_name}.build-receipt.json" \
        "$binary_name" "$RELEASE_INVOCATION_ID" "$SOURCE_SNAPSHOT_ID" \
        "$SOURCE_SNAPSHOT_DESCRIPTOR_SHA256" \
        "$RELEASE_INVOCATION_DESCRIPTOR_SHA256" "$COMMIT" "$SCRIPT_DIR" \
        "$CARGO_BUILD_INPUT_SNAPSHOT" <<'PY'
import hashlib
import json
import pathlib
import sys

binary_path = pathlib.Path(sys.argv[1])
receipt_path = pathlib.Path(sys.argv[2])
name = sys.argv[3]
sys.path.insert(0, sys.argv[9])
import build_input_snapshot

build_input_descriptor = build_input_snapshot.verify_snapshot(
    pathlib.Path(sys.argv[10]), "cargo-workspace"
)
build_inputs = {
    "claim": (
        "pre-build-external-input-snapshot-consistency-"
        "not-compiler-consumption-or-build-causality-proof"
    ),
    "selection_authority": (
        "manifest-from-same-git-authenticated-release-capsule-source-snapshot"
    ),
    "evidence": build_input_snapshot.snapshot_evidence(build_input_descriptor),
}
release_capsule = {
    "schema": "org.dcentral.dcentos.release-capsule-lineage.v2",
    "release_invocation_descriptor_sha256": sys.argv[7],
    "release_invocation_id": sys.argv[4],
    "source_snapshot_id": sys.argv[5],
    "source_snapshot_descriptor_sha256": sys.argv[6],
}
binary = binary_path.read_bytes()
receipt = {
    "schema_version": 4,
    "claim": "declared-release-capsule-and-post-build-snapshot-consistency-not-build-causality-or-reproducibility-proof",
    "release_capsule": release_capsule,
    "build_inputs": build_inputs,
    "target_triple": "armv7-unknown-linux-musleabihf",
    "profile": "release",
    "build_variant": "zynq",
    "git": {"commit": sys.argv[8], "source_kind": "exact-git-object-snapshot"},
    "build_environment": {},
    "builder": {
        "kind": "docker-cross",
        "base_reference": "rust@sha256:" + "1" * 64,
        "image_id": "sha256:" + "2" * 64,
        "package_resolution": "fixture-not-reproducible",
    },
    "toolchain_context": {},
    "compile_environment": {"entries": {}},
    "source_inventory_sha256": "0" * 64,
    "source_inventory": [],
    "cargo_metadata": {},
    "binary": {
        "name": name,
        "path": f"target/armv7-unknown-linux-musleabihf/release/{name}",
        "size": len(binary),
        "sha256": hashlib.sha256(binary).hexdigest(),
    },
}
receipt_path.write_text(
    json.dumps(receipt, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY
done

COMMON_ARGS=(
    --repo-root "$REPO"
    --source-commit "$COMMIT"
    --source-snapshot "$SOURCE_SNAPSHOT"
    --release-invocation "$RELEASE_INVOCATION"
    --source-date-epoch 1700000000
    --target s9
    --arch armv7-unknown-linux-musleabihf
    --cargo-lock "$SOURCE_TREE/dcentrald/Cargo.lock"
    --cargo-build-script "$SOURCE_TREE/scripts/build-dcentrald.sh"
    --buildroot-repository https://github.com/buildroot/buildroot.git
    --buildroot-commit 7c8edc1b402efcd7bba2dabfe0b3be877adaed7a
    --external-tree "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos"
    --toolchain-id linaro-7.2.1:test-fixture
    --toolchain-sha256 cee0087b1f1205b73996651b99acd3a926d136e71047048f1758ffcec69b1ca2
    --toolchain-verified
    --container-image-id sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    --container-definition "$SOURCE_TREE/Dockerfile.build"
    --build-input-snapshot "$BUILD_INPUT_SNAPSHOT"
    --artifact "$ARTIFACT_DIR/release.tar"
    --prebuilt-rust-input dcentrald \
        "$ARTIFACT_DIR/release.tar.prebuilt-rust.dcentrald.bin" \
        "$ARTIFACT_DIR/release.tar.prebuilt-rust.dcentrald.build-receipt.json"
    --prebuilt-rust-input dcentos-init \
        "$ARTIFACT_DIR/release.tar.prebuilt-rust.dcentos-init.bin" \
        "$ARTIFACT_DIR/release.tar.prebuilt-rust.dcentos-init.build-receipt.json"
)

VERIFY_CAPSULE_ARGS=(
    --source-snapshot "$SOURCE_SNAPSHOT"
    --release-invocation "$RELEASE_INVOCATION"
    --build-input-snapshot "$BUILD_INPUT_SNAPSHOT"
)

verify_closure() {
    python3 "$GENERATOR" verify "${VERIFY_CAPSULE_ARGS[@]}" "$@"
}

python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --output "$ARTIFACT_DIR/one.json" >/dev/null
touch -t 202501020304 "$REPO/Dockerfile.build" "$ARTIFACT_DIR/release.tar"
python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --output "$ARTIFACT_DIR/two.json" >/dev/null
cmp "$ARTIFACT_DIR/one.json" "$ARTIFACT_DIR/two.json"
if python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --output "$ARTIFACT_DIR/reversed-config-order.json" >/dev/null 2>&1; then
    echo "ERROR: source closure accepted wrong target config merge order" >&2
    exit 1
fi
verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$ARTIFACT_DIR" \
    "$ARTIFACT_DIR/one.json" >/dev/null

# Schema v3 remains historical verification-only. Build one canonical fixture
# from the v4 bytes without exposing any schema-v3 generation CLI.
V3_DIR="$TMPDIR_TEST/historical-v3"
mkdir -p "$V3_DIR"
cp "$ARTIFACT_DIR/release.tar" "$V3_DIR/release.tar"
cp "$ARTIFACT_DIR"/release.tar.prebuilt-rust.* "$V3_DIR/"
python3 - "$SCRIPT_DIR" "$ARTIFACT_DIR/one.json" "$V3_DIR" <<'PY'
import hashlib
import json
import pathlib
import sys

sys.path.insert(0, sys.argv[1])
import source_closure

manifest = json.loads(pathlib.Path(sys.argv[2]).read_text(encoding="ascii"))
directory = pathlib.Path(sys.argv[3])
manifest["schema"] = source_closure.HISTORICAL_SCHEMA
manifest.pop("release_capsule")
manifest["source"]["tree_state"] = "clean"
manifest["scope"] = source_closure.closure_scope(
    manifest["scope"]["receipt_authentication"], source_closure.HISTORICAL_SCHEMA
)
for entry in manifest["prebuilt_rust_inputs"]["entries"]:
    entry["receipt_schema_version"] = 3
    entry["receipt_claim"] = source_closure.HISTORICAL_PREBUILT_RUST_RECEIPT_CLAIM
    receipt_path = directory / entry["receipt"]["path"]
    receipt = json.loads(receipt_path.read_text(encoding="ascii"))
    receipt["schema_version"] = 3
    receipt.pop("release_capsule")
    receipt.pop("build_inputs")
    receipt["claim"] = source_closure.HISTORICAL_PREBUILT_RUST_RECEIPT_CLAIM
    receipt["git"] = {
        "commit": manifest["source"]["commit"],
        "tree_state": "clean",
        "status_sha256": "0" * 64,
    }
    receipt["binary"]["path"] = (
        "DCENT_OS_Antminer/dcentrald/" + receipt["binary"]["path"]
    )
    raw = source_closure.canonical_bytes(receipt)
    receipt_path.write_bytes(raw)
    entry["receipt"] = {
        "path": receipt_path.name,
        "sha256": hashlib.sha256(raw).hexdigest(),
        "size": len(raw),
    }
(directory / "source-closure-v3.json").write_bytes(
    source_closure.canonical_bytes(manifest)
)
PY
python3 "$GENERATOR" verify --repo-root "$REPO" --artifact-dir "$V3_DIR" \
    "$V3_DIR/source-closure-v3.json" >/dev/null
python3 - "$V3_DIR/source-closure-v3.json" <<'PY'
import json
import pathlib
import sys

manifest = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
assert manifest["scope"]["binds"] == (
    "declared source/build definitions, retained packaging input snapshots, "
    "and produced artifact bytes"
)
assert any("clean Git tree" in item for item in manifest["scope"]["unresolved"])
PY

# The live checkout is neither a generation nor a verification byte source for
# schema v4 after the Git-backed source snapshot has been authenticated.
cp "$REPO/Dockerfile.build" "$TMPDIR_TEST/Dockerfile.build.saved"
cp "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" "$TMPDIR_TEST/build_inputs.manifest.saved"
printf 'live checkout mutation\n' >> "$REPO/Dockerfile.build"
printf '# live manifest mutation after authenticated snapshots\n' >> \
    "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"
python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --output "$ARTIFACT_DIR/live-manifest-mutated.json" >/dev/null
cmp "$ARTIFACT_DIR/one.json" "$ARTIFACT_DIR/live-manifest-mutated.json"
verify_closure --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" \
    "$ARTIFACT_DIR/one.json" >/dev/null
cp "$TMPDIR_TEST/Dockerfile.build.saved" "$REPO/Dockerfile.build"
cp "$TMPDIR_TEST/build_inputs.manifest.saved" "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"

expect_failure v4-missing-source-snapshot python3 "$GENERATOR" verify \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" \
    --release-invocation "$RELEASE_INVOCATION" \
    --build-input-snapshot "$BUILD_INPUT_SNAPSHOT" \
    "$ARTIFACT_DIR/one.json"
expect_failure v4-missing-release-invocation python3 "$GENERATOR" verify \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" \
    --source-snapshot "$SOURCE_SNAPSHOT" \
    --build-input-snapshot "$BUILD_INPUT_SNAPSHOT" \
    "$ARTIFACT_DIR/one.json"
expect_failure v4-missing-build-input-snapshot python3 "$GENERATOR" verify \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" \
    --source-snapshot "$SOURCE_SNAPSHOT" \
    --release-invocation "$RELEASE_INVOCATION" \
    "$ARTIFACT_DIR/one.json"
SWAPPED_INVOCATION_RESULT="$(python3 "$INVOCATION_HELPER" create \
    --stage-parent "$TMPDIR_TEST/invocations" --name swapped-authority)"
SWAPPED_INVOCATION="$(printf '%s\n' "$SWAPPED_INVOCATION_RESULT" | \
    python3 "$INVOCATION_HELPER" query-result --field stage)"
expect_failure swapped-release-invocation-authority python3 "$GENERATOR" verify \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" \
    --source-snapshot "$SOURCE_SNAPSHOT" \
    --release-invocation "$SWAPPED_INVOCATION" \
    --build-input-snapshot "$BUILD_INPUT_SNAPSHOT" \
    "$ARTIFACT_DIR/one.json"

LEGACY_BUILD_INPUT_RESULT="$(python3 "$BUILD_INPUT_SNAPSHOT_HELPER" create \
    --repo-root "$REPO" \
    --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target s9 --stage-parent "$TMPDIR_TEST/build-input-snapshots")"
LEGACY_BUILD_INPUT_SNAPSHOT="$(printf '%s\n' "$LEGACY_BUILD_INPUT_RESULT" | \
    python3 "$BUILD_INPUT_SNAPSHOT_HELPER" query-result --field snapshot)"
expect_failure v1-build-input-snapshot-in-v4-generate python3 "$GENERATOR" generate \
    "${COMMON_ARGS[@]}" --build-input-snapshot "$LEGACY_BUILD_INPUT_SNAPSHOT" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --output "$ARTIFACT_DIR/v1-build-input.json"
expect_failure v1-build-input-snapshot-in-v4-verify verify_closure \
    --build-input-snapshot "$LEGACY_BUILD_INPUT_SNAPSHOT" \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" "$ARTIFACT_DIR/one.json"

MISMATCH_SELECTION_ROOT="$TMPDIR_TEST/mismatched-selection-root"
mkdir -p "$MISMATCH_SELECTION_ROOT/DCENT_OS_Antminer/scripts"
cp "$SOURCE_TREE/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    "$MISMATCH_SELECTION_ROOT/DCENT_OS_Antminer/scripts/build_inputs.manifest"
printf '# independently modified selection authority\n' >> \
    "$MISMATCH_SELECTION_ROOT/DCENT_OS_Antminer/scripts/build_inputs.manifest"
MISMATCH_BUILD_INPUT_RESULT="$(python3 "$BUILD_INPUT_SNAPSHOT_HELPER" create \
    --repo-root "$REPO" --selection-root "$MISMATCH_SELECTION_ROOT" \
    --build-input-manifest "$MISMATCH_SELECTION_ROOT/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target s9 --stage-parent "$TMPDIR_TEST/build-input-snapshots")"
MISMATCH_BUILD_INPUT_SNAPSHOT="$(printf '%s\n' "$MISMATCH_BUILD_INPUT_RESULT" | \
    python3 "$BUILD_INPUT_SNAPSHOT_HELPER" query-result --field snapshot)"
expect_failure mismatched-selection-manifest-in-v4-generate python3 "$GENERATOR" generate \
    "${COMMON_ARGS[@]}" --build-input-snapshot "$MISMATCH_BUILD_INPUT_SNAPSHOT" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --output "$ARTIFACT_DIR/mismatched-selection.json"
expect_failure mismatched-selection-manifest-in-v4-verify verify_closure \
    --build-input-snapshot "$MISMATCH_BUILD_INPUT_SNAPSHOT" \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" "$ARTIFACT_DIR/one.json"

CAPSULE_TAMPER_DIR="$TMPDIR_TEST/capsule-tamper"
mkdir -p "$CAPSULE_TAMPER_DIR"
python3 - "$ARTIFACT_DIR/one.json" "$CAPSULE_TAMPER_DIR" <<'PY'
import copy
import json
import pathlib
import sys

source = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
output = pathlib.Path(sys.argv[2])
mutations = {}
malformed = copy.deepcopy(source)
malformed["release_capsule"]["unexpected"] = "forbidden"
mutations["malformed"] = malformed
mixed = copy.deepcopy(source)
mixed["release_capsule"]["release_invocation_id"] = "f" * 64
mutations["mixed"] = mixed
swapped = copy.deepcopy(source)
swapped["release_capsule"]["source_snapshot_id"] = "e" * 64
mutations["swapped"] = swapped
for name, value in mutations.items():
    (output / f"{name}.json").write_text(
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="ascii",
    )
PY
for malformed_capsule in "$CAPSULE_TAMPER_DIR"/*.json; do
    expect_failure "capsule-$(basename "$malformed_capsule")" verify_closure \
        --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" "$malformed_capsule"
done

cp "$SOURCE_SNAPSHOT" "$TMPDIR_TEST/source-snapshot.saved"
SOURCE_SNAPSHOT_OWNER="$(dirname "$SOURCE_SNAPSHOT")/.dcentos-source-snapshot-owner"
cp "$SOURCE_SNAPSHOT_OWNER" "$TMPDIR_TEST/source-snapshot-owner.saved"
chmod u+w "$SOURCE_SNAPSHOT"
chmod u+w "$SOURCE_SNAPSHOT_OWNER"
python3 - "$SCRIPT_DIR" "$SOURCE_SNAPSHOT" "$SOURCE_SNAPSHOT_OWNER" <<'PY'
import hashlib
import json
import pathlib
import sys

sys.path.insert(0, sys.argv[1])
import source_snapshot

path = pathlib.Path(sys.argv[2])
value = json.loads(path.read_text(encoding="ascii"))
value["commit"]["sha256"] = "0" * 64
value.pop("snapshot_id")
value["snapshot_id"] = hashlib.sha256(source_snapshot.canonical_bytes(value)).hexdigest()
path.write_bytes(source_snapshot.canonical_bytes(value))
owner_path = pathlib.Path(sys.argv[3])
owner = json.loads(owner_path.read_text(encoding="ascii"))
owner["snapshot_id"] = value["snapshot_id"]
owner_path.write_bytes(source_snapshot.canonical_bytes(owner))
PY
# The forged descriptor is internally consistent and passes local snapshot
# validation. Only reconstruction from authenticated Git objects exposes it.
python3 "$SOURCE_SNAPSHOT_HELPER" verify --commit "$COMMIT" \
    "$SOURCE_SNAPSHOT" >/dev/null
expect_failure forged-source-snapshot-vs-git verify_closure \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" "$ARTIFACT_DIR/one.json"
cp "$TMPDIR_TEST/source-snapshot.saved" "$SOURCE_SNAPSHOT"
cp "$TMPDIR_TEST/source-snapshot-owner.saved" "$SOURCE_SNAPSHOT_OWNER"
EXPECTED_RELEASE_SHA="$(sha256sum "$ARTIFACT_DIR/release.tar" | awk '{print $1}')"
QUERIED_RELEASE_SHA="$(python3 "$GENERATOR" query-artifact \
    --manifest "$ARTIFACT_DIR/one.json" \
    --path release.tar \
    --field sha256)"
[ "$QUERIED_RELEASE_SHA" = "$EXPECTED_RELEASE_SHA" ]

DISPARATE_ARTIFACT_DIR="$TMPDIR_TEST/disparate-artifact"
mkdir -p "$DISPARATE_ARTIFACT_DIR"
printf 'disparate\n' > "$DISPARATE_ARTIFACT_DIR/extra.json"
expect_failure disparate-artifact-parents python3 "$GENERATOR" generate \
    "${COMMON_ARGS[@]}" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --artifact "$DISPARATE_ARTIFACT_DIR/extra.json" \
    --output "$ARTIFACT_DIR/disparate.json"

SIGNED_RECEIPT="$ARTIFACT_DIR/signed-policy.json"
printf '{"inventory":"buildroot-legal"}\n' > "$ARTIFACT_DIR/release.tar.buildroot-legal.json"
printf '{"inventory":"rust-dependencies"}\n' > "$ARTIFACT_DIR/release.tar.rust-dependencies.json"
printf '{"inventory":"rootfs-ownership"}\n' > "$ARTIFACT_DIR/release.tar.rootfs-ownership.json"
printf 'release marker fixture\n' > "$ARTIFACT_DIR/release.tar.release.txt"
python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --artifact "$ARTIFACT_DIR/release.tar.buildroot-legal.json" \
    --artifact "$ARTIFACT_DIR/release.tar.rust-dependencies.json" \
    --artifact "$ARTIFACT_DIR/release.tar.rootfs-ownership.json" \
    --receipt-authentication detached_ed25519_required_for_release \
    --output "$SIGNED_RECEIPT" >/dev/null
expect_failure release-receipt-missing-authentication verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$ARTIFACT_DIR" \
    "$SIGNED_RECEIPT"
python3 - "$ARTIFACT_DIR/one.json" "$SIGNED_RECEIPT" <<'PY'
import json
import pathlib
import sys

unsigned = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
required = json.loads(pathlib.Path(sys.argv[2]).read_text(encoding="ascii"))
assert unsigned["scope"]["receipt_authentication"] == "not_independently_signed"
assert required["scope"]["receipt_authentication"] == "detached_ed25519_required_for_release"
assert "receipt authenticity depends on the authenticated release channel" in unsigned["scope"]["unresolved"]
assert "receipt authenticity depends on the authenticated release channel" not in required["scope"]["unresolved"]
assert unsigned["scope"]["build_execution_attestation"] == "not_attested"
assert required["scope"]["build_execution_attestation"] == "not_attested"
assert any("consumer-to-artifact causality" in item for item in required["scope"]["unresolved"])
assert any("do not prove compiler causality" in item for item in required["scope"]["unresolved"])
PY

# The canonical receipt is the signed release evidence envelope. Its detached
# signature authenticates exactly the bounded claims already present in the
# receipt; it does not expand them into SBOM/SPDX/reproducibility claims.
PRIVATE_KEY="$TMPDIR_TEST/release-ed25519.pem"
PUBLIC_KEY="$TMPDIR_TEST/release-ed25519.pub"
WRONG_PRIVATE_KEY="$TMPDIR_TEST/wrong-ed25519.pem"
WRONG_PUBLIC_KEY="$TMPDIR_TEST/wrong-ed25519.pub"
openssl genpkey -algorithm Ed25519 -out "$PRIVATE_KEY" >/dev/null 2>&1
openssl pkey -in "$PRIVATE_KEY" -pubout -out "$PUBLIC_KEY" >/dev/null 2>&1
openssl genpkey -algorithm Ed25519 -out "$WRONG_PRIVATE_KEY" >/dev/null 2>&1
openssl pkey -in "$WRONG_PRIVATE_KEY" -pubout -out "$WRONG_PUBLIC_KEY" >/dev/null 2>&1

bash "$SCRIPT_DIR/sign_release_receipt.sh" \
    "$SIGNED_RECEIPT" "$PRIVATE_KEY" "$PUBLIC_KEY" \
    "$SIGNED_RECEIPT.sig" >/dev/null
openssl pkeyutl -verify -rawin -pubin \
    -inkey "$PUBLIC_KEY" \
    -sigfile "$SIGNED_RECEIPT.sig" \
    -in "$SIGNED_RECEIPT" >/dev/null
[ "$(wc -c < "$SIGNED_RECEIPT.sig" | tr -d '[:space:]')" = "64" ]
verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$ARTIFACT_DIR" \
    --signature "$SIGNED_RECEIPT.sig" \
    --public-key "$PUBLIC_KEY" \
    "$SIGNED_RECEIPT" >/dev/null

# Retain only canonical audit projections. The post-cleanup verifier below
# must succeed after the original source/input/invocation authorities are gone.
PORTABLE_PROJECTIONS="$TMPDIR_TEST/portable-projections"
mkdir -p "$PORTABLE_PROJECTIONS"
cp "$SOURCE_SNAPSHOT" "$PORTABLE_PROJECTIONS/release-source-snapshot.json"
cp "$RELEASE_INVOCATION/invocation.json" \
    "$PORTABLE_PROJECTIONS/release-invocation.json"
cp "$BUILD_INPUT_SNAPSHOT" \
    "$PORTABLE_PROJECTIONS/release-packaging-input.json"
cp "$CARGO_BUILD_INPUT_SNAPSHOT" \
    "$PORTABLE_PROJECTIONS/release-cargo-input.json"
python3 "$RESULT_STAGE_HELPER" project-audit \
    --invocation-stage "$RELEASE_INVOCATION" "$RESULT_STAGE" \
    > "$PORTABLE_PROJECTIONS/release-result-audit.json"

# A receipt signature authenticates bytes, not the safety of paths encoded in
# those bytes. Independently reject escape/alias/duplicate artifact paths and
# archive-member paths for both unsigned and genuinely signed malformed
# receipts before joining any declared name to --artifact-dir.
for mutation in traversal absolute duplicate-artifact archive-alias duplicate-member; do
    for policy in unsigned signed; do
        if [ "$policy" = signed ]; then
            base_receipt="$SIGNED_RECEIPT"
        else
            base_receipt="$ARTIFACT_DIR/one.json"
        fi
        malformed="$ARTIFACT_DIR/malformed-$policy-$mutation.json"
        python3 - "$base_receipt" "$malformed" "$mutation" <<'PY'
import copy
import json
import pathlib
import sys

source = pathlib.Path(sys.argv[1])
destination = pathlib.Path(sys.argv[2])
mutation = sys.argv[3]
manifest = json.loads(source.read_text(encoding="ascii"))
artifact = manifest["artifacts"][0]
if mutation == "traversal":
    artifact["path"] = "../outside-release.tar"
elif mutation == "absolute":
    artifact["path"] = "/outside-release.tar"
elif mutation == "duplicate-artifact":
    manifest["artifacts"].append(copy.deepcopy(artifact))
elif mutation == "archive-alias":
    assert artifact["archive_regular_members"]
    artifact["archive_regular_members"][0]["path"] = (
        "./" + artifact["archive_regular_members"][0]["path"]
    )
elif mutation == "duplicate-member":
    assert artifact["archive_regular_members"]
    artifact["archive_regular_members"].append(
        copy.deepcopy(artifact["archive_regular_members"][0])
    )
else:
    raise AssertionError(mutation)
destination.write_text(
    json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="ascii",
)
PY
        verify_args=(
            --repo-root "$REPO"
            --artifact-dir "$ARTIFACT_DIR"
        )
        if [ "$policy" = signed ]; then
            openssl pkeyutl -sign -rawin \
                -inkey "$PRIVATE_KEY" \
                -in "$malformed" \
                -out "$malformed.sig"
            verify_args+=(--signature "$malformed.sig" --public-key "$PUBLIC_KEY")
        fi
        expect_failure "malformed-$policy-$mutation" verify_closure \
            "${verify_args[@]}" "$malformed"
    done
done
expect_failure release-receipt-missing-public-key verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$ARTIFACT_DIR" \
    --signature "$SIGNED_RECEIPT.sig" \
    "$SIGNED_RECEIPT"
expect_failure release-receipt-missing-signature verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$ARTIFACT_DIR" \
    --public-key "$PUBLIC_KEY" \
    "$SIGNED_RECEIPT"
expect_failure release-receipt-wrong-public-key verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$ARTIFACT_DIR" \
    --signature "$SIGNED_RECEIPT.sig" \
    --public-key "$WRONG_PUBLIC_KEY" \
    "$SIGNED_RECEIPT"

cp "$SIGNED_RECEIPT.sig" "$ARTIFACT_DIR/corrupt-signature.sig"
python3 - "$ARTIFACT_DIR/corrupt-signature.sig" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
data = bytearray(path.read_bytes())
data[0] ^= 0x01
path.write_bytes(data)
PY
expect_failure release-receipt-corrupt-signature verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$ARTIFACT_DIR" \
    --signature "$ARTIFACT_DIR/corrupt-signature.sig" \
    --public-key "$PUBLIC_KEY" \
    "$SIGNED_RECEIPT"
printf 'short\n' > "$ARTIFACT_DIR/short-signature.sig"
expect_failure release-receipt-short-signature verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$ARTIFACT_DIR" \
    --signature "$ARTIFACT_DIR/short-signature.sig" \
    --public-key "$PUBLIC_KEY" \
    "$SIGNED_RECEIPT"

expect_failure unsigned-receipt-cannot-be-promoted verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$ARTIFACT_DIR" \
    --signature "$SIGNED_RECEIPT.sig" \
    --public-key "$PUBLIC_KEY" \
    "$ARTIFACT_DIR/one.json"

# Deterministically replace the caller-visible receipt after Python has parsed
# it but immediately before OpenSSL runs. Verification must still use the
# immutable snapshot of the exact parsed bytes, never reopen the swapped path.
REAL_OPENSSL="$(command -v openssl)"
SWAP_BIN="$TMPDIR_TEST/swap-bin"
SWAP_RECEIPT="$ARTIFACT_DIR/swap-race-receipt.json"
mkdir -p "$SWAP_BIN"
cp "$SIGNED_RECEIPT" "$SWAP_RECEIPT"
cat > "$SWAP_BIN/openssl" <<'EOF'
#!/bin/sh
printf 'swapped-after-parse\n' >> "$DCENT_SWAP_RECEIPT"
exec "$DCENT_REAL_OPENSSL" "$@"
EOF
chmod +x "$SWAP_BIN/openssl"
PATH="$SWAP_BIN:$PATH" \
DCENT_SWAP_RECEIPT="$SWAP_RECEIPT" \
DCENT_REAL_OPENSSL="$REAL_OPENSSL" \
verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$ARTIFACT_DIR" \
    --signature "$SIGNED_RECEIPT.sig" \
    --public-key "$PUBLIC_KEY" \
    "$SWAP_RECEIPT" >/dev/null
grep -Fq 'swapped-after-parse' "$SWAP_RECEIPT"

# Model the exact evidence set downloaded from image-smoke. Verification must
# succeed without access to the producer output directory, then fail closed if
# any receipt-bound artifact, signature, or trusted key is absent.
PUBLISHED_EVIDENCE="$TMPDIR_TEST/published-evidence"
mkdir -p "$PUBLISHED_EVIDENCE"
for file in \
    release.tar \
    release.tar.buildroot-legal.json \
    release.tar.rust-dependencies.json \
    release.tar.rootfs-ownership.json \
    release.tar.release.txt \
    release.tar.prebuilt-rust.dcentrald.bin \
    release.tar.prebuilt-rust.dcentrald.build-receipt.json \
    release.tar.prebuilt-rust.dcentos-init.bin \
    release.tar.prebuilt-rust.dcentos-init.build-receipt.json \
    signed-policy.json \
    signed-policy.json.sig; do
    cp "$ARTIFACT_DIR/$file" "$PUBLISHED_EVIDENCE/$file"
done
cp "$PUBLIC_KEY" "$PUBLISHED_EVIDENCE/source-closure.ed25519.pub"
verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$PUBLISHED_EVIDENCE" \
    --signature "$PUBLISHED_EVIDENCE/signed-policy.json.sig" \
    --public-key "$PUBLISHED_EVIDENCE/source-closure.ed25519.pub" \
    "$PUBLISHED_EVIDENCE/signed-policy.json" >/dev/null
for bound in \
    release.tar \
    release.tar.buildroot-legal.json \
    release.tar.rust-dependencies.json \
    release.tar.rootfs-ownership.json \
    release.tar.prebuilt-rust.dcentrald.bin \
    release.tar.prebuilt-rust.dcentrald.build-receipt.json \
    release.tar.prebuilt-rust.dcentos-init.bin \
    release.tar.prebuilt-rust.dcentos-init.build-receipt.json; do
    mv "$PUBLISHED_EVIDENCE/$bound" "$PUBLISHED_EVIDENCE/$bound.saved"
    expect_failure "published-evidence-missing-$bound" verify_closure \
        --repo-root "$REPO" \
        --artifact-dir "$PUBLISHED_EVIDENCE" \
        --signature "$PUBLISHED_EVIDENCE/signed-policy.json.sig" \
        --public-key "$PUBLISHED_EVIDENCE/source-closure.ed25519.pub" \
        "$PUBLISHED_EVIDENCE/signed-policy.json"
    mv "$PUBLISHED_EVIDENCE/$bound.saved" "$PUBLISHED_EVIDENCE/$bound"
done
mv "$PUBLISHED_EVIDENCE/signed-policy.json.sig" "$PUBLISHED_EVIDENCE/signed-policy.json.sig.saved"
expect_failure published-evidence-missing-signature verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$PUBLISHED_EVIDENCE" \
    --signature "$PUBLISHED_EVIDENCE/signed-policy.json.sig" \
    --public-key "$PUBLISHED_EVIDENCE/source-closure.ed25519.pub" \
    "$PUBLISHED_EVIDENCE/signed-policy.json"
mv "$PUBLISHED_EVIDENCE/signed-policy.json.sig.saved" "$PUBLISHED_EVIDENCE/signed-policy.json.sig"
mv "$PUBLISHED_EVIDENCE/source-closure.ed25519.pub" "$PUBLISHED_EVIDENCE/source-closure.ed25519.pub.saved"
expect_failure published-evidence-missing-key verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$PUBLISHED_EVIDENCE" \
    --signature "$PUBLISHED_EVIDENCE/signed-policy.json.sig" \
    --public-key "$PUBLISHED_EVIDENCE/source-closure.ed25519.pub" \
    "$PUBLISHED_EVIDENCE/signed-policy.json"
mv "$PUBLISHED_EVIDENCE/source-closure.ed25519.pub.saved" "$PUBLISHED_EVIDENCE/source-closure.ed25519.pub"

bash "$SCRIPT_DIR/sign_release_receipt.sh" \
    "$SIGNED_RECEIPT" "$PRIVATE_KEY" "$PUBLIC_KEY" \
    "$SIGNED_RECEIPT.sig.second" >/dev/null
cmp "$SIGNED_RECEIPT.sig" "$SIGNED_RECEIPT.sig.second"

ln -s "$(basename "$SIGNED_RECEIPT")" "$ARTIFACT_DIR/receipt-link.json"
if bash "$SCRIPT_DIR/sign_release_receipt.sh" \
    "$ARTIFACT_DIR/receipt-link.json" "$PRIVATE_KEY" "$PUBLIC_KEY" \
    "$ARTIFACT_DIR/receipt-link.json.sig" >/dev/null 2>&1; then
    echo "ERROR: release receipt signer accepted a symlink input" >&2
    exit 1
fi
[ ! -e "$ARTIFACT_DIR/receipt-link.json.sig" ] || {
    echo "ERROR: rejected symlink receipt left a signature output" >&2
    exit 1
}

cp "$SIGNED_RECEIPT" "$ARTIFACT_DIR/mutated-receipt.json"
printf 'mutation\n' >> "$ARTIFACT_DIR/mutated-receipt.json"
if openssl pkeyutl -verify -rawin -pubin \
    -inkey "$PUBLIC_KEY" \
    -sigfile "$SIGNED_RECEIPT.sig" \
    -in "$ARTIFACT_DIR/mutated-receipt.json" >/dev/null 2>&1; then
    echo "ERROR: signed source closure accepted mutated receipt bytes" >&2
    exit 1
fi
printf 'stale signature bytes\n' > "$ARTIFACT_DIR/wrong-key.sig"
if bash "$SCRIPT_DIR/sign_release_receipt.sh" \
    "$SIGNED_RECEIPT" "$PRIVATE_KEY" "$WRONG_PUBLIC_KEY" \
    "$ARTIFACT_DIR/wrong-key.sig" >/dev/null 2>&1; then
    echo "ERROR: release receipt signer accepted a mismatched trusted public key" >&2
    exit 1
fi
[ ! -e "$ARTIFACT_DIR/wrong-key.sig" ] || {
    echo "ERROR: release receipt signer left a failed signature output" >&2
    exit 1
}

# Anti-orphan the production signing path and its release metadata binding.
grep -Fq '/project/scripts/sign_release_receipt.sh' "$SCRIPT_DIR/build_in_docker.sh"
python3 - "$SCRIPT_DIR/build_in_docker.sh" <<'PY'
import pathlib
import sys

driver = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
start = driver.index('SOURCE_CLOSURE_SIGNATURE_PATH="${SOURCE_CLOSURE_PATH}.sig"')
end = driver.index('[ -f "$SOURCE_CLOSURE_SIGNATURE_PATH" ]', start)
signing_block = driver[start:end]
if '-v "${POSIX_PROJECT_DIR}:/project:ro"' not in signing_block:
    raise SystemExit(
        "ERROR: source-closure signer cannot execute the project signing helper "
        "without the read-only /project mount"
    )
PY
grep -Fq 'source_closure_signature=$(basename "$SOURCE_CLOSURE_SIGNATURE_PATH")' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'Ed25519 source-closure signature must be exactly 64 bytes' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq -- '--receipt-authentication detached_ed25519_required_for_release' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq -- '--signature "$SOURCE_CLOSURE_SIGNATURE_PATH"' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq -- '--public-key "$DCENT_RELEASE_PUBKEY_FILE"' \
    "$SCRIPT_DIR/build_in_docker.sh"
grep -Fq 'TMP_SIGNATURE=$(mktemp "${SIGNATURE}.tmp.XXXXXX")' \
    "$SCRIPT_DIR/sign_release_receipt.sh"
grep -Fq 'published release receipt signature failed final verification' \
    "$SCRIPT_DIR/sign_release_receipt.sh"
if grep -Fq 'TMP_SIGNATURE="${SIGNATURE}.tmp.$$"' \
    "$SCRIPT_DIR/sign_release_receipt.sh"; then
    echo "ERROR: release receipt signer regressed to a predictable temp path" >&2
    exit 1
fi

TAMPER_DIR="$ARTIFACT_DIR/tampered"
mkdir -p "$TAMPER_DIR"
python3 - "$ARTIFACT_DIR/one.json" "$TAMPER_DIR" <<'PY'
import copy
import json
import pathlib
import sys

source = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
output = pathlib.Path(sys.argv[2])
mutations = {
    "target": (("build", "target"), "invalid target with spaces"),
    "arch": (("build", "arch"), "aarch64-unknown-linux-musl"),
    "cargo-policy": (("build", "cargo", "dependency_resolution"), "best-effort"),
    "buildroot-repository": (("build", "buildroot", "repository"), "https://example.invalid/buildroot.git"),
    "buildroot-commit": (("build", "buildroot", "commit"), "main"),
    "checkout-policy": (("build", "buildroot", "checkout_policy"), "tracked-files-only"),
    "checkout-verification": (("build", "buildroot", "checkout_verification"), "self-asserted"),
    "toolchain-id": (("build", "toolchain", "id"), "mutable toolchain"),
    "toolchain-verification": (("build", "toolchain", "verification"), "not-checked"),
    "build-input-policy": (
        ("build", "out_of_band_inputs", "selection_policy"),
        "operator-selected",
    ),
    "build-input-snapshot-claim": (
        ("build", "out_of_band_inputs", "snapshot", "claim"),
        "consumer-execution-proven",
    ),
    "prebuilt-claim": (
        ("prebuilt_rust_inputs", "claim"),
        "build-execution-proven",
    ),
    "prebuilt-installed-equivalence": (
        ("prebuilt_rust_inputs", "installed_payload_equivalence"),
        "verified",
    ),
    "prebuilt-receipt-context": (
        ("prebuilt_rust_inputs", "entries", 0, "build_variant"),
        "generic",
    ),
    "prebuilt-wrong-prefix": (
        ("prebuilt_rust_inputs", "entries", 0, "binary", "path"),
        "unrelated.prebuilt-rust.dcentos-init.bin",
    ),
    "prebuilt-unsafe-path": (
        ("prebuilt_rust_inputs", "entries", 0, "receipt", "path"),
        "nested\\receipt.json",
    ),
}
for name, (path, value) in mutations.items():
    manifest = copy.deepcopy(source)
    cursor = manifest
    for key in path[:-1]:
        cursor = cursor[key]
    cursor[path[-1]] = value
    (output / f"{name}.json").write_text(
        json.dumps(manifest, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
        encoding="ascii",
    )
wrong_type = copy.deepcopy(source)
wrong_type["build"] = []
(output / "wrong-type-build.json").write_text(
    json.dumps(wrong_type, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
missing_input = copy.deepcopy(source)
missing_input["build"]["out_of_band_inputs"]["files"].pop()
(output / "missing-build-input.json").write_text(
    json.dumps(missing_input, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
extra_input = copy.deepcopy(source)
extra_input["build"]["out_of_band_inputs"]["files"].append(
    copy.deepcopy(extra_input["build"]["out_of_band_inputs"]["files"][0])
)
(output / "extra-build-input.json").write_text(
    json.dumps(extra_input, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
extra_top = copy.deepcopy(source)
extra_top["reproducible"] = True
(output / "extra-top-level-overclaim.json").write_text(
    json.dumps(extra_top, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
extra_nested = copy.deepcopy(source)
extra_nested["build"]["buildroot"]["payload_reproducible"] = True
(output / "extra-nested-overclaim.json").write_text(
    json.dumps(extra_nested, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
extra_snapshot = copy.deepcopy(source)
extra_snapshot["build"]["out_of_band_inputs"]["snapshot"]["consumer_execution"] = True
(output / "extra-snapshot-overclaim.json").write_text(
    json.dumps(extra_snapshot, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
missing_prebuilt = copy.deepcopy(source)
missing_prebuilt["prebuilt_rust_inputs"]["entries"].pop()
(output / "missing-prebuilt-input.json").write_text(
    json.dumps(missing_prebuilt, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
extra_prebuilt = copy.deepcopy(source)
extra_prebuilt["prebuilt_rust_inputs"]["entries"].append(
    copy.deepcopy(extra_prebuilt["prebuilt_rust_inputs"]["entries"][0])
)
(output / "extra-prebuilt-input.json").write_text(
    json.dumps(extra_prebuilt, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
reordered_prebuilt = copy.deepcopy(source)
reordered_prebuilt["prebuilt_rust_inputs"]["entries"].reverse()
(output / "reordered-prebuilt-input.json").write_text(
    json.dumps(reordered_prebuilt, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
duplicate_prebuilt_path = copy.deepcopy(source)
duplicate_prebuilt_path["prebuilt_rust_inputs"]["entries"][1]["binary"]["path"] = (
    duplicate_prebuilt_path["prebuilt_rust_inputs"]["entries"][0]["binary"]["path"]
)
(output / "duplicate-prebuilt-path.json").write_text(
    json.dumps(duplicate_prebuilt_path, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
missing_key = copy.deepcopy(source)
del missing_key["build"]["toolchain"]["verification"]
(output / "missing-required-key.json").write_text(
    json.dumps(missing_key, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
PY
for TAMPERED_RECEIPT in "$TAMPER_DIR"/*.json; do
    if verify_closure \
        --repo-root "$REPO" \
        --artifact-dir "$ARTIFACT_DIR" \
        "$TAMPERED_RECEIPT" >/dev/null 2>&1; then
        echo "ERROR: source closure accepted weakened schema field: $TAMPERED_RECEIPT" >&2
        exit 1
    fi
done

PREBUILT_BINARY="$ARTIFACT_DIR/release.tar.prebuilt-rust.dcentrald.bin"
PREBUILT_RECEIPT="$ARTIFACT_DIR/release.tar.prebuilt-rust.dcentrald.build-receipt.json"
cp "$PREBUILT_BINARY" "$TMPDIR_TEST/prebuilt-binary.saved"
printf 'mutated retained binary\n' > "$PREBUILT_BINARY"
expect_failure changed-retained-prebuilt-binary verify_closure \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" "$ARTIFACT_DIR/one.json"
cp "$TMPDIR_TEST/prebuilt-binary.saved" "$PREBUILT_BINARY"

cp "$PREBUILT_RECEIPT" "$TMPDIR_TEST/prebuilt-receipt.saved"
printf ' ' >> "$PREBUILT_RECEIPT"
expect_failure noncanonical-retained-prebuilt-receipt verify_closure \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" "$ARTIFACT_DIR/one.json"
cp "$TMPDIR_TEST/prebuilt-receipt.saved" "$PREBUILT_RECEIPT"

mv "$PREBUILT_BINARY" "$PREBUILT_BINARY.regular"
ln -s "$(basename "$PREBUILT_BINARY.regular")" "$PREBUILT_BINARY"
expect_failure symlinked-retained-prebuilt-binary verify_closure \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" "$ARTIFACT_DIR/one.json"
rm "$PREBUILT_BINARY"
mv "$PREBUILT_BINARY.regular" "$PREBUILT_BINARY"

PREBUILT_RECEIPT_LINK_TARGET="$PREBUILT_RECEIPT.regular"
mv "$PREBUILT_RECEIPT" "$PREBUILT_RECEIPT_LINK_TARGET"
ln -s "$(basename "$PREBUILT_RECEIPT_LINK_TARGET")" "$PREBUILT_RECEIPT"
expect_failure symlinked-retained-prebuilt-receipt verify_closure \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" "$ARTIFACT_DIR/one.json"
rm "$PREBUILT_RECEIPT"
mv "$PREBUILT_RECEIPT_LINK_TARGET" "$PREBUILT_RECEIPT"

printf 'undeclared sidecar\n' > \
    "$ARTIFACT_DIR/release.tar.prebuilt-rust.unused.bin"
expect_failure undeclared-retained-prebuilt-sidecar verify_closure \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" "$ARTIFACT_DIR/one.json"
rm "$ARTIFACT_DIR/release.tar.prebuilt-rust.unused.bin"

# Exercise receipt semantics during generation, before any outer source-closure
# hash exists. Each modified receipt remains canonical JSON, so these failures
# prove field validation rather than merely detecting a changed sidecar digest.
cp "$PREBUILT_RECEIPT" "$TMPDIR_TEST/semantic-receipt.saved"
for mutation in schema claim target profile variant builder-kind builder-base builder-image \
    binary-name binary-path binary-hash capsule-malformed capsule-invocation-swap \
    capsule-snapshot-swap git-source-kind git-commit build-input-missing \
    build-input-selection build-input-target build-input-file build-input-manifest \
    build-input-policy build-input-legacy-authority; do
    python3 - "$PREBUILT_RECEIPT" "$mutation" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
mutation = sys.argv[2]
receipt = json.loads(path.read_text(encoding="utf-8"))
if mutation == "schema":
    receipt["schema_version"] = 1
elif mutation == "claim":
    receipt["claim"] = "build-execution-proven"
elif mutation == "target":
    receipt["target_triple"] = "aarch64-unknown-linux-musl"
elif mutation == "profile":
    receipt["profile"] = "debug"
elif mutation == "variant":
    receipt["build_variant"] = "generic"
elif mutation == "builder-kind":
    receipt["builder"]["kind"] = "native-host"
elif mutation == "builder-base":
    receipt["builder"]["base_reference"] = "rust:1.90-bookworm"
elif mutation == "builder-image":
    receipt["builder"]["image_id"] = "mutable-tag"
elif mutation == "binary-name":
    receipt["binary"]["name"] = "other"
elif mutation == "binary-path":
    receipt["binary"]["path"] = "target/release/dcentrald"
elif mutation == "binary-hash":
    receipt["binary"]["sha256"] = "0" * 64
elif mutation == "capsule-malformed":
    receipt["release_capsule"]["unexpected"] = "forbidden"
elif mutation == "capsule-invocation-swap":
    receipt["release_capsule"]["release_invocation_id"] = "f" * 64
elif mutation == "capsule-snapshot-swap":
    receipt["release_capsule"]["source_snapshot_id"] = "e" * 64
elif mutation == "git-source-kind":
    receipt["git"]["source_kind"] = "live-working-tree"
elif mutation == "git-commit":
    receipt["git"]["commit"] = "0" * 40
elif mutation == "build-input-missing":
    receipt.pop("build_inputs")
elif mutation == "build-input-selection":
    receipt["build_inputs"]["selection_authority"] = "caller-supplied"
elif mutation == "build-input-target":
    receipt["build_inputs"]["evidence"]["snapshot"]["target"] = "s9"
elif mutation == "build-input-file":
    receipt["build_inputs"]["evidence"]["files"].append(
        {"path": "unexpected.bin", "sha256": "0" * 64, "size": 0}
    )
elif mutation == "build-input-manifest":
    receipt["build_inputs"]["evidence"]["manifest"]["sha256"] = "0" * 64
elif mutation == "build-input-policy":
    receipt["build_inputs"]["evidence"]["selection_policy"] = "caller-selected"
elif mutation == "build-input-legacy-authority":
    receipt["compile_environment"]["entries"]["DCENT_STOCK_FPGA_SHA256"] = "0" * 64
path.write_text(
    json.dumps(receipt, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY
    expect_failure "semantic-receipt-$mutation" python3 "$GENERATOR" generate \
        "${COMMON_ARGS[@]}" \
        --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
        --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
        --output "$ARTIFACT_DIR/semantic-$mutation.json"
    cp "$TMPDIR_TEST/semantic-receipt.saved" "$PREBUILT_RECEIPT"
done

LINKED_ARTIFACT_DIR="$TMPDIR_TEST/linked-artifact-dir"
ln -s "$ARTIFACT_DIR" "$LINKED_ARTIFACT_DIR"
expect_failure symlinked-verify-artifact-dir verify_closure \
    --repo-root "$REPO" --artifact-dir "$LINKED_ARTIFACT_DIR" \
    "$ARTIFACT_DIR/one.json"
SYMLINK_PARENT_ARGS=()
for value in "${COMMON_ARGS[@]}"; do
    case "$value" in
        "$ARTIFACT_DIR"/*.prebuilt-rust.*)
            SYMLINK_PARENT_ARGS+=("$LINKED_ARTIFACT_DIR/${value#"$ARTIFACT_DIR/"}")
            ;;
        *) SYMLINK_PARENT_ARGS+=("$value") ;;
    esac
done
expect_failure symlinked-retained-prebuilt-parent python3 "$GENERATOR" generate \
    "${SYMLINK_PARENT_ARGS[@]}" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --output "$ARTIFACT_DIR/symlink-parent.json"

python3 - "$ARTIFACT_DIR/one.json" "$RELEASE_INVOCATION_ID" \
    "$SOURCE_SNAPSHOT_ID" "$SOURCE_SNAPSHOT_DESCRIPTOR_SHA256" \
    "$RELEASE_INVOCATION_DESCRIPTOR_SHA256" <<'PY'
import json
import pathlib
import sys

manifest = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
assert manifest["schema"] == "org.dcentral.dcentos.source-closure.v4"
assert manifest["source"]["tree_state"] == "exact_git_object_snapshot"
assert manifest["release_capsule"] == {
    "schema": "org.dcentral.dcentos.release-capsule-lineage.v2",
    "release_invocation_descriptor_sha256": sys.argv[5],
    "release_invocation_id": sys.argv[2],
    "source_snapshot_id": sys.argv[3],
    "source_snapshot_descriptor_sha256": sys.argv[4],
}
assert manifest["created_at_utc"] == "2023-11-14T22:13:20Z"
assert manifest["scope"]["level"] == "partial"
assert manifest["scope"]["payload_byte_reproducibility"] == "not_evaluated"
assert manifest["scope"]["binds"] == (
    "one declared release invocation identity, one exact Git-object source snapshot "
    "identity, retained packaging input snapshots, and produced artifact bytes"
)
assert not any("clean Git tree" in item for item in manifest["scope"]["unresolved"])
assert any(
    "Git blob executable modes are bound" in item
    for item in manifest["scope"]["unresolved"]
)
prebuilt = manifest["prebuilt_rust_inputs"]
assert prebuilt["claim"] == "retained-packaging-input-snapshots-not-build-execution-attestation"
assert prebuilt["build_execution_attestation"] == "not_attested"
assert prebuilt["installed_payload_equivalence"] == "not_evaluated"
assert [entry["name"] for entry in prebuilt["entries"]] == ["dcentos-init", "dcentrald"]
assert all(entry["target_triple"] == "armv7-unknown-linux-musleabihf" for entry in prebuilt["entries"])
assert all(entry["profile"] == "release" for entry in prebuilt["entries"])
assert all(entry["build_variant"] == "zynq" for entry in prebuilt["entries"])
assert manifest["build"]["cargo"]["dependency_resolution"] == "--locked-required"
inputs = manifest["build"]["out_of_band_inputs"]
assert inputs["selection_policy"] == "org.dcentral.dcentos.release-build-input-selection.v1"
assert [item["path"] for item in inputs["files"]] == [
    "knowledge-base/extractions/s9/kernel.bin",
    "knowledge-base/extractions/s9/s9_devicetree.dtb",
]
assert len(inputs["snapshot"]["snapshot_id"]) == 64
assert inputs["snapshot"]["target"] == "s9"
assert inputs["snapshot"]["claim"] == (
    "selected_manifest_pinned_bytes_copied_from_open_regular_file_handles"
)
assert [item["path"] for item in manifest["build"]["buildroot"]["configs"]] == sorted(
    item["path"] for item in manifest["build"]["buildroot"]["configs"]
)
assert manifest["build"]["buildroot"]["config_merge_order"] == [
    "DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment",
    "DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig",
]
assert manifest["build"]["buildroot"]["external_tree"]["filesystem_mode_scope"].startswith(
    "not_bound"
)
members = manifest["artifacts"][0]["archive_regular_members"]
assert [member["path"] for member in members] == sorted(member["path"] for member in members)
assert any(member["path"].endswith("/kernel") for member in members)
assert any(member["path"].endswith("/root") for member in members)
PY

# Target-scoped ignored inputs are verified before either build consumer. The
# policy includes only bytes actually consumed by the selected lane.
python3 "$GENERATOR" verify-inputs \
    --repo-root "$REPO" \
    --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target cargo-workspace >/dev/null
python3 "$GENERATOR" verify-inputs \
    --repo-root "$REPO" \
    --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target s9 >/dev/null
python3 "$GENERATOR" verify-inputs \
    --repo-root "$REPO" \
    --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target am2-s19jpro >/dev/null
for alias in am2-s19jpro-sd am2-s19pro; do
    python3 "$GENERATOR" verify-inputs \
        --repo-root "$REPO" \
        --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
        --target "$alias" >/dev/null
done
expect_failure unknown-target python3 "$GENERATOR" verify-inputs \
    --repo-root "$REPO" \
    --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target future-unmapped-miner

mapfile -t BLOCKED_TARGETS < <(python3 - "$GENERATOR" <<'PY'
import importlib.util
import sys

spec = importlib.util.spec_from_file_location("source_closure", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
for target in sorted(module.BLOCKED_BUILD_INPUT_TARGETS):
    print(target)
PY
)
for blocked in "${BLOCKED_TARGETS[@]}"; do
    expect_failure "blocked-$blocked" python3 "$GENERATOR" verify-inputs \
        --repo-root "$REPO" \
        --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
        --target "$blocked"
done

cp "$REPO/knowledge-base/extractions/s19j/kernel.bin" "$TMPDIR_TEST/s19j-kernel.saved"
printf 'mutated alias kernel\n' > "$REPO/knowledge-base/extractions/s19j/kernel.bin"
for alias in am2-s19jpro am2-s19jpro-sd am2-s19pro; do
    expect_failure "changed-kernel-$alias" python3 "$GENERATOR" verify-inputs \
        --repo-root "$REPO" \
        --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
        --target "$alias"
done
cp "$TMPDIR_TEST/s19j-kernel.saved" "$REPO/knowledge-base/extractions/s19j/kernel.bin"

cp "$REPO/knowledge-base/extractions/s9/s9_devicetree.dtb" "$TMPDIR_TEST/s9-dtb.saved"
printf 'mutated dtb\n' > "$REPO/knowledge-base/extractions/s9/s9_devicetree.dtb"
expect_failure changed-s9-dtb python3 "$GENERATOR" verify-inputs \
    --repo-root "$REPO" \
    --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target s9
cp "$TMPDIR_TEST/s9-dtb.saved" "$REPO/knowledge-base/extractions/s9/s9_devicetree.dtb"

rm "$REPO/knowledge-base/extractions/s9/s9_devicetree.dtb"
ln -s kernel.bin "$REPO/knowledge-base/extractions/s9/s9_devicetree.dtb"
expect_failure symlinked-s9-dtb python3 "$GENERATOR" verify-inputs \
    --repo-root "$REPO" \
    --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target s9
rm "$REPO/knowledge-base/extractions/s9/s9_devicetree.dtb"
cp "$TMPDIR_TEST/s9-dtb.saved" "$REPO/knowledge-base/extractions/s9/s9_devicetree.dtb"

cp "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" "$TMPDIR_TEST/build-inputs.saved"
grep -v 's9_devicetree.dtb$' "$TMPDIR_TEST/build-inputs.saved" \
    > "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"
expect_failure unlisted-required-dtb python3 "$GENERATOR" verify-inputs \
    --repo-root "$REPO" \
    --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target s9
cp "$TMPDIR_TEST/build-inputs.saved" "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"

expect_failure wrong-target-arch python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --arch aarch64-unknown-linux-musl \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --output "$ARTIFACT_DIR/wrong-arch.json"
expect_failure wrong-target-defconfig python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_am2_s19jpro_defconfig" \
    --output "$ARTIFACT_DIR/wrong-defconfig.json"

head -n 1 "$TMPDIR_TEST/build-inputs.saved" >> "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"
expect_failure duplicate-manifest-path python3 "$GENERATOR" verify-inputs \
    --repo-root "$REPO" \
    --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target s9
cp "$TMPDIR_TEST/build-inputs.saved" "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"

printf '%064d  ../escape.bin\n' 0 >> "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"
expect_failure unsafe-manifest-path python3 "$GENERATOR" verify-inputs \
    --repo-root "$REPO" \
    --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target s9
cp "$TMPDIR_TEST/build-inputs.saved" "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"

sed '1s/^[0-9a-f]\{64\}/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/' \
    "$TMPDIR_TEST/build-inputs.saved" > "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"
expect_failure uppercase-manifest-digest python3 "$GENERATOR" verify-inputs \
    --repo-root "$REPO" \
    --build-input-manifest "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest" \
    --target s9
cp "$TMPDIR_TEST/build-inputs.saved" "$REPO/DCENT_OS_Antminer/scripts/build_inputs.manifest"

python3 - "$ARTIFACT_DIR/one.json" "$ARTIFACT_DIR/legacy-v1.json" <<'PY'
import json
import pathlib
import sys

receipt = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
receipt["schema"] = "org.dcentral.dcentos.source-closure.v1"
pathlib.Path(sys.argv[2]).write_text(
    json.dumps(receipt, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
PY
expect_failure legacy-v1-default-rejection verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$ARTIFACT_DIR" \
    "$ARTIFACT_DIR/legacy-v1.json"

python3 - "$ARTIFACT_DIR/one.json" "$ARTIFACT_DIR/legacy-v2.json" <<'PY'
import json
import pathlib
import sys

receipt = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
receipt["schema"] = "org.dcentral.dcentos.source-closure.v2"
pathlib.Path(sys.argv[2]).write_text(
    json.dumps(receipt, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
PY
expect_failure legacy-v2-default-rejection verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$ARTIFACT_DIR" \
    "$ARTIFACT_DIR/legacy-v2.json"

printf 'artifact mutation\n' >> "$ARTIFACT_DIR/release.tar"
expect_failure changed-artifact verify_closure \
    --repo-root "$REPO" \
    --artifact-dir "$ARTIFACT_DIR" \
    "$ARTIFACT_DIR/one.json"
tar --sort=name --format=ustar --mtime='@1700000000' \
    --owner=0 --group=0 --numeric-owner \
    -cf "$ARTIFACT_DIR/release.tar" -C "$ARTIFACT_DIR/stage" sysupgrade-test

expect_failure missing-input python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --cargo-lock "$REPO/dcentrald/missing.lock" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --output "$ARTIFACT_DIR/missing.json"

expect_failure mutable-container python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --container-image-id dcentos-build:latest \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --output "$ARTIFACT_DIR/mutable.json"

UNVERIFIED_ARGS=()
for value in "${COMMON_ARGS[@]}"; do
    if [ "$value" != "--toolchain-verified" ]; then
        UNVERIFIED_ARGS+=("$value")
    fi
done
expect_failure unverified-toolchain python3 "$GENERATOR" generate "${UNVERIFIED_ARGS[@]}" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --output "$ARTIFACT_DIR/unverified-toolchain.json"

cp "$REPO/scripts/build-dcentrald.sh" "$TMPDIR_TEST/locked-builder"
printf '#!/bin/sh\ncargo build --release\n' > "$REPO/scripts/build-dcentrald.sh"
git -C "$REPO" add scripts/build-dcentrald.sh
git -C "$REPO" commit -q -m unlocked-builder
UNLOCKED_COMMIT="$(git -C "$REPO" rev-parse HEAD)"
UNLOCKED_SNAPSHOT_RESULT="$(python3 "$SOURCE_SNAPSHOT_HELPER" create \
    --repo-root "$REPO" --commit "$UNLOCKED_COMMIT" \
    --stage-parent "$TMPDIR_TEST/source-snapshots")"
UNLOCKED_SNAPSHOT="$(printf '%s\n' "$UNLOCKED_SNAPSHOT_RESULT" | \
    python3 "$SOURCE_SNAPSHOT_HELPER" query-result --field snapshot)"
UNLOCKED_TREE="$(printf '%s\n' "$UNLOCKED_SNAPSHOT_RESULT" | \
    python3 "$SOURCE_SNAPSHOT_HELPER" query-result --field tree)"
expect_failure unlocked-cargo python3 "$GENERATOR" generate "${COMMON_ARGS[@]}" \
    --source-commit "$UNLOCKED_COMMIT" \
    --source-snapshot "$UNLOCKED_SNAPSHOT" \
    --cargo-lock "$UNLOCKED_TREE/dcentrald/Cargo.lock" \
    --cargo-build-script "$UNLOCKED_TREE/scripts/build-dcentrald.sh" \
    --external-tree "$UNLOCKED_TREE/DCENT_OS_Antminer/br2_external_dcentos" \
    --container-definition "$UNLOCKED_TREE/Dockerfile.build" \
    --buildroot-config "$UNLOCKED_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$UNLOCKED_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --output "$ARTIFACT_DIR/unlocked.json"
cp "$TMPDIR_TEST/locked-builder" "$REPO/scripts/build-dcentrald.sh"
git -C "$REPO" add scripts/build-dcentrald.sh
git -C "$REPO" commit -q -m restore-locked-builder

ln -s board/example/overlay.txt "$REPO/DCENT_OS_Antminer/br2_external_dcentos/linked-input"
git -C "$REPO" add DCENT_OS_Antminer/br2_external_dcentos/linked-input
git -C "$REPO" commit -q -m linked-input
LINKED_COMMIT="$(git -C "$REPO" rev-parse HEAD)"
expect_failure symlink-input python3 "$SOURCE_SNAPSHOT_HELPER" create \
    --repo-root "$REPO" --commit "$LINKED_COMMIT" \
    --stage-parent "$TMPDIR_TEST/source-snapshots"

# The persistent Buildroot checkout is ephemeral and cannot be re-inspected by
# the receipt verifier. Pin the builder's exact fail-closed command and prove
# that it detects an otherwise clean checkout with one untracked source file.
grep -Fq 'git -C buildroot status --porcelain --untracked-files=normal' \
    "$SCRIPT_DIR/build_in_docker.sh"
WARM_BUILDROOT="$TMPDIR_TEST/warm-buildroot"
git init -q "$WARM_BUILDROOT"
git -C "$WARM_BUILDROOT" config user.name source-closure-test
git -C "$WARM_BUILDROOT" config user.email source-closure-test.invalid
printf 'tracked\n' > "$WARM_BUILDROOT/Makefile"
git -C "$WARM_BUILDROOT" add Makefile
git -C "$WARM_BUILDROOT" commit -q -m fixture
printf 'untracked build input\n' > "$WARM_BUILDROOT/local.mk"
if [ -z "$(git -C "$WARM_BUILDROOT" status --porcelain --untracked-files=normal)" ]; then
    echo "ERROR: warm Buildroot negative fixture did not expose untracked input" >&2
    exit 1
fi

# Assemble one real signed exact release set while the live authorities still
# exist. Its verification below happens only after every authority is gone.
verify_closure --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" \
    --signature "$SIGNED_RECEIPT.sig" --public-key "$PUBLIC_KEY" \
    "$SIGNED_RECEIPT" >/dev/null
PORTABLE_PACKAGE_ROOT="$TMPDIR_TEST/portable-package"
mkdir -p "$PORTABLE_PACKAGE_ROOT/sysupgrade-am1-s9"
printf 'kernel fixture\n' > "$PORTABLE_PACKAGE_ROOT/sysupgrade-am1-s9/kernel"
printf 'root fixture\n' > "$PORTABLE_PACKAGE_ROOT/sysupgrade-am1-s9/root"
cp "$PUBLIC_KEY" "$PORTABLE_PACKAGE_ROOT/sysupgrade-am1-s9/release_ed25519.pub"
cat > "$PORTABLE_PACKAGE_ROOT/sysupgrade-am1-s9/MANIFEST.json" <<'EOF'
{"board":"am1-s9","board_target":"am1-s9","package_type":"sysupgrade","product":"DCENT_OS","provenance":{"build_target":"s9"},"schema":1}
EOF
openssl pkeyutl -sign -rawin -inkey "$PRIVATE_KEY" \
    -in "$PORTABLE_PACKAGE_ROOT/sysupgrade-am1-s9/MANIFEST.json" \
    -out "$PORTABLE_PACKAGE_ROOT/sysupgrade-am1-s9/MANIFEST.sig"
tar --sort=name --format=ustar --mtime='@1700000000' \
    --owner=0 --group=0 --numeric-owner \
    -cf "$ARTIFACT_DIR/dcentos-sysupgrade-118.tar" \
    -C "$PORTABLE_PACKAGE_ROOT" sysupgrade-am1-s9
for binary_name in dcentrald dcentos-init; do
    cp "$ARTIFACT_DIR/release.tar.prebuilt-rust.${binary_name}.bin" \
        "$ARTIFACT_DIR/dcentos-sysupgrade-118.tar.prebuilt-rust.${binary_name}.bin"
    cp "$ARTIFACT_DIR/release.tar.prebuilt-rust.${binary_name}.build-receipt.json" \
        "$ARTIFACT_DIR/dcentos-sysupgrade-118.tar.prebuilt-rust.${binary_name}.build-receipt.json"
done
PORTABLE_COMMON_ARGS=()
for argument in "${COMMON_ARGS[@]}"; do
    PORTABLE_COMMON_ARGS+=("${argument//release.tar/dcentos-sysupgrade-118.tar}")
done
PORTABLE_RECEIPT="$ARTIFACT_DIR/portable.source-closure.json"
python3 "$GENERATOR" generate "${PORTABLE_COMMON_ARGS[@]}" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos-common.fragment" \
    --buildroot-config "$SOURCE_TREE/DCENT_OS_Antminer/br2_external_dcentos/configs/dcentos_s9_defconfig" \
    --receipt-authentication detached_ed25519_required_for_release \
    --output "$PORTABLE_RECEIPT" >/dev/null
openssl pkeyutl -sign -rawin -inkey "$PRIVATE_KEY" \
    -in "$PORTABLE_RECEIPT" -out "$PORTABLE_RECEIPT.sig"
verify_closure --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" \
    --signature "$PORTABLE_RECEIPT.sig" --public-key "$PUBLIC_KEY" \
    "$PORTABLE_RECEIPT" >/dev/null
PORTABLE_STAGE_PARENT="$TMPDIR_TEST/portable-stage-parent"
PORTABLE_OUTPUT_PARENT="$TMPDIR_TEST/portable-published"
mkdir -p "$PORTABLE_STAGE_PARENT" "$PORTABLE_OUTPUT_PARENT"
PORTABLE_CAPABILITY_JSON="$(python3 "$RELEASE_SET_HELPER" create-stage \
    --parent "$PORTABLE_STAGE_PARENT")"
PORTABLE_CAPABILITY_FILE="$TMPDIR_TEST/portable-capability.json"
printf '%s\n' "$PORTABLE_CAPABILITY_JSON" > "$PORTABLE_CAPABILITY_FILE"
PORTABLE_PRIVATE_STAGE="$(printf '%s\n' "$PORTABLE_CAPABILITY_JSON" | \
    python3 "$RELEASE_SET_HELPER" query --field stage-path)"
python3 - "$PORTABLE_RECEIPT" "$PORTABLE_RECEIPT.sig" "$ARTIFACT_DIR" \
    "$PORTABLE_PROJECTIONS" "$PORTABLE_PRIVATE_STAGE" <<'PY'
import json
import pathlib
import shutil
import sys

receipt_path = pathlib.Path(sys.argv[1])
signature_path = pathlib.Path(sys.argv[2])
artifact_dir = pathlib.Path(sys.argv[3])
projections = pathlib.Path(sys.argv[4])
stage = pathlib.Path(sys.argv[5])
receipt = json.loads(receipt_path.read_text(encoding="ascii"))
names = {item["path"] for item in receipt["artifacts"]}
for entry in receipt["prebuilt_rust_inputs"]["entries"]:
    names.add(entry["binary"]["path"])
    names.add(entry["receipt"]["path"])
for name in sorted(names):
    shutil.copyfile(artifact_dir / name, stage / name)
shutil.copyfile(receipt_path, stage / "firmware.source-closure.json")
shutil.copyfile(signature_path, stage / "firmware.source-closure.json.sig")
shutil.copyfile(
    projections / "release-packaging-input.json",
    stage / "release-packaging-input.json",
)
PY
python3 "$PORTABLE_EVIDENCE_HELPER" create-live \
    --repo-root "$REPO" --source-commit "$COMMIT" \
    --target s9 \
    --output-name DCENTOS_XIL1_S9_beta20260712 \
    --source-snapshot "$SOURCE_SNAPSHOT" \
    --release-invocation "$RELEASE_INVOCATION" \
    --cargo-input-snapshot "$CARGO_BUILD_INPUT_SNAPSHOT" \
    --result-stage "$RESULT_STAGE" --artifact-dir "$PORTABLE_PRIVATE_STAGE" \
    --closure "$PORTABLE_PRIVATE_STAGE/firmware.source-closure.json" \
    --closure-signature "$PORTABLE_PRIVATE_STAGE/firmware.source-closure.json.sig" \
    --public-key "$PUBLIC_KEY" >/dev/null
openssl pkeyutl -sign -rawin -inkey "$PRIVATE_KEY" \
    -in "$PORTABLE_PRIVATE_STAGE/portable-release-evidence.json" \
    -out "$PORTABLE_PRIVATE_STAGE/portable-release-evidence.json.sig"
PORTABLE_FILES_MANIFEST="$TMPDIR_TEST/portable-files.json"
python3 "$RELEASE_SET_HELPER" manifest-stage \
    --capability-file "$PORTABLE_CAPABILITY_FILE" \
    --output "$PORTABLE_FILES_MANIFEST" >/dev/null
python3 "$RELEASE_SET_HELPER" seal-stage \
    --capability-file "$PORTABLE_CAPABILITY_FILE" \
    --manifest "$PORTABLE_FILES_MANIFEST" \
    --output-name DCENTOS_XIL1_S9_beta20260712 >/dev/null
python3 "$PORTABLE_EVIDENCE_HELPER" verify-stage \
    --repo-root "$REPO" --public-key "$PUBLIC_KEY" \
    "$PORTABLE_PRIVATE_STAGE" >/dev/null
PORTABLE_PUBLISH_RESULT="$(python3 "$RELEASE_SET_HELPER" publish \
    --capability-file "$PORTABLE_CAPABILITY_FILE" \
    --output-parent "$PORTABLE_OUTPUT_PARENT")"
PORTABLE_PUBLISHED="$(printf '%s\n' "$PORTABLE_PUBLISH_RESULT" | \
    python3 "$RELEASE_SET_HELPER" query --field published-path)"

# Destroy the original private authorities, then independently reauthenticate
# the signed closure from trusted Git plus retained path/hash/ID projections.
python3 "$RESULT_STAGE_HELPER" destroy --capability "$RESULT_CAPABILITY" \
    --invocation-stage "$RELEASE_INVOCATION" "$RESULT_STAGE"
python3 "$BUILD_INPUT_SNAPSHOT_HELPER" destroy \
    --token "$BUILD_INPUT_DESTROY_TOKEN" "$BUILD_INPUT_SNAPSHOT"
python3 "$BUILD_INPUT_SNAPSHOT_HELPER" destroy \
    --token "$CARGO_BUILD_INPUT_DESTROY_TOKEN" "$CARGO_BUILD_INPUT_SNAPSHOT"
python3 "$SOURCE_SNAPSHOT_HELPER" destroy \
    --token "$SOURCE_DESTROY_TOKEN" "$SOURCE_SNAPSHOT"
python3 "$INVOCATION_HELPER" mark-gc-eligible \
    --capability "$RELEASE_INVOCATION_CAPABILITY" \
    --reason post-cleanup-portable-audit-test "$RELEASE_INVOCATION" >/dev/null
python3 "$INVOCATION_HELPER" destroy \
    --capability "$RELEASE_INVOCATION_CAPABILITY" "$RELEASE_INVOCATION"
[ ! -e "$SOURCE_SNAPSHOT" ] && [ ! -e "$BUILD_INPUT_SNAPSHOT" ] \
    && [ ! -e "$CARGO_BUILD_INPUT_SNAPSHOT" ] \
    && [ ! -e "$RESULT_STAGE" ] && [ ! -e "$RELEASE_INVOCATION" ]
python3 "$GENERATOR" verify-portable \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" \
    --source-snapshot-projection "$PORTABLE_PROJECTIONS/release-source-snapshot.json" \
    --release-invocation-projection "$PORTABLE_PROJECTIONS/release-invocation.json" \
    --build-input-projection "$PORTABLE_PROJECTIONS/release-packaging-input.json" \
    --signature "$SIGNED_RECEIPT.sig" --public-key "$PUBLIC_KEY" \
    "$SIGNED_RECEIPT" >/dev/null
python3 "$PORTABLE_EVIDENCE_HELPER" verify --repo-root "$REPO" \
    --public-key "$PUBLIC_KEY" "$PORTABLE_PUBLISHED" >/dev/null
PORTABLE_TAMPERED="$PORTABLE_OUTPUT_PARENT/portable-release-tampered"
cp -a "$PORTABLE_PUBLISHED" "$PORTABLE_TAMPERED"
python3 - "$PORTABLE_TAMPERED" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
closure = json.loads((root / "firmware.source-closure.json").read_text(encoding="ascii"))
receipt_name = closure["prebuilt_rust_inputs"]["entries"][0]["receipt"]["path"]
with (root / receipt_name).open("ab") as stream:
    stream.write(b" ")

def evidence(path):
    raw = path.read_bytes()
    return {"name": path.name, "sha256": hashlib.sha256(raw).hexdigest(), "size": len(raw)}

index_path = root / "portable-release-evidence.json"
signature_path = root / "portable-release-evidence.json.sig"
descriptor_path = root / ".dcent-release-set.json"
index = json.loads(index_path.read_text(encoding="ascii"))
payload = [
    evidence(path)
    for path in root.iterdir()
    if path.name not in {index_path.name, signature_path.name, descriptor_path.name}
]
payload.sort(key=lambda item: item["name"].encode("utf-8"))
index["payload_files"] = payload
by_name = {item["name"]: item for item in payload}
index["source_closure"] = by_name[index["source_closure"]["name"]]
index["source_closure_signature"] = by_name[index["source_closure_signature"]["name"]]
for key, record in list(index["projections"].items()):
    index["projections"][key] = by_name[record["name"]]
index_path.write_text(
    json.dumps(index, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="ascii",
)
PY
openssl pkeyutl -sign -rawin -inkey "$PRIVATE_KEY" \
    -in "$PORTABLE_TAMPERED/portable-release-evidence.json" \
    -out "$PORTABLE_TAMPERED/portable-release-evidence.json.sig"
python3 - "$PORTABLE_TAMPERED" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
descriptor_path = root / ".dcent-release-set.json"
descriptor = json.loads(descriptor_path.read_text(encoding="utf-8"))
files = []
for path in root.iterdir():
    if path == descriptor_path:
        continue
    raw = path.read_bytes()
    files.append({"name": path.name, "sha256": hashlib.sha256(raw).hexdigest(), "size": len(raw)})
files.sort(key=lambda item: item["name"])
descriptor["files"] = files
descriptor["output_name"] = root.name
descriptor_path.write_text(
    json.dumps(descriptor, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY
expect_failure portable-retained-receipt-tamper \
    python3 "$PORTABLE_EVIDENCE_HELPER" verify --repo-root "$REPO" \
        --public-key "$PUBLIC_KEY" "$PORTABLE_TAMPERED"
expect_failure portable-wrong-key python3 "$GENERATOR" verify-portable \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" \
    --source-snapshot-projection "$PORTABLE_PROJECTIONS/release-source-snapshot.json" \
    --release-invocation-projection "$PORTABLE_PROJECTIONS/release-invocation.json" \
    --build-input-projection "$PORTABLE_PROJECTIONS/release-packaging-input.json" \
    --signature "$SIGNED_RECEIPT.sig" --public-key "$WRONG_PUBLIC_KEY" \
    "$SIGNED_RECEIPT"
WRONG_GIT="$TMPDIR_TEST/wrong-git"
git init -q "$WRONG_GIT"
expect_failure portable-wrong-git python3 "$GENERATOR" verify-portable \
    --repo-root "$WRONG_GIT" --artifact-dir "$ARTIFACT_DIR" \
    --source-snapshot-projection "$PORTABLE_PROJECTIONS/release-source-snapshot.json" \
    --release-invocation-projection "$PORTABLE_PROJECTIONS/release-invocation.json" \
    --build-input-projection "$PORTABLE_PROJECTIONS/release-packaging-input.json" \
    --signature "$SIGNED_RECEIPT.sig" --public-key "$PUBLIC_KEY" \
    "$SIGNED_RECEIPT"
cp "$PORTABLE_PROJECTIONS/release-invocation.json" \
    "$PORTABLE_PROJECTIONS/noncanonical-invocation.json"
printf ' ' >> "$PORTABLE_PROJECTIONS/noncanonical-invocation.json"
expect_failure portable-noncanonical-invocation python3 "$GENERATOR" verify-portable \
    --repo-root "$REPO" --artifact-dir "$ARTIFACT_DIR" \
    --source-snapshot-projection "$PORTABLE_PROJECTIONS/release-source-snapshot.json" \
    --release-invocation-projection "$PORTABLE_PROJECTIONS/noncanonical-invocation.json" \
    --build-input-projection "$PORTABLE_PROJECTIONS/release-packaging-input.json" \
    --signature "$SIGNED_RECEIPT.sig" --public-key "$PUBLIC_KEY" \
    "$SIGNED_RECEIPT"

# NTFS junctions are reparse points rather than POSIX symlinks. They must not
# let retained evidence or generic artifacts escape their lexical directory on
# native Windows runners.
python3 - "$GENERATOR" <<'PY'
import importlib.util
import os
import pathlib
import subprocess
import sys
import tempfile

if os.name != "nt":
    raise SystemExit(0)
spec = importlib.util.spec_from_file_location("source_closure", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with tempfile.TemporaryDirectory(prefix="dcent-closure-junction-") as temporary:
    root = pathlib.Path(temporary)
    target = root / "target"
    target.mkdir()
    (target / "evidence.json").write_text("{}\n", encoding="ascii")
    junction = root / "junction"
    result = subprocess.run(
        ["cmd.exe", "/d", "/c", "mklink", "/J", str(junction), str(target)],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        raise SystemExit(0)
    try:
        for operation in (
            lambda: module.read_regular_nonsymlink(
                str(junction / "evidence.json"), "junction evidence"
            ),
            lambda: module.artifact_entry(str(junction / "evidence.json")),
        ):
            try:
                operation()
            except module.ClosureError:
                pass
            else:
                raise AssertionError("NTFS junction was accepted")
    finally:
        os.rmdir(junction)
PY

echo "source closure: PASS"
