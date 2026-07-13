#!/usr/bin/env bash
# Offline fixture contract for the bounded final-rootfs ownership ledger.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ANALYZER="$SCRIPT_DIR/rootfs_ownership_ledger.py"
TMPDIR_TEST="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_TEST"' EXIT

TARGET="$TMPDIR_TEST/target"
BUILD="$TMPDIR_TEST/build"
BASE_OVERLAY="$TMPDIR_TEST/overlay-base"
BOARD_OVERLAY="$TMPDIR_TEST/overlay-board"
HOOK_ROOT="$TMPDIR_TEST/hook-root"
POST_BUILD="$TMPDIR_TEST/post-build.sh"
COMMON_PRUNE="$TMPDIR_TEST/common-prune.sh"
mkdir -p \
    "$TARGET/usr/bin" "$TARGET/etc" "$TARGET/bin" \
    "$BUILD/package-a" "$BUILD/package-b" \
    "$BASE_OVERLAY/etc" "$BOARD_OVERLAY/etc" "$HOOK_ROOT/etc"

printf 'tool bytes\n' > "$TARGET/usr/bin/tool"
chmod 0755 "$TARGET/usr/bin/tool"
printf 'shared bytes\n' > "$TARGET/usr/bin/shared"
printf 'board config\n' > "$TARGET/etc/config"
printf 'hook output\n' > "$TARGET/etc/hook"
chmod 0600 "$TARGET/etc/hook"
printf 'changed after overlay\n' > "$TARGET/etc/changed"
printf 'no claimant\n' > "$TARGET/etc/mystery"
ln -s ../usr/bin/tool "$TARGET/bin/tool-link"

printf 'base config\n' > "$BASE_OVERLAY/etc/config"
printf 'overlay original\n' > "$BASE_OVERLAY/etc/changed"
printf 'board config\n' > "$BOARD_OVERLAY/etc/config"
printf 'hook output\n' > "$HOOK_ROOT/etc/hook"
chmod 0600 "$HOOK_ROOT/etc/hook"
printf '#!/bin/sh\nprintf direct-mutation\\n\n' > "$POST_BUILD"
printf '#!/bin/sh\nprintf common-prune\\n\n' > "$COMMON_PRUNE"
printf 'rootfs artifact bytes\n' > "$TMPDIR_TEST/rootfs.img"

cat > "$BUILD/package-a/.files-list.txt" <<'EOF'
package-a,./usr/bin/tool
package-a,./usr/bin/shared
package-a,./bin/tool-link
EOF
cat > "$BUILD/package-b/.files-list.txt" <<'EOF'
package-b,./usr/bin/shared
EOF

COMMON_ARGS=(
    --target-dir "$TARGET"
    --build-dir "$BUILD"
    --overlay-root "base=$BASE_OVERLAY"
    --overlay-root "board=$BOARD_OVERLAY"
    --hook-root "post-build=$HOOK_ROOT"
    --post-build-script "board=$POST_BUILD"
    --post-build-script "common-prune=$COMMON_PRUNE"
    --artifact "$TMPDIR_TEST/rootfs.img"
)

python3 "$ANALYZER" "${COMMON_ARGS[@]}" --output "$TMPDIR_TEST/one.json" >/dev/null
touch -t 202501020304 "$TARGET/usr/bin/tool" "$BOARD_OVERLAY/etc/config"
python3 "$ANALYZER" "${COMMON_ARGS[@]}" --output "$TMPDIR_TEST/two.json" >/dev/null
cmp "$TMPDIR_TEST/one.json" "$TMPDIR_TEST/two.json"
python3 "$ANALYZER" "${COMMON_ARGS[@]}" --verify "$TMPDIR_TEST/one.json" >/dev/null

python3 - "$TMPDIR_TEST/one.json" "$TMPDIR_TEST" <<'PY'
import hashlib
import json
import pathlib
import sys

ledger = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
serialized = pathlib.Path(sys.argv[1]).read_text(encoding="ascii")
assert sys.argv[2] not in serialized, "host-specific fixture path leaked into deterministic ledger"
assert ledger["schema"] == "org.dcentral.dcentos.final-rootfs-ownership-ledger.v1"
assert ledger["claim_scope"]["is_sbom"] is False
assert ledger["claim_scope"]["is_spdx"] is False
assert ledger["claim_scope"]["proves_causal_content_origin"] is False
assert ledger["inputs"]["rootfs_artifacts"] == [{
    "path": "rootfs.img",
    "size": len(b"rootfs artifact bytes\n"),
    "sha256": hashlib.sha256(b"rootfs artifact bytes\n").hexdigest(),
}]
mutators = ledger["inputs"]["declared_direct_post_build_scripts"]
assert [mutator["name"] for mutator in mutators] == ["board", "common-prune"]
assert [mutator["path"] for mutator in mutators] == [
    "post-build.sh",
    "common-prune.sh",
]
entries = {entry["path"]: entry for entry in ledger["entries"]}
assert entries["/usr/bin/tool"]["classification"] == "uniquely_attributed"
assert entries["/usr/bin/tool"]["owner"] == "package:package-a"
assert entries["/usr/bin/tool"]["mode"] == "0755"
assert entries["/usr/bin/tool"]["sha256"] == hashlib.sha256(b"tool bytes\n").hexdigest()
assert entries["/usr/bin/shared"]["classification"] == "ambiguous"
assert entries["/etc/config"]["classification"] == "overlay_or_hook_owned"
assert entries["/etc/config"]["owner"] == "overlay:board"
assert entries["/etc/hook"]["owner"] == "hook:post-build"
assert entries["/etc/hook"]["mode"] == "0600"
assert entries["/etc/changed"]["classification"] == "unattributed"
assert entries["/etc/changed"]["attribution_basis"] == "last-declared-stage-identity-mismatch"
assert entries["/etc/mystery"]["classification"] == "unattributed"
assert entries["/bin/tool-link"]["type"] == "symlink"
assert entries["/bin/tool-link"]["symlink_target"] == "../usr/bin/tool"
assert sum(ledger["summary"]["classification_counts"].values()) == ledger["summary"]["entry_count"]
PY

printf 'mutated\n' > "$TARGET/usr/bin/tool"
if python3 "$ANALYZER" "${COMMON_ARGS[@]}" --verify "$TMPDIR_TEST/one.json" >/dev/null 2>&1; then
    echo "ERROR: ledger verification accepted mutated final content" >&2
    exit 1
fi
printf 'tool bytes\n' > "$TARGET/usr/bin/tool"
chmod 0755 "$TARGET/usr/bin/tool"

printf 'mutated artifact\n' >> "$TMPDIR_TEST/rootfs.img"
if python3 "$ANALYZER" "${COMMON_ARGS[@]}" --verify "$TMPDIR_TEST/one.json" >/dev/null 2>&1; then
    echo "ERROR: ledger verification accepted mutated rootfs artifact" >&2
    exit 1
fi
printf 'rootfs artifact bytes\n' > "$TMPDIR_TEST/rootfs.img"

printf '# direct mutator drift\n' >> "$POST_BUILD"
if python3 "$ANALYZER" "${COMMON_ARGS[@]}" --verify "$TMPDIR_TEST/one.json" >/dev/null 2>&1; then
    echo "ERROR: ledger verification accepted mutated post-build definition" >&2
    exit 1
fi
printf '#!/bin/sh\nprintf direct-mutation\\n\n' > "$POST_BUILD"

printf '# common prune drift\n' >> "$COMMON_PRUNE"
if python3 "$ANALYZER" "${COMMON_ARGS[@]}" --verify "$TMPDIR_TEST/one.json" >/dev/null 2>&1; then
    echo "ERROR: ledger verification accepted mutated common-prune definition" >&2
    exit 1
fi
printf '#!/bin/sh\nprintf common-prune\\n\n' > "$COMMON_PRUNE"

# Exercise the same shared helper used by production post-image hooks. The
# fixture proves it consumes Buildroot's BASE_DIR/build and TARGET_DIR rather
# than a retained ad-hoc tree.
export TARGET_DIR="$TARGET"
export BASE_DIR="$TMPDIR_TEST"
export BR2_EXTERNAL_DCENTOS_PATH="$(cd "$SCRIPT_DIR/../br2_external_dcentos" && pwd)"
# shellcheck source=lib/rootfs_ownership_ledger.sh
. "$SCRIPT_DIR/lib/rootfs_ownership_ledger.sh"
dcent_emit_rootfs_ownership_ledger \
    "$TMPDIR_TEST/rootfs.img" \
    "$TMPDIR_TEST/integrated.json" \
    --post-build-script "board=$POST_BUILD" \
    --post-build-script "common-prune=$COMMON_PRUNE" \
    --overlay-root "base=$BASE_OVERLAY" \
    --overlay-root "board=$BOARD_OVERLAY" >/dev/null
python3 - "$TMPDIR_TEST/integrated.json" <<'PY'
import json
import pathlib
import sys

ledger = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="ascii"))
assert ledger["inputs"]["rootfs_artifacts"][0]["path"] == "rootfs.img"
assert [
    mutator["name"]
    for mutator in ledger["inputs"]["declared_direct_post_build_scripts"]
] == ["board", "common-prune"]
assert [stage["name"] for stage in ledger["inputs"]["stages"]] == [
    "overlay:base",
    "overlay:board",
]
PY

if dcent_emit_rootfs_ownership_ledger \
    "$TMPDIR_TEST/missing-rootfs.img" \
    "$TMPDIR_TEST/should-not-exist.json" \
    --post-build-script "board=$POST_BUILD" \
    --post-build-script "common-prune=$COMMON_PRUNE" \
    --overlay-root "base=$BASE_OVERLAY" >/dev/null 2>&1; then
    echo "ERROR: post-image helper accepted a missing rootfs payload" >&2
    exit 1
fi

# Anti-orphan checks: every integrated production lane must call the helper,
# and the release collector must require, export, and source-closure-bind it.
for post_image in \
    "$SCRIPT_DIR/../br2_external_dcentos/board/zynq/post-image-ramdisk.sh" \
    "$SCRIPT_DIR/../br2_external_dcentos/board/zynq/am2-s19jpro/post-image.sh" \
    "$SCRIPT_DIR/../br2_external_dcentos/board/cvitek/cv1835-s19jpro/post-image.sh"
do
    grep -F 'scripts/lib/rootfs_ownership_ledger.sh' "$post_image" >/dev/null
    grep -F 'dcent_emit_rootfs_ownership_ledger' "$post_image" >/dev/null
    grep -F 'board/common/prune-runtime-research-tools.sh' "$post_image" >/dev/null
done
grep -F 's9|am2-s19jpro|cv1835-s19jpro)' "$SCRIPT_DIR/build_in_docker.sh" >/dev/null
grep -F 'integrated final-rootfs ownership ledger missing' "$SCRIPT_DIR/build_in_docker.sh" >/dev/null
grep -F 'rm -f buildroot/output/images/rootfs-ownership.json' "$SCRIPT_DIR/build_in_docker.sh" >/dev/null
grep -F 'SOURCE_CLOSURE_ARTIFACT_ARGS+=(--artifact "$ROOTFS_OWNERSHIP_LEDGER_PATH")' \
    "$SCRIPT_DIR/build_in_docker.sh" >/dev/null
grep -F 'BR2_ROOTFS_OVERLAY="$(BR2_EXTERNAL_DCENTOS_PATH)/board/zynq/rootfs-overlay"' \
    "$SCRIPT_DIR/../br2_external_dcentos/configs/dcentos_s9_defconfig" >/dev/null
grep -F 'BR2_ROOTFS_OVERLAY="$(BR2_EXTERNAL_DCENTOS_PATH)/board/zynq/rootfs-overlay $(BR2_EXTERNAL_DCENTOS_PATH)/board/zynq/am2-s19jpro/rootfs-overlay"' \
    "$SCRIPT_DIR/../br2_external_dcentos/configs/dcentos_am2_s19jpro_defconfig" >/dev/null
grep -F 'BR2_ROOTFS_OVERLAY="$(BR2_EXTERNAL_DCENTOS_PATH)/board/amlogic/rootfs-overlay $(BR2_EXTERNAL_DCENTOS_PATH)/board/cvitek/cv1835-s19jpro/rootfs-overlay"' \
    "$SCRIPT_DIR/../br2_external_dcentos/configs/dcentos_cv1835_s19jpro_defconfig" >/dev/null

printf 'package-a,../../escape\n' >> "$BUILD/package-a/.files-list.txt"
if python3 "$ANALYZER" "${COMMON_ARGS[@]}" --output "$TMPDIR_TEST/unsafe.json" >/dev/null 2>&1; then
    echo "ERROR: ledger accepted an unsafe package path claim" >&2
    exit 1
fi

echo "final rootfs ownership ledger: PASS"
