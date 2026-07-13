#!/usr/bin/env bash
# Offline proof for the deterministic, artifact-bound Rust dependency inventory.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INVENTORY_TOOL="$SCRIPT_DIR/rust_dependency_inventory.py"

RUSTUP_BIN="$(command -v rustup 2>/dev/null || true)"
if [ -z "$RUSTUP_BIN" ] && [ -x "${HOME:-}/.cargo/bin/rustup" ]; then
    RUSTUP_BIN="${HOME}/.cargo/bin/rustup"
fi
[ -n "$RUSTUP_BIN" ] || {
    echo "ERROR: Rust dependency inventory test requires rustup" >&2
    exit 1
}
command -v python3 >/dev/null 2>&1 || {
    echo "ERROR: Rust dependency inventory test requires python3" >&2
    exit 1
}

TMPDIR_TEST="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_TEST"' EXIT
WORKSPACE="$TMPDIR_TEST/workspace"
ARTIFACT_DIR="$TMPDIR_TEST/artifacts"
mkdir -p "$WORKSPACE/app/src" "$WORKSPACE/helper/src" "$ARTIFACT_DIR"

cat > "$WORKSPACE/Cargo.toml" <<'EOF'
[workspace]
resolver = "2"
members = ["app", "helper"]
EOF
cat > "$WORKSPACE/app/Cargo.toml" <<'EOF'
[package]
name = "inventory-app"
version = "0.1.0"
edition = "2021"
license = "MIT"

[dependencies]
inventory-helper = { path = "../helper" }
EOF
cat > "$WORKSPACE/app/src/main.rs" <<'EOF'
fn main() { inventory_helper::run(); }
EOF
cat > "$WORKSPACE/helper/Cargo.toml" <<'EOF'
[package]
name = "inventory-helper"
version = "0.1.0"
edition = "2021"
license = "Apache-2.0"
EOF
cat > "$WORKSPACE/helper/src/lib.rs" <<'EOF'
pub fn run() {}
EOF

(cd "$WORKSPACE" && "$RUSTUP_BIN" run 1.90.0 cargo generate-lockfile --offline)
METADATA_JSON="$TMPDIR_TEST/armv7.metadata.json"
(cd "$WORKSPACE" && "$RUSTUP_BIN" run 1.90.0 cargo metadata --locked --offline \
    --filter-platform armv7-unknown-linux-musleabihf --format-version 1 > "$METADATA_JSON")
python3 - "$METADATA_JSON" "$WORKSPACE" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
workspace = pathlib.Path(sys.argv[2]).resolve()
metadata = json.loads(path.read_text(encoding="utf-8"))
for package in metadata["packages"]:
    for field in ("manifest_path", "license_file"):
        value = package.get(field)
        if not value:
            continue
        candidate = pathlib.Path(value).resolve()
        try:
            relative = candidate.relative_to(workspace)
        except ValueError:
            continue
        package[field] = "/src/" + relative.as_posix()
path.write_text(json.dumps(metadata, separators=(",", ":")), encoding="utf-8")
PY
printf 'release artifact fixture\n' > "$ARTIFACT_DIR/release.tar"

generate_inventory() {
    local output="$1"
    python3 "$INVENTORY_TOOL" generate \
        --workspace "$WORKSPACE" \
        --source-root "$TMPDIR_TEST" \
        --metadata-json "$METADATA_JSON" \
        --metadata-path-map "/src=$WORKSPACE" \
        --target armv7-unknown-linux-musleabihf \
        --artifact "$ARTIFACT_DIR/release.tar" \
        --source-date-epoch 1700000000 \
        --output "$output" >/dev/null
}

generate_inventory "$ARTIFACT_DIR/one.json"
touch -t 202501020304 "$WORKSPACE/Cargo.lock" "$ARTIFACT_DIR/release.tar"
generate_inventory "$ARTIFACT_DIR/two.json"
cmp "$ARTIFACT_DIR/one.json" "$ARTIFACT_DIR/two.json"
python3 "$INVENTORY_TOOL" verify \
    --workspace "$WORKSPACE" \
    --source-root "$TMPDIR_TEST" \
    --metadata-json "$METADATA_JSON" \
    --metadata-path-map "/src=$WORKSPACE" \
    --artifact-dir "$ARTIFACT_DIR" \
    "$ARTIFACT_DIR/one.json" >/dev/null

python3 - "$ARTIFACT_DIR/one.json" "$WORKSPACE" <<'PY'
import json
import pathlib
import sys

raw = pathlib.Path(sys.argv[1]).read_text(encoding="ascii")
inventory = json.loads(raw)
assert inventory["schema"] == "org.dcentral.dcentos.rust-dependency-inventory.v1"
assert inventory["created_at_utc"] == "2023-11-14T22:13:20Z"
assert inventory["scope"]["spdx_conformance"] == "not_claimed"
assert inventory["scope"]["cyclonedx_conformance"] == "not_claimed"
assert inventory["scope"]["vulnerability_analysis"] == "not_performed"
assert inventory["graph"]["component_count"] == 2
assert inventory["graph"]["relationship_node_count"] == 2
assert len(inventory["graph"]["workspace_members"]) == 2
assert str(pathlib.Path(sys.argv[2]).resolve()) not in raw
components = {item["name"]: item for item in inventory["graph"]["components"]}
assert components["inventory-app"]["license_declared"] == "MIT"
assert components["inventory-helper"]["license_declared"] == "Apache-2.0"
app_relationship = next(
    item for item in inventory["graph"]["relationships"]
    if "inventory-app@0.1.0" in item["component_id"]
)
assert len(app_relationship["depends_on"]) == 1
assert "inventory-helper@0.1.0" in app_relationship["depends_on"][0]["component_id"]
PY

expect_failure() {
    local label="$1"
    shift
    if "$@" >/dev/null 2>&1; then
        echo "ERROR: Rust dependency inventory accepted invalid input: $label" >&2
        exit 1
    fi
}

python3 - "$ARTIFACT_DIR/one.json" "$ARTIFACT_DIR" <<'PY'
import copy
import json
import pathlib
import sys

source = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
output = pathlib.Path(sys.argv[2])
weakened = copy.deepcopy(source)
weakened["resolver"]["command"] = "cargo metadata"
(output / "weakened-resolver.json").write_text(
    json.dumps(weakened, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
wrong_type = copy.deepcopy(source)
wrong_type["resolver"] = []
(output / "wrong-type-resolver.json").write_text(
    json.dumps(wrong_type, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n",
    encoding="ascii",
)
PY
for INVALID_INVENTORY in "$ARTIFACT_DIR/weakened-resolver.json" "$ARTIFACT_DIR/wrong-type-resolver.json"; do
    expect_failure weakened-schema python3 "$INVENTORY_TOOL" verify \
        --workspace "$WORKSPACE" \
        --source-root "$TMPDIR_TEST" \
        --metadata-json "$METADATA_JSON" \
        --metadata-path-map "/src=$WORKSPACE" \
        --artifact-dir "$ARTIFACT_DIR" \
        "$INVALID_INVENTORY"
done

printf 'artifact mutation\n' >> "$ARTIFACT_DIR/release.tar"
expect_failure changed-artifact python3 "$INVENTORY_TOOL" verify \
    --workspace "$WORKSPACE" \
    --source-root "$TMPDIR_TEST" \
    --metadata-json "$METADATA_JSON" \
    --metadata-path-map "/src=$WORKSPACE" \
    --artifact-dir "$ARTIFACT_DIR" \
    "$ARTIFACT_DIR/one.json"
printf 'release artifact fixture\n' > "$ARTIFACT_DIR/release.tar"

cp "$WORKSPACE/Cargo.lock" "$TMPDIR_TEST/Cargo.lock.saved"
printf '# lock mutation\n' >> "$WORKSPACE/Cargo.lock"
expect_failure changed-lock python3 "$INVENTORY_TOOL" verify \
    --workspace "$WORKSPACE" \
    --source-root "$TMPDIR_TEST" \
    --metadata-json "$METADATA_JSON" \
    --metadata-path-map "/src=$WORKSPACE" \
    --artifact-dir "$ARTIFACT_DIR" \
    "$ARTIFACT_DIR/one.json"
cp "$TMPDIR_TEST/Cargo.lock.saved" "$WORKSPACE/Cargo.lock"

mv "$WORKSPACE/Cargo.lock" "$WORKSPACE/Cargo.lock.missing"
expect_failure missing-lock python3 "$INVENTORY_TOOL" generate \
    --workspace "$WORKSPACE" \
    --source-root "$TMPDIR_TEST" \
    --metadata-json "$METADATA_JSON" \
    --metadata-path-map "/src=$WORKSPACE" \
    --target armv7-unknown-linux-musleabihf \
    --artifact "$ARTIFACT_DIR/release.tar" \
    --source-date-epoch 1700000000 \
    --output "$ARTIFACT_DIR/missing-lock.json"
mv "$WORKSPACE/Cargo.lock.missing" "$WORKSPACE/Cargo.lock"

mkdir -p "$WORKSPACE/extra/src"
cat > "$WORKSPACE/extra/Cargo.toml" <<'EOF'
[package]
name = "inventory-extra"
version = "0.1.0"
edition = "2021"
EOF
printf 'pub fn extra() {}\n' > "$WORKSPACE/extra/src/lib.rs"
cat >> "$WORKSPACE/app/Cargo.toml" <<'EOF'
inventory-extra = { path = "../extra" }
EOF
expect_failure stale-lock "$RUSTUP_BIN" run 1.90.0 cargo metadata \
    --manifest-path "$WORKSPACE/Cargo.toml" \
    --locked --offline --filter-platform armv7-unknown-linux-musleabihf --format-version 1

echo "Rust dependency inventory: PASS"
