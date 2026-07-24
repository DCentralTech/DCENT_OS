#!/bin/sh
#
# Host-only OTA version monotonicity gate.
#
# This test never opens a miner connection and never writes flash. It extracts
# the version-comparison / rollback-floor functions from each shipped Zynq
# sysupgrade overlay and drives them with local fixture manifests.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH='' cd -- "$SCRIPT_DIR/.." && pwd)
MANIFEST_JSON_HELPER="$PROJECT_DIR/scripts/lib/sysupgrade_manifest_json.py"
export MANIFEST_JSON_HELPER

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/dcent-ota-version.XXXXXX" 2>/dev/null || mktemp -d)
CASES=0

cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

mkdir -p "$TMP_DIR/bin"
cat > "$TMP_DIR/bin/jsonfilter" <<'EOF_JSONFILTER'
#!/bin/sh
file=
expr=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -i) file=${2:-}; shift 2 ;;
        -e) expr=${2:-}; shift 2 ;;
        *) shift ;;
    esac
done
[ -n "$file" ] && [ -n "$expr" ] || exit 1
key=${expr#@.}
[ "$key" != "$expr" ] || exit 1
case "$key" in
    *.*) exit 1 ;;
esac
sed -n 's/.*"'"$key"'"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$file" | head -n 1
EOF_JSONFILTER
chmod +x "$TMP_DIR/bin/jsonfilter"
PATH="$TMP_DIR/bin:$PATH"
export PATH

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

pass() {
    echo "PASS: $*"
}

sysupgrade_paths() {
    cat <<'EOF_PATHS'
br2_external_dcentos/board/zynq/rootfs-overlay/usr/sbin/sysupgrade
br2_external_dcentos/board/zynq/am2-s19jpro/rootfs-overlay/usr/sbin/sysupgrade
br2_external_dcentos/board/zynq/am2-s19pro/rootfs-overlay/usr/sbin/sysupgrade
br2_external_dcentos/board/zynq/am2-s17pro/rootfs-overlay/usr/sbin/sysupgrade
EOF_PATHS
}

extract_version_prefix() {
    script_path=$1
    output_path=$2

    # Extract function definitions only. Executing the caller prologue would
    # incorrectly require its installed persistent-state helper even though
    # this test exercises no upgrade transaction or filesystem mutation.
    awk '
        /^cleanup_package\(\)/ { emit=1 }
        /^verify_sha256\(\)/ { exit }
        emit { print }
    ' "$script_path" >"$output_path"

    grep -Fq 'compare_versions()' "$output_path" \
        || fail "$script_path prefix missing compare_versions"
    grep -Fq 'enforce_sysupgrade_version_floor()' "$output_path" \
        || fail "$script_path prefix missing enforce_sysupgrade_version_floor"
}

run_compare_case() {
    prefix=$1
    candidate=$2
    current=$3
    expected=$4
    label=$5

    out=$(sh -c '. "$1"; compare_versions "$2" "$3"' sh "$prefix" "$candidate" "$current") \
        || fail "$label: compare_versions failed unexpectedly"
    [ "$out" = "$expected" ] \
        || fail "$label: expected $expected, got '$out'"
    CASES=$((CASES + 1))
}

run_compare_reject_case() {
    prefix=$1
    candidate=$2
    current=$3
    label=$4

    set +e
    out=$(sh -c '. "$1"; compare_versions "$2" "$3"' sh "$prefix" "$candidate" "$current" 2>&1)
    rc=$?
    set -e
    [ "$rc" -ne 0 ] \
        || fail "$label: malformed version compared successfully: $out"
    CASES=$((CASES + 1))
}

write_manifest() {
    manifest=$1
    version=$2
    status=$3

    if [ "$version" = "__missing__" ]; then
        printf '{"status":"%s"}\n' "$status" >"$manifest"
    else
        printf '{"version":"%s","status":"%s"}\n' "$version" "$status" >"$manifest"
    fi
}

run_floor_case() {
    prefix=$1
    candidate=$2
    current=$3
    status=$4
    allow_downgrade=$5
    expected_rc=$6
    expected_text=$7
    label=$8

    case_dir="$TMP_DIR/floor-$CASES"
    mkdir -p "$case_dir"
    manifest="$case_dir/MANIFEST.json"
    version_file="$case_dir/dcentos-version"
    write_manifest "$manifest" "$candidate" "$status"
    if [ "$current" != "__missing__" ]; then
        printf '%s\n' "$current" >"$version_file"
    fi

    set +e
    out=$(
        sh -c '
            . "$1"
            PACKAGE_MANIFEST=$2
            VERSION_PATH=$3
            PACKAGE_STATUS=$4
            ALLOW_DOWNGRADE=$5
            enforce_sysupgrade_version_floor
        ' sh "$prefix" "$manifest" "$version_file" "$status" "$allow_downgrade" 2>&1
    )
    rc=$?
    set -e

    [ "$rc" = "$expected_rc" ] \
        || fail "$label: expected rc=$expected_rc got rc=$rc output=$out"
    printf '%s\n' "$out" | grep -Fq -- "$expected_text" \
        || fail "$label: expected output containing '$expected_text', got '$out'"
    CASES=$((CASES + 1))
}

run_one_script() {
    rel_path=$1
    script_path="$PROJECT_DIR/$rel_path"
    [ -f "$script_path" ] || fail "missing sysupgrade overlay: $rel_path"

    safe_name=$(printf '%s' "$rel_path" | tr '/.' '__')
    prefix="$TMP_DIR/$safe_name.prefix.sh"
    extract_version_prefix "$script_path" "$prefix"

    run_compare_case "$prefix" "1.0.0" "1.0.0" "0" "$rel_path equal release"
    run_compare_case "$prefix" "1.0.1" "1.0.0" "1" "$rel_path patch upgrade"
    run_compare_case "$prefix" "1.0.0" "1.0.1" "-1" "$rel_path patch downgrade"
    run_compare_case "$prefix" "v1.2.0+build7" "1.2.0" "0" "$rel_path v-prefix build metadata"
    run_compare_case "$prefix" "1.2" "1.2.0" "0" "$rel_path padded release"
    run_compare_case "$prefix" "1.2.0" "1.2.0-rc1" "1" "$rel_path final beats prerelease"
    run_compare_case "$prefix" "1.2.0-rc1" "1.2.0" "-1" "$rel_path prerelease below final"
    run_compare_case "$prefix" "1.2.0-rc1" "1.2.0-rc2" "-1" "$rel_path compact prerelease lexical order"
    run_compare_case "$prefix" "1.2.0-rc10" "1.2.0-rc2" "-1" "$rel_path compact prerelease is explicitly lexical"
    run_compare_case "$prefix" "1.2.0-rc.10" "1.2.0-rc.2" "1" "$rel_path dotted numeric prerelease order"
    run_compare_case "$prefix" "1.2.0-alpha" "1.2.0-alpha.0" "-1" "$rel_path shorter prerelease ranks lower"
    run_compare_case "$prefix" "1.2.0-1" "1.2.0-alpha" "-1" "$rel_path numeric prerelease ranks below alphanumeric"
    run_compare_case "$prefix" "1.2.0-ab" "1.2.0-ba" "-1" "$rel_path prerelease labels use collision-free ASCII order"
    run_compare_case "$prefix" "9007199254740993.0" "9007199254740992.0" "1" "$rel_path release comparison preserves integers above 2^53"
    run_compare_case "$prefix" "18446744073709551616.0" "18446744073709551615.0" "1" "$rel_path release comparison is not bounded by u64"
    run_compare_case "$prefix" "1.2.0+build-7" "1.2.0+other" "0" "$rel_path build metadata never changes precedence"
    run_compare_reject_case "$prefix" "not-a-version" "1.0.0" "$rel_path malformed candidate"
    run_compare_reject_case "$prefix" "1.0.0" "not-a-version" "$rel_path malformed current"
    for malformed in 1 1.2.3.4 01.2.0 1.02.0 1.2.0- 1.2.0-alpha..1 1.2.0-alpha.01 1.2.0+ 1.2.0_bad 1.2.0:bad; do
        run_compare_reject_case "$prefix" "$malformed" "1.2.0" "$rel_path rejects noncanonical $malformed"
    done

    run_floor_case "$prefix" "1.0.0" "1.0.0" "release" "0" "0" \
        "matches running firmware version" "$rel_path equal floor"
    run_floor_case "$prefix" "1.0.1" "1.0.0" "release" "0" "0" \
        "is newer than running firmware version" "$rel_path upgrade floor"
    run_floor_case "$prefix" "0.9.0" "1.0.0" "release" "0" "1" \
        "Downgrade refused: package version 0.9.0" "$rel_path release downgrade refused"
    run_floor_case "$prefix" "0.9.0" "1.0.0" "release" "1" "1" \
        "Downgrade refused: package version 0.9.0" "$rel_path release downgrade override refused"
    run_floor_case "$prefix" "0.9.0" "1.0.0" "lab" "1" "0" \
        "allowing non-release downgrade 1.0.0 -> 0.9.0" "$rel_path lab downgrade override"
    run_floor_case "$prefix" "__missing__" "1.0.0" "release" "0" "1" \
        "has no single non-empty string version" "$rel_path missing candidate version"
    run_floor_case "$prefix" "1.0.0" "__missing__" "release" "0" "1" \
        "current " "$rel_path missing current version"

    pass "$rel_path OTA version monotonicity matrix green"
}

cd "$PROJECT_DIR"
for rel_path in $(sysupgrade_paths); do
    [ -n "$rel_path" ] || continue
    run_one_script "$rel_path"
done

echo "OTA_VERSION_MONOTONICITY_OK cases=$CASES"
